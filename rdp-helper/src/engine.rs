//! RDP 引擎：IronRDP 连接与会话驱动。
//!
//! # 驱动模型
//!
//! IronRDP 是 sans-I/O 状态机，传输由调用方提供；这里的传输是 [`PipeStream`]
//! （stdio 管道，见 `pipe.rs`）。连接三段：
//!
//! 1. `connect_begin`：X.224 连接请求/确认与协议协商，直到需要 TLS 升级；
//! 2. `ironrdp_tls::upgrade`：TLS 握手——**不校验证书**（见下面「TOFU 的时机」）；
//! 3. `connect_finalize`：CredSSP/NLA 认证 + 连接最终化 → `ConnectionResult`。
//!
//! 之后进入会话循环：`read_pdu` → `ActiveStage::process`（解码进 `DecodedImage`）
//! → 脏矩形经 [`DirtyRects`] 合并 → `Frame` 消息上行；输入经
//! `Database::apply` → `process_fastpath_input` 下行。
//!
//! # TOFU 的时机：为什么可以「先握手后裁决」
//!
//! `ironrdp_tls::upgrade` 用不校验的 verifier 完成握手并把对端证书**带回来**。
//! 看起来像安全缺口，实际不是：**口令只在第 3 段（CredSSP）才过线**。第 2 段
//! 结束时拿到的证书先算指纹、上报主程序、等裁决——裁决通过才进入第 3 段。
//! 也就是说：MITM 能与攻击者完成 TLS 握手，但拿不到任何秘密；指纹不匹配则
//! 连接在发送第一个凭据字节**之前**终止。TOFU 的检查点放在「秘密出线前」
//! 而不是「握手前」，安全性等价，实现则免去了在 rustls 的同步 verifier 回调里
//! 阻塞等跨进程裁决的丑陋。
//!
//! # 零网络
//!
//! `connect_finalize` 需要一个 `NetworkClient`——那是 Kerberos KDC-proxy 的
//! HTTP 通道。本仓裁定只做 NTLM（不出站面零开口）， [`NoNetwork`] 对任何请求
//! 返回错误：NTLM 路径不会调它，Kerberos 配置永远是 `None`，谁调谁错。

use crate::fb::{clamp_to_frame, extract_rgba, DirtyRects};
use crate::pipe::PipeStream;
use fs_rdpproto::{
    ConnectParams, FailureKind, FromHelper, InputEvent, MouseButton, Rect, ToHelper,
};
use ironrdp_connector::{ClientConnector, Config, Credentials, DesktopSize};
use ironrdp_core::IntoOwned as _;
use ironrdp_pdu::input::fast_path::FastPathInputEvent;
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput};
use ironrdp_tokio::{
    connect_begin, connect_finalize, mark_as_upgraded, Framed, FramedWrite, NetworkClient,
    TokioStream,
};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use x509_cert::der::Encode as _;

/// 主循环 → 引擎的指令。
pub enum EngineCmd {
    CertVerdict,
    Input(InputEvent),
    Resize {
        width: u16,
        height: u16,
    },
    /// 本机剪贴板有新文本（体 = UTF-8）：通告给远端。
    ClipboardOffer(Vec<u8>),
    /// 本机对远端 `ClipboardRequest` 的应答（体 = UTF-8）。
    ClipboardData(Vec<u8>),
    /// 取远端剪贴板（用户在本机按下粘贴）。
    ClipboardPull,
    /// 前端画完一帧：归还一个在途配额（帧级背压，见 flush_frames）。
    FrameAck,
    /// 前端要一次整屏重绘（画失败/画布重建后自愈用）。
    RequestFullFrame,
    /// 主程序挂载共享目录（RDPDR）。
    ///
    /// readonly 字段在本侧有意不读（只读闸在主程序——它才碰得到文件系统）；
    /// 字段留在协议形状里，主程序回放/测试自己发出的消息时它得在场。
    MountDrive {
        device: u8,
        name: String,
        #[allow(dead_code)]
        readonly: bool,
    },
    /// 卸载共享目录。
    UnmountDrive {
        device: u8,
    },
    /// 主程序对一条 DriveIo 的应答（header 原样 + 体 = Read 数据）。
    DriveIoResult(ToHelper, Vec<u8>),
    Shutdown,
}

/// 引擎 → 主循环的上行消息（header + 可选体；主循环是唯一的 stdout 写者）。
pub struct ToMain {
    pub header: FromHelper,
    pub body: Vec<u8>,
}

/// Kerberos 不做：网络客户端是断头路。
struct NoNetwork;

impl NetworkClient for NoNetwork {
    async fn send(
        &mut self,
        _req: &ironrdp_connector::sspi::generator::NetworkRequest,
    ) -> ironrdp_connector::ConnectorResult<Vec<u8>> {
        Err(ironrdp_connector::ConnectorError::new(
            "kerberos disabled",
            ironrdp_connector::ConnectorErrorKind::General,
        ))
    }
}

type Fail = (FailureKind, String);

/// 引擎入口。跑到会话结束或失败才返回；失败时上报 `Failed` 后安静退出。
pub async fn run(
    stream: PipeStream,
    params: ConnectParams,
    mut cmd_rx: mpsc::UnboundedReceiver<EngineCmd>,
    to_main: mpsc::UnboundedSender<ToMain>,
    cert_verdict: oneshot::Receiver<bool>,
) {
    if let Err((kind, msg)) =
        run_inner(stream, params, &mut cmd_rx, to_main.clone(), cert_verdict).await
    {
        let _ = to_main.send(ToMain {
            header: FromHelper::Failed { kind, message: msg },
            body: Vec::new(),
        });
    }
}

