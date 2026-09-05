//! 审计链的用户入口（M3 出口第 7 项「快照校验命令可用」的那一半）。
//!
//! # 为什么要有这个文件
//!
//! hash 链本体早就完整：`fs_connmgr::audit_repo::verify_chain`（逐行重算、报出第一处
//! 断点的行 id 与断因）与 `forensics::export` / `verify_json`（取证包导出与不接触
//! 数据库的独立校验）都有实现、都有单测。但它们此前**只有库函数与 #[test]**——
//! `invoke_handler` 里没有对应命令、`scripts/` 里没有调用、前端源码一处未提。
//!
//! 读严一点，出口原文要的是一个**可用**的「命令」。一个库函数加一条单测不是命令：
//! 用户到不了它，运维在出事后也到不了它。审计这件事的全部价值就在出事之后的那
//! 一次校验，而那一次此前只能靠「会写 Rust 的人连上同一个数据库」。
//!
//! 这与本仓反复出现的那种遗漏同型（功能做完、入口没接）：区别只在这次是**先建绊线
//! 再补功能**——`audit_wiring` 测试与本命令同批落，此后「链校验能力存在但用户
//! 到不了」这个状态无法再悄悄成立。
//!
//! # 三个命令的边界
//!
//! * `audit_verify` 只读库、只算 hash，**不写任何东西**。校验本身不得留痕——
//!   一条「有人校验过审计链」的审计行会告诉查看者链曾被审计过，那是对手的情报。
//! * `audit_export` 导出的包**含命令文本**（已脱敏，但脱敏是形状匹配，挡不住一段
//!   看起来像散文的机密）。`forensics::export` 的文档把「导出前 UI 必须明示这一点」
//!   写成调用方的义务，本文件把该义务转交给前端（对话框里有一段不会略过的提示）。
//! * `audit_verify_bundle` 收 JSON 文本而不是文件路径：文件怎么选是前端的事
//!   （`<input type="file">`），Rust 侧拿路径反而把「路径来自谁」变成一个新问题。

use crate::state::AppState;
use fs_connmgr::audit_repo::{AuditRepo, BreakReason, ChainStatus};
use fs_connmgr::forensics;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

/// `audit_verify` 的返回。
///
/// `why` 用稳定串而不是枚举直译：`content_altered`（这一行的内容与它自己的 hash
/// 不符——字段被改过）与 `link_mismatch`（这一行的 prev_hash 与上一行接不上——
/// 有行被删掉或插入过）。两种断法的**追查方向不同**：前者查「谁能写这个字段」，
/// 后者查「谁动过这张表」。
#[derive(Debug, Clone, Serialize)]
pub struct ChainReport {
    pub ok: bool,
    pub rows: usize,
    pub at_id: Option<i64>,
    pub why: Option<String>,
    /// 给人看的一句话（含行号与断因），前端原样显示。
    pub message: String,
}

/// 校验整条审计链。空库也算完整（一条没写过的链不会是断的）。
#[tauri::command]
pub async fn audit_verify(state: State<'_, Arc<AppState>>) -> Result<ChainReport, String> {
    let repo = AuditRepo::new(state.db.pool());
    let status = repo.verify_chain().await.map_err(|e| e.to_string())?;
    Ok(match status {
        ChainStatus::Intact { rows } => ChainReport {
            ok: true,
            rows,
            at_id: None,
            why: None,
            message: format!("审计链完整，共 {rows} 条记录。"),
        },
        ChainStatus::Broken { at_id, why } => {
            // 一处 match 取（给人看的话， 稳定串）一对：写两遍 match 会让
            // 「文案与稳定串的对应关系」变成两处要同步的东西。
            let (w, code) = match why {
                BreakReason::ContentAltered => (
                    "这一行的内容与它自己的哈希不符：字段被改过。查「谁能写这个字段」。",
                    "content_altered",
                ),
                BreakReason::LinkMismatch => (
                    "这一行的 prev_hash 与上一行接不上：有行被删掉或插入过。查「谁动过这张表」。",
                    "link_mismatch",
                ),
            };
            ChainReport {
                ok: false,
                rows: 0,
                at_id: Some(at_id),
                why: Some(code.to_string()),
                message: format!("审计链在第 {at_id} 条记录处断裂。{w}"),
            }
        }
    })
}

