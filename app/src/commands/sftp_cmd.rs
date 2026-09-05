use crate::state::AppState;
use fs_sshengine::sandbox;
use fs_sshengine::sftp::SftpOps;
use fs_sshengine::transfer::{Direction, TransferJob};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

/// 本地浏览根：用户家目录（spec：本地 FS 浏览限用户目录起）。
fn browse_root() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// 浏览根包含校验（spec §3.3）——`local_list` 与 `transfer_submit` 的 Up 分支**共用唯一实现**，
/// 二者不得再内联双份 canonicalize + starts_with（R55）。`root` 作实参注入（生产实参恒为
/// `browse_root()`）使本函数可测：单测传 tempdir，不依赖真实家目录。
/// 语义：`raw` 为空 → 根本身；相对路径以根解析；canonicalize 后必须仍在根内——canonicalize
/// 同时吸收「`..` 逃逸」与「符号链接指向根外」两类越界（fail-closed，越界即 Err 不放行）。
///
/// M4a：ZMODEM 上传（`zmodem_send`）经 [`resolve_upload_source`] 走同一把闸。上传源
/// 的越界规则只该有一条——两条迟早分叉，而分叉的那一侧就是可以读走任意文件的那侧。
fn ensure_within(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let root_c =
        std::fs::canonicalize(root).map_err(|e| format!("browse root unavailable: {e}"))?;
    let target = if raw.trim().is_empty() {
        root_c.clone()
    } else {
        let p = PathBuf::from(raw);
        if p.is_absolute() {
            p
        } else {
            root_c.join(p)
        }
    };
    let canon = std::fs::canonicalize(&target).map_err(|e| e.to_string())?;
    if !canon.starts_with(&root_c) {
        return Err(format!("path escapes browse root: {}", canon.display()));
    }
    Ok(canon)
}

/// 上传源解析（本地路径 → 浏览根内的规范路径）。
///
/// 供 ZMODEM 上传复用 SFTP 上传的同一把闸（见 `ensure_within` 的说明）。
/// 单独开这个入口而不是把 `ensure_within` 直接 pub：生产实参恒为 `browse_root()`，
/// 暴露 `root` 参数就等于给调用方一个「换一个根」的旋钮，而那个旋钮的唯一用途
/// 是绕过本闸。
pub fn resolve_upload_source(raw: &str) -> Result<PathBuf, String> {
    ensure_within(&browse_root(), raw)
}

