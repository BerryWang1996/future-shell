//! 隧道（本地端口转发 `-L`）的容器集成测试
//! （M4a 出口标准：「对容器 sshd 的本地端口转发端到端测试通过」）。
//!
//! 判据是**真实字节穿过整条链**：本机 TCP 客户端 → 本地监听端口 → direct-tcpip 通道
//! → 容器内的服务 → 回程。单测里的 EchoOpener 验的是转发循环自己（accept、对拷、停止），
//! 它换不来「真实 SSH 通道上也成立」这句话：direct-tcpip 的目标地址在容器网络命名空间里
//! 解析、AllowTcpForwarding 默认是关的、半关闭（EOF 单向）在真通道上才有意义。
//!
//! 容器内的「服务」用 sh + nc 起一个一次性回显器。选它而不是复用 sshd 端口：回显能逐字节
//! 比对内容，而对着 sshd 只能看到一行 banner——那分辨不出「通道通了」与「通道通到了别处」。

use fs_itest::sshd::SshdContainer;
use fs_sshengine::tunnel::{start_local_forward, ChannelOpener, TunnelSpec, TunnelStream};
use russh::client;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct AcceptAllKeys;
impl russh::client::Handler for AcceptAllKeys {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _k: &russh::keys::PublicKey) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// 与生产 `RegistryOpener` 同形的 opener：每次 open 都在**当前**会话句柄上开通道。
struct HandleOpener(Arc<tokio::sync::Mutex<client::Handle<AcceptAllKeys>>>);

impl ChannelOpener for HandleOpener {
    fn open(
        &self,
        host: String,
        port: u16,
        origin: SocketAddr,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<TunnelStream, String>> + Send + '_>,
    > {
        Box::pin(async move {
            let handle = self.0.lock().await;
            let channel = handle
                .channel_open_direct_tcpip(
                    host,
                    port as u32,
                    origin.ip().to_string(),
                    origin.port() as u32,
                )
                .await
                .map_err(|e| format!("开 direct-tcpip 通道失败：{e}"))?;
            drop(handle);
            Ok(Box::new(channel.into_stream()) as TunnelStream)
        })
    }
}

const ECHO_PORT: u16 = 7777;

/// 出口标准整条：真实 sshd 上的 `-L`，字节逐字相等。
#[tokio::test(flavor = "multi_thread")]
async fn local_forward_carries_bytes_through_real_sshd() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("tunnel").await.unwrap();
    // 镜像默认 AllowTcpForwarding no：不开则 direct-tcpip 一律
    // ChannelOpenFailure(AdministrativelyProhibited)（与 ssh_jump 同一前置）
    sshd.enable_tcp_forwarding().await.unwrap();

    // 容器内起回显服务。必须用 **busybox nc** 而不是裸 `nc`：这个镜像里的 `nc` 是
    // OpenBSD netcat（实测其 -h 里没有 `-e`），而 busybox 的 nc 有 `-e PROG` 与 `-lk`
    // （持久监听）。写成 `nc -l -p N -e /bin/cat` 会静默不起来，随后隧道第一次连接撞在
    // 「没人监听」上——那个失败看起来像转发被拒，排查方向全错。
    //
    // 就绪判据只匹配端口号、不带行尾空格：busybox nc 绑的是 `:::7777`（IPv6 通配，
    // v4 映射同样可达），而 `grep ':7777 '` 依赖 netstat 的列宽与行尾空格——实测不命中，
    // 于是服务明明在听却被判成起不来。
    sshd.run(&format!(
        "(setsid busybox nc -lk -p {ECHO_PORT} -e /bin/cat >/dev/null 2>&1 &) ; \
         for i in $(seq 1 50); do (netstat -ltn 2>/dev/null || ss -ltn) | grep -q ':{ECHO_PORT}' && exit 0; sleep 0.2; done; \
         echo 'echo server did not come up' >&2; (netstat -ltn 2>/dev/null || ss -ltn) >&2; exit 1"
    ))
    .await
    .unwrap();

    let mut session = client::connect(
        Arc::new(client::Config::default()),
        sshd.addr().await,
        AcceptAllKeys,
    )
    .await
    .unwrap();
    assert!(session
        .authenticate_password(&sshd.username, &sshd.password)
        .await
        .unwrap()
        .success());
    let handle = Arc::new(tokio::sync::Mutex::new(session));

