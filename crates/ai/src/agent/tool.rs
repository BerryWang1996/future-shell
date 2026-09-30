//! 工具词表：封闭枚举，不是字符串分发。加工具 = 改一处编译期事实。
//!
//! 发给模型的 tools 数组与（未来的）MCP tools/list 都从 [`TOOL_MANIFEST`] 过滤生成
//! ——「工具清单有两个来源」等于有两个清单。

use crate::exec::DEFAULT_MAX_OUTPUT_BYTES;
use serde::Deserialize;

#[derive(Debug)]
pub enum Tool {
    /// 模型可写面只有 `command` 与 `mode`。`timeout_secs` / `max_output_bytes`
    /// **不在**模型可写面——预算不由被审者自定，由 ExecPort 从 profile 配置 /
    /// `DEFAULT_*` 注入。
    RunCommand {
        command: String,
        mode: crate::exec::ExecMode,
    },
    /// 远端读：恒 `Tier::ReadOnly`（实现只会读），但**仍过 gate_check**——
    /// Disabled 下 `decide(ReadOnly, Disabled)` = Deny，读工具同样被全局开关拦死。
    ReadRemoteFile {
        path: String,
        max_bytes: Option<usize>,
    },
    ListRemoteDir {
        path: String,
    },
    /// 下载：本地目的路径过 `classify_transfer`（永不 ReadOnly）。
    SftpGet {
        remote: String,
        local_dest: String,
    },
    /// 上传：双向取严——本地源（外传敏感文件）× 远端目的（写远端敏感路径）。
    SftpPut {
        local_src: String,
        remote_dest: String,
    },
    /// 人类在环：streak 两个重置点之一。
    AskUser {
        question: String,
    },
    /// 收敛正路：不计数、不开通道，但仍过 gate_check（ReadOnly）。
    Finish {
        summary: String,
    },
    /// 外部 MCP 工具（M3 出口 6，已接通）。
    ///
    /// **不在 [`TOOL_MANIFEST`] 里**：外部工具由用户挂载决定，是运行期集合。
    /// 模型可见名是拼出来的 `mcp__<server>__<tool>`（[`EXTERNAL_MCP_PREFIX`]），
    /// `parse_tool` 按前缀分流，`wire_name` 按同一规则拼回。
    ///
    /// 分级：`gate` 的 `floor_at_write`——Write 起步、工具级配置只能 `.max()`
    /// 提级、永不降级（外部工具不匹配 shell 白名单，无从判断它到底做什么）。
    ExternalMcp {
        server_id: String,
        tool: String,
        arguments_json: String,
    },
}

impl Tool {
    /// manifest 里的 wire 名（stable；发给模型的 tools 数组与回包的 name 都用它）。
    ///
    /// **外部 MCP 工具没有静态名**——它的线上名是运行期拼的
    /// `mcp__<server>__<tool>`，所以这里返回自有串而不是 `&'static str`。
    /// 第一版返回 `&'static str` 并对 ExternalMcp `expect` 失败（当时它不可达）；
    /// 出口 6 把它接通之后那条 expect 就成了一颗一调就炸的雷。
    pub fn wire_name(&self) -> String {
        match self {
            Tool::ExternalMcp {
                server_id, tool, ..
            } => format!("{EXTERNAL_MCP_PREFIX}{server_id}__{tool}"),
            other => manifest_of(other).agent_wire.to_string(),
        }
    }
}

/// 外部 MCP 工具在模型可见面上的名字前缀（M3 出口 6）。
///
/// 形如 `mcp__<server_id>__<tool>`。为什么用前缀而不是把它们塞进
/// [`TOOL_MANIFEST`]：外部工具由**用户挂载**决定，是运行期集合，而 manifest 是
/// 编译期常量。给 `parse_tool` 传一份动态白名单也可以，但那要改它的签名与全部
/// 调用点，而前缀方案让「这是外部工具」成为名字**自身**的性质——
/// `parse_tool` 不必知道当前挂了哪些 server，分派仍是纯函数。
///
/// 双下划线分隔（而非单个 `.` 或 `:`）：多数模型的 tool name 只允许
/// `[A-Za-z0-9_-]`，点与冒号会被провider 拒。
pub const EXTERNAL_MCP_PREFIX: &str = "mcp__";