/// 增量校验：从最近一次[周期性快照](fs_connmgr::audit_checkpoint)往后重算。
///
/// # 它比 `audit_verify` 弱，且这件事不许被藏起来
///
/// 增量校验只重算快照之后那一段。快照之前的行**这一次根本没读**——它们的完整性
/// 只由快照断言，而快照与审计表同库同权限，能改一个的人也能改另一个。
///
/// 所以 `ok == true` 在这条路径上**不等于**「审计链完整」。`message` 由
/// `fs_connmgr` 那侧无条件带上覆盖范围（「第 N 条及之前的 K 条这次未重算」），
/// 本命令原样透传、不重写文案——重写一遍就是给「把增量印成全量」开一个口子。
/// `full_coverage` 字段让前端能在 UI 上把两者区分开，而不必解析中文。
///
/// # 为什么它不落快照
///
/// 见模块头：校验本身不得留痕。一条「有人校验过审计链」的记录会告诉查看者
/// 链曾被审计过，那是对手的情报。快照只由**按行数自动触发**的那条路径落
/// （`checkpoint_on_startup`）——它泄露的只是「程序跑过、表长了」，
/// 不是「有人在查」。
#[tauri::command]
pub async fn audit_verify_quick(state: State<'_, Arc<AppState>>) -> Result<QuickReport, String> {
    let repo = AuditRepo::new(state.db.pool());
    let r = fs_connmgr::audit_checkpoint::verify(&repo)
        .await
        .map_err(|e| e.to_string())?;
    Ok(QuickReport {
        ok: matches!(r.status, ChainStatus::Intact { .. }),
        full_coverage: r.is_full_coverage(),
        at_id: match r.status {
            ChainStatus::Broken { at_id, .. } => Some(at_id),
            ChainStatus::Intact { .. } => None,
        },
        message: r.message(),
    })
}

/// `audit_verify_quick` 的返回。
#[derive(Debug, Clone, Serialize)]
pub struct QuickReport {
    pub ok: bool,
    /// 这一次是否覆盖了整条链。`ok && !full_coverage` 是最需要被读懂的组合：
    /// 查过的那一段没问题，**没查的那一段这次什么也没说**。
    pub full_coverage: bool,
    pub at_id: Option<i64>,
    /// 由 `fs_connmgr` 生成、本层原样透传——它无条件含覆盖范围声明。
    pub message: String,
}

/// 启动时按行数落一份周期性快照。
///
/// 「周期」按行数而不是按时间：审计表不写的时候没有任何东西需要快照。
///
/// 三种结果三种处置，且**没有一种是静默**：
/// - 落了 / 没到阈值 → `info` / 不记；
/// - **拒落（链已断）** → `error`。这是本函数唯一真正重要的一条：程序在启动时
///   发现审计链断了。不是 warn——warn 会淹在日志里，而这条的含义是
///   「这台机器上的审计记录已经不可信」。
/// - 库错误 → `warn`。快照是维护动作，落不下不该拦住程序启动。
pub async fn checkpoint_on_startup(pool: &sqlx::SqlitePool) {
    let repo = AuditRepo::new(pool);
    match fs_connmgr::audit_checkpoint::record_if_due(
        &repo,
        &now_rfc3339(),
        fs_connmgr::audit_checkpoint::DEFAULT_EVERY_ROWS,
    )
    .await
    {
        Ok(None) => {}
        Ok(Some(fs_connmgr::audit_checkpoint::CheckpointOutcome::Recorded(c))) => {
            tracing::info!(row_id = c.row_id, rows = c.row_count, "审计链快照已记录");
        }
        Ok(Some(fs_connmgr::audit_checkpoint::CheckpointOutcome::NothingToRecord)) => {}
        Ok(Some(fs_connmgr::audit_checkpoint::CheckpointOutcome::RefusedChainBroken(s))) => {
            tracing::error!(status = ?s, "审计链校验未通过，已拒绝记录快照——审计记录已不可信");
        }
        Err(e) => tracing::warn!(%e, "审计链快照记录失败"),
    }
}

/// 导出取证包（JSON 文本，由前端走下载落盘）。
///
/// **包里含命令文本**——见模块头与 `forensics::export` 的文档；前端在保存前有明示。
#[tauri::command]
pub async fn audit_export(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let repo = AuditRepo::new(state.db.pool());
    let bundle = forensics::export(
        &repo,
        &now_rfc3339(),
        &crate::commands::update_cmd::app_version(),
    )
    .await
    .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&bundle).map_err(|e| format!("取证包序列化失败：{e}"))
}