async fn run_inner(
    stream: PipeStream,
    params: ConnectParams,
    cmd_rx: &mut mpsc::UnboundedReceiver<EngineCmd>,
    to_main: mpsc::UnboundedSender<ToMain>,
    cert_verdict: oneshot::Receiver<bool>,
) -> Result<(), Fail> {
    let server_name = params.server_name.clone();

    // 远端剪贴板是否有文本（后端写、引擎读；见 PipeClipboard 的字段注释）
    let remote_has_text = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // ── 第 1 段：X.224 ────────────────────────────────────────────────────
    let mut framed: Framed<TokioStream<PipeStream>> = Framed::new(stream);
    // RDPDR（驱动器重定向批）：backend 只做翻译，文件 IO 与三道闸全在主程序。
    // RDPDR 通道必须与 RDPSND 一起宣告（[MS-RDPEFS] Appendix A<1>，上游注释）。
    let rdpdr_bridge = crate::drive::DriveBridge::new(to_main.clone());
    let rdpdr_backend_bridge = rdpdr_bridge.clone();
    // 线上设备号从 2 起：0/1 是 RDPDR 协议保留。
    let next_rdpdr_device = std::sync::atomic::AtomicU32::new(2);
    let mut connector = ClientConnector::new(build_config(&params), client_addr())
        // 动态虚拟通道容器（阶段 2）。挂在**连接之前**：静态通道集合在
        // MCS 通道加入阶段就要定下来，会话建立之后再加是加不进去的
        //（服务器不知道有这么个通道，DVC 的创建请求永远不会来）。
        .with_static_channel(ironrdp_dvc::DrdynvcClient::new().with_dynamic_channel(
            ironrdp_displaycontrol::client::DisplayControlClient::new(|_caps| Ok(Vec::new())),
        ))
        // 剪贴板（阶段 2，CLIPRDR）。同样必须在连接前挂。
        .with_static_channel(ironrdp_cliprdr::CliprdrClient::new(Box::new(
            crate::clipboard::PipeClipboard::new(to_main.clone(), remote_has_text.clone()),
        )))
        // 音频（阶段 3，RDPSND）：PCM 经管道上行，helper 不碰声卡。
        .with_static_channel(ironrdp_rdpsnd::client::Rdpsnd::new(Box::new(
            crate::audio::PipeAudio::new(to_main.clone()),
        )))
        // 驱动器重定向（RDPDR 批）。盘符**不预置**：主程序的 MountDrive 指令
        // 到达时经 add_drive 补充宣告（post-logon device announce）。
        .with_static_channel(ironrdp_rdpdr::Rdpdr::new(
            Box::new(crate::drive::PipeDriveBackend {
                bridge: rdpdr_backend_bridge,
            }),
            "futureshell".to_string(),
        ));
    let should_upgrade = connect_begin(&mut framed, &mut connector)
        .await
        .map_err(|e| (FailureKind::Protocol, format!("连接协商失败：{e}")))?;

    // ── 第 2 段：TLS + 证书上报 ──────────────────────────────────────────
    let stream = framed.into_inner_no_leftover();
    let (tls_stream, cert) = ironrdp_tls::upgrade(stream, &server_name)
        .await
        .map_err(|e| (FailureKind::Transport, format!("TLS 握手失败：{e}")))?;

    let fingerprint = fingerprint_sha256(
        &cert
            .to_der()
            .map_err(|e| (FailureKind::Internal, format!("证书编码失败：{e}")))?,
    );
    let subject = cert.tbs_certificate.subject.to_string();
    let issuer = cert.tbs_certificate.issuer.to_string();
    let not_after = rfc3339_of(
        cert.tbs_certificate
            .validity
            .not_after
            .to_unix_duration()
            .as_secs() as i64,
    );
    to_main
        .send(ToMain {
            header: FromHelper::CertPresented {
                fingerprint,
                subject,
                issuer,
                not_after,
            },
            body: Vec::new(),
        })
        .map_err(|_| (FailureKind::Internal, "主循环已退出".into()))?;

    let accepted = cert_verdict
        .await
        .map_err(|_| (FailureKind::Internal, "裁决通道关闭（主循环退出？）".into()))?;
    if !accepted {
        return Err((
            FailureKind::CertRejected,
            "服务器证书被拒绝（用户裁决或严格模式不匹配）".into(),
        ));
    }

    // ── 第 3 段：CredSSP（NLA/NTLM）+ 最终化 ─────────────────────────────
    // CredSSP 通道绑定公钥。**字节格式是这次真机翻车的根因**（2026-08-27）：
    // 必须是 SubjectPublicKey 的**原始字节**（RFC 5280 §4.1.2.7 的 BIT STRING
    // 内容），而不是整个 SubjectPublicKeyInfo 的 DER——后者多包了一层
    // AlgorithmIdentifier 和外壳。CredSSP 把这些字节哈希进 TSRequest 的
    // pub_key_auth，服务端拿**它自己的**公钥做同样运算再比对：字节差一层壳，
    // 哈希就全对不上，服务端在 NLA 阶段回 TLS fatal alert（InternalError）
    // 并断连。mstsc 能连、我们必死，就是这个差别。
    //
    // 直接用上游函数（与官方 ironrdp-client 一字不差），不再手搓字段访问——
    // 本地曾经的「手搓版」正是把 SPKI DER 当成了原始字节。
    // itest 为什么没抓住：Debian xrdp 整包无 CredSSP，这条 NLA 路径在容器里
    // 从未被执行过（见仓内 RDP 实施记录「NLA 那条判据带因跳过」）。
    let server_public_key = credssp_channel_binding_key(&cert)?;

    let mut framed: Framed<TokioStream<ironrdp_tls::TlsStream<PipeStream>>> =
        Framed::new(tls_stream);
    // 协商结果播报：mark_as_upgraded 之后状态就前进了，必须在此刻取。
    // UI 与测试都要知道连上的是 NLA（口令在协议层送出）还是 SSL（图形登录，
    // 口令不出本机）——两者的安全含义不同，不该让用户猜。
    // （错口令→Auth 的 itest 也据此分流：无 NLA 的服务器连不上这条路径。）
    if let ironrdp_connector::ClientConnectorState::EnhancedSecurityUpgrade { selected_protocol } =
        &connector.state
    {
        let layer = if selected_protocol.contains(ironrdp_pdu::nego::SecurityProtocol::HYBRID) {
            "NLA（CredSSP）"
        } else if selected_protocol.contains(ironrdp_pdu::nego::SecurityProtocol::SSL) {
            "TLS（图形登录）"
        } else {
            "RDP 标准安全（不加密）"
        };
        let _ = to_main.send(ToMain {
            header: FromHelper::Status {
                message: format!("协商安全层：{layer}"),
            },
            body: Vec::new(),
        });
    }
    let upgraded = mark_as_upgraded(should_upgrade, &mut connector);
    let server_name_typed = ironrdp_connector::ServerName::new(server_name);
    let mut no_network = NoNetwork;
    let result = connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut no_network,
        server_name_typed,
        server_public_key,
        None, // Kerberos 永不启用（零网络出口）
    )
    .await
    .map_err(map_connector_error)?;

    // ── 会话循环 ─────────────────────────────────────────────────────────
    let ironrdp_connector::ConnectionResult {
        static_channels,
        desktop_size,
        user_channel_id,
        io_channel_id,
        message_channel_id,
        share_id,
        compression_type,
        enable_server_pointer,
        pointer_software_rendering,
        // Deactivation-Reactivation（服务器改分辨率后重走激活）要用：
        // 2026-08-27 真机实测，DisplayControl 的 resize 一落地，Windows 就
        // 送 Deactivate-All 要求重激活——当时这里把 factory 直接丢掉了，
        // DeactivateAll 分支只会报错，表现是「画面冻结 + 鼠标无效 + 一分钟后
        // 服务器踢人」。重连能好纯属侥幸（桌面已是目标分辨率，服务器不再
        // 要求重激活）。
        activation_factory,
    } = result;
    let (w, h) = (desktop_size.width, desktop_size.height);

    let mut image = DecodedImage::new(
        ironrdp_graphics::image_processing::PixelFormat::RgbA32,
        w,
        h,
    );
    let mut active = ActiveStageBuilder {
        static_channels,
        user_channel_id,
        io_channel_id,
        message_channel_id,
        share_id,
        compression_type,
        enable_server_pointer,
        pointer_software_rendering,
    }
    .build();
    let mut input_db = ironrdp_input::Database::new();
    // 「不支持动态分辨率」只播报一次：拖窗口会连发几十条 Resize，
    // 每条都播一句会把状态栏刷成一堵墙。
    let mut resize_unavailable_reported = false;
    let mut dirty = DirtyRects::new((w, h));
    // 在途帧计数（帧级背压）：见 flush_frames 与 MAX_FRAMES_IN_FLIGHT。
    let mut in_flight: u32 = 0;

    // transport_err：写失败统一翻译（RDPDR 三处发送共用）。
    let transport_err = |e: std::io::Error| (FailureKind::Transport, format!("写会话帧失败：{e}"));

    // 初始同步事件：RDP 要求会话激活后先同步一次键盘锁定态，否则远端的
    // NumLock/CapsLock 与本地脱节（Shift 出数字之类的怪象）。
    send_sync(&mut active, &mut image, false, false, false);

    to_main
        .send(ToMain {
            header: FromHelper::Connected {
                width: w,
                height: h,
            },
            body: Vec::new(),
        })
        .map_err(|_| (FailureKind::Internal, "主循环已退出".into()))?;

    loop {
        let cmd = tokio::select! {
            pdu = framed.read_pdu() => {
                let (action, payload) =
                    pdu.map_err(|e| (FailureKind::Transport, format!("读会话帧失败：{e}")))?;
                let outputs = active
                    .process(&mut image, action, &payload)
                    .map_err(|e| (FailureKind::Protocol, format!("会话处理失败：{e}")))?;
                match apply_outputs(outputs, &mut framed, &mut image, &mut dirty, &to_main, &mut in_flight).await? {
                    StageFlow::Continue => {}
                    StageFlow::Terminate => return Ok(()), // Closed 已上报
                    StageFlow::Reactivate => {
                        reactivate(&mut framed, &mut active, &mut image, &mut dirty,
                                   &activation_factory, compression_type, &to_main,
                                   &mut in_flight).await?;
                    }
                }
                continue;
            }
            cmd = cmd_rx.recv() => match cmd {
                None => return Ok(()), // 主循环退出：安静结束
                Some(c) => c,
            },
        };
        match cmd {
            EngineCmd::CertVerdict => {
                // 裁决早已消费；迟到的重复裁决是主程序的错，忽略但留痕。
                eprintln!("fs-rdp-helper: 迟到的 CertVerdict 被忽略");
            }
            EngineCmd::Input(ev) => {
                // ReleaseAll 不是一次「操作」而是**状态复位**，上游给的是
                // 专门的 release_all()——它只为真正按着的键/按钮生成事件，
                // 没按着的不发（发了等于凭空多出一串抬起，远端会当真）。
                let events = if matches!(ev, InputEvent::ReleaseAll) {
                    input_db.release_all()
                } else {
                    input_db.apply(to_operations(&ev))
                };
                if !events.is_empty() {
                    let outputs = active
                        .process_fastpath_input(&mut image, &events)
                        .map_err(|e| (FailureKind::Protocol, format!("输入编码失败：{e}")))?;
                    match apply_outputs(
                        outputs,
                        &mut framed,
                        &mut image,
                        &mut dirty,
                        &to_main,
                        &mut in_flight,
                    )
                    .await?
                    {
                        StageFlow::Continue => {}
                        StageFlow::Terminate => return Ok(()),
                        // 输入触发重激活理论不该发生（那是服务器侧的决议），
                        // 但流程上不能是不可能的分支——照样处理即可。
                        StageFlow::Reactivate => {
                            reactivate(
                                &mut framed,
                                &mut active,
                                &mut image,
                                &mut dirty,
                                &activation_factory,
                                compression_type,
                                &to_main,
                                &mut in_flight,
                            )
                            .await?;
                        }
                    }
                }
            }
            EngineCmd::Resize { width, height } => {
                // MS-RDPEDISP 2.2.2.2.1 的硬约束：宽高须在 200..=8192，且**宽为偶数**。
                // 不先钳就直接编码，服务器会拒整条 PDU（表现是「拖窗口没反应」，
                // 而日志里什么都没有——它拒的是我们发的，不是它自己出错）。
                let width = width.clamp(200, 8192) & !1;
                let height = height.clamp(200, 8192);
                // DVC 未就绪（服务器还没送 DisplayControl 能力，或压根不支持）时
                // encode_resize 返回 None。**如实播报一次**而不是静默丢：
                // 用户拖了窗口却没反应时，这句是唯一的线索。
                match active.encode_resize(u32::from(width), u32::from(height), None, None) {
                    Some(Ok(bytes)) => {
                        framed.write_all(&bytes).await.map_err(|e| {
                            (FailureKind::Transport, format!("发送 resize 失败：{e}"))
                        })?;
                    }
                    Some(Err(e)) => eprintln!("fs-rdp-helper: 调整分辨率失败：{e}"),
                    None => {
                        if !resize_unavailable_reported {
                            resize_unavailable_reported = true;
                            let _ = to_main.send(ToMain {
                                header: FromHelper::Status {
                                    message:
                                        "该服务器不支持动态分辨率（画面按连接时的尺寸缩放显示）"
                                            .into(),
                                },
                                body: Vec::new(),
                            });
                        }
                    }
                }
            }
            EngineCmd::ClipboardOffer(text) => {
                // 本机复制了东西 → 通告远端「我有 CF_UNICODETEXT」。
                // **只通告不发内容**：远端要粘时才发 FormatDataRequest 来取，
                // 那时引擎把请求转给主程序（FromHelper::ClipboardRequest），
                // 由**主程序**读当下的系统剪贴板应答。
                //
                // 不在引擎里存一份副本：存了就要回答「用户在通告之后又复制了
                // 别的东西时，该发哪一份」——而那个问题只有主程序答得了
                //（它才看得见系统剪贴板）。多一份可能过期的状态不是优化。
                let _ = &text; // 内容不在这一跳使用（见上）
                let formats = [ironrdp_cliprdr::pdu::ClipboardFormat::new(
                    ironrdp_cliprdr::pdu::ClipboardFormatId::CF_UNICODETEXT,
                )];
                if let Some(cliprdr) =
                    active.get_svc_processor_mut::<ironrdp_cliprdr::CliprdrClient>()
                {
                    match cliprdr.initiate_copy(&formats) {
                        Ok(msgs) => send_svc(&mut active, &mut framed, msgs).await?,
                        Err(e) => eprintln!("fs-rdp-helper: 通告剪贴板失败：{e}"),
                    }
                }
            }
            EngineCmd::ClipboardData(text) => {
                // 主程序对远端取数的应答。
                let payload =
                    crate::clipboard::utf8_to_cf_unicodetext(&String::from_utf8_lossy(&text));
                if let Some(cliprdr) = active.get_svc_processor::<ironrdp_cliprdr::CliprdrClient>()
                {
                    match cliprdr.submit_format_data(
                        ironrdp_cliprdr::pdu::FormatDataResponse::new_data(payload).into_owned(),
                    ) {
                        Ok(msgs) => send_svc(&mut active, &mut framed, msgs).await?,
                        Err(e) => eprintln!("fs-rdp-helper: 应答剪贴板取数失败：{e}"),
                    }
                }
            }
            EngineCmd::ClipboardPull => {
                // 用户在本机按下粘贴 → 向远端取数。
                // 远端没通告过文本时**不发**：发了只会得到一个 is_error 应答，
                // 而那条错误在日志里看起来像「剪贴板坏了」。
                if !remote_has_text.load(std::sync::atomic::Ordering::SeqCst) {
                    continue;
                }
                if let Some(cliprdr) =
                    active.get_svc_processor_mut::<ironrdp_cliprdr::CliprdrClient>()
                {
                    match cliprdr
                        .initiate_paste(ironrdp_cliprdr::pdu::ClipboardFormatId::CF_UNICODETEXT)
                    {
                        Ok(msgs) => send_svc(&mut active, &mut framed, msgs).await?,
                        Err(e) => eprintln!("fs-rdp-helper: 取远端剪贴板失败：{e}"),
                    }
                }
            }
            EngineCmd::RequestFullFrame => {
                // 整屏标脏 → 下一轮 flush 自然把整屏发出去。不直接发：
                // 发不发、发几块、配额够不够，全归 flush_frames 一家管
                //（两处各判一次配额迟早分叉）。
                dirty.add(fs_rdpproto::Rect {
                    x: 0,
                    y: 0,
                    width: image.width(),
                    height: image.height(),
                });
                flush_frames(&image, &mut dirty, &to_main, &mut in_flight);
            }
            EngineCmd::FrameAck => {
                // 归还配额并立刻把积压（已合并成一块）发出去——不等下一个
                // PDU：远端静止时不会再有 PDU，等于让最后一次更新永远停在
                // helper 里（用户看到画面「差最后一帧」）。
                in_flight = in_flight.saturating_sub(1);
                flush_frames(&image, &mut dirty, &to_main, &mut in_flight);
            }
            EngineCmd::MountDrive {
                device,
                name,
                readonly: _,
            } => {
                // `readonly` 标志在 helper 侧不用（只读闸在主程序——它才碰得到
                // 文件系统）；保留在消息里是因为协议形状由两侧共有，主程序读
                // 自己发出的消息时（审计回放/测试）它得在场。
                // 职责：把盘公告出去，设备号对上两个世界的编号。
                if let Some(rdpdr) = active.get_svc_processor_mut::<ironrdp_rdpdr::Rdpdr>() {
                    let wire_id =
                        next_rdpdr_device.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    rdpdr_bridge.register_device(wire_id, device);
                    let announce = rdpdr.add_drive(wire_id, name);
                    let msgs: ironrdp_svc::SvcProcessorMessages<ironrdp_rdpdr::Rdpdr> =
                        ironrdp_svc::SvcProcessorMessages::new(vec![
                            ironrdp_svc::SvcMessage::from(
                                ironrdp_rdpdr::pdu::RdpdrPdu::ClientDeviceListAnnounce(announce),
                            ),
                        ]);
                    if let Ok(bytes) = active.process_svc_processor_messages(msgs) {
                        framed.write_all(&bytes).await.map_err(transport_err)?;
                    }
                }
            }
            EngineCmd::UnmountDrive { device } => {
                if let Some(rdpdr) = active.get_svc_processor_mut::<ironrdp_rdpdr::Rdpdr>() {
                    if let Some(wire_id) = rdpdr_bridge.take_wire_id(device) {
                        if let Some(remove) = rdpdr.remove_device(wire_id) {
                            let msgs: ironrdp_svc::SvcProcessorMessages<ironrdp_rdpdr::Rdpdr> =
                                ironrdp_svc::SvcProcessorMessages::new(vec![
                                    ironrdp_svc::SvcMessage::from(
                                        ironrdp_rdpdr::pdu::RdpdrPdu::ClientDeviceListRemove(
                                            remove,
                                        ),
                                    ),
                                ]);
                            if let Ok(bytes) = active.process_svc_processor_messages(msgs) {
                                framed.write_all(&bytes).await.map_err(transport_err)?;
                            }
                        }
                    }
                }
            }
            EngineCmd::DriveIoResult(header, body) => {
                // bridge 组装 MS-RDPEFS 响应（在途表里存着完成信息）。
                for msg in rdpdr_bridge.complete(header, body) {
                    // 单条一条地发：SvcProcessorMessages 一次一条的形态与
                    // 上游 backend 返回值一致。
                    let single: ironrdp_svc::SvcProcessorMessages<ironrdp_rdpdr::Rdpdr> =
                        ironrdp_svc::SvcProcessorMessages::new(vec![msg]);
                    if let Ok(bytes) = active.process_svc_processor_messages(single) {
                        framed.write_all(&bytes).await.map_err(transport_err)?;
                    }
                }
            }
            EngineCmd::Shutdown => {
                let outputs = active
                    .graceful_shutdown()
                    .map_err(|e| (FailureKind::Internal, format!("构造关机请求失败：{e}")))?;
                for out in outputs {
                    if let ActiveStageOutput::ResponseFrame(bytes) = out {
                        framed.write_all(&bytes).await.map_err(|e| {
                            (FailureKind::Transport, format!("发送关机请求失败：{e}"))
                        })?;
                    }
                }
                return Ok(());
            }
        }
    }
}

