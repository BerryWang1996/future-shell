//! 主程序 ↔ RDP helper 进程之间的消息协议。
//!
//! # 为什么会有一个 helper 进程
//!
//! IronRDP（经 `picky`）钉死 `curve25519-dalek =5.0.0-rc.1`，而 russh 0.62.4 要 `^5`
//! （解到 5.0.0）。两者在 Cargo 眼里同属 `5.x` 兼容区间，必须统一成一个版本，
//! 而谁也不接受对方——**这两棵依赖树不能出现在同一个进程里**。
//! （picky 的 rc.26 已经修掉，但 IronRDP 连 master 都还钉着 rc.25。）
//!
//! 于是 RDP 引擎独立成 `rdp-helper/` 一个**单独的工作区**（自己的 Cargo.lock），
//! 经 stdio 与主程序对话。本 crate 是两个工作区**唯一**共享的东西，
//! 因此它谁也不碰：不碰 russh，也不碰 ironrdp。往这里加任何会牵连到那两棵树的
//! 依赖，都会把那个冲突重新引回来。
//!
//! # helper 没有任何网络出口
//!
//! **网络在主程序那一侧。** helper 除了 stdin/stdout 没有别的 I/O：
//! RDP 的线上字节也走这条管道（[`Message::NetIn`] / [`Message::NetOut`]）。
//!
//! 这不是绕路，是这个架构的主要价值：
//!
//! - helper 是解析**远端不可信字节**的那一侧（RDP 是一个很大的解析面）。
//!   它连不到任何地方，也拿不到 Vault——出了内存安全问题，波及面止步于此。
//! - 「经 SSH 跳板连 RDP」因此是免费的：主程序那侧已有 `Box<dyn AsyncStream>`
//!   （直连 TCP 或 direct-tcpip 通道二选一），拿到什么就往管道里灌什么，
//!   helper 根本不知道自己在跟谁说话。
//! - 跨进程传 socket 句柄要三平台各写一套（Unix `SCM_RIGHTS`、
//!   Windows `WSADuplicateSocket`）。复用同一条管道，零平台特定代码。
//!
//! # 信任裁决不在 helper
//!
//! helper 只**上报**服务器证书（[`Message::CertPresented`]）然后等一个裁决，
//! TOFU 四态的判定与落库在主程序——它才有数据库和 UI。
//! 被沙箱化的那一侧不做信任决定。
//!
//! # 线格式
//!
//! ```text
//! [u32 BE header_len][u32 BE body_len][header: JSON][body: 裸字节]
//! ```
//!
//! 头走 JSON 而不是二进制序列化，是为了**出事时读得懂**：两个自家进程之间的
//! 协议，诊断成本比几十字节的紧凑度重要得多。
//!
//! 但**大块二进制绝不进 JSON**：`serde_json` 把 `Vec<u8>` 编成数字数组，
//! 一个 8 MB 的帧会变成二十多 MB 的文本，再花一大笔 CPU 解析回来。
//! 故像素与 RDP 线上字节走裸的 `body` 段。

use serde::{Deserialize, Serialize};

/// 头部的字节上限。
///
/// 头是 JSON，正常只有几十到几百字节；64 KiB 已经宽出三个数量级。
/// 设上限不是为了省内存，是因为**长度前缀来自另一个进程**——
/// helper 那一侧正在解析远端不可信字节，它出 bug 时可能吐出一个荒谬的长度，
/// 而一个没有上限的 `Vec::with_capacity(n)` 会当场把内存吃光。
pub const MAX_HEADER_BYTES: u32 = 64 * 1024;

/// 体的字节上限（16 MiB）。
///
/// 1920×1080 的 RGBA 整帧是 8.3 MB；我们**从不发整帧**（只发脏矩形），
/// 所以正常载荷远小于此。16 MiB 留的是「4K 屏上一次大面积重绘」的余量。
///
/// 理由同 [`MAX_HEADER_BYTES`]：这是一个来自另一个进程的长度前缀。
pub const MAX_BODY_BYTES: u32 = 16 * 1024 * 1024;

/// 协议版本。两侧握手时比对——helper 是随安装包一起发的，
/// 正常永远同版本；但用户手工替换过二进制、或安装包升级到一半失败时不是。
/// 那时给一句准确的「版本不匹配」，比让一个陌生的解析错误冒出来强。
pub const PROTOCOL_VERSION: u32 = 1;