/// 从 `mcp__<server>__<tool>` 拆出 (server_id, tool)。
///
/// 拆不出 ⇒ `None`（调用方按未知工具处理）。**server_id 与 tool 都不许为空**：
/// 空 server_id 会让挂载表查找退化成「查第一个」，空 tool 名则会被外部 server
/// 当成缺参数——两者都是「看起来调了、其实调了别的」。
pub fn split_external_mcp(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix(EXTERNAL_MCP_PREFIX)?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

pub struct ManifestEntry {
    /// 模型只见白名单内的名字。
    pub agent_wire: &'static str,
    /// 同一操作在对外 MCP 面上的名字（`fs_mcpbridge::tool::MCP_MANIFEST`）。
    ///
    /// **只在两边确实是同一个操作、且分类规则相同时才填。** `sftp_put` 保持
    /// `None`：MCP 的 `sftp.write` 收的是内联内容而不是本地源路径（外部调用方
    /// 引用不到我们的本地文件系统），分类因此只看远端目的路径——名字对上而语义
    /// 不对上，比不对名字更坏。`sftp_get`/`ask_user`/`finish` 同理无对应面。
    ///
    /// 对账由 mcpbridge 侧两条断言守着：名字必须真实存在、**分类结论必须一致**。
    pub mcp_wire: Option<&'static str>,
    pub description: &'static str,
    /// 参数 JSON Schema。
    pub schema_json: &'static str,
    /// 本仓恒 `false`：Vault 工具在枚举里**不可表达**，而非被过滤掉。
    pub vault_class: bool,
}

/// Agent 面暴露的全部工具。`ExternalMcp` 不在模型可见清单里（见其变体注释），
/// `vault_class` 字段则保证「将来有人想加 Vault 工具」这件事在数据结构层面
/// 就被问一道。
pub const TOOL_MANIFEST: &[ManifestEntry] = &[
    ManifestEntry {
        agent_wire: "run_command",
        mcp_wire: Some("command.run"),
        description: "在目标主机上执行一条命令，返回 stdout/stderr/exit_code。\
                      流式命令（tail -f 等）会超时——用后台模式（nohup cmd &）后轮询。",
        schema_json: r#"{"type":"object","properties":{"command":{"type":"string","description":"要执行的命令"},"mode":{"type":"string","enum":["exec","current_terminal"],"description":"默认 exec。两种模式都**没有 TTY**：sudo/passwd/ssh 这类要读密码的命令在两边都会失败（'a terminal is required'），请改为把命令交给用户在他自己的终端里执行。current_terminal 的差别只是附一个哨兵行来取退出码"}},"required":["command"],"additionalProperties":false}"#,
        vault_class: false,
    },
    ManifestEntry {
        agent_wire: "read_remote_file",
        mcp_wire: Some("sftp.read"),
        description: "读取远端文件（只读）。",
        schema_json: r#"{"type":"object","properties":{"path":{"type":"string"},"max_bytes":{"type":"integer","description":"可选，默认 32768"}},"required":["path"],"additionalProperties":false}"#,
        vault_class: false,
    },
    ManifestEntry {
        agent_wire: "list_remote_dir",
        mcp_wire: Some("sftp.list"),
        description: "列出远端目录（只读）。",
        schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}"#,
        vault_class: false,
    },
    ManifestEntry {
        agent_wire: "sftp_get",
        mcp_wire: None,
        description: "下载远端文件到本地沙箱目录。",
        schema_json: r#"{"type":"object","properties":{"remote":{"type":"string"},"local_dest":{"type":"string","description":"本地目的路径（落在下载沙箱内）"}},"required":["remote","local_dest"],"additionalProperties":false}"#,
        vault_class: false,
    },
    ManifestEntry {
        agent_wire: "sftp_put",
        mcp_wire: None,
        description: "上传本地文件到远端。",
        schema_json: r#"{"type":"object","properties":{"local_src":{"type":"string"},"remote_dest":{"type":"string"}},"required":["local_src","remote_dest"],"additionalProperties":false}"#,
        vault_class: false,
    },
    ManifestEntry {
        agent_wire: "ask_user",
        mcp_wire: None,
        description: "向用户提问（需要 TTY 密码、二选一、或确认意图时用）。",
        schema_json: r#"{"type":"object","properties":{"question":{"type":"string"}},"required":["question"],"additionalProperties":false}"#,
        vault_class: false,
    },
    ManifestEntry {
        agent_wire: "finish",
        mcp_wire: None,
        description: "任务完成，提交最终报告。这是收敛的唯一正路。",
        schema_json: r#"{"type":"object","properties":{"summary":{"type":"string","description":"给用户看的最终报告"}},"required":["summary"],"additionalProperties":false}"#,
        vault_class: false,
    },
];

