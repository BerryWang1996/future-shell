//! 远端文件的剪切 / 复制 / 粘贴与脚本运行（M7.3）。
//!
//! 决策全在 `fs_sshengine::fileops`（纯函数、逐格可测）；这一层只负责按计划执行，
//! 并把「哪一条成了、哪一条为什么没成」逐条带回去。
//!
//! # 为什么逐条报，而不是整批成败
//!
//! 粘贴十个文件、第七个因为权限失败——只回一个 `Err` 的话，用户不知道前六个动没动，
//! 也不知道后三个跑没跑。文件操作不可撤销，「不知道现在是什么状态」比失败本身更糟。
//! 故 [`PasteOutcome`] 是逐条的，界面据此说清楚。
//!
//! # 剪切走 SFTP、复制走 exec
//!
//! 理由见 `fs_sshengine::fileops` 的模块头注：SFTP 没有服务端复制，用它做远端→远端复制
//! 要把字节来回搬两趟网络。剪切是同主机 `rename`，SFTP 原语就够，不必落到 shell。

use crate::state::AppState;
use fs_sshengine::fileops::{
    copy_command, run_script_background, run_script_foreground, ConflictPolicy, PastePlan,
};
use fs_sshengine::sftp::SftpOps; // trait 方法（list/rename/read_range…）要它在作用域里
use std::sync::Arc;
use tauri::State;

/// 单条粘贴的结局。
#[derive(Debug, serde::Serialize)]
pub struct PasteOutcome {
    /// 落到目标目录里的最终名字（`KeepBoth` 时是改过的那个）。
    pub name: String,
    pub ok: bool,
    /// 失败原因；成功时为空串。
    pub error: String,
    /// 这一条是不是因为冲突而改了名（界面要说「实际存成了 X」）。
    pub renamed: bool,
}

/// 一次粘贴的整体结果。
#[derive(Debug, serde::Serialize)]
pub struct PasteResult {
    pub done: Vec<PasteOutcome>,
    /// 用户选了「跳过」而没动的名字。
    pub skipped: Vec<String>,
    /// 源与目标同路径而被拒的名字。
    pub same_path: Vec<String>,
}

/// 按**已经算好的计划**执行一次粘贴。
///
/// 计划由前端调 [`fileops_plan`] 取得并展示给用户确认；这里不重新裁决——
/// 重新裁决意味着用户看到的计划与实际执行的可能不是同一份，而这中间目录内容可能已经变了。
#[tauri::command]
pub async fn fileops_paste(
    session_id: String,
    plan: PastePlan,
    state: State<'_, Arc<AppState>>,
) -> Result<PasteResult, String> {
    let ops = state.sftp_ops_for(&session_id).await?;
    let mut done = Vec::new();
    for op in &plan.ops {
        let r = if op.cut {
            // 同主机移动：SFTP rename 一步到位。**覆盖语义**：多数 sftp-server 的 rename
            // 对已存在的目标会失败，故覆盖档先删目标——这正是用户选「覆盖」的意思。
            // 判据只认计划里那个显式的 `overwrite`：用「没改名」去反推会把不冲突的条目
            // 也算进来（它们同样没改名），于是每一次普通剪切都先对目标发一次删除。
            if op.overwrite {
                let _ = fs_sshengine::sftp::remove_tree(ops.as_ref(), &op.dst).await;
            }
            ops.rename(&op.src, &op.dst)
                .await
                .map_err(|e| e.to_string())
        } else {
            let cmd = copy_command(&op.src, &op.dst);
            let exec = state.exec_adapter_for(&session_id).await?;
            match exec.exec_once(&cmd).await {
                Ok(o) if o.code == Some(0) => Ok(()),
                Ok(o) => Err(if o.stderr.trim().is_empty() {
                    format!(
                        "复制失败（exit {}）",
                        o.code
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "未观测到".into())
                    )
                } else {
                    o.stderr.trim().to_string()
                }),
                Err(e) => Err(e.to_string()),
            }
        };
        done.push(PasteOutcome {
            name: op.dst_name.clone(),
            ok: r.is_ok(),
            error: r.err().unwrap_or_default(),
            renamed: op.renamed,
        });
    }
    Ok(PasteResult {
        done,
        skipped: plan.skipped.clone(),
        same_path: plan.same_path.clone(),
    })
}

