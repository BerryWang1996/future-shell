//! 带 `lrzsz` 的 sshd 容器（ZMODEM 集成测试专用）。
//!
//! 为什么不复用 [`crate::sshd::SshdContainer`]：那个用的是 `linuxserver/openssh-server`
//! （Alpine 3.24），而 **Alpine 的仓库里已经没有 `lrzsz`** 了（main/community 都没有，
//! 回退到 v3.12 的仓库也没有——实测过）。Debian 仍然在打包 `lrzsz 0.12.21`，
//! 所以 ZMODEM 这一项另起一个 Debian 容器。
//!
//! 也**没有**改成「跳过 SSH、用 `docker run -i` 的管道直接喂 rz/sz」。那样确实更快，
//! 但会把「我们放到 SSH 通道上的字节」这一层从测试里摘掉；而 ZMODEM 恰恰对字节
//! 透明度敏感（转义、控制字符、8 位干净），摘掉的正是最该测的那层。
//!
//! 代价是启动慢（容器内 apt 装 openssh-server + lrzsz，几十秒），所以只有 ZMODEM
//! 这一个测试文件用它。

use testcontainers::core::{CmdWaitFor, ContainerPort, ExecCommand, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// 容器内 sshd 监听端口。用 2222 而不是 22：容器里以 root 跑 sshd 虽然能绑 22，
/// 但与本仓另一个容器保持同一个约定，读日志时不必分辨是哪一个。
const SSH_PORT: ContainerPort = ContainerPort::Tcp(2222);

pub struct LrzszSshd {
    pub container: ContainerAsync<GenericImage>,
    pub username: String,
    pub password: String,
}

impl LrzszSshd {
    pub async fn start(tag: &str) -> testcontainers::core::error::Result<Self> {
        let username = format!("z{tag}");
        let password = format!("pass-{tag}");
        // 一条 sh -c 干完：装包 → 建用户 → 起 sshd（前台 + 日志到 stderr）。
        //
        // `-e` 让 sshd 把日志写到 stderr，等待策略才有东西可等（默认写 syslog，
        // 容器里没有 syslogd，等待会永远命中不了）。
        // `PermitRootLogin no` 不必设：我们用普通用户登录。
        //
        // ## apt 的 stderr **不再**被吞掉
        //
        // 原来写的是 `>/dev/null 2>&1`，理由是「apt 的进度输出会把等待用的日志行冲掉」。
        // 那个理由随下面的就绪判定改造一起失效了——现在等待匹配的是 apt **之前**打的
        // `fs-itest-boot`，apt 说什么都冲不掉它。
        //
        // 而吞掉 stderr 的代价是实打实的：apt 一失败就 `set -e` 退出、容器死掉，
        // 而**零痕迹**。这一处害我查了一轮：`record.rs` 的用例在 300 秒后报
        // 「Connection refused」，容器 stderr 里只有一行 `fs-itest-boot`——
        // 真因（apt 说了什么）被这条重定向擦干净了。现在只吞 stdout（进度条），
        // stderr 留着；配合就绪超时那里把容器 stderr 摊进 panic 文案，
        // 下一次同样的失败会直接告诉你 apt 报了什么。
        //
        // 同批加 `Acquire::Retries=3`：Debian 镜像源偶发抖动是这类容器测试最常见的
        // 单点，而重试是 apt 自己就支持的事，没有理由让它去红一整条测试。
        //
        // `--no-install-recommends` 不是省心的优化，是必要的：不加它 apt 会把
        // systemd 整套拉进来，实测 75 秒；加了 42 秒。
        //
        // `command -v sz` 是**非空证明**：apt 在网络异常时会打一串 WARNING 后
        // 以 0 退出，只靠退出码会得到一个「装好了」的假象，随后测试在
        // `sz: not found` 上以完全无关的方式失败。`set -e` + 这两行让容器直接
        // 起不来，报错指向真正的原因。
        //
        // ## 就绪判定为什么**不**等 "Server listening on"
        //
        // 那句话要等 40 秒（容器内 apt 装 openssh-server + lrzsz），期间输出全被
        // 重定向到 /dev/null——于是 testcontainers 的日志跟随连接要在一条**静默 40 秒**
        // 的流上挂着。直连宿主 Docker（在 Windows 上跑测试）没问题；经
        // `scripts/linux-itest.sh` 的「挂载 socket + 兄弟容器」路径时，那条连接会
        // 随机断掉，报 `WaitLog(EndOfStream([]))`——**空缓冲**，一个字节都没读到。
        // 三条 zmodem 用例因此在那条路径上随机红（哪一条红每次都不同；
        // Windows 宿主 3/3 稳过）。
        //
        // 先试过每 5 秒吐一行心跳让流不空转，**没用**——所以问题不在「静默」，
        // 而在那条连接活得太久本身。现改为：开局第一件事就打一个标记，
        // 日志等待在毫秒内命中、跟随连接随即关闭；就绪与否改由**轮询 TCP 拿 SSH banner**
        // 判定（`SshdContainer::wait_for_sshd_banner` 同款做法，它一直是稳的）。
        // 日志流从此不参与就绪判定，那条脆弱的长连接也就不存在了。
        let script = format!(
            "set -e; \
             echo 'fs-itest-boot' >&2; \
             export DEBIAN_FRONTEND=noninteractive; \
             apt-get -o Acquire::Retries=3 update -qq >/dev/null; \
             apt-get -o Acquire::Retries=3 install -y -qq --no-install-recommends openssh-server lrzsz >/dev/null; \
             command -v sz >/dev/null; command -v rz >/dev/null; \
             useradd -m -s /bin/sh {username}; \
             echo '{username}:{password}' | chpasswd; \
             mkdir -p /run/sshd; \
             exec /usr/sbin/sshd -D -e -p 2222"
        );
        // 次序有讲究：`with_exposed_port`/`with_wait_for`/`with_entrypoint` 是
        // `GenericImage` 自己的方法，`with_startup_timeout`/`with_cmd` 来自 `ImageExt`
        // 并把值变成 `ContainerRequest`。先调 ImageExt 的就再也回不到 GenericImage
        // 的方法上（E0599）。
        let container = GenericImage::new("debian", "stable-slim")
            .with_exposed_port(SSH_PORT)
            // 见上：等的是**开局那一行**，不是装完包之后的 "Server listening on"。
            .with_wait_for(WaitFor::message_on_stderr("fs-itest-boot"))
            .with_entrypoint("sh")
            // 默认 60 秒不够：容器里现装 openssh-server + lrzsz 实测 42 秒，
            // 冷镜像、慢网或忙机器上很容易越过 60。给到 300 秒——超时的意义是
            // 「卡住了」而不是「慢」，宁可留足余量也不要把一次装包抖动报成失败。
            .with_startup_timeout(std::time::Duration::from_secs(300))
            .with_cmd(["-c", &script])
            .start()
            .await?;
        let me = Self {
            container,
            username,
            password,
        };
        // 真正的就绪判定。装包 + 起 sshd 实测 42 秒，给到 300 秒——
        // 与 `with_startup_timeout` 同口径：超时的意义是「卡住了」而不是「慢」。
        me.wait_for_sshd_banner(std::time::Duration::from_secs(300))
            .await;
        Ok(me)
    }

    /// 轮询 TCP 直到对端吐出 SSH banner。
    ///
    /// 判据取 banner 的前四字节而不是「连得上」：Docker 的端口发布代理在容器内
    /// 服务尚未起来时**照样接受 TCP 连接**（`monitor.rs` 那条停机测试栽过同一个坑），
    /// 只看 connect 成功会在 sshd 还没起来的时候就返回。
    async fn wait_for_sshd_banner(&self, budget: std::time::Duration) {
        use tokio::io::AsyncReadExt;
        let addr = self.addr().await;
        let deadline = tokio::time::Instant::now() + budget;
        let mut last = String::from("(未发起过连接)");
        while tokio::time::Instant::now() < deadline {
            match tokio::net::TcpStream::connect(addr).await {
                Ok(mut s) => {
                    let mut buf = [0u8; 4];
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        s.read_exact(&mut buf),
                    )
                    .await
                    {
                        Ok(Ok(_)) if &buf == b"SSH-" => return,
                        Ok(Ok(_)) => last = format!("banner 前四字节为 {buf:?}，非 SSH-"),
                        Ok(Err(e)) => last = format!("读 banner 失败：{e}"),
                        Err(_) => last = "读 banner 超时".into(),
                    }
                }
                Err(e) => last = format!("连接失败：{e}"),
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        // 报错要带上容器日志：`EndOfStream([])` 那种什么也不说的错误，
        // 正是这次排查最费时间的部分。
        let logs = self
            .container
            .stderr_to_vec()
            .await
            .map(|v| String::from_utf8_lossy(&v).to_string())
            .unwrap_or_else(|e| format!("(取容器日志失败：{e})"));
        panic!("lrzsz sshd 在 {budget:?} 内未就绪：{last}\n容器 stderr：\n{logs}");
    }

    pub async fn addr(&self) -> std::net::SocketAddr {
        let host = self.container.get_host().await.unwrap().to_string();
        let port = self.container.get_host_port_ipv4(SSH_PORT).await.unwrap();
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            return std::net::SocketAddr::new(ip, port);
        }
        let hostport = format!("{host}:{port}");
        let resolved = tokio::net::lookup_host(&hostport)
            .await
            .unwrap_or_else(|e| panic!("resolve {hostport}: {e}"))
            .find(|a| a.is_ipv4());
        resolved.unwrap_or_else(|| panic!("no IPv4 address for {hostport}"))
    }

    /// 在容器内跑一条 shell 命令并断言成功，返回 stdout。
    ///
    /// ZMODEM 测试用它造已知内容的源文件，以及在上传之后**在容器侧**校验落地字节
    /// ——在客户端比对自己刚发出去的字节，证明的只是「我发的等于我发的」。
    pub async fn run(&self, script: &str) -> testcontainers::core::error::Result<String> {
        let mut res = self
            .container
            .exec(
                ExecCommand::new(["sh", "-c", script]).with_cmd_ready_condition(CmdWaitFor::exit()),
            )
            .await?;
        let out = String::from_utf8_lossy(&res.stdout_to_vec().await?).to_string();
        let err = String::from_utf8_lossy(&res.stderr_to_vec().await?).to_string();
        assert_eq!(
            res.exit_code().await?,
            Some(0),
            "容器内命令失败：{script}\nstdout: {out}\nstderr: {err}"
        );
        Ok(out)
    }
}
