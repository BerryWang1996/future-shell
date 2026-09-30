//! MCP 面的**唯一**分类与裁决入口（总设计 §4.5）。
//!
//! 规格原文：「⚠ = 经策略引擎，**与内置 Agent 同一闸门**」。这里把「同一」
//! 做成字面事实而不是口径约定：凡两边确实是同一个操作的工具，分级路径就是
//! 构造一个 `fs_ai::agent::tool::Tool` 再调 `classify_for_gate`——同一个函数、
//! 同一份规则表。
//!
//! 剩下的四个工具在 Agent 面没有对应物（`sessions.*` / `terminal.*` /
//! 内联内容写远端），它们的分级规则写在本文件里，且**只写在这里**：
//! `lib.rs` 的 `every_policy_surface_has_exactly_one_production_call_site_in_this_crate`
//! 钉住 `fs_policy::classify` / `classify_transfer` / `classify_for_gate` /
//! `gate::decide` 在本 crate 里只能出现在 gate.rs。
//!
//! # 为什么裁决类型直接用 `fs_policy::Decision`
//!
//! `fs_ai::agent::gate::GateOutcome` 是 Agent 面为自己的展示需要包的一层。
//! MCP 面再包一层同构的枚举，只会多出一处「四个臂要对上」的地方；而那种
//! 地方一旦对不上，错的方向永远是把 `Deny` 翻译成别的。这里直接用
//! `fs_policy::gate::decide` 的返回值，四个臂无处可错。

use crate::payload::{self, Opacity};
use crate::tool::McpTool;
use fs_policy::{gate, AiMode, Decision, DenyReason, Reason, Tier, Verdict};

/// `terminal.send` 的 Write 地板（规格：「基线**至少** `write`」）。
pub const RULE_SEND_FLOOR: &str = "mcp_terminal_send_floor";
/// `terminal.send_raw` 恒 `dangerous`（规格：「⚠⚠ 恒为 `dangerous`，强确认」）。
pub const RULE_SEND_RAW: &str = "mcp_send_raw_is_always_dangerous";
/// `sessions.open` 会消耗保险箱凭据。
pub const RULE_SESSIONS_OPEN: &str = "mcp_sessions_open_spends_credentials";

/// 分类结果 + 发起方。
///
/// 与 `fs_ai::agent::gate::Gated` 一样，**刻意没有「接受一个现成 Tier」的构造
/// 路径**：调用方自报的分级在类型上无处安放。MCP 面比 Agent 面更需要这条——
/// 这里的调用方是任何能以同一 OS 用户启动本二进制的本地进程（§4.5 信任边界）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpGated {
    /// 线上工具名，进审计行。
    pub tool_name: &'static str,
    pub verdict: Verdict,
    /// 客户端标签。**展示用，非安全边界**（§4.5：stdio 无协议级调用方身份）。
    pub caller: String,
}

impl McpGated {
    pub fn tier(&self) -> Tier {
        self.verdict.tier
    }
}

/// 只读且无话可说的裁决（`sessions.list` / `terminal.read`）。
///
/// **仍然要过 [`decide_for_mcp`]**——总开关 `Disabled` 档下连只读都要拦死。
/// 这与 `fs_ai::agent::gate` 对四个只读工具的处理是同一口径。
fn read_only() -> Verdict {
    Verdict {
        tier: Tier::ReadOnly,
        reasons: Vec::new(),
    }
}

fn one(rule: &'static str, tier: Tier, detail: impl Into<String>) -> Verdict {
    Verdict {
        tier,
        reasons: vec![Reason {
            rule,
            tier,
            detail: detail.into(),
        }],
    }
}

/// 借道 Agent 面的闸门给一个 Agent 工具分级。
///
/// 这个函数是「同一闸门」那句话的全部实现。它**不**做任何调整、地板或覆盖——
/// 一旦这里出现一行「MCP 这边特殊处理一下」，两个面就开始漂了。
fn via_agent_gate(t: fs_ai::agent::tool::Tool, caller: &str) -> Verdict {
    let ext = fs_ai::agent::gate::ExternalToolTiers::default();
    fs_ai::agent::gate::classify_for_gate(
        t,
        fs_ai::agent::gate::Initiator::Mcp {
            caller: caller.to_string(),
        },
        &ext,
    )
    .verdict
}

