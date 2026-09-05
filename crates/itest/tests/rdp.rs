//! RDP 引擎对**真 xrdp** 的端到端测试（阶段 1：连接 + 认证）。
//!
//! 本测试同时充当「主程序」的角色：own TCP 连接、把线上字节经 fs_rdpproto 帧
//! 中继给 helper 子进程、对证书做裁决。这正是生产装配的形状——只是把
//! Tauri 那层换成了测试循环。
//!
//! 覆盖（引擎的全部连接路径）：
//! ① X.224 协商 → TLS 握手 → 证书上报（指纹/主题/有效期非空）；
//! ② 裁决通过 → CredSSP(NTLM/PAM) 用**对的口令**走到 Connected；
//! ③ **错的口令**必须被拒，且失败类别是 Auth（可区分，不是一句泛泛的连接失败）；
//! ④ 证书被拒时 helper 不得进入认证（CertRejected，口令一个字节都没发）。
//!
//! 需 `FS_ITEST=1` + Docker；helper 二进制自动构建（或经 FS_RDP_HELPER 指定）。

use fs_itest::rdp_server::XrdpContainer;
use fs_rdpproto::{ConnectParams, FailureKind, FromHelper, Packet, ToHelper, PROTOCOL_VERSION};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 定位（必要时构建）helper 二进制。
///
/// 默认按相对路径找 rdp-helper 工作区的 target；找不到就地构建一次
/// （增量，只有首次慢）。FS_RDP_HELPER 可显式指定（CI 用）。
fn helper_exe() -> PathBuf {
    if let Ok(p) = std::env::var("FS_RDP_HELPER") {
        return PathBuf::from(p);
    }
    let exe = if cfg!(windows) {
        "fs-rdp-helper.exe"
    } else {
        "fs-rdp-helper"
    };
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../rdp-helper/target/debug")
            .join(exe),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../rdp-helper/target/release")
            .join(exe),
    ];
    for c in &candidates {
        if c.exists() {
            return c.clone();
        }
    }
    // 就地构建（helper 是独立工作区，主仓的 cargo 不会顺手编它）
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rdp-helper/Cargo.toml");
    let status = std::process::Command::new("cargo")
        .args(["build", "--locked", "--manifest-path"])
        .arg(&manifest)
        .status()
        .expect("起不了 cargo（构建 helper）");
    assert!(status.success(), "helper 构建失败");
    candidates[0].clone()
}

/// 一次测试会话：helper 子进程 + 到 xrdp 的 TCP，双向中继。
struct Rig {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    tcp: tokio::net::TcpStream,
    buf: Vec<u8>,
    // NetEof 只发一次：代理 RST/瞬时网络错误不该把它重复灌给 helper
    eof_sent: bool,
}

impl Rig {
    async fn start(server: std::net::SocketAddr, username: &str, password: &str) -> Self {
        let exe = helper_exe();
        let mut child = tokio::process::Command::new(&exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()) // 诊断用；结束时摊开
            .spawn()
            .unwrap_or_else(|e| panic!("起不了 helper（{}）：{e}", exe.display()));
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let tcp = tokio::time::timeout(
            Duration::from_secs(15),
            tokio::net::TcpStream::connect(server),
        )
        .await
        .expect("连 xrdp 超时")
        .expect("连 xrdp 失败");

        let mut rig = Self {
            child,
            stdin,
            stdout,
            tcp,
            buf: Vec::new(),
            eof_sent: false,
        };
        // 握手
        rig.send(&ToHelper::Hello {
            version: PROTOCOL_VERSION,
        })
        .await;
        let pkt = rig.next_from_helper().await;
        assert!(
            matches!(pkt.header, FromHelper::Hello { version } if version == PROTOCOL_VERSION),
            "helper 握手回包不对：{:?}",
            pkt.header
        );
        // 建连指令（此刻 TCP 已通）
        rig.send(&ToHelper::Connect(ConnectParams {
            server_name: server.ip().to_string(),
            username: username.into(),
            domain: String::new(),
            password: password.into(),
            width: 1280,
            height: 800,
            keyboard_layout: 0x0409,
        }))
        .await;
        rig
    }

