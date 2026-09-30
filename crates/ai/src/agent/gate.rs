//! fs_policy 咨询面收拢在一个文件里。
//!
//! `classify` / `classify_transfer` 的生产调用只允许出现在 [`classify_for_gate`]，
//! `gate::decide` 的生产调用只允许出现在 [`gate_check`]——grep 自守网（mod.rs 的
//! T-01）按「唯一调用点」钉死。**契约有两个实现就等于没有契约**；而第二个实现
//! 出现的方式从来不是有人故意写一份，而是有人在别处「顺手」调了一下。

use crate::agent::tool::Tool;
use fs_policy::{gate, AiMode, Decision, DenyReason, Tier, Verdict};

pub enum Initiator {
    Agent,
    Mcp { caller: String },
}

pub struct Gated {
    pub tool: Tool,
    /// 永远出自本地 fs_policy。`Gated` 没有「接受一个现成 Tier」的构造路径——
    /// 模型自报的分级在类型上无处安放。
    pub verdict: Verdict,
    pub initiator: Initiator,
}

/// 外部 MCP 工具的风险地板配置（工具级，只能向严调整）。
///
/// §4.5 的「可以在工具级配置中提级」按**提到更高风险**读：地板只升不降。
#[derive(Default)]
pub struct ExternalToolTiers {
    /// `(server_id, tool)` → 已评估的 Tier；查无 ⇒ Write 地板。
    floors: std::collections::HashMap<(String, String), Tier>,
}

impl ExternalToolTiers {
    /// 外部工具不匹配 shell 白名单，基线 `Write`（单次确认起步，§4.5 原文）。
    /// 已评估条目只能 **`.max()` 提级**——`min` 那个方向（降级）在类型上做不到。
    pub fn floor_at_write(&self, server_id: &str, tool: &str) -> Verdict {
        let floor = self
            .floors
            .get(&(server_id.to_string(), tool.to_string()))
            .copied()
            .unwrap_or(Tier::Write);
        Verdict {
            tier: floor.max(Tier::Write),
            reasons: vec![],
        }
    }
}

/// 全仓唯一的工具分类组合点。
pub fn classify_for_gate(tool: Tool, initiator: Initiator, _ext: &ExternalToolTiers) -> Gated {
    let verdict = match &tool {
        // 不重造白名单——fs_policy 的语料库（269 条 + CI 召回率断言）是它的资产。
        Tool::RunCommand { command, .. } => fs_policy::classify(command),
        Tool::SftpGet { local_dest, .. } => fs_policy::classify_transfer(local_dest),
        Tool::SftpPut {
            local_src,
            remote_dest,
        } => {
            // 双向取严：Tier 的 Ord 即严重度（ReadOnly<Write<Dangerous），max 取严。
            // 只组合 classify_transfer 既有结论，绝不重写路径判断
            // （paths 的 `/etc/../etc/passwd` 规范化绕过教训就在那儿）。
            let (mut a, mut b) = (
                fs_policy::classify_transfer(local_src),
                fs_policy::classify_transfer(remote_dest),
            );
            a.tier = a.tier.max(b.tier);
            a.reasons.append(&mut b.reasons);
            a
        }
        // 读工具恒 ReadOnly（实现只会读），但**仍要过 gate_check**——
        // Disabled 下读工具同样被拦（变体注释里的理由）。
        Tool::ReadRemoteFile { .. }
        | Tool::ListRemoteDir { .. }
        | Tool::AskUser { .. }
        | Tool::Finish { .. } => Verdict {
            tier: Tier::ReadOnly,
            reasons: Vec::new(),
        },
        Tool::ExternalMcp {
            server_id, tool, ..
        } => _ext.floor_at_write(server_id, tool),
    };
    Gated {
        tool,
        verdict,
        initiator,
    }
}

pub enum GateOutcome {
    Auto,
    /// `decide()==Confirm` → false；`StrongConfirm` → true（长按/二次输入）。
    Confirm {
        strong: bool,
    },
    Deny {
        reason: DenyReason,
        /// `fs_policy::rules::RULE_*` 稳定 id，给机器。
        rules: Vec<&'static str>,
        /// `Reason.detail` 中文原句 + `DenyReason::message()`，给人。
        details: Vec<String>,
    },
}

/// `fs_policy::gate::decide` 的全仓唯一生产调用点。3×3 裁决表唯一持有者仍是 gate.rs。
pub fn gate_check(g: &Gated, mode: AiMode) -> GateOutcome {
    match gate::decide(g.verdict.tier, mode) {
        Decision::AutoRun => GateOutcome::Auto,
        Decision::Confirm => GateOutcome::Confirm { strong: false },
        Decision::StrongConfirm => GateOutcome::Confirm { strong: true },
        Decision::Deny(reason) => GateOutcome::Deny {
            reason,
            rules: g.verdict.reasons.iter().map(|r| r.rule).collect(),
            details: {
                let mut v: Vec<String> =
                    g.verdict.reasons.iter().map(|r| r.detail.clone()).collect();
                v.push(reason.message().to_string());
                v
            },
        },
    }
}