/// 校验一份取证包 JSON（不接触数据库——取证包不是备份，见 forensics 模块头）。
#[derive(Debug, Clone, Serialize)]
pub struct BundleReport {
    pub usable: bool,
    /// 给人看的一句话。`UnsupportedVersion` 的措辞是「包比程序新」，**不是**「被篡改」
    /// ——把前者报成后者会让人去查一件没发生过的事。
    pub message: String,
}

#[tauri::command]
pub async fn audit_verify_bundle(content: String) -> Result<BundleReport, String> {
    // verify_json 是纯同步函数，但保持 async：三个审计命令一个签名习惯，
    // 且将来若要加 IO（如读大文件的流式解析）不必破前端契约。
    let status = forensics::verify_json(&content)?;
    Ok(BundleReport {
        usable: status.is_usable(),
        message: status.message(),
    })
}

/// RFC3339 时间戳。直接用 ai_cmd 那一份（pub(crate)）而不是复制——日期数学
/// 是最不该复制的东西：那份曾经算错过一次（%86400 的余数看错），复制两份就是
/// 两处可错，而审计时间戳与 AI 记录本来就该同一个粒度、同一个时区口径。
fn now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    crate::commands::ai_cmd::format_rfc3339(now)
}

#[cfg(test)]
mod tests {

    /// 接线守卫：**「链校验能力存在但用户到不了」这个状态必须无法悄悄成立。**
    ///
    /// 判据是源码扫查而非运行时调用——运行时版本要造一个 Tauri State，那比这条
    /// 断言贵一个数量级，而这里要钉的是「注册没注册、调用方在不在」，字符串就够。
    /// 三件事各自独立可红：
    ///   ① 三个命令真的注册进了 invoke_handler（没注册 = 前端 invoke 得到「不存在」）；
    ///   ② 前端真的调用它们（没调用 = 命令是死的，这正是本文件模块头描述的那种遗漏）；
    ///   ③ 菜单里真的有入口（功能做完入口没接，本仓已发生过四次）。
    #[test]
    fn audit_wiring_commands_registered_frontend_calls_menu_entry_exists() {
        let lib = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
        )
        .expect("读不到 app/src/lib.rs");
        let registered = lib.contains("commands::audit_cmd::audit_verify")
            && lib.contains("commands::audit_cmd::audit_verify_quick")
            && lib.contains("commands::audit_cmd::audit_export")
            && lib.contains("commands::audit_cmd::audit_verify_bundle");
        assert!(
            registered,
            "audit_verify/verify_quick/export/verify_bundle 没有全部注册进 invoke_handler"
        );
        // 周期性快照必须真的有人调。只有库函数加 #[test] 的话，快照表永远是空的，
        // 增量校验永远退化成全表——功能「实现了」而一次也不会发生。
        // 这正是本文件模块头描述的那种遗漏，本仓已发生过四次。
        assert!(
            lib.contains("commands::audit_cmd::checkpoint_on_startup"),
            "启动流程没有调用 checkpoint_on_startup——周期性快照永远不会发生"
        );

