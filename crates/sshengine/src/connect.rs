use crate::auth::{self, CredentialSet, Method};
use crate::events::{HostKeyChoice, Prompt, SessionEvents};
use crate::hostkey::{self, Decision, PresentedKey, TrustStore};
use crate::secrets::{self, SecretSource};
use crate::Error;
use base64::Engine;
use fs_connmgr::Profile;
#[allow(unused_imports)]
use fs_connmgr::Protocol as _;
use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, PublicKey, PublicKeyBase64};
use sqlx::SqlitePool;
use std::sync::Arc;
use zeroize::Zeroizing;

/// 连接期观测到的事实（M4a 连接详情弹层：认证方式 / 主机密钥指纹）。
///
/// 为什么走 host:port 键的进程级表、而不是从 `connect()` 返回：
/// ① 改返回类型要动所有调用点（含重连路径与跳板链的**每一跳**）；
/// ② 这些事实的产生者分散在两处——主机密钥在 `check_server_key`（Handler 回调，
///    同步上下文、拿不到 Handle），认证方法在认证循环里；一个共享表让两处各自
///    就地记账，而不必把一个可变引用穿过整条调用链。
///
/// 代价如实记下：同一 host:port 的**并发**连接会互相覆盖（后连的赢）。对详情弹层
/// 这是可接受的——它展示的是「这台主机当前这条连接」的事实，而同机多开的会话
/// 出示的必然是同一把主机密钥；认证方法在极端情况下可能显示成另一条会话用的那个。
/// 要做到严格 per-session 需把事实穿进 `establish_session`，那是更大的改动，
/// 留给需要时再做——**不假装**当前实现已经严格。
#[derive(Debug, Clone, Default)]
pub struct ObservedFacts {
    /// 服务端主机密钥类型（如 `ssh-ed25519`）
    pub key_type: String,
    /// 主机密钥 SHA256 指纹（OpenSSH 口径 `SHA256:...`）
    pub fingerprint_sha256: String,
    /// 最终成功的认证方法（`publickey` / `password` / `keyboard-interactive` / `agent`）
    pub auth_method: String,
}

static OBSERVED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, ObservedFacts>>,
> = std::sync::LazyLock::new(Default::default);

fn facts_key(host: &str, port: u16) -> String {
    format!("{host}:{port}")
}

/// 记下主机密钥事实（`check_server_key` 内调用）。
pub fn note_host_key(host: &str, port: u16, key_type: &str, fingerprint_sha256: &str) {
    let mut m = OBSERVED.lock().unwrap_or_else(|p| p.into_inner());
    let e = m.entry(facts_key(host, port)).or_default();
    e.key_type = key_type.to_string();
    e.fingerprint_sha256 = fingerprint_sha256.to_string();
}

/// 记下最终成功的认证方法（认证循环的 Success 分支调用）。
pub fn note_auth_method(host: &str, port: u16, method: &str) {
    let mut m = OBSERVED.lock().unwrap_or_else(|p| p.into_inner());
    m.entry(facts_key(host, port)).or_default().auth_method = method.to_string();
}

/// 读某主机的观测事实（app 层连接详情命令消费）。未连过则为全空。
pub fn observed_facts(host: &str, port: u16) -> ObservedFacts {
    OBSERVED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&facts_key(host, port))
        .cloned()
        .unwrap_or_default()
}

pub struct Connector {
    host: String,
    port: u16,
    policy: fs_connmgr::HostKeyPolicy,
    pins: Vec<fs_connmgr::HostKeyPin>,
    events: Arc<dyn SessionEvents>,
    /// SqlitePool 克隆廉价（内部 Arc）；Handler 需 'static，故持有 pool 而非借用 TrustStore
    pool: SqlitePool,
}

impl Connector {
    pub fn new(
        host: String,
        port: u16,
        policy: fs_connmgr::HostKeyPolicy,
        pins: Vec<fs_connmgr::HostKeyPin>,
        events: Arc<dyn SessionEvents>,
        pool: SqlitePool,
    ) -> Self {
        Self {
            host,
            port,
            policy,
            pins,
            events,
            pool,
        }
    }

    /// 连接期观测事实的读句柄（app 层经 host:port 取用，M4a 连接详情弹层）。
    pub fn facts(&self) -> ObservedFacts {
        observed_facts(&self.host, self.port)
    }

    /// decide→查询→落库 的**唯一实现**：check_server_key 生产路径与单测共用。
    /// 按 policy + 信任库 + 钉扎 + 用户回调得出是否接受；AcceptAndRecord 立即落库
    /// （首触经 `record`；密钥变更经 `replace` 清退旧行再落新行——信任库恒「每 host:port
    /// 至多一行当前有效密钥」，旧键退库后再出示判 Changed 硬失败 + 弹框，消除 fail-open；R13/spec §2.1）。
    pub async fn resolve_host_key(
        trust: &TrustStore<'_>,
        events: &dyn SessionEvents,
        host: &str,
        port: u16,
        policy: fs_connmgr::HostKeyPolicy,
        pins: &[fs_connmgr::HostKeyPin],
        presented: &PresentedKey,
    ) -> Result<bool, Error> {
        let known = trust.lookup(host, port).await?;
        // 审计2 #22：吊销名单与信任名单一起取，交给 `decide` 做第一道判断。
        // 单独一次查询而非塞进 `lookup`：两者的语义相反（黑名单 vs 白名单）、行数上限不同，
        // 合并返回只会诱使下游写出 `known.first()` 却拿到一把吊销密钥。
        let revoked = trust.lookup_revoked(host, port).await?;
        match hostkey::decide(policy, pins, &known, &revoked, presented) {
            Decision::Accept => Ok(true),
            // 硬拒绝：**不经** `host_key_decision`，因此前端连按钮都看不到，无从点穿。
            // 理由由 `decide` 就地给出（审计 P1：旧实现把理由写死成「strict 模式」，
            // 而钉扎策略下的两种拒绝成因完全不同，说错成因等于没说）。
            Decision::Refuse { reason } => {
                events.status(&format!("拒绝连接 {host}:{port}——{reason}"));
                Ok(false)
            }
            hint @ (Decision::AskTofu | Decision::Changed { .. }) => {
                match events.host_key_decision(host, port, presented, &hint) {
                    HostKeyChoice::Refuse => Ok(false),
                    HostKeyChoice::AcceptOnce => Ok(true),
                    HostKeyChoice::AcceptAndRecord => {
                        let new_key = hostkey::StoredKey {
                            key_type: presented.key_type.clone(),
                            key_blob: presented.key_blob.clone(),
                            fingerprint_sha256: presented.fingerprint_sha256.clone(),
                            source: "tofu".into(),
                        };
                        if matches!(hint, Decision::Changed { .. }) {
                            // 密钥变更 + 显式接受 → 替换：同事务清退该 host:port 全部旧行后插新行（R13）
                            trust.replace(host, port, &new_key).await?;
                        } else {
                            // TOFU 首触 → 首触接受落库
                            trust.record(host, port, &new_key, "tofu").await?;
                        }
                        Ok(true)
                    }
                }
            }
        }
    }
}

impl client::Handler for Connector {
    type Error = russh::Error;

    async fn check_server_key(&mut self, server_key: &PublicKey) -> Result<bool, Self::Error> {
        // OpenSSH 指纹对二进制 key blob 求 SHA256：先解码 base64，切勿对文本求
        let blob = base64::engine::general_purpose::STANDARD
            .decode(server_key.public_key_base64())
            .map_err(|_| russh::Error::Keys(russh::keys::Error::KeyIsCorrupt))?;
        let presented = PresentedKey {
            key_type: server_key.algorithm().as_str().to_string(),
            key_blob: server_key.public_key_base64(),
            fingerprint_sha256: hostkey::fingerprint_sha256(&blob),
        };
        // 记下观测事实（M4a 连接详情）：此处是**唯一**能看到真实出示密钥的地方。
        // 记在校验之前：即便随后被拒（policy/钉扎不符），用户也需要在错误里看到
        // 「对方出示的是哪把钥匙」——那正是判断是不是 MITM 的依据。
        note_host_key(
            &self.host,
            self.port,
            &presented.key_type,
            &presented.fingerprint_sha256,
        );
        let trust = TrustStore::new(&self.pool);
        Self::resolve_host_key(
            &trust,
            self.events.as_ref(),
            &self.host,
            self.port,
            self.policy,
            &self.pins,
            &presented,
        )
        .await
        .map_err(|e| russh::Error::IO(std::io::Error::other(e.to_string())))
    }
}

/// direct-tcpip 通道即流（russh 0.62.4 的 connect_stream 只要流，无 peer 参数）。
///
/// 之所以要这个 marker trait 而不是直接写 `Box<dyn AsyncRead + AsyncWrite + Unpin + Send>`：
/// trait object 至多允许**一个**非 auto trait，AsyncRead 与 AsyncWrite 两个都是非 auto trait，
/// 并列写会 E0225。以本 trait 取二者之并、再给全体满足者一个 blanket impl 即可装箱。
pub trait AsyncStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> AsyncStream for T {}
pub type BoxedStream = Box<dyn AsyncStream>;

// ── P1-8：分阶段超时边界 ────────────────────────────────────────────────────────
//
// 在此之前，建连路径上**唯一**的时间约束是 keepalive（15 s × 3），而 keepalive 只在
// 连接**已经建立**之后才起作用：DNS 解析、TCP 建连、SSH 握手、认证、开 channel 这五段
// 全都没有上限。一台半开（SYN 收了但再无回音）或行为异常（吞掉 KEX 不回应）的服务器
// 因此可以把一个 worker 无限期占住——重连逻辑再把这件事乘上重试次数，故障形态是
// 「界面上一直转圈、任务永不结束」，与 S27/S33 记录的挂死同类：**挂死必须由客户端封住**。
//
// 各段单独设常量、超时文案单独点名阶段，是因为这五段的排查手段完全不同：DNS 超时查解析、
// TCP 超时查防火墙/端口、KEX 超时查算法协商与中间设备、认证超时查服务端 PAM/LDAP 后端、
// 开 channel 超时查服务端 `MaxSessions`。若统一折叠成一个 io error（russh 的默认行为），
// 用户拿到的只有「操作超时」，等于把这五条排查路径全抹平。

/// DNS 解析 + TCP 建连上限。纯网络握手，不含任何人机回路。
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// SSH 握手（版本交换 + KEX + `check_server_key`）上限。
///
/// 之所以比建连预算宽出一个量级：`check_server_key` 在 TOFU / 密钥变更时会**同步阻塞**等待
/// 用户在对话框上裁决（app 层 `GuiEvents::PROMPT_TIMEOUT` = 120 s），而这段等待就发生在
/// `connect_stream` 的 await **内部**，无法从外层择出来。故预算 = 网络握手 30 s + 人机回路
/// 120 s + 余量 30 s。压到纯网络的量级就等于「用户泡杯茶回来再点接受」必被判超时——
/// 那是把一个安全确认框做成了不可用的东西，用户学到的只会是「先点接受再说」。
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// **单次**认证请求的往返上限（不是整条认证状态机的上限）。
///
/// 逐次而非整体设限的理由：kbd-interactive 的用户应答发生在 `events.kbd_interactive` 这个
/// **同步回调**里，恰好落在两次 await 之间，天然不在本预算内（其超时由 app 层 PROMPT_TIMEOUT
/// 负责）。整体设限则会把「用户思考时间」和「服务器不回话」算进同一个池子，两者的合理量级
/// 相差两个数量级。60 s 是给服务端认证后端（PAM/LDAP/RADIUS/OTP 校验）留的余量。
const AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// 开 direct-tcpip 通道上限。服务端 `MaxSessions` 打满或跳板机禁了转发时，
/// 请求可能既不成功也不被拒绝，这里必须有个头。
const CHANNEL_OPEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

// 「给一个 await 套上点名阶段的超时」（P1-8）此前在本文件里自带一份实现，把超时压成
// `Error::Connect(String)`。审计2 #11 给全仓库的远程 I/O 都补上预算之后，那份实现就成了
// **第二种超时表示**——`Error::is_timeout()` 认得 `Error::Timeout` 却认不出它，于是同一件事
// 在建连阶段和传输阶段要用两套判据，而「两套判据」正是这类边界最容易漏的地方。
// 现在统一走 `crate::timeouts::with_timeout`。
use crate::timeouts::with_timeout;

