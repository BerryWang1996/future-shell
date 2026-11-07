//! 对外 MCP 工具词表（总设计 §4.5）：封闭枚举 + 一张清单，不是字符串分发。
//!
//! **Vault 在这里不可表达。** 规格那句「Vault 永不以任何 MCP 工具形式暴露」
//! 若靠「枚举时过滤掉 vault 工具」兑现，就依赖一个人记得写过滤；这里的做法是
//! 枚举里根本没有那一支——想暴露 Vault 得先改这个枚举、改 [`MCP_MANIFEST`]、
//! 再改 [`ManifestEntry::vault_class`] 的断言，三处都在同一次 review 里可见。

use serde::Deserialize;

/// `terminal.read` 的取值面。封闭枚举而非字符串：`source: "everything"`
/// 这种拼错在 serde 层就死，而不是在某个 `match` 的 `_ =>` 里被当成默认值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadSource {
    Screen,
    Scrollback,
    All,
}

/// 对外暴露的全部 MCP 工具。⚠ 标记见 [`MCP_MANIFEST`] 的 `gated` 字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpTool {
    Managed(crate::managed::ManagedTool),
    ProfilesList,
    /// 列出当前会话（只读；不含任何凭据字段——见 `ports::SessionSummary`）。
    SessionsList,
    /// ⚠ 打开会话：经流 1 **消耗 Vault 凭据**连接主机。授权层另有两道
    /// （profile 须标 `mcp_allowed` + 每个 (client, profile) 首次打开须确认），
    /// 见 [`crate::authz`]。
    SessionsOpen {
        profile_id: String,
    },
    /// 读 headless 网格纯文本（不含转义序列），经同一脱敏过滤器。
    TerminalRead {
        session_id: String,
        source: ReadSource,
        max_lines: Option<usize>,
    },
    /// ⚠ 向终端发送 payload。分类目标 = **完整 payload 字符串**，基线至少 `Write`；
    /// 含控制/不可打印字符或超阈值 → `Dangerous`（§4.5，实现见 `gate.rs`）。
    TerminalSend {
        session_id: String,
        payload: String,
    },
    /// ⚠⚠ 原始终端控制序列：**恒 `Dangerous`**（强确认）。
    ///
    /// 单开一个工具而不是给 `terminal.send` 加个 `raw: true` 开关：布尔开关会让
    /// 「这次调用要不要强确认」取决于一个**调用方可自选**的参数，而工具名是
    /// 授权白名单的键——用户可以只授权 `terminal.send` 而不授权 `send_raw`。
    TerminalSendRaw {
        session_id: String,
        payload: String,
    },
    /// ⚠ 执行一条命令。共享 §4.4 `run_command` 契约（超时/截断/观察值），
    /// 契约实现只有一份（`fs_ai::exec`），由 app 层的端口转调。
    ///
    /// **没有 `mode` 字段**（Agent 那边有）。`current_terminal` 模式把命令注入
    /// 用户可见的 PTY，那是 `terminal.send` 的语义面、且它按 payload 整串分类；
    /// 若 `command.run` 也能选它，外部调用方就能拿着一个「按命令文本分类」的
    /// 裁决去做终端注入，绕开 `terminal.send` 的控制字符口径。
    CommandRun {
        session_id: String,
        command: String,
    },
    /// 列远端目录（只读）。
    SftpList {
        session_id: String,
        path: String,
    },
    /// 读远端文件（只读）。
    SftpRead {
        session_id: String,
        path: String,
        max_bytes: Option<usize>,
    },
    /// ⚠ 写远端文件（内容内联）。
    ///
    /// 与 Agent 的 `sftp_put`（本地源 → 远端目的）**不是同一个操作**：外部调用方
    /// 引用不到我们的本地文件系统，所以这里收内容而不收本地路径。分类因此也不同
    /// （只有远端目的路径参与），这就是 `TOOL_MANIFEST` 里 `sftp_put` 的
    /// `mcp_wire` 保持 `None` 的原因——名字对上而语义不对上，比不对名字更坏。
    SftpWrite {
        session_id: String,
        path: String,
        content: String,
    },
}