/// 规划一次粘贴：列一次目标目录、把冲突裁决算出来。**不执行任何操作。**
#[tauri::command]
pub async fn fileops_plan(
    session_id: String,
    srcs: Vec<String>,
    dst_dir: String,
    policy: ConflictPolicy,
    cut: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<PastePlan, String> {
    let ops = state.sftp_ops_for(&session_id).await?;
    let listing = ops.list(&dst_dir).await.map_err(|e| e.to_string())?;
    let existing: Vec<String> = listing.entries.iter().map(|e| e.name.clone()).collect();
    Ok(fs_sshengine::fileops::plan_paste(
        &srcs, &dst_dir, &existing, policy, cut,
    ))
}

/// 读一段远端文本（脚本原文预览用）。
///
/// 走 SFTP 的 `read_range` 而不是 `cat`：不经 shell 就没有引号与元字符的问题，
/// 也不会因为文件巨大而把整条 exec 通道堵住。
///
/// 超过 `SCRIPT_PREVIEW_MAX` 的部分**截断并说明**，不静默省略——用户是拿这段原文来决定
/// 要不要执行的，「你看到的不是全部」这件事必须写在他眼前。
#[tauri::command]
pub async fn sftp_read_text(
    session_id: String,
    path: String,
    state: State<'_, Arc<AppState>>,
) -> Result<ScriptText, String> {
    let ops = state.sftp_ops_for(&session_id).await?;
    let size = ops.stat_size(&path).await.map_err(|e| e.to_string())?;
    let want = size.min(SCRIPT_PREVIEW_MAX as u64) as usize;
    let bytes = ops
        .read_range(&path, 0, want)
        .await
        .map_err(|e| e.to_string())?;
    Ok(ScriptText {
        // 非 UTF-8 用替换字符而不是报错：一个带乱码的预览仍然让用户看得出「这不是我要跑的东西」，
        // 而一句「读取失败」什么都不告诉他。
        text: String::from_utf8_lossy(&bytes).into_owned(),
        truncated: size > SCRIPT_PREVIEW_MAX as u64,
        total_bytes: size,
    })
}

/// 脚本预览的字节上限。够看清一段部署脚本在干什么，又不会把几十 MB 的东西塞进对话框。
pub const SCRIPT_PREVIEW_MAX: usize = 256 * 1024;

#[derive(Debug, serde::Serialize)]
pub struct ScriptText {
    pub text: String,
    pub truncated: bool,
    pub total_bytes: u64,
}

/// 在**后台**跑一个远端脚本（`nohup … &`）。
///
/// 前台那一路不经这里：它要把命令送进一个真正的 PTY 才能看到输出，而 PTY 在会话侧，
/// 由前端开一个新终端标签并把 [`fs_sshengine::fileops::run_script_foreground`] 的命令发进去。
///
/// **审计不在这一层写**，这是刻意的：脚本运行是「双击即以远端身份执行任意代码」，两条路
/// （前台/后台）都必须留痕，而前台那一路根本不经过任何 Rust 执行点（命令是被打进 PTY 的）。
/// 若后台在这里写、前台在前端写，同一件事就有两种记法与两个时间点。故统一走 M7.2 的口径：
/// **确认与审计都在决策点**，由前端在用户按下「运行」之后调 `audit_dangerous_action` 写一条
/// （`kind: "script.run"`）。这与 `services_cmd` 的服务动作不同——那一路的执行点在 Rust 里，
/// 且能拿到真实退出码，所以写在后端。
#[tauri::command]
pub async fn fileops_run_script_background(
    session_id: String,
    path: String,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    let cmd = run_script_background(&path);
    let exec = state.exec_adapter_for(&session_id).await?;
    let o = exec.exec_once(&cmd).await.map_err(|e| e.to_string())?;
    if o.code != Some(0) {
        return Err(if o.stderr.trim().is_empty() {
            format!(
                "后台启动失败（exit {}）",
                o.code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "未观测到".into())
            )
        } else {
            o.stderr.trim().to_string()
        });
    }
    Ok(cmd)
}

