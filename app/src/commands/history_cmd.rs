//! 命令历史（M4a：历史命令 UI）。
//!
//! 两个数据源，[`fs_connmgr::HistoryRepo`] 的 `source` 列把它们分开：
//! - `sent`：本程序亲手发出的字节（组合栏、快速命令、广播、历史面板重发），字节精确；
//! - `grid`：从终端回滚里启发式提取（[`fs_terminal::history`]），近似值。
//!
//! 前端必须按来源区分呈现——把一条可能带提示符残渣的近似值当成可一键重发的命令，
//! 是真事故。

use crate::state::AppState;
use std::sync::Arc;
use tauri::State;

/// 检索一次最多回多少条。
///
/// 面板是「搜索 + 挑一条」的用法，不是「翻阅全部」。500 条已远超人能扫视的量，
/// 再多只是把 IPC 与渲染成本白花掉。
const SEARCH_LIMIT: i64 = 500;

/// 单条命令的长度上限。
///
/// 与 `MAX_TERM_INPUT_BYTES` 不同源，刻意小得多：历史库存的是**命令**，不是粘贴
/// 进去的一整份文件。8 KiB 装得下任何真实命令行（Linux 的 ARG_MAX 单参数上限也是
/// 128 KiB 量级，但人手敲的命令没有接近它的）。越界的只可能是误记（比如把一段
/// 输出当命令记了），拦下来比污染历史库好。
const MAX_COMMAND_BYTES: usize = 8 * 1024;

// 两条包络关系是**编译期事实**，用 const 断言而不是 #[test]：编译期不通过比测试
// 红更早、更硬，而且不依赖有人去跑测试。
// ① 历史条目上限必须远小于终端输入上限——历史库存命令，不存粘贴进去的整份文件；
// ② 又不能小到装不下真实的长命令行（带一串 `--flag` 的 docker/ffmpeg 命令）。
const _: () = assert!(MAX_COMMAND_BYTES < super::session_cmd::MAX_TERM_INPUT_BYTES);
const _: () = assert!(MAX_COMMAND_BYTES >= 4096);
// 检索一次的回条上限：面板是「搜索 + 挑一条」，不是「翻阅全部」
const _: () = assert!(SEARCH_LIMIT > 0 && SEARCH_LIMIT <= 1000);

/// 记一条「亲手发出」的命令。
///
/// `session_id` 可空（广播到多个会话、或无会话时的记录）：能取到会话就顺带把
/// host/profile_id 记上，取不到就记空——历史本身仍然有价值，不该因为拿不到主机名
/// 就整条丢掉。
#[tauri::command]
pub async fn history_record(
    command: String,
    session_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if command.len() > MAX_COMMAND_BYTES {
        return Err(format!(
            "命令过长（{} 字节，上限 {MAX_COMMAND_BYTES}），未记入历史",
            command.len()
        ));
    }
    let (host, profile_id) = resolve_origin(&state, session_id.as_deref()).await;
    fs_connmgr::HistoryRepo::new(state.db.pool())
        .record(&command, &host, &profile_id, "sent", now_secs())
        .await
        .map_err(|e| e.to_string())
}