pub async fn connect(
    profile: &Profile,
    secrets: &dyn SecretSource,
    pool: &SqlitePool,
    events: Arc<dyn SessionEvents>,
) -> Result<client::Handle<Connector>, Error> {
    if profile.jump.is_empty() {
        return connect_inner(profile, secrets, pool, events, None).await;
    }
    // ProxyJump（spec §2.1）：h[0] 直连；h[i] 经 h[i-1] 的 direct-tcpip；目标经最后一跳。
    // 各跳主机密钥校验同样走 Connector::check_server_key → resolve_host_key（未知/变更 →
    // host_key_decision 回调，app 层桥接 hostkey:prompt），但**每一跳用自己的 policy/pins**
    // （P1-9，详见 hop_profile）：失陷跳板机同样是 MITM 向量（spec §6.1），只是「校验谁」
    // 必须按主机身份逐跳分开，不能拿目标机的钉去校验跳板机。
    // S36：逐跳播报。跳板链上任何一跳的握手/认证失败都只会冒泡出一个不带跳数的
    // Error::Connect / Error::Auth（Error 里没有 profile 名的位置），用户看到「认证失败」
    // 却无从判断失败在哪一跳。status 回调本就是连接过程的进度通道，标注当前跳成本近乎为零。
    let first = &profile.jump[0];
    events.status(&format!(
        "跳板 1/{}：直连 {}:{}",
        profile.jump.len(),
        first.host,
        first.port
    ));
    let mut prev = connect_inner(
        &hop_profile(first, profile.host_key_policy),
        secrets,
        pool,
        events.clone(),
        None,
    )
    .await?;
    for (i, hop) in profile.jump[1..].iter().enumerate() {
        events.status(&format!(
            "跳板 {}/{}：经上一跳转发至 {}:{}",
            i + 2,
            profile.jump.len(),
            hop.host,
            hop.port
        ));
        let stream = forward(&mut prev, &hop.host, hop.port).await?;
        prev = connect_inner(
            &hop_profile(hop, profile.host_key_policy),
            secrets,
            pool,
            events.clone(),
            Some(stream),
        )
        .await?;
    }
    events.status(&format!(
        "经末跳转发至目标 {}:{}",
        profile.host, profile.port
    ));
    let stream = forward(&mut prev, &profile.host, profile.port).await?;
    connect_inner(profile, secrets, pool, events, Some(stream)).await
}

/// 经跳板链打开一条到**任意 host:port** 的裸转发流（RDP 阶段 2）。
///
/// # 与 [`connect`] 的分工
///
/// `connect` 的产物是一条 SSH 会话（末跳之后还要再做一次 SSH 握手+认证）。
/// 这里的产物是**流本身**：调用方拿去说别的协议。RDP 正是这样用的——
/// 主程序把这条流当作到 RDP 服务器的传输，字节经 stdio 管道喂给 helper，
/// helper 完全不知道自己在跟谁说话（它没有网络）。
///
/// 于是「经 SSH 跳板连 RDP」在架构上是免费的：跳板链一个字节都不用改，
/// 只是末跳的 `forward` 目标从「SSH 端口」换成了「RDP 端口」。
///
/// # 逐跳校验一寸未松
///
/// 每一跳仍走 `hop_profile`（该跳自己的 policy/pins），理由见那个函数的文档：
/// 拿目标机的钉去校验跳板机是类型混淆，而失陷的跳板机同样是 MITM 向量。
///
/// # 目标端口不是 profile.port 的默认值
///
/// 调用方显式传 `target_host`/`target_port`：RDP 的 profile 里 `port` 就是
/// 3389，但把它写死在这里会让本函数只能服务 RDP 一种用途。
pub async fn open_forwarded_stream(
    profile: &Profile,
    target_host: &str,
    target_port: u16,
    secrets: &dyn SecretSource,
    pool: &SqlitePool,
    events: Arc<dyn SessionEvents>,
) -> Result<BoxedStream, Error> {
    if profile.jump.is_empty() {
        return Err(Error::Connect(
            "该连接未配置跳板——直连不需要经本函数".into(),
        ));
    }
    let first = &profile.jump[0];
    events.status(&format!(
        "跳板 1/{}：直连 {}:{}",
        profile.jump.len(),
        first.host,
        first.port
    ));
    let mut prev = connect_inner(
        &hop_profile(first, profile.host_key_policy),
        secrets,
        pool,
        events.clone(),
        None,
    )
    .await?;
    for (i, hop) in profile.jump[1..].iter().enumerate() {
        events.status(&format!(
            "跳板 {}/{}：经上一跳转发至 {}:{}",
            i + 2,
            profile.jump.len(),
            hop.host,
            hop.port
        ));
        let stream = forward(&mut prev, &hop.host, hop.port).await?;
        prev = connect_inner(
            &hop_profile(hop, profile.host_key_policy),
            secrets,
            pool,
            events.clone(),
            Some(stream),
        )
        .await?;
    }
    events.status(&format!("经末跳转发至 {target_host}:{target_port}"));
    let stream = forward(&mut prev, target_host, target_port).await?;
    // **末跳的 SSH 会话必须活着**：转发通道挂在它上面，handle 一 drop
    // 通道就断。把它与流绑成一个整体交出去——调用方只管流，
    // 生命周期由 ForwardedStream 兜住。
    Ok(Box::new(ForwardedStream {
        stream,
        _keepalive: prev,
    }))
}

/// 转发流 + 它所依赖的末跳 SSH 会话。
///
/// 存在的唯一理由是**生命周期**：`channel.into_stream()` 拿到的流不持有
/// `client::Handle`，handle 一 drop，russh 会关掉整条会话连同其上的转发通道。
/// 历史上这类 bug 的表现是「连上了，几秒后无故断开」——而那几秒正是
/// handle 被 drop 到对端发现连接消失的间隔。
struct ForwardedStream {
    stream: BoxedStream,
    _keepalive: client::Handle<Connector>,
}

impl tokio::io::AsyncRead for ForwardedStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for ForwardedStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

/// 跳板临时 Profile：host/port/username/auth **以及主机密钥策略/钉扎**一律取自该跳自身。
///
/// P1-9（安全）：历史实现在这里写的是 `parent.host_key_policy` / `parent.host_key_pins.clone()`。
/// 指纹钉是**绑定到某台具体主机身份**的断言——「`host:port` 这台机器的公钥必须恰好是这一把」——
/// 拿目标服务器的钉去校验跳板机属于类型混淆，两台机器的公钥本就不可能相同：
/// `hostkey::decide` 见 `pins` 非空即走钉扎分支，跳板机出示的密钥恒不匹配 → 每一跳都判
/// `Changed`。于是只有两种结局，且都是坏的：① pinned 策略下跳板链**必然连不上**；
/// ② 用户为了连上去在那个「密钥已变更」的红框上点接受——跳板机就此**从未被真正校验**，
/// 而 UI 上显示的却是「已按钉扎策略校验」。
///
/// known-host 身份始终是**该跳自己的 host:port**（`Connector::new` 收的就是 hop 的 host/port），
/// 因此跳板机的密钥按本机对**它**的认知校验，绝不拿目标机的钉去比。
///
/// ── 审计 P1（`connect.rs:260`）：严格度意图对跳板机静默失效 ─────────────────────
/// 旧实现是 `h.host_key_policy.unwrap_or_default()`，即未逐跳配置时回落 Tofu；而上一段
/// 注释里承诺的补救手段（「在这一跳上配 `JumpHop::host_key_policy`」）**在产品里根本没有写入端**：
/// ProfileDialog 无跳板编辑 UI（`save()` 里写着 `jump: initial?.jump ?? []` 并自注「原样透传」），
/// 而 jump 链唯一现实来源 `import_json` 又逐跳执行 `h.host_key_policy = None`。
/// 于是「可创建的跳板链」与「可达的补救手段」互斥：**hop 恒为 Tofu**。
/// 用户为目标机选了 Strict，堡垒机这一跳照样弹一个可点接受的 TOFU 框——
/// 攻击者由此可完整 MITM 到堡垒机的会话（窃取堡垒机口令 / kbd 应答），
/// 并在用户点过一次「接受并记录」后把伪造密钥长期写进信任库。
///（危害到此为止：目标机那一跳仍以完整 profile 在转发流之上做端到端握手，
///  攻击者若不出示自己的密钥就读不到内层，出示了就会被目标机的 Strict/known_hosts 挡下。）
///
/// 修法的关键是把两样东西分开——它们过去被一并丢弃，这才是缺陷成因：
///   · **pins 是绑定到具体主机身份的断言**（「这台机器的公钥必须恰好是这一把」），
///     跨主机继承属于类型混淆，**绝不继承**（这是旧实现已经做对的部分，保持不变）；
///   · **policy 是与主机无关的严格度意图**（「我不接受点穿」），它本就该覆盖整条链——
///     一条链的安全性取下限，堡垒机是链上**第一个**拿到你流量的节点。
///
/// 故优先级：逐跳显式配置 > 继承父 Profile 的 policy。其中 `FingerprintPinned` 在
/// **该跳自己没有钉**时降级为 `Strict`：钉扎白名单无法继承（见上），而继承来的意图里
/// 仍然可满足的最强一档就是「必须已在信任库中，不许点穿」。若直接继承 `FingerprintPinned`，
/// 按新的 `decide` 语义空钉恒硬拒 → 凡配了钉扎的 profile 跳板链一律连不上，
/// 而用户又没有添加 hop 钉的入口，等于把功能焊死。
fn hop_profile(h: &fs_connmgr::JumpHop, inherited: fs_connmgr::HostKeyPolicy) -> Profile {
    let policy = h.host_key_policy.unwrap_or(match inherited {
        fs_connmgr::HostKeyPolicy::FingerprintPinned if h.host_key_pins.is_empty() => {
            fs_connmgr::HostKeyPolicy::Strict
        }
        other => other,
    });
    Profile {
        id: uuid::Uuid::nil(),
        name: format!("jump:{}", h.host),
        group_path: None,
        host: h.host.clone(),
        port: h.port,
        username: h.username.clone(),
        protocol: fs_connmgr::Protocol::Ssh, // 跳板链合成的 Profile 恒为 SSH（RDP 不走跳板链）
        auth: h.auth.clone(),
        jump: vec![],
        host_key_policy: policy,
        host_key_pins: h.host_key_pins.clone(),
        env: Default::default(),
        term: Default::default(),
        sftp: Default::default(),
        ai_policy: Default::default(),
        serial: Default::default(),
    }
}

/// 经上一跳的 direct-tcpip 通道打开到目标的转发流（端口参数为 u32）。
///
/// P1-8：开通道同样限时。跳板链上这一步的失败模式最隐蔽——跳板机若配了
/// `AllowTcpForwarding no` 或 `MaxSessions` 已打满，请求可能长时间既不成功也不被拒绝，
/// 而此时前一跳已经登录成功，用户看到的是「连上了却卡住」。
async fn forward(
    handle: &mut client::Handle<Connector>,
    host: &str,
    port: u16,
) -> Result<BoxedStream, Error> {
    let channel = with_timeout(
        &format!("经跳板开转发通道至 {host}:{port}"),
        CHANNEL_OPEN_TIMEOUT,
        handle.channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0),
    )
    .await?
    .map_err(|e| Error::Connect(e.to_string()))?;
    Ok(Box::new(channel.into_stream()))
}

async fn connect_inner(
    profile: &Profile,
    secrets: &dyn SecretSource,
    pool: &SqlitePool,
    events: Arc<dyn SessionEvents>,
    transport: Option<BoxedStream>,
) -> Result<client::Handle<Connector>, Error> {
    // 审计 P2：把信任库里已记录的那把密钥的算法族排到协商偏好最前（OpenSSH `order_hostkeyalgs`
    // 的同款做法，理由见 `hostkey::preferred_host_key_order`）。这一步必须在握手**之前**做完，
    // 因为 check_server_key 拿到的已经是协商完的结果——那时再发现算法对不上，只剩下弹一个
    // 与真 MITM 逐字相同的假告警这一条路。
    //
    // 查库失败不阻断连接：偏好只是排序，排不动就用 russh 的默认序，`decide` 那一关一寸未松。
    let preferred = match TrustStore::new(pool)
        .lookup(&profile.host, profile.port)
        .await
    {
        Ok(known) => hostkey::preferred_from_known(&known),
        Err(e) => {
            tracing::warn!("读取信任库以排序主机密钥算法失败（回退默认序）：{e}");
            Default::default()
        }
    };

    // 用结构体更新语法而非 `let mut c = default(); c.field = ..`：后者触发
    // clippy::field_reassign_with_default（style，默认 warn），CI 的 `-D warnings` 下即红。
    let config = client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(15)),
        keepalive_max: 3,
        preferred,
        ..Default::default()
    };

    let connector = Connector::new(
        profile.host.clone(),
        profile.port,
        profile.host_key_policy,
        profile.host_key_pins.clone(),
        events.clone(),
        pool.clone(),
    );

    let config = Arc::new(config);
    let handle = match transport {
        None => {
            let addr = format!("{}:{}", profile.host, profile.port);
            // P1-8：这里刻意**不用** `client::connect`。它把「DNS 解析 + TCP 建连」与「SSH 握手」
            // 揉进同一个 await，两段无法分别限时，而两段的合理预算相差近十倍（后者要含 TOFU
            // 弹框的人机回路）。下面按 russh 0.62.4 `client::connect` 的实现原样展开
            //（TcpStream::connect → set_nodelay → connect_stream），语义不变，只为拆开限时。
            let socket = with_timeout(
                &format!("DNS 解析 + TCP 建连 {addr}"),
                CONNECT_TIMEOUT,
                tokio::net::TcpStream::connect(&addr),
            )
            .await?
            .map_err(|e| Error::Connect(format!("连接 {addr} 失败：{e}")))?;
            if config.nodelay {
                // 与 russh 同口径：set_nodelay 失败不影响可用性，记 warn 后照常继续
                if let Err(e) = socket.set_nodelay(true) {
                    tracing::warn!("set_nodelay 失败（不影响连接）：{e}");
                }
            }
            handshake(config, socket, connector).await?
        }
        // russh 0.62.4 无 connect_on：用三参 connect_stream(config, stream, handler)
        Some(stream) => handshake(config, stream, connector).await?,
    };

    authenticate(handle, profile, secrets, events).await
}