/// 像素格式。**闭合枚举**——两侧都必须认得，加一种要两侧同批改。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    /// 裸 RGBA，每像素 4 字节，行优先无 padding。前端直接 `putImageData`。
    Rgba,
}

/// 一个矩形（像素坐标，左上原点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    /// 面积（像素数）。用 `u32` 承载：`u16 × u16` 最大约 43 亿，
    /// 而 `u16::MAX²` 是 4_294_836_225，恰好放得下 `u32`。
    pub fn area(&self) -> u32 {
        u32::from(self.width) * u32::from(self.height)
    }

    /// 包含两者的最小矩形。**积压时用它合并**——见 `rdp-helper` 的
    /// framebuffer 模块：像素更新是后写覆盖前写，合并成包围盒并丢弃中间态
    /// 是正确的（终端字节相反，一个都不能丢）。
    pub fn union(&self, other: &Rect) -> Rect {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = (self.x + self.width).max(other.x + other.width);
        let y1 = (self.y + self.height).max(other.y + other.height);
        Rect {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// 鼠标按键。闭合枚举，两侧同批改。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// 一次输入事件（前端 → helper）。
///
/// 键盘用**扫描码**而不是字符：RDP 传的是 PS/2 scancode，
/// 而浏览器的 `KeyboardEvent.code` 是与布局无关的物理键位——两者能对上，
/// 且这样远端的键盘布局设置才是生效的那一个（用户在远端选了中文输入法，
/// 按下的应当是他自己布局下的那个键）。传字符会把布局在本地定死。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputEvent {
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
        x: u16,
        y: u16,
    },
    /// 滚轮。`delta` 正为向上/向左。
    MouseScroll {
        vertical: bool,
        delta: i16,
        x: u16,
        y: u16,
    },
    /// `scancode` 为 PS/2 Set 1 扫描码；`extended` 对应 0xE0 前缀键
    /// （方向键、右 Ctrl/Alt、Insert/Delete/Home/End/PgUp/PgDn 等）。
    Key {
        scancode: u16,
        extended: bool,
        down: bool,
    },
    /// 同步锁定键状态（CapsLock / NumLock / ScrollLock）。
    ///
    /// 单列一支而不是当普通按键：锁定键是**状态**不是**动作**。
    /// 窗口失焦再回来时本地与远端的锁定态可能已经不一致，
    /// 那时要同步的是「现在是什么状态」，而不是「按了一下」——
    /// 后者会把已经一致的状态改成不一致。
    SyncLockKeys {
        caps: bool,
        num: bool,
        scroll: bool,
    },
    /// **松开所有按下的键与鼠标按钮**（窗口失焦/切走时发）。
    ///
    /// 按下与松开是两条独立事件，而窗口失焦之后**松开事件不会再来**：
    /// 用户按住 Alt 摁 Tab 切走，远端就一直认为 Alt 还按着——回来以后
    /// 每个字母都变成 Alt+ 组合键，用户只能在远端点一下 Alt 自救。
    /// 拖拽中切走同理，远端的鼠标左键永远按着。
    ///
    /// 这是**状态复位**而不是动作，与 [`InputEvent::SyncLockKeys`] 同类：
    /// 上游 `ironrdp_input::Database::release_all()` 一次把键盘与鼠标
    /// 全部按下态放掉，只为真正按着的那些生成事件。
    ReleaseAll,
}

/// 连接失败的类别。
///
/// **分类而不是一句话**：主程序要据此决定下一步——认证失败该重新问口令，
/// 网络失败该走重连退避，证书被拒该把信任记录亮出来给用户看。
/// 一个笼统的 `Error(String)` 会把这三件事压成同一件，
/// 而它们对用户的意义完全不同（这与 `fs_sshengine::Error` 分 `Auth`/`Connect`/
/// `HostKey` 是同一个判断）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FailureKind {
    /// 用户名/口令/域不对。
    Auth,
    /// 协议层谈崩（版本、能力、编码）。
    Protocol,
    /// 证书裁决为拒绝（用户点了拒绝，或严格模式下不匹配）。
    CertRejected,
    /// 传输断了。**注意**：传输在主程序那一侧，helper 只能观察到
    /// 「对端不再给我字节了」。真正的原因（连不上/被拒/超时）由主程序判定。
    Transport,
    /// helper 自己出了问题（状态机崩了、解码失败）。
    Internal,
}

