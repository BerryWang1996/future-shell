//! **只通告 keyboard-interactive** 的 sshd 容器（M1 出口「四路认证」的第三路）。
//!
//! # 为什么不复用 `crate::sshd::SshdContainer`
//!
//! 那个用的是 `linuxserver/openssh-server`（Alpine）。Alpine 把 PAM 支持拆在
//! **另一个包**里（`openssh-server-pam`），基础包编出来的 sshd 在 Linux 上
//! 没有任何 keyboard-interactive 认证设备可用——它会通告 keyboard-interactive，
//! 然后对任何应答都返回失败。
//!
//! 那种服务器测不出这一路：客户端的 kbd-interactive 代码走完整个流程、
//! 最后拿到一个 failure，与「客户端根本没实现 kbd-interactive」在断言上不可区分。
//! 所以另起一个 Debian 容器（Debian 的 openssh-server 默认带 PAM，
//! `UsePAM yes` 是它的出厂配置）。
//!
//! # 只通告一种方法是判据的一半
//!
//! 配置里把 publickey 与 password **都关掉**。三样俱全时「客户端到底走了哪一路」
//! 根本看不出来——口令认证成功也会让测试变绿，而那证明不了 kbd-interactive
//! 这条链上的任何一行代码。这与 `SshdContainer::restrict_to_publickey_only`
//! 是同一个思路，方向相反。
//!
//! 启动慢（容器内 apt 装 openssh-server，几十秒），与 `crate::lrzsz` 同款代价。

use testcontainers::core::{CmdWaitFor, ContainerPort, ExecCommand, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

const SSH_PORT: ContainerPort = ContainerPort::Tcp(2222);

pub struct KbdSshd {
    pub container: ContainerAsync<GenericImage>,
    pub username: String,
    pub password: String,
}

impl KbdSshd {
    pub async fn start(tag: &str) -> testcontainers::core::error::Result<Self> {
        let username = format!("k{tag}");
        let password = format!("pass-{tag}");
        // 配置写成**独立文件再 Include**是不行的：Debian 的 sshd_config 里
        // `Include /etc/ssh/sshd_config.d/*.conf` 在文件开头，而 sshd_config 的语义是
        // **首次取到的值生效**——drop-in 确实会先被读到。但 Debian 各版本对这一行
        // 的位置并不一致（bookworm 有、buster 没有），依赖它就等于依赖一个会变的前提。
        //
        // 改为直接**重写整份 sshd_config**：这份测试要的服务器只有一种用途，
        // 没有必要保留发行版的默认值，也就不存在「哪一行先生效」的问题。
        //
        // `UsePAM yes` 是 keyboard-interactive 在 Linux 上唯一可用的认证设备。
        // 少了它，sshd 照样通告 keyboard-interactive，但对任何应答都返回失败——
        // 那种服务器会让这条测试以「客户端有 bug」的样子红，而真因在服务端配置。
        // 就绪判定与 apt 输出的处理都照搬 `crate::lrzsz` 的结论，理由见那边的长注释：
        // ① 等的是 apt **之前**打的 `fs-itest-boot`，不是装完包之后的
        //    "Server listening on"——后者要让日志跟随连接在一条静默 40 秒的流上挂着，
        //    经 `scripts/linux-itest.sh` 的兄弟容器路径时会随机断成
        //    `WaitLog(EndOfStream([]))`（空缓冲，什么也不说）；
        // ② apt 的 **stderr 不吞**——吞掉它的话，一次 apt 失败留下的是零痕迹，
        //    而表象是 300 秒后一句 "Connection refused"；
        // ③ `Acquire::Retries=3`——镜像源抖动是这类容器测试最常见的单点。
        let script = format!(
            "set -e; \
             echo 'fs-itest-boot' >&2; \
             export DEBIAN_FRONTEND=noninteractive; \
             apt-get -o Acquire::Retries=3 update -qq >/dev/null; \
             apt-get -o Acquire::Retries=3 install -y -qq --no-install-recommends openssh-server >/dev/null; \
             command -v sshd >/dev/null || command -v /usr/sbin/sshd >/dev/null; \
             useradd -m -s /bin/sh {username}; \
             echo '{username}:{password}' | chpasswd; \
             mkdir -p /run/sshd; \
             printf '%s\\n' \
               'Port 2222' \
               'UsePAM yes' \
               'KbdInteractiveAuthentication yes' \
               'PasswordAuthentication no' \
               'PubkeyAuthentication no' \
               'PermitRootLogin no' \
               'Subsystem sftp /usr/lib/openssh/sftp-server' \
               > /etc/ssh/sshd_config; \
             /usr/sbin/sshd -t; \
             exec /usr/sbin/sshd -D -e -p 2222"
        );
        // `sshd -t` 是**非空证明**：配置写错时它当场非零退出，容器起不来、
        // 报错指向配置本身；没有它的话，sshd 会带着一份被忽略的配置正常启动，
        // 而失败会推迟到一次语焉不详的认证拒绝。
        let container = GenericImage::new("debian", "stable-slim")
            .with_exposed_port(SSH_PORT)
            .with_wait_for(WaitFor::message_on_stderr("fs-itest-boot"))
            .with_entrypoint("sh")
            // 与 lrzsz 同口径：超时的意义是「卡住了」而不是「慢」。
            .with_startup_timeout(std::time::Duration::from_secs(300))
            .with_cmd(["-c", &script])
            .start()
            .await?;
        let me = Self {
            container,
            username,
            password,
        };
        me.wait_for_sshd_banner(std::time::Duration::from_secs(300))
            .await;
        Ok(me)
    }

    /// 轮询 TCP 直到对端吐出 SSH banner。判据取 banner 前四字节而不是「连得上」：
    /// Docker 的端口发布代理在容器内服务尚未起来时照样接受 TCP 连接。
    async fn wait_for_sshd_banner(&self, budget: std::time::Duration) {
        use tokio::io::AsyncReadExt;
        let addr = self.addr().await;
        let deadline = tokio::time::Instant::now() + budget;
        let mut last = String::from("(未发起过连接)");
        while tokio::time::Instant::now() < deadline {
            if let Ok(mut s) = tokio::net::TcpStream::connect(addr).await {
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
            } else {
                last = "连接被拒".into();
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        // 带上容器日志：`EndOfStream([])` / 一句光秃秃的 "Connection refused"
        // 都是查起来最费时间的那种报错。
        let logs = self
            .container
            .stderr_to_vec()
            .await
            .map(|v| String::from_utf8_lossy(&v).to_string())
            .unwrap_or_else(|e| format!("(取容器日志失败：{e})"));
        panic!("kbd sshd 在 {budget:?} 内未就绪：{last}\n容器 stderr：\n{logs}");
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
    pub async fn run(&self, script: &str) -> testcontainers::core::error::Result<String> {
        let mut res = self
            .container
            .exec(
                ExecCommand::new(["sh", "-c", script]).with_cmd_ready_condition(CmdWaitFor::exit()),
            )
            .await?;
        let code = res.exit_code().await?;
        let out = String::from_utf8_lossy(&res.stdout_to_vec().await?).to_string();
        let err = String::from_utf8_lossy(&res.stderr_to_vec().await?).to_string();
        assert_eq!(
            code,
            Some(0),
            "容器内命令失败：{script}\nout={out}\nerr={err}"
        );
        Ok(out)
    }
}