pub struct ManifestEntry {
    /// 线上工具名（点分，与规格逐字一致）。它同时是授权白名单的键。
    pub name: &'static str,
    pub description: &'static str,
    pub schema_json: &'static str,
    /// 规格里的 ⚠：这个工具会不会走策略闸门。
    ///
    /// **只读工具的 `gated: false` 不等于「不过闸门」**——`server.rs` 对每个工具
    /// 都调 `gate_check`（`Disabled` 档下连只读都拦死）。这个字段只用于清单展示与
    /// 「⚠ 集合与规格一致」的那条断言。
    pub gated: bool,
    /// 本清单恒 `false`。见模块头。
    pub vault_class: bool,
}

/// 对外 MCP 的完整工具清单。`tools/list` 从这里**过滤**生成（过滤器 = 授权层，
/// 见 [`crate::authz::Authorization::enumerable`]），而不是另写一份。
pub const MCP_MANIFEST: &[ManifestEntry] = &[
    ManifestEntry {
        name: "sftp.upload",
        description: "上传已授权本地目录中的普通文件到 SSH 原始连接端点，支持二进制，拒绝覆盖。PTY 菜单切换主机不会改变 SFTP 目标。返回任务 ID 后用 transfer.status 查询结果。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string","description":"会话 ID"},"local_path":{"type":"string","description":"已授权上传目录内的相对路径或绝对路径"},"remote_path":{"type":"string","description":"远端目标文件路径，相对路径基于 SSH 登录目录"}},"required":["session_id","local_path","remote_path"],"additionalProperties":false}"#,
        gated: true,
        vault_class: false,
    },
    ManifestEntry {
        name: "sftp.download",
        description: "从 SSH 原始连接端点下载普通文件到应用下载沙箱，拒绝覆盖。PTY 菜单不会改变 SFTP 目标。返回任务 ID，用 transfer.status 确认完成及校验结果。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string","description":"会话 ID"},"remote_path":{"type":"string","description":"远端源文件路径，相对路径基于 SSH 登录目录"}},"required":["session_id","remote_path"],"additionalProperties":false}"#,
        gated: true,
        vault_class: false,
    },
    ManifestEntry {
        name: "transfer.list",
        description: "查询本次运行中已接收的 MCP 传输。提交超时时先查询任务，避免重复提交。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string","description":"可选会话 ID"}},"required":[],"additionalProperties":false}"#,
        gated: false,
        vault_class: false,
    },
    ManifestEntry {
        name: "transfer.status",
        description: "查询任务进度、真实终态、错误和校验结果。完成前请等待并再次查询。",
        schema_json: r#"{"type":"object","properties":{"transfer_id":{"type":"string","description":"上传或下载工具返回的任务 ID"}},"required":["transfer_id"],"additionalProperties":false}"#,
        gated: false,
        vault_class: false,
    },
    ManifestEntry {
        name: "transfer.cancel",
        description: "请求取消指定传输。用 transfer.status 核实最终为取消或已完成。",
        schema_json: r#"{"type":"object","properties":{"transfer_id":{"type":"string","description":"任务 ID"}},"required":["transfer_id"],"additionalProperties":false}"#,
        gated: true,
        vault_class: false,
    },
    ManifestEntry {
        name: "sftp.stat",
        description: "在 SSH 原始连接端点读取条目的大小、类型、修改时间及软链接信息。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string","description":"会话 ID"},"path":{"type":"string","description":"远端路径"}},"required":["session_id","path"],"additionalProperties":false}"#,
        gated: false,
        vault_class: false,
    },
    ManifestEntry {
        name: "sftp.walk",
        description: "在 SSH 原始连接端点有界递归列出文件与空目录。返回跳过的软链接，超限会报错。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string","description":"会话 ID"},"path":{"type":"string","description":"远端根目录"},"exclude":{"type":"string","description":"可选排除表达式"}},"required":["session_id","path"],"additionalProperties":false}"#,
        gated: false,
        vault_class: false,
    },
    ManifestEntry {
        name: "sftp.mkdir",
        description: "在 SSH 原始连接端点创建一个目录。父目录必须存在，目标冲突时失败。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string","description":"会话 ID"},"path":{"type":"string","description":"远端目录路径"}},"required":["session_id","path"],"additionalProperties":false}"#,
        gated: true,
        vault_class: false,
    },
    ManifestEntry {
        name: "sftp.rename",
        description: "在同一 SSH 原始连接端点重命名或移动条目。目标已存在时拒绝。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string","description":"会话 ID"},"from":{"type":"string","description":"源路径"},"to":{"type":"string","description":"目标路径"}},"required":["session_id","from","to"],"additionalProperties":false}"#,
        gated: true,
        vault_class: false,
    },
    ManifestEntry {
        name: "sftp.copy",
        description: "在 SSH 原始连接端点内复制文件或目录，拒绝软链接和特殊文件。目标必须不存在，部分失败会报告已完成内容。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string","description":"会话 ID"},"from":{"type":"string","description":"源路径"},"to":{"type":"string","description":"目标路径"}},"required":["session_id","from","to"],"additionalProperties":false}"#,
        gated: true,
        vault_class: false,
    },
    ManifestEntry {
        name: "sftp.delete",
        description: "删除 SSH 原始连接端点的文件、链接或空目录。非空目录必须明确传 recursive=true，根目录禁止删除。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string","description":"会话 ID"},"path":{"type":"string","description":"待删除远端路径"},"recursive":{"type":"boolean","description":"是否递归删除目录，默认 false","default":false}},"required":["session_id","path"],"additionalProperties":false}"#,
        gated: true,
        vault_class: false,
    },
    ManifestEntry {
        name: "sftp.symlink",
        description: "在 SSH 原始连接端点创建软链接。明确传入目标和新链接路径，目标路径不因创建链接而改变。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string","description":"会话 ID"},"target":{"type":"string","description":"链接指向的路径"},"link_path":{"type":"string","description":"新建链接路径"}},"required":["session_id","target","link_path"],"additionalProperties":false}"#,
        gated: true,
        vault_class: false,
    },
    ManifestEntry {
        name: "sessions.details",
        description: "查询真实 SSH 连接端点、状态与连接代次。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string","description":"会话 ID"}},"required":["session_id"],"additionalProperties":false}"#,
        gated: false,
        vault_class: false,
    },
    ManifestEntry {
        name: "sessions.close",
        description: "关闭指定 SSH 会话。有正在传输的文件时先取消或等待完成，再关闭。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string","description":"会话 ID"}},"required":["session_id"],"additionalProperties":false}"#,
        gated: true,
        vault_class: false,
    },
    ManifestEntry {
        name: "sessions.reconnect",
        description: "请求已断线 SSH 会话重连。使用 sessions.details 确认新连接代次和结果。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string","description":"会话 ID"}},"required":["session_id"],"additionalProperties":false}"#,
        gated: true,
        vault_class: false,
    },

    ManifestEntry {
        name: "profiles.list",
        description: "查找用户已允许 MCP 打开的 SSH 连接配置。按名称选择，将返回的 profile_id 传给 sessions.open。名称重合时先请用户确认。",
        schema_json: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
        gated: false,
        vault_class: false,
    },
    ManifestEntry {
        name: "sessions.list",
        description: "列出 FutureShell 当前打开的 SSH 会话（id / 名称 / 主机 / 是否已连接）。空列表不代表没有可连接配置；用 profiles.list 找到配置后可用 sessions.open 新建会话。",
        schema_json: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
        gated: false,
        vault_class: false,
    },
    ManifestEntry {
        name: "sessions.open",
        description:
            "使用已允许 MCP 访问的 SSH 配置新建会话，无需先在 FutureShell 手动打开该服务器。从 profiles.list 获取 profile_id；同一配置可重复或并发打开，每次成功都会创建独立会话；使用返回的 session_id 操作。结果不明时先用 sessions.list 核对，不要盲目重发。认证和确认按当前配置执行。",
        schema_json: r#"{"type":"object","properties":{"profile_id":{"type":"string"}},"required":["profile_id"],"additionalProperties":false}"#,
        gated: true,
        vault_class: false,
    },
    ManifestEntry {
        name: "terminal.read",
        description: "读取会话终端的纯文本内容（已脱敏）。source=screen 为当前可见屏；all/scrollback 读取最新尾部，可设 max_lines。查看提示符或回显后再决定输入。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string"},"source":{"type":"string","enum":["screen","scrollback","all"]},"max_lines":{"type":"integer"}},"required":["session_id","source"],"additionalProperties":false}"#,
        gated: false,
        vault_class: false,
    },
    ManifestEntry {
        name: "terminal.send",
        description: "向已打开的交互终端输入文字。提交菜单选项或命令时，传单行 payload 并设置 press_enter=true，例如 payload 为 p。发送后用 terminal.read 查看结果。",
        schema_json: r#"{
            "type":"object",
            "properties":{
                "session_id":{"type":"string","description":"sessions.list 或 sessions.open 返回的会话 ID"},
                "payload":{"type":"string","description":"要输入的文字。与 press_enter=true 一起使用时传单行文字，无需添加换行或转义。空字符串可用于单独按 Enter。"},
                "press_enter":{"type":"boolean","default":false,"description":"在输入文字后按一次 Enter（发送 CR）。用于交互菜单、堡垒机和终端命令，按现有危险操作策略判定。"}
            },
            "required":["session_id","payload"],
            "additionalProperties":false
        }"#,
        gated: true,
        vault_class: false,
    },
    ManifestEntry {
        name: "terminal.send_raw",
        description: "向终端发送原始控制序列（Ctrl-C、ANSI CSI 等）。恒按危险处理，默认需强确认。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string"},"payload":{"type":"string"}},"required":["session_id","payload"],"additionalProperties":false}"#,
        gated: true,
        vault_class: false,
    },
    ManifestEntry {
        name: "command.run",
        description: "在 SSH 连接端点上通过独立的无 PTY exec 通道执行命令，返回 stdout/stderr/exit_code。\
                      已在终端内通过堡垒机菜单或嵌套 SSH 切换主机时，请用 terminal.send（press_enter=true）和 terminal.read 操作该终端。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string"},"command":{"type":"string"}},"required":["session_id","command"],"additionalProperties":false}"#,
        gated: true,
        vault_class: false,
    },
    ManifestEntry {
        name: "sftp.list",
        description: "通过原始 SSH 连接端点的独立无 PTY exec 通道列目录（只读），不跟随交互终端内的堡垒机选主；通道不支持时先查 sessions.details 和终端状态。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string"},"path":{"type":"string"}},"required":["session_id","path"],"additionalProperties":false}"#,
        gated: false,
        vault_class: false,
    },
    ManifestEntry {
        name: "sftp.read",
        description: "通过原始 SSH 连接端点的独立无 PTY exec 通道读文件（只读，默认上限 32 KiB），不跟随交互终端内的堡垒机选主；通道不支持时先查 sessions.details 和终端状态。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string"},"path":{"type":"string"},"max_bytes":{"type":"integer"}},"required":["session_id","path"],"additionalProperties":false}"#,
        gated: false,
        vault_class: false,
    },
    ManifestEntry {
        name: "sftp.write",
        description: "把一段内容写入远端文件。",
        schema_json: r#"{"type":"object","properties":{"session_id":{"type":"string"},"path":{"type":"string"},"content":{"type":"string"}},"required":["session_id","path","content"],"additionalProperties":false}"#,
        gated: true,
        vault_class: false,
    },
];