/// `ExternalMcp` 在 manifest 里**没有条目**——它的名字是运行期拼的
/// （见 [`Tool::wire_name`]）。本函数因此不接受它：`wire_name` 在调用本函数
/// 之前就把那一支分流走了，这里的 `unreachable!` 是那个分流的对偶断言。
fn manifest_of(tool: &Tool) -> &'static ManifestEntry {
    let name = match tool {
        Tool::RunCommand { .. } => "run_command",
        Tool::ReadRemoteFile { .. } => "read_remote_file",
        Tool::ListRemoteDir { .. } => "list_remote_dir",
        Tool::SftpGet { .. } => "sftp_get",
        Tool::SftpPut { .. } => "sftp_put",
        Tool::AskUser { .. } => "ask_user",
        Tool::Finish { .. } => "finish",
        Tool::ExternalMcp { .. } => {
            unreachable!("外部 MCP 工具的名字是运行期拼的，wire_name 已分流")
        }
    };
    TOOL_MANIFEST
        .iter()
        .find(|m| m.agent_wire == name)
        .expect("TOOL_MANIFEST 与 Tool 枚举同源维护；这条 expect 由 below 的守卫测试钉死")
}

#[derive(Debug)]
pub enum ToolParseError {
    /// 回模型：附可用工具名列表。
    UnknownTool(String),
    /// serde 错误原文回模型（它需要知道自己哪里写错了）。
    BadArgs { tool: String, msg: String },
    /// 空/纯空白命令：必须在 gate **之前**死掉——
    /// `classify("")` 返回 ReadOnly + 空 reasons（rules.rs 的分类器空洞），
    /// `decide(ReadOnly, WithConfirm)` 会给 AutoRun：一条空命令被「自动执行」。
    BlankCommand,
}