        let fe_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../frontend/src");
        let mut frontend_calls = false;
        let mut menu_entry = false;
        let mut quick_wired = false;
        for entry in walk(&fe_root) {
            let Ok(s) = std::fs::read_to_string(&entry) else {
                continue;
            };
            let name = entry.to_string_lossy();
            if name.ends_with(".svelte") || name.ends_with(".ts") {
                if s.contains("\"audit_verify\"") || s.contains("\"audit_export\"") {
                    frontend_calls = true;
                }
                if name.ends_with("AuditDialog.svelte") && s.contains("\"audit_verify_quick\"") {
                    quick_wired = true;
                }
                if name.ends_with("menus.ts") && s.contains("tools.auditVerify") {
                    menu_entry = true;
                }
            }
        }
        assert!(
            frontend_calls,
            "前端没有任何地方 invoke audit_verify/audit_export——命令注册了但没人调用，是死入口"
        );
        assert!(
            menu_entry,
            "menus.ts 里没有 tools.auditVerify——功能做完入口没接，本仓已发生过四次"
        );
        assert!(
            quick_wired,
            "AuditDialog.svelte 里没有 invoke audit_verify_quick——增量校验注册了但用户到不了"
        );
    }

    /// 脱敏不变量绊线：**每个构造 `NewAuditEntry` 的地方都必须经过 `redact`。**
    ///
    /// `NewAuditEntry.action` 的文档写着「调用方负责先脱敏……由 app 层的测试守着」
    /// ——而那个测试此前**不存在**，这句话本身又是一个只由注释担保的承诺。
    /// 今天只有一个写入方（ai_cmd::audit_ai，它脱敏），但 M3 马上要加两个：
    /// Agent 步骤与 MCP 调用。两个新写入方各有一百种理由「这条不脱也行」
    /// （它只是个工具名 / 它来自我们自己 / 模型输出已经洗过了），
    /// 而审计行会**永远留在库里**并且**随取证包离开本机**。
    ///
    /// 判据：扫 app/src 生产代码（剥注释），每个出现 `NewAuditEntry` 的文件
    /// 必须也出现 `redact`。宽松但守住底线——精确到「每处构造都调 redact」
    /// 的数据流分析在源码扫查里做不到，而「文件级没有 redact」必然意味着
    /// 裸写。反向锚点：ai_cmd.rs 必须命中（否则扫查坏了，下面全绿无意义）。
    #[test]
    fn every_audit_writer_routes_through_redact() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut violators: Vec<String> = Vec::new();
        let mut saw_writer = false;
        for f in walk(&root) {
            let name = f.to_string_lossy().to_string();
            if !name.ends_with(".rs") || name.contains("audit_cmd.rs") {
                continue; // 本文件自己不构造 NewAuditEntry
            }
            let Ok(s) = std::fs::read_to_string(&f) else {
                continue;
            };
            // 剥注释：解释「为什么要脱敏」的注释里必然写着 redact，
            // 不剥就成了注释给自己作证。
            let code: String = s
                .lines()
                .map(|l| match l.find("//") {
                    Some(i) => &l[..i],
                    None => l,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if code.contains("NewAuditEntry") {
                saw_writer = true;
                // 判据必须带**调用括号**，不能是裸词 `redact`：变异测试证明
                // 裸词会被类型路径骗过——不脱敏的写法
                // `fs_ai::redact::Redacted { text: action.to_string(), .. }`
                // 里也有 "redact" 这个词（模块名），文件级判定照样绿。
                //
                // 两条等价路径都算数：
                //   ① `redact(`——直接调脱敏函数（ai_cmd::audit_ai 那条）；
                //   ② `RedactedAction::new(`——**构造即脱敏的 newtype**
                //      （fs_ai::agent::record）。它比 ① 更强：未脱敏的串在那个
                //      类型之外无处安放，而 ① 靠的是「调用方记得调」。
                //      M3 的两个写入方（agent_audit / agent_cmd）走的是这条。
                let redacted = code.contains("redact(")
                    || code.contains("RedactedAction::new(")
                    // ③ 显式类型标注的 RedactedAction 绑定——保证来自类型本身
                    //（写入方只是把已脱敏的值取出来用）。
                    || code.contains(": &fs_ai::agent::record::RedactedAction");
                if !redacted {
                    violators.push(f.file_name().unwrap().to_string_lossy().to_string());
                }
            }
        }
        // 做空防护：至少 ai_cmd.rs 是写入方。它不在了说明扫查坏了。
        assert!(
            saw_writer,
            "扫查没找到任何 NewAuditEntry 构造——ai_cmd.rs 应当是写入方，扫查路径坏了"
        );
        assert!(
            violators.is_empty(),
            "这些文件构造 NewAuditEntry 却不经过 redact：{violators:?}。\n\
             审计行永远留在库里并随取证包离开本机；调用方脱敏的义务见 \
             NewAuditEntry.action 的文档，本测试就是那句「由 app 层的测试守着」"
        );
    }

    /// 递归收集文件（测试侧的小工具，不进生产代码）。
    fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(dir) else {
            return out;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                // node_modules 与测试快照不扫：那里面出现这些串不代表本仓接线。
                let n = p
                    .file_name()
                    .map(|x| x.to_string_lossy().to_string())
                    .unwrap_or_default();
                if n == "node_modules" || n == "snapshot" || n.starts_with('.') {
                    continue;
                }
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }
}