/// 把一批 SVC 消息（剪贴板等静态通道产物）编码后写到线上。
///
/// 单列成函数是因为三个剪贴板分支都要它，而这段有个易错处：
/// `process_svc_processor_messages` 才是把 `SvcProcessorMessages` 变成
/// **带通道头**的线上字节的那一步——直接 `write_all(msgs)` 编不出通道 id，
/// 远端会把它当垃圾丢掉（表现是「剪贴板没反应」且两侧日志都干净）。
async fn send_svc<S, C>(
    active: &mut ActiveStage,
    framed: &mut Framed<TokioStream<S>>,
    msgs: ironrdp_svc::SvcProcessorMessages<C>,
) -> Result<(), Fail>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + Sync,
    C: ironrdp_svc::SvcProcessor + 'static,
{
    let bytes = active
        .process_svc_processor_messages(msgs)
        .map_err(|e| (FailureKind::Protocol, format!("编码通道消息失败：{e}")))?;
    framed
        .write_all(&bytes)
        .await
        .map_err(|e| (FailureKind::Transport, format!("发送通道消息失败：{e}")))
}

/// 一批 ActiveStage 输出处理完后的流向。
#[derive(PartialEq)]
enum StageFlow {
    /// 正常继续会话循环。
    Continue,
    /// 会话结束（Terminate 已上报）。
    Terminate,
    /// 服务器送了 Deactivate-All，要求重走激活序列（见 [`reactivate`]）。
    Reactivate,
}

