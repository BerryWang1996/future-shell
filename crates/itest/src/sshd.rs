use testcontainers::core::{CmdWaitFor, ContainerPort, ExecCommand, WaitFor};
use testcontainers::runners::AsyncRunner; // .start() 由此 trait 提供（0.24/0.27 两版 lib.rs 均无 root re-export）；不导入则下方 .start() 无法解析
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

/// 容器**内部**的 sshd 监听端口（linuxserver 镜像的 init 把 `Port` 定死为 2222）。
/// 与 `addr()` 返回的宿主映射端口不是一回事：跳板测试经 direct-tcpip 让容器自己发起
/// TCP 连接，目标必须用这个内部端口——宿主映射端口在容器网络命名空间里根本不存在。
pub const INTERNAL_SSH_PORT: u16 = 2222;

const SSH_PORT: ContainerPort = ContainerPort::Tcp(INTERNAL_SSH_PORT);

pub struct SshdContainer {
    pub container: ContainerAsync<GenericImage>,
    pub username: String,
    pub password: String,
}

impl SshdContainer {
    pub async fn start(tag: &str) -> testcontainers::core::error::Result<Self> {
        let username = format!("user{}", tag);
        let password = format!("pass-{tag}");
        let container = GenericImage::new("linuxserver/openssh-server", "latest")
            .with_exposed_port(SSH_PORT)
            // 等待策略依赖 stdout 日志行；linuxserver 镜像的 s6-overlay 默认把服务日志重定向到文件，
            // 不设 LOG_STDOUT=true 则 WaitFor::message_on_stdout 永不命中（Round 2 复审 P2-4 根因修复）。
            .with_wait_for(WaitFor::message_on_stdout("Server listening on"))
            .with_env_var("LOG_STDOUT", "true")
            .with_env_var("PASSWORD_ACCESS", "true")
            .with_env_var("USER_NAME", &username)
            .with_env_var("USER_PASSWORD", &password)
            .with_env_var("PUID", "1000")
            .with_env_var("PGID", "1000")
            .start()
            .await?;
        Ok(Self {
            container,
            username,
            password,
        })
    }

    /// 打开容器内 sshd 的 TCP 转发（跳板/ProxyJump 测试前置）。
    ///
    /// linuxserver/openssh-server 的运行时配置 `/config/sshd/sshd_config` 里
    /// **`AllowTcpForwarding no` 是未注释的实配项**（源自 Alpine 打包的 sshd_config），
    /// 不改就一律回 `ChannelOpenFailure(AdministrativelyProhibited)`，direct-tcpip 全灭。
    /// 该镜像的 `Include` 段又被 init 脚本注释掉、`/config/sshd` 目录还是 init 现场创建的，
    /// 所以 `with_copy_to` 预置 drop-in 片段不可靠；改为「起容器后改配置 + `s6-svc -r`
    /// 重启服务 + 轮询等 SSH banner」。
    ///
    /// `grep -qx` 是**非空证明**：sed 若因上游镜像改写法而没匹配上，这里立刻非零退出
    /// 让测试红掉，而不是让转发继续被禁、把失败推迟到语焉不详的 channel open 错误。
    pub async fn enable_tcp_forwarding(&self) -> testcontainers::core::error::Result<()> {
        let res = self
            .container
            .exec(
                ExecCommand::new([
                    "sh",
                    "-c",
                    "sed -i 's/^AllowTcpForwarding no/AllowTcpForwarding yes/' \
                     /config/sshd/sshd_config \
                     && grep -qx 'AllowTcpForwarding yes' /config/sshd/sshd_config \
                     && s6-svc -r /run/service/svc-openssh-server",
                ])
                .with_cmd_ready_condition(CmdWaitFor::exit()),
            )
            .await?;
        assert_eq!(
            res.exit_code().await?,
            Some(0),
            "开启容器 TCP 转发失败（sed 未命中 AllowTcpForwarding no，或 s6-svc 重启失败）"
        );
        self.wait_for_sshd_banner().await;
        Ok(())
    }

    // ZMODEM 测试所需的 `lrzsz` 装不到这个镜像上：Alpine 的仓库（main/community，
    // 连回退到 v3.12 的仓库）都已经没有 `lrzsz` 这个包了。故 ZMODEM 另用
    // `crate::lrzsz::LrzszSshd`（Debian 基底）。此处留注不留代码——曾经在这里放过
    // 一个 `install_lrzsz`，它在任何机器上都只会失败。