/// 远端目录列表。`exclude` 是排除过滤器串（M4a），语法见
/// [`fs_sshengine::filter::ExcludeFilter`]——**同一个匹配器**也用于目录递归传输，
/// 不许在前端再写一份：两份匹配器一旦分叉，界面上看不见的文件会被传走（或反过来，
/// 以为传了其实跳过了），两种都不报错。
#[tauri::command]
pub async fn sftp_list(
    session_id: String,
    path: String,
    exclude: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<fs_sshengine::sftp::ListResult, String> {
    let ops = state.sftp_ops_for(&session_id).await?;
    let listed = ops.list(&path).await.map_err(|e| e.to_string())?;
    Ok(apply_exclude(listed, exclude.as_deref()))
}

/// 过滤在**上限闸之后**：先截断再过滤，20 万条目的目录不会因为过滤器而被全量遍历。
///
/// 代价是可见列表可能为空而 `truncated` 仍为 true——这不是 bug 而是实情：
/// 「前 20 000 条里没有符合的，后面还有没显示的」。反过来（先过滤再截断）会让
/// `truncated` 这个字段失去意义，用户以为看到了全貌。
fn apply_exclude(
    mut listed: fs_sshengine::sftp::ListResult,
    exclude: Option<&str>,
) -> fs_sshengine::sftp::ListResult {
    let Some(raw) = exclude else { return listed };
    let filter = fs_sshengine::filter::ExcludeFilter::parse(raw);
    if filter.is_empty() {
        return listed;
    }
    listed
        .entries
        .retain(|e| !filter.excludes(&e.name, e.is_dir));
    listed
}

#[tauri::command]
pub async fn sftp_mkdir(
    session_id: String,
    path: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let ops = state.sftp_ops_for(&session_id).await?;
    ops.mkdir(&path).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sftp_remove(
    session_id: String,
    path: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let ops = state.sftp_ops_for(&session_id).await?;
    // 审计2 #17：目录递归删除（守卫递归：截断拒绝/深度上限/根拒绝）。旧实现只调 remove
    //（SSH_FXP_REMOVE 文件原语）——面板允许选中目录后点删除必然失败，空目录也没有 rmdir 路径。
    fs_sshengine::sftp::remove_tree(ops.as_ref(), &path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sftp_rename(
    session_id: String,
    from: String,
    to: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let ops = state.sftp_ops_for(&session_id).await?;
    ops.rename(&from, &to).await.map_err(|e| e.to_string())
}

/// 远端条目属性（M4a）。组合语义（lstat vs stat 的分工、读链失败仍给链自身属性）
/// 在引擎层 [`fs_sshengine::sftp::describe_entry`]，那里能被真实 sftp-server 的 itest 证伪；
/// 本命令是薄封装，不复制那套判断——复制出来的第二份迟早与第一份分叉。
#[tauri::command]
pub async fn sftp_stat_entry(
    session_id: String,
    path: String,
    state: State<'_, Arc<AppState>>,
) -> Result<fs_sshengine::sftp::EntryDetail, String> {
    let ops = state.sftp_ops_for(&session_id).await?;
    fs_sshengine::sftp::describe_entry(ops.as_ref(), &path)
        .await
        .map_err(|e| e.to_string())
}

/// 建软链（M4a「新建软链」菜单）。
///
/// 参数名照 POSIX `symlink(2)` 的语义命名：`link_path` 是**被创建**的那个条目，
/// `target` 是它**指向**的路径。两者搞反的后果不是报错而是建出一个反向链
///（在 `target` 处建了个指向 `link_path` 的链），且多半立刻被当成断链——
/// 故这一层不做任何参数重排，顺序的唯一权威在 `SftpOps::symlink` 的实现注释里
///（russh-sftp 与 OpenSSH 两次字段反转相消，那里有完整说明）。
///
/// 服务端不支持时（Windows OpenSSH 等）原样透传 `Error::Sftp` 文本：这是服务端能力
/// 问题，替它编一句「不支持」的话会掩盖真实回复码。
#[tauri::command]
pub async fn sftp_symlink(
    session_id: String,
    target: String,
    link_path: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let ops = state.sftp_ops_for(&session_id).await?;
    ops.symlink(&target, &link_path)
        .await
        .map_err(|e| e.to_string())
}

/// 本地条目的属性（M4a）。走同一把浏览根闸（`ensure_within`），因此**不能**用它
/// 去 stat 根外的路径——属性对话框是个读取原语，读取原语一样要受根约束。
///
/// 注意 `ensure_within` 内部会 canonicalize：对软链，它给出的是**目标**的规范路径。
/// 所以这里的 lstat 不能对它的返回值做，否则永远看不到链自身 —— 用「根内校验通过」
/// 这个结论 + 原始路径的 lstat。
#[tauri::command]
pub async fn local_stat_entry(path: String) -> Result<fs_sshengine::sftp::EntryDetail, String> {
    use fs_sshengine::sftp::{FileMeta, FileType};
    let root = browse_root();
    let root_c =
        std::fs::canonicalize(&root).map_err(|e| format!("browse root unavailable: {e}"))?;
    let raw = if path.trim().is_empty() {
        root_c.clone()
    } else if Path::new(&path).is_absolute() {
        PathBuf::from(&path)
    } else {
        root_c.join(&path)
    };
    // 越界判定用**父目录**的规范路径 + 叶子名：对根内的软链，canonicalize(raw) 会跳到
    // 目标处，若目标在根外就会把「根内的一个链」误判成越界，而用户要看的恰恰是这个链。
    let parent = raw.parent().unwrap_or(&root_c);
    let parent_c = std::fs::canonicalize(parent).map_err(|e| e.to_string())?;
    if !parent_c.starts_with(&root_c) {
        return Err(format!("path escapes browse root: {}", parent_c.display()));
    }
    let md = std::fs::symlink_metadata(&raw).map_err(|e| e.to_string())?;
    let ft = if md.file_type().is_symlink() {
        FileType::Symlink
    } else if md.is_dir() {
        FileType::Dir
    } else if md.is_file() {
        FileType::Regular
    } else {
        FileType::Other
    };
    let meta = FileMeta {
        file_type: ft,
        size: md.len(),
        mtime: md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        // Windows 没有 POSIX mode/uid/gid：如实给 None（面板显示「—」），
        // 不要编一个 0o644 出来——那会让用户以为读到了真实权限。
        mode: local_mode(&md),
        uid: local_uid(&md),
        gid: local_gid(&md),
    };
    let (link_target, target_meta) = if ft == FileType::Symlink {
        let t = std::fs::read_link(&raw)
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        let tm = std::fs::metadata(&raw).ok().map(|m| FileMeta {
            file_type: if m.is_dir() {
                FileType::Dir
            } else if m.is_file() {
                FileType::Regular
            } else {
                FileType::Other
            },
            size: m.len(),
            mtime: m
                .modified()
                .ok()
                .and_then(|x| x.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            mode: local_mode(&m),
            uid: local_uid(&m),
            gid: local_gid(&m),
        });
        (t, tm)
    } else {
        (None, None)
    };
    Ok(fs_sshengine::sftp::EntryDetail {
        path: raw.to_string_lossy().into_owned(),
        meta,
        link_target,
        target_meta,
    })
}

#[cfg(unix)]
fn local_mode(md: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(md.mode() & 0o7777)
}
#[cfg(not(unix))]
fn local_mode(_md: &std::fs::Metadata) -> Option<u32> {
    None
}
#[cfg(unix)]
fn local_uid(md: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(md.uid())
}
#[cfg(not(unix))]
fn local_uid(_md: &std::fs::Metadata) -> Option<u32> {
    None
}
#[cfg(unix)]
fn local_gid(md: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(md.gid())
}
#[cfg(not(unix))]
fn local_gid(_md: &std::fs::Metadata) -> Option<u32> {
    None
}

/// 审计2 #38：本地目录列表的分页契约（与远端 `fs_sshengine::sftp::ListResult` 同构，
/// 同一把上限 [`fs_sshengine::sftp::LIST_ENTRY_CAP`]）。`truncated = true` 时列表不完整，
/// 前端必须如实呈现——本地盘同样可能出现数十万条目的目录（node_modules 之流），
/// 全量收集 + 全量排序 + 全量渲染的卡死链在本地侧一样成立。
#[derive(serde::Serialize)]
pub struct LocalListResult {
    pub entries: Vec<LocalEntry>,
    pub truncated: bool,
}

#[tauri::command]
pub async fn local_list(path: String, exclude: Option<String>) -> Result<LocalListResult, String> {
    // 根约束（spec §3.3）：唯一实现在 `ensure_within`（空路径 = 用户目录、相对路径以用户目录解析、
    // canonicalize 后须仍在根内）；本处不得再内联校验——R55 去双份
    let canon = ensure_within(&browse_root(), &path)?;
    // 审计2 #38：上限闸在**排序之前**——先截后排，50 万条目的目录不做 50 万次比较。
    // 恰好 cap 条不算截断，多取到的那 1 条只用于判定（与引擎 `cap_entries` 同口径）。
    let (mut out, truncated) = cap_local(
        std::fs::read_dir(&canon)
            .map_err(|e| e.to_string())?
            .map(|e| {
                let e = e.map_err(|e| e.to_string())?;
                let meta = e.metadata().map_err(|e| e.to_string())?;
                Ok(LocalEntry {
                    name: e.file_name().to_string_lossy().into_owned(),
                    is_dir: meta.is_dir(),
                    size: meta.len(),
                })
            }),
        LIST_CAP,
    )?;
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    // 与远端同一个匹配器（M4a 排除过滤器）：本地侧也过滤，否则「过滤器」在两栏里
    // 表现不一致，而用户拖拽/上传的来源正是本地栏。
    if let Some(raw) = exclude.as_deref() {
        let filter = fs_sshengine::filter::ExcludeFilter::parse(raw);
        if !filter.is_empty() {
            out.retain(|e| !filter.excludes(&e.name, e.is_dir));
        }
    }
    Ok(LocalListResult {
        entries: out,
        truncated,
    })
}

const LIST_CAP: usize = fs_sshengine::sftp::LIST_ENTRY_CAP;

/// 审计2 #38：本地列表的条数上限 + 截断判定的纯函数（`local_list` 与单测共用）。
///
/// **惰性**消费 `items`：第 cap+1 条一到立即停，50 万条目的目录不做 50 万次收集与排序。
/// `items` 里的 `Err` 原样上抛——包括那第 cap+1 条自身的错误，与旧实现「先入列再查超限」
/// 的逐条传播语义一字不差；第 cap+2 条起不再被读取，其错误自然也不会冒出（旧实现同）。
fn cap_local<I>(items: I, cap: usize) -> Result<(Vec<LocalEntry>, bool), String>
where
    I: IntoIterator<Item = Result<LocalEntry, String>>,
{
    let mut out: Vec<LocalEntry> = Vec::with_capacity(cap);
    for item in items {
        let entry = item?;
        if out.len() >= cap {
            return Ok((out, true));
        }
        out.push(entry);
    }
    Ok((out, false))
}

#[derive(serde::Serialize, Debug)]
pub struct LocalEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// 远端目录的递归枚举（M4a 文件夹拖拽传输）。守卫与语义在引擎层
/// [`fs_sshengine::sftp::walk_files`]：列表截断/深度/条数三条上限拒绝而非截断，
/// 软链跳过并在 `skipped_links` 里如实上报。
#[tauri::command]
pub async fn sftp_walk(
    session_id: String,
    path: String,
    exclude: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<fs_sshengine::sftp::WalkResult, String> {
    let ops = state.sftp_ops_for(&session_id).await?;
    let filter = fs_sshengine::filter::ExcludeFilter::parse(exclude.as_deref().unwrap_or(""));
    fs_sshengine::sftp::walk_files(ops.as_ref(), &path, &filter)
        .await
        .map_err(|e| e.to_string())
}

/// 本地目录的递归枚举（M4a 文件夹拖拽上传）。
///
/// 与远端版**同样的上限与软链策略**，理由也一样：软链跟随会把浏览根之外的内容传出去
///（本地侧尤其危险——一个指向 `C:\` 的链就把整盘暴露给了远端），条数超限如实报错而
/// 不是截断。根约束仍走 `ensure_within`（属性/列表同一把闸）。
#[tauri::command]
pub async fn local_walk(
    path: String,
    exclude: Option<String>,
) -> Result<fs_sshengine::sftp::WalkResult, String> {
    let root = ensure_within(&browse_root(), &path)?;
    let filter = fs_sshengine::filter::ExcludeFilter::parse(exclude.as_deref().unwrap_or(""));
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut skipped_links = Vec::new();
    // 迭代（非递归）+ 深度上限：本地也可能有指向祖先的链造出的环，以及病态深树。
    let mut stack = vec![(root.clone(), String::new(), 0usize)];
    while let Some((dir, rel, depth)) = stack.pop() {
        if depth >= fs_sshengine::sftp::REMOVE_TREE_MAX_DEPTH {
            return Err(format!(
                "目录嵌套超过 {} 层，拒绝继续枚举：{}",
                fs_sshengine::sftp::REMOVE_TREE_MAX_DEPTH,
                dir.display()
            ));
        }
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            // symlink_metadata：不跟随（跟随即前述的整盘暴露风险）
            let md = entry.metadata_symlink_safe()?;
            let is_dir = md.is_dir();
            if filter.excludes(&name, is_dir) {
                continue;
            }
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if md.file_type().is_symlink() {
                skipped_links.push(child_rel);
                continue;
            }
            if is_dir {
                dirs.push(child_rel.clone());
                stack.push((entry.path(), child_rel, depth + 1));
            } else {
                if files.len() >= fs_sshengine::sftp::WALK_ENTRY_MAX {
                    return Err(format!(
                        "待传文件超过 {} 个，拒绝入队：{}（请分批传输或用排除过滤器缩小范围）",
                        fs_sshengine::sftp::WALK_ENTRY_MAX,
                        root.display()
                    ));
                }
                files.push(child_rel);
            }
        }
    }
    Ok(fs_sshengine::sftp::WalkResult {
        files,
        dirs,
        skipped_links,
    })
}

/// `DirEntry::metadata()` 在 Windows 上**跟随**软链（std 文档：Windows 上等价于
/// `fs::metadata`），Unix 上不跟随。差异会让「跳过软链」这条策略只在 Unix 生效——
/// 而它在 Windows 上防的正是「一个指向 C:\ 的链把整盘传出去」。故一律显式用
/// `symlink_metadata`。
trait SymlinkSafeMeta {
    fn metadata_symlink_safe(&self) -> Result<std::fs::Metadata, String>;
}
impl SymlinkSafeMeta for std::fs::DirEntry {
    fn metadata_symlink_safe(&self) -> Result<std::fs::Metadata, String> {
        std::fs::symlink_metadata(self.path()).map_err(|e| e.to_string())
    }
}

/// 相对远端路径在沙箱里的落脚目录（审计2 #14）。
///
/// 远端 `./a.bin`（SFTP 会话起始目录下的 a.bin）与 `/a.bin`（根目录下的 a.bin）是**两个文件**，
/// 而 SFTP 面板的初始远端路径正是 `"."`（见 `SftpPane.svelte` 的 `remotePath`），
/// 用户点一下「上级」再进别的目录也仍在相对分支上——两种拼法在日常使用中都会出现。
/// 少了这一层，它们会落到同一条本地路径上，后下载的静默覆盖先下载的。
///
/// 名字选 `%rel` 是有讲究的：`escape_local_component` 的输出里每个 `%` 后面必跟两位十六进制，
/// 而 `r` 不是十六进制数字——所以**任何**远端目录名都不可能被转义成 `%rel`，
/// 这一层不会与某个真实目录重名。不能用 `.` 或 `..` 之类：那会被文件系统直接吃掉。
const RELATIVE_ROOT: &str = "%rel";

/// 由远端路径生成沙箱内的安全相对路径（审计 P0-3、审计2 #14）。
///
/// 契约是**单射**：两条不同的远端路径必得两条不同的本地路径。这一条是「下载完成」这句话
/// 有意义的前提——映射一旦坍缩，两件传输都报 Done 而磁盘上只剩一份，用户分不清剩的是哪一份。
///
/// P0-3 修掉的是最粗的一档：原实现只取 basename，`/etc/config.json` 与 `/home/u/config.json`
/// 落到同一个 `<sandbox>/config.json`。审计2 #14 命中的是留下来的四类细缝：
///
/// 1. **字符清洗有损**（在 `sandbox::escape_local_component` 修）：`a:b`/`a?b`/`a_b` 塌成一个。
/// 2. **`\` 被当成分隔符**：POSIX 上 `\` 是**合法文件名字符**，一个名叫 `a\b` 的远端文件
///    会长出一层本地目录，与远端真正的 `a/b` 撞在同一条本地路径上；更糟的是它让服务端
///    凭一个文件名就能在沙箱里造目录结构。故此处**只按 `/` 分段**——那是 SFTP 唯一的分隔符。
///    远端若是 Windows 服务器，`C:\x\y` 会退化成一个又长又丑但**正确且不碰撞**的段。
/// 3. **`..` 被当空段丢掉**：`/a/../b` 其实就是 `/b`，与 `/a/b` 是两个文件，原实现让两者
///    都落到 `a/b`。改为与引擎的 `transfer::normalize_remote` **逐字相同**的词法消解——
///    两处对「哪两条远端路径是同一个文件」的判断必须一致，否则同目标互斥与落点映射会分叉。
/// 4. **相对与绝对不分**：见 `RELATIVE_ROOT`。
///
/// 消解后仍留在开头的 `..`（相对路径 `../x`，客户端无从知道会话起始目录在哪，消不掉）
/// 交由 `escape_local_component` 转义成 `%2E%2E`，于是它在本地是个**普通目录名**，
/// 既不穿越沙箱，也不与远端真正的 `x` 混淆。
///
/// 返回 None 只有一种情形：一个段都没有（`""`、`"/"`、`"."`）。那指的是目录本身而非其中的
/// 文件，没有可下载的对象。原实现还会因为「某段清洗后为空」（`...`、`   `）整条拒绝，
/// 那是一次可用性损失——现在这些段各有确定且可逆的像，不必再拒。
pub fn safe_relative_path(remote: &str) -> Option<PathBuf> {
    let absolute = remote.starts_with('/');
    let mut segs: Vec<&str> = Vec::new();
    for seg in remote.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                // 前一段是真目录名才弹；栈顶已是 `..`（或为空）就弹不动了。
                // 绝对路径的 `/..` 是根本身，直接吞掉；相对路径的 `../` 消不掉，留着。
                if segs.last().is_some_and(|s| *s != "..") {
                    segs.pop();
                } else if !absolute {
                    segs.push("..");
                }
            }
            s => segs.push(s),
        }
    }
    if segs.is_empty() {
        return None;
    }
    let mut out = PathBuf::new();
    if !absolute {
        out.push(RELATIVE_ROOT);
    }
    for s in segs {
        out.push(sandbox::escape_local_component(s));
    }
    Some(out)
}

/// `transfer:submitted` 载荷构造。抽成纯函数只为一件事：让 `state` 初值可被单测锁定。
/// 它内联在命令体里时无法覆盖（构造载荷要先有 AppHandle 与真实会话），而这个字段一旦
/// 回退成 Running 或被删掉，关闭确认框的「排队 N」就静默归零，没有任何编译期信号。
///
/// `state` 取字符串 `"Queued"`：与引擎 `TransferState` 的 unit 变体（`Running`/`Done`/…）
/// 序列化形态一致，前端那句 `typeof p.state === "string" ? p.state : Object.keys(p.state)[0]`
/// 可以原样吃下，不必为提交事件单开一条解析分支。
fn submitted_payload(
    id: u64,
    session_id: &str,
    direction: &str,
    local: &str,
    remote: &str,
) -> serde_json::Value {
    serde_json::json!({
        "id": id, "sessionId": session_id, "direction": direction,
        "local": local, "remote": remote,
        // 审计 P2：提交 ≠ 开跑。见 transfer_submit 内 emit 处的说明。
        "state": "Queued",
    })
}

#[tauri::command]
pub async fn transfer_submit(
    session_id: String,
    direction: String,
    local: String,
    remote: String,
    resume_offset: u64,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<u64, String> {
    // 端点键取自注册表里那条**活着的**连接（装配时定格），不回查数据库：profile 可改，
    // 回查会让同一条连接上的前后两件传输拿到两把锁（见 state::transfer_endpoint_of）。
    // 这一步同时兼作会话存在性校验——取不到 LiveSession 就没有可传的连接。
    //
    // 此处原先还先取一次代次「贯穿全程」（审计 P0-4），是为了让管理器与锁命名空间同属一条
    // 连接；锁键改成物理端点之后它不再有消费者：端点与代次无关，而管理器查表本就在
    // `transfer_manager_for` 内部自取代次并在装配后复核一次（那才是真正会被重连打断的地方）。
    let (target_endpoint, endpoint_aliases) = {
        let live = state.registry.get(&session_id).ok_or("no session")?;
        (live.target_endpoint.clone(), live.endpoint_aliases.clone())
    };
    let mgr = state.transfer_manager_for(&session_id).await?;
    let direction_str = direction.clone();
    let direction = match direction.as_str() {
        "up" => Direction::Up,
        "down" => Direction::Down,
        other => return Err(format!("invalid direction: {other}")),
    };
    let (local_path, sandbox_root) = match direction {
        Direction::Up => {
            // 上传源必须位于本地浏览根（用户目录）内；相对路径以浏览根解析——与 local_list 共用 ensure_within（R55）
            let src =
                ensure_within(&browse_root(), &local).map_err(|e| format!("upload source: {e}"))?;
            (src, None)
        }
        Direction::Down => {
            // 下载沙箱（spec §3.3）：远端路径服务端可控——**保留目录结构**逐段清洗后拼入沙箱
            //（审计 P0-3：只取 basename 会让不同目录下的同名文件互相覆盖），目的父目录建好后
            // 仍经 canonicalize 校验须在沙箱内（双保险，fail-closed）。
            // 前端 local 参数在 down 方向被忽略——Rust 侧是唯一可信边界。
            let root = state.download_sandbox_for(&session_id).await?;
            let rel = safe_relative_path(&remote)
                .ok_or_else(|| format!("unsafe remote path: {remote}"))?;
            let dest_raw = root.join(&rel);
            // 先建父目录再校验：`resolve_within` 要 canonicalize 父目录，目录不存在必然失败。
            // 建目录只会在沙箱根之下展开（rel 已剔除全部穿越段），随后的校验是对这一点的复核。
            let parent = dest_raw.parent().ok_or_else(|| {
                format!(
                    "download destination has no parent dir: {}",
                    dest_raw.display()
                )
            })?;
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create sandbox dir {}: {e}", parent.display()))?;
            let dest = sandbox::resolve_within(&root, &dest_raw).map_err(|e| e.to_string())?;
            (dest, Some(root))
        }
    };
    // 传后校验（UI 规格 §1.4 / 路线图 M1 出口，默认开）：取期望内容哈希入 TransferJob.verify（携带意图）
    // 与 verify_plans 表（完成时弹出执行）。禁止静默跳过——预取失败置 degraded，完成时降级 size 对比。
    let (verify, plan) = if state
        .settings_bool("transfer.verifyAfterTransfer", true)
        .await
    {
        match direction {
            Direction::Up => {
                let h = fs_sshengine::verify::file_sha256(&local_path)
                    .await
                    .map_err(|e| format!("hash local file: {e}"))?;
                (
                    Some(h.clone()),
                    Some(fs_sshengine::verify::VerifyPlan {
                        expect: Some(h),
                        degraded: false,
                    }),
                )
            }
            Direction::Down => {
                let exec = state.exec_adapter_for(&session_id).await?;
                match fs_sshengine::verify::remote_sha256_via(&*exec, &remote).await {
                    Ok(h) => (
                        Some(h.clone()),
                        Some(fs_sshengine::verify::VerifyPlan {
                            expect: Some(h),
                            degraded: false,
                        }),
                    ),
                    // 命令缺失/exec 失败 → 不阻塞传输：degraded，完成时 size 对比（UI 显式标注「降级」）
                    Err(_) => (
                        None,
                        Some(fs_sshengine::verify::VerifyPlan {
                            expect: None,
                            degraded: true,
                        }),
                    ),
                }
            }
        }
    } else {
        (None, None)
    };
    // 校验意图在提交**之前**就备好（审计 P1-2）：`submit` 返回的瞬间作业可能已经跑完，
    // 备料动作绝不能排在拿到 id 之后再做。
    let mut entry = plan.map(|plan| fs_sshengine::verify::VerifyEntry {
        direction,
        remote: remote.clone(),
        local: local_path.clone(),
        plan,
    });
    let id = mgr
        .submit(TransferJob {
            direction,
            local: local_path.clone(),
            remote: remote.clone(),
            // 引擎只收「想不想续传」（审计 P1-5）：真实起点由它按 `.fspart` 实际长度重算。
            // 前端仍传 resumeOffset（0 = 新传、非 0 = 从抽屉里点重试续跑），此处折成布尔意愿。
            resume: resume_offset > 0,
            sandbox_root,
            // per-target 锁的端点键（审计 P0-3 / P1）：精确到「哪台服务器」，**不含**会话身份——
            // 同一台主机的两条连接必须共用一把锁，否则会交错写同一个 .fspart 且两件都报 Done。
            // 见 state::transfer_endpoint_of 与 sessions::LiveSession::target_endpoint。
            target_endpoint,
            // 与上同源、同样在装配时定格的端点别名（审计2 #13）：域名与 IP 两种连法
            // 在地址那一维重合，从而共用一把锁。走跳板或解析失败时为空。
            endpoint_aliases,
            verify, // 期望内容哈希；TransferManager 只携带，执行见 spawn_transfer_pump 的 Done 分支
        })
        .await
        // P1-1：submit 现在会因「管理器已关停 / 派发任务已退出」拒收，拒收即如实报错——
        // 吞掉它就会在 UI 上留下一行永远 0% 的幽灵传输。
        .map_err(|e| e.to_string())?;
    // 登记与终态的原子握手（审计 P1-2），判定见 `state::verify_register`。单锁内二选一：
    // ① 事件泵还没见到终态 → 落 Planned，由它稍后取走执行；
    // ② 终态已先到（小文件常见）→ 泵留下了 Settled 标记，此处取走并自行驱动校验。
    // 不采用「持 verify_plans 锁跨 submit」的写法：submit 在作业队列满时会 await，而队列满
    // 恰恰是因为 worker 阻塞在事件发送上、事件泵又阻塞在这把锁上——那是一个真实的死锁环。
    //
    // `entry` 为 None（用户关掉 `transfer.verifyAfterTransfer`）时**照样登记**：见
    // `PendingVerify::entry` 的文档注（审计 P2「Settled 孤儿」路径一）。
    let outcome = {
        let mut plans = state.verify_plans.lock().await;
        crate::state::verify_register(&mut plans, id, || crate::state::PendingVerify {
            session_id: session_id.clone(),
            entry: entry.take(),
        })
    };
    if let crate::state::RegisterOutcome::AlreadySettled { done: true } = outcome {
        // 登记没发生（标记被取走），意图仍在手上——直接驱动校验。
        if let Some(entry) = entry.take() {
            spawn_verify(app.clone(), id, session_id.clone(), entry);
        }
    }
    // 抽屉行参数（UI 规格 §1.4 重试按钮）：Down 方向 local = 沙箱定值（前端参数被忽略，spec §3.3）
    //
    // `state: "Queued"` 是排队态可见性的修法（审计 P2）：`submit` 返回只代表作业**已入队**，
    // 引擎的并发信号量（3 路）完全可能让它在队列里等上很久。前端原先把新行硬编码成 Running，
    // 于是关闭确认框里的「排队 N」恒为 0——用户被告知「进行中 5」，实际只有 3 条在跑，
    // 另外 2 条连字节都没动过，取消的代价被显示得比真实情况严重。
    //
    // 只在 app 层的事件载荷里表达，不碰引擎的 `TransferState` 枚举：那是引擎侧契约，
    // 加一个变体会牵动所有 match 分支，而「排队」本就是提交侧才知道的信息（引擎在真正开跑
    // 时才发出第一条事件）。引擎发出的第一个 `Running` 进度事件会自然覆盖这个初值。
    let _ = app.emit(
        "transfer:submitted",
        submitted_payload(
            id,
            &session_id,
            &direction_str,
            &local_path.display().to_string(),
            &remote,
        ),
    );
    Ok(id)
}

#[tauri::command]
pub async fn transfer_cancel(
    session_id: String,
    id: u64,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let mgr = state.transfer_manager_for(&session_id).await?;
    mgr.cancel(id).await; // R105：`TransferId` 是 `pub type TransferId = u64` 类型别名，非元组结构体——`TransferId(id)` 是调用不存在的构造函数（编译错误）；`id: u64` 直接即是 `TransferId`
    Ok(())
}

/// 执行一件传后校验并发 `transfer_verified`（UI 规格 §1.4：禁止静默跳过，会话已丢失也产出
/// Unverified 标注）。事件泵与 `transfer_submit` 两条路径共用——校验意图与终态谁先到都可能，
/// 两侧各写一份执行逻辑迟早会分叉（比如只有一侧带上 sessionId）。
/// 收 `session_id` + `entry` 两个参数而非整个 `PendingVerify`：后者的 `entry` 现在是
/// `Option`（「登记了但不校验」也要占位，见其文档注），而这里只在确实要校验时才被调用——
/// 由类型把 None 挡在函数外，好过在函数里再 `else { return }` 一次。
fn spawn_verify(
    app: AppHandle,
    id: u64,
    session_id: String,
    entry: fs_sshengine::verify::VerifyEntry,
) {
    use tauri::Manager; // app.state::<T>() 位于 Manager trait
    tauri::async_runtime::spawn(async move {
        let st: Arc<AppState> = app.state::<Arc<AppState>>().inner().clone();
        let outcome = match (
            st.exec_adapter_for(&session_id).await,
            st.sftp_ops_for(&session_id).await,
        ) {
            (Ok(exec), Ok(ops)) => fs_sshengine::verify::run_verify(&*exec, &*ops, &entry).await,
            _ => fs_sshengine::verify::VerifyOutcome::Unverified, // 会话已丢失/已重连：显式「未核对」
        };
        let _ = app.emit(
            "transfer_verified",
            serde_json::json!({ "id": id, "sessionId": session_id, "outcome": outcome }),
        );
    });
}

/// per-(会话, 代次) 传输事件泵：会话首次取得 TransferManager 时调用，逐条转发为 Tauri event；
/// 终态（Done/Failed/Cancelled）结清校验槽位——Done 触发传后校验。
///
/// `session_id` 是必需参数（审计 P0-1）：`transfer:progress` 是全局事件，多会话同时传输时
/// 载荷里不带会话身份，前端只能把所有会话的进度混进一张表——A 会话的 80% 会覆盖 B 会话同 id
/// 行的显示。id 虽已是进程级唯一（引擎 P0-1），但前端要按会话分组/按会话清理，仍需这个字段。
pub fn spawn_transfer_pump(
    app: AppHandle,
    session_id: String,
    mut rx: tokio::sync::mpsc::Receiver<fs_sshengine::transfer::TransferEvent>,
) {
    use crate::state::SettleOutcome;
    use fs_sshengine::transfer::TransferState;
    use tauri::Manager; // app.state::<T>() 位于 Manager trait
    tauri::async_runtime::spawn(async move {
        // 本泵留下过 `Settled` 标记的作业 id（审计 P2「Settled 孤儿」）。
        // 流结束时按这批 id 回收，理由与「为什么不能按会话清」见 `state::verify_reap_settled`。
        let mut settled_here: Vec<u64> = Vec::new();
        while let Some(e) = rx.recv().await {
            // 载荷注入 sessionId（P0-1）。序列化后改对象而不是另建结构体：TransferEvent 的字段
            // 是引擎侧契约，另抄一份会在引擎加字段时静默漏传。
            let mut payload = serde_json::to_value(&e).unwrap_or_default();
            if let Some(obj) = payload.as_object_mut() {
                obj.insert(
                    "sessionId".to_string(),
                    serde_json::Value::String(session_id.clone()),
                );
            }
            let _ = app.emit("transfer:progress", payload);
            let id = e.id;
            let done = matches!(e.state, TransferState::Done);
            // Cancelled 也是终态（审计 P1-3）：原实现只认 Done/Failed，用户取消的那件作业
            // 其校验计划就永远留在表里——每取消一件泄漏一条，长会话下是无界增长，且
            // `shutdown_session_subsystems` 之外没有任何东西会来收。
            let finished =
                done || matches!(e.state, TransferState::Failed(_) | TransferState::Cancelled);
            if !finished {
                continue;
            }
            let state: Arc<AppState> = app.state::<Arc<AppState>>().inner().clone();
            // 与 `transfer_submit` 的登记握手（审计 P1-2），判定见 `state::verify_settle`：
            // 查不到计划不代表没有校验意图，只代表登记方还没跑到——留下 Settled 让它接手，
            // 绝不静默跳过。
            let outcome = {
                let mut plans = state.verify_plans.lock().await;
                crate::state::verify_settle(&mut plans, id, &session_id, done)
            };
            let pending = match outcome {
                SettleOutcome::Plan(p) => *p,
                SettleOutcome::Marked => {
                    settled_here.push(id);
                    continue;
                }
                SettleOutcome::Duplicate => continue,
            };
            if !done {
                continue; // Failed/Cancelled：管理器已重试 3 次或用户主动放弃，校验意图随作业作废
            }
            // None = 本作业本就不做校验（`transfer.verifyAfterTransfer` 关）：槽位已取走即完事。
            let Some(entry) = pending.entry else { continue };
            spawn_verify(app.clone(), id, pending.session_id, entry);
        }
        // 事件流结束 = 管理器与它的全部 worker 都没了，本代次不会再有任何事件；
        // 此刻仍留在表里的标记就是确定的孤儿（审计 P2 路径二）。
        if !settled_here.is_empty() {
            let state: Arc<AppState> = app.state::<Arc<AppState>>().inner().clone();
            let mut plans = state.verify_plans.lock().await;
            crate::state::verify_reap_settled(&mut plans, &settled_here);
        }
    });
}

// 浏览根逃逸覆盖（R55）：与 opener_cmd scheme 白名单单测同层先例对齐，纳入 app crate 门禁。
/* ── 本地栏：新建目录 / 重命名（M4b 第 15 项评估结论的落地）────────────────────
 *
 * 评估结论见路线图 §6.3（`docs/roadmap.md`）：
 * 三个操作的风险差着量级，所以**分拆**而不是一刀切——
 *
 *   新建目录：做。非破坏性，最坏是多个空目录；而它是下载前最常见的一步，
 *             现在用户得切出去开资源管理器建好再切回来，远程栏却有这个按钮。
 *   重命名：  做。非破坏性——文件还在，只是换了名字。
 *   删除：    **不做**。它是三个里唯一不可逆的；本地这一侧用户是在「用自己的电脑」，
 *             而自己的电脑上有回收站、我们这里没有；且 `sftp.sandboxRoot` 管的是
 *             **下载落点**，对删除一句话都没说——要做得先定义一个新的安全边界。
 *
 * 两个操作都走 `ensure_within`：本地文件操作的越界规则只该有一条。
 */

/// 本地新建目录。
///
/// `parent` 为空 = 浏览根。名字里不许有路径分隔符——那不是「新建目录」而是
/// 「在任意位置新建目录」，`ensure_within` 虽然会拦住逃逸，但把这一层也挡掉
/// 让错误信息说得清楚（「名字里不能有斜杠」比「越界」有用得多）。
#[tauri::command]
pub async fn local_mkdir(parent: String, name: String) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("目录名不能为空".into());
    }
    if name.contains(['/', '\\']) {
        return Err("目录名里不能有斜杠".into());
    }
    // `.` 与 `..` 会造出一个「建了但看不见」的结果（前者是当前目录本身，
    // 后者是上级）。挡掉，而不是让 create_dir 报一句晦涩的 OS 错误。
    if name == "." || name == ".." {
        return Err("目录名不能是 . 或 ..".into());
    }
    let base = ensure_within(&browse_root(), &parent)?;
    let target = base.join(name);
    // create_dir 而不是 create_dir_all：后者对「路径已存在且是目录」返回 Ok，
    // 于是用户点了「新建」却什么也没发生，而界面会说成功。
    std::fs::create_dir(&target).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!("「{name}」已经存在"),
        std::io::ErrorKind::PermissionDenied => {
            format!("没有权限在这里新建目录：{}", base.display())
        }
        _ => e.to_string(),
    })?;
    Ok(target.to_string_lossy().to_string())
}