/// `terminal.send` 的分级（规格 §4.5 那一整段）。
///
/// 两步，顺序不能换：
/// 1. **先看透不透**。看不透就恒 `Dangerous`，**不再问分类器的意见**——
///    分类器对这种输入的结论已被证明不可信（见 [`crate::payload`] 模块头那张
///    实测表），拿一个不可信的 `ReadOnly` 去取严没有意义，只会在审计里留下一个
///    误导性的「分类结论」。
/// 2. 看得透才把**完整 payload 字符串**交给分类器，再抬到 Write 地板。
///
/// 地板的方向只能是 `.max()`。写成 `.min()` 或者「若分类器说 ReadOnly 就
/// 用 ReadOnly」都是把地板变成天花板，而这类方向错误没有任何正常测试会红——
/// `the_write_floor_only_ever_raises` 专门钉这个方向。
fn classify_send(payload_str: &str) -> Verdict {
    match payload::opacity_of(payload_str) {
        Opacity::Opaque(r) => one(r.rule(), Tier::Dangerous, r.detail()),
        Opacity::Transparent => {
            let mut v = fs_policy::classify(payload_str);
            let floored = v.tier.max(Tier::Write);
            if floored != v.tier {
                // 抬了地板就得说出来，否则确认框上是一句「无可疑之处」配一个
                // 需要确认的操作，用户无从判断该不该批。
                v.reasons.push(Reason {
                    rule: RULE_SEND_FLOOR,
                    tier: Tier::Write,
                    detail: "向终端发送文本按至少 `write` 处理：分片/逐字符发送无法逐片分类".into(),
                });
            }
            v.tier = floored;
            v
        }
    }
}

/// **本 crate 唯一的分类入口。**
///
/// `caller` 只是原样装进 [`McpGated`] 供审计区分，**完全不参与分级**——
/// 与 `fs_ai::agent::gate::classify_for_gate` 对 `initiator` 的处理同口径。
/// 让调用方标签参与分级就是给「换个标签再试一次」开门。
pub fn classify_for_mcp(tool: &McpTool, caller: &str) -> McpGated {
    use fs_ai::agent::tool::Tool as AgentTool;

    let verdict = match tool {
        // ── 与 Agent 面同一个操作：借道同一个闸门 ──────────────────────
        McpTool::CommandRun { command, .. } => via_agent_gate(
            AgentTool::RunCommand {
                command: command.clone(),
                // MCP 面没有 mode 字段（见 tool.rs 那支的注释）：外部调用方
                // 不能选终端注入模式，这里硬写 Exec。
                mode: fs_ai::exec::ExecMode::Exec,
            },
            caller,
        ),
        McpTool::SftpRead {
            path, max_bytes, ..
        } => via_agent_gate(
            AgentTool::ReadRemoteFile {
                path: path.clone(),
                max_bytes: *max_bytes,
            },
            caller,
        ),
        McpTool::SftpList { path, .. } => {
            via_agent_gate(AgentTool::ListRemoteDir { path: path.clone() }, caller)
        }

        // ── Agent 面没有对应物：规则写在这里 ──────────────────────────
        McpTool::SessionsList | McpTool::TerminalRead { .. } => read_only(),

        // 打开会话不改远端任何东西，但它**花掉一份凭据**并建立一条新的活连接。
        // 定 Write 而不是 Dangerous 有一个具体后果值得说清：`ReadOnlyOnly` 档下
        // `decide(Write, ReadOnlyOnly) == Deny`——只读档的用户不会因为装了个
        // 外部 MCP 客户端就能让它替自己开新会话。这正是想要的。
        McpTool::SessionsOpen { .. } => one(
            RULE_SESSIONS_OPEN,
            Tier::Write,
            "打开新会话会使用本机保险箱里的凭据，并建立一条新的活连接",
        ),

        McpTool::TerminalSend { payload, .. } => classify_send(payload),

        // 原始控制序列恒 Dangerous。这里**不看 payload**：`send_raw` 的语义
        // 就是「我要发的是控制序列」，看一眼内容再决定等于给它留一条
        // 「内容看着人畜无害」的降级路径，而那正是它与 `terminal.send`
        // 分成两个工具的理由（工具名是授权白名单的键）。
        McpTool::TerminalSendRaw { .. } => one(
            RULE_SEND_RAW,
            Tier::Dangerous,
            "原始终端控制序列：策略引擎无法判断它对终端与远端 shell 的实际作用",
        ),

        // 内联内容写远端：只有**远端目的路径**参与分级。
        // 与 Agent 的 `sftp_put` 不是同一个操作（那边收本地源路径、双向取严），
        // 所以这里不借道 Agent 闸门——名字对上而语义不对上比不对名字更坏。
        McpTool::SftpWrite { path, .. } => fs_policy::classify_transfer(path),
    };

    McpGated {
        tool_name: tool.name(),
        verdict,
        caller: caller.to_string(),
    }
}

