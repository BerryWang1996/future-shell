//! xrdp 容器（RDP 集成测试用，阶段 1）。
//!
//! # 为什么是 xrdp 而不是 IronRDP 自带的 server 示例
//!
//! IronRDP 的 server 与客户端**共享同一套 PDU 编解码**：一处理解偏差会被同时
//! 犯在两边、于是同时抵消掉。xrdp 是独立实现（C，跑了二十年的真服务器），
//! 它点头才算协议对。这与 ZMODEM 对真 lrzsz、MCP 对手写 JSON-RPC 是同一条纪律。
//!
//! # 阶段 1 只测到认证为止
//!
//! 连接测试走完 X.224 → TLS → CredSSP(NTLM/PAM) → 连接最终化。
//! 不需要能登录出桌面（那要 Xvnc/Xorg 后端）：错的口令要被拒、对的口令要
//! 走到 Connected 或至少通过认证进入会话建立——这已经把引擎的全部连接路径
//! 压满了。桌面帧的验证归阶段 2（配 Xvnc 后端）。

use testcontainers::core::{CmdWaitFor, ContainerPort, ExecCommand, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

const RDP_PORT: ContainerPort = ContainerPort::Tcp(3389);

pub struct XrdpContainer {
    pub container: ContainerAsync<GenericImage>,
    pub username: String,
    pub password: String,
}

impl XrdpContainer {
    pub async fn start(tag: &str) -> testcontainers::core::error::Result<Self> {
        let username = format!("u{tag}");
        let password = format!("pw-{tag}");
        // xrdp 不带 systemd 起不来服务，全部手工：
        // · 建目录（/var/run/xrdp 等，包后处理脚本在容器里没跑）；
        // · 建用户并设密码（PAM 用系统口令验证）；
        // · apt 装 xrdp；
        // · 写 xrdp.ini 强制 negotiate（默认即可用 NLA；显式写防发行版默认漂移）；
        // · `xrdp -n` 前台跑。
        //
        // apt 的**整段重试**：本机的容器出网走一条会随机 5xx 的代理链，
        // Acquire::Retries 只管单请求，包列表里死一个 502 整轮就废。
        // 脚本不用 set -e：任何一步失败都**活着**（sleep 600）等诊断 exec，
        // 而不是让容器直接死掉——死容器的 stdout 拿不到（testcontainers Ryuk 会收走），
        // 「容器 not running」的报错离真因十万八千里。
        let script = format!(
            "step() {{ echo \"STEP:$1\" >&2; }}; \
             export DEBIAN_FRONTEND=noninteractive; \
             ok=0; for i in 1 2 3 4 5; do \
               apt-get -o Acquire::Retries=3 update -qq >/dev/null 2>&1 && \
               apt-get -o Acquire::Retries=3 install -y -qq --no-install-recommends xrdp openssl >/dev/null 2>&1 && ok=1 && break; \
               sleep 4; \
             done; \
             if [ \"$ok\" != 1 ]; then echo APT-FAILED >&2; sleep 600; fi; \
             step user; useradd -m -s /bin/bash {username} || {{ echo USERADD-FAILED >&2; sleep 600; }}; \
             echo '{username}:{password}' | chpasswd; \
             mkdir -p /var/run/xrdp /var/log/xrdp /usr/sbin; \
             if [ ! -s /etc/xrdp/cert.pem ]; then (cd /etc/xrdp && \
               openssl req -x509 -newkey rsa:2048 -nodes -days 30 \
                 -keyout key.pem -out cert.pem -subj '/CN=xrdp-itest' >/dev/null 2>&1) \
               || {{ echo CERT-FAILED >&2; sleep 600; }}; fi; \
             sed -i 's/^security=.*/security=negotiate/' /etc/xrdp/xrdp.ini; \
             step xrdp; exec xrdp -n"
        );
        let container = GenericImage::new("debian", "stable-slim")
            .with_exposed_port(RDP_PORT)
            // **不等日志、也不等宿主侧端口可连**：xrdp 把日志写进文件（stderr 无输出，
            // message_on_stderr 等不来）；而宿主侧 TCP 轮询会被 **Docker 端口代理**
            // 骗过——服务还没起代理就接受连接（monitor.rs 停机测试记录过同一个坑），
            // 随即 RST。就绪判定只能在**容器内**做：轮询 exec 查 xrdp 日志里的
            // 监听行，那才是 xrdp 自己说的话。
            .with_wait_for(WaitFor::seconds(2))
            .with_entrypoint("sh")
            .with_startup_timeout(std::time::Duration::from_secs(300))
            .with_cmd(["-c", &script])
            .start()
            .await?;
        let me = Self {
            container,
            username,
            password,
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            let res = me
                .container
                .exec(
                    ExecCommand::new([
                        "sh",
                        "-c",
                        "grep -q 'listening to port' /var/log/xrdp.log 2>/dev/null",
                    ])
                    .with_cmd_ready_condition(CmdWaitFor::exit()),
                )
                .await;
            if let Ok(r) = &res {
                if r.exit_code().await.ok().flatten() == Some(0) {
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "xrdp 300 秒内未开始监听；容器主进程输出：{:?}；容器诊断：{}",
                {
                    let v = me.container.stderr_to_vec().await.unwrap_or_default();
                    let s = String::from_utf8_lossy(&v);
                    s.lines().take(8).map(|l| l.to_string()).collect::<Vec<_>>()
                },
                {
                    // 摊开现场（不断言退出码——诊断本身不许再 panic）：xrdp 装没装、日志到哪一步
                    match me
                        .container
                        .exec(
                            ExecCommand::new([
                                "sh",
                                "-c",
                                "command -v xrdp; echo ---; tail -3 /var/log/xrdp.log 2>&1",
                            ])
                            .with_cmd_ready_condition(CmdWaitFor::exit()),
                        )
                        .await
                    {
                        Ok(mut r) => {
                            let out_v = r.stdout_to_vec().await.unwrap_or_default();
                            let err_v = r.stderr_to_vec().await.unwrap_or_default();
                            let out = String::from_utf8_lossy(&out_v).to_string();
                            let err = String::from_utf8_lossy(&err_v).to_string();
                            format!("stdout={out} stderr={err}")
                        }
                        Err(e) => format!("exec 失败：{e}"),
                    }
                }
            );
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        }
        Ok(me)
    }

    pub async fn addr(&self) -> std::net::SocketAddr {
        let host = self.container.get_host().await.unwrap().to_string();
        let port = self.container.get_host_port_ipv4(RDP_PORT).await.unwrap();
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

    /// 在容器内跑命令并断言成功，返回 stdout（诊断用，stderr 一并摊开）。
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
