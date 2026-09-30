//! 外部编辑器关联（M4a，Xftp「用关联程序编辑」对标）。
//!
//! 流程：远端文件下到暂存区 → 交给系统关联程序打开 → 用户存盘 → 回传。
//!
//! 判定（要不要回传、要不要拦下）在引擎层 [`fs_sshengine::editsync`]，本模块只做
//! 「取快照、开程序、把结论交给前端」这三件事。判定单独成模块的理由是它决定
//! **要不要覆盖远端**——覆盖别人刚改的文件不可逆，这种判断必须能被单测逐条打死。
//!
//! 轮询而不是文件监视：编辑器们的保存策略五花八门（原地写、写临时文件再改名、
//! 先截断再写），inotify/ReadDirectoryChanges 在「改名替换」下会丢掉目标、
//! 在「先截断再写」下会看到一个空文件。前端按秒级问一次 `editor_check`，
//! 判定用 size+mtime——慢一点，但不会漏。
//!
//! 暂存区不是下载沙箱：编辑是**双向**的，回传时要读回同一个文件。沙箱是下载的
//! 落点（只写不读回），混用会让「用户手动下载的同名文件」与「正在编辑的暂存件」
//! 撞在一条路径上。

use crate::state::AppState;
use fs_sshengine::editsync::{decide, Decision, Snapshot};
use fs_sshengine::sftp::SftpOps;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;

/// 一次编辑会话。
pub struct EditSession {
    pub session_id: String,
    pub remote: String,
    pub local: PathBuf,
    pub local_at_open: Snapshot,
    pub remote_at_open: Snapshot,
}

pub type EditMap = Arc<std::sync::Mutex<HashMap<String, EditSession>>>;

/// 单个可编辑文件的大小上限。编辑走「整份下来、整份传回」，没有分块——
/// 8 MiB 之外的东西（日志、数据库、镜像）用编辑器打开本身就不是好主意，
/// 而在这里不设限会让一次误点吃掉几百 MB 内存。
pub const EDIT_SIZE_MAX: u64 = 8 * 1024 * 1024;

/// 编辑键。**长度前缀**而非单纯拼接：`<len>:<session_id>\u{1}<remote>`。
///
/// 单纯用分隔符拼接是可碰撞的——POSIX 文件名里除了 `/` 与 NUL 什么字节都合法，
/// 包括 `\u{1}`。今天的 session_id 是我们自己生成的 UUID（不含 `\u{1}`），所以
/// 碰撞暂时构造不出来；但那是**调用方性质**，不是这个函数的性质，而键一旦碰撞，
/// 一次回传就会写到另一台机器的文件上。长度前缀让不可碰撞成为键本身的性质。
fn key(session_id: &str, remote: &str) -> String {
    format!("{}:{session_id}\u{1}{remote}", session_id.len())
}

fn snap_local(p: &std::path::Path) -> Option<Snapshot> {
    let md = std::fs::metadata(p).ok()?;
    Some(Snapshot {
        size: md.len(),
        mtime: md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    })
}

