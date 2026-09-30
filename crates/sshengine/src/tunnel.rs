//! SSH 隧道 / 端口转发（M4a：隧道管理器；路线图 §M4）。
//!
//! 本模块只做**本地转发**（OpenSSH 的 `-L`）：在本机监听一个端口，每个进来的
//! TCP 连接开一条 `direct-tcpip` SSH 通道，双向对拷到远端 `host:port`。
//!
//! ## 为什么先只做本地转发
//!
//! 远程转发（`-R`）需要处理服务端反向发起的通道（`server_channel_open_forwarded_tcpip`
//! 回调），而那要动 `connect.rs` 的 Handler——那是认证/主机密钥的关键路径，改它的
//! 风险与本任务不成比例。动态转发（`-D`，SOCKS5）还要在本机实现 SOCKS 协商。
//! 两者留作后续增量，**此处如实标注不做**，不假装隧道功能已完整。
//!
//! ## 三条设计约束
//!
//! **① 监听地址默认只绑 127.0.0.1。** 绑 `0.0.0.0` 会把「我这台机器到生产库的
//! 隧道」变成同网段任何人都能用的跳板——这是转发功能最典型的事故形态。要绑全网卡
//! 必须显式配置 `bind_all`，且 UI 上写明后果。
//!
//! **② 每条连接一条通道、彼此独立。** 一条连接崩了不影响别的；单条通道的错误
//! 只记日志不拆隧道（隧道是长期设施，不该因为某个客户端半途断开就整体下线）。
//!
//! **③ 隧道生命周期绑会话。** 会话关闭/断线即停所有监听：SSH 连接没了，隧道
//! 只会 accept 出一堆立刻失败的连接，留着比停掉更误导。

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Notify;

/// 一条隧道的配置（本地转发）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TunnelSpec {
    /// 稳定 id（前端列表键、停止时的句柄）
    pub id: String,
    /// 本机监听端口
    pub local_port: u16,
    /// 远端目标主机（**在服务端解析**：`localhost` 指的是服务端自己）
    pub remote_host: String,
    /// 远端目标端口
    pub remote_port: u16,
    /// 是否绑全网卡（默认 false = 只绑 127.0.0.1，见模块头①）
    #[serde(default)]
    pub bind_all: bool,
}

/// 隧道校验错误（人可读中文，直出 UI）。
pub fn validate_spec(spec: &TunnelSpec) -> Result<(), String> {
    if spec.local_port == 0 {
        return Err("本地端口不能为 0（0 表示由系统随机分配，隧道地址将无法告知使用方）".into());
    }
    if spec.remote_port == 0 {
        return Err("远端端口不能为 0".into());
    }
    let host = spec.remote_host.trim();
    if host.is_empty() {
        return Err("远端主机不能为空".into());
    }
    if host.chars().count() > 253 {
        return Err("远端主机名超过 253 字符（DNS 上限）".into());
    }
    // 控制字符/空白会被拼进 direct-tcpip 请求，服务端行为未定义
    if host.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("远端主机名含空白或控制字符".into());
    }
    Ok(())
}

/// 监听地址（见模块头①：默认只绑回环）。
pub fn bind_addr(spec: &TunnelSpec) -> SocketAddr {
    let ip = if spec.bind_all {
        std::net::IpAddr::from([0, 0, 0, 0])
    } else {
        std::net::IpAddr::from([127, 0, 0, 1])
    };
    SocketAddr::new(ip, spec.local_port)
}