/// 主程序 → helper。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToHelper {
    /// 握手：确认协议版本。**必须是第一条**。
    Hello { version: u32 },
    /// 开始 RDP 连接序列。此时传输已由主程序建好，
    /// 线上字节随后经 [`ToHelper::NetIn`] 灌入。
    Connect(ConnectParams),
    /// 从网络收到的字节（体）。
    NetIn,
    /// 传输已终结（对端关闭或出错）。
    NetEof,
    /// 对 [`FromHelper::CertPresented`] 的裁决。
    CertVerdict { accept: bool },
    /// 一次输入事件。
    Input(InputEvent),
    /// 桌面尺寸变化（动态分辨率，阶段 2）。
    Resize { width: u16, height: u16 },
    /// **本机剪贴板变了**：把新内容通告给远端（阶段 2，CLIPRDR）。
    ///
    /// 体 = UTF-8 文本。走体而不是 JSON 字段有两个理由：剪贴板可以很大
    /// （复制一整个文件的内容很常见），JSON 转义会把它再放大一截；
    /// 而体是逐字节原样穿过的，不经任何转义。
    ///
    /// 阶段 2 只做**文本**：图片/文件列表是 RDPDR 与格式协商的大工程，
    /// 归阶段 3。不支持的格式在两侧都明确不通告，而不是通告了传空。
    ClipboardOffer,
    /// 对 [`FromHelper::ClipboardRequest`] 的应答：远端要的文本（体，UTF-8）。
    ClipboardData,
    /// 取远端剪贴板内容（对 [`FromHelper::ClipboardOffer`] 的响应动作）。
    ///
    /// 与 `ClipboardOffer` 分成两条而不是「通告即带数据」，是 CLIPRDR 自己的
    /// 形状：远端复制了 500 MB 文本时，通告是几十字节、取数才是那 500 MB。
    /// 本机没人粘贴就永远不取——这是协议给的免费优化，不该在桥接层抹掉。
    ClipboardPull,
    /// **一帧画完了，可以发下一帧**（帧级背压的回执）。
    ///
    /// # 为什么帧流需要回执
    ///
    /// helper 产帧的速度由**远端**决定（看视频时每秒几十帧全屏），
    /// 前端画帧的速度由**本机 webview** 决定。没有回执时两者之间是一条
    /// 无界队列：前端画不过来，帧就在事件桥里堆积，主线程被 JSON/base64
    /// 解析占满——表现是整个界面卡死，连「关闭连接」都点不动
    /// （2026-08-27 用户实测）。
    ///
    /// 回执把队列钉在一个很小的在途上限内。**积压期间不丢内容**：helper
    /// 侧的 `DirtyRects` 会把这段时间的所有更新合并成一块包围盒——像素的
    /// 语义就是「后写覆盖前写」，中间态本来就没有保留价值。这与终端字节流
    /// 的 never-drop 策略相反，理由记在 helper 的 fb.rs 模块头。
    FrameAck,
    /// **挂载一个共享目录**（RDPDR）。helper 把它作为一个盘符宣告给远端；
    /// 此后远端对这个盘的文件操作经 [`FromHelper::DriveIo`] 到达主程序，
    /// 主程序执行后用 [`ToHelper::DriveIoResult`] 应答。
    MountDrive {
        device: u8,
        name: String,
        readonly: bool,
    },
    /// 卸载共享目录：helper 撤销设备宣告（远端的资源管理器会显示盘消失）。
    UnmountDrive { device: u8 },
    /// 对 [`FromHelper::DriveIo`] 的应答。`status = 0` = 成功；非 0 为
    /// [`FsError`] 的 NTSTATUS（由 [`FsError::ntstatus`] 算出，两端只传一个 u32，
    /// 不在协议里复刻 Windows 的错误码表）。Read 的数据走**体**。
    DriveIoResult {
        id: u64,
        status: u32,
        /// Create 成功时的句柄（主程序侧 opaque，helper/远端只当数字用）。
        handle: Option<u64>,
        /// Write 的字节数 / Stat 的尺寸 / mtime（毫秒）。
        n: u64,
        is_dir: bool,
    },
    /// **把整屏重发一遍**。
    ///
    /// 帧是增量的（只带脏矩形的像素），所以客户端一旦漏画一块，那块就
    /// **永久错**到下次远端自己重绘为止——没有任何机制会自愈。两个场景需要它：
    /// · 前端画帧时抛异常（载荷损坏、尺寸不符、内存不足）——此时回执照发
    ///   （不发就是配额只减不还 → 画面冻死），但那块像素已经丢了；
    /// · 画布上下文重建（尺寸变化、标签重挂）——新画布是空白的。
    ///
    /// helper 侧的实现是把整屏标脏，让下一轮 flush 自然发出去。
    RequestFullFrame,
    /// 请求优雅退出。
    Shutdown,
}