    /// 把容器内 sshd 收紧为**只通告 publickey**（认证状态机的通告驱动性质需要这样一台服务器
    /// 才能证伪：默认镜像通告 `[publickey, password, keyboard-interactive]`，三样俱全时
    /// 「有没有先问过服务器」根本看不出差别）。
    ///
    /// 用「在文件**开头**插入」而不是追加：`sshd_config` 的语义是**首次取到的值生效**
    /// （sshd_config(5)：for each keyword, the first obtained value will be used），
    /// 追加到末尾会被前面 init 写入的 `PasswordAuthentication yes`（实测在第 61 行）压住而毫无效果。
    /// `KbdInteractiveAuthentication` 在该镜像里是注释行（第 67 行），默认 yes，也必须显式关掉。
    ///
    /// `grep -qx` 同 [`Self::enable_tcp_forwarding`]，是**非空证明**：插入若没生效就地非零退出。
    /// 更强的证明在调用方——服务器实际通告什么，由测试断言 `Error::Auth` 的 `remaining` 来锁定。
    pub async fn restrict_to_publickey_only(&self) -> testcontainers::core::error::Result<()> {
        let res = self
            .container
            .exec(
                ExecCommand::new([
                    "sh",
                    "-c",
                    "sed -i '1i PasswordAuthentication no\\nKbdInteractiveAuthentication no' \
                     /config/sshd/sshd_config \
                     && grep -qx 'PasswordAuthentication no' /config/sshd/sshd_config \
                     && grep -qx 'KbdInteractiveAuthentication no' /config/sshd/sshd_config \
                     && s6-svc -r /run/service/svc-openssh-server",
                ])
                .with_cmd_ready_condition(CmdWaitFor::exit()),
            )
            .await?;
        assert_eq!(
            res.exit_code().await?,
            Some(0),
            "收紧容器认证方法失败（sed 插入未生效，或 s6-svc 重启失败）"
        );
        self.wait_for_sshd_banner().await;
        Ok(())
    }

    /// 在容器里生成一对密钥、把公钥装进该用户的 `authorized_keys`，返回**私钥 PEM**。
    ///
    /// 公钥认证此前**没有任何容器 itest**（2026-08-23 盘点发现）：M1 出口原文要求
    /// 「密码/公钥/kbd-interactive/agent 四路认证 itest 全绿」，而 harness 压根没有植入
    /// `authorized_keys` 的能力——这条测试不是「忘了写」，是**写不出来**。本方法补上这个能力。
    ///
    /// 为什么在容器里 `ssh-keygen` 而不是在测试进程里造密钥：① 不引新依赖（ssh-key 目前
    /// 只是 sshengine 的 dev-dependency，itest 侧没有）；② 生成出来的就是 OpenSSH 自己认的
    /// 格式，绕开「我们写的 PEM 服务端认不认」这一层无关变量——本测试要验的是**认证链路**，
    /// 不是密钥编码。
    ///
    /// 权限是硬前置：sshd 对 `~/.ssh`（0700）与 `authorized_keys`（0600）有严格检查，
    /// 权限过宽会被静默忽略，表现为「密钥明明装了却还是认证失败」。故这里显式 chmod 并
    /// 用 `grep -q` 做非空证明——装不进去要当场红，而不是把失败推迟到语焉不详的认证拒绝。
    pub async fn install_authorized_key(&self) -> testcontainers::core::error::Result<String> {
        // linuxserver 镜像的用户家目录（USER_NAME 指定的用户，家目录固定在 /config）
        let home = "/config";
        self.run(&format!(
            "set -e; \
             mkdir -p {home}/.ssh; \
             rm -f {home}/.ssh/itest_key {home}/.ssh/itest_key.pub; \
             ssh-keygen -t ed25519 -N '' -C itest -f {home}/.ssh/itest_key >/dev/null; \
             cat {home}/.ssh/itest_key.pub >> {home}/.ssh/authorized_keys; \
             chown -R {uid}:{gid} {home}/.ssh; \
             chmod 700 {home}/.ssh; chmod 600 {home}/.ssh/authorized_keys; \
             grep -q itest {home}/.ssh/authorized_keys",
            home = home,
            uid = 1000,
            gid = 1000,
        ))
        .await?;
        // 私钥原样取出（cat 而非 base64：ed25519 私钥是纯 ASCII PEM，无需转码）
        self.run(&format!("cat {home}/.ssh/itest_key")).await
    }

    /// 在容器内跑一段 shell，回 stdout（非零退出即断言失败，把 stderr 一起摊开）。
    ///
    /// 跳板链测试要的路径证据只能从**服务端**取：我们自己的 status 播报是「打算走哪条路」，
    /// 而容器内的连接表是「实际有几条链路建起来了」——后者才能证伪。
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

    /// 轮询等 sshd 重新可用：连上并读到 `SSH-` banner 才算数。
    /// 只判 TCP connect 成功不够——Docker 的端口转发进程在容器内服务已停时仍可能接受连接后立刻断开，
    /// 那会让紧随其后的握手以「连接被重置」形式假红。
    async fn wait_for_sshd_banner(&self) {
        use tokio::io::AsyncReadExt;
        let addr = self.addr().await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
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
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        panic!("sshd 重启后 60s 内未恢复：{last}");
    }

    /// S26：`get_host()` 返回的是 `url::Host` 而**不是** IP —— Docker Desktop（Windows/macOS，
    /// 经 npipe / unix socket 连 daemon）下它是域名字面量 `localhost`，而 `SocketAddr` 的
    /// `FromStr` 对任何域名一律 `AddrParseError`。因此 `format!("{host}:{port}").parse()` 会让
    /// **全部**容器测试在开发机上 100% panic 在助手内部（本机 Docker 29.6.1 实测：
    /// `called Result::unwrap() on an Err value: AddrParseError(Socket)`，4/4 全红）。
    /// 先按 IP 字面量解析（Linux CI 常见的 `127.0.0.1` 直接命中，零 DNS），失败再走异步 DNS。
    ///
    /// DNS 结果**只取 IPv4**：端口取自 `get_host_port_ipv4`，那是 IPv4 侧的映射；
    /// `localhost` 在双栈机器上常先解析出 `::1`，据此拼出的地址指向一个根本没有映射的端口，
    /// 表现为连接被拒/挂起 —— 是比 parse 失败更难查的假失败。
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
}
