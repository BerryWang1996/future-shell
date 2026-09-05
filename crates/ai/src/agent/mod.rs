//! M3 Agent 运行时（裁决与不变量：路线图 §6.2（`docs/roadmap.md`）；实现细节以本模块代码为准）。
//!
//! # 单闸门单契约
//!
//! fs_policy 的咨询面收拢在 [`gate`]（`classify_for_gate` / `gate_check`），
//! `exec::run_command` 的调用收拢在 app 层 `exec_port`，`gate::resolve` 收拢在
//! [`confirm::conclude`]，审计写入收拢在 app 层 `agent_audit`。**契约有两个
//! 实现就等于没有契约**——而第二个实现出现的方式从来不是有人故意写一份，
//! 而是有人在别处「顺手」调了一下。文件尾的 grep 自守测试把「唯一调用点」
//! 升格为 CI 不变量：第二个实现没有可被调用的位置。

pub mod budget;
pub mod confirm;
pub mod dispatch;
pub mod gate;
pub mod history;
pub mod ports;
pub mod record;
pub mod run;
pub mod stop;
pub mod tool;

#[cfg(test)]
mod self_guard {
    //! T-01 grep 自守网（先例：ai_cmd.rs 的 no_code_path_reads_a_danger_verdict）。
    //!
    //! 两类断言：
    //! ① 禁词——危险级判定永远不出自模型回包；
    //! ② 正向钉调用点——fs_policy 咨询面的每个入口恰一个生产调用点。
    //! 第二类比第一类强：它防的不是「出现了坏东西」而是「第二个实现有了
    //! 可被调用的位置」。

    use std::path::Path;

    /// 本模块自身的源码（自守测试的注释里不能出现裸禁词——那会让自己红，
    /// 而为了不红去改松断言是最常见的事故链）。所以这里用拼接构造禁词。
    fn agent_sources() -> Vec<(String, String)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agent");
        let mut out = Vec::new();
        for e in std::fs::read_dir(&dir).expect("src/agent 必须在") {
            let p = e.expect("read_dir").path();
            if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                let s = std::fs::read_to_string(&p).expect("读源码");
                out.push((p.file_name().unwrap().to_string_lossy().to_string(), s));
            }
        }
        assert!(
            out.len() >= 9,
            "agent/ 下应有至少 9 个文件，只看到 {}：扫查坏了",
            out.len()
        );
        out
    }

    /// 剥注释（同仓内其他源码扫查的口径：让注释给断言作证是守卫的第一失效模式）。
    fn code(src: &str) -> String {
        src.lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 生产代码 = 剥掉 `#[cfg(test)] mod` 之后的段。粗切但方向对：
    /// 测试里的调用不该被算进「生产调用点」。
    fn production(src: &str) -> String {
        match src.find("#[cfg(test)]") {
            Some(i) => src[..i].to_string(),
            None => src.to_string(),
        }
    }

    #[test]
    fn no_code_path_reads_a_danger_verdict_out_of_the_model_reply() {
        let banned = ["SA", "FE", "DANGER", "RISK"];
        // 逐词拼出完整禁词，避免本文件自己命中。
        let full: Vec<String> = vec![
            format!("\"{}{}\"", banned[0], banned[1]), // "SAFE"
            format!("\"{}:\"", banned[2]),             // "DANGER:"
            format!("\"{}:\"", banned[3]),             // "RISK:"
            "model_tier".to_string(),
            "self_assessed".to_string(),
        ];
        for (name, src) in agent_sources() {
            let c = production(&code(&src));
            for w in &full {
                assert!(
                    !c.contains(w.as_str()),
                    "{name} 生产代码出现了 {w}——危险级判定只能在 fs_policy"
                );
            }
        }
    }

    /// 正向钉调用点：每个收拢面**恰一个**生产调用点。
    #[test]
    fn every_gate_surface_has_exactly_one_production_call_site() {
        let all = agent_sources();
        let prod: Vec<(String, String)> = all
            .iter()
            .map(|(n, s)| (n.clone(), production(&code(s))))
            .collect();
        let count =
            |needle: &str| -> usize { prod.iter().filter(|(_, c)| c.contains(needle)).count() };
        let files_with = |needle: &str| -> Vec<String> {
            prod.iter()
                .filter(|(_, c)| c.contains(needle))
                .map(|(n, _)| n.clone())
                .collect()
        };

        // fs_policy::classify / classify_transfer 只出现在 gate.rs
        assert_eq!(
            count("fs_policy::classify"),
            1,
            "classify 的生产调用点必须唯一（gate.rs）：{:?}",
            files_with("fs_policy::classify")
        );
        assert_eq!(
            files_with("fs_policy::classify"),
            vec!["gate.rs".to_string()]
        );

        // gate::decide 只出现在 gate.rs
        assert_eq!(
            files_with("gate::decide"),
            vec!["gate.rs".to_string()],
            "decide 的生产调用点必须唯一"
        );

        // gate::resolve 只出现在 confirm.rs
        assert_eq!(
            files_with("gate::resolve"),
            vec!["confirm.rs".to_string()],
            "resolve 的生产调用点必须唯一（conclude）"
        );

        // exec::run_command 在 fs_ai 的 agent 模块里**零**生产调用——它的唯一
        // 生产调用点在 app 层 exec_port（app 侧有对偶断言；这里钉住本 crate
        // 不长出第二个）。
        assert!(
            count("run_command(") == 0
                || files_with("run_command(") == vec!["ports.rs".to_string()],
            "run_command 的实现规格调用点在 app 层；agent 模块内只允许 ports.rs 的文档提及"
        );

        // human_touched 的**调用点只允许在 dispatch.rs**（两处：确认被批准、
        // ask_user 被回答）。出现第三处、或出现在别的文件里，这里红——
        // 逼人回答「这个触点真是人给的吗」。把清零钥匙交给模型能自产的事件
        // 等于没有连续上限（budget.rs 那段注释里的威胁模型）。
        let human_callers = files_with("human_touched()");
        assert_eq!(
            human_callers,
            vec!["dispatch.rs".to_string()],
            "human_touched 的调用文件必须只有 dispatch.rs"
        );
        // 且恰两处：确认 Approved 臂 + ask_user Answered 臂
        let dispatch_src = prod
            .iter()
            .find(|(n, _)| n == "dispatch.rs")
            .map(|(_, c)| c.clone())
            .unwrap_or_default();
        assert_eq!(
            dispatch_src.matches("human_touched()").count(),
            2,
            "人触点恰两个（确认批准 / ask_user 被回答）"
        );
    }

    /// manifest 与 schema 的禁字段：模型可写面没有 timeout/max_output——
    /// 预算不由被审者自定（Tool::RunCommand 的变体注释）。
    #[test]
    fn the_model_writable_surface_has_no_budget_fields() {
        for m in crate::agent::tool::TOOL_MANIFEST {
            assert!(
                !m.schema_json.contains("timeout_secs"),
                "{} 的 schema 暴露了 timeout_secs——预算不由被审者自定",
                m.agent_wire
            );
            assert!(
                !m.schema_json.contains("max_output_bytes"),
                "{} 的 schema 暴露了 max_output_bytes",
                m.agent_wire
            );
            // JSON Schema 全部声明 additionalProperties:false（deny_unknown_fields
            // 的文档化面）：模型塞自报字段在协议层就被拒。
            assert!(
                m.schema_json.contains("\"additionalProperties\":false")
                    || m.schema_json.contains("\"additionalProperties\": false"),
                "{} 的 schema 没关 additionalProperties",
                m.agent_wire
            );
        }
    }
}