/// 一条运行中的隧道的句柄。
#[derive(Debug)]
pub struct RunningTunnel {
    pub spec: TunnelSpec,
    /// 实际绑定到的地址（端口与 spec 相同；保留完整地址便于 UI 显示与测试断言）
    pub local_addr: SocketAddr,
    /// 停止请求：**标志 + 通知**两件套。
    ///
    /// 只用 `Notify::notify_waiters()` 是有洞的：它只唤醒**此刻已在等**的等待者。
    /// accept 循环在「取到一条连接 → spawn 桥接任务 → 回到 select」这段窗口里不是
    /// 等待者，此时到来的 stop 信号会被彻底丢掉——隧道看起来停了（UI 已移除条目）
    /// 却还在监听端口。故置标志在前、通知在后；循环每轮先读标志。
    stopped: Arc<std::sync::atomic::AtomicBool>,
    stop: Arc<Notify>,
    /// 累计接受的连接数（诊断用；UI 可显示「已转发 N 条连接」）
    accepted: Arc<std::sync::atomic::AtomicU64>,
}

impl RunningTunnel {
    pub fn accepted_count(&self) -> u64 {
        self.accepted.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 停止监听。已建立的连接**不强断**：正在传输的会话中途被掐比多活几秒更糟；
    /// 它们随 SSH 通道自然结束（会话关闭时整条连接一起走）。
    pub fn stop(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.stop.notify_waiters();
    }
}

/// 开一条 direct-tcpip 通道的能力（依赖注入）。
///
/// 不直接收 `Arc<Handle<H>>` 的理由：app 层把连接句柄存成
/// `Mutex<russh::client::Handle<Connector>>`（russh 0.62 的 Handle **非 Clone**，
/// 见 sessions.rs 的 LiveSession 注记），拿不出 `Arc<Handle>`。收一个「怎么开通道」
/// 的闭包，则本模块不关心句柄怎么存、锁怎么持——桥接逻辑与句柄所有权解耦，
/// 测试也能用假 opener 跑通监听/停止/计数而不需要真 SSH 连接。
pub trait ChannelOpener: Send + Sync + 'static {
    /// 开一条到 `host:port` 的 direct-tcpip 通道；`origin` 为真实来源（RFC 4254 §7.2）。
    fn open(
        &self,
        host: String,
        port: u16,
        origin: SocketAddr,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<TunnelStream, String>> + Send + '_>,
    >;
}

/// 隧道的远端一侧：可读可写的双向流（生产实现即 russh `ChannelStream`）。
pub type TunnelStream = Box<dyn TunnelIo>;

/// `AsyncRead + AsyncWrite + Send + Unpin` 的对象安全别名。
pub trait TunnelIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> TunnelIo for T {}

/// 启动一条本地转发隧道。
///
/// `opener` 提供「怎么开 direct-tcpip 通道」（见 [`ChannelOpener`]）。绑定失败
/// （端口被占）立即返回 Err——这是用户要马上知道的事（换个端口即可），
/// 不该变成后台静默失败。
pub async fn start_local_forward(
    opener: Arc<dyn ChannelOpener>,
    spec: TunnelSpec,
) -> Result<RunningTunnel, String> {
    validate_spec(&spec)?;
    let addr = bind_addr(&spec);
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("监听 {addr} 失败：{e}（端口可能已被占用）"))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| format!("取监听地址失败：{e}"))?;
    let stop = Arc::new(Notify::new());
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let accepted = Arc::new(std::sync::atomic::AtomicU64::new(0));

    let stop_c = stop.clone();
    let stopped_c = stopped.clone();
    let accepted_c = accepted.clone();
    let spec_c = spec.clone();
    tokio::spawn(async move {
        loop {
            // 先读标志：stop 落在「上一轮 accept 返回 → 回到 select」的窗口里时，
            // notify 没有等待者可唤醒（见 RunningTunnel.stopped 的注记）
            if stopped_c.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            let (sock, peer) = tokio::select! {
                // 停止优先：置位后不再 accept（已建立的连接不受影响，见 stop 文档）
                _ = stop_c.notified() => break,
                r = listener.accept() => match r {
                    Ok(v) => v,
                    Err(e) => {
                        // accept 错误多为瞬时（fd 耗尽/连接已 RST）：记一条继续，
                        // 不因单次失败拆掉长期设施
                        tracing::warn!(port = spec_c.local_port, %e, "隧道 accept 失败");
                        continue;
                    }
                },
            };
            accepted_c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let opener_c = opener.clone();
            let spec_i = spec_c.clone();
            // 每条连接独立任务：一条崩了不影响别的（模块头②）
            tokio::spawn(async move {
                if let Err(e) = bridge_one(opener_c, &spec_i, sock, peer).await {
                    tracing::warn!(
                        port = spec_i.local_port,
                        target = %format!("{}:{}", spec_i.remote_host, spec_i.remote_port),
                        %e,
                        "隧道连接结束（异常）"
                    );
                }
            });
        }
        tracing::info!(port = spec_c.local_port, "隧道已停止监听");
    });

    tracing::info!(
        local = %local_addr,
        target = %format!("{}:{}", spec.remote_host, spec.remote_port),
        "隧道已启动（本地转发）"
    );
    Ok(RunningTunnel {
        spec,
        local_addr,
        stopped,
        stop,
        accepted,
    })
}

