use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HostKeyPolicy {
    #[default]
    Tofu,
    Strict,
    FingerprintPinned,
}

// S5（med）：以下领域类型一律派生 `PartialEq, Eq`。这不是为了「方便写测试」——
// 没有它，往返测试只能逐字段挑几个断言（历史 `crud_roundtrip` 只断言了 name 与 group_path），
// 于是 upsert 的 14 个 bind 与 SELECT 的 14 列一旦发生列序错位或漏绑，测试仍旧全绿。
// 有了整体相等，`assert_eq!(got, p)` 一句就把「写进去的 == 读出来的」这条不变量钉死。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct JumpHop {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthRef,
    /// P1-9：**这一跳自己的**主机密钥策略。`None` = 未配置 → 连接层回落 TOFU。
    ///
    /// 为什么不能沿用 Profile 级字段：`host_key_pins` 是**绑定到某台具体主机身份**的断言
    /// （「host:port 这台机器的公钥必须恰好是这一把」），跨主机复用属于类型混淆。旧实现让每一跳
    /// 都继承目标主机的 policy/pins，于是 `hostkey::decide` 拿目标机的钉去比跳板机出示的密钥：
    /// 恒不匹配 → 每一跳都判 `Changed`（pinned 场景下跳板链必然连不上），而用户为了连上去
    /// 一旦在那个「密钥已变更」的红框上点了接受，跳板机就等于**从未被真正校验过**——
    /// 比不校验更糟，因为界面上显示的是「已按钉扎策略校验」。
    #[serde(default)]
    pub host_key_policy: Option<HostKeyPolicy>,
    /// P1-9：这一跳自己的指纹钉。为空即「本跳无钉」，不得回退去用目标主机的钉（同上）。
    #[serde(default)]
    pub host_key_pins: Vec<HostKeyPin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AuthRef {
    pub vault_record: Option<u64>, // 密码或私钥的 vault 记录 id
    /// P1-23：**加密私钥的口令（passphrase）**所在的 vault 记录 id；`None` = 私钥未加密。
    ///
    /// 单列一栏而不与 `vault_record` 合流：那一栏承载的是「口令 **或** 私钥 PEM」二选一
    /// （`build_credentials` 按是否含 `PRIVATE KEY` 分流），加密私钥需要的是**两份**秘密
    /// 同时在场。口令只在 publickey 认证真的要发起时才出 vault（见 `connect.rs::load_passphrase`），
    /// 以 `Zeroizing` 承载、随该轮 drop 清零；恒不进日志、不进错误文案、不回传前端（总设计 §3.2）。
    ///
    /// 带 `#[serde(default)]` 是硬约束：已落库的旧 Profile JSON 里没有这个键，缺省即 `None`。
    #[serde(default)]
    pub passphrase_vault_record: Option<u64>,
    #[serde(default)]
    pub allow_agent: bool, // 前端 partial 载荷缺省字段回填默认值（与 Profile 级 #[serde(default)] 同口径）
    #[serde(default)]
    pub allow_kbd_interactive: bool,
    /// **审计2 #36 已接通**：开 = keyboard-interactive 恰一个且不回显（echo=false）的提示
    /// 可自动以口令应答（判定纯函数 `auth::kbd_auto_answer`，多提示/回显提示/无口令一律
    /// 照旧弹框）。曾经是 M1 无消费方的死配置（R53 相位注），现已入认证循环。
    #[serde(default)]
    pub kbd_auto_answer_single: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostKeyPin {
    pub key_blob: String,
    pub fingerprint_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TermSettings {
    pub term: Option<String>,
    pub encoding: Option<String>,
    pub scrollback_lines: Option<u32>,
    #[serde(default)]
    pub theme_override: Option<TermThemeOverride>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TermThemeOverride {
    pub scheme: Option<String>,
    pub font_size: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SftpDefaults {
    pub local_dir: Option<String>,
    pub remote_dir: Option<String>,
    pub download_sandbox: Option<String>,
}

/// 串口档案的参数（M7.4）。
///
/// # 为什么在这里重定义一遍而不是直接用 `fs_serial::SerialParams`
///
/// 这一层是**档案的持久化格式**：它落进 SQLite 里的 profile JSON，用户存了半年的连接
/// 要一直读得出来。`fs_connmgr` 依赖 `fs_serial` 会让「数据层」倒过来依赖「传输层」，
/// 而且把档案格式绑在一个带 C 依赖（serialport）的 crate 上。
///
/// 两边的 JSON 形状**逐字相同**，由 app 层的契约测试钉住
///（`serial_cmd.rs` 的 `wire_shape_matches_fs_serial`）——分叉的那一天，用户的档案会读成
/// 默认值：115200 8N1，而他配的是 9600 7E1，现象是「连上了全是乱码」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SerialSettings {
    /// 端口名：`COM3` / `/dev/ttyUSB0` / `/dev/tty.usbserial-1420`。
    #[serde(default)]
    pub port: String,
    #[serde(default = "default_baud")]
    pub baud: u32,
    /// `five` / `six` / `seven` / `eight`
    #[serde(default = "default_data_bits")]
    pub data_bits: String,
    /// `none` / `odd` / `even`
    #[serde(default = "default_none")]
    pub parity: String,
    /// `one` / `two`
    #[serde(default = "default_stop_bits")]
    pub stop_bits: String,
    /// `none` / `software` / `hardware`
    #[serde(default = "default_none")]
    pub flow: String,
}

fn default_baud() -> u32 {
    115_200
}
fn default_data_bits() -> String {
    "eight".into()
}
fn default_stop_bits() -> String {
    "one".into()
}
fn default_none() -> String {
    "none".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiPolicy {
    #[serde(default)]
    pub auto_execute: AutoExec,
    #[serde(default)]
    pub mcp_allowed: bool,
}
impl Default for AiPolicy {
    fn default() -> Self {
        Self {
            auto_execute: AutoExec::Off,
            mcp_allowed: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutoExec {
    #[default]
    Off,
    ReadOnly,
    WithConfirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    Ssh,
    /// RDP（阶段 1）：端口默认 3389 由前端在协议切换时填——`default_port()`
    /// 是 SSH 的 22，改它会让所有既有 SSH profile 的端口语义漂移。
    Rdp,
    /// 串口直连（M7.4）。`host`/`port`/`username` 三栏对它**没有意义**——
    /// 连接目标是本机的一个设备（`COM3` / `/dev/ttyUSB0`），全部参数在 [`SerialSettings`]。
    /// 保留那三栏是为了不动存量表结构，校验时对串口档案放行（见 `Profile::validate`）。
    Serial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: Uuid,
    pub name: String,
    pub group_path: Option<String>,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    /// 连接协议（阶段 1 加）。`#[serde(default)]` 硬约束：已落库的旧 Profile
    /// JSON 没有这个键，缺省即 SSH——存量库零迁移平滑读到新版本。
    #[serde(default)]
    pub protocol: Protocol,
    #[serde(default)]
    pub auth: AuthRef, // 前端精简 JSON 新建连接时缺省字段回填默认值（P2-29）
    #[serde(default)]
    pub jump: Vec<JumpHop>,
    #[serde(default)]
    pub host_key_policy: HostKeyPolicy,
    #[serde(default)]
    pub host_key_pins: Vec<HostKeyPin>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub term: TermSettings,
    #[serde(default)]
    pub sftp: SftpDefaults,
    /// 串口参数（M7.4）。仅 `protocol == Serial` 时有意义；缺省即全默认（115200 8N1）。
    #[serde(default)]
    pub serial: SerialSettings,
    #[serde(default)]
    pub ai_policy: AiPolicy,
}
fn default_port() -> u16 {
    22
}

/// 审计2 #37：Profile 输入校验的尺寸与规模上限。
///
/// 这些数字**不是**安全边界（vault/主机信任另有一套），是「一条配置不该长成这样」的
/// 产品边界：挡住把库和内存当垃圾场用的异常输入。选值原则：真实合法配置永远到不了上限
/// （主机名 253 是 DNS 硬上限；8 跳板、64 钉、128 环境变量、1M 滚动行都远超日常），
/// 而越界值只会来自损坏的导出文件、手改的 JSON 或异常 WebView 调用。
pub mod limits {
    pub const NAME_MAX: usize = 128;
    pub const HOST_MAX: usize = 253; // DNS 全限定名的硬上限（含 IPv6 字面量）
    pub const USERNAME_MAX: usize = 128;
    pub const GROUP_PATH_MAX: usize = 512;
    pub const JUMP_MAX: usize = 8;
    pub const PINS_MAX: usize = 64;
    pub const PIN_BLOB_MAX: usize = 16 * 1024;
    pub const PIN_FP_MAX: usize = 128;
    pub const ENV_MAX: usize = 128;
    pub const ENV_KEY_MAX: usize = 128;
    pub const ENV_VALUE_MAX: usize = 8192;
    pub const TERM_ID_MAX: usize = 64;
    pub const SCROLLBACK_MAX: u32 = 1_000_000;
    pub const FONT_SIZE_MIN: u32 = 4;
    pub const FONT_SIZE_MAX: u32 = 100;
    pub const PATH_MAX: usize = 4096;
    /// 导入（JSON 与 known_hosts 文本）的单文件字节上限。
    pub const IMPORT_MAX_BYTES: usize = 8 * 1024 * 1024;
    /// 导入 JSON 的记录条数上限。
    pub const IMPORT_MAX_RECORDS: usize = 5_000;
    /// 导入 known_hosts 的文本行数上限。
    pub const IMPORT_MAX_LINES: usize = 100_000;
}

impl Profile {
    /// 审计2 #37：保存/导入前的统一校验。返回**人可读**的第一条违规描述（中文，直出 UI）。
    ///
    /// 校验刻意放在 `fs_connmgr` 而非 app 命令层：`profile_save`（IPC）与 `import_json`
    /// （导入面）走**同一个**函数，两条入口不可能漂移出两套规则。加载路径不校验——
    /// 库里已有的坏行由 `list_with_diagnostics` 诊断呈现，不在这里重复（也拦不住）。
    ///
    /// 长度一律按 `chars().count()` 计：上限的意义是「异常规模」，按字节惩罚 CJK 名字
    /// 没有任何好处。端口零值单独校验（类型 u16 已挡负数与 65536+）。
    pub fn validate(&self) -> Result<(), String> {
        let nonempty = |v: &str, what: &str| -> Result<(), String> {
            if v.trim().is_empty() {
                Err(format!("{what}不能为空"))
            } else {
                Ok(())
            }
        };
        let len_ok = |v: &str, max: usize, what: &str| -> Result<(), String> {
            if v.chars().count() > max {
                Err(format!("{what}长度超过上限 {max}"))
            } else {
                Ok(())
            }
        };
        nonempty(&self.name, "名称")?;
        len_ok(&self.name, limits::NAME_MAX, "名称")?;
        // 串口档案没有主机/用户名/端口：连接目标是本机的一个设备。对它按网络档案校验的话，
        // 用户会被要求填一个「主机」，而填什么都不影响它连到哪里——那是纯粹的噪音字段。
        if self.protocol == Protocol::Serial {
            nonempty(&self.serial.port, "串口")?;
            len_ok(&self.serial.port, limits::HOST_MAX, "串口")?;
            if self.serial.baud == 0 {
                return Err("波特率不能为 0".to_string());
            }
        } else {
            nonempty(&self.host, "主机")?;
            len_ok(&self.host, limits::HOST_MAX, "主机")?;
            nonempty(&self.username, "用户名")?;
            len_ok(&self.username, limits::USERNAME_MAX, "用户名")?;
            if self.port == 0 {
                return Err("端口不能为 0".to_string());
            }
        }
        if let Some(g) = &self.group_path {
            len_ok(g, limits::GROUP_PATH_MAX, "分组路径")?;
        }
        if self.jump.len() > limits::JUMP_MAX {
            return Err(format!("跳板数量超过上限 {}", limits::JUMP_MAX));
        }
        for (i, h) in self.jump.iter().enumerate() {
            nonempty(&h.host, &format!("第 {} 跳主机", i + 1))?;
            len_ok(&h.host, limits::HOST_MAX, &format!("第 {} 跳主机", i + 1))?;
            len_ok(
                &h.username,
                limits::USERNAME_MAX,
                &format!("第 {} 跳用户名", i + 1),
            )?;
            if h.port == 0 {
                return Err(format!("第 {} 跳端口不能为 0", i + 1));
            }
        }
        if self.host_key_pins.len() > limits::PINS_MAX {
            return Err(format!("指纹钉数量超过上限 {}", limits::PINS_MAX));
        }
        for p in &self.host_key_pins {
            len_ok(&p.key_blob, limits::PIN_BLOB_MAX, "指纹钉密钥")?;
            len_ok(&p.fingerprint_sha256, limits::PIN_FP_MAX, "指纹")?;
        }
        if self.env.len() > limits::ENV_MAX {
            return Err(format!("环境变量数量超过上限 {}", limits::ENV_MAX));
        }
        for (k, v) in &self.env {
            nonempty(k, "环境变量名")?;
            len_ok(k, limits::ENV_KEY_MAX, "环境变量名")?;
            len_ok(v, limits::ENV_VALUE_MAX, "环境变量值")?;
        }
        if let Some(t) = &self.term.term {
            len_ok(t, limits::TERM_ID_MAX, "终端类型")?;
        }
        if let Some(e) = &self.term.encoding {
            len_ok(e, limits::TERM_ID_MAX, "终端编码")?;
        }
        if self.term.scrollback_lines.unwrap_or(0) > limits::SCROLLBACK_MAX {
            return Err(format!("滚动行数超过上限 {}", limits::SCROLLBACK_MAX));
        }
        if let Some(f) = self.term.theme_override.as_ref().and_then(|t| t.font_size) {
            if !(limits::FONT_SIZE_MIN..=limits::FONT_SIZE_MAX).contains(&f) {
                return Err(format!(
                    "字号必须在 {}..={} 之间",
                    limits::FONT_SIZE_MIN,
                    limits::FONT_SIZE_MAX
                ));
            }
        }
        for (v, what) in [
            (&self.sftp.local_dir, "本地默认目录"),
            (&self.sftp.remote_dir, "远端默认目录"),
            (&self.sftp.download_sandbox, "下载沙箱"),
        ] {
            if let Some(p) = v {
                len_ok(p, limits::PATH_MAX, what)?;
            }
        }
        Ok(())
    }
}