/// SSH 握手一段的限时封装（P1-8）。写成泛型函数而非在两个分支各抄一遍：
/// 直连分支传 `TcpStream`、跳板分支传 `BoxedStream`，两者类型不同但阶段语义与预算完全一致，
/// 抄两遍必然在后续改动中漂移（S17 同类教训）。
async fn handshake<R>(
    config: Arc<client::Config>,
    stream: R,
    connector: Connector,
) -> Result<client::Handle<Connector>, Error>
where
    R: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    with_timeout(
        "SSH 握手（版本交换 / 密钥交换 / 主机密钥校验）",
        HANDSHAKE_TIMEOUT,
        client::connect_stream(config, stream, connector),
    )
    .await?
    .map_err(|e| Error::Connect(e.to_string()))
}

/// 单次 keyboard-interactive 认证内允许的最大 InfoRequest 回合数（S33）。
const KBD_MAX_ROUNDS: usize = 32;

/// 单次认证动作的结果：AuthResult 与 KeyboardInteractiveAuthResponse 统一于此。
enum AuthStep {
    Success,
    /// 认证请求**真的发出去了**，服务器拒绝并回带新的可用方法通告。
    Failure {
        remaining: russh::MethodSet,
    },
    /// 因**本地**原因根本没发出认证请求（S38）：agent 三条传输都连不上、agent 里一把可用
    /// 身份都没有等。与 `Failure` 必须区别对待——服务器通告在这种情况下毫无变化。
    Unattempted {
        reason: String,
    },
}

/// 一次认证动作之后的候选集推进（S38）。
///
/// - `Failure`：方法真的发起过，服务器给了新通告 → 据此重算候选。
/// - `Unattempted`：方法从未发出请求，服务器通告毫无变化 → 候选集必须**原样保留**。
///   若沿用 `Failure` 语义把 `MethodSet::empty()` 灌进来，`announced_methods` 会算出空集，
///   `next_method` 随即返回 `None`，于是「agent + 口令」双配置的 profile 只要本机没起 agent，
///   连口令都不会试一次——故障形态是「凭据明明是对的却登不上」，而日志里看不出为什么。
/// - `Success`：调用方在进入本函数前已返回，这里按原样返回只为让函数总有定义。
fn advance_remaining(step: &AuthStep, prev: Vec<Method>, creds: &CredentialSet) -> Vec<Method> {
    match step {
        AuthStep::Failure { remaining } => announced_methods(remaining, creds),
        AuthStep::Success | AuthStep::Unattempted { .. } => prev,
    }
}

async fn authenticate(
    mut handle: client::Handle<Connector>,
    profile: &Profile,
    secrets: &dyn SecretSource,
    events: Arc<dyn SessionEvents>,
) -> Result<client::Handle<Connector>, Error> {
    // 认证状态机：服务器通告驱动（spec §2.1 / Task 8）
    let (creds_built, degraded_reason) = build_credentials(profile, secrets)?;
    let mut creds = creds_built;
    let mut tried: Vec<Method> = vec![];
    // 连接时输口令：档案没配口令、服务器又通告 password 时，**问一次**用户要口令。
    // 只问一次（无论取消还是答错都不重问）：答错了让服务器按密码错误收尾，比无限弹框诚实。
    let mut password_prompted = false;
    // S38：本地原因导致「未发起」的方法在此留痕。不进 tried（它没试过），也不进 remaining
    // （那是服务器的通告），最终随 Error::Auth.notes 一起交给用户——否则 agent 三管道的排查
    // 指引在「agent 不可用但还有别的方法可试」这条路径上会被整条吞掉。
    let mut notes: Vec<String> = vec![];

    // S32：先发 `auth none` 取服务器通告（RFC 4252 §5.2 正是为此设的），再进状态机。
    // 此前 remaining 初值硬编码成全四项，等于让**第一次尝试**绕开「绝不尝试服务器未通告的
    // 方法」这条硬约束（spec §2.1，`auth::next_method` 文档亦明写「∩ 服务器通告」）。
    // 后果不是理论上的：「只配了口令的 profile 连 publickey-only 服务器」这条最常见路径会把
    // 用户明文口令送给一台**永远不会使用它**的服务器，并白占一次 MaxAuthTries（堡垒机常配
    // 2–3 次，四个候选方法足以直接锁死）。审计2 #26 把 PREFERENCE 改成密钥在前、口令在后
    // **不能**替代这道探测：那条路径上口令是唯一有料的方法，排在第几都会被送出去；
    // 挡住它的只有「服务器没通告 password」这个事实本身。
    // 少数服务器允许 none 直接登入，故 Success 分支直接放行。
    let none = with_timeout(
        "认证协商（auth none 探测服务器通告）",
        AUTH_TIMEOUT,
        handle.authenticate_none(&profile.username),
    )
    .await?
    .map_err(|e| Error::Ssh(e.to_string()))?;
    // 直接就地 match 而不复用 to_step：to_step 的返回类型现在含 Unattempted 变体，而
    // `auth none` 不可能产生它，就地 match 可以免掉一个 unreachable! 分支。
    let mut remaining = match none {
        client::AuthResult::Success => return Ok(handle),
        client::AuthResult::Failure {
            remaining_methods, ..
        } => announced_methods(&remaining_methods, &creds),
    };
    loop {
        let Some(method) = auth::next_method(&creds, &tried, &remaining) else {
            // 连接时输口令：没料可试、服务器仍在通告 password、且本会话还没问过 → 问一次。
            // 注入的凭据随 `creds` 的 Zeroizing 字段在函数返回时 drop 清零，不落库、不进日志。
            if !password_prompted
                && remaining.contains(&Method::Password)
                && creds.password.is_none()
            {
                password_prompted = true;
                // 锁着时换一句提示语：固定那句「该连接未配置口令」在这里是**错的**
                // （配置了，只是锁着取不到），用户会以为自己在连接属性里存的
                // 凭据丢了。
                let pw = match degraded_reason.as_deref() {
                    Some(_) => events.password_prompt_reason(
                        "本次取不到已保存的凭据——可输入本次使用的口令，或取消后先处理凭据再连",
                    ),
                    None => events.password_prompt(),
                };
                if !pw.is_empty() {
                    creds.password = Some(Zeroizing::new(pw));
                    continue;
                }
            }
            // 审计2 #19 的配套：把「因为配置关掉了才没试」和「试了没过」区分开。
            // 只在**终态**追加，不在每轮追加——同一句话在循环里会被反复推进 notes。
            for m in auth::suppressed_by_config(&creds, &remaining) {
                notes.push(format!(
                    "服务器仍在通告 {m}，本连接持有口令，但连接设置中该方法处于关闭状态，故未尝试。\
                     若这台服务器只以 {m} 的形式收口令（常见于挂了 OTP／二次验证的堡垒机），\
                     请在连接设置中打开它后重试"
                ));
            }
            // 一条都没试、且一条材料都没有：不是「试了没过」，是「根本没料」——给一句能自解释的。
            if let Some(n) = auth::credentialless_note(&creds, &tried, &remaining) {
                notes.push(n.to_string());
            }
            // 锁着的降级走到终态（比如服务器只收 publickey、弹框路径没接上）：
            // 必须说清「材料在、只是锁着」——否则用户会以为自己在保险库里存的东西丢了。
            if let Some(reason) = degraded_reason.as_deref() {
                notes.push(reason.to_string());
            }
            // S303：有料、却与服务器要的不是一类（配了口令 / 服务器只收 publickey）。
            // 此前这一格无人认领，用户只得到 `tried: []; notes: []`——一次都没试，
            // 也没有任何一句说明为什么。三个提示函数彼此互斥，至多出一句。
            if let Some(n) = auth::material_mismatch_note(&creds, &tried, &remaining) {
                notes.push(n);
            }
            return Err(Error::Auth {
                tried: tried.iter().map(|m| m.to_string()).collect(),
                remaining: remaining.iter().map(|m| m.to_string()).collect(),
                notes: std::mem::take(&mut notes),
            });
        };
        tried.push(method);
        events.status(&format!("尝试认证方法：{method}"));
        let step = match method {
            Method::Password => {
                // Zeroizing<String> 经 as_ref()+as_str() 借出 &str，不再克隆明文（克隆体用毕不保证擦除）
                let pw = creds
                    .password
                    .as_ref()
                    .map(|z| z.as_str())
                    .unwrap_or_default();
                let r = with_timeout(
                    "password 认证请求",
                    AUTH_TIMEOUT,
                    handle.authenticate_password(&profile.username, pw),
                )
                .await?
                .map_err(|e| Error::Ssh(e.to_string()))?;
                to_step(r)
            }
            Method::KbdInteractive => {
                kbd_interactive_loop(
                    &mut handle,
                    &profile.username,
                    events.as_ref(),
                    profile.auth.kbd_auto_answer_single,
                    creds.password.as_ref().map(|z| z.as_str()),
                )
                .await?
            }
            Method::PublicKey => {
                // 同 Password 分支：借出 &str，PEM 明文不克隆
                let pem = creds
                    .public_key_pem
                    .as_ref()
                    .map(|z| z.as_str())
                    .unwrap_or_default();
                // P1-23：口令**只在真要发 publickey 请求时**才出 vault，而不是随 CredentialSet
                // 一起在整条认证状态机上全程驻留——明文在内存里的存活窗口按需缩到最短（总设计 §3.2）。
                // 未配 passphrase_vault_record 时为 None，等价于旧行为（未加密私钥）。
                let passphrase = load_passphrase(profile, secrets)?;
                let key = decode_private_key(pem, passphrase.as_ref())?;
                // authenticate_publickey 按值收 PrivateKeyWithHashAlg（非 Arc）；new() 直接返回 Self
                let key = PrivateKeyWithHashAlg::new(Arc::new(key), None);
                let r = with_timeout(
                    "publickey 认证请求",
                    AUTH_TIMEOUT,
                    handle.authenticate_publickey(&profile.username, key),
                )
                .await?
                .map_err(|e| Error::Ssh(e.to_string()))?;
                to_step(r)
            }
            Method::Agent => agent_auth(&mut handle, &profile.username).await?,
        };
        match step {
            AuthStep::Success => {
                // M4a 连接详情：记下**最终成功**的认证方法。写在 return 之前、
                // 且只写成功的那一次——记每次尝试会让详情弹层显示第一个失败的
                // 方法（用户看到「password」而实际登进去用的是 publickey）。
                note_auth_method(&profile.host, profile.port, &method.to_string());
                return Ok(handle);
            }
            step => {
                if let AuthStep::Unattempted { reason } = &step {
                    // 未发起也要让用户看见：状态栏出一行，同时留痕进 notes 供终态错误带出
                    events.status(reason);
                    notes.push(reason.clone());
                }
                remaining = advance_remaining(&step, remaining, &creds);
            }
        }
    }
}