/// 连接参数。
///
/// **口令在这里是明文的**，且这是刻意的：helper 需要它做 NTLM 计算。
/// 与之配套的约束写在这里，实现必须兑现：
/// - 只经匿名管道传递，不落盘、不进命令行参数（`ps` 看得见命令行）、
///   不进环境变量（同样可被枚举）；
/// - helper 用完即 `zeroize`；
/// - 主程序侧这个结构由 Vault 现取现用，不缓存。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectParams {
    /// 目标主机名。**只用于证书校验与 SPN 构造**，不用于建连接
    /// （连接是主程序建的，helper 没有网络）。
    pub server_name: String,
    pub username: String,
    /// 域名。空串 = 无域（本地账户）。
    pub domain: String,
    pub password: String,
    pub width: u16,
    pub height: u16,
    /// 键盘布局（Windows KLID，如美式 0x0409）。
    pub keyboard_layout: u32,
}

/// helper → 主程序。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum FromHelper {
    /// 握手应答。
    Hello { version: u32 },
    /// 要发到网络的字节（体）。
    NetOut,
    /// 服务器证书已收到，**等主程序裁决**。
    /// helper 在收到 [`ToHelper::CertVerdict`] 之前不会继续握手。
    CertPresented {
        /// SHA-256 指纹，小写十六进制加冒号分隔（与 SSH 主机密钥指纹同款排版，
        /// 用户对比时是同一套肌肉记忆）。
        fingerprint: String,
        subject: String,
        issuer: String,
        /// RFC3339。过期证书本身不是拒绝理由（内网自签常年过期），
        /// 但要让用户看得见。
        not_after: String,
    },
    /// 会话已建立，桌面尺寸如下。
    Connected { width: u16, height: u16 },
    /// 一块脏矩形（体 = 像素数据）。
    Frame { rect: Rect, format: PixelFormat },
    /// 进度播报（给用户看的一句话）。
    Status { message: String },
    /// 终止性失败。
    Failed { kind: FailureKind, message: String },
    /// **远端剪贴板变了**：远端通告它有文本可粘（阶段 2，CLIPRDR）。
    ///
    /// 不带内容——CLIPRDR 是**按需取**的协议：远端只通告「我有」，
    /// 真正的数据要本机发起一次请求才传。主程序收到这条即向 helper 发
    /// [`ToHelper::Input`] 之外的一条取数请求（`ClipboardPull`）。
    ClipboardOffer,
    /// 远端要本机的剪贴板内容（对本机 [`ToHelper::ClipboardOffer`] 的取数）。
    /// 主程序应答一条 [`ToHelper::ClipboardData`]。
    ClipboardRequest,
    /// 远端剪贴板的文本内容（体，UTF-8）——对 `ClipboardPull` 的应答。
    ClipboardData,
    /// **音频会话开始**（阶段 3，RDPSND）：远端要放声音了，格式如下。
    ///
    /// 主程序据此开输出设备。格式在一次会话里可能变（远端换了音频源），
    /// 故这条消息可以出现多次——每次都以最新的为准重开设备。
    AudioFormat {
        /// 采样率（Hz），如 44100。
        sample_rate: u32,
        /// 声道数（1 单声道 / 2 立体声）。
        channels: u16,
        /// 位深（8/16）。
        bits_per_sample: u16,
    },
    /// 一段 PCM 音频（体 = 裸样本，小端交错，格式由最近一条 `AudioFormat` 给出）。
    ///
    /// **只做 PCM**：远端可以协商 ADPCM/MP3/AAC 等编码格式，但那要在 helper
    /// 里塞一个解码器——而 helper 正是解析远端不可信字节的那一侧，往里加
    /// 一整个音频解码器与「把解析面关小」的整个设计相反。只通告 PCM 让远端
    /// 自己转（服务器端转码是标准做法，Windows 一定支持）。
    AudioData {
        /// 远端给的时间戳（毫秒）。主程序用它判断积压——音频与画面不同，
        /// 迟到的音频没有意义（人听得出 200ms 的不同步），该丢就丢。
        timestamp_ms: u32,
    },
    /// 音频会话结束（远端停了）。
    AudioClose,
    /// 远端对一个共享盘发起了文件操作（RDPDR）。helper 已把上游 IRP 解好、
    /// **翻译成本协议的形状**——主程序不认识 MS-RDPEFS 的类型，也不该认识：
    /// 安全闸（路径夹紧/只读/审计）全在主程序，语义层协议保持两边共有的最小集。
    /// Write 的数据走**体**。
    DriveIo { id: u64, device: u8, op: FsOp },
    /// 挂载的确认（设备已宣告给远端）。失败走 `Failed`。
    DriveMounted { device: u8 },
    /// 会话正常结束。
    Closed,
}