/// 处理一批 ActiveStage 输出。
#[allow(clippy::too_many_arguments)]
async fn apply_outputs<S>(
    outputs: Vec<ActiveStageOutput>,
    framed: &mut Framed<TokioStream<S>>,
    image: &mut DecodedImage,
    dirty: &mut DirtyRects,
    to_main: &mpsc::UnboundedSender<ToMain>,
    in_flight: &mut u32,
) -> Result<StageFlow, Fail>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + Sync,
{
    for out in outputs {
        match out {
            ActiveStageOutput::ResponseFrame(bytes) => {
                framed
                    .write_all(&bytes)
                    .await
                    .map_err(|e| (FailureKind::Transport, format!("回写会话帧失败：{e}")))?;
            }
            ActiveStageOutput::GraphicsUpdate(rect) => {
                // InclusiveRectangle（含端点）→ 宽高
                dirty.add(Rect {
                    x: rect.left,
                    y: rect.top,
                    width: rect.right - rect.left + 1,
                    height: rect.bottom - rect.top + 1,
                });
            }
            ActiveStageOutput::Terminate(reason) => {
                let _ = to_main.send(ToMain {
                    header: FromHelper::Closed,
                    body: Vec::new(),
                });
                let _ = to_main.send(ToMain {
                    header: FromHelper::Status {
                        message: format!("远端断开：{reason}"),
                    },
                    body: Vec::new(),
                });
                return Ok(StageFlow::Terminate);
            }
            ActiveStageOutput::DeactivateAll => {
                // Deactivation-Reactivation Sequence（MS-RDPBCGR）：服务器改了
                // 分辨率等参数后送 Deactivate-All，客户端必须重走一遍
                // Demand→Confirm→Sync→Cooperate→FontList 激活序列，会话才能
                // 恢复收帧与收输入。
                //
                // 这不是边角：动态分辨率（DisplayControl）每一次生效都会走
                // 到这里。2026-08-27 真机实测，旧代码在这里直接报错——表现是
                // 画面冻结、鼠标无效、约一分钟后服务器断开；重连「好了」只是
                // 因为桌面已是目标分辨率、服务器不再要求重激活。
                return Ok(StageFlow::Reactivate);
            }
            // 指针走服务端渲染（Config.enable_server_pointer = true），指针类输出不会出现。
            ActiveStageOutput::PointerDefault
            | ActiveStageOutput::PointerHidden
            | ActiveStageOutput::PointerPosition { .. }
            | ActiveStageOutput::PointerBitmap(_) => {}
            // 多传输与自动检测探测：不启用 UDP 多传输，忽略。
            ActiveStageOutput::MultitransportRequest(_) | ActiveStageOutput::AutoDetect(_) => {}
        }
    }
    flush_frames(image, dirty, to_main, in_flight);
    Ok(StageFlow::Continue)
}