    // 端口自己挑：`local_port = 0` 被 validate_spec 显式拒绝（随机端口无法告知使用方，
    // 那是产品决定而非缺陷），而硬编码一个端口会在开发机上与真实服务撞车——那种失败
    // （Address already in use）与被测行为无关，却看起来像转发起不来。
    let port = free_local_port().await;
    let running = start_local_forward(
        Arc::new(HandleOpener(handle.clone())),
        TunnelSpec {
            id: "itest-tunnel".into(),
            local_port: port,
            remote_host: "127.0.0.1".into(), // 容器自身（direct-tcpip 由服务端发起）
            remote_port: ECHO_PORT,
            bind_all: false,
        },
    )
    .await
    .expect("启动本地转发失败");
    let local = running.local_addr;

    // 经隧道往回显服务写一段带中文的字节，读回逐字比对
    let payload = "hello-隧道-🚇\n";
    let mut client_sock = tokio::net::TcpStream::connect(local).await.unwrap();
    client_sock.write_all(payload.as_bytes()).await.unwrap();
    client_sock.flush().await.unwrap();

    let mut got = vec![0u8; payload.len()];
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        client_sock.read_exact(&mut got),
    )
    .await
    .expect("经隧道读回超时（通道可能没接到容器内的回显服务）")
    .expect("读回失败");
    assert_eq!(
        String::from_utf8_lossy(&got),
        payload,
        "经隧道回来的字节必须与发出的逐字相等"
    );
    assert_eq!(running.accepted_count(), 1, "应恰好接受了一条本地连接");

    // 停止后监听须真的退出。判据用「端口可被重新绑定」而不是「连不上」——
    // 后者在 Windows 上会假绿又假红：accept 循环已退出时，内核仍可能从 listen backlog
    // 里完成三次握手（于是 connect 成功，但没人服务它）。能重新 bind 才证明 listener
    // 已被 drop。与单测 `stop_stops_accepting` 同一判据。
    running.stop();
    drop(running); // listener 由 accept 任务持有，stop 后它退出即释放
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut rebound = false;
    while tokio::time::Instant::now() < deadline {
        if tokio::net::TcpListener::bind(local).await.is_ok() {
            rebound = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert!(rebound, "stop 后监听未退出（端口 {local} 仍被占用）");
}

/// 目标端口无人监听时，隧道**如实报错**而不是让客户端挂着。
///
/// 这一条只有真服务器能验：服务端对「连不上目标」回 SSH_MSG_CHANNEL_OPEN_FAILURE，
/// 而内存 fake 的 opener 想回什么都行。用户视角的判据是「连上本地端口后立刻断开」
/// 而不是「一直连着但什么也没发生」——后者会让人以为服务卡了，去查错的地方。
#[tokio::test(flavor = "multi_thread")]
async fn local_forward_reports_dead_target_instead_of_hanging() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("tunneldead").await.unwrap();
    sshd.enable_tcp_forwarding().await.unwrap();
    let mut session = client::connect(
        Arc::new(client::Config::default()),
        sshd.addr().await,
        AcceptAllKeys,
    )
    .await
    .unwrap();
    assert!(session
        .authenticate_password(&sshd.username, &sshd.password)
        .await
        .unwrap()
        .success());
    let handle = Arc::new(tokio::sync::Mutex::new(session));

    let running = start_local_forward(
        Arc::new(HandleOpener(handle)),
        TunnelSpec {
            id: "itest-tunnel-dead".into(),
            local_port: free_local_port().await,
            remote_host: "127.0.0.1".into(),
            remote_port: 9, // discard 端口，容器里没人听
            bind_all: false,
        },
    )
    .await
    .unwrap();

    let mut sock = tokio::net::TcpStream::connect(running.local_addr)
        .await
        .unwrap();
    // 写入可能成功（本地 socket 缓冲），但读必须很快见到 EOF/错误——不能永远挂着
    let _ = sock.write_all(b"ping\n").await;
    let mut buf = [0u8; 16];
    let r = tokio::time::timeout(std::time::Duration::from_secs(15), sock.read(&mut buf)).await;
    match r {
        Ok(Ok(0)) => {}  // EOF：通道开失败后本地侧被关掉，正是期望
        Ok(Err(_)) => {} // 连接被重置，同样是「明确失败」
        Ok(Ok(n)) => panic!("目标无人监听却读到了 {n} 字节：{:?}", &buf[..n]),
        Err(_) => panic!("目标无人监听时本地连接一直挂着——用户会以为服务卡了，去查错的地方"),
    }
    running.stop();
}

/// 挑一个当下空闲的本地端口：bind(0) 拿到号码后立刻释放，再交给被测代码去绑。
///
/// 这中间有一个 TOCTOU 窗口（别的进程可能抢先绑上），但它比硬编码端口好得多：
/// 硬编码在开发机上**必然**与某个常驻服务撞车，而这个窗口只有几毫秒。
async fn free_local_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap().port()
}