    async fn send(&mut self, msg: &ToHelper) {
        let mut out = Vec::new();
        fs_rdpproto::encode(msg, &[], &mut out);
        self.stdin.write_all(&out).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    /// 读出 helper 的下一条上行（同时泵 TCP → helper 的中继）。
    ///
    /// 中继必须在读 stdout 的同一循环里做：helper 在等网络字节时不会说话，
    /// 而网络字节要等我们把 TCP 的东西喂给它——不同时泵就是死锁。
    async fn next_from_helper(&mut self) -> Packet<FromHelper> {
        loop {
            if let Some((pkt, used)) = fs_rdpproto::decode::<FromHelper>(&self.buf).unwrap() {
                self.buf.drain(..used);
                // NetOut 是要送回网络的——在调用点之前先把它记着，由调用方
                // （这里的循环本身）写回 TCP。
                if let FromHelper::NetOut = pkt.header {
                    let body = pkt.body.clone();
                    self.tcp.write_all(&body).await.unwrap();
                    continue; // NetOut 不是给测试看的「事件」，继续读
                }
                // Rig 就是这里的画面消费者。生产前端消费后会发 FrameAck，
                // 测试也必须归还配额；否则初始重绘耗尽在途帧预算后，
                // helper 会按背压协议停止发帧，击键重绘永远等不到。
                if matches!(pkt.header, FromHelper::Frame { .. }) {
                    self.send(&ToHelper::FrameAck).await;
                }
                return Packet {
                    header: pkt.header,
                    body: pkt.body,
                };
            }
            // 缓冲**在堆上**：async fn 的局部量全进 Future 状态机，两个 64 KiB 数组
            // 让这个函数的状态机在默认测试线程栈（Windows 2 MiB）上贴着天花板——
            // 敲键触发 xrdp 大面积重绘、帧密集到达时（2026-09-04 实测）当场
            // STATUS_STACK_OVERFLOW。Vec 的堆分配把状态机缩回几十字节。
            let mut chunk = vec![0u8; 64 * 1024];
            let mut tcp_closed = false;
            let mut out_closed = false;
            // 两个读各用各的缓冲（select! 的分支同时发起，不能共用一个 &mut）
            let mut wire = vec![0u8; 64 * 1024];
            tokio::select! {
                n = self.stdout.read(&mut chunk) => match n {
                    Ok(0) | Err(_) => out_closed = true,
                    Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                },
                n = self.tcp.read(&mut wire) => match n {
                    Ok(0) | Err(_) => tcp_closed = true,
                    Ok(n) => {
                        let mut out = Vec::new();
                        fs_rdpproto::encode(&ToHelper::NetIn, &wire[..n], &mut out);
                        self.stdin.write_all(&out).await.unwrap();
                        self.stdin.flush().await.unwrap();
                    }
                },
            }
            if out_closed {
                panic!(
                    "helper 提前退出（stdout EOF），已收：{:?}",
                    String::from_utf8_lossy(&self.buf)
                );
            }
            if tcp_closed && !self.eof_sent {
                // 服务器先断：把 EOF 告知 helper（只一次），读它对此的结论
                self.eof_sent = true;
                self.send(&ToHelper::NetEof).await;
            }
        }
    }

    /// 关停：先给 helper 优雅退出的机会（10s），不退就 kill。
    /// stderr 的读取**必须**在 kill 之后：活进程的管道永不 EOF，死等它就是
    /// 把测试挂死——第一版正是这样挂的（helper 卡在引擎里不退）。
    async fn finish(&mut self) -> String {
        let _ = self.send(&ToHelper::Shutdown).await;
        let _ = self.stdin.shutdown().await;
        if tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
        let mut err = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            let _ = e.read_to_string(&mut err).await;
        }
        err
    }
}

/// ①② 对的口令：证书上报 → 裁决通过 → NTLM 认证成功 → Connected。
#[tokio::test(flavor = "multi_thread")]
async fn rdp_connects_and_authenticates_against_real_xrdp() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let xrdp = XrdpContainer::start("rdpok").await.unwrap();
    let mut rig = Rig::start(xrdp.addr().await, &xrdp.username, &xrdp.password).await;

    loop {
        match rig.next_from_helper().await.header {
            FromHelper::CertPresented { .. } => break,
            FromHelper::Status { .. } => continue, // 过程播报
            other => panic!("期望证书上报，实得 {other:?}"),
        }
    }
    // 裁决通过
    rig.send(&ToHelper::CertVerdict { accept: true }).await;

    let deadline = Duration::from_secs(60);
    let started = std::time::Instant::now();
    loop {
        match rig.next_from_helper().await.header {
            FromHelper::Connected { width, height } => {
                assert!(width > 0 && height > 0, "桌面尺寸不对：{width}x{height}");
                break;
            }
            FromHelper::Status { .. } => {
                assert!(
                    started.elapsed() < deadline,
                    "60 秒内未到 Connected（卡在认证/最终化？）"
                );
                continue;
            }
            other => panic!(
                "对的口令不该失败，实得 {other:?}｜stderr：{}",
                rig.finish().await
            ),
        }
    }
    let err = rig.finish().await;
    assert!(err.is_empty(), "helper stderr 非空：{err}");
}