/// 档位合成：会话侧 `None` = 未表态 = 不覆盖。
///
/// 注意 `AiMode` 的 Ord 方向与 Tier 相反：stricter == smaller，「只紧不松」是
/// `apply_session_override` 封装的比较——绝不在这里手写 `min`/`max`，
/// 方向写反一次就是全局放松。
pub fn effective_mode(global: AiMode, session: Option<AiMode>) -> AiMode {
    session
        .map(|s| gate::apply_session_override(global, s))
        .unwrap_or(global)
}

/// `Decision` 的四个 IPC 字面量（从 ai_cmd.rs 抽成共享函数——前端在按这四个
/// 连字符串匹配，两处各写一份迟早写错一处处）：
/// `"auto" | "confirm" | "strong-confirm" | "deny"`。
pub fn decision_wire_str(d: Decision) -> &'static str {
    match d {
        Decision::AutoRun => "auto",
        Decision::Confirm => "confirm",
        Decision::StrongConfirm => "strong-confirm",
        Decision::Deny(_) => "deny",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tool::parse_tool;
    use fs_policy::AiMode;

    fn gated_cmd(cmd: &str) -> Gated {
        let tool = parse_tool("run_command", &format!("{{\"command\":{cmd:?}}}")).unwrap();
        classify_for_gate(tool, Initiator::Agent, &ExternalToolTiers::default())
    }

    /// **工具 × 模式全积表**（每类工具 × 三档 AiMode 的期望 GateOutcome）。
    ///
    /// 零 fake、纯查表。任何一格 match 臂写错 ⇒ 恰一格红——
    /// 这正是把分类收拢到一个文件里的报酬。
    #[test]
    fn the_full_tool_by_mode_cartesian_table() {
        /// (期望结局, strong)：结局三选，strong 只在 confirm 档有意义。
        struct Want(&'static str, bool);
        /// 一行 = 一类工具 × 三档的期望。
        struct Row(&'static str, &'static str, [(AiMode, Want); 3]);
        let cases: Vec<Row> = vec![
            Row(
                "只读命令",
                "df -h",
                [
                    (AiMode::WithConfirm, Want("auto", false)),
                    (AiMode::ReadOnlyOnly, Want("auto", false)),
                    (AiMode::Disabled, Want("deny", false)),
                ],
            ),
            Row(
                "写命令",
                "touch /tmp/x",
                [
                    (AiMode::WithConfirm, Want("confirm", false)),
                    (AiMode::ReadOnlyOnly, Want("deny", false)),
                    (AiMode::Disabled, Want("deny", false)),
                ],
            ),
            Row(
                "危险命令",
                "rm -rf /",
                [
                    (AiMode::WithConfirm, Want("confirm", true)),
                    (AiMode::ReadOnlyOnly, Want("deny", false)),
                    (AiMode::Disabled, Want("deny", false)),
                ],
            ),
        ];
        for Row(label, cmd, rows) in cases {
            for (mode, want) in rows {
                let g = gated_cmd(cmd);
                let got = match gate_check(&g, mode) {
                    GateOutcome::Auto => "auto".to_string(),
                    GateOutcome::Confirm { strong: s } => {
                        format!("confirm{}", if s { "+strong" } else { "" })
                    }
                    GateOutcome::Deny { .. } => "deny".to_string(),
                };
                let want_full = if want.0 == "confirm" && want.1 {
                    "confirm+strong".to_string()
                } else {
                    want.0.to_string()
                };
                assert_eq!(
                    got,
                    want_full,
                    "{label} × {}: 期望 {want_full} 得 {got}",
                    mode.as_str()
                );
            }
        }
    }

    /// **Disabled 连只读工具都拦**——读工具恒 ReadOnly 仍过 gate 的理由就在这条。
    #[test]
    fn disabled_denies_even_read_tools() {
        for name_args in [
            ("read_remote_file", r#"{"path":"/etc/hosts"}"#),
            ("list_remote_dir", r#"{"path":"/"}"#),
            ("ask_user", r#"{"question":"?"}"#),
            ("finish", r#"{"summary":"done"}"#),
        ] {
            let tool = parse_tool(name_args.0, name_args.1).unwrap();
            let g = classify_for_gate(tool, Initiator::Agent, &ExternalToolTiers::default());
            // 总开关关着 = 一切 AI 发起的动作不可用（出口第 8 项原文口径）。
            assert!(
                matches!(gate_check(&g, AiMode::Disabled), GateOutcome::Deny { .. }),
                "{} 在 Disabled 下没被拦",
                name_args.0
            );
        }
    }

    /// sftp_put 双向取严。
    #[test]
    fn sftp_put_takes_the_stricter_of_both_sides() {
        let mk = |local: &str, remote: &str| {
            let tool = parse_tool(
                "sftp_put",
                &format!(r#"{{"local_src":{local:?},"remote_dest":{remote:?}}}"#),
            )
            .unwrap();
            classify_for_gate(tool, Initiator::Agent, &ExternalToolTiers::default())
                .verdict
                .tier
        };
        // 双普通路径：at least Write（传输不可能是 ReadOnly——classify_transfer 对
        // 普通路径给什么由它定，这里只钉「不是 ReadOnly」这个方向）
        assert_ne!(mk("/tmp/a.txt", "/tmp/b.txt"), Tier::ReadOnly);
        // 远端敏感 ⇒ Dangerous。用 `~` 家相对路径：classify_transfer 的敏感判定
        // 以 ~/$HOME 为锚——绝对路径的 /root 是不是某人的 home 它不猜
        // （猜错方向会把安全路径判危险、危险路径判安全，两个错都不轻）。
        assert_eq!(mk("/tmp/a.txt", "~/.ssh/authorized_keys"), Tier::Dangerous);
        // 本地敏感外传 ⇒ Dangerous
        assert_eq!(mk("~/.ssh/id_rsa", "/tmp/b.txt"), Tier::Dangerous);
    }

    /// 外部 MCP 工具的 Write 地板：未评估 → Write；已评估 Dangerous → Dangerous；
    /// **已评估 ReadOnly 也压不下去**（地板只升不降）。
    #[test]
    fn external_mcp_tools_floor_at_write_and_never_below() {
        let mut ext = ExternalToolTiers::default();
        ext.floors
            .insert(("s1".into(), "t1".into()), Tier::Dangerous);
        ext.floors
            .insert(("s1".into(), "t2".into()), Tier::ReadOnly);
        let mk = |ext: &ExternalToolTiers, s: &str, t: &str| {
            let tool = Tool::ExternalMcp {
                server_id: s.into(),
                tool: t.into(),
                arguments_json: "{}".into(),
            };
            classify_for_gate(
                tool,
                Initiator::Mcp {
                    caller: "test".into(),
                },
                ext,
            )
            .verdict
            .tier
        };
        assert_eq!(
            mk(&ext, "s1", "unknown-tool"),
            Tier::Write,
            "未评估 = Write 地板"
        );
        assert_eq!(mk(&ext, "s1", "t1"), Tier::Dangerous);
        // 关键一条：已评估为 ReadOnly 的也**抬回 Write**——外部工具不匹配
        // shell 白名单，「提级」只有提到更高风险这一个方向。
        assert_eq!(mk(&ext, "s1", "t2"), Tier::Write, "地板被降级了");
    }

    /// 档位合成的默认陷阱：`(WithConfirm, None) == WithConfirm`——**绝不 Disabled**。
    #[test]
    fn effective_mode_session_none_means_no_opinion() {
        use fs_policy::AiMode::*;
        // None（未表态）不覆盖：全局是什么就是什么。
        assert_eq!(effective_mode(WithConfirm, None), WithConfirm);
        // 显式表态走 apply_session_override（只紧不松——它的既有测试盖住方向）。
        assert_eq!(
            effective_mode(WithConfirm, Some(ReadOnlyOnly)),
            ReadOnlyOnly
        );
        assert_eq!(effective_mode(WithConfirm, Some(WithConfirm)), WithConfirm);
        // 全局关着就是关着，会话无权打开。
        assert_eq!(effective_mode(Disabled, Some(WithConfirm)), Disabled);
    }

    /// decision_wire_str 四串——前端按字面匹配，写错即静默失配。
    #[test]
    fn decision_wire_strings_are_the_four_the_frontend_matches() {
        assert_eq!(decision_wire_str(Decision::AutoRun), "auto");
        assert_eq!(decision_wire_str(Decision::Confirm), "confirm");
        assert_eq!(decision_wire_str(Decision::StrongConfirm), "strong-confirm");
        assert_eq!(
            decision_wire_str(Decision::Deny(DenyReason::GloballyDisabled)),
            "deny"
        );
    }

    /// Deny 载荷：rules 是 RULE_* id、details 是中文原句 + DenyReason 的指引句。
    #[test]
    fn deny_carries_rule_ids_and_human_details() {
        let g = gated_cmd("rm -rf /");
        if let GateOutcome::Deny {
            reason,
            rules,
            details,
        } = gate_check(&g, AiMode::ReadOnlyOnly)
        {
            assert_eq!(reason, DenyReason::ModeAllowsReadOnlyOnly);
            assert!(!rules.is_empty(), "危险命令该有 rule id");
            // `Reason.rule` 存的是 RULE_* 常量的**值**（如 recursive_delete_at_root_or_home），
            // 不是带 RULE_ 前缀的常量名——第一版断言按前缀判，全错。
            assert!(
                rules.contains(&"recursive_delete_at_root_or_home"),
                "rm -rf / 该带这条 rule：{rules:?}"
            );
            // 给人的那半：理由原句 + 下一步指引（最后一条是 DenyReason::message）
            assert!(details.last().unwrap().contains("仅只读"));
        } else {
            panic!("ReadOnlyOnly 下 rm -rf / 该 Deny");
        }
    }
}