/// Deactivation-Reactivation Sequence（MS-RDPBCGR）。
///
/// # 什么时候会走到这里
///
/// 服务器改变会话参数（**最常见的就是分辨率**——我们经 DisplayControl 请求
/// 改分辨率，服务器答应了）时，它会送一个 Deactivate-All PDU：意思是
/// 「当前这套激活参数作废了，重来一遍」。此后服务器**既不送画面也不收输入**，
/// 直到客户端重走完 Demand Active → Confirm Active → Synchronize →
/// Control(Cooperate) → Control(Request) → Font List → Font Map 这一串。
///
/// # 不做会怎样（2026-08-27 真机实测）
///
/// 旧代码在这里直接报错。用户看到的是：连上后画面正常，一改窗口大小
/// （前端 250ms 防抖后发 resize）画面**冻结**、鼠标键盘全部**无效**，
/// 约一分钟后服务器把连接踢掉。而重连往往「就好了」——因为远端桌面
/// 已经是目标分辨率，服务器不再需要重激活，于是这条路径不再被触发。
/// 这种「第一次坏、重连就好」的形态最容易被误判成网络抖动。
///
/// # 实现要点
///
/// · 通道 ID（MCS 层）**跨重激活不变**——上游文档明说，故 factory 捕获一次
///   即可反复造序列；
/// · `share_id` **会变**（新的 Demand Active 带新的），必须同步给 x224 与
///   fastpath 两个处理器，否则之后发出去的 PDU 会被服务器当成陈旧会话丢弃；
/// · 画面尺寸变了要重建 `DecodedImage` 与脏矩形累加器，并把新尺寸上报主程序
///   （前端据此重设画布位图尺寸——不重设的话新画面会被按旧尺寸错切）；
/// · fastpath 处理器只能整体重建（字段私有），重建时**必须把解压器一起带过去**
///   ——compression_type 是连接期定下的，丢了它之后所有压缩帧都解不开。
#[allow(clippy::too_many_arguments)]
async fn reactivate<S>(
    framed: &mut Framed<TokioStream<S>>,
    active: &mut ActiveStage,
    image: &mut DecodedImage,
    dirty: &mut DirtyRects,
    factory: &ironrdp_connector::connection_activation::ConnectionActivationFactory,
    compression_type: Option<ironrdp_pdu::rdp::client_info::CompressionType>,
    to_main: &mpsc::UnboundedSender<ToMain>,
    in_flight: &mut u32,
) -> Result<(), Fail>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + Sync,
{
    use ironrdp_connector::connection_activation::ConnectionActivationState;

    let _ = to_main.send(ToMain {
        header: FromHelper::Status {
            message: "远端重设了会话参数（多半是分辨率），正在重新激活…".into(),
        },
        body: Vec::new(),
    });

    let mut seq = factory.create();
    let mut buf = ironrdp_core::WriteBuf::new();
    let (desktop_size, share_id, enable_server_pointer, pointer_software_rendering) = loop {
        ironrdp_tokio::single_sequence_step(framed, &mut seq, &mut buf)
            .await
            .map_err(|e| {
                (
                    FailureKind::Protocol,
                    format!("重新激活失败：{}", error_chain(&e)),
                )
            })?;
        if let ConnectionActivationState::Finalized {
            desktop_size,
            share_id,
            enable_server_pointer,
            pointer_software_rendering,
        } = seq.connection_activation_state()
        {
            break (
                desktop_size,
                share_id,
                enable_server_pointer,
                pointer_software_rendering,
            );
        }
    };

    // 在途配额清零：重激活期间前端那些帧的回执要么已经在路上、要么随旧画面
    // 一起作废，不清零就等于永久漏掉几个配额（配额只减不还 → 画面冻死）。
    *in_flight = 0;

    let (w, h) = (desktop_size.width, desktop_size.height);
    *image = DecodedImage::new(
        ironrdp_graphics::image_processing::PixelFormat::RgbA32,
        w,
        h,
    );
    *dirty = DirtyRects::new((w, h));

    // share_id 两处都要更新：x224 处理器用它编 ShareDataPdu（上游 set_share_id
    // 的文档原话就是「必须在重激活时调用」），fastpath 处理器把它烧在
    // FrameMarker 里——只改一处，另一处发出去的帧会带着上一代 share_id。
    active.set_share_id(share_id);
    active.set_enable_server_pointer(enable_server_pointer);
    active.set_fastpath_processor(
        ironrdp_session::fast_path::ProcessorBuilder {
            io_channel_id: seq.io_channel_id(),
            user_channel_id: seq.user_channel_id(),
            share_id,
            enable_server_pointer,
            pointer_software_rendering,
            bulk_decompressor: compression_type.and_then(bulk_decompressor_for),
        }
        .build(),
    );

    // 新尺寸上报：前端据此重设画布位图尺寸（复用 Connected——它的语义就是
    // 「此刻的桌面尺寸是这个」，重激活后正是这个语义再次成立的时刻）。
    to_main
        .send(ToMain {
            header: FromHelper::Connected {
                width: w,
                height: h,
            },
            body: Vec::new(),
        })
        .map_err(|_| (FailureKind::Internal, "主循环已退出".into()))?;
    Ok(())
}

/// 压缩类型 → 解压器。与 `ActiveStageBuilder::build` 内部的映射保持一致
/// （那个函数是私有的，这里只能照抄一份；映射错了表现为压缩帧解不开 = 花屏）。
fn bulk_decompressor_for(
    ct: ironrdp_pdu::rdp::client_info::CompressionType,
) -> Option<ironrdp_bulk::BulkCompressor> {
    use ironrdp_pdu::rdp::client_info::CompressionType as Pdu;
    let bulk = match ct {
        Pdu::K8 => ironrdp_bulk::CompressionType::Rdp4,
        Pdu::K64 => ironrdp_bulk::CompressionType::Rdp5,
        Pdu::Rdp6 => ironrdp_bulk::CompressionType::Rdp6,
        Pdu::Rdp61 => ironrdp_bulk::CompressionType::Rdp61,
    };
    ironrdp_bulk::BulkCompressor::new(bulk).ok()
}

/// 在途帧上限（帧级背压，见 [`fs_rdpproto::ToHelper::FrameAck`]）。
///
/// 2 而不是 1：留一帧的流水线余量，让「前端画上一帧」与「helper 编下一帧」
/// 重叠，稳态吞吐不因往返延迟打折；也不给更多——上限越大，积压越深、
/// 输入到画面的延迟越长（用户感受是「鼠标拖影」）。
const MAX_FRAMES_IN_FLIGHT: u32 = 2;

/// 把已合并的脏矩形按**在途配额**上行。
///
/// 配额用尽时**不发也不丢**：矩形留在 [`DirtyRects`] 里继续与后续更新合并，
/// 等 `FrameAck` 到达再发。这就是「像素可合并」这条性质的兑现处——
/// 积压期间远端画了十帧，用户最终看到的只是最后那一帧的内容，
/// 而这正是他该看到的东西。
fn flush_frames(
    image: &DecodedImage,
    dirty: &mut DirtyRects,
    to_main: &mpsc::UnboundedSender<ToMain>,
    in_flight: &mut u32,
) {
    if !dirty.has_pending() {
        return;
    }
    // **配额要在循环内判**：一次 flush 可能有几十上百个矩形（DirtyRects 最多
    // 攒 256 块），只在循环外判一次等于把在途一次拉到 256——背压名存实亡。
    // 发不出去的留回 dirty 继续合并，绝不丢。
    let mut deferred: Vec<Rect> = Vec::new();
    for rect in dirty.take() {
        if *in_flight >= MAX_FRAMES_IN_FLIGHT {
            deferred.push(rect);
            continue;
        }
        // 钳到帧内再取像素：IronRDP 的矩形理论上在界内，钳一次是免费的保险
        // （越界矩形在 extract 里会变成补零行，屏幕上是一块黑——钳掉它）。
        let clamped = clamp_to_frame(image.data().len(), image.stride(), &rect);
        if clamped.is_empty() {
            continue;
        }
        let body = extract_rgba(image.data(), image.stride(), &clamped);
        let _ = to_main.send(ToMain {
            header: FromHelper::Frame {
                rect: clamped,
                format: fs_rdpproto::PixelFormat::Rgba,
            },
            body,
        });
        *in_flight += 1;
    }
    for rect in deferred {
        dirty.add(rect);
    }
}