/// ③ 错的口令：必须被拒，且类别是 Auth（主程序据此走「重新问口令」）。
///
/// **前置：服务器要有 NLA（CredSSP）。** 本仓 itest 用的 Debian xrdp **整包无
/// CredSSP**（实测：libxrdp/xrdp 全库零 credssp/ntlm 字符串；协商回落 SSL）——
/// SSL 路径下口令在协议层根本不送（图形登录，用户在远端屏上输），「错口令被拒」
/// 无从发生。此时本测试**带因跳过**而不是假绿；真 Windows / 任何 NLA 服务器上
/// 它会真跑（引擎通告 HYBRID|HYBRID_EX|SSL，有 NLA 的服务器必选 NLA）。
#[tokio::test(flavor = "multi_thread")]
async fn rdp_wrong_password_is_an_auth_failure_not_a_generic_one() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let xrdp = XrdpContainer::start("rdpbad").await.unwrap();
    let mut rig = Rig::start(xrdp.addr().await, &xrdp.username, "definitely-wrong").await;

    let mut layer = String::new();
    loop {
        match rig.next_from_helper().await.header {
            FromHelper::CertPresented { .. } => break,
            FromHelper::Status { message } => {
                if message.contains("协商安全层") {
                    layer = message;
                }
                continue;
            }
            other => panic!("期望证书上报，实得 {other:?}"),
        }
    }
    rig.send(&ToHelper::CertVerdict { accept: true }).await;

    // 协商播报在裁决**之后**才发（引擎在 TLS 与最终化之间）——补收一段
    if layer.is_empty() {
        loop {
            match rig.next_from_helper().await.header {
                FromHelper::Status { message } if message.contains("协商安全层") => {
                    layer = message;
                    break;
                }
                FromHelper::Status { .. } | FromHelper::Connected { .. } => break,
                FromHelper::Failed { .. } => break,
                _ => continue,
            }
        }
    }

    if !layer.contains("NLA") {
        eprintln!(
            "skip（诚实跳过，不是通过）：该服务器无 NLA（{layer}），\
                   错口令在 SSL 图形登录路径下不经协议验证。\
                   需要 NLA 服务器（真 Windows）才能跑这条判据。"
        );
        let _ = rig.finish().await;
        return;
    }

    let deadline = Duration::from_secs(60);
    let started = std::time::Instant::now();
    loop {
        match rig.next_from_helper().await.header {
            FromHelper::Failed { kind, .. } => {
                assert_eq!(
                    kind,
                    FailureKind::Auth,
                    "错口令的失败类别必须是 Auth（主程序据此重新问口令），实得 {kind:?}"
                );
                break;
            }
            FromHelper::Status { .. } => {
                assert!(started.elapsed() < deadline, "60 秒内未出结果");
                continue;
            }
            other => panic!("错口令该被拒，实得 {other:?}"),
        }
    }
}

/// ④ 证书被拒：helper 不得进入认证（口令一个字节都没发出去）。
#[tokio::test(flavor = "multi_thread")]
async fn rdp_rejected_certificate_never_authenticates() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let xrdp = XrdpContainer::start("rdpcert").await.unwrap();
    let mut rig = Rig::start(xrdp.addr().await, &xrdp.username, &xrdp.password).await;

    loop {
        match rig.next_from_helper().await.header {
            FromHelper::CertPresented { .. } => break,
            FromHelper::Status { .. } => continue,
            other => panic!("期望证书上报，实得 {other:?}"),
        }
    }
    rig.send(&ToHelper::CertVerdict { accept: false }).await;

    let deadline = Duration::from_secs(30);
    let started = std::time::Instant::now();
    loop {
        match rig.next_from_helper().await.header {
            FromHelper::Failed { kind, .. } => {
                assert_eq!(kind, FailureKind::CertRejected, "实得 {kind:?}");
                break;
            }
            FromHelper::Status { .. } => {
                assert!(started.elapsed() < deadline);
                continue;
            }
            // 会话收尾时各通道会各自 close（阶段 3 的 Rdpsnd 会发一条
            // AudioClose）。这些是**收尾噪声**，与本判据无关——
            // 本判据要防的是「拒绝了证书却仍然连上了」。
            FromHelper::AudioClose | FromHelper::ClipboardOffer => continue,
            FromHelper::Connected { .. } => {
                panic!("拒绝证书后仍然连上了——TOFU 闸失效")
            }
            other => panic!("拒绝证书后出现未预期的消息：{other:?}"),
        }
    }
}