/// 前台命令的构造（前端拿去送进新终端标签）。**只构造不执行**——构造在 Rust 是为了
/// 让 `sh_quote` 只有一份实现：前端再写一遍引号规则，两份迟早分叉，而分叉的那一半是注入。
#[tauri::command]
pub fn fileops_foreground_command(path: String) -> String {
    run_script_foreground(&path)
}

#[cfg(test)]
mod tests {
    /// 本文件的生产段（口径同 `services_cmd.rs`：`include_str!` 会把测试模块自己读进来）。
    fn production_src() -> &'static str {
        const RAW: &str = include_str!("fileops_cmd.rs");
        let cut = RAW
            .find("\n#[cfg(test)]")
            .expect("本文件必须有 #[cfg(test)] 段");
        &RAW[..cut]
    }

    /// 路径**永远**经 `fileops` 的构造器进命令，本文件不得自己拼 shell 串。
    /// 自己拼的那一次就是注入——文件名里带引号和 `$` 是合法且常见的。
    #[test]
    fn this_file_never_builds_a_shell_string_by_hand() {
        let src = production_src();
        for bad in [
            "format!(\"cp ",
            "format!(\"mv ",
            "format!(\"sh ",
            "format!(\"nohup ",
        ] {
            assert!(
                !src.contains(bad),
                "本文件自己拼了 shell 串（{bad:?}）——一律走 fileops 的构造器"
            );
        }
        // 非空证明：确实用了那三个构造器
        for f in [
            "copy_command(",
            "run_script_background(",
            "run_script_foreground(",
        ] {
            assert!(src.contains(f), "没用上 {f}");
        }
    }

    /// 本层**不写审计**，这是刻意的（见 `fileops_run_script_background` 的文档）：
    /// 前台那一路根本不经过 Rust 执行点，两条路统一在前端的决策点各记一条。
    ///
    /// 这条守卫防的是「顺手补齐对偶」——看到 `services_cmd` 在后端写了审计就在这里也补一处，
    /// 于是同一次后台运行在库里出现两行、时间点还不一样，事后没人说得清那是跑了一次还是两次。
    #[test]
    fn audit_is_written_at_the_decision_point_not_here() {
        let src = production_src();
        // 针拼出来而不是写成字面量：`audit_cmd.rs` 的脱敏门禁是**逐文件**扫这个词的
        // （剥注释后仍算数），本文件只要出现一次完整字面量，就会被当成审计写入方而要求
        // 走脱敏——而本文件恰恰是「不写审计」的那一方。这与 `production_src()` 是同一类
        // 自指陷阱：守卫写在被守的文件里，字面量本身就成了证据。
        let needle = concat!("NewAudit", "Entry");
        assert!(
            !src.contains(needle),
            "本层写了审计——前端决策点已经写了一条，会变成同一件事两行"
        );
        // 缺席必须有出处：文档里要指明审计到底写在哪，否则下一个人只会看到「这里没写」
        assert!(
            src.contains("audit_dangerous_action"),
            "没有说明审计写在哪里——「这里不写」不能是一句无处可查的断言"
        );
    }

    /// 「先删目标」只认计划里那个显式标记。用「没改名」反推会把不冲突的条目也算成覆盖。
    #[test]
    fn destructive_step_is_gated_on_the_explicit_flag() {
        let src = production_src();
        assert!(
            src.contains("if op.overwrite {"),
            "删除目标的判据不是 op.overwrite"
        );
        assert!(!src.contains("!op.renamed"), "又用「没改名」去反推覆盖了");
    }

    /// 预览上限要足够看清一段部署脚本，又不能大到把对话框塞爆。
    ///
    /// 用 `const` 块断言：两边都是编译期常量，普通 `assert!` 会被 clippy 判成
    /// `assertions_on_constants`（而 `-D warnings` 下那是编译失败）。放进 const 块之后
    /// 判定发生在编译期——改坏了根本编译不过，比运行到这条测试更早。
    #[test]
    fn preview_cap_is_sane() {
        const {
            assert!(super::SCRIPT_PREVIEW_MAX >= 64 * 1024);
            assert!(super::SCRIPT_PREVIEW_MAX <= 1024 * 1024);
        }
    }
}