/// agent 认证一轮：连 agent → 取身份 → 按 cap 依次尝试。
///
/// **本地**故障（连不上 agent、列不出身份、一把可用身份都没有）一律返回 `Unattempted` 而非
/// `Err`（S38）：本机没起 agent 不该让一个同时配了口令的 profile 直接连不上。`Err` 只留给
/// 传输层错误——那时连接本身已经不可用，继续试别的方法也没有意义。
async fn agent_auth(
    handle: &mut client::Handle<Connector>,
    username: &str,
) -> Result<AuthStep, Error> {
    let mut agent = match crate::agent::connect_agent_client().await {
        Ok(a) => a,
        Err(e) => {
            return Ok(AuthStep::Unattempted {
                reason: e.to_string(),
            })
        }
    };
    let ids = match agent.request_identities().await {
        Ok(ids) => ids,
        Err(e) => {
            return Ok(AuthStep::Unattempted {
                reason: format!("agent 已连上但列不出身份：{e}"),
            })
        }
    };
    let total = ids.len();
    // MVP 仅用普通公钥身份；证书身份（AgentIdentity::Certificate）留待后续
    let keys: Vec<PublicKey> = ids
        .into_iter()
        .filter_map(|id| match id {
            russh::keys::agent::AgentIdentity::PublicKey { key, .. } => Some(key),
            russh::keys::agent::AgentIdentity::Certificate { .. } => None,
        })
        .collect();
    let available: Vec<String> = keys.iter().map(|k| k.public_key_base64()).collect();
    // 键空间纠偏（R62）：`host_key_pins` 是**服务器主机密钥** blob，与 `preferred` 契约
    // （**用户 agent 公钥** blob）不是同一键空间——喂进去会使优先排序恒失效（filter 永不命中）。
    // MVP 无 agent 身份偏好字段：preferred 恒空 → 按 available 原序取前 cap 个。
    // agent 身份偏好字段随 M4+（与「容器内 agent 真测」R56 一并记入路线图）。
    let preferred: Vec<String> = Vec::new();
    let chosen = crate::auth::prioritize_agent_keys(
        &available,
        &preferred,
        crate::auth::AGENT_IDENTITY_CAP_DEFAULT,
    );
    let mut last = russh::MethodSet::empty();
    // sent 显式记录「至少真发出去过一次」。chosen ⊆ available 时下面的 find 必然命中，但把这条
    // 不变量写成运行时事实而非注释里的假设：否则 chosen 为空/find 全落空时会退化成
    // `Failure { remaining: empty }`，正是 S38 要根除的那种「静默清空候选集」。
    let mut sent = false;
    for blob in &chosen {
        let Some(key) = keys
            .iter()
            .find(|k| &k.public_key_base64() == blob)
            .cloned()
        else {
            continue;
        };
        sent = true;
        // authenticate_publickey_with：公钥按值传，signer 为 &mut AgentClient（已实现 auth::Signer）
        // P1-8：agent 签名要经 IPC 往返到 agent 进程，卡住的成因（agent 进程无响应、
        // 硬件 token 等着按指纹）与服务端无关，但表现同样是无限期挂起，故同样限时。
        let r = with_timeout(
            "agent publickey 认证请求（含 agent 签名往返）",
            AUTH_TIMEOUT,
            handle.authenticate_publickey_with(username, key, None, &mut agent),
        )
        .await?
        .map_err(|e| Error::Ssh(e.to_string()))?;
        match r {
            client::AuthResult::Success => return Ok(AuthStep::Success),
            client::AuthResult::Failure {
                remaining_methods, ..
            } => last = remaining_methods,
        }
    }
    Ok(if sent {
        AuthStep::Failure { remaining: last }
    } else {
        AuthStep::Unattempted {
            reason: format!(
                "agent 已连上但无可用公钥身份（agent 共 {total} 条身份，其中普通公钥 {} 条）",
                available.len()
            ),
        }
    })
}

/// 服务器通告 → 本地候选方法列表。**凡从通告构造候选集的地方都必须走这里**（现有两处：
/// S32 的 none 探测、每次认证失败后的重建），否则下述约束会在其中一处被漏掉。
///
/// `MethodSet` Deref 出 `&[MethodKind]`，经 `<&str>::from` 取 RFC 4252 线名映射回本地 `Method`。
/// **S25 硬约束**：agent 在协议层就是 `publickey`，线上根本没有 `"agent"` 这个方法名，故
/// `Method::from_wire` 刻意不接受它；通告里 publickey 在列且本会话启用了 agent 时必须把
/// `Method::Agent` 补回候选，否则 [`auth::next_method`] 永远选不到它，agent 认证整条路
/// **静默失效**——不报错、不告警，只是「agent 里明明有可用密钥却总是登不上」。
fn announced_methods(announced: &russh::MethodSet, creds: &CredentialSet) -> Vec<Method> {
    let mut out: Vec<Method> = announced
        .iter()
        .filter_map(|m| auth::Method::from_wire(<&str>::from(m)))
        .collect();
    if creds.agent && out.contains(&Method::PublicKey) && !out.contains(&Method::Agent) {
        out.push(Method::Agent);
    }
    out
}

fn to_step(result: client::AuthResult) -> AuthStep {
    match result {
        client::AuthResult::Success => AuthStep::Success,
        client::AuthResult::Failure {
            remaining_methods, ..
        } => AuthStep::Failure {
            remaining: remaining_methods,
        },
    }
}

/// keyboard-interactive 多轮回合（russh 0.62.4 客户端无 KbdInteractive trait，
/// 只有 authenticate_keyboard_interactive_start/_respond 两个原语）：
/// InfoRequest → 经 SessionEvents::kbd_interactive 收集应答 → respond，直至 Success/Failure。
///
/// 审计2 #36：`auto_answer_single`/`auto_answer_password` 承载档案开关
/// `kbd_auto_answer_single` 与口令——恰一个且不回显的提示可按设置以口令自动应答
/// （判定纯函数见 `auth::kbd_auto_answer`）；状态栏只报「自动应答了」这回事，
/// 提示文本与口令一律不出现（P1-19/20/23 口径）。
async fn kbd_interactive_loop(
    handle: &mut client::Handle<Connector>,
    username: &str,
    events: &dyn SessionEvents,
    auto_answer_single: bool,
    auto_answer_password: Option<&str>,
) -> Result<AuthStep, Error> {
    // P1-8：限时只包住**网络往返**（start/respond）。用户在提示框上思考的那段时间发生在下方
    // `events.kbd_interactive` 这个同步回调里，恰在两次 await 之间，天然不在预算内——
    // 其上限由 app 层 PROMPT_TIMEOUT 单独负责，两者不得混算。
    let mut response = with_timeout(
        "keyboard-interactive 认证发起",
        AUTH_TIMEOUT,
        handle.authenticate_keyboard_interactive_start(username, None::<String>),
    )
    .await?
    .map_err(|e| Error::Ssh(e.to_string()))?;
    // S33：回合数封顶。无上限时故障或恶意服务器可以无休止地回 InfoRequest，客户端就永远在
    // 「弹框 → 应答 → 再弹框」之间打转——故障形态是**挂死而非报错**（与 Task 9 第二裁判确认的
    // S27 同类：CI 表现为任务级超时，用户表现为对话框永远关不掉），此类故障必须由客户端侧封住。
    // 真实服务端的回合数是个位数（OpenSSH + PAM 通常 1–2 轮），KBD_MAX_ROUNDS 足够宽松。
    for _ in 0..KBD_MAX_ROUNDS {
        match response {
            client::KeyboardInteractiveAuthResponse::Success => return Ok(AuthStep::Success),
            client::KeyboardInteractiveAuthResponse::Failure {
                remaining_methods, ..
            } => {
                return Ok(AuthStep::Failure {
                    remaining: remaining_methods,
                });
            }
            client::KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let mapped: Vec<Prompt> = prompts
                    .iter()
                    .map(|p| Prompt {
                        text: p.prompt.clone(),
                        echo: p.echo,
                    })
                    .collect();
                // russh 字段名 instructions → trait 形参/载荷键 instruction（总设计 §2.1 v3）；不得以 `..` 丢弃
                // 审计2 #36：单提示自动应答判定先行；不满足任何一条即照旧弹框（fail-safe）。
                let answers: Vec<String> = match crate::auth::kbd_auto_answer(
                    auto_answer_single,
                    &mapped,
                    auto_answer_password,
                ) {
                    Some(answer) => {
                        events.status(
                            "keyboard-interactive 单提示自动应答：按连接设置以口令应答（提示内容不回显）",
                        );
                        vec![answer]
                    }
                    None => events.kbd_interactive(&name, &instructions, &mapped),
                };
                response = with_timeout(
                    "keyboard-interactive 应答提交",
                    AUTH_TIMEOUT,
                    handle.authenticate_keyboard_interactive_respond(answers),
                )
                .await?
                .map_err(|e| Error::Ssh(e.to_string()))?;
            }
        }
    }
    Err(Error::Ssh(format!(
        "keyboard-interactive 连续 {KBD_MAX_ROUNDS} 轮仍未给出成败结论，判为服务器异常并中止"
    )))
}

/// 加密私钥的口令（P1-23）：按需从 vault 取出，`Zeroizing<String>` 承载。
///
/// 未配置 `auth.passphrase_vault_record` 即 `None`——与历史行为逐字等价（按未加密私钥解码）。
///
/// **口令绝不出这个函数以外的任何出口**：不进 `tracing`、不进错误文案（下方 `decode_private_key`
/// 的三条文案均不含任何秘密材料）、不进任何 IPC 载荷。调用方持有的 `Zeroizing` 随该轮认证
/// drop 时清零（总设计 §3.2）。
fn load_passphrase(
    profile: &Profile,
    secrets: &dyn SecretSource,
) -> Result<Option<Zeroizing<String>>, Error> {
    let Some(rec) = profile.auth.passphrase_vault_record else {
        return Ok(None);
    };
    let s = secrets.secret(rec)?;
    // 审计2 #20：口令槽只收 Password 类记录。放宽没有好处，收紧能当场说清一类真实的误配——
    // 把私钥记录填进 passphrase 槽，旧实现的现象是一句「私钥已加密，且所给口令无法解开它」，
    // 用户会去反复核对一个根本没填错的口令。
    if s.kind != secrets::SecretKind::Password {
        return Err(Error::Ssh(format!(
            "vault 记录 {rec} 的类别是「{}」，不能用作私钥口令（该槽位只接受「{}」类记录）：\
             请把连接设置里的私钥口令改指向一条口令记录",
            s.kind.label(),
            secrets::SecretKind::Password.label()
        )));
    }
    let bytes = s.bytes;
    // 与 build_credentials 同口径的严格 UTF-8 校验（S35）：绝不 from_utf8_lossy——
    // lossy 会造出一份不受 Zeroizing 保护的裸 String，且把非法字节静默替换成 U+FFFD，
    // 结果是一个「看起来对、解不开」的口令，用户只会看到无从排查的解码失败。
    let text = std::str::from_utf8(&bytes)
        .map(|v| Zeroizing::new(v.to_string()))
        .map_err(|e| {
            Error::Ssh(format!(
                "vault 记录 {rec}（私钥口令）不是合法 UTF-8 文本（第 {} 字节起非法）",
                e.valid_up_to()
            ))
        })?;
    Ok(Some(text))
}

/// 私钥解码（P1-23）。历史实现硬编码 `decode_secret_key(pem, None)`：加密私钥一律解不开，
/// 且 russh 回的是 `Error::KeyIsEncrypted`（Display 为英文 "The key is encrypted"），经
/// `Error::Ssh(e.to_string())` 冒泡后用户看到的是一句像解析失败的话——真正的成因
/// 「这把私钥有口令，而这里没给口令」被埋掉了，而这恰恰是**用户自己一步就能修好**的问题。
///
/// 故这里把三种结局分开命名：
/// ① 需要口令但没配 → 直说要去哪儿配；
/// ② 配了口令但解不开 → 只可能是口令错或私钥损坏，不必再怀疑格式；
/// ③ 与加密无关的失败 → 原样带出底层错误。
///
/// 「这把私钥是不是加密的」不能只看错误变体，实测三条路径各不相同：
/// - OpenSSH / PKCS#5 **缺**口令 → `KeyIsEncrypted`（唯一一条直说的）；
/// - OpenSSH 口令**给错** → `SshKey(cryptographic error)`，看不出与加密有关；
/// - PKCS#8（`BEGIN ENCRYPTED PRIVATE KEY`）缺口令 → DER 解析失败，同样看不出。
///
/// 故补两条判据：① 给过口令而失败时，用 `None` 再探一次——`decode_openssh`/`decode_pkcs5`
/// 在 `is_encrypted` 检查处就返回 `KeyIsEncrypted`，不会跑 bcrypt KDF，代价可忽略；
/// ② PEM 头部特征，兜住 PKCS#8/PKCS#5 那两种从错误变体上看不出来的格式。
fn decode_private_key(
    pem: &str,
    passphrase: Option<&Zeroizing<String>>,
) -> Result<russh::keys::PrivateKey, Error> {
    // as_str() 借出 &str：口令不再克隆一份（克隆体用毕不保证擦除）
    match russh::keys::decode_secret_key(pem, passphrase.map(|p| p.as_str())) {
        Ok(key) => Ok(key),
        Err(e) => {
            let encrypted = matches!(e, russh::keys::Error::KeyIsEncrypted)
                || (passphrase.is_some()
                    && matches!(
                        russh::keys::decode_secret_key(pem, None),
                        Err(russh::keys::Error::KeyIsEncrypted)
                    ))
                || pem.contains("ENCRYPTED PRIVATE KEY")
                || pem.contains("DEK-Info:");
            Err(Error::Ssh(match (encrypted, passphrase.is_some()) {
                (true, false) => "私钥已加密，需要口令（passphrase）才能解码，但本连接未绑定口令：\
                     请在连接设置中为该私钥补一条口令记录（AuthRef.passphrase_vault_record）后重试"
                    .to_string(),
                (true, true) => {
                    "私钥已加密，且所给口令无法解开它：口令有误，或私钥文件本身已损坏".to_string()
                }
                (false, _) => format!("私钥解码失败：{e}"),
            }))
        }
    }
}