/// 把一个已接受的 TCP 连接桥到一条 direct-tcpip 通道上。
async fn bridge_one(
    opener: Arc<dyn ChannelOpener>,
    spec: &TunnelSpec,
    mut sock: tokio::net::TcpStream,
    peer: SocketAddr,
) -> Result<(), String> {
    let mut stream = opener
        .open(spec.remote_host.clone(), spec.remote_port, peer)
        .await?;
    // 双向对拷：任一方向 EOF 即收尾（copy_bidirectional 的语义），随后两端各自 drop
    tokio::io::copy_bidirectional(&mut sock, &mut stream)
        .await
        .map(|_| ())
        .map_err(|e| format!("隧道数据对拷中断：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> TunnelSpec {
        TunnelSpec {
            id: "t1".into(),
            local_port: 18022,
            remote_host: "localhost".into(),
            remote_port: 22,
            bind_all: false,
        }
    }

    /// S323：默认只绑回环——绑 0.0.0.0 会把隧道变成同网段任何人可用的跳板。
    #[test]
    fn binds_loopback_unless_explicitly_all() {
        assert_eq!(bind_addr(&spec()).ip().to_string(), "127.0.0.1");
        let all = TunnelSpec {
            bind_all: true,
            ..spec()
        };
        assert_eq!(bind_addr(&all).ip().to_string(), "0.0.0.0");
        // 端口如实透传（写错这条会让「我配的 18022」监听到别处）
        assert_eq!(bind_addr(&spec()).port(), 18022);
    }

    /// S324：校验逐条点名违规（只认 is_err 的弱见证会让某道检查被悄悄删掉）。
    #[test]
    fn validate_names_each_violation() {
        assert!(validate_spec(&spec()).is_ok());

        let e = validate_spec(&TunnelSpec {
            local_port: 0,
            ..spec()
        })
        .unwrap_err();
        assert!(e.contains("本地端口"), "{e}");
        let e = validate_spec(&TunnelSpec {
            remote_port: 0,
            ..spec()
        })
        .unwrap_err();
        assert!(e.contains("远端端口"), "{e}");
        let e = validate_spec(&TunnelSpec {
            remote_host: "  ".into(),
            ..spec()
        })
        .unwrap_err();
        assert!(e.contains("不能为空"), "{e}");
        let e = validate_spec(&TunnelSpec {
            remote_host: "a".repeat(254),
            ..spec()
        })
        .unwrap_err();
        assert!(e.contains("253"), "{e}");
        let e = validate_spec(&TunnelSpec {
            remote_host: "ho st".into(),
            ..spec()
        })
        .unwrap_err();
        assert!(e.contains("空白"), "{e}");
        let e = validate_spec(&TunnelSpec {
            remote_host: "h\u{1}x".into(),
            ..spec()
        })
        .unwrap_err();
        assert!(e.contains("控制字符"), "{e}");
    }

    /// 序列化往返（前端列表与 settings 持久化共用此形状）。
    #[test]
    fn spec_json_roundtrip() {
        let s = spec();
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<TunnelSpec>(&json).unwrap(), s);
        // bind_all 缺省即 false（旧配置无此字段时不得反成 true——那会把回环
        // 默认悄悄变成全网卡）
        let legacy = r#"{"id":"t","local_port":1,"remote_host":"h","remote_port":2}"#;
        assert!(!serde_json::from_str::<TunnelSpec>(legacy).unwrap().bind_all);
    }

    /// 假 opener：把「远端」接到一个本地回显服务上——于是整条链路
    /// （监听 → accept → opener → copy_bidirectional）都真跑，只有 SSH 那一段被替换。
    struct EchoOpener {
        /// 记录每次 open 收到的目标与来源，断言 originator/目标透传正确
        seen: Arc<std::sync::Mutex<Vec<(String, u16, SocketAddr)>>>,
        echo_addr: SocketAddr,
    }

    impl ChannelOpener for EchoOpener {
        fn open(
            &self,
            host: String,
            port: u16,
            origin: SocketAddr,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<TunnelStream, String>> + Send + '_>,
        > {
            self.seen.lock().unwrap().push((host, port, origin));
            let addr = self.echo_addr;
            Box::pin(async move {
                let s = tokio::net::TcpStream::connect(addr)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(Box::new(s) as TunnelStream)
            })
        }
    }

    /// 起一个回显服务，返回其地址。
    async fn spawn_echo() -> SocketAddr {
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        addr
    }

    /// S325：端到端——本地端口收到的字节原样到达「远端」并回来。
    ///
    /// 这是隧道的**唯一**要紧判据：连得上不算，字节双向对得上才算。
    /// 本地端口取 0（系统分配）避免与开发机上的占用冲突——`local_addr`
    /// 回报真实端口，测试按它连。
    #[tokio::test]
    async fn local_forward_carries_bytes_both_ways() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let echo = spawn_echo().await;
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let opener = Arc::new(EchoOpener {
            seen: seen.clone(),
            echo_addr: echo,
        });
        // local_port 0 会被 validate_spec 拒（它是产品规则），故这里绕过校验直接
        // 用 bind_addr 的语义：给一个高位端口并容忍偶发占用——改用 0 需要放宽
        // 产品规则，不值得为测试便利动生产判据。
        let mut tunnel = None;
        for port in [18131u16, 18132, 18133, 18134] {
            let s = TunnelSpec {
                local_port: port,
                remote_host: "db.internal".into(),
                remote_port: 5432,
                ..spec()
            };
            if let Ok(t) = start_local_forward(opener.clone(), s).await {
                tunnel = Some(t);
                break;
            }
        }
        let tunnel = tunnel.expect("四个候选端口都被占用（本机环境异常）");

        let mut c = tokio::net::TcpStream::connect(tunnel.local_addr)
            .await
            .unwrap();
        c.write_all(b"ping-through-tunnel").await.unwrap();
        c.flush().await.unwrap();
        let mut buf = vec![0u8; 19];
        c.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping-through-tunnel", "隧道须逐字节双向透传");

        // 目标与来源如实透传给 opener（写错则服务端连到错误的主机/端口）
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "db.internal");
        assert_eq!(seen[0].1, 5432);
        assert_eq!(seen[0].2.ip().to_string(), "127.0.0.1");
        assert_eq!(tunnel.accepted_count(), 1, "接受计数须为 1");
    }

    /// `stopped` 标志防的那个窗口，单独钉一次。
    ///
    /// `stop()` 是「置标志 + notify_waiters」两件套，而 `notify_waiters` 只唤醒**此刻已在等**
    /// 的等待者。accept 循环在「刚被 spawn、还没跑到 select」以及「取到一条连接 → spawn
    /// 桥接 → 回到 select」这两段窗口里都不是等待者——此时到来的 stop 信号会被彻底丢掉，
    /// 隧道于是「UI 上已停、端口还在听」。
    ///
    /// 判据：用 **current_thread** 运行时确定性地造出第一个窗口——`start_local_forward`
    /// 返回后 spawn 出的任务还一次都没被轮询过（单线程运行时里只有 await 点才让出），
    /// 此刻立刻 stop。若实现只依赖 notify、不置标志，这个信号必然丢失，
    /// 端口就永远回收不了（下面的 rebind 一直失败）。
    ///
    /// `stop_stops_accepting` 覆盖不到这一形：那里 stop 之前有 `spawn_echo().await` 等
    /// 若干 await 点，循环早已进入 select 成为等待者。
    #[test]
    fn stop_before_first_poll_is_not_lost() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let opener = Arc::new(EchoOpener {
                seen: Arc::new(std::sync::Mutex::new(Vec::new())),
                echo_addr: "127.0.0.1:9".parse().unwrap(), // 不会被用到
            });
            let mut tunnel = None;
            for port in [18151u16, 18152, 18153, 18154] {
                let s = TunnelSpec {
                    local_port: port,
                    ..spec()
                };
                if let Ok(t) = start_local_forward(opener.clone(), s).await {
                    tunnel = Some(t);
                    break;
                }
            }
            let tunnel = tunnel.expect("端口全被占用");
            let addr = tunnel.local_addr;
            // 关键：accept 任务此刻**尚未被轮询过**（单线程运行时 + 之间没有 await 点）
            tunnel.stop();
            drop(tunnel);
            // 让运行时轮询 accept 任务：它必须先读标志、立刻退出并释放 listener
            for _ in 0..50 {
                tokio::task::yield_now().await;
                if TcpListener::bind(addr).await.is_ok() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            panic!("首次轮询前发出的 stop 被丢掉了：端口 {addr} 仍被监听（只靠 notify 而不置标志的典型症状）");
        });
    }

    /// S325：stop 之后不再接受新连接（旧连接不强断——见 stop 文档）。
    #[tokio::test]
    async fn stop_stops_accepting() {
        let echo = spawn_echo().await;
        let opener = Arc::new(EchoOpener {
            seen: Arc::new(std::sync::Mutex::new(Vec::new())),
            echo_addr: echo,
        });
        let mut tunnel = None;
        for port in [18141u16, 18142, 18143, 18144] {
            let s = TunnelSpec {
                local_port: port,
                ..spec()
            };
            if let Ok(t) = start_local_forward(opener.clone(), s).await {
                tunnel = Some(t);
                break;
            }
        }
        let tunnel = tunnel.expect("端口全被占用");
        let addr = tunnel.local_addr;
        tunnel.stop();
        // 给 accept 循环一拍退出（notify_waiters 只唤醒已在等的等待者，
        // 循环恰在 select 上等着）
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        drop(tunnel);
        // 停止后端口应已释放：能重新绑上即证明监听确实退出了（比「连不上」
        // 更强的判据——连不上也可能是 backlog 里排着）
        let rebind = TcpListener::bind(addr).await;
        assert!(rebind.is_ok(), "stop 后监听须退出、端口可重新绑定");
    }

    /// 端口被占时立即返回 Err 并点名端口——不静默后台失败。
    #[tokio::test]
    async fn bind_conflict_reports_immediately() {
        let hog = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = hog.local_addr().unwrap().port();
        let opener = Arc::new(EchoOpener {
            seen: Arc::new(std::sync::Mutex::new(Vec::new())),
            echo_addr: hog.local_addr().unwrap(),
        });
        let err = start_local_forward(
            opener,
            TunnelSpec {
                local_port: port,
                ..spec()
            },
        )
        .await
        .unwrap_err();
        assert!(err.contains(&port.to_string()), "错误须点名端口：{err}");
        assert!(err.contains("占用") || err.contains("失败"), "{err}");
    }
}