/// 检索历史。`host` 非空时只看该主机上的记录。
#[tauri::command]
pub async fn history_search(
    query: String,
    host: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<fs_connmgr::HistoryEntry>, String> {
    let host = host.filter(|h| !h.trim().is_empty());
    fs_connmgr::HistoryRepo::new(state.db.pool())
        .search(&query, host.as_deref(), SEARCH_LIMIT)
        .await
        .map_err(|e| e.to_string())
}

/// 从指定会话的终端回滚里扫一遍候选命令并入库，返回新提取的条数。
///
/// 回滚文本可以是几 MB，**不**经 IPC 交给前端再回传：提取是纯字节逻辑
/// （`fs_terminal::history`），在后端就地做完，只回一个计数。
#[tauri::command]
pub async fn history_scan_grid(
    session_id: String,
    max_lines: usize,
    state: State<'_, Arc<AppState>>,
) -> Result<usize, String> {
    let session = state
        .registry
        .get(&session_id)
        .ok_or("会话不存在或已关闭")?;
    // 上限钳在这里而不是信前端：max_lines 直接决定一次要拷多少文本
    let lines = max_lines.clamp(1, fs_terminal::grid::DEFAULT_SCROLLBACK_LINES);
    let text = session.pipe.scrollback_text(lines);
    let commands = fs_terminal::history::extract_commands(&text);
    let (host, profile_id) = resolve_origin(&state, Some(&session_id)).await;
    let repo = fs_connmgr::HistoryRepo::new(state.db.pool());
    let now = now_secs();
    let mut n = 0usize;
    for cmd in &commands {
        if cmd.len() > MAX_COMMAND_BYTES {
            continue;
        }
        // 单条失败不打断整批：一条写不进去（约束冲突等）不该让其余几十条也丢掉
        match repo.record(cmd, &host, &profile_id, "grid", now).await {
            Ok(()) => n += 1,
            Err(e) => tracing::warn!(%session_id, error = %e, "历史条目写入失败，跳过"),
        }
    }
    tracing::info!(%session_id, lines, extracted = commands.len(), stored = n, "已从终端回滚提取候选命令");
    Ok(n)
}

/// 删一条历史（剔掉误记的条目）。
#[tauri::command]
pub async fn history_delete(id: i64, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    fs_connmgr::HistoryRepo::new(state.db.pool())
        .delete(id)
        .await
        .map_err(|e| e.to_string())
}

/// 清空全部历史。
///
/// 这是**隐私操作**而不只是清理：命令行里可能有一次性口令、令牌、私有主机名。
/// 前端必须先确认。
#[tauri::command]
pub async fn history_clear(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    fs_connmgr::HistoryRepo::new(state.db.pool())
        .clear()
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!("命令历史已清空（用户操作）");
    Ok(())
}

/// 取会话的 (host, profile_id)。取不到一律回空串——历史记录不该因为拿不到主机名
/// 就整条丢掉，而 `''` 正是 schema 里「未知主机」的表示（见迁移脚本的列注：
/// 用 NULL 会让唯一索引失效、去重整体失灵）。
async fn resolve_origin(state: &Arc<AppState>, session_id: Option<&str>) -> (String, String) {
    let Some(sid) = session_id else {
        return (String::new(), String::new());
    };
    let Some(session) = state.registry.get(sid) else {
        return (String::new(), String::new());
    };
    let profile_id = session.profile_id.clone();
    (host_of_endpoint(&session.target_endpoint), profile_id)
}

/// 从 `host:port` 取主机名。
///
/// 按**最后一个**冒号切：IPv6 端点长成 `[::1]:22`，按第一个冒号切会得到 `[`。
/// 没有冒号（形状意外）时原样返回——历史里记一个不完美的主机名，比记空好。
fn host_of_endpoint(endpoint: &str) -> String {
    match endpoint.rsplit_once(':') {
        Some((h, _)) if !h.is_empty() => h.to_string(),
        _ => endpoint.to_string(),
    }
}

/// 当前 unix 秒。
///
/// 系统时钟早于 1970 时回 0 而不是 panic：一个坏时钟不该让「记一条历史」把整个
/// 命令发送流程带崩。
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 上限包络的三条断言已上移为模块级 `const _: () = assert!(…)`（编译期即判），
    // 此处不再重复一份运行期版本——同一事实两处断言，改一处漏一处。

    /// `host:port` → 主机名。IPv6 端点必须按**最后一个**冒号切。
    #[test]
    fn host_is_taken_from_the_last_colon() {
        assert_eq!(host_of_endpoint("web-01:22"), "web-01");
        assert_eq!(host_of_endpoint("10.0.0.5:2222"), "10.0.0.5");
        // 按第一个冒号切会得到 "["——IPv6 是这条规则存在的唯一理由
        assert_eq!(host_of_endpoint("[::1]:22"), "[::1]");
        assert_eq!(host_of_endpoint("[fe80::1%eth0]:22"), "[fe80::1%eth0]");
        // 形状意外时原样返回（记个不完美的主机名比记空好）
        assert_eq!(host_of_endpoint("bare-host"), "bare-host");
        assert_eq!(host_of_endpoint(""), "");
        assert_eq!(
            host_of_endpoint(":22"),
            ":22",
            "主机名段为空时不该退化成空串"
        );
    }

    #[test]
    fn now_secs_is_sane() {
        // 2020-01-01 之后、2100 之前：钉住「不是 0、也不是被算成毫秒」
        let n = now_secs();
        assert!(n > 1_577_836_800, "时间戳应晚于 2020，实得 {n}");
        assert!(
            n < 4_102_444_800,
            "时间戳应早于 2100（单位应是秒而非毫秒），实得 {n}"
        );
    }
}