/// 一段文本是否带有私钥文件的特征串；返回命中的那一条，用于错误文案。
///
/// **只用于「声明是口令却像私钥」这一道 fail-closed 的门**（见 `build_credentials`），
/// 绝不用来推断用途——推断正是审计2 #20 修掉的那件事。
///
/// 四条特征各有出处，缺一条就漏一类真实文件：
/// - `PRIVATE KEY`：PEM 系（`BEGIN RSA/EC/DSA/OPENSSH/ENCRYPTED PRIVATE KEY`），
///   以及 SSH.com/Tectia 那种四横线加空格的变体（`---- BEGIN SSH2 ENCRYPTED PRIVATE KEY ----`）。
/// - `PuTTY-User-Key-File-`：**旧实现漏的正是这一类**。`.ppk` 全文没有 `PRIVATE KEY` 这个词，
///   于是一份 PuTTY 私钥会被原样当口令发出去。Windows 用户手里的私钥多半就是这个格式。
/// - `openssh-key-v1`：OpenSSH v1 私钥容器的魔数明文形态（只贴了 base64 正文、丢了 PEM 头
///   的粘贴事故并不罕见，解出来的头部就是它）。
/// - `b3BlbnNzaC1rZXktdjEA`：上一条的 base64 形态，接住「只贴了正文」的那一半情形。
///
/// 判据是**子串**而非前缀：粘贴时带上前导空行、BOM、注释行都很常见，卡前缀等于给误配留门。
fn private_key_marker(text: &str) -> Option<&'static str> {
    const MARKERS: [&str; 4] = [
        "PRIVATE KEY",
        "PuTTY-User-Key-File-",
        "openssh-key-v1",
        "b3BlbnNzaC1rZXktdjEA",
    ];
    MARKERS.into_iter().find(|m| text.contains(m))
}

/// 返回 (凭据集, 保险库是否锁着)。`locked = true` 表示档案绑了 vault 记录、
/// 但来源锁着取不到，凭据集里**没有**那份材料——调用方据此走弹框问口令的
/// 降级路径（2026-08-31，与 RDP 同口径），而不是把连接判死。
fn build_credentials(
    profile: &Profile,
    secrets: &dyn SecretSource,
) -> Result<(CredentialSet, Option<String>), Error> {
    let mut b = CredentialSet::builder();
    if let Some(rec) = profile.auth.vault_record {
        // 取不到库里那份材料 → **一律降级**，绝不把连接判死（2026-08-31 第二修）。
        //
        // 上一版只对「锁着」降级，其余（记录被删、类型不符、非法 UTF-8）仍旧
        // `return Err(e)`。那条界线画错了，而且错得很贵：`authenticate` 的第一行就是
        // `build_credentials(...)?`，一旦硬失败，**整个认证循环一次都不执行**——
        // 循环里那条「服务器通告 password 且本地无口令 → 问一次」的弹框路径
        // 因此永远到不了。用户真机现场（日志在案）正是这个：
        //   auth failed; tried: []; server allows: ["publickey", "password"]
        // 服务器明明收口令、弹框条件字面上全部成立，人却只收到一句
        // 「去会话属性配凭据」的长文案，当场什么也做不了。
        //
        // 「让用户知道配置坏了」这个目的**不需要靠判死达成**：降级之后原因进
        // `Error::Auth.notes`（连接真失败时看得到），而连接若靠弹框问到的口令
        // 连上了——那正是用户此刻要的，一条过期的 vault 引用不该拦住他。
        //
        // 返回的第二项从 bool 变成 Option<String>：Some(原因) = 降级过，
        // 原因原样进 notes。bool 只能说「锁着」，说不出「记录 7 已不存在」。
        let s = match secrets.secret(rec) {
            Ok(s) => s,
            Err(Error::SecretSourceLocked) => {
                return Ok((
                    b.build(),
                    Some(
                        "保险库未解锁：档案绑定的凭据本次未使用。解锁保险库后重连，                         或用本次弹框输入的口令登录"
                            .to_string(),
                    ),
                ));
            }
            Err(e) => {
                // 记录被删/类型不符/读不出：同样降级，原因如实带走。
                return Ok((
                    b.build(),
                    Some(format!(
                        "档案绑定的凭据记录取不到（{e}）——它可能已被删除或类别不符；                         本次改用弹框输入的口令，可在连接属性里重新绑定"
                    )),
                ));
            }
        };
        // S35：严格 UTF-8 校验，不用 from_utf8_lossy。lossy 有两个独立问题：
        // ① 字节非法时它走 Cow::Owned 分支，先造出一个**不受 Zeroizing 保护**的裸 String，
        //    明文随普通 drop 留在堆上（总设计 §3.2：秘密不得以裸 String 存活）；
        // ② 非法字节被替换成 U+FFFD，得到一份**静默损坏**的口令/PEM，拿去认证只会神秘失败，
        //    排查成本远高于当场报错。vault 里存的是任意字节 blob（二进制 DER 私钥、含非 UTF-8
        //    字节的口令都可能），这条路并非不可达。
        // 合法路径上 `v.to_string()` 的结果立即包入 Zeroizing：秘密以 Zeroizing<String> 承载入
        // CredentialSet（跨 await 存活），随 drop 清零。
        // 写成 map_err+? 而非 match：该 match 的错误臂长度恰好卡在 rustfmt 的 max_width 边界上，
        // 块式与单行式互为「格式化后仍需再格式化」，`cargo fmt --check` 会在两态间反复横跳。
        let text = std::str::from_utf8(&s.bytes)
            .map(|v| Zeroizing::new(v.to_string()))
            .map_err(|e| {
                Error::Ssh(format!(
                    "vault 记录 {rec} 不是合法 UTF-8 文本（第 {} 字节起非法），无法作为口令或私钥使用",
                    e.valid_up_to()
                ))
            })?;
        // 审计2 #20：用途来自存储层声明的 kind，不再由明文内容猜（判据见 secrets::SecretKind）。
        match s.kind {
            secrets::SecretKind::PrivateKey => b = b.public_key(text),
            secrets::SecretKind::Password => {
                // 声明是口令、内容却像私钥 —— fail-closed。
                //
                // 这条不是复活「靠内容猜」，方向恰恰相反：kind 是唯一的判据，这里只在
                // **判据与内容公然矛盾**时拒绝动作，绝不擅自改判用途。之所以值得为一种
                // 情形单设一道门，是因为它的后果不可逆且不对称——口令被误当私钥，最坏是
                // 认证失败；私钥被误当口令，是把整份私钥材料**发给对端**。通道加密救不了：
                // 拿到它的正是连接的另一端。
                //
                // 这类误配不必有人使坏就会发生：vault 的类别是用户在录入时自己选的，
                // 把一份 `.ppk` 贴进「密码」框只是点错一个下拉菜单。
                if let Some(marker) = private_key_marker(&text) {
                    return Err(Error::Ssh(format!(
                        "vault 记录 {rec} 的类别是「{}」，但内容含私钥材料特征（{marker}），\
                         已拒绝把它作为口令发送给服务器。若这确实是一把私钥，请把该记录的类别\
                         改成「{}」；若它确实是口令，请换一个不含该特征串的口令",
                        secrets::SecretKind::Password.label(),
                        secrets::SecretKind::PrivateKey.label()
                    )));
                }
                b = b.password(text)
            }
            secrets::SecretKind::ApiKey => {
                return Err(Error::Ssh(format!(
                    "vault 记录 {rec} 的类别是「{}」，SSH 认证不使用这类记录：\
                     把它当口令发出去等于把一份与本次登录无关的凭据交给这台服务器。\
                     请在连接设置中改选一条「{}」或「{}」记录",
                    secrets::SecretKind::ApiKey.label(),
                    secrets::SecretKind::Password.label(),
                    secrets::SecretKind::PrivateKey.label()
                )));
            }
        }
    }
    if profile.auth.allow_kbd_interactive {
        b = b.kbd_interactive();
    }
    if profile.auth.allow_agent {
        b = b.agent();
    }
    Ok((b.build(), None))
}

/// 服务器**只收 publickey** 时，不得把用户往「去存口令」引（2026-08-31 真机报出）。
///
/// 用户现场：`server allows: ["publickey"]`，而提示第一句是「请存入口令」。
/// 他照做——存了口令、回来重连、照样失败，因为那台机器 `PasswordAuthentication no`。
/// 一句把人引向死路的提示比没有提示更糟：它消耗的是用户的信任。
///
/// 文案只陈述**对端通告的事实**（「这台服务器只接受密钥认证」），
/// 不猜本机状态（不说「你机器上有 agent 没用」——客户端无从得知，
/// 这条纪律见 `suppressed_by_config` 的文档）。
#[test]
fn a_pubkey_only_server_is_not_told_to_save_a_password() {
    let empty = CredentialSet::builder().build();
    let note = auth::credentialless_note(&empty, &[], &[Method::PublicKey])
        .expect("零凭据零尝试必须给提示");
    assert!(
        note.contains("只接受密钥认证"),
        "必须点明服务器只收密钥（实得：{note}）"
    );
    assert!(
        note.contains("SSH Agent"),
        "要给出当场可行的第一条路：Agent 不需要先建保险库（实得：{note}）"
    );
    assert!(
        !note.contains("存入口令（存入 Vault…）"),
        "不得把用户引向「去存口令」——这台服务器不收口令，存了也连不上（实得：{note}）"
    );
}

/// 服务器**通告 password** 时仍用原文案：那条路是通的，不该被改掉。
#[test]
fn a_password_capable_server_keeps_the_original_advice() {
    let empty = CredentialSet::builder().build();
    let note = auth::credentialless_note(&empty, &[], &[Method::Password, Method::PublicKey])
        .expect("零凭据零尝试必须给提示");
    assert!(
        note.contains("存入口令"),
        "服务器收口令时，「去存口令」是有效建议（实得：{note}）"
    );
    assert!(!note.contains("只接受密钥认证"));
}