/// Client Info PDU 里的「客户端地址」。虚拟网卡时代这个字段没有诚实值，
/// 沿用各类客户端的惯例：127.0.0.1。
fn client_addr() -> std::net::SocketAddr {
    "127.0.0.1:0".parse().expect("静态字面量必合法")
}

/// 按本仓的裁定拼连接配置。取值来源：IronRDP screenshot 示例（各项含义有注释）。
fn build_config(p: &ConnectParams) -> Config {
    use ironrdp_pdu::rdp::capability_sets::MajorPlatformType;
    Config {
        desktop_size: DesktopSize {
            width: p.width,
            height: p.height,
        },
        desktop_scale_factor: 100,
        // 同时通告 NLA（HYBRID）与 SSL：服务器挑它俩中它支持的最强者。
        // · 真 Windows/多数现代服务器 → HYBRID（CredSSP+NTLM）：口令在协议层
        //   送出（这正是 ConnectParams.password 的用途）；
        // · 老/精简构建（如 Debian 的 xrdp：整包无 CredSSP，实测协商回落 SSL）
        //   → SSL 图形登录：**我们的口令一个字节都不出本机**——用户在远端登录
        //   屏上亲手输入。TOFU 闸两条路都拦在秘密出线之前（证书先裁决）。
        // 这就是 mstsc 的行为模型；只通告 HYBRID 固然「更严」，代价是整类服务器
        // 连不上——那不是安全，是失能。
        enable_tls: true,
        enable_credssp: true,
        credentials: Credentials::UsernamePassword {
            username: p.username.clone(),
            password: p.password.clone(),
        },
        domain: if p.domain.is_empty() {
            None
        } else {
            Some(p.domain.clone())
        },
        client_build: 2600,
        client_name: "FS-CLIENT".into(),
        keyboard_type: ironrdp_pdu::gcc::KeyboardType::IbmEnhanced,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        keyboard_layout: p.keyboard_layout,
        ime_file_name: String::new(),
        bitmap: Some(ironrdp_connector::BitmapConfig {
            lossy_compression: false,
            color_depth: 32,
            // **必须通告编解码器**（2026-08-27 真机：动态画面卡成幻灯片）。
            //
            // 这里曾经是 `BitmapCodecs(Vec::new())` —— 那不是「用默认」，是
            // 明确告诉服务器「我一个编解码器都不支持」：`connection_activation.rs`
            // 对 `config.bitmap = Some(..)` 时**原样采用**我们给的列表，只有
            // `None` 才回落到上游默认。于是服务器只能退回**未压缩的位图更新**：
            // 一块 800×600 的脏区就是 1.9 MB 像素，视频/拖窗口这类连续全屏变化
            // 直接把 TCP 和我们的事件桥一起压垮。
            //
            // `client_codecs_capabilities(&[])` 是上游默认集合（RemoteFX 默认开）。
            // RemoteFX 的解码在 ironrdp-session 的 fast_path 里现成
            //（`rfx_handler: rfx::DecodingContext`），不需要我们做任何事。
            codecs: ironrdp_pdu::rdp::capability_sets::client_codecs_capabilities(&[])
                .expect("空配置不会失败（上游文档保证）"),
        }),
        dig_product_id: String::new(),
        client_dir: String::new(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        platform: {
            #[cfg(windows)]
            {
                MajorPlatformType::WINDOWS
            }
            #[cfg(target_os = "macos")]
            {
                MajorPlatformType::MACINTOSH
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                MajorPlatformType::UNIX
            }
        },
        hardware_id: None,
        request_data: None,
        autologon: false,
        // 必须为 true：false 会在 Client Info PDU 里置 INFO_NOAUDIOPLAYBACK，按 MS-RDPBCGR
        // 2.2.1.11.1.1 服务器「MUST NOT」做音频重定向——RDPSND 通道挂着也收不到一个样本。
        // 这里曾写着 `false, // 阶段 3（RDPSND）再开`：阶段 3 交付了通道与播放，这个开关
        // 却没人翻，音频在真 Windows 上是哑的（xrdp itest 不覆盖音频，没人撞见）。
        // 1.0.0 候选复查时发现（2026-09-29）；守卫见本文件 tests 的 audio_is_requested。
        enable_audio_playback: true,
        performance_flags: Default::default(),
        license_cache: None,
        timezone_info: Default::default(),
        compression_type: None, // 不压上行（下行解压由 ironrdp-graphics 处理）
        enable_server_pointer: true, // 阶段 1：服务端画指针，省掉整套指针位图上行
        pointer_software_rendering: false,
        multitransport_flags: None,
    }
}

/// InputEvent（fs_rdpproto）→ Operation（ironrdp-input）。
fn to_operations(ev: &InputEvent) -> Vec<ironrdp_input::Operation> {
    use ironrdp_input as inp;
    match *ev {
        InputEvent::MouseMove { x, y } => {
            vec![inp::Operation::MouseMove(inp::MousePosition { x, y })]
        }
        InputEvent::MouseButton { button, down, x, y } => {
            // 先移到位再按下/抬起：RDP 的按钮事件也带坐标，但「移动中点击」的
            // 序列里若不先行同步位置，按下会落到上一次的坐标上。
            let mut ops = vec![inp::Operation::MouseMove(inp::MousePosition { x, y })];
            let b = match button {
                MouseButton::Left => inp::MouseButton::Left,
                MouseButton::Right => inp::MouseButton::Right,
                MouseButton::Middle => inp::MouseButton::Middle,
            };
            ops.push(if down {
                inp::Operation::MouseButtonPressed(b)
            } else {
                inp::Operation::MouseButtonReleased(b)
            });
            ops
        }
        InputEvent::MouseScroll {
            vertical,
            delta,
            x,
            y,
        } => {
            let mut ops = vec![inp::Operation::MouseMove(inp::MousePosition { x, y })];
            ops.push(inp::Operation::WheelRotations(inp::WheelRotations {
                is_vertical: vertical,
                rotation_units: delta,
            }));
            ops
        }
        InputEvent::Key {
            scancode,
            extended,
            down,
        } => {
            let sc = inp::Scancode::from_u8(extended, scancode as u8);
            vec![if down {
                inp::Operation::KeyPressed(sc)
            } else {
                inp::Operation::KeyReleased(sc)
            }]
        }
        // ReleaseAll 走 Database::release_all()，不经这里（见调用点注释）。
        // 留一个空向量而不是 unreachable!()：协议消息来自另一个进程，
        // 拿 panic 兜进程间的意外是最差的形状。
        InputEvent::ReleaseAll => Vec::new(),
        InputEvent::SyncLockKeys { caps, num, scroll } => {
            // 锁定键同步不是普通按键（是状态不是动作）——由 send_sync 直接产事件。
            // 这里返回空，调用处的调用者（主循环）应当改调 send_sync；
            // 保守起见对空操作不产事件，而不是把它当一次按键。
            let _ = (caps, num, scroll);
            Vec::new()
        }
    }
}

/// 发一次键盘同步事件（会话激活后的第一件事，以及 SyncLockKeys 到达时）。
fn send_sync(
    active: &mut ActiveStage,
    image: &mut DecodedImage,
    caps: bool,
    num: bool,
    scroll: bool,
) {
    use ironrdp_pdu::input::fast_path::SynchronizeFlags;
    let mut flags = SynchronizeFlags::empty();
    // 位序按 [MS-RDPBCGR] 2.2.8.1.1.2.1.1（Bit 0 = ScrollLock, 1 = NumLock, 2 = CapsLock）
    if scroll {
        flags |= SynchronizeFlags::SCROLL_LOCK;
    }
    if num {
        flags |= SynchronizeFlags::NUM_LOCK;
    }
    if caps {
        flags |= SynchronizeFlags::CAPS_LOCK;
    }
    let events = [FastPathInputEvent::SyncEvent(flags)];
    let _ = active.process_fastpath_input(image, &events);
}

/// SHA-256 指纹（小写十六进制 + 冒号，与 SSH 主机密钥同款排版）。
fn fingerprint_sha256(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// UNIX 秒 → RFC3339（UTC，秒精度——证书的 not_after 本来就是这个精度）。
fn rfc3339_of(unix: i64) -> String {
    // civil-from-days（Howard Hinnant 算法）：无时区库依赖的日期换算。
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// 把一条错误链整条摊平成一行。
///
/// **为什么必须有这个函数**：IronRDP 的 `ConnectorError` 用 `Display` 只打印
/// 最外层的上下文，形如
///
/// ```text
/// [read frame by hint @ connector.rs:175] custom error
/// ```
///
/// 「custom error」四个字对用户和排查者都是**零信息**——真正的原因
/// （连接被对端关闭、CredSSP 拒绝、PDU 解析失败……）藏在 `source()` 链里。
/// 一次真实的排查（2026-08-27）就卡在这里：用户拿到的报错除了行号什么都没有。
///
/// 用 `←` 连接而不是换行：这条串要经协议上报、再进 toast，得是一行。
fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    // 上限防御：错误链理论上可以成环（自定义 Error 实现里见过），
    // 排查工具本身不该把进程挂死。
    for _ in 0..16 {
        let Some(s) = cur else { break };
        out.push_str(" ← ");
        out.push_str(&s.to_string());
        cur = s.source();
    }
    out
}

/// CredSSP 通道绑定用的服务器公钥字节。见 [`run_inner`] 里调用处的注释——
/// 格式必须是 SubjectPublicKey 的**原始字节**，与官方客户端
/// （`ironrdp_tls::extract_tls_server_public_key`）一字不差。
///
/// 独立成函数不是为了复用（只此一处），是为了给下面的判据一个可钉的靶子。
fn credssp_channel_binding_key(
    cert: &x509_cert::Certificate,
) -> Result<Vec<u8>, (FailureKind, String)> {
    ironrdp_tls::extract_tls_server_public_key(cert)
        .map(|b| b.to_vec())
        .ok_or_else(|| {
            (
                FailureKind::Internal,
                "证书公钥无法提取原始字节（不支持的密钥类型）".to_string(),
            )
        })
}

/// 连接器错误 → 失败分类。认证失败（口令/用户名错）要单列：
/// 主程序据此走「重新问口令」而不是「连接失败」。
fn map_connector_error(e: ironrdp_connector::ConnectorError) -> Fail {
    use ironrdp_connector::ConnectorErrorKind;
    let detail = error_chain(&e);
    match e.kind() {
        ConnectorErrorKind::Credssp(_) => {
            // CredSSP 层的错误大多是 NTLM 拒绝（口令/用户名/域不对）
            (FailureKind::Auth, format!("认证失败：{detail}"))
        }
        _ => (FailureKind::Protocol, format!("连接失败：{detail}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 挂了 RDPSND 通道就必须向服务器**要**音频：`enable_audio_playback = false`
    /// 会置 INFO_NOAUDIOPLAYBACK，服务器据此完全不做音频重定向，播放链路全程空转。
    #[test]
    fn audio_is_requested_because_rdpsnd_is_attached() {
        let cfg = build_config(&ConnectParams {
            server_name: "h".into(),
            username: "u".into(),
            domain: String::new(),
            password: "p".into(),
            width: 800,
            height: 600,
            keyboard_layout: 0,
        });
        assert!(
            cfg.enable_audio_playback,
            "RDPSND 已挂载，却告诉服务器不要音频"
        );
    }

    /// 帧级背压的配额记账（2026-08-27 用户实测：动态画面把界面卡死到
    /// 连关闭连接都点不动，根因是帧流没有任何背压）。
    ///
    /// 三条不变量一起钉：配额内照发、配额耗尽**不发也不丢**（留在 DirtyRects
    /// 里继续合并）、回执归还配额后积压能发出去。
    #[test]
    fn frame_flush_respects_the_in_flight_budget_and_never_drops_pixels() {
        use ironrdp_graphics::image_processing::PixelFormat;
        let image = DecodedImage::new(PixelFormat::RgbA32, 64, 64);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut dirty = DirtyRects::new((64, 64));
        let mut in_flight = 0u32;

        // 三块互不相邻的脏区（避免被合并成一块，好数帧数）
        for (i, y) in [0u16, 24, 48].into_iter().enumerate() {
            dirty.add(Rect {
                x: 0,
                y,
                width: 8,
                height: 8,
            });
            let _ = i;
        }
        flush_frames(&image, &mut dirty, &tx, &mut in_flight);
        let sent = std::iter::from_fn(|| rx.try_recv().ok()).count();
        assert!(
            sent >= 1 && in_flight >= 1,
            "配额内应当发出帧（实发 {sent}，在途 {in_flight}）"
        );

        // 配额已满：再有更新一律不发，且**不能丢**
        in_flight = MAX_FRAMES_IN_FLIGHT;
        dirty.add(Rect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        });
        flush_frames(&image, &mut dirty, &tx, &mut in_flight);
        assert!(rx.try_recv().is_err(), "配额耗尽时不该再发帧");
        assert!(
            dirty.has_pending(),
            "配额耗尽时脏矩形必须留着继续合并——丢了就是画面永久缺一块"
        );

        // 回执归还配额 → 积压发得出去
        in_flight = 0;
        flush_frames(&image, &mut dirty, &tx, &mut in_flight);
        assert!(rx.try_recv().is_ok(), "回执归还配额后积压必须发出");
        assert!(!dirty.has_pending(), "发完之后不该还有积压");
    }

    /// **一次 flush 内也不得超发**（2026-08-27 复核抓到的自造 bug）：
    /// 配额判断曾经只在 `for rect in dirty.take()` **之前**做一次，
    /// 而循环里每发一个矩形 +1——DirtyRects 最多攒 256 块，于是一次 flush
    /// 能把在途从 0 拉到 256，背压名存实亡（正是它要防的那个卡死）。
    #[test]
    fn one_flush_cannot_exceed_the_budget_even_with_many_rects() {
        use ironrdp_graphics::image_processing::PixelFormat;
        let image = DecodedImage::new(PixelFormat::RgbA32, 512, 512);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut dirty = DirtyRects::new((512, 512));
        let mut in_flight = 0u32;

        // 10 块互不相邻的脏区（间隔够大，不会被贪心合并成一块）
        for i in 0..10u16 {
            dirty.add(Rect {
                x: 0,
                y: i * 48,
                width: 8,
                height: 8,
            });
        }
        flush_frames(&image, &mut dirty, &tx, &mut in_flight);

        let sent = std::iter::from_fn(|| rx.try_recv().ok()).count();
        assert!(
            sent as u32 <= MAX_FRAMES_IN_FLIGHT,
            "一次 flush 发了 {sent} 帧，超过在途上限 {MAX_FRAMES_IN_FLIGHT}——背压失效"
        );
        assert!(
            in_flight <= MAX_FRAMES_IN_FLIGHT,
            "在途计数 {in_flight} 超过上限"
        );
        assert!(
            dirty.has_pending(),
            "没发出去的矩形必须留在 dirty 里继续合并（丢了就是画面永久缺块）"
        );
    }

    /// **通道绑定公钥的字节格式**（2026-08-27 真机翻车的根因，必须钉死）：
    /// 必须是 SubjectPublicKey 的原始字节（BIT STRING 内容），而**不是**
    /// 整个 SubjectPublicKeyInfo 的 DER。曾经手搓的版本传了后者，服务端
    /// 哈希对不上，在 NLA 阶段回 TLS InternalError 断连——mstsc 能连、
    /// 我们必死，itest 的 xrdp 无 CredSSP 又测不到这条路。
    ///
    /// 判据用一份**固定的自签证书**（openssl 生成、DER 文件内嵌于
    /// tests-data/），同时断言：
    /// ① 等于 SubjectPublicKey 原始字节；
    /// ② **不等于** SPKI DER（把实现改回 to_der() 此处必红）；
    /// ③ 长度互相咬合（防止空实现骗过 ①②）。
    #[test]
    fn credssp_channel_binding_key_is_raw_spk_not_spki_der() {
        let der = include_bytes!("../tests-data/credssp-cert-2048-rsa.der");
        use x509_cert::der::Decode as _;
        let cert = x509_cert::Certificate::from_der(der.as_slice()).expect("内嵌证书 DER 损坏");

        let got = credssp_channel_binding_key(&cert).expect("提取失败");

        // ① 与 SubjectPublicKey 原始字节一致
        let raw_spk = cert
            .tbs_certificate
            .subject_public_key_info
            .subject_public_key
            .as_bytes()
            .expect("原始字节应可得");
        assert_eq!(&got, raw_spk, "必须是 SubjectPublicKey 的原始字节");

        // ② 不是 SPKI 的 DER（历史 bug 的形态：多包一层算法标识）
        let spki_der = cert
            .tbs_certificate
            .subject_public_key_info
            .to_der()
            .expect("SPKI DER 应可编码");
        assert_ne!(
            got, spki_der,
            "传成了 SPKI DER——2026-08-27 那次「服务端 InternalError 断连」就是这么来的"
        );

        // ③ 数量级健全：2048 位 RSA 公钥的 RSAPublicKey DER ≈ 270 字节，
        // SPKI DER ≈ 294，整证书 793。这三条互相咬合，防止空实现骗过 ①②。
        assert_eq!(
            got.len(),
            270,
            "2048 位 RSA 的 RSAPublicKey DER 应为 270 字节"
        );
        assert_eq!(spki_der.len(), 294);
    }

    /// 错误链必须整条摊平——只打最外层就是 2026-08-27 那次排查卡住的原因：
    /// 用户拿到的报错是 `[read frame by hint @ connector.rs:175] custom error`，
    /// 除了行号什么信息都没有，真正的原因在 source() 里。
    #[test]
    fn error_chain_walks_the_whole_source_chain() {
        #[derive(Debug)]
        struct Layer(&'static str, Option<Box<Layer>>);
        impl std::fmt::Display for Layer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.0)
            }
        }
        impl std::error::Error for Layer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                self.1
                    .as_deref()
                    .map(|l| l as &(dyn std::error::Error + 'static))
            }
        }

        let deep = Layer(
            "外层",
            Some(Box::new(Layer(
                "中层",
                Some(Box::new(Layer("真正的原因", None))),
            ))),
        );
        assert_eq!(error_chain(&deep), "外层 ← 中层 ← 真正的原因");

        // 单层（无 source）不该多出分隔符
        assert_eq!(error_chain(&Layer("就一层", None)), "就一层");
    }

    /// 成环的错误链不得把进程挂死（自定义 Error 实现里 source() 指回自己是
    /// 见过的写法）。排查工具本身必须比被排查的东西更结实。
    #[test]
    fn error_chain_terminates_on_a_cyclic_chain() {
        struct Loop;
        impl std::fmt::Debug for Loop {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("Loop")
            }
        }
        impl std::fmt::Display for Loop {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("环")
            }
        }
        impl std::error::Error for Loop {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                // 指回一个与自己等价的 'static 值：形成无限链
                static SELF: Loop = Loop;
                Some(&SELF)
            }
        }
        // 不挂死即为通过；顺带断言确实被截断而不是无限拼接
        let s = error_chain(&Loop);
        assert_eq!(s.matches('环').count(), 17, "应为 1 层自身 + 16 层上限");
    }

    #[test]
    fn fingerprint_format_matches_ssh_style() {
        let fp = fingerprint_sha256(&[0xAB, 0xCD]);
        // sha256(AB CD) 的完整摘要——指纹不是截断，64 个十六进制字符一个不少
        assert_eq!(
            fp,
            "12:3d:4c:7e:f2:d1:60:0a:1b:3a:0f:6a:dd:c6:0a:10:f0:5a:34:95:c9:40:9f:2e:cb:f4:cc:09:5d:00:0a:6b"
        );
    }

    #[test]
    fn rfc3339_epoch_and_known_dates() {
        assert_eq!(rfc3339_of(0), "1970-01-01T00:00:00Z");
        // 2026-08-27 00:00:00 UTC = 1787788800（2026-01-01 + 238 天）
        assert_eq!(rfc3339_of(1_787_788_800), "2026-08-27T00:00:00Z");
        // 闰年边界：2024-02-29 23:59:59 = 1709251199
        assert_eq!(rfc3339_of(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(rfc3339_of(1_709_251_200), "2024-03-01T00:00:00Z");
    }

    #[test]
    fn key_input_maps_scancode_and_extended() {
        // 普通键：extended=false，低 8 位即扫描码
        let ops = to_operations(&InputEvent::Key {
            scancode: 0x1F,
            extended: false,
            down: true,
        });
        assert!(
            matches!(ops[0], ironrdp_input::Operation::KeyPressed(sc) if sc.as_u8() == (false, 0x1F)),
            "普通键映射错"
        );
        // **扩展键**（方向键/右 Ctrl 等，0xE0 前缀族）：extended 位必须原样穿到
        // Scancode——丢了它，左方向键会变成小键盘 4（0x4B 一族的经典混淆）。
        // 第一版判据只测普通键，「丢 extended」的变异对它是绿的。
        let ops = to_operations(&InputEvent::Key {
            scancode: 0x4B,
            extended: true,
            down: true,
        });
        assert!(
            matches!(ops[0], ironrdp_input::Operation::KeyPressed(sc) if sc.as_u8() == (true, 0x4B)),
            "扩展键的 extended 位丢了"
        );
    }

    #[test]
    fn mouse_button_moves_before_pressing() {
        let ops = to_operations(&InputEvent::MouseButton {
            button: MouseButton::Left,
            down: true,
            x: 100,
            y: 200,
        });
        // 第一件事必须是移动——按下带着坐标落在旧位置就是点错处
        assert!(matches!(
            ops[0],
            ironrdp_input::Operation::MouseMove(ironrdp_input::MousePosition { x: 100, y: 200 })
        ));
        assert!(matches!(
            ops[1],
            ironrdp_input::Operation::MouseButtonPressed(_)
        ));
    }
}