/// 远端文件操作的语义形状（helper 从 MS-RDPEFS IRP 翻译而来）。
///
/// # 为什么不复刻上游的 IRP 类型
///
/// `ServerDriveIoRequest` 携带 NT 语义（DesiredAccess 位掩码、CreateDisposition、
/// FileInformationClass…）。原样穿过管道等于让主程序也实现一遍 NT 语义——而
/// 安全闸需要的是**意图**（读还是写）与**路径**。翻译收窄在 helper 侧做掉，
/// 主程序只见「打开（读/写）、读、写、关、列目录、元数据」六件事。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsOp {
    /// 打开/创建。`write = true` = 远端要写（含追加/覆盖/新建）。
    /// `disposition`：0=打开已有（不存在则失败）、1=建新（已存在则失败）、
    /// 2=打开或截断、3=打开或建新。
    Create {
        path: String,
        write: bool,
        disposition: u8,
    },
    /// 读一块。数据走应答（`ToHelper::DriveIoResult`）的体。
    Read { handle: u64, offset: u64, len: u32 },
    /// 写一块。数据走本条消息的体。
    Write { handle: u64, offset: u64 },
    /// 关句柄。
    Close { handle: u64 },
    /// 列目录。应答体 = 每行 `名字\t类型(d/f)\t尺寸\tmtime_unix_ms\n`（UTF-8）。
    /// 列完再拉：status = [`FS_NO_MORE_FILES`] 且体空。
    ListDir { path: String },
    /// 元数据（尺寸/类型/mtime），供远端资源管理器的属性页与图标用。
    /// `n` = 字节数，`is_dir` = 是目录，mtime 不单独给（走 `n` 之外的审计口径
    /// 即可——资源管理器对 mtime 的显示精度不做承诺，MVP 不搬 FILETIME 换算）。
    Stat { path: String },
}

impl FsOp {
    /// 这条操作是不是**写**意图（只读闸与审计都按它判）。
    pub fn is_write(&self) -> bool {
        matches!(self, FsOp::Create { write: true, .. } | FsOp::Write { .. })
    }

    /// 操作涉及的相对路径（Create/ListDir/Stat 有；句柄操作没有——路径在
    /// 打开那一刻已经过闸并记进审计，之后按句柄走）。
    pub fn path(&self) -> Option<&str> {
        match self {
            FsOp::Create { path, .. } | FsOp::ListDir { path } | FsOp::Stat { path } => Some(path),
            _ => None,
        }
    }
}

/// 主程序执行 FS 操作的语义错误。`ntstatus()` 给出 helper 应答回给远端的
/// NTSTATUS（Windows 资源管理器认这些码并显示对应文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsError {
    /// 路径不在共享根内（`..`、绝对路径、符号链接逃逸）。回 ACCESS_DENIED：
    /// 回「不存在」会让远端反复重试，回「拒绝」语义更诚实。
    OutsideShare,
    NoSuchPath,
    /// 只读共享上的写意图。
    WriteProtected,
    /// 目标已存在（建新档撞名）。
    AlreadyExists,
    /// IO 错误（盘满/权限/…）。细节进审计与日志，远端只拿通用码。
    Io,
}