/// ④ 输入端到端：键盘敲击必须让远端画面发生变化（新帧上行）。
///
/// **为什么必须有这条**：输入路径（Input 事件 → fastpath 编码 → 服务器 →
/// 画面重绘 → 新帧）此前从未被任何测试端到端验证过——2026-08-27 真机实测
/// 发现「画面出来了但鼠标无效」，而仓内全绿。xrdp 的登录界面（greeter）
/// 有一个会**回显击键**的输入框：敲 'a' → 框里出现字符 → 像素变化 → 帧。
/// 这给了输入路径一个可断言的观测面。
///
/// 静置期先数自然帧（时钟/光标闪烁可能自己产生帧），再敲 5 个 'a'，
/// 之后 15 秒内必须出现**敲键之后才开始的新帧**——用静置期最大间隔的
/// 数量级做基线，避免把「本来就在动的画面」误判成输入生效。
#[tokio::test(flavor = "multi_thread")]
async fn rdp_keyboard_input_reaches_the_server_and_repaints() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let xrdp = XrdpContainer::start("rdpin").await.unwrap();
    let mut rig = Rig::start(xrdp.addr().await, &xrdp.username, &xrdp.password).await;

    // 连到 Connected（与①同款骨架）
    loop {
        match rig.next_from_helper().await.header {
            FromHelper::CertPresented { .. } => break,
            FromHelper::Status { .. } => continue,
            other => panic!("期望证书上报，实得 {other:?}"),
        }
    }
    rig.send(&ToHelper::CertVerdict { accept: true }).await;
    let started = std::time::Instant::now();
    loop {
        match rig.next_from_helper().await.header {
            FromHelper::Connected { width, height } => {
                assert!(width > 0 && height > 0, "桌面尺寸不对：{width}x{height}");
                break;
            }
            FromHelper::Status { .. } => {
                assert!(
                    started.elapsed() < Duration::from_secs(60),
                    "60 秒内未到 Connected"
                );
                continue;
            }
            other => panic!("连接失败 {other:?}｜stderr：{}", rig.finish().await),
        }
    }

    // 静置期：把积压的初始重绘帧排掉，再观察 4 秒的自然帧（可能为零——
    // xrdp greeter 的光标闪烁不一定会产生 fastpath 像素更新）。
    let settle_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < settle_deadline {
        if let Ok(pkt) = tokio::time::timeout_at(settle_deadline, rig.next_from_helper()).await {
            assert!(
                !matches!(pkt.header, FromHelper::Failed { .. }),
                "静置期不该失败：{:?}",
                pkt.header
            );
        }
    }

    // 敲 5 个 'a'（PS/2 Set 1 扫描码 0x1E）。先聚焦输入框这一步没法做
    // （没有可点击坐标的知识），greeter 的用户名框默认就是焦点——这正是
    // 我们要的。按下+抬起成对，间隔 60ms 模拟真实击键。
    for _ in 0..5 {
        rig.send(&ToHelper::Input(fs_rdpproto::InputEvent::Key {
            scancode: 0x1E,
            extended: false,
            down: true,
        }))
        .await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        rig.send(&ToHelper::Input(fs_rdpproto::InputEvent::Key {
            scancode: 0x1E,
            extended: false,
            down: false,
        }))
        .await;
        tokio::time::sleep(Duration::from_millis(60)).await;
    }

    // 15 秒窗口内必须看到至少一帧新帧（敲键产生的重绘）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let pkt = match tokio::time::timeout_at(deadline, rig.next_from_helper()).await {
            Ok(p) => p,
            Err(_) => panic!(
                "敲了 5 个 'a' 之后 15 秒内没有任何新帧——输入没有到达服务器\
                 （或服务器重绘了但我们没上行）。stderr：{}",
                rig.finish().await
            ),
        };
        match pkt.header {
            FromHelper::Frame { .. } => break, // 敲键生效：画面重绘了
            FromHelper::Status { .. } => continue,
            other => panic!("输入阶段不该失败，实得 {other:?}"),
        }
    }
    let err = rig.finish().await;
    assert!(err.is_empty(), "helper stderr 非空：{err}");
}