impl McpTool {
    /// 线上名。与 [`MCP_MANIFEST`] 同源（`manifest()` 反查那一条）。
    pub fn name(&self) -> &'static str {
        match self {
            Self::Managed(tool) => tool.name(),
            Self::ProfilesList => "profiles.list",
            Self::SessionsList => "sessions.list",
            Self::SessionsOpen { .. } => "sessions.open",
            Self::TerminalRead { .. } => "terminal.read",
            Self::TerminalSend { .. } => "terminal.send",
            Self::TerminalSendRaw { .. } => "terminal.send_raw",
            Self::CommandRun { .. } => "command.run",
            Self::SftpList { .. } => "sftp.list",
            Self::SftpRead { .. } => "sftp.read",
            Self::SftpWrite { .. } => "sftp.write",
        }
    }

    /// 这次调用作用于哪个会话（`sessions.list` / `sessions.open` 没有）。
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Managed(tool) => tool.session_id(),
            Self::ProfilesList | Self::SessionsList | Self::SessionsOpen { .. } => None,
            Self::TerminalRead { session_id, .. }
            | Self::TerminalSend { session_id, .. }
            | Self::TerminalSendRaw { session_id, .. }
            | Self::CommandRun { session_id, .. }
            | Self::SftpList { session_id, .. }
            | Self::SftpRead { session_id, .. }
            | Self::SftpWrite { session_id, .. } => Some(session_id),
        }
    }
}