impl FsError {
    pub fn ntstatus(self) -> u32 {
        match self {
            FsError::OutsideShare => 0xC000_0022,   // STATUS_ACCESS_DENIED
            FsError::NoSuchPath => 0xC000_000F,     // STATUS_NO_SUCH_FILE
            FsError::WriteProtected => 0xC000_00A2, // STATUS_MEDIA_WRITE_PROTECTED
            FsError::AlreadyExists => 0xC000_0035,  // STATUS_OBJECT_NAME_COLLISION
            FsError::Io => 0xC000_0001,             // STATUS_UNSUCCESSFUL
        }
    }
}

/// 列目录已到头（远端按页拉取，这条让它停——` DriveIoResult` 的 status）。
pub const FS_NO_MORE_FILES: u32 = 0x8000_0006;

/// 一条消息 = 头 + 可选的二进制体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet<H> {
    pub header: H,
    /// 大块二进制。**只有声明了要体的变体才允许非空**——
    /// 见 [`HasBody::wants_body`]，编解码两侧都据它校验。
    pub body: Vec<u8>,
}

/// 「这个变体带不带二进制体」。
///
/// 做成 trait + 闭合 match 而不是写在文档里：加一个变体时编译器会强制你
/// 回答这个问题。写成注释的话，新变体会默认落进「不带体」，
/// 而那个默认在它真的需要体时是静默错的。
pub trait HasBody {
    fn wants_body(&self) -> bool;
}

impl HasBody for ToHelper {
    fn wants_body(&self) -> bool {
        match self {
            // 线上字节与剪贴板文本都走体（前者是二进制，后者可能很大且
            // 不该被 JSON 转义放大）
            ToHelper::NetIn | ToHelper::ClipboardOffer | ToHelper::ClipboardData => true,
            ToHelper::Hello { .. }
            | ToHelper::Connect(_)
            | ToHelper::NetEof
            | ToHelper::CertVerdict { .. }
            | ToHelper::Input(_)
            | ToHelper::Resize { .. }
            | ToHelper::ClipboardPull
            | ToHelper::FrameAck
            | ToHelper::RequestFullFrame
            | ToHelper::Shutdown
            | ToHelper::MountDrive { .. }
            | ToHelper::UnmountDrive { .. } => false,
            // Read 的应答带数据字节（其他 op 体空）
            ToHelper::DriveIoResult { .. } => true,
        }
    }
}

impl HasBody for FromHelper {
    fn wants_body(&self) -> bool {
        match self {
            // 线上字节、像素、剪贴板文本、PCM 样本——四种大块二进制都走体
            FromHelper::NetOut
            | FromHelper::Frame { .. }
            | FromHelper::ClipboardData
            | FromHelper::AudioData { .. } => true,
            FromHelper::Hello { .. }
            | FromHelper::CertPresented { .. }
            | FromHelper::Connected { .. }
            | FromHelper::Status { .. }
            | FromHelper::Failed { .. }
            | FromHelper::ClipboardOffer
            | FromHelper::ClipboardRequest
            | FromHelper::AudioFormat { .. }
            | FromHelper::AudioClose
            | FromHelper::DriveMounted { .. }
            | FromHelper::Closed => false,
            // Write 的数据字节（其他 op 体空）——HasBody 说的是「允许非空」，
            // 编解码两侧只校验这个；哪一种 FsOp 真的带体由 helper 侧组装时决定。
            FromHelper::DriveIo { .. } => true,
        }
    }
}