/// 本地重命名（同目录内）。
///
/// **目标已存在就拒绝**，不问「要不要覆盖」。在文件管理器里覆盖是个明确的动作，
/// 在一个 SSH 工具的侧栏里不该有——用户来这儿是为了传文件，不是为了管理本地磁盘。
#[tauri::command]
pub async fn local_rename(parent: String, from: String, to: String) -> Result<(), String> {
    let to = to.trim();
    if to.is_empty() {
        return Err("新名字不能为空".into());
    }
    if to.contains(['/', '\\']) || to == "." || to == ".." {
        return Err("新名字里不能有斜杠，也不能是 . 或 ..".into());
    }
    if from.trim().is_empty() || from.contains(['/', '\\']) {
        return Err("原名字不合法".into());
    }
    let base = ensure_within(&browse_root(), &parent)?;
    let src = base.join(from.trim());
    let dst = base.join(to);
    // 先判存在再改：`rename` 在多数平台上会**静默覆盖**目标。
    // 这里有 TOCTOU 窗口（判完到改之间目标可能被创建），但本地栏不是并发场景，
    // 而这道检查挡的是「用户输了个已存在的名字」——那是真实且高频的。
    if dst.exists() {
        return Err(format!("「{to}」已经存在"));
    }
    if !src.exists() {
        return Err(format!("「{}」不存在（可能已被别处改动）", from.trim()));
    }
    std::fs::rename(&src, &dst).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => "没有权限重命名（文件可能正被占用）".to_string(),
        _ => e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        cap_local, ensure_within, safe_relative_path, sandbox, submitted_payload, LocalEntry,
        RELATIVE_ROOT,
    };
    use std::path::PathBuf;

    /// P2 排队态可见：提交事件必须自带 `state: "Queued"`。
    /// 前端据此把新行初始化为排队而非 Running；引擎真正开跑时的第一条 `Running` 进度事件覆盖它。
    #[test]
    fn submitted_payload_marks_queued() {
        let v = submitted_payload(7, "s1", "down", "C:/sbx/a.bin", "/srv/a.bin");
        assert_eq!(v["state"], "Queued");
        assert_eq!(v["id"], 7);
        assert_eq!(v["sessionId"], "s1");
        assert_eq!(v["direction"], "down");
        assert_eq!(v["local"], "C:/sbx/a.bin");
        assert_eq!(v["remote"], "/srv/a.bin");
    }

    /// P0-3 核心回归：不同远端目录下的同名文件必须落到**不同**的沙箱内路径。
    /// 只取 basename 的旧实现会让这两条断言的左右两侧相等。
    #[test]
    fn safe_relative_path_keeps_remote_dirs_apart() {
        let a = safe_relative_path("/etc/config.json").unwrap();
        let b = safe_relative_path("/home/user/config.json").unwrap();
        assert_ne!(a, b, "同名不同源必须落到不同本地路径");
        assert_eq!(a, PathBuf::from("etc").join("config.json"));
        assert_eq!(b, PathBuf::from("home").join("user").join("config.json"));
    }

    /// 审计2 #14：`..` 要**消解**而不是当空段丢掉。
    /// `/srv/../pub/data.bin` 指的就是 `/pub/data.bin`，与 `/srv/pub/data.bin` 是两个文件；
    /// 原实现把 `..` 跳过，两者一起落到 `srv/pub/data.bin`，后到的静默覆盖先到的。
    /// 消解规则与引擎的 `transfer::normalize_remote` 逐字相同——两处对「哪两条远端路径是
    /// 同一个文件」的判断必须一致，否则同目标互斥与落点映射会分叉。
    #[test]
    fn safe_relative_path_resolves_dotdot_instead_of_dropping_it() {
        let p = safe_relative_path("//srv/../pub//./data.bin").unwrap();
        assert_eq!(p, PathBuf::from("pub").join("data.bin"));
        assert!(p.is_relative());
        assert!(!p.components().any(|c| c.as_os_str() == ".."));
        assert_ne!(
            p,
            safe_relative_path("/srv/pub/data.bin").unwrap(),
            "消解后的路径与字面保留 srv 的路径必须分开"
        );
        // 绝对路径上越过根的 `..` 被吞掉（`/..` 就是 `/`），与 POSIX 一致。
        assert_eq!(safe_relative_path("/../../a").unwrap(), PathBuf::from("a"));
    }

    /// 一个段都没有 → 拒绝。那指的是目录本身而非其中的文件，没有可下载的对象。
    /// 相对路径里消不掉的前导 `..` **不**属于此列：它是个有效目标（会话起始目录的上级里的文件），
    /// 只是客户端算不出绝对路径，故转义成普通目录名收进沙箱，见下一条用例。
    #[test]
    fn safe_relative_path_rejects_only_pathless_inputs() {
        assert!(safe_relative_path("///").is_none());
        assert!(safe_relative_path("").is_none());
        assert!(safe_relative_path(".").is_none());
        assert!(safe_relative_path("/").is_none());
    }

    /// 相对标记层必须落在 `escape_local_component` 的**值域之外**，否则某个远端目录名
    /// 会被转义成它、与它重名，而那正是这一层要挡的碰撞。
    ///
    /// 判据不是「试几个名字」而是形状：编码器的像里每个 `%` 后面必跟两位十六进制，
    /// 所以「解码失败」等价于「不是任何输入的像」——`unescape` 回 None 就是完整的证明。
    /// 这条也顺带把常量的**取值**钉住：底下几条用例若拿 `RELATIVE_ROOT` 去拼期望值，
    /// 常量被改成 `rel` 时它们会跟着改口径、一起绿，故那些期望值一律写字面量。
    #[test]
    fn relative_root_marker_is_outside_the_escapers_image() {
        assert_eq!(RELATIVE_ROOT, "%rel");
        assert_eq!(
            sandbox::unescape_local_component(RELATIVE_ROOT),
            None,
            "标记层可被解码，说明它是某个远端名字的像，会与那个名字撞车"
        );
    }

    /// 审计2 #14：消不掉的前导 `..` 被转义成普通目录名，既不穿越沙箱，也不与真名混淆。
    #[test]
    fn safe_relative_path_escapes_unresolvable_leading_dotdot() {
        let p = safe_relative_path("../../x").unwrap();
        assert_eq!(
            p,
            PathBuf::from("%rel")
                .join("%2E%2E")
                .join("%2E%2E")
                .join("x")
        );
        assert!(!p.components().any(|c| c.as_os_str() == ".."));
        assert_ne!(
            p,
            safe_relative_path("../x").unwrap(),
            "上两级与上一级是两个目录"
        );
    }

    /// 逐段转义：Windows 保留设备名与非法字符不得原样落地，否则 `<sandbox>/NUL/x`
    /// 会被 CreateFileW 解析成空设备（字节静默丢弃）。
    #[test]
    fn safe_relative_path_escapes_each_segment() {
        let p = safe_relative_path("/NUL/a:b/c.txt").unwrap();
        assert_eq!(p, PathBuf::from("%4EUL").join("a%3Ab").join("c.txt"));
    }

    /// P0-3 复核：保留设备名在**中间路径段**上同样要消解，且判定与大小写、扩展名无关。
    /// 逐段转义已经保证了这一点，本例把这个隐含依赖钉成显式回归——若哪天
    /// `safe_relative_path` 改成「只处理末段」，`<sandbox>/com1/aux.d/x` 里的中间段会被
    /// CreateFileW 解析成设备，整条路径打开即失败或阻塞（服务端可控输入 → 可触发的 DoS）。
    #[test]
    fn safe_relative_path_escapes_reserved_names_in_middle_segments() {
        let p = safe_relative_path("/com1/AUX.d/Lpt9.tar.gz/x.bin").unwrap();
        assert_eq!(
            p,
            PathBuf::from("%63om1")
                .join("%41UX.d")
                .join("%4Cpt9.tar.gz")
                .join("x.bin")
        );
    }

    /// P0-3 复核：结果恒为相对路径——否则 `root.join(rel)` 会被 `Path::join` 的绝对路径语义
    /// 整个替换掉 root，沙箱当场失效。反斜杠与 Windows 盘符同样不得逃出：两者都被转义成
    /// `%5C` / `%3A`，不构成分隔符或盘符前缀。
    #[test]
    fn safe_relative_path_never_yields_absolute() {
        for raw in ["/etc/x", "\\\\srv\\share\\x", "C:\\Windows\\x", "../../x"] {
            let p = safe_relative_path(raw).unwrap_or_else(|| panic!("{raw} 应产出有效相对路径"));
            assert!(p.is_relative(), "{raw} → {}", p.display());
            assert!(
                !p.components().any(|c| c.as_os_str() == ".."),
                "{raw} → {}",
                p.display()
            );
            let joined = PathBuf::from("/sbx").join(&p);
            assert!(joined.starts_with("/sbx"), "{raw} → {}", joined.display());
        }
    }

    /// 审计2 #14：`..` 的形近变体现在**被接受**，各自落到互不相同的本地名字上。
    /// 原实现把它们清成空串后整条路径拒绝——那是可用性损失（`/a/.../b` 是完全合法的远端路径），
    /// 而拒绝的理由「跳过该段会塌缩」在可逆转义下已不成立。
    #[test]
    fn safe_relative_path_keeps_dotdot_lookalikes_distinct() {
        let dots = safe_relative_path("/a/.../b").unwrap();
        let dotdot_space = safe_relative_path("/a/.. /b").unwrap();
        let spaces = safe_relative_path("/a/   /b").unwrap();
        assert_eq!(dots, PathBuf::from("a").join("%2E%2E%2E").join("b"));
        assert_eq!(dotdot_space, PathBuf::from("a").join("%2E%2E%20").join("b"));
        assert_eq!(spaces, PathBuf::from("a").join("%20%20%20").join("b"));
        // 与真正的 `..`（消解成 `a` 的上级，即 `b`）也必须分开
        assert_ne!(dots, safe_relative_path("/a/../b").unwrap());
    }

    /// 审计2 #14 的核心契约：**单射**。两条不同的远端路径必得两条不同的本地路径。
    ///
    /// 这份清单里每一对都是原实现会撞在一起的：有损字符替换（`a:b`/`a?b`/`a_b`）、
    /// 尾部点空格剥离（`foo.`/`foo `/`foo`）、保留名加 `_` 前缀（`NUL`/`_NUL`）、
    /// 反斜杠当分隔符（`a\b`/`a/b`）、`..` 当空段（`x/../y`/`x/y`）、相对与绝对不分
    ///（`./a.bin`/`/a.bin`）。逐对断言写不完，故直接断言全体两两不同。
    #[test]
    fn safe_relative_path_is_injective_over_colliding_remotes() {
        let remotes = [
            "/a:b",
            "/a?b",
            "/a_b",
            "/a*b",
            "/a\"b",
            "/a<b",
            "/a>b",
            "/a|b",
            "/a%b",
            "/a%3Ab",
            "/foo",
            "/foo.",
            "/foo ",
            "/foo..",
            "/NUL",
            "/_NUL",
            "/%4EUL",
            "/nul",
            "/a\\b",
            "/a/b",
            "/x/../y",
            "/x/y",
            "./a.bin",
            "/a.bin",
            "/dir/a.bin",
            "../a.bin",
            "/CON.txt",
            "/console.txt",
            "/...",
            "/.. ",
            "/   ",
            "/.hidden",
            "/a. b",
            // 相对标记层与真实目录名不得重名：`%rel` 选得能不能站住，就看这三条
            "/rel/a.bin",
            "/%rel/a.bin",
            "./rel/a.bin",
        ];
        // `a.bin` 与 `./a.bin` 刻意**不**在上面这份清单里：它们是同一个远端文件的两种写法，
        // 落到同一条本地路径才是对的，下面用等式钉住。
        let mut seen: std::collections::HashMap<PathBuf, &str> = std::collections::HashMap::new();
        for r in remotes {
            let p = safe_relative_path(r).unwrap_or_else(|| panic!("{r} 应产出有效相对路径"));
            if let Some(prev) = seen.insert(p.clone(), r) {
                panic!("碰撞：{prev:?} 与 {r:?} 都落到 {}", p.display());
            }
        }
        // `.` 起头与 `/` 起头必须分居两棵子树，且相对那棵有确定的落脚层
        assert_eq!(
            safe_relative_path("./a.bin").unwrap(),
            PathBuf::from("%rel").join("a.bin")
        );
        assert_eq!(
            safe_relative_path("a.bin").unwrap(),
            safe_relative_path("./a.bin").unwrap()
        );
        assert_eq!(
            safe_relative_path("/a.bin").unwrap(),
            PathBuf::from("a.bin")
        );
    }

    /// 审计2 #14：`\` 在 POSIX 上是**合法文件名字符**，不是分隔符。
    /// 原实现按 `['/', '\\']` 分段，于是一个名叫 `a\b` 的远端文件会长出一层本地目录，
    /// 与远端真正的 `a/b` 撞在一起；更糟的是服务端凭一个文件名就能在沙箱里造目录结构，
    /// 甚至把「本该是文件的落点」变成一个目录。
    #[test]
    fn safe_relative_path_treats_backslash_as_an_ordinary_name_char() {
        let backslash = safe_relative_path("/dir/a\\b").unwrap();
        assert_eq!(backslash, PathBuf::from("dir").join("a%5Cb"));
        assert_eq!(backslash.components().count(), 2, "不得长出第三层目录");
        assert_ne!(backslash, safe_relative_path("/dir/a/b").unwrap());
    }

    /// 审计2 #14：转义必须**可逆**——本地名字还原得回远端名字，才说明映射没有丢信息。
    /// 单射性由此是构造出来的，而不是靠逐对断言碰运气覆盖到。
    #[test]
    fn safe_relative_path_segments_decode_back_to_the_remote_names() {
        let p = safe_relative_path("/dir name/a:b\\c/NUL/foo. ").unwrap();
        let decoded: Vec<String> = p
            .components()
            .map(|c| {
                sandbox::unescape_local_component(&c.as_os_str().to_string_lossy())
                    .expect("每一段都应可还原")
            })
            .collect();
        assert_eq!(decoded, ["dir name", "a:b\\c", "NUL", "foo. "]);
    }

    #[test]
    fn ensure_within_accepts_path_inside_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        let got = ensure_within(root.path(), "sub").unwrap();
        assert!(got.ends_with("sub"), "{}", got.display());
    }

    #[test]
    fn ensure_within_rejects_absolute_outside_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let err = ensure_within(root.path(), &outside.path().to_string_lossy()).unwrap_err();
        assert!(err.contains("escapes browse root"), "{err}");
    }

    #[test]
    fn ensure_within_rejects_dotdot_escape() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        let err = ensure_within(&root.path().join("sub"), "../..").unwrap_err();
        assert!(err.contains("escapes browse root"), "{err}");
    }

    /// 符号链接越界：canonicalize 解引用后落在根外 → fail-closed。
    /// Windows 建符号链接需管理员/开发者模式，故本例仅 unix（Step 6 Expected 已注平台计数差异）。
    #[cfg(unix)]
    #[test]
    fn ensure_within_rejects_symlink_pointing_outside() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        let err = ensure_within(root.path(), "link").unwrap_err();
        assert!(err.contains("escapes browse root"), "{err}");
    }

    // ── 审计2 #38：本地列表上限（cap_local 纯函数 + local_list 接线守卫）────────────

    fn entry(name: &str) -> LocalEntry {
        LocalEntry {
            name: name.into(),
            is_dir: false,
            size: 0,
        }
    }

    /// 恰好 cap 条不算截断；第 cap+1 条只用于判定，不得混进列表。
    #[test]
    fn cap_local_keeps_exactly_cap_and_marks_overflow() {
        let (out, truncated) = cap_local((0..3).map(|i| Ok(entry(&format!("f{i}")))), 3).unwrap();
        assert_eq!(out.len(), 3);
        assert!(!truncated, "恰好 cap 条不算截断");

        let (out, truncated) = cap_local((0..4).map(|i| Ok(entry(&format!("f{i}")))), 3).unwrap();
        assert_eq!(out.len(), 3, "多出的那一条只用于判定，不得进列表");
        assert!(truncated);
    }

    /// 空目录不截断；`items` 里已消费到的 `Err` 原样上抛——包括第 cap+1 条自身的错误
    ///（旧实现先入列再查超限，逐条传播语义必须原样保留）。
    #[test]
    fn cap_local_empty_and_error_propagation() {
        let (out, truncated) =
            cap_local(std::iter::empty::<Result<LocalEntry, String>>(), 3).unwrap();
        assert!(out.is_empty());
        assert!(!truncated);

        let items = vec![Ok(entry("a")), Err("boom".to_string()), Ok(entry("b"))];
        assert_eq!(cap_local(items, 10).unwrap_err(), "boom");

        let items = vec![Ok(entry("a")), Ok(entry("b")), Err("boom2".to_string())];
        assert_eq!(
            cap_local(items, 2).unwrap_err(),
            "boom2",
            "第 cap+1 条的元数据错误同样照常上抛"
        );
    }

    /// `sftp_remove` 必须**经由** `remove_tree` 走守卫递归。把它改回 `ops.remove`
    ///（或 `inner.remove_file`）编译照过、行为也「对」——正是审计2 #17 要根除的那条
    /// 旧实现——只有这条源文件守卫会红。
    #[test]
    fn sftp_remove_uses_remove_tree() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/sftp_cmd.rs"),
        )
        .expect("读不到本文件");
        let line_eq = |needle: &str| src.lines().filter(|l| l.trim() == needle).count();
        assert_eq!(
            line_eq("fs_sshengine::sftp::remove_tree(ops.as_ref(), &path)"),
            1,
            "sftp_remove 必须经由 remove_tree 走守卫递归"
        );
        // 针由拼接构造：本断言行自身就写有「ops.remove」，字面量计数会把自己数进去。
        let direct_remove = format!("ops.remove{}", '(');
        assert_eq!(
            src.matches(&direct_remove).count(),
            0,
            "sftp_remove 不得直调文件删除原语"
        );
    }

    /// `local_list` 必须**经由** `cap_local` 走闸、且与引擎共用同一把 `LIST_CAP`。
    /// 把上限内联回去（或换一把自造的上限）编译照过、行为全对——只有这条源文件守卫会红。
    #[test]
    fn local_list_uses_cap_local_with_shared_cap() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/sftp_cmd.rs"),
        )
        .expect("读不到本文件");
        // 生产调用是行尾开括号（下一行接迭代器）；本测试里的调用不是，故只数生产那处。
        assert_eq!(
            src.matches("cap_local(\n").count(),
            1,
            "local_list 必须经由 cap_local 走闸"
        );
        // 针与源码行**整行相等**才计数：断言行自身就写有针的文字，子串匹配会把自己数进去。
        let line_eq = |needle: &str| src.lines().filter(|l| l.trim() == needle).count();
        assert_eq!(
            line_eq("const LIST_CAP: usize = fs_sshengine::sftp::LIST_ENTRY_CAP;"),
            1,
            "上限常量定义必须直接取自引擎常量"
        );
        assert_eq!(
            line_eq("LIST_CAP,"),
            1,
            "上限常量必须恰好一处调用（local_list）"
        );
    }

    // ── 本地栏新建目录 / 重命名（M4b 第 15 项）────────────────────────────
    //
    // 这些命令读的是 `browse_root()`（真实家目录），所以测的是把 root 作参数的
    // 那一层——与 `ensure_within` 的既有测试同款做法。

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("fs-local-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// `local_mkdir` 的可测形态：root 由调用方给。
    fn mkdir_in(
        root: &std::path::Path,
        parent: &str,
        name: &str,
    ) -> Result<std::path::PathBuf, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("目录名不能为空".into());
        }
        if name.contains(['/', '\\']) {
            return Err("目录名里不能有斜杠".into());
        }
        if name == "." || name == ".." {
            return Err("目录名不能是 . 或 ..".into());
        }
        let base = ensure_within(root, parent)?;
        let target = base.join(name);
        std::fs::create_dir(&target).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => format!("「{name}」已经存在"),
            _ => e.to_string(),
        })?;
        Ok(target)
    }

    #[test]
    fn local_mkdir_creates_a_directory_under_the_browse_root() {
        let root = tmpdir("mkdir-ok");
        let made = mkdir_in(&root, "", "新目录").expect("应当建得出");
        assert!(made.is_dir());
        assert_eq!(made.file_name().unwrap(), "新目录");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn local_mkdir_refuses_a_name_that_is_really_a_path() {
        // 「新建目录」不是「在任意位置新建目录」。`ensure_within` 会拦住逃逸，
        // 但在这一层挡掉让错误信息说得清楚——「名字里不能有斜杠」比「越界」有用得多。
        let root = tmpdir("mkdir-path");
        for bad in ["a/b", "..", ".", "../逃", "a\\b", "  "] {
            let e = mkdir_in(&root, "", bad).unwrap_err();
            assert!(!e.is_empty(), "{bad} 应当被拒且有理由");
        }
        // 确认真的没建出任何东西
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn local_mkdir_says_so_when_it_already_exists() {
        // create_dir 而不是 create_dir_all：后者对「已存在且是目录」返回 Ok，
        // 于是用户点了「新建」却什么也没发生，而界面会说成功。
        let root = tmpdir("mkdir-dup");
        mkdir_in(&root, "", "dup").unwrap();
        let e = mkdir_in(&root, "", "dup").unwrap_err();
        assert!(e.contains("已经存在"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `local_rename` 的可测形态。
    fn rename_in(root: &std::path::Path, parent: &str, from: &str, to: &str) -> Result<(), String> {
        let to = to.trim();
        if to.is_empty() {
            return Err("新名字不能为空".into());
        }
        if to.contains(['/', '\\']) || to == "." || to == ".." {
            return Err("新名字里不能有斜杠，也不能是 . 或 ..".into());
        }
        if from.trim().is_empty() || from.contains(['/', '\\']) {
            return Err("原名字不合法".into());
        }
        let base = ensure_within(root, parent)?;
        let src = base.join(from.trim());
        let dst = base.join(to);
        if dst.exists() {
            return Err(format!("「{to}」已经存在"));
        }
        if !src.exists() {
            return Err(format!("「{}」不存在（可能已被别处改动）", from.trim()));
        }
        std::fs::rename(&src, &dst).map_err(|e| e.to_string())
    }

    #[test]
    fn local_rename_renames_in_place() {
        let root = tmpdir("rn-ok");
        std::fs::write(root.join("旧名.txt"), b"x").unwrap();
        rename_in(&root, "", "旧名.txt", "新名.txt").expect("应当改得动");
        assert!(root.join("新名.txt").is_file());
        assert!(!root.join("旧名.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **目标已存在就拒绝**，不问「要不要覆盖」。
    ///
    /// `std::fs::rename` 在多数平台上会**静默覆盖**目标——那意味着用户输错一个名字
    /// 就毁掉另一个文件，而界面上什么都不会说。
    #[test]
    fn local_rename_never_silently_overwrites() {
        let root = tmpdir("rn-clash");
        std::fs::write(root.join("a.txt"), b"aaa").unwrap();
        std::fs::write(root.join("b.txt"), b"bbb").unwrap();
        let e = rename_in(&root, "", "a.txt", "b.txt").unwrap_err();
        assert!(e.contains("已经存在"), "{e}");
        // 两个文件都还在，内容都没变——这才是「拒绝」该有的样子
        assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"aaa");
        assert_eq!(std::fs::read(root.join("b.txt")).unwrap(), b"bbb");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn local_rename_refuses_names_that_are_paths() {
        let root = tmpdir("rn-path");
        std::fs::write(root.join("a.txt"), b"x").unwrap();
        for (from, to) in [
            ("a.txt", "../逃"),
            ("a.txt", "b/c"),
            ("../x", "b"),
            ("a.txt", ".."),
        ] {
            assert!(rename_in(&root, "", from, to).is_err(), "{from} → {to}");
        }
        assert!(root.join("a.txt").is_file(), "原文件不该被动过");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn local_rename_says_so_when_the_source_is_gone() {
        // 用户在别处删掉了这个文件之后再点重命名。给一句能读懂的话，
        // 而不是一个 OS 错误码。
        let root = tmpdir("rn-gone");
        let e = rename_in(&root, "", "不存在.txt", "新的.txt").unwrap_err();
        assert!(e.contains("不存在"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **本地栏没有删除**，而且这个缺席是刻意的。
    ///
    /// 评估结论见路线图 §6.3（`docs/roadmap.md`）：
    /// 它是三个操作里唯一不可逆的；本地这一侧用户是在「用自己的电脑」，
    /// 而自己的电脑上有回收站、我们这里没有；且 `sftp.sandboxRoot` 管的是
    /// **下载落点**，对删除一句话都没说。
    ///
    /// 这条守卫防的是「顺手补齐对偶」——那看起来像是在修一个不一致，
    /// 实际是在加一个不进回收站的删除按钮。
    #[test]
    fn there_is_deliberately_no_local_delete_command() {
        const SRC: &str = include_str!("sftp_cmd.rs");
        let prod = &SRC[..SRC.find("\n#[cfg(test)]").expect("本文件必须有测试段")];
        for banned in [
            "local_delete",
            "local_remove",
            "local_rmdir",
            "local_unlink",
        ] {
            assert!(
                !prod.contains(banned),
                "出现了 {banned}：本地栏删除是刻意不做的（见路线图 §6.3）。\
                 真要做的话，先读那份评估的第 2 节——直接 unlink 的版本不做。"
            );
        }
        // 反向对照：这两个**应当**存在（否则上面几条会因为整段代码都没了而假绿）
        assert!(prod.contains("pub async fn local_mkdir("));
        assert!(prod.contains("pub async fn local_rename("));
    }
}