/// 按名字取清单条目。查无 ⇒ `None` ⇒ 这个工具**不存在**。
pub fn manifest_of(name: &str) -> Option<&'static ManifestEntry> {
    MCP_MANIFEST.iter().find(|m| m.name == name)
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// 清单里查无此名。**未授权工具走的不是这条**（那条是 `Forbidden`）——
    /// 「不存在」与「存在但你不能用」是两件事，且前者不该泄露后者的存在性。
    UnknownTool(String),
    BadArgs {
        tool: String,
        detail: String,
    },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTool(n) => write!(f, "没有名为 {n} 的工具"),
            Self::BadArgs { tool, detail } => write!(f, "{tool} 的参数不合法：{detail}"),
        }
    }
}

// ── 参数 DTO ──────────────────────────────────────────────────────────────
// 全部 `deny_unknown_fields`。多出来的字段一律拒绝，而不是忽略：
// 调用方（或它背后的模型）自报 `"tier":"read_only"` / `"confirmed":true` 这类
// 字段时，宽容的解析器会**静默丢弃**它们，于是那次调用看起来完全正常——
// 而写这段调用的人以为自己已经把风险声明清楚了。拒绝是唯一诚实的回应。

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    profile_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    session_id: String,
    source: ReadSource,
    #[serde(default)]
    max_lines: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArgs {
    session_id: String,
    payload: String,
    #[serde(default)]
    press_enter: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSendArgs {
    session_id: String,
    payload: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    session_id: String,
    command: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    session_id: String,
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SftpReadArgs {
    session_id: String,
    path: String,
    #[serde(default)]
    max_bytes: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SftpWriteArgs {
    session_id: String,
    path: String,
    content: String,
}

/// `tools/call` 的唯一解析入口。
///
/// 顺序是**先查清单、后解析参数**：清单查无就直接 `UnknownTool`，不去看参数。
/// 反过来（先解析）会让一个不存在的工具因为参数畸形而报 `BadArgs`，
/// 调用方据此以为这个工具是存在的。
pub fn parse_tool(name: &str, args_json: &str) -> Result<McpTool, ParseError> {
    if manifest_of(name).is_none() {
        return Err(ParseError::UnknownTool(name.to_string()));
    }
    let bad = |e: serde_json::Error| ParseError::BadArgs {
        tool: name.to_string(),
        detail: e.to_string(),
    };
    // MCP 客户端对无参工具常常发 `null` 或干脆不发 `arguments`。
    let args_json = if args_json.trim().is_empty() || args_json.trim() == "null" {
        "{}"
    } else {
        args_json
    };
    if matches!(
        name,
        "sftp.upload"
            | "sftp.download"
            | "transfer.list"
            | "transfer.status"
            | "transfer.cancel"
            | "sftp.stat"
            | "sftp.walk"
            | "sftp.mkdir"
            | "sftp.rename"
            | "sftp.copy"
            | "sftp.delete"
            | "sftp.symlink"
            | "sessions.details"
            | "sessions.close"
            | "sessions.reconnect"
    ) {
        let mut value: serde_json::Value = serde_json::from_str(args_json).map_err(bad)?;
        let map = value.as_object_mut().ok_or_else(|| ParseError::BadArgs {
            tool: name.into(),
            detail: "参数必须是对象".into(),
        })?;
        if map.contains_key("operation") {
            return Err(ParseError::BadArgs {
                tool: name.into(),
                detail: "未知参数 operation".into(),
            });
        }
        map.insert("operation".into(), serde_json::Value::String(name.into()));
        return serde_json::from_value(value)
            .map(McpTool::Managed)
            .map_err(bad);
    }
    Ok(match name {
        "profiles.list" => {
            let _: Empty = serde_json::from_str(args_json).map_err(bad)?;
            McpTool::ProfilesList
        }
        "sessions.list" => {
            let _: Empty = serde_json::from_str(args_json).map_err(bad)?;
            McpTool::SessionsList
        }
        "sessions.open" => {
            let a: OpenArgs = serde_json::from_str(args_json).map_err(bad)?;
            McpTool::SessionsOpen {
                profile_id: a.profile_id,
            }
        }
        "terminal.read" => {
            let a: ReadArgs = serde_json::from_str(args_json).map_err(bad)?;
            McpTool::TerminalRead {
                session_id: a.session_id,
                source: a.source,
                max_lines: a.max_lines,
            }
        }
        "terminal.send" => {
            let mut a: SendArgs = serde_json::from_str(args_json).map_err(bad)?;
            if a.press_enter {
                if a.payload.contains(['\r', '\n']) {
                    return Err(ParseError::BadArgs {
                        tool: name.to_string(),
                        detail:
                            "press_enter=true 时 payload 必须是单行文字，由 FutureShell 附加回车"
                                .into(),
                    });
                }
                // 输入和 Enter 一次送入策略、审计与 PTY，沿用显式 CR 的判定。
                a.payload.push('\r');
            }
            McpTool::TerminalSend {
                session_id: a.session_id,
                payload: a.payload,
            }
        }
        "terminal.send_raw" => {
            let a: RawSendArgs = serde_json::from_str(args_json).map_err(bad)?;
            McpTool::TerminalSendRaw {
                session_id: a.session_id,
                payload: a.payload,
            }
        }
        "command.run" => {
            let a: RunArgs = serde_json::from_str(args_json).map_err(bad)?;
            McpTool::CommandRun {
                session_id: a.session_id,
                command: a.command,
            }
        }
        "sftp.list" => {
            let a: PathArgs = serde_json::from_str(args_json).map_err(bad)?;
            McpTool::SftpList {
                session_id: a.session_id,
                path: a.path,
            }
        }
        "sftp.read" => {
            let a: SftpReadArgs = serde_json::from_str(args_json).map_err(bad)?;
            McpTool::SftpRead {
                session_id: a.session_id,
                path: a.path,
                max_bytes: a.max_bytes,
            }
        }
        "sftp.write" => {
            let a: SftpWriteArgs = serde_json::from_str(args_json).map_err(bad)?;
            McpTool::SftpWrite {
                session_id: a.session_id,
                path: a.path,
                content: a.content,
            }
        }
        // 到不了：上面已按清单挡过。留 `unreachable!` 而不是给个默认工具——
        // 加了工具却忘记加解析臂时，我们要的是当场炸，不是悄悄退化成别的工具。
        other => unreachable!("清单里有 {other} 却没有解析臂"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 规格 §4.5 逐字点名的八个工具都在，且 send_raw 也在（同节另起一段点名）。
    #[test]
    fn the_manifest_is_exactly_the_spec_tool_list() {
        let names: Vec<_> = MCP_MANIFEST.iter().map(|m| m.name).collect();
        assert_eq!(
            names,
            vec![
                "sftp.upload",
                "sftp.download",
                "transfer.list",
                "transfer.status",
                "transfer.cancel",
                "sftp.stat",
                "sftp.walk",
                "sftp.mkdir",
                "sftp.rename",
                "sftp.copy",
                "sftp.delete",
                "sftp.symlink",
                "sessions.details",
                "sessions.close",
                "sessions.reconnect",
                "profiles.list",
                "sessions.list",
                "sessions.open",
                "terminal.read",
                "terminal.send",
                "terminal.send_raw",
                "command.run",
                "sftp.list",
                "sftp.read",
                "sftp.write",
            ]
        );
    }

    /// ⚠ 集合与规格一致。写死一张表而不是 `m.gated == 某个推导`——
    /// 推导会跟着实现一起漂，写死的表跟着**规格**走。
    #[test]
    fn the_gated_set_matches_the_spec_warning_marks() {
        let gated: Vec<_> = MCP_MANIFEST
            .iter()
            .filter(|m| m.gated)
            .map(|m| m.name)
            .collect();
        assert_eq!(
            gated,
            vec![
                "sftp.upload",
                "sftp.download",
                "transfer.cancel",
                "sftp.mkdir",
                "sftp.rename",
                "sftp.copy",
                "sftp.delete",
                "sftp.symlink",
                "sessions.close",
                "sessions.reconnect",
                "sessions.open",
                "terminal.send",
                "terminal.send_raw",
                "command.run",
                "sftp.write",
            ]
        );
    }

    /// 「Vault 永不以任何 MCP 工具形式暴露」的清单侧断言。
    ///
    /// 两头都断：① 没有条目标着 vault_class；② 没有任何工具名/描述沾 vault 词面。
    /// 只做①的话，加一个叫 `vault.read` 但忘了标 flag 的工具照样过。
    #[test]
    fn no_manifest_entry_is_vault_class_and_no_name_smells_like_one() {
        // 拼出来的词，免得这个测试文件本身被自己的扫描命中。
        let banned = [
            "va".to_string() + "ult",
            "key".to_string() + "ring",
            "pass".to_string() + "phrase",
            "secr".to_string() + "et",
        ];
        for m in MCP_MANIFEST {
            assert!(!m.vault_class, "{} 标了 vault_class", m.name);
            let hay = format!("{} {}", m.name, m.description).to_lowercase();
            for b in &banned {
                assert!(
                    !hay.contains(b.as_str()),
                    "{} 的名字/描述里出现了 {b}",
                    m.name
                );
            }
        }
        assert!(!MCP_MANIFEST.is_empty(), "做空防护：清单不能是空的");
    }

    #[test]
    fn terminal_send_only_appends_enter_when_requested() {
        for (args, payload) in [
            (r#"{"session_id":"s","payload":"p\n"}"#, "p\n"),
            (
                r#"{"session_id":"s","payload":"p","press_enter":false}"#,
                "p",
            ),
            (
                r#"{"session_id":"s","payload":"p","press_enter":true}"#,
                "p\r",
            ),
            (
                r#"{"session_id":"s","payload":"","press_enter":true}"#,
                "\r",
            ),
        ] {
            assert_eq!(
                parse_tool("terminal.send", args).unwrap(),
                McpTool::TerminalSend {
                    session_id: "s".into(),
                    payload: payload.into(),
                }
            );
        }
        let args = r#"{"session_id":"s","payload":"p\r","press_enter":true}"#;
        assert!(parse_tool("terminal.send", args).is_err());
        let args = r#"{"session_id":"s","payload":"p\n","press_enter":true}"#;
        assert!(parse_tool("terminal.send", args).is_err());
        let args = r#"{"session_id":"s","payload":"p","press_enter":true}"#;
        assert!(parse_tool("terminal.send_raw", args).is_err());
    }

    /// 枚举的每一支都能反查到清单条目，且清单每一条都能被解析出来。
    ///
    /// 双向：单向的话，「清单里加了一条却没有解析臂」或「枚举里加了一支却
    /// 不在清单里」各能漏掉一个方向。
    #[test]
    fn enum_and_manifest_cover_each_other_exactly() {
        let mut samples = vec![
            McpTool::ProfilesList,
            McpTool::SessionsList,
            McpTool::SessionsOpen {
                profile_id: "p".into(),
            },
            McpTool::TerminalRead {
                session_id: "s".into(),
                source: ReadSource::Screen,
                max_lines: None,
            },
            McpTool::TerminalSend {
                session_id: "s".into(),
                payload: "x".into(),
            },
            McpTool::TerminalSendRaw {
                session_id: "s".into(),
                payload: "x".into(),
            },
            McpTool::CommandRun {
                session_id: "s".into(),
                command: "ls".into(),
            },
            McpTool::SftpList {
                session_id: "s".into(),
                path: "/".into(),
            },
            McpTool::SftpRead {
                session_id: "s".into(),
                path: "/f".into(),
                max_bytes: None,
            },
            McpTool::SftpWrite {
                session_id: "s".into(),
                path: "/f".into(),
                content: "c".into(),
            },
        ];
        samples.extend(
            crate::managed::examples()
                .into_iter()
                .map(|(name, args)| parse_tool(name, &args.to_string()).unwrap()),
        );
        assert_eq!(
            samples.len(),
            MCP_MANIFEST.len(),
            "枚举支数与清单条数必须一致"
        );
        for s in &samples {
            assert!(manifest_of(s.name()).is_some(), "{} 不在清单里", s.name());
        }
        let from_enum: std::collections::BTreeSet<_> = samples.iter().map(|s| s.name()).collect();
        for m in MCP_MANIFEST {
            assert!(
                from_enum.contains(m.name),
                "{} 在清单里但枚举里没有对应支",
                m.name
            );
        }
    }

    /// 每条 schema 都是合法 JSON 且 `additionalProperties: false`。
    ///
    /// 后者与 DTO 的 `deny_unknown_fields` 是同一件事的两面：schema 说"不许多给"，
    /// 解析器真的不许。只写一面 = 客户端按 schema 拼对了、服务端却宽容（或反之）。
    #[test]
    fn every_schema_is_valid_json_and_closed() {
        for m in MCP_MANIFEST {
            let v: serde_json::Value =
                serde_json::from_str(m.schema_json).unwrap_or_else(|e| panic!("{}: {e}", m.name));
            assert_eq!(v["type"], "object", "{}", m.name);
            assert_eq!(
                v["additionalProperties"],
                serde_json::json!(false),
                "{} 的 schema 没有关掉 additionalProperties",
                m.name
            );
        }
    }

    #[test]
    fn an_unknown_tool_name_never_reaches_argument_parsing() {
        // 参数是畸形的；若先解析参数就会报 BadArgs，从而泄露「这个名字是认识的」。
        let e = parse_tool("vault.read", "{{{").unwrap_err();
        assert_eq!(e, ParseError::UnknownTool("vault.read".into()));
    }

    #[test]
    fn unknown_argument_fields_are_rejected_not_ignored() {
        let e = parse_tool(
            "command.run",
            r#"{"session_id":"s","command":"ls","tier":"read_only"}"#,
        )
        .unwrap_err();
        assert!(matches!(e, ParseError::BadArgs { .. }), "{e:?}");
    }

    /// `command.run` **没有** mode 字段——外部调用方不能选终端注入模式。
    #[test]
    fn command_run_refuses_a_mode_field() {
        let e = parse_tool(
            "command.run",
            r#"{"session_id":"s","command":"ls","mode":"current_terminal"}"#,
        )
        .unwrap_err();
        assert!(matches!(e, ParseError::BadArgs { .. }), "{e:?}");
        // 正向对照：不带 mode 是好的。
        assert!(parse_tool("command.run", r#"{"session_id":"s","command":"ls"}"#).is_ok());
        // schema 里也不该出现 mode（否则客户端会照着 schema 发，然后被拒）。
        let m = manifest_of("command.run").unwrap();
        assert!(!m.schema_json.contains("mode"), "schema 不该提 mode");
    }

    #[test]
    fn no_argument_tools_accept_empty_null_and_object() {
        for a in ["", "null", "{}", "   "] {
            assert_eq!(
                parse_tool("sessions.list", a).unwrap(),
                McpTool::SessionsList,
                "输入 {a:?}"
            );
        }
        // 但给了东西就得是对的形状。
        assert!(parse_tool("sessions.list", r#"{"x":1}"#).is_err());
    }

    #[test]
    fn read_source_is_a_closed_set() {
        assert!(parse_tool("terminal.read", r#"{"session_id":"s","source":"screen"}"#).is_ok());
        assert!(parse_tool(
            "terminal.read",
            r#"{"session_id":"s","source":"everything"}"#
        )
        .is_err());
    }

    #[test]
    fn session_bound_tools_all_carry_a_session_id() {
        let bound = [
            McpTool::TerminalRead {
                session_id: "s1".into(),
                source: ReadSource::All,
                max_lines: None,
            },
            McpTool::CommandRun {
                session_id: "s2".into(),
                command: "ls".into(),
            },
            McpTool::SftpWrite {
                session_id: "s3".into(),
                path: "/f".into(),
                content: "".into(),
            },
        ];
        for (t, want) in bound.iter().zip(["s1", "s2", "s3"]) {
            assert_eq!(t.session_id(), Some(want));
        }
        assert_eq!(McpTool::SessionsList.session_id(), None);
        assert_eq!(
            McpTool::SessionsOpen {
                profile_id: "p".into()
            }
            .session_id(),
            None
        );
    }

    /// 与 Agent 侧清单的**名字**对账：`TOOL_MANIFEST` 里凡填了 `mcp_wire` 的，
    /// 那个名字必须真的在 MCP 清单里存在。
    ///
    /// 这条防的是两张清单各自演化：Agent 那边改名或删工具时，这里当场红。
    #[test]
    fn every_agent_tool_that_claims_an_mcp_name_really_has_one() {
        let mut claimed = 0usize;
        for m in fs_ai::agent::tool::TOOL_MANIFEST {
            if let Some(w) = m.mcp_wire {
                claimed += 1;
                assert!(
                    manifest_of(w).is_some(),
                    "Agent 工具 {} 声称的 MCP 名 {w} 在 MCP 清单里不存在",
                    m.agent_wire
                );
            }
        }
        // 做空防护：一个都没填 = 这条断言无法失败。
        assert!(
            claimed >= 3,
            "至少三个 Agent 工具应有 MCP 对应名，实测 {claimed}"
        );
    }
}