/// 暂存目录：`<data_dir>/edit/<会话>`。按会话分目录是为了「关会话即可清理」，
/// 也避免两台服务器上的同名文件在本地互相覆盖。
fn edit_dir(state: &AppState, session_id: &str) -> Result<PathBuf, String> {
    let dir = state
        .data_dir
        .join("edit")
        .join(fs_sshengine::sandbox::escape_local_component(session_id));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// 打开远端文件到本地编辑器。返回本地暂存路径（供 UI 显示「正在编辑」）。
#[tauri::command]
pub async fn editor_open(
    session_id: String,
    remote: String,
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    let ops = state.sftp_ops_for(&session_id).await?;
    // lstat 而非 stat：软链的「编辑」语义含糊（改链还是改目标？），直接拒绝并说清楚，
    // 让用户自己决定去编辑目标路径——猜错的那一半是不可逆的。
    let meta = ops.lstat(&remote).await.map_err(|e| e.to_string())?;
    match meta.file_type {
        fs_sshengine::sftp::FileType::Regular => {}
        fs_sshengine::sftp::FileType::Symlink => {
            return Err(format!(
                "「{remote}」是符号链接：编辑链还是编辑它指向的文件语义不明。请在属性里查看目标，再直接编辑目标路径"
            ))
        }
        other => return Err(format!("「{remote}」不是普通文件（{other:?}），无法编辑")),
    }
    if meta.size > EDIT_SIZE_MAX {
        return Err(format!(
            "文件 {} 字节，超过编辑上限 {EDIT_SIZE_MAX} 字节（编辑是整份下载/整份回传，没有分块）。请改用下载",
            meta.size
        ));
    }

    let name = remote
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("edited");
    // 逐段转义（与下载沙箱同一个函数）：远端文件名是服务端可控的
    let local =
        edit_dir(&state, &session_id)?.join(fs_sshengine::sandbox::escape_local_component(name));

    // 整份读下来。分块读的意义在这里不成立（上限已封 8 MiB），而一次 read_range
    // 让「下到一半失败」不会留下半个文件被误当成内容。
    let bytes = ops
        .read_range(&remote, 0, meta.size as usize)
        .await
        .map_err(|e| e.to_string())?;
    std::fs::write(&local, &bytes).map_err(|e| format!("write {}: {e}", local.display()))?;

    let local_at_open = snap_local(&local)
        .ok_or_else(|| format!("暂存文件刚写好却读不到元数据：{}", local.display()))?;
    let remote_at_open = Snapshot {
        size: meta.size,
        mtime: meta.mtime,
    };
    state
        .edits
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(
            key(&session_id, &remote),
            EditSession {
                session_id: session_id.clone(),
                remote: remote.clone(),
                local: local.clone(),
                local_at_open,
                remote_at_open,
            },
        );

    // 交给系统关联程序。走 Rust 侧 OpenerExt（与 reveal_* 同路径），因此 capability
    // 不必授 `opener:*`——R119 的最小权限结论在这里同样成立。
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(local.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| format!("打开本地编辑器失败：{e}"))?;
    Ok(local.to_string_lossy().into_owned())
}

/// 编辑状态检查（前端按秒级轮询）。判定见 [`fs_sshengine::editsync::decide`]。
#[tauri::command]
pub async fn editor_check(
    session_id: String,
    remote: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Decision, String> {
    let (local, local_at_open, remote_at_open) = {
        let map = state.edits.lock().unwrap_or_else(|p| p.into_inner());
        let e = map
            .get(&key(&session_id, &remote))
            .ok_or("这个文件不在编辑中")?;
        (e.local.clone(), e.local_at_open, e.remote_at_open)
    };
    let ops = state.sftp_ops_for(&session_id).await?;
    // 远端侧用 lstat：与开局那次取快照的语义必须一致，否则「变了没变」这个比较
    // 会在软链场景下比两个不同的东西。
    let remote_now = ops.lstat(&remote).await.ok().map(|m| Snapshot {
        size: m.size,
        mtime: m.mtime,
    });
    Ok(decide(
        local_at_open,
        snap_local(&local),
        remote_at_open,
        remote_now,
    ))
}

/// 回传（用户确认后调用；冲突时也走这里，由前端在确认对话框后再调一次）。
///
/// 回传成功后**刷新两侧快照**：不刷的话下一次 check 会拿旧的 remote_at_open 去比，
/// 而远端已经是我们自己刚写上去的内容 → 立刻报一个不存在的冲突。
#[tauri::command]
pub async fn editor_upload(
    session_id: String,
    remote: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let local = {
        let map = state.edits.lock().unwrap_or_else(|p| p.into_inner());
        map.get(&key(&session_id, &remote))
            .ok_or("这个文件不在编辑中")?
            .local
            .clone()
    };
    let bytes = std::fs::read(&local).map_err(|e| format!("read {}: {e}", local.display()))?;
    if bytes.len() as u64 > EDIT_SIZE_MAX {
        return Err(format!(
            "本地文件已增长到 {} 字节，超过编辑回传上限 {EDIT_SIZE_MAX}",
            bytes.len()
        ));
    }
    let ops = state.sftp_ops_for(&session_id).await?;
    // 先截断再写：write_at 刻意不带 TRUNCATE（那会毁断点续传，见 SftpOps::truncate 的
    // 文档）。少了这一步，把一个长文件改短后回传只会盖住前半段，旧尾巴留在远端——
    // 而那是一个语法上合法、语义上错误的配置文件。
    ops.truncate(&remote, 0).await.map_err(|e| e.to_string())?;
    ops.write_at(&remote, 0, &bytes)
        .await
        .map_err(|e| e.to_string())?;

    let remote_now = ops.lstat(&remote).await.map_err(|e| e.to_string())?;
    let mut map = state.edits.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(e) = map.get_mut(&key(&session_id, &remote)) {
        e.local_at_open = snap_local(&e.local).unwrap_or(e.local_at_open);
        e.remote_at_open = Snapshot {
            size: remote_now.size,
            mtime: remote_now.mtime,
        };
    }
    Ok(())
}

/// 结束编辑（用户点「停止编辑」或关闭会话）。删暂存文件；删不掉不算失败——
/// 编辑器可能还占着它，而「停止跟踪」这件事已经完成了。
#[tauri::command]
pub async fn editor_close(
    session_id: String,
    remote: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let removed = state
        .edits
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&key(&session_id, &remote));
    if let Some(e) = removed {
        let _ = std::fs::remove_file(&e.local);
    }
    Ok(())
}

/// 当前正在编辑的清单（UI 用来显示「正在编辑 N 个文件」）。
#[tauri::command]
pub async fn editor_list(state: State<'_, Arc<AppState>>) -> Result<Vec<EditItem>, String> {
    let map = state.edits.lock().unwrap_or_else(|p| p.into_inner());
    Ok(map
        .values()
        .map(|e| EditItem {
            session_id: e.session_id.clone(),
            remote: e.remote.clone(),
            local: e.local.to_string_lossy().into_owned(),
        })
        .collect())
}

#[derive(serde::Serialize)]
pub struct EditItem {
    pub session_id: String,
    pub remote: String,
    pub local: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 编辑键必须把会话与路径都算进去，且**不可碰撞**：两台服务器上的 `/etc/nginx.conf`
    /// 是两件事，键坍缩会让一次回传写到另一台机器上。
    #[test]
    fn key_separates_session_and_path() {
        assert_ne!(key("s1", "/etc/a.conf"), key("s2", "/etc/a.conf"));
        assert_ne!(key("s1", "/etc/a.conf"), key("s1", "/etc/b.conf"));
        assert_eq!(key("s1", "a"), "2:s1\u{1}a");
        // 长度前缀让「把分隔符塞进 session_id 来伪造另一个键」不可能：
        // 单纯拼接时 ("s\u{1}1","a") 与 ("s","1\u{1}a") 会得到同一个串。
        assert_ne!(
            key("s\u{1}1", "a"),
            key("s", "1\u{1}a"),
            "键必须结构上不可碰撞，不能只依赖「调用方不会传含分隔符的 id」"
        );
        // 长度相同、内容不同也必须分开（前缀不是判据的全部）
        assert_ne!(key("ab", "x"), key("ba", "x"));
    }
}