/// 编解码出的错。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// 头部超过 [`MAX_HEADER_BYTES`]。
    HeaderTooLarge { declared: u32 },
    /// 体超过 [`MAX_BODY_BYTES`]。
    BodyTooLarge { declared: u32 },
    /// 头不是合法 JSON，或不是本协议认得的形状。
    BadHeader { detail: String },
    /// **变体与体的有无对不上。**
    ///
    /// 单列一支而不是当成 `BadHeader`：这一条命中意味着两侧对协议的理解已经分叉，
    /// 而那与「这一条消息坏了」是完全不同的严重度——前者说明该换 helper 了。
    BodyMismatch { wants: bool, got: usize },
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HeaderTooLarge { declared } => {
                write!(f, "头部声明 {declared} 字节，超过上限 {MAX_HEADER_BYTES}")
            }
            Self::BodyTooLarge { declared } => {
                write!(f, "体声明 {declared} 字节，超过上限 {MAX_BODY_BYTES}")
            }
            Self::BadHeader { detail } => write!(f, "头部解析失败：{detail}"),
            Self::BodyMismatch { wants, got } => write!(
                f,
                "变体与体的有无不符：该变体 wants_body={wants}，实得 {got} 字节。\
                 这说明两侧对协议的理解已分叉，不是单条消息损坏"
            ),
        }
    }
}

impl std::error::Error for CodecError {}

/// 一条消息的固定头长度：两个 u32 大端。
pub const FRAME_PREFIX_LEN: usize = 8;

/// 编码一条消息，追加到 `out`。
///
/// **不返回 `Result`**：能出错的只有 serde 序列化，而本协议的头类型全部是
/// 平凡可序列化的（无自定义 `Serialize`、无 map 的非串键）。真序列化不了
/// 说明类型定义被改坏了，那是编译期该拦的事，不该在每个调用点摊一个 Result。
///
/// 体超限**在这里就 panic 而不是静默截断**：截断会产出一条半截的帧，
/// 而接收侧完全看不出来（长度前缀是自洽的），最后表现为屏幕上一块花掉的区域。
/// 宁可当场崩在产生它的那一行。
pub fn encode<H: Serialize + HasBody>(header: &H, body: &[u8], out: &mut Vec<u8>) {
    assert!(
        header.wants_body() || body.is_empty(),
        "给一个不带体的变体传了 {} 字节的体",
        body.len()
    );
    assert!(
        body.len() <= MAX_BODY_BYTES as usize,
        "体 {} 字节超过上限 {MAX_BODY_BYTES}",
        body.len()
    );
    let json = serde_json::to_vec(header).expect("协议头必须可序列化");
    assert!(
        json.len() <= MAX_HEADER_BYTES as usize,
        "头 {} 字节超过上限 {MAX_HEADER_BYTES}",
        json.len()
    );
    out.extend_from_slice(&(json.len() as u32).to_be_bytes());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(body);
}

/// 从缓冲区前端解出一条消息。
///
/// 返回 `Ok(None)` = **还没收全**（不是错误）。调用方继续读，缓冲区不动。
/// 返回 `Ok(Some((packet, consumed)))` 时，调用方负责把前 `consumed` 字节丢弃。
///
/// # 长度上限在**分配之前**检查
///
/// 顺序是刻意的：先读长度前缀、先比上限、**再**去看有没有收够。
/// 反过来写（先等收够再校验）等于让一个声称有 4 GB 体的坏包把接收循环
/// 挂在那里等一辈子——而那个坏包可能来自 helper 解析远端字节时的一个 bug。
pub fn decode<H: for<'de> Deserialize<'de> + HasBody>(
    buf: &[u8],
) -> Result<Option<(Packet<H>, usize)>, CodecError> {
    if buf.len() < FRAME_PREFIX_LEN {
        return Ok(None);
    }
    let header_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let body_len = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if header_len > MAX_HEADER_BYTES {
        return Err(CodecError::HeaderTooLarge {
            declared: header_len,
        });
    }
    if body_len > MAX_BODY_BYTES {
        return Err(CodecError::BodyTooLarge { declared: body_len });
    }
    let total = FRAME_PREFIX_LEN + header_len as usize + body_len as usize;
    if buf.len() < total {
        return Ok(None);
    }
    let hstart = FRAME_PREFIX_LEN;
    let hend = hstart + header_len as usize;
    let header: H =
        serde_json::from_slice(&buf[hstart..hend]).map_err(|e| CodecError::BadHeader {
            detail: e.to_string(),
        })?;
    let body = buf[hend..total].to_vec();
    if header.wants_body() != !body.is_empty() {
        return Err(CodecError::BodyMismatch {
            wants: header.wants_body(),
            got: body.len(),
        });
    }
    Ok(Some((Packet { header, body }, total)))
}