/// 裁决。就是 `fs_policy::gate::decide`，一行。
///
/// 保留这个薄包装是为了让 `lib.rs` 的自守测试有一个可钉的名字，
/// 并让「MCP 面的裁决没有自己的表」这件事在代码里可见。
pub fn decide_for_mcp(g: &McpGated, mode: AiMode) -> Decision {
    gate::decide(g.verdict.tier, mode)
}

/// 拒绝时给人看的理由列表：分类理由在前，闸门理由在最后一条。
///
/// 顺序与 `fs_ai::agent::gate::GateOutcome::Deny` 一致（先具体后总括）。
pub fn deny_details(g: &McpGated, reason: DenyReason) -> Vec<String> {
    let mut v: Vec<String> = g.verdict.reasons.iter().map(|r| r.detail.clone()).collect();
    v.push(reason.message().to_string());
    v
}

/// 命中的规则 id 列表，进审计行。
pub fn rules_of(g: &McpGated) -> Vec<&'static str> {
    g.verdict.reasons.iter().map(|r| r.rule).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ReadSource;

    fn send(p: &str) -> McpGated {
        classify_for_mcp(
            &McpTool::TerminalSend {
                session_id: "s".into(),
                payload: p.into(),
            },
            "test",
        )
    }

    /// 「与内置 Agent 同一闸门」——三个同义工具的分级结论必须逐字相同。
    ///
    /// 这条**由 `TOOL_MANIFEST.mcp_wire` 驱动**：Agent 侧哪天再填一个 `mcp_wire`，
    /// 这里就会因为查不到映射而红，逼人显式回答「这两个真是同一个操作吗」。
    /// 写死三条的话，第四条会悄悄地没有对账。
    #[test]
    fn tools_that_claim_to_be_the_same_operation_classify_identically() {
        use fs_ai::agent::tool::Tool as A;
        // 每对：同一个操作在两个面上的表达。
        let pairs: Vec<(&str, McpTool, A)> = vec![
            (
                "command.run",
                McpTool::CommandRun {
                    session_id: "s".into(),
                    command: "rm -rf /".into(),
                },
                A::RunCommand {
                    command: "rm -rf /".into(),
                    mode: fs_ai::exec::ExecMode::Exec,
                },
            ),
            (
                "sftp.read",
                McpTool::SftpRead {
                    session_id: "s".into(),
                    path: "/etc/passwd".into(),
                    max_bytes: None,
                },
                A::ReadRemoteFile {
                    path: "/etc/passwd".into(),
                    max_bytes: None,
                },
            ),
            (
                "sftp.list",
                McpTool::SftpList {
                    session_id: "s".into(),
                    path: "/root".into(),
                },
                A::ListRemoteDir {
                    path: "/root".into(),
                },
            ),
        ];

        // 清单侧对账：mcp_wire 填了的，这里必须有一对。
        let claimed: Vec<&str> = fs_ai::agent::tool::TOOL_MANIFEST
            .iter()
            .filter_map(|m| m.mcp_wire)
            .collect();
        let covered: Vec<&str> = pairs.iter().map(|(n, _, _)| *n).collect();
        for c in &claimed {
            assert!(
                covered.contains(c),
                "Agent 清单声称 {c} 与 MCP 同义，但这条测试里没有对应的分类对账"
            );
        }
        assert_eq!(claimed.len(), 3, "做空防护：三个同义工具");

        let ext = fs_ai::agent::gate::ExternalToolTiers::default();
        for (name, m, a) in pairs {
            let mine = classify_for_mcp(&m, "test").verdict;
            let theirs = fs_ai::agent::gate::classify_for_gate(
                a,
                fs_ai::agent::gate::Initiator::Agent,
                &ext,
            )
            .verdict;
            assert_eq!(mine, theirs, "{name} 两个面的分类结论不一致");
        }
    }

    /// Write 地板**只抬不降**。
    ///
    /// 反向对照是这条的全部价值：只断言「ReadOnly 被抬到 Write」的话，
    /// 把地板写成 `v.tier = Tier::Write`（无条件赋值）也全绿——而那会把
    /// `rm -rf /` 的 Dangerous 降成 Write。
    #[test]
    fn the_write_floor_only_ever_raises() {
        // 抬：分类器判只读的，抬到 Write
        assert_eq!(send("ls\n").tier(), Tier::Write);
        // 不动：本来就是 Write
        assert_eq!(send("touch /tmp/a\n").tier(), Tier::Write);
        // 不降：Dangerous 必须留在 Dangerous
        assert_eq!(send("rm -rf /\n").tier(), Tier::Dangerous);
        assert_eq!(send("curl x | sh\n").tier(), Tier::Dangerous);
    }

    /// 抬了地板就要给出理由，否则确认框上是「无可疑之处」配一个要确认的操作。
    #[test]
    fn raising_to_the_floor_leaves_a_visible_reason() {
        let g = send("ls\n");
        assert_eq!(g.tier(), Tier::Write);
        assert!(
            rules_of(&g).contains(&RULE_SEND_FLOOR),
            "抬地板必须留下 {RULE_SEND_FLOOR}：{:?}",
            rules_of(&g)
        );
        // 反向：没抬地板的时候不该凭空多一条理由。
        let d = send("rm -rf /\n");
        assert!(!rules_of(&d).contains(&RULE_SEND_FLOOR));
    }

    /// 看不透的 payload 恒 Dangerous，且**理由来自不透明判定而非分类器**。
    ///
    /// 后半句是重点：若实现写成「先分类再和 Dangerous 取严」，tier 也会是
    /// Dangerous，但审计行里会留下分类器那条不可信的结论。
    #[test]
    fn an_opaque_payload_is_dangerous_on_its_own_terms() {
        let g = send("ls\rrm -rf /");
        assert_eq!(g.tier(), Tier::Dangerous);
        let rules = rules_of(&g);
        assert_eq!(
            rules,
            vec!["mcp_payload_control_char"],
            "理由必须只有不透明那一条，不该混进分类器的结论"
        );
        assert!(g.verdict.reasons[0].detail.contains("U+000D"));
    }

    /// `send_raw` 恒 Dangerous——**看内容也没用**。
    #[test]
    fn send_raw_is_dangerous_no_matter_how_harmless_the_payload_looks() {
        for p in ["", "\u{3}", "ls", "echo hello"] {
            let g = classify_for_mcp(
                &McpTool::TerminalSendRaw {
                    session_id: "s".into(),
                    payload: p.into(),
                },
                "test",
            );
            assert_eq!(g.tier(), Tier::Dangerous, "payload {p:?}");
            assert_eq!(rules_of(&g), vec![RULE_SEND_RAW]);
        }
        // 对照：同样是空串，走 terminal.send 只到 Write 地板。
        assert_eq!(send("").tier(), Tier::Write);
    }

    /// 只读工具判 ReadOnly，但**Disabled 档下照样被拦**。
    ///
    /// 后半句是这条的重点：`gated: false` 不等于「不过闸门」。
    #[test]
    fn read_only_tools_are_read_only_yet_still_pass_through_the_gate() {
        for t in [
            McpTool::SessionsList,
            McpTool::TerminalRead {
                session_id: "s".into(),
                source: ReadSource::All,
                max_lines: None,
            },
        ] {
            let g = classify_for_mcp(&t, "test");
            assert_eq!(g.tier(), Tier::ReadOnly, "{}", g.tool_name);
            assert_eq!(decide_for_mcp(&g, AiMode::WithConfirm), Decision::AutoRun);
            assert_eq!(decide_for_mcp(&g, AiMode::ReadOnlyOnly), Decision::AutoRun);
            assert_eq!(
                decide_for_mcp(&g, AiMode::Disabled),
                Decision::Deny(DenyReason::GloballyDisabled),
                "{} 在总开关关闭时必须被拦",
                g.tool_name
            );
        }
    }

    /// `sessions.open` 定 Write 的那个**具体后果**：只读档下开不了新会话。
    #[test]
    fn sessions_open_is_denied_under_read_only_mode() {
        let g = classify_for_mcp(
            &McpTool::SessionsOpen {
                profile_id: "p".into(),
            },
            "test",
        );
        assert_eq!(g.tier(), Tier::Write);
        assert_eq!(
            decide_for_mcp(&g, AiMode::ReadOnlyOnly),
            Decision::Deny(DenyReason::ModeAllowsReadOnlyOnly)
        );
        assert_eq!(decide_for_mcp(&g, AiMode::WithConfirm), Decision::Confirm);
        assert!(rules_of(&g).contains(&RULE_SESSIONS_OPEN));
    }

    /// `sftp.write` 按**远端目的路径**分级，敏感路径升危。
    #[test]
    fn sftp_write_is_graded_by_its_remote_destination() {
        let mk = |p: &str| {
            classify_for_mcp(
                &McpTool::SftpWrite {
                    session_id: "s".into(),
                    path: p.into(),
                    content: "x".into(),
                },
                "test",
            )
        };
        assert_eq!(mk("/tmp/note.txt").tier(), Tier::Write);
        assert_eq!(mk("/etc/passwd").tier(), Tier::Dangerous);
        // 内容不参与分级——写 `rm -rf /` 这几个字进一个普通文件不是危险操作。
        let a = mk("/tmp/a.sh");
        let b = classify_for_mcp(
            &McpTool::SftpWrite {
                session_id: "s".into(),
                path: "/tmp/a.sh".into(),
                content: "rm -rf /".into(),
            },
            "test",
        );
        assert_eq!(a.verdict, b.verdict, "内容不该影响分级");
    }

    /// 调用方标签完全不参与分级——换个标签再试一次不会得到不同结论。
    #[test]
    fn the_caller_label_never_changes_the_verdict() {
        let mk = |c: &str| {
            classify_for_mcp(
                &McpTool::CommandRun {
                    session_id: "s".into(),
                    command: "rm -rf /".into(),
                },
                c,
            )
        };
        let a = mk("claude-desktop");
        let b = mk("trusted");
        let c = mk("");
        assert_eq!(a.verdict, b.verdict);
        assert_eq!(a.verdict, c.verdict);
        // 但标签本身要原样留下来，审计要用。
        assert_eq!(a.caller, "claude-desktop");
    }

    /// 九个工具全覆盖：每一支都能分类，且 tool_name 与清单对得上。
    #[test]
    fn every_tool_variant_can_be_classified() {
        let all = [
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
                payload: "ls\n".into(),
            },
            McpTool::TerminalSendRaw {
                session_id: "s".into(),
                payload: "\u{3}".into(),
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
        assert_eq!(all.len(), crate::tool::MCP_MANIFEST.len());
        for t in &all {
            let g = classify_for_mcp(t, "test");
            assert_eq!(g.tool_name, t.name());
            assert!(crate::tool::manifest_of(g.tool_name).is_some());
        }
    }

    /// ⚠ 标记与实际分级的对账。
    ///
    /// **⚠ 不等于「永远要人批」**——它的意思是「经策略引擎」，即结论取决于
    /// 参数内容。`command.run ls` 在 WithConfirm 下自动放行正是分级闸门存在的
    /// 意义；要求它也弹框，就是把确认训练成一个照点不误的动作。
    ///
    /// 所以对账写成两条**都能失败**的不变式：
    /// - 没标 ⚠ 的工具是**结构性只读**：换任何参数都到不了需要人的档位；
    /// - 标了 ⚠ 的工具**够得着**需要人的档位：每个都给一个见证输入。
    ///
    /// 只写前一条，把所有工具都标 ⚠ 能过；只写后一条，把只读工具也标 ⚠ 能过。
    #[test]
    fn the_warning_marks_match_what_the_gate_actually_does() {
        // ① 没标 ⚠ 的：换什么参数都是 ReadOnly。
        let free: Vec<(&str, Vec<McpTool>)> = vec![
            ("sessions.list", vec![McpTool::SessionsList]),
            (
                "terminal.read",
                [ReadSource::Screen, ReadSource::Scrollback, ReadSource::All]
                    .into_iter()
                    .map(|source| McpTool::TerminalRead {
                        session_id: "s".into(),
                        source,
                        max_lines: Some(999_999),
                    })
                    .collect(),
            ),
            (
                "sftp.list",
                ["/", "/etc", "/root/.ssh", "~"]
                    .into_iter()
                    .map(|p| McpTool::SftpList {
                        session_id: "s".into(),
                        path: p.into(),
                    })
                    .collect(),
            ),
            (
                "sftp.read",
                ["/etc/shadow", "/root/.ssh/id_rsa", "/f"]
                    .into_iter()
                    .map(|p| McpTool::SftpRead {
                        session_id: "s".into(),
                        path: p.into(),
                        max_bytes: None,
                    })
                    .collect(),
            ),
        ];
        for (name, variants) in &free {
            assert!(!crate::tool::manifest_of(name).unwrap().gated);
            assert!(!variants.is_empty(), "做空防护：{name} 没有变体");
            for t in variants {
                let g = classify_for_mcp(t, "test");
                assert_eq!(
                    g.tier(),
                    Tier::ReadOnly,
                    "{name} 没标 ⚠，却在某个参数下升到了 {:?}",
                    g.tier()
                );
                assert_eq!(
                    decide_for_mcp(&g, AiMode::WithConfirm),
                    Decision::AutoRun,
                    "{name} 没标 ⚠ 却需要人"
                );
            }
        }

        // ② 标了 ⚠ 的：每个都给一个够得着「需要人」的见证输入。
        let gated: Vec<(&str, McpTool)> = vec![
            (
                "sessions.open",
                McpTool::SessionsOpen {
                    profile_id: "p".into(),
                },
            ),
            (
                "terminal.send",
                McpTool::TerminalSend {
                    session_id: "s".into(),
                    payload: "ls\n".into(),
                },
            ),
            (
                "terminal.send_raw",
                McpTool::TerminalSendRaw {
                    session_id: "s".into(),
                    payload: "x".into(),
                },
            ),
            (
                "command.run",
                McpTool::CommandRun {
                    session_id: "s".into(),
                    command: "rm -rf /".into(),
                },
            ),
            (
                "sftp.write",
                McpTool::SftpWrite {
                    session_id: "s".into(),
                    path: "/etc/passwd".into(),
                    content: "x".into(),
                },
            ),
        ];
        for (name, t) in &gated {
            assert!(crate::tool::manifest_of(name).unwrap().gated);
            let d = decide_for_mcp(&classify_for_mcp(t, "test"), AiMode::WithConfirm);
            assert!(d.needs_user(), "{name} 标了 ⚠ 却够不着需要人的档位：{d:?}");
        }

        // ③ 做空防护 + 与清单对账：两组加起来正好是整张清单。
        assert_eq!(free.len(), 4);
        assert_eq!(gated.len(), 5);
        let covered: std::collections::BTreeSet<&str> = free
            .iter()
            .map(|(n, _)| *n)
            .chain(gated.iter().map(|(n, _)| *n))
            .collect();
        let all: std::collections::BTreeSet<&str> =
            crate::tool::MCP_MANIFEST.iter().map(|m| m.name).collect();
        assert_eq!(covered, all, "有工具没被这条对账覆盖");
    }

    /// ⚠ 工具在**无害参数**下确实会自动放行——分级不是摆设。
    ///
    /// 与上一条是一对：那条防「该问的不问」，这条防「不该问的乱问」。
    /// 后者不是小事：每一次多余的确认都在教用户闭眼点确认。
    #[test]
    fn a_gated_tool_with_harmless_arguments_still_runs_without_asking() {
        let harmless = [
            McpTool::CommandRun {
                session_id: "s".into(),
                command: "ls -la /var/log".into(),
            },
            McpTool::CommandRun {
                session_id: "s".into(),
                command: "df -h".into(),
            },
        ];
        for t in &harmless {
            assert_eq!(
                decide_for_mcp(&classify_for_mcp(t, "test"), AiMode::WithConfirm),
                Decision::AutoRun,
                "{:?} 不该需要确认",
                t
            );
        }
        // 但 sftp.write / terminal.send / send_raw / sessions.open 有地板，
        // **没有**「无害参数」这一说——它们的地板就是拿掉这条豁免。
        for t in [
            McpTool::SftpWrite {
                session_id: "s".into(),
                path: "/tmp/harmless.txt".into(),
                content: "hello".into(),
            },
            McpTool::TerminalSend {
                session_id: "s".into(),
                payload: "echo hi\n".into(),
            },
        ] {
            assert!(
                decide_for_mcp(&classify_for_mcp(&t, "test"), AiMode::WithConfirm).needs_user(),
                "{:?} 有地板，不该因为参数无害就自动放行",
                t
            );
        }
    }

    /// 强确认恰好落在两个地方：`send_raw`（恒危）与分类器判危的内容。
    #[test]
    fn strong_confirm_is_reserved_for_dangerous() {
        let raw = classify_for_mcp(
            &McpTool::TerminalSendRaw {
                session_id: "s".into(),
                payload: "x".into(),
            },
            "t",
        );
        assert_eq!(
            decide_for_mcp(&raw, AiMode::WithConfirm),
            Decision::StrongConfirm
        );
        let danger = classify_for_mcp(
            &McpTool::CommandRun {
                session_id: "s".into(),
                command: "rm -rf /".into(),
            },
            "t",
        );
        assert_eq!(
            decide_for_mcp(&danger, AiMode::WithConfirm),
            Decision::StrongConfirm
        );
        // 对照：普通写操作只要单次确认。
        assert_eq!(
            decide_for_mcp(&send("touch /tmp/a\n"), AiMode::WithConfirm),
            Decision::Confirm
        );
    }

    /// 拒绝理由：分类理由在前，闸门那句总括在最后。
    #[test]
    fn deny_details_put_the_gate_message_last() {
        let g = classify_for_mcp(
            &McpTool::SessionsOpen {
                profile_id: "p".into(),
            },
            "t",
        );
        let d = deny_details(&g, DenyReason::ModeAllowsReadOnlyOnly);
        assert!(d.len() >= 2);
        assert_eq!(
            d.last().unwrap(),
            DenyReason::ModeAllowsReadOnlyOnly.message()
        );
        assert!(d[0].contains("凭据"));
    }
}