impl ToolParseError {
    /// 回喂给模型的错误文本（模型错误 lane：不进 gate，回喂重试）。
    pub fn feed_message(&self) -> String {
        match self {
            Self::UnknownTool(name) => format!(
                "未知工具 {name}。可用工具：{}。",
                TOOL_MANIFEST
                    .iter()
                    .map(|m| m.agent_wire.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::BadArgs { tool, msg } => format!("工具 {tool} 的参数不合法：{msg}"),
            Self::BlankCommand => "command 不能为空或纯空白".to_string(),
        }
    }
}

/* ── 参数 DTO：显式列字段 + deny_unknown_fields ─────────────────────────
 *
 * 模型塞 {"risk":"safe","tier":"read_only"} 之类的自报字段直接 BadArgs。
 * **不存在的字段无从读取**，比「读到了再忽略」强一个量级——这正是
 * ai_cmd.rs 那次手抄 match 事故想防的那类静默错位。
 */

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunCommandArgs {
    command: String,
    #[serde(default)]
    mode: Option<crate::exec::ExecMode>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadRemoteFileArgs {
    path: String,
    #[serde(default)]
    max_bytes: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListRemoteDirArgs {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SftpGetArgs {
    remote: String,
    local_dest: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SftpPutArgs {
    local_src: String,
    remote_dest: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskUserArgs {
    question: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinishArgs {
    summary: String,
}

/// 模型工具调用（name + arguments JSON 文本）→ `Tool`。
///
/// `arguments_json` 是 JSON 文本（wire.rs 既定约定——OpenAI 流式分片只有拼接后
/// 才 parse 得动），此处逐调用 `serde_json::from_str`；空参数归一 `"{}"`。
pub fn parse_tool(name: &str, args_json: &str) -> Result<Tool, ToolParseError> {
    // 外部 MCP 工具（M3 出口 6）：名字自带 `mcp__<server>__<tool>` 前缀，不在
    // 静态 manifest 里（它们由用户挂载决定，是运行期集合）。
    //
    // **这里不校验该 server 是否真的挂载着**：那份表在 app 层，而分派要保持纯函数。
    // 查不到的 server_id 由执行端口明确报错（exec_port 的 external_mcp），
    // 不会静默变成别的调用——见那里的 `找不到名为 X 的挂载配置`。
    if let Some((server_id, tool)) = split_external_mcp(name) {
        let args = if args_json.trim().is_empty() {
            "{}"
        } else {
            args_json
        };
        // arguments 必须是合法 JSON 对象——外部 server 那边也要按对象解析。
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(args).map_err(|e| {
            ToolParseError::BadArgs {
                tool: name.to_string(),
                msg: e.to_string(),
            }
        })?;
        return Ok(Tool::ExternalMcp {
            server_id: server_id.to_string(),
            tool: tool.to_string(),
            arguments_json: args.to_string(),
        });
    }
    // manifest 查无 ⇒ 不存在这个工具。
    let Some(entry) = TOOL_MANIFEST.iter().find(|m| m.agent_wire == name) else {
        return Err(ToolParseError::UnknownTool(name.to_string()));
    };
    let _ = entry; // 名字合法性的判据用过了；分派靠 match（全覆盖）
    let args = if args_json.trim().is_empty() {
        "{}"
    } else {
        args_json
    };
    match name {
        "run_command" => {
            let a: RunCommandArgs =
                serde_json::from_str(args).map_err(|e| ToolParseError::BadArgs {
                    tool: "run_command".to_string(),
                    msg: e.to_string(),
                })?;
            // 空白命令在 gate 之前死（见 BlankCommand 的注释）。
            if a.command.trim().is_empty() {
                return Err(ToolParseError::BlankCommand);
            }
            Ok(Tool::RunCommand {
                command: a.command,
                mode: a.mode.unwrap_or_default(),
            })
        }
        "read_remote_file" => {
            let a: ReadRemoteFileArgs =
                serde_json::from_str(args).map_err(|e| ToolParseError::BadArgs {
                    tool: "read_remote_file".to_string(),
                    msg: e.to_string(),
                })?;
            // max_bytes 钳到契约上限：模型自报 1GB 不该变成 1GB 的上下文回喂。
            let max_bytes = a.max_bytes.map(|n| n.clamp(1, DEFAULT_MAX_OUTPUT_BYTES));
            Ok(Tool::ReadRemoteFile {
                path: a.path,
                max_bytes,
            })
        }
        "list_remote_dir" => {
            let a: ListRemoteDirArgs =
                serde_json::from_str(args).map_err(|e| ToolParseError::BadArgs {
                    tool: "list_remote_dir".to_string(),
                    msg: e.to_string(),
                })?;
            Ok(Tool::ListRemoteDir { path: a.path })
        }
        "sftp_get" => {
            let a: SftpGetArgs =
                serde_json::from_str(args).map_err(|e| ToolParseError::BadArgs {
                    tool: "sftp_get".to_string(),
                    msg: e.to_string(),
                })?;
            Ok(Tool::SftpGet {
                remote: a.remote,
                local_dest: a.local_dest,
            })
        }
        "sftp_put" => {
            let a: SftpPutArgs =
                serde_json::from_str(args).map_err(|e| ToolParseError::BadArgs {
                    tool: "sftp_put".to_string(),
                    msg: e.to_string(),
                })?;
            Ok(Tool::SftpPut {
                local_src: a.local_src,
                remote_dest: a.remote_dest,
            })
        }
        "ask_user" => {
            let a: AskUserArgs =
                serde_json::from_str(args).map_err(|e| ToolParseError::BadArgs {
                    tool: "ask_user".to_string(),
                    msg: e.to_string(),
                })?;
            Ok(Tool::AskUser {
                question: a.question,
            })
        }
        "finish" => {
            let a: FinishArgs =
                serde_json::from_str(args).map_err(|e| ToolParseError::BadArgs {
                    tool: "finish".to_string(),
                    msg: e.to_string(),
                })?;
            Ok(Tool::Finish { summary: a.summary })
        }
        other => Err(ToolParseError::UnknownTool(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    /// **工具 schema 不得承诺一个不存在的 TTY**（2026-08-28 实测缺陷）。
    ///
    /// 原 schema 写着「需要 TTY（sudo 等）时用 current_terminal」，而
    /// `app/src/agent/exec_port.rs::exec` 对两种模式都走
    /// `channel_open_session()` + `channel.exec(...)`，**全程没有
    /// `request_pty`**（那个 `true` 是 want_reply 不是 PTY）；
    /// `wrap_for_terminal` 也只是接了个 `printf` 哨兵。
    ///
    /// 于是模型照着 schema 选 current_terminal 跑 `sudo`，照样快速失败
    /// （"a terminal is required"）——**schema 在骗模型**，而模型没有任何
    /// 办法发现这一点。
    ///
    /// 这条守卫钉的是「承诺与实现一致」：schema 里不许再出现「用
    /// current_terminal 就能拿到 TTY」这类话。真要给 TTY，先让实现走
    /// `session.write`（用户 PTY），再回来改这条判据。
    #[test]
    fn current_terminal_must_not_promise_a_tty() {
        let schema = super::TOOL_MANIFEST
            .iter()
            .find(|e| e.agent_wire == "run_command")
            .expect("run_command 必在清单里")
            .schema_json;
        // 「需要 TTY…用 current_terminal」这个句式是被禁的那一个
        let promises_tty =
            schema.contains("需要 TTY") || (schema.contains("TTY") && !schema.contains("没有 TTY"));
        assert!(
            !promises_tty,
            "run_command 的 schema 又在承诺 current_terminal 能给 TTY，             而实现里两种模式都没有 request_pty（见 exec_port.rs::exec）。             schema 是模型唯一的依据，骗它的代价是模型反复用一条必然失败的路。             当前 schema：{schema}"
        );
        // 反向：必须明确告诉模型「两边都没 TTY」，否则它只是不知道，
        // 仍会拿 sudo 去试
        assert!(
            schema.contains("没有 TTY") || schema.contains("no TTY"),
            "schema 应当明说两种模式都没有 TTY，让模型改走「交给用户执行」那条路"
        );
    }

    use super::*;
    use std::collections::HashSet;

    /// manifest 与枚举同源：每个枚举可达变体在 manifest 里有且仅有一条，
    /// manifest 里也没有枚举之外的条目。`manifest_of` 的 expect 由这条钉死。
    #[test]
    fn the_manifest_and_the_enum_are_the_same_source() {
        // 枚举侧能拿到的名字（全覆盖构造）
        let enum_names = [
            Tool::RunCommand {
                command: "x".into(),
                mode: Default::default(),
            },
            Tool::ReadRemoteFile {
                path: "x".into(),
                max_bytes: None,
            },
            Tool::ListRemoteDir { path: "x".into() },
            Tool::SftpGet {
                remote: "x".into(),
                local_dest: "x".into(),
            },
            Tool::SftpPut {
                local_src: "x".into(),
                remote_dest: "x".into(),
            },
            Tool::AskUser {
                question: "x".into(),
            },
            Tool::Finish {
                summary: "x".into(),
            },
        ]
        .iter()
        .map(Tool::wire_name)
        .collect::<HashSet<_>>();
        let manifest_names = TOOL_MANIFEST
            .iter()
            .map(|m| m.agent_wire.to_string())
            .collect::<HashSet<_>>();
        assert_eq!(enum_names, manifest_names, "枚举与 manifest 不同源");
        // ExternalMcp **不在** manifest 里——它的名字是运行期拼的
        // `mcp__<server>__<tool>`（M3 出口 6）。这里断言 manifest 里没有任何
        // 带外部前缀的条目：外部工具若哪天被塞进静态表，「用户挂了什么才有什么」
        // 这条就破了。
        assert!(
            !manifest_names
                .iter()
                .any(|n| n.starts_with(EXTERNAL_MCP_PREFIX)),
            "外部 MCP 工具不该出现在静态 manifest 里"
        );
        // 全部条目 vault_class = false：Vault 工具不可表达。
        for m in TOOL_MANIFEST {
            assert!(!m.vault_class, "{} 不该是 vault_class", m.agent_wire);
        }
    }

    /// `mcp_wire` 的形状约束（**内容对账在 mcpbridge 侧**——名字是否真实存在、
    /// 分类结论是否一致，那两条断言要读到 MCP 清单，而依赖方向是
    /// mcpbridge → fs_ai，不能反过来）。
    ///
    /// 这里只钉两件本 crate 自己能判的事：填了的必须是点分名（Agent 侧用下划线，
    /// 两套命名混用会让「这个串该发给谁」变成靠记忆的事），以及**哪几个该是
    /// `None`**——后者是本条的重点：`sftp_put` 与 `sftp.write` 是两个不同的操作
    /// （前者收本地源路径并双向取严，后者收内联内容只看远端目的），
    /// 有人图省事把它填上，这条当场红。
    #[test]
    fn mcp_wire_is_dotted_where_present_and_absent_where_the_operations_differ() {
        let by = |n: &str| TOOL_MANIFEST.iter().find(|m| m.agent_wire == n).unwrap();
        for n in ["sftp_put", "sftp_get", "ask_user", "finish"] {
            assert!(
                by(n).mcp_wire.is_none(),
                "{n} 在 MCP 面上没有语义相同的对应工具，mcp_wire 必须留空"
            );
        }
        let mut filled = 0;
        for m in TOOL_MANIFEST {
            if let Some(w) = m.mcp_wire {
                filled += 1;
                assert!(
                    w.contains('.'),
                    "{} 的 mcp_wire {w} 不是点分名",
                    m.agent_wire
                );
                assert!(
                    !w.contains('_') || w == "terminal.send_raw",
                    "{w} 混用了下划线"
                );
            }
        }
        // 做空防护：全 None 会让上面的循环一次都不执行。
        assert_eq!(
            filled, 3,
            "当前应有三个工具两边同义：run_command / read_remote_file / list_remote_dir"
        );
    }

    /// **空命令在 parse 层死**——`classify("")` 会给 ReadOnly（分类器空洞），
    /// 闸门拦不住一条「被自动执行的空命令」。
    #[test]
    fn a_blank_command_dies_at_parse_not_at_the_gate() {
        // 模型给的是 JSON 文本：命令里的换行/制表符以 \n / \t 转义出现，
        // 不是裸字节（裸控制字节在 JSON 字符串里不合法，serde 会先拒）。
        // 空格无需转义，直书。
        for cmd in ["", "   ", "\\n\\t "] {
            let r = parse_tool("run_command", &format!("{{\"command\":\"{cmd}\"}}"));
            assert!(
                matches!(r, Err(ToolParseError::BlankCommand)),
                "{cmd:?} 该拒"
            );
        }
        // 对照：正常命令照过
        assert!(parse_tool("run_command", r#"{"command":"ls"}"#).is_ok());
    }

    /// **模型自报危险级被拒**：`deny_unknown_fields` 的结构性防御。
    #[test]
    fn a_model_self_reported_risk_field_is_rejected() {
        let r = parse_tool("run_command", r#"{"command":"ls","risk":"safe"}"#);
        assert!(
            matches!(r, Err(ToolParseError::BadArgs { .. })),
            "自报字段被静默忽略了"
        );
        let r = parse_tool("run_command", r#"{"command":"ls","tier":"read_only"}"#);
        assert!(matches!(r, Err(ToolParseError::BadArgs { .. })));
    }

    #[test]
    fn unknown_tools_come_back_with_the_whitelist() {
        let r = parse_tool("external_mcp", "{}");
        assert!(matches!(&r, Err(ToolParseError::UnknownTool(n)) if n == "external_mcp"));
        // ExternalMcp 本批不可达：模型发它 → UnknownTool（manifest 查无），
        // 而不是被当成真工具去分类执行。
        let msg = r.unwrap_err().feed_message();
        assert!(msg.contains("run_command"), "错误回喂要带可用清单：{msg}");
        assert!(msg.contains("finish"));
    }

    /// 空参数归一 `"{}"` 的分支只对**全字段可选**的工具有意义——当前每个工具
    /// 都有必填字段，于是空白参数必然 BadArgs。这条钉的是那个诚实的行为：
    /// 缺 `path` 就说缺 `path`，不是静默当成空参数成功。
    #[test]
    fn whitespace_arguments_on_a_required_field_tool_is_bad_args() {
        let r = parse_tool("list_remote_dir", "   ");
        assert!(
            matches!(&r, Err(ToolParseError::BadArgs { tool, .. }) if tool == "list_remote_dir"),
            "缺必填字段该 BadArgs，而不是被当成空参数：{r:?}"
        );
    }

    /// max_bytes 钳到契约上限：模型自报 1GB 不该变成 1GB 的上下文回喂。
    #[test]
    fn max_bytes_is_clamped_to_the_contract_cap() {
        let Ok(Tool::ReadRemoteFile {
            max_bytes: Some(n), ..
        }) = parse_tool("read_remote_file", r#"{"path":"/x","max_bytes":100000000}"#)
        else {
            panic!("该解析成功");
        };
        assert_eq!(n, DEFAULT_MAX_OUTPUT_BYTES);
        // 0 也钳到 1（0 字节的读没有意义，但也不该被当 0 传下去）
        let Ok(Tool::ReadRemoteFile {
            max_bytes: Some(n), ..
        }) = parse_tool("read_remote_file", r#"{"path":"/x","max_bytes":0}"#)
        else {
            panic!()
        };
        assert_eq!(n, 1);
    }

    /// run_command 的 mode 缺省 Exec；显式 current_terminal 也认。
    #[test]
    fn run_command_mode_defaults_to_exec() {
        assert!(matches!(
            parse_tool("run_command", r#"{"command":"ls"}"#),
            Ok(Tool::RunCommand {
                mode: crate::exec::ExecMode::Exec,
                ..
            })
        ));
        assert!(matches!(
            parse_tool(
                "run_command",
                r#"{"command":"sudo x","mode":"current_terminal"}"#
            ),
            Ok(Tool::RunCommand {
                mode: crate::exec::ExecMode::CurrentTerminal,
                ..
            })
        ));
        // 未知 mode 串 = BadArgs（不猜）
        assert!(matches!(
            parse_tool("run_command", r#"{"command":"x","mode":"turbo"}"#),
            Err(ToolParseError::BadArgs { .. })
        ));
    }

    /// 外部 MCP 工具名的拆分：形状对了才拆得出，且**两段都不许为空**。
    ///
    /// 空 server_id 会让挂载表查找退化成「查第一个」；空 tool 名会被外部 server
    /// 当成缺参数——两者都是「看起来调了、其实调了别的」。
    #[test]
    fn an_external_tool_name_splits_only_when_both_halves_are_present() {
        assert_eq!(split_external_mcp("mcp__fs__read"), Some(("fs", "read")));
        // tool 名里可以再有下划线（split_once 只吃第一个分隔）
        assert_eq!(
            split_external_mcp("mcp__fs__read_file"),
            Some(("fs", "read_file"))
        );
        // 拆不出的形状一律 None
        for bad in [
            "run_command",  // 内置工具
            "mcp__fs",      // 缺 tool 段
            "mcp____read",  // 空 server_id
            "mcp__fs__",    // 空 tool
            "mcp_fs__read", // 前缀不对（单下划线）
            "",
        ] {
            assert_eq!(split_external_mcp(bad), None, "{bad:?} 不该拆得出");
        }
    }

    /// 带前缀的名字进 parse_tool ⇒ 得到 ExternalMcp，且 wire_name 拼得回去。
    ///
    /// 往返是重点：名字拆开又拼回必须是同一个串，否则审计行里记的工具名与
    /// 模型实际调的对不上。
    #[test]
    fn an_external_tool_round_trips_through_parse_and_wire_name() {
        let t = parse_tool("mcp__fs__read_file", r#"{"path":"/etc/motd"}"#).unwrap();
        match &t {
            Tool::ExternalMcp {
                server_id,
                tool,
                arguments_json,
            } => {
                assert_eq!(server_id, "fs");
                assert_eq!(tool, "read_file");
                assert!(arguments_json.contains("/etc/motd"));
            }
            other => panic!("该解析成 ExternalMcp，实得 {other:?}"),
        }
        assert_eq!(t.wire_name(), "mcp__fs__read_file", "名字必须能拼回去");
    }

    /// 外部工具的参数**必须是 JSON 对象**——不是对象就 BadArgs 回喂，
    /// 而不是把一个畸形串原样转给外部 server。
    #[test]
    fn external_tool_arguments_must_be_a_json_object() {
        assert!(parse_tool("mcp__fs__read", "{}").is_ok());
        assert!(parse_tool("mcp__fs__read", "").is_ok(), "空参数归一为 {{}}");
        for bad in ["[1,2]", "\"str\"", "not json", "null"] {
            let r = parse_tool("mcp__fs__read", bad);
            assert!(
                matches!(&r, Err(ToolParseError::BadArgs { .. })),
                "{bad:?} 该是 BadArgs，实得 {r:?}"
            );
        }
    }
}