/// publickey + keyboard-interactive（堡垒机常见：密钥过了才出 OTP）**不算**
/// publickey-only：那条路上口令类的东西仍有用武之地，不该改文案。
#[test]
fn pubkey_plus_kbd_is_not_treated_as_pubkey_only() {
    let empty = CredentialSet::builder().build();
    let note = auth::credentialless_note(&empty, &[], &[Method::PublicKey, Method::KbdInteractive])
        .expect("零凭据零尝试必须给提示");
    assert!(!note.contains("只接受密钥认证"), "实得：{note}");
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::secrets::{Secret, SecretKind};

    struct FixedSecret(SecretKind, Vec<u8>);
    impl SecretSource for FixedSecret {
        fn secret(&self, _rec: u64) -> Result<Secret, Error> {
            Ok(Secret {
                kind: self.0,
                bytes: Zeroizing::new(self.1.clone()),
            })
        }
    }
    fn pw_secret(s: &str) -> FixedSecret {
        FixedSecret(SecretKind::Password, s.as_bytes().to_vec())
    }

    fn profile_with_vault_record() -> Profile {
        Profile {
            id: uuid::Uuid::nil(),
            name: "t".into(),
            group_path: None,
            host: "h".into(),
            port: 22,
            username: "u".into(),
            protocol: fs_connmgr::Protocol::Ssh,
            auth: fs_connmgr::AuthRef {
                vault_record: Some(7),
                ..Default::default()
            },
            jump: vec![],
            host_key_policy: Default::default(),
            host_key_pins: vec![],
            env: Default::default(),
            term: Default::default(),
            sftp: Default::default(),
            ai_policy: Default::default(),
            serial: Default::default(),
        }
    }

    /// 锁着的凭据来源必须**降级**而不是判死（2026-08-31，真机报出后修）。
    ///
    /// 此前 VaultSecrets 把「保险库未解锁」伪装成 Auth 终态，authenticate 开头
    /// 的 `build_credentials(...)?` 直接把它抛给用户——日志原样记录了那句
    /// `auth failed; tried: []; notes: ["vault 未解锁"]`，而引擎里**本来就有**
    /// 「没口令且服务器通告 password → 弹框问一次」的路径，只是永远走不到。
    /// 保险库现在默认就是关的，这条路径是从「配置了凭据的老用户」到「完全连不上
    /// 且只收到一句天书」的主路。
    #[test]
    fn locked_secret_source_degrades_instead_of_failing_the_connection() {
        struct Locked;
        impl SecretSource for Locked {
            fn secret(&self, _rec: u64) -> Result<Secret, Error> {
                Err(Error::SecretSourceLocked)
            }
        }
        let p = profile_with_vault_record();
        let (creds, reason) = build_credentials(&p, &Locked).unwrap();
        assert!(
            reason.is_some_and(|r| r.contains("保险库未解锁")),
            "必须把降级原因上报给调用方（弹框缘由与终态 notes 都用它）"
        );
        assert!(
            creds.password.is_none(),
            "锁着时不得填入任何凭据材料——认证循环的弹框路径靠 password.is_none() 接管"
        );
    }

    /// 记录取不到（被删/类型不符/读不出）**同样降级**，不判死（2026-08-31 第二修）。
    ///
    /// 上一版这条判据钉的是相反的行为（"仍然硬失败"），理由写着「静默跳过会让用户
    /// 以为记住没生效」。那条界线画错了，而且错得很贵：`authenticate` 第一行就是
    /// `build_credentials(...)?`，硬失败 = **认证循环一次都不跑**，循环里那条
    /// 「服务器通告 password → 问一次口令」的弹框路径永远到不了。用户真机现场
    /// （日志在案）正是这个：server allows 里明明有 password，人却只收到一句
    /// 「去会话属性配凭据」，当场无事可做。
    ///
    /// 「让用户知道配置坏了」不需要靠判死达成：原因随降级进 `Error::Auth.notes`
    /// （连接真失败时看得到），而连接若靠弹框问到的口令连上了——那正是用户此刻
    /// 要的，一条过期的 vault 引用不该拦住他。
    #[test]
    fn an_unreadable_secret_record_also_degrades_with_a_reason() {
        struct Missing;
        impl SecretSource for Missing {
            fn secret(&self, _rec: u64) -> Result<Secret, Error> {
                Err(Error::Auth {
                    tried: vec![],
                    remaining: vec![],
                    notes: vec!["vault record 7: 不存在".into()],
                })
            }
        }
        let p = profile_with_vault_record();
        let (creds, reason) = build_credentials(&p, &Missing)
            .expect("取不到记录不得把连接判死——弹框路径靠它才轮得到");
        let reason = reason.expect("降级必须带原因，否则用户不知道绑的凭据出了什么事");
        assert!(
            reason.contains("取不到"),
            "原因要点名是凭据记录取不到（实得：{reason}）"
        );
        assert!(creds.password.is_none());
    }

    /// S35：非 UTF-8 的 vault 字节必须当场报错，而不是被 lossy 替换成 U+FFFD 后
    /// 当作口令送上网络（静默损坏 + 一份未擦除的裸 String 明文）。
    #[test]
    fn non_utf8_secret_is_rejected_not_lossily_mangled() {
        // 0xFF 在任何 UTF-8 序列里都非法；前缀刻意放合法字节，确保 valid_up_to() 有意义
        let p = profile_with_vault_record();
        let err = build_credentials(
            &p,
            &FixedSecret(SecretKind::Password, vec![b'p', b'w', 0xFF, 0xFE]),
        )
        .expect_err("非 UTF-8 秘密必须被拒绝");
        let msg = err.to_string();
        assert!(
            msg.contains("不是合法 UTF-8") && msg.contains("第 2 字节"),
            "错误文案须点明是编码问题并给出位置，实得：{msg}"
        );
    }

    /// 合法 UTF-8 仍按原样承载，且口令/私钥分流不变
    #[test]
    fn utf8_secret_is_carried_through() {
        let p = profile_with_vault_record();
        let (creds, reason) = build_credentials(&p, &pw_secret("s3cr3t-口令")).unwrap();
        assert!(reason.is_none(), "正常取到凭据时不该有降级原因");
        assert_eq!(
            creds.password.as_deref().map(String::as_str),
            Some("s3cr3t-口令")
        );
        assert!(creds.public_key_pem.is_none());

        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nx\n-----END OPENSSH PRIVATE KEY-----\n";
        let (creds, _locked) = build_credentials(
            &p,
            &FixedSecret(SecretKind::PrivateKey, pem.as_bytes().to_vec()),
        )
        .unwrap();
        assert_eq!(
            creds.public_key_pem.as_deref().map(String::as_str),
            Some(pem)
        );
        assert!(creds.password.is_none());
    }

    /// 审计2 #20：用途由 vault 声明的 kind 决定，**不看内容**。
    ///
    /// 两个方向都钉住，因为旧实现 `text.contains("PRIVATE KEY")` 两个方向都会错：
    /// ① 一句含 `PRIVATE KEY` 字样的口令，声明是口令就该走口令位——旧实现会把它送去解 PEM，
    ///    认证以一句「私钥解码失败」神秘告终（这里改由 fail-closed 的门明确拒绝，理由见 ②）；
    /// ② 一份不含该词的私钥（PuTTY `.ppk` 就是），声明是私钥就该走私钥位——旧实现会把
    ///    **整份私钥材料当口令发给服务器**。
    #[test]
    fn secret_purpose_comes_from_declared_kind_not_from_content() {
        let p = profile_with_vault_record();

        // ② 不含 "PRIVATE KEY" 的真私钥：kind 说了算，进私钥位，不上网。
        let ppk = "PuTTY-User-Key-File-3: ssh-ed25519\nEncryption: none\n";
        let creds = build_credentials(&p, &FixedSecret(SecretKind::PrivateKey, ppk.into()))
            .unwrap()
            .0;
        assert_eq!(
            creds.public_key_pem.as_deref().map(String::as_str),
            Some(ppk),
            "PuTTY 私钥必须按声明进私钥位；旧实现按内容猜，会把它当口令发给服务器"
        );
        assert!(creds.password.is_none());
    }

    /// 声明是口令、内容却是私钥材料：**拒绝，绝不发送**（审计2 #20 的 fail-closed 门）。
    ///
    /// 这四种特征串各自对应一类真实文件，`.ppk` 与「只贴了 base64 正文」这两类都不含
    /// `PRIVATE KEY` 这个词——它们正是旧判据漏掉、也是最容易被误录成「密码」的那两类。
    #[test]
    fn private_key_material_is_never_sent_as_a_password() {
        let p = profile_with_vault_record();
        for material in [
            "-----BEGIN RSA PRIVATE KEY-----\nMII...\n-----END RSA PRIVATE KEY-----\n",
            "---- BEGIN SSH2 ENCRYPTED PRIVATE KEY ----\nx\n",
            "PuTTY-User-Key-File-3: ssh-ed25519\nEncryption: none\n",
            "\u{feff}\n# 粘贴时带了前导垃圾\nopenssh-key-v1\u{0}\n",
            "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAAB",
        ] {
            let err = build_credentials(&p, &pw_secret(material))
                .expect_err("私钥材料绝不能作为口令发给服务器，实得 Ok");
            let msg = err.to_string();
            assert!(
                msg.contains("私钥材料特征") && msg.contains("拒绝"),
                "错误须点明是私钥材料且已拒绝，实得：{msg}"
            );
            assert!(
                !msg.contains("MII") && !msg.contains("b3BlbnNzaC1rZXktdjEAAAAABG5vbmU"),
                "错误文案不得回显秘密材料本身，实得：{msg}"
            );
        }

        // 反向：普通口令不得被这道门误伤（否则它会变成一个随机拒绝登录的怪毛病）
        for ok in [
            "hunter2",
            "私钥在别处-not-a-key",
            "PRIVATEKEY",
            "b3BlbnNzaA",
        ] {
            build_credentials(&p, &pw_secret(ok))
                .unwrap_or_else(|e| panic!("普通口令 {ok:?} 被误判为私钥材料：{e}"));
        }
    }

    /// API Key 记录被指给 SSH 认证：硬错，不静默当口令送出去。
    ///
    /// 这条路径今天可达：vault 里可以存 ApiKey 记录，而 `AuthRef.vault_record` 只是个 u64，
    /// 指向哪条记录没有任何类型约束。旧实现会把它当口令发给服务器——那是把一份与本次登录
    /// 无关的凭据交付给对端。
    #[test]
    fn api_key_record_is_rejected_for_ssh_auth() {
        let p = profile_with_vault_record();
        let err = build_credentials(
            &p,
            &FixedSecret(SecretKind::ApiKey, b"sk-live-xxx".to_vec()),
        )
        .expect_err("ApiKey 不能用于 SSH 认证");
        let msg = err.to_string();
        assert!(
            msg.contains("API Key") && !msg.contains("sk-live-xxx"),
            "错误须点明类别且不得回显秘密，实得：{msg}"
        );
    }

    /// russh 自带的测试用加密私钥（OpenSSH v1 格式，aes256-cbc + bcrypt），口令为 `blabla`。
    /// 用现成的公开测试向量而不是运行时生成：生成加密私钥需要 KDF 轮数，单测里跑一次
    /// bcrypt 就是几十毫秒的无谓开销，且引入随机性会让失败不可复现。
    const ENCRYPTED_ED25519_PEM: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jYmMAAAAGYmNyeXB0AAAAGAAAABDLGyfA39
J2FcJygtYqi5ISAAAAEAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIN+Wjn4+4Fcvl2Jl
KpggT+wCRxpSvtqqpVrQrKN1/A22AAAAkOHDLnYZvYS6H9Q3S3Nk4ri3R2jAZlQlBbUos5
FkHpYgNw65KCWCTXtP7ye2czMC3zjn2r98pJLobsLYQgRiHIv/CUdAdsqbvMPECB+wl/UQ
e+JpiSq66Z6GIt0801skPh20jxOO3F52SoX1IeO5D5PXfZrfSZlw6S8c7bwyp2FHxDewRx
7/wNsnDM0T7nLv/Q==
-----END OPENSSH PRIVATE KEY-----";

    /// P1-9：跳板每一跳只用**自己**的指纹钉，绝不继承目标主机的（pin 绑定主机身份，
    /// 跨主机复用是类型混淆）。旧实现 `parent.host_key_pins.clone()` 会让第一条断言直接变红。
    ///
    /// **审计 P1 修订**：本用例原先还断言「父 Profile = FingerprintPinned 时跳板回落 Tofu」——
    /// 那一条钉住的其实是缺陷本身。policy 不是主机身份断言，而是与主机无关的**严格度意图**，
    /// 它随 pins 一起被丢弃，正是「目标机配了 Strict，堡垒机却弹一个可点接受的 TOFU 框」
    /// 这个 P1 的成因。现在 pins 仍不继承（安全性由此保住），policy 则按
    /// 「逐跳显式配置 > 继承父 Profile」生效。
    #[test]
    fn hop_never_inherits_target_host_pins() {
        let mut parent = profile_with_vault_record();
        parent.host_key_policy = fs_connmgr::HostKeyPolicy::FingerprintPinned;
        parent.host_key_pins = vec![fs_connmgr::HostKeyPin {
            key_blob: "TARGET-KEY-BLOB".into(),
            fingerprint_sha256: "SHA256:target".into(),
        }];
        let bare = || fs_connmgr::JumpHop {
            host: "bastion".into(),
            port: 2222,
            username: "j".into(),
            ..Default::default()
        };

        // ① 父 = 钉扎、跳板未配置 → **空钉**（核心安全断言）+ 策略降级为 Strict。
        //    降级而非照搬钉扎：钉扎白名单无法跨主机继承，空白名单按新的 `decide` 语义恒硬拒，
        //    照搬会让所有配了钉扎的 profile 跳板链一律连不上（用户又无处添加 hop 钉）。
        //    Strict 是继承来的意图里仍然可满足的最强一档：必须已在信任库中，不许点穿。
        let hp = hop_profile(&bare(), parent.host_key_policy);
        assert!(
            hp.host_key_pins.is_empty(),
            "跳板绝不得继承目标主机的指纹钉（pin 绑定主机身份，跨主机复用是类型混淆）"
        );
        assert_eq!(
            hp.host_key_policy,
            fs_connmgr::HostKeyPolicy::Strict,
            "父为钉扎而本跳无钉 → 降级 Strict，绝不得回落 Tofu（那是可点穿的）"
        );
        assert_eq!((hp.host.as_str(), hp.port), ("bastion", 2222));

        // ② 严格度意图必须覆盖整条链：一条链的安全性取下限，而堡垒机是链上**第一个**
        //    拿到你流量的节点。这一条正是审计 P1 的直接回归。
        assert_eq!(
            hop_profile(&bare(), fs_connmgr::HostKeyPolicy::Strict).host_key_policy,
            fs_connmgr::HostKeyPolicy::Strict,
            "父为 Strict 时跳板不得静默回落 Tofu——否则目标机的严格设定对堡垒机形同虚设"
        );
        // ③ 父为 Tofu → 跳板 Tofu（不无中生有地收紧，免得既有跳板链一升级就全连不上）
        assert_eq!(
            hop_profile(&bare(), fs_connmgr::HostKeyPolicy::Tofu).host_key_policy,
            fs_connmgr::HostKeyPolicy::Tofu
        );

        // ④ 跳板自配策略/钉 → 原样生效，且仍与父 Profile 的钉无关
        let pinned = fs_connmgr::JumpHop {
            host_key_policy: Some(fs_connmgr::HostKeyPolicy::Strict),
            host_key_pins: vec![fs_connmgr::HostKeyPin {
                key_blob: "BASTION-KEY-BLOB".into(),
                fingerprint_sha256: "SHA256:bastion".into(),
            }],
            ..bare()
        };
        let hp = hop_profile(&pinned, parent.host_key_policy);
        assert_eq!(hp.host_key_policy, fs_connmgr::HostKeyPolicy::Strict);
        assert_eq!(
            hp.host_key_pins
                .iter()
                .map(|p| p.key_blob.as_str())
                .collect::<Vec<_>>(),
            vec!["BASTION-KEY-BLOB"],
            "跳板只认自己的钉"
        );

        // ⑤ 本跳自带钉 + 父为钉扎 → 不降级：白名单可满足，钉扎语义原样保留
        let own_pins = fs_connmgr::JumpHop {
            host_key_pins: vec![fs_connmgr::HostKeyPin {
                key_blob: "BASTION-KEY-BLOB".into(),
                fingerprint_sha256: "SHA256:bastion".into(),
            }],
            ..bare()
        };
        assert_eq!(
            hop_profile(&own_pins, parent.host_key_policy).host_key_policy,
            fs_connmgr::HostKeyPolicy::FingerprintPinned,
            "本跳有自己的钉时白名单可满足，无需降级"
        );

        // ⑥ 逐跳显式配置优先于继承——包括显式放宽。否则 `JumpHop::host_key_policy`
        //    就成了「写了也可能不生效」的字段，正是本轮在猎杀的那类「只写不读」。
        let relaxed = fs_connmgr::JumpHop {
            host_key_policy: Some(fs_connmgr::HostKeyPolicy::Tofu),
            ..bare()
        };
        assert_eq!(
            hop_profile(&relaxed, fs_connmgr::HostKeyPolicy::Strict).host_key_policy,
            fs_connmgr::HostKeyPolicy::Tofu,
            "本跳显式配置是最具体的意图，须压过继承值"
        );
    }

    /// P1-9 的延伸：`JumpHop` 的两个新字段必须能从**缺这两个键**的旧 JSON 反序列化出来。
    /// 少了 `#[serde(default)]`，已落库的 `jump_blob` 会在 `Row::into_profile` 处整行报
    /// `CorruptRow`——用户的连接会从侧栏里凭空消失（repo.rs 的 list() 对坏行是 warn 后跳过）。
    #[test]
    fn legacy_jump_hop_json_without_new_fields_still_deserializes() {
        let legacy = r#"{"host":"b","port":22,"username":"u","auth":{"vault_record":null}}"#;
        let h: fs_connmgr::JumpHop =
            serde_json::from_str(legacy).expect("旧 jump_blob 必须仍可解析");
        assert_eq!(h.host_key_policy, None);
        assert!(h.host_key_pins.is_empty());
        assert_eq!(h.auth.passphrase_vault_record, None);
    }

    /// P1-23：加密私钥缺口令时，错误必须**点名缺的是口令**，而不是含糊的解析失败。
    #[test]
    fn encrypted_key_without_passphrase_says_so() {
        let err =
            decode_private_key(ENCRYPTED_ED25519_PEM, None).expect_err("加密私钥无口令必然解不开");
        let msg = err.to_string();
        assert!(
            msg.contains("已加密") && msg.contains("口令"),
            "错误须点明「需要口令」这一可自助修复的成因，实得：{msg}"
        );
    }

    /// P1-23：口令正确即解得开（历史实现硬编码 None，这条恒红）。
    #[test]
    fn encrypted_key_decodes_with_correct_passphrase() {
        let pass = Zeroizing::new("blabla".to_string());
        decode_private_key(ENCRYPTED_ED25519_PEM, Some(&pass)).expect("正确口令必须能解开加密私钥");
    }

    /// P1-23：口令错误与「没给口令」是两回事，文案必须能分辨；且**绝不得回显口令本身**。
    #[test]
    fn wrong_passphrase_is_distinguished_and_never_echoed() {
        let pass = Zeroizing::new("wrong-secret-123".to_string());
        let err =
            decode_private_key(ENCRYPTED_ED25519_PEM, Some(&pass)).expect_err("错误口令必然解不开");
        let msg = err.to_string();
        assert!(
            msg.contains("口令有误"),
            "口令错误须与「未绑定口令」区分开，实得：{msg}"
        );
        assert!(
            !msg.contains("wrong-secret-123"),
            "错误文案里出现了口令明文——秘密恒不得进入任何可被日志/前端消费的出口"
        );
    }

    /// P1-23：口令从 vault 按需取出，且只在配置了记录时才取。
    #[test]
    fn passphrase_is_loaded_only_when_configured() {
        let mut p = profile_with_vault_record();
        assert!(
            load_passphrase(&p, &pw_secret("blabla")).unwrap().is_none(),
            "未配置 passphrase_vault_record 时不得访问 vault（明文驻留窗口应为零）"
        );

        p.auth.passphrase_vault_record = Some(9);
        let got = load_passphrase(&p, &pw_secret("blabla")).unwrap();
        assert_eq!(got.as_deref().map(String::as_str), Some("blabla"));

        // 非 UTF-8 口令走与 S35 同一条严格校验，不得 lossy 成一个「看着对、解不开」的口令
        let err = load_passphrase(&p, &FixedSecret(SecretKind::Password, vec![0xFF]))
            .expect_err("非 UTF-8 口令必须被拒");
        assert!(err.to_string().contains("不是合法 UTF-8"));

        // 审计2 #20：口令槽只收 Password 类。把私钥记录填进来是一类真实误配，
        // 旧实现的现象是「口令解不开私钥」，用户会去反复核对一个没填错的口令。
        for wrong in [SecretKind::PrivateKey, SecretKind::ApiKey] {
            let err = load_passphrase(&p, &FixedSecret(wrong, b"x".to_vec()))
                .expect_err("私钥口令槽只接受口令类记录");
            let msg = err.to_string();
            assert!(
                msg.contains(wrong.label()) && msg.contains("私钥口令"),
                "错误须点明实际类别与槽位用途，实得：{msg}"
            );
        }
    }

    /// P1-8：超时错误必须**点名阶段**。折叠成一个统一的 io error 时这条会红——
    /// 而那正是「用户只看到『操作超时』，五条排查路径全被抹平」的形态。
    #[tokio::test]
    async fn timeout_error_names_the_stage() {
        let err = with_timeout(
            "DNS 解析 + TCP 建连 example:22",
            std::time::Duration::from_millis(10),
            std::future::pending::<()>(),
        )
        .await
        .expect_err("永不完成的 future 必须被超时截断");
        let msg = err.to_string();
        assert!(
            msg.contains("DNS 解析 + TCP 建连 example:22") && msg.contains("超时"),
            "超时文案须点名阶段，实得：{msg}"
        );
        // 审计2 #11：建连阶段的超时必须与传输阶段的超时是**同一个**类型，否则
        // `is_timeout()` 只认得一半，调用方就得回去靠 `msg.contains("超时")` 分诊。
        assert!(err.is_timeout(), "建连超时没有被 is_timeout 认出来：{msg}");
    }

    /// S38：因本地原因「未发起」的方法不得改写候选集。
    ///
    /// 对照组是**同一个函数**在 `Failure { empty }` 下的行为——服务器真回了空通告时候选集
    /// 确实该清空。两条断言相差的恰好是「有没有真发出去过一次认证请求」这一件事，
    /// 因此把 `Unattempted` 臂改成走 `Failure` 语义会立刻让第一条断言变红。
    #[test]
    fn unattempted_method_keeps_remaining_intact() {
        let creds = CredentialSet::builder()
            .password(Zeroizing::new("pw".into()))
            .agent()
            .build();
        let prev = vec![Method::Password, Method::Agent];
        let kept = advance_remaining(
            &AuthStep::Unattempted {
                reason: "agent unavailable".into(),
            },
            prev.clone(),
            &creds,
        );
        assert_eq!(
            kept, prev,
            "本机 agent 不可用不得清空候选集，否则同 profile 的口令回退被连带杀死"
        );

        let wiped = advance_remaining(
            &AuthStep::Failure {
                remaining: russh::MethodSet::empty(),
            },
            prev,
            &creds,
        );
        assert!(
            wiped.is_empty(),
            "服务器通告为空时应如实清空，实得 {wiped:?}"
        );
    }

    /// 「没试且没料」必须给出可行动的「去配凭据」提示，与「有料但被开关压掉」严格区分——
    /// 两条的修法完全不同，混成一句「认证失败」会把用户引去开关页瞎找，而真正的缺口
    /// （根本忘了存口令）无人提示。
    #[test]
    fn credentialless_profile_gets_an_actionable_note() {
        // 空凭据 + 未尝试 → 提示去配凭据
        let empty = CredentialSet::builder().build();
        let note = auth::credentialless_note(&empty, &[], &[Method::Password]);
        assert!(note.is_some(), "零凭据零尝试必须给出提示");
        assert!(
            note.unwrap().contains("未配置任何可用的认证凭据"),
            "提示必须点明「未配置凭据」而非含糊的「认证失败」"
        );

        // 有口令 + 未尝试（可能是被 kbd 开关压掉）→ 不得说「没配凭据」
        let with_pw = CredentialSet::builder()
            .password(Zeroizing::new("pw".into()))
            .build();
        assert_eq!(
            auth::credentialless_note(&with_pw, &[], &[Method::Password]),
            None,
            "有口令却被说成「未配置凭据」——两种失败的修法被并成了一句"
        );

        // 试过但没过 → 不得说「没配凭据」
        assert_eq!(
            auth::credentialless_note(&empty, &[Method::Password], &[Method::Password]),
            None,
            "试过没过的失败不该被说成「未配置凭据」"
        );
    }

    /// S303：「持有的凭据类型与服务器要求的不匹配」必须出声。
    ///
    /// 真实现场（2026-08-19 用户报障）：配了口令、服务器只收 publickey，用户拿到的是
    /// `auth failed; tried: []; server allows: ["publickey"]; notes: []`——一次都没试、
    /// 一句解释都没有。这一格恰好掉在 `credentialless_note`（要求「一条材料都没有」）
    /// 与 `suppressed_by_config`（要求「服务器通告 kbd-interactive」）的缝隙里。
    #[test]
    fn password_only_profile_against_publickey_only_server_gets_a_note() {
        let with_pw = CredentialSet::builder()
            .password(Zeroizing::new("pw".into()))
            .build();

        // 主场景：口令 vs 只收 publickey 的服务器
        let note = auth::material_mismatch_note(&with_pw, &[], &[Method::PublicKey])
            .expect("配了口令、服务器只收 publickey：必须给出可行动提示（原为 notes: []）");
        assert!(
            note.contains("publickey") && note.contains("password"),
            "提示必须**同时点名**服务器要什么、本连接有什么——只说一半用户仍不知道差在哪：{note}"
        );
        assert!(
            note.contains("私钥") || note.contains("Agent"),
            "publickey 场景须给出具体修法（选私钥记录 / 启用 Agent）：{note}"
        );

        // 有交集 → 未发起另有其因，不得断言为「类型不匹配」（说错比不说更糟）
        assert_eq!(
            auth::material_mismatch_note(&with_pw, &[], &[Method::Password]),
            None,
            "服务器通告 password 且本连接有口令：有交集，不该被说成类型不匹配"
        );

        // 试过但没过 → 是「试了没过」，不是「配错类型」
        assert_eq!(
            auth::material_mismatch_note(&with_pw, &[Method::Password], &[Method::PublicKey]),
            None,
            "试过没过的失败不该被说成类型不匹配"
        );

        // 一条材料都没有 → 让位给 credentialless_note，两句不得并列
        let empty = CredentialSet::builder().build();
        assert_eq!(
            auth::material_mismatch_note(&empty, &[], &[Method::PublicKey]),
            None,
            "零凭据该由 credentialless_note 说「去配凭据」，本函数须让位"
        );

        // 被开关压掉 → 让位给 suppressed_by_config（那句点到了具体开关，更可行动）
        assert_eq!(
            auth::material_mismatch_note(&with_pw, &[], &[Method::KbdInteractive]),
            None,
            "kbd 开关压制场景该由 suppressed_by_config 说，两句并列会稀释可行动的那条"
        );

        // 服务器侧不得把本地合成的 Agent 报成「服务器要求 agent」（协议层无此方法）
        let note = auth::material_mismatch_note(&with_pw, &[], &[Method::PublicKey, Method::Agent])
            .expect("仍须出声");
        assert!(
            !note.contains("服务器只接受 publickey、agent"),
            "Method::Agent 是本地合成的（协议层即 publickey），不得报成服务器要求：{note}"
        );
    }

    // ── 审计 P2 的端到端证明：主机密钥算法重排真的走上了线 ────────────────────────
    //
    // `hostkey` 里的单元测试只能证明「排序函数排得对」。而这个修复真正的主张是
    // **「服务器因此改选了另一把密钥」**——中间隔着 `client::Config.preferred`、russh 的
    // 协商实现、以及服务端的选键逻辑，没有一段是那些单元测试碰得到的。
    //
    // 所以这里起一个**进程内**的 russh 服务端：`tokio::io::duplex` 给一对内存双工流，
    // 一端交给 `server::run_stream`，另一端作为 `transport` 喂给 `connect_inner`。
    // 不占端口、不碰网络、不受防火墙影响，握手却是真的。
    //
    // 服务端同时提供 ed25519 与 ecdsa-p256 两把主机密钥——这正是审计里那台「多算法主机」。
    // russh 的默认偏好里 ed25519 排第一，所以：库里存着 ecdsa 那把时，若不重排，协商必然
    // 回到 ed25519 → blob 对不上 → 假的「密钥已变更」。

    use crate::events::{HostKeyChoice, Prompt};
    use russh::keys::{EcdsaCurve, PrivateKey, PublicKeyBase64};

    /// 把 `host_key_decision` 的每一次触发原样记下来。**不作应答**（一律 Refuse），
    /// 因为本组测试要断言的就是「这个框到底弹没弹、弹的时候看见的是哪一族密钥」。
    #[derive(Default)]
    struct PromptRecorder {
        seen: std::sync::Mutex<Vec<(String, String)>>,
    }
    impl SessionEvents for PromptRecorder {
        fn host_key_decision(
            &self,
            _host: &str,
            _port: u16,
            presented: &PresentedKey,
            hint: &Decision,
        ) -> HostKeyChoice {
            let kind = match hint {
                Decision::Accept => "accept",
                Decision::AskTofu => "tofu",
                Decision::Changed { .. } => "changed",
                Decision::Refuse { .. } => "refuse",
            };
            self.seen
                .lock()
                .unwrap()
                .push((kind.into(), presented.key_type.clone()));
            HostKeyChoice::Refuse
        }
        fn kbd_interactive(&self, _n: &str, _i: &str, _p: &[Prompt]) -> Vec<String> {
            vec![]
        }
        fn password_prompt(&self) -> String {
            // 记一笔：认证编排有没有真的问过口令（2026-08-31 用户报出「不弹框」后加）。
            // 返回空串 = 用户取消，认证照旧失败——本组测试断言的是**问没问**，
            // 不是连没连上（进程内夹具的服务端一律拒认证）。
            self.seen
                .lock()
                .unwrap()
                .push(("password_prompt".to_string(), String::new()));
            String::new()
        }
        fn status(&self, _msg: &str) {}
    }

    struct NoSecret;
    impl SecretSource for NoSecret {
        fn secret(&self, _rec: u64) -> Result<Secret, Error> {
            Err(Error::Connect("测试不提供任何凭据".into()))
        }
    }

    /// 最简服务端：只负责完成 KEX 并出示主机密钥。认证一律拒绝——本组测试断言的一切
    /// 都发生在认证**之前**（`check_server_key` 在 KEX 阶段），认证成不成功无关紧要。
    struct SilentServer;
    impl russh::server::Handler for SilentServer {
        type Error = russh::Error;
    }

    /// 信任库在握手前该处于什么状态。
    enum Stored {
        /// 空库：首连。
        Nothing,
        /// 存着**服务端此刻真正持有**的那把 ecdsa 密钥——等价于「导入 known_hosts 后只留下 ecdsa 行」。
        ServersEcdsaKey,
        /// 算法族对得上、blob 对不上：用来把「偏好是否上了线」与「blob 是否匹配」分开观察。
        EcdsaAlgorithmButAnotherKey,
    }

    /// 起一个进程内 russh 服务端、跑一次真握手，返回本次连接中 `host_key_decision` 的触发记录。
    ///
    /// 三个用例共用**同一份**实现。曾经写成两份几乎相同的拷贝，随即意识到那正是本仓库
    /// 反复吃过亏的形态（见 `handshake` 上方关于 S17 的注）：两份拷贝里只要有一份悄悄
    /// 少做了一步，它对应的那条断言就会变成「因为没连上所以没弹框」的假绿。
    async fn handshake_prompts(stored: Stored) -> Vec<(String, String)> {
        let ed = PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
        let ec = PrivateKey::random(
            &mut rand::rng(),
            russh::keys::Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            },
        )
        .unwrap();
        // 公钥要在私钥被移进 server::Config 之前取出来——这是「库里那把 == 服务端那把」
        // 唯一诚实的取得方式（另生成一把再声称它相同，测的就只是我们自己的假设了）。
        let ec_pub = ec.public_key().clone();

        // 服务端两把都拿着；用哪一把由**客户端偏好序**决定（russh `server::Config.keys`
        // 的语义即「按客户端偏好挑第一把能用的」）——正是本修复要影响的那个决策点。
        let server_config = russh::server::Config {
            keys: vec![ed, ec],
            ..Default::default()
        };

        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            // 我们必然拒绝（主机密钥 Refuse 或认证无凭据），这里返回 Err 属预期。
            let _ = russh::server::run_stream(
                std::sync::Arc::new(server_config),
                server_side,
                SilentServer,
            )
            .await;
        });

        let dir = tempfile::tempdir().unwrap();
        let db = fs_connmgr::Db::open(&dir.path().join("fs.db"))
            .await
            .unwrap();
        let pool = db.pool().clone();

        let row = match stored {
            Stored::Nothing => None,
            Stored::ServersEcdsaKey => Some(ec_pub.public_key_base64()),
            Stored::EcdsaAlgorithmButAnotherKey => {
                let other = PrivateKey::random(
                    &mut rand::rng(),
                    russh::keys::Algorithm::Ecdsa {
                        curve: EcdsaCurve::NistP256,
                    },
                )
                .unwrap();
                Some(other.public_key().public_key_base64())
            }
        };
        if let Some(key_blob) = row {
            let blob = base64::engine::general_purpose::STANDARD
                .decode(&key_blob)
                .unwrap();
            TrustStore::new(&pool)
                .record(
                    "h",
                    22,
                    &hostkey::StoredKey {
                        key_type: ec_pub.algorithm().as_str().to_string(),
                        key_blob,
                        fingerprint_sha256: hostkey::fingerprint_sha256(&blob),
                        source: "imported".into(),
                    },
                    "imported",
                )
                .await
                .unwrap();
        }

        let events = Arc::new(PromptRecorder::default());
        let profile = profile_with_vault_record();
        // 连接必以失败告终（密钥被 Refuse，或认证无凭据）。被测的是过程，不是结果。
        let _ = connect_inner(
            &profile,
            &NoSecret,
            &pool,
            events.clone(),
            Some(Box::new(client_side)),
        )
        .await;

        let seen = events.seen.lock().unwrap().clone();
        seen
    }

    /// 对照组，兼观测手段：信任库为空时 TOFU 必然弹一次框，而框里那把密钥的算法
    /// **就是本次协商实际选中的那一族**。
    ///
    /// 这一条同时钉死两件事：其一，这套进程内握手确实跑通了 KEX——否则一次都不会弹，
    /// 下一条「一个框都不弹」的断言就会因为「根本没连上」而假绿；其二，不做任何重排时，
    /// 多算法主机协商到的是 ed25519 ——审计里那次假告警的成因，就此成为可复现的事实。
    #[tokio::test]
    async fn without_a_stored_key_the_handshake_lands_on_ed25519() {
        let seen = handshake_prompts(Stored::Nothing).await;
        assert_eq!(
            seen,
            vec![("tofu".to_string(), "ssh-ed25519".to_string())],
            "空信任库应恰好弹一次 TOFU 框，且协商到 russh 默认首选的 ed25519"
        );
    }

    /// 库里存着服务端那把 ecdsa（等价于「导入 known_hosts 后只留下 ecdsa 行」）时，
    /// 握手必须协商回 **ecdsa**：blob 因此对得上，`decide` 直接 Accept，**一个框都不弹**。
    ///
    /// 这就是审计 P2 的原始症状——修复前此处会弹一个红色「主机密钥已变更，可能存在中间人
    /// 攻击」，而那台主机什么都没换过。断言写成「一次都没弹」而非「没弹 changed」：在这条
    /// 路径上任何弹框都是缺陷，tofu 框同样意味着协商跑到了另一把密钥上。
    #[tokio::test]
    async fn a_stored_ecdsa_key_is_negotiated_back_instead_of_ed25519() {
        let seen = handshake_prompts(Stored::ServersEcdsaKey).await;
        // 只看**主机密钥**这一类事件：2026-08-31 起 PromptRecorder 也记口令询问
        // （零凭据档案必然被问一次口令），那与本条要钉的「主机密钥不该弹框」无关。
        let hostkey_prompts: Vec<_> = seen
            .iter()
            .filter(|(kind, _)| kind == "tofu" || kind == "changed")
            .collect();
        assert!(
            hostkey_prompts.is_empty(),
            "库里已有该主机的 ecdsa 密钥时不应弹任何主机密钥框，实得 {seen:?}"
        );
    }

    /// 零凭据 + 服务器通告 password → **必须问一次口令**（2026-08-31 真机报出）。
    ///
    /// 现场日志：`tried: []; server allows: ["publickey", "password"];
    /// notes: ["本连接未配置任何可用的认证凭据…"]`——弹框条件（remaining 含
    /// Password、creds.password 为空、本会话没问过）字面上全部成立，用户却只
    /// 收到一句「去设置页配凭据」的长文案，没有任何当场可做的事。
    ///
    /// 这条坏在**编排**而不在判定：`next_method` 与 `credentialless_note` 各自
    /// 都对，纯函数测试全绿——只有把整条 authenticate 跑起来才照得出。故本判据
    /// 用仓内既有的进程内 russh 服务端夹具（同 TOFU 那三条），断言
    /// `SessionEvents::password_prompt` 真的被调到。
    ///
    /// 夹具的服务端是 russh 默认 Handler：它对 `auth none` 返回 Failure 并通告
    /// 默认方法集（含 password），正是用户那台服务器的形状。
    #[tokio::test]
    async fn a_credentialless_profile_gets_asked_for_a_password() {
        let seen = handshake_prompts(Stored::ServersEcdsaKey).await;
        assert!(
            seen.iter().any(|(kind, _)| kind == "password_prompt"),
            "服务器通告 password、本地一份凭据都没有时必须问一次口令，\
             实际发生的事件序列：{seen:?}（空 = 一次都没问，用户只会收到\
             一句「去设置页配凭据」的长文案）"
        );
    }

    /// 库里存的是 ecdsa **算法**但 blob 是另一把密钥时，协商仍必须落到 ecdsa 上。
    ///
    /// 这一条把「偏好确实上了线」与「blob 恰好对上了」彻底分开：上一条的「不弹框」原则上
    /// 也可能来自别的什么让判定提前放行。这里 blob 故意对不上，框一定会弹——于是框里那个
    /// 算法名就是协商结果的直接读数。修复前它是 `ssh-ed25519`，修复后是 `ecdsa-sha2-nistp256`。
    #[tokio::test]
    async fn the_stored_algorithm_reaches_the_wire_even_when_the_blob_differs() {
        let seen = handshake_prompts(Stored::EcdsaAlgorithmButAnotherKey).await;
        assert_eq!(
            seen,
            vec![("changed".to_string(), "ecdsa-sha2-nistp256".to_string())],
            "存了 ecdsa 就该协商到 ecdsa；若这里读到 ssh-ed25519，说明偏好重排根本没生效"
        );
    }

    // ───────────── 审计2 #36：kbd 单提示自动应答 ─────────────

    fn p(text: &str, echo: bool) -> Prompt {
        Prompt {
            text: text.into(),
            echo,
        }
    }

    /// 全条件满足 → 返回口令本身作为答案（开关开 ∧ 恰一个 ∧ 不回显 ∧ 有口令）。
    #[test]
    fn auto_answer_fires_on_single_hidden_prompt() {
        assert_eq!(
            crate::auth::kbd_auto_answer(true, &[p("Password: ", false)], Some("pw")),
            Some("pw".to_string())
        );
    }

    /// 开关关 → 照旧弹框（None）。这是默认值路径：用户没开就是没开。
    #[test]
    fn auto_answer_off_by_flag() {
        assert_eq!(
            crate::auth::kbd_auto_answer(false, &[p("Password: ", false)], Some("pw")),
            None
        );
    }

    /// 多提示 → 拒绝自动应答：服务器在问多件事，客户端不知道哪一件吃口令（审计原文
    /// 「凭据用途靠猜明文」的批评面）。
    #[test]
    fn auto_answer_refuses_multiple_prompts() {
        assert_eq!(
            crate::auth::kbd_auto_answer(
                true,
                &[p("Password: ", false), p("OTP: ", false)],
                Some("pw")
            ),
            None
        );
    }

    /// echo=true 的提示是**新秘密录入**（改密/注册/OATH 绑定），自动填旧口令是帮倒忙。
    #[test]
    fn auto_answer_refuses_echo_prompts() {
        assert_eq!(
            crate::auth::kbd_auto_answer(true, &[p("New password: ", true)], Some("pw")),
            None
        );
    }

    /// 没有口令就没有可应答的材料——即便服务器只问了一个不回显的提示。
    #[test]
    fn auto_answer_needs_a_password() {
        assert_eq!(
            crate::auth::kbd_auto_answer(true, &[p("Password: ", false)], None),
            None
        );
    }

    /// 空提示集（服务器未发任何 prompt 的 InfoRequest）同样拒绝。
    #[test]
    fn auto_answer_refuses_empty_prompt_set() {
        assert_eq!(crate::auth::kbd_auto_answer(true, &[], Some("pw")), None);
    }

    /// 源守卫：档案开关必须真的传进认证循环——修掉「开关只写不读」靠接线，
    /// 接线是单元测试照不到的（这里没有可驱动的真实 kbd 服务器），故以整行相等钉住
    /// 两处文本锚：调用点传档、应答点做判定。任一被删，本用例直接变红。
    #[test]
    fn kbd_auto_answer_single_is_wired_into_auth_loop() {
        // `file!()` 在 workspace 构建下是相对 workspace 根的路径，而测试进程 cwd 是包目录
        // ——两者拼不拢。用 MANIFEST_DIR + 固定相对路径，与 cwd 解耦。
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/connect.rs"),
        )
        .expect("源码必须可读");
        let line_eq = |needle: &str| src.lines().filter(|l| l.trim() == needle).count();
        assert_eq!(
            line_eq("profile.auth.kbd_auto_answer_single,"),
            1,
            "调用点必须把档案开关传给 kbd_interactive_loop"
        );
        assert_eq!(
            line_eq("let answers: Vec<String> = match crate::auth::kbd_auto_answer("),
            1,
            "应答点必须经 kbd_auto_answer 判定（直接弹框即变异）"
        );
    }
}
