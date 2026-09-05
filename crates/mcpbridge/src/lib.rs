//! fs_mcpbridge — MCP Server（对外）+ MCP Client（内置挂载）（总设计 §4.5）。
//!
//! # 这个 crate 的一句话职责
//!
//! 把第三方 MCP 客户端（Claude Desktop / Claude Code / Cursor）发来的工具调用，
//! **过与内置 Agent 完全相同的那一道闸门**，然后交给同一份 `run_command` 契约执行。
//!
//! 「完全相同」是字面意思，不是「口径一致」：`command.run` / `sftp.read` /
//! `sftp.list` 三个工具的分级是把 [`tool::McpTool`] 映射成
//! `fs_ai::agent::tool::Tool` 之后调 `fs_ai::agent::gate::classify_for_gate`——
//! 同一个函数、同一份规则表。两套「口径一致」的实现会各自漂移，而漂移的方向
//! 从来是宽的那边赢：谁先放行谁就没人投诉。
//!
//! # 三条结构性事实（不是靠人记得遵守的约定）
//!
//! - **Vault 不可表达**：[`tool::McpTool`] 枚举里没有那一支，`Cargo.toml` 里
//!   没有 `fs_vault` 这条依赖边。想暴露 Vault 得先改这两处。
//! - **传输没有监听地址**：stdio 是 M3 阶段唯一传输。本 crate 不声明任何
//!   HTTP 客户端或服务端框架，由 [`self_guard`] 与 `egress-contract.test.ts`
//!   的 MCP 专章两头守。
//! - **不得静默放行**：[`reject`] 里没有任何一条路径能构造出「放行」，
//!   它只表达拒绝。放行走的是另一条类型完全不同的路。

pub mod authz;
pub mod client;
pub mod confirm;
pub mod gate;
pub mod payload;
pub mod ports;
pub mod reject;
pub mod server;
pub mod tool;
pub mod transport;

#[cfg(test)]
mod self_guard {
    //! grep 自守网（先例：`fs_ai::agent::mod.rs` 的 T-01 自守）。
    //!
    //! 与那一份的区别在于本 crate 多守一件事：**这里一行出站代码都没有**。
    //! 本 crate 传递地依赖 `fs_ai`（因而传递地含 reqwest），依赖边挡不住，
    //! 只能逐文件扫源码。

    use std::path::Path;

    fn sources() -> Vec<(String, String)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        for e in std::fs::read_dir(&dir).expect("src 必须在") {
            let p = e.expect("read_dir").path();
            if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                let s = std::fs::read_to_string(&p).expect("读源码");
                out.push((p.file_name().unwrap().to_string_lossy().to_string(), s));
            }
        }
        assert!(
            out.len() >= 4,
            "src 下应有至少 4 个 .rs，只看到 {}：扫查坏了",
            out.len()
        );
        out
    }

    /// 剥注释——让注释给断言作证是守卫的第一失效模式。
    fn code(src: &str) -> String {
        src.lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 生产代码 = 剥掉 `#[cfg(test)]` 之后的段。
    fn production(src: &str) -> String {
        match src.find("#[cfg(test)]") {
            Some(i) => src[..i].to_string(),
            None => src.to_string(),
        }
    }

    /// 本 crate 不说 HTTP，也不开监听。
    ///
    /// 禁词按拼接构造，免得本文件自己命中（同仓惯例）。
    #[test]
    fn this_crate_never_speaks_http() {
        let banned: Vec<String> = vec![
            "req".to_string() + "west",
            "hy".to_string() + "per",
            "ax".to_string() + "um",
            "Tcp".to_string() + "Listener",
            "Tcp".to_string() + "Stream",
            "ht".to_string() + "tp://",
            "ht".to_string() + "tps://",
            "127.0".to_string() + ".0.1",
            "0.0".to_string() + ".0.0",
            "::".to_string() + "bind",
        ];
        let all = sources();
        for (name, src) in &all {
            let c = code(src);
            for b in &banned {
                assert!(
                    !c.contains(b.as_str()),
                    "{name} 里出现了 {b}——本 crate 的传输面只有 stdio，没有地址"
                );
            }
        }
        assert!(!all.is_empty(), "做空防护");
    }

    /// 本 crate 不碰 Vault。依赖边已经挡了一层，这里挡「有人加了依赖边」。
    #[test]
    fn this_crate_never_touches_the_vault() {
        let manifest =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                .expect("读 Cargo.toml");
        let banned = "fs_".to_string() + "vault";
        assert!(
            !manifest.contains(&banned),
            "Cargo.toml 里出现了 {banned}——「Vault 永不以任何 MCP 工具形式暴露」"
        );
        for (name, src) in sources() {
            assert!(!code(&src).contains(&banned), "{name} 里出现了 {banned}");
        }
    }

    /// 正向钉调用点：fs_policy 的每个咨询面在本 crate 里**恰一个**生产调用点。
    ///
    /// 这条与 `fs_ai::agent` 那一份是对偶的：那边钉住 Agent 面不长出第二个闸门，
    /// 这边钉住 MCP 面不长出第三个。
    #[test]
    fn every_policy_surface_has_exactly_one_production_call_site_in_this_crate() {
        let all = sources();
        let prod: Vec<(String, String)> = all
            .iter()
            .map(|(n, s)| (n.clone(), production(&code(s))))
            .collect();
        let files_with = |needle: &str| -> Vec<String> {
            let mut v: Vec<String> = prod
                .iter()
                .filter(|(_, c)| c.contains(needle))
                .map(|(n, _)| n.clone())
                .collect();
            v.sort();
            v
        };

        // 分类只在 gate.rs 发生——无论是自己调 fs_policy，还是转调 fs_ai 的闸门。
        for needle in [
            "fs_policy::classify",
            "classify_for_gate",
            "classify_transfer",
        ] {
            let f = files_with(needle);
            assert!(
                f.is_empty() || f == vec!["gate.rs".to_string()],
                "{needle} 的生产调用点必须唯一（gate.rs），实测 {f:?}"
            );
        }

        // 裁决只在 gate.rs：`decide` 与 `gate_check` 是同一件事的两个层次。
        for needle in ["gate::decide", "gate_check"] {
            let f = files_with(needle);
            assert!(
                f.is_empty() || f == vec!["gate.rs".to_string()],
                "{needle} 的生产调用点必须唯一（gate.rs），实测 {f:?}"
            );
        }

        // 确认收敛只在 confirm.rs。
        let f = files_with("gate::resolve");
        assert!(
            f.is_empty() || f == vec!["confirm.rs".to_string()],
            "resolve 的生产调用点必须唯一（confirm.rs），实测 {f:?}"
        );
    }

    /// 模型/调用方自报分级在本 crate 里无处安放。
    ///
    /// MCP 面比 Agent 面更需要这条：Agent 那边的「模型」至少是我们自己配的
    /// provider，MCP 这边的调用方是**任何能以同一 OS 用户启动本二进制的本地
    /// 进程**（§4.5 信任边界）。它自报的任何风险声明都不能进裁决路径。
    #[test]
    fn no_caller_supplied_risk_assessment_can_enter_the_decision_path() {
        let banned: Vec<String> = vec![
            "caller_tier".to_string(),
            "self_assessed".to_string(),
            "\"".to_string() + "SA" + "FE\"",
            "client_says".to_string(),
            "trusted_caller".to_string(),
        ];
        for (name, src) in sources() {
            let c = production(&code(&src));
            for b in &banned {
                assert!(!c.contains(b.as_str()), "{name} 生产代码出现了 {b}");
            }
        }
    }
}
