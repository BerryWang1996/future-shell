//! SFTP 文件操作抽象与 russh-sftp 生产实现（spec §2.3）。
//! `SftpOps` 是对象安全 trait：传输管理器持 `Arc<dyn SftpOps>`，单测注入内存 fake、
//! itest 注入容器真实会话，两侧共用同一套语义。
use crate::Error;
use async_trait::async_trait;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub mtime: i64,
    /// 权限位（`rwxr-xr-x` 九字符）。`None` = 服务端未回权限属性（部分 SFTP 实现/Windows 远端），
    /// 前端按「—」渲染。这是「我有没有权限读写这个文件」在界面上的唯一依据。
    pub perms: Option<String>,
}

/// lstat 语义的全量元数据（M4a 文件面板属性对话框/软链图标）。
#[derive(Debug, Clone, Serialize)]
pub struct FileMeta {
    pub file_type: FileType,
    pub size: u64,
    pub mtime: i64,
    /// POSIX mode 低 12 位（含 setuid/setgid/sticky）；服务端未回该属性时为 None
    /// （属性面板权限列显示「—」）。
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileType {
    Regular,
    Dir,
    Symlink,
    Other,
}

/// 审计2 #38：目录列表的分页契约。`entries` 至多 [`LIST_ENTRY_CAP`] 条；
/// `truncated = true` 时列表**不完整**——调用方必须把这个事实呈现给用户，
/// 「目录里就这么多内容」绝不能由截断来伪装（比如按名字找文件找不到时，
/// 用户要知道该去更深的子目录，而不是以为文件不存在）。
#[derive(Debug, Clone, Serialize)]
pub struct ListResult {
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

/// 审计2 #38：单次目录列表的条目上限。20 000 条足以覆盖任何日常目录，同时把
/// 「后端全量收集 → IPC 全量序列化 → 前端全量排序渲染」的 CPU/内存/UI 卡死三条链一起封顶。
/// 这不是分页：本版本不做翻页，超出的部分以 `truncated` 如实上报。
pub const LIST_ENTRY_CAP: usize = 20_000;
/// 审计2 #38：单个条目名字的字符上限。异常服务端/异常目录可送回超长名——
/// 一个名字就足以打爆序列化与渲染。超长条目跳过并计 `truncated`（它是真实存在的条目）。
pub const LIST_NAME_CHARS_MAX: usize = 1024;

#[async_trait]
pub trait SftpOps: Send + Sync {
    async fn list(&self, path: &str) -> Result<ListResult, Error>;
    async fn stat_size(&self, path: &str) -> Result<u64, Error>;
    /// SSH_FXP_STAT：**跟随**软链的元数据（审计2 #9/#15）。
    ///
    /// 与 `lstat` 的分工不可含糊：`lstat` 看的是链**自身**，用于「这是不是个软链」这类问题；
    /// 本方法看的是**真正会被读到的那个文件**，用于「我要传的这份内容变了没有」。
    /// 拿 `lstat` 当变更探针会漏判——链指向的文件被整份换掉时，链自身的 mtime 与 size
    /// 纹丝不动。而漏判在这里会以「一切正常」的形式呈现，是最不能接受的一种错。
    ///
    /// 与 `stat_size` 的分工：那个只回 size，续传身份还需要 mtime，故不能复用。
    async fn stat_meta(&self, path: &str) -> Result<FileMeta, Error>;
    async fn read_range(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error>;
    async fn write_at(&self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error>;
    /// SSH_FXP_SETSTAT(size)：把远端文件截断/扩展到 `size` 字节。
    ///
    /// 存在的唯一理由是 `write_at` 刻意不带 TRUNCATE（否则毁断点续传）：非续传上传若覆写一个
    /// **更大**的旧文件，只会盖住前 n 字节、旧尾巴原样留下（发现 S43）。传输管理器在
    /// `resume_offset == 0` 的上传前调用本方法清零。
    async fn truncate(&self, path: &str, size: u64) -> Result<(), Error>;
    async fn mkdir(&self, path: &str) -> Result<(), Error>;
    /// SSH_FXP_REMOVE：删除**文件**（或软链自身）。目录删除走 [`remove_tree`]（守卫递归）。
    async fn remove(&self, path: &str) -> Result<(), Error>;
    /// SSH_FXP_RMDIR：删除**空**目录。非空目录必须先由 [`remove_tree`] 递归删空。
    async fn remove_dir(&self, path: &str) -> Result<(), Error>;
    async fn rename(&self, from: &str, to: &str) -> Result<(), Error>;
    /// SSH_FXP_LSTAT：不跟随软链的元数据（软链本身）
    async fn lstat(&self, path: &str) -> Result<FileMeta, Error>;
    /// SSH_FXP_READLINK：读取软链目标路径
    async fn read_link(&self, path: &str) -> Result<String, Error>;
    /// SSH_FXP_SYMLINK：创建软链（部分服务端如 Windows OpenSSH 可能不支持 → 原样透传 Error::Sftp）
    async fn symlink(&self, target: &str, link_path: &str) -> Result<(), Error>;
    /// SSH_FXP_REALPATH：把路径解成服务端认定的绝对路径（消解 `~`、相对基准、软链、`..`）。
    ///
    /// **只用来算同目标互斥的锁键，绝不能拿去当线上路径**（审计2 #13）。
    /// 理由是两条，缺一条这个约束就立不住：
    ///
    /// 1. 解析结果与真正写入用的路径若不是同一个字符串，中间就多了一次 TOCTOU
    ///    ——软链在解析之后、写入之前被改指向，我们会把字节写到解析时没见过的地方。
    ///    锁键不怕这个：键错了顶多是互斥退化（多挡或少挡一次），不会改写文件位置。
    /// 2. 服务端对**不存在**的路径回什么没有统一约定。OpenSSH 的 sftp-server 对
    ///    v3 REALPATH 一般会把不存在的尾段原样拼上返回，但别的实现（以及某些 chroot 配置）
    ///    直接回 SSH_FX_NO_SUCH_FILE。上传的目标本来就常常还不存在，所以调用方必须
    ///    自己保证问的是**已经存在**的那一段（例如父目录），再把叶子名拼回去。
    ///
    /// 失败一律降级而非中止：拿不到规范路径时，调用方退回纯词法的键。少一维互斥，
    /// 不是少一次传输。
    async fn canonicalize(&self, path: &str) -> Result<String, Error>;
}

/// 审计2 #38：把「任意多」的条目折叠成有上限的载荷。抽成纯函数是让上限逻辑可被
/// 单测直接打死——`RemoteSftp::list` 只有真服务器才跑得到，上限本身必须离线可测。
///
/// 取 `cap + 1` 是为了**区分**「恰好 cap 条」与「比 cap 多」两种事实：多取的那 1 条
/// 只用来判定 `truncated`，不随载荷返回。恰好 cap 条不算截断。
fn cap_entries<I>(items: I) -> ListResult
where
    I: IntoIterator<Item = (String, bool, bool, u64, i64, Option<String>)>,
{
    let mut entries: Vec<Entry> = Vec::with_capacity(LIST_ENTRY_CAP);
    let mut name_skips = 0usize;
    for (name, is_dir, is_symlink, size, mtime, perms) in items.into_iter().take(LIST_ENTRY_CAP + 1)
    {
        if name.chars().count() > LIST_NAME_CHARS_MAX {
            name_skips += 1;
            continue;
        }
        entries.push(Entry {
            name,
            is_dir,
            is_symlink,
            size,
            mtime,
            perms,
        });
    }
    let over = entries.len() > LIST_ENTRY_CAP;
    if over {
        entries.truncate(LIST_ENTRY_CAP);
    }
    ListResult {
        entries,
        truncated: over || name_skips > 0,
    }
}

/// 审计2 #17：递归删除的目录嵌套深度上限。
///
/// 正常目录树几十层封顶（POSIX 单段 255 字节、PATH_MAX 4096 字节，全路径算下来也到不了
/// 百层）。上限挡的是**恶意或故障服务端**：一台服务器可以对目录里任何条目谎报 `is_dir`，
/// 甚至把软链回标成目录，让客户端无限下钻。深度封顶把这种诱导的代价压成一次明确报错，
/// 而不是栈溢出或永远删不完。真实深树超限时报错让用户自己改名/挪走，好过客户端默默删光。
pub const REMOVE_TREE_MAX_DEPTH: usize = 64;

/// 属性对话框（M4a）所需的一条条目的完整描述：**链自身** + 它指向何处 + 目标是什么样。
///
/// 三样必须分开，不能合成「解析后的元数据」：
///   · `meta` 取自 `lstat` —— 回答「这个条目是什么」。软链的 size 是链文本长度、
///     mtime 是链被创建/改指向的时刻；用 `stat` 会把目标的属性冒充成链的属性，
///     界面上根本看不出这是个链，而用户据此做的删/传/覆盖决定全是错的。
///   · `link_target` 是 `read_link` 的**原文**：相对链（`../x`）与绝对链（`/x`）照原样
///     呈现。代替用户在心里解析，只会把断链显示成有效链。
///   · `target_meta` 可以为 `None`：断链、权限不足、跨 chroot 都会取不到。
///     None 不是错误，是「链在、目标不可达」这一真实状态。
///
/// 放在引擎层而不是 tauri 命令里：这套组合语义（lstat vs stat 的分工、读链失败仍要给出
/// 链自身属性）唯一能被真实 sftp-server 证伪的地方是 itest，而 itest 到不了 tauri 命令。
#[derive(Debug, Clone, Serialize)]
pub struct EntryDetail {
    pub path: String,
    pub meta: FileMeta,
    pub link_target: Option<String>,
    pub target_meta: Option<FileMeta>,
}

/// 见 [`EntryDetail`]。非软链条目的后两栏恒为 `None`（不去问服务端，省一次往返）。
pub async fn describe_entry(ops: &dyn SftpOps, path: &str) -> Result<EntryDetail, Error> {
    let meta = ops.lstat(path).await?;
    let mut link_target = None;
    let mut target_meta = None;
    if meta.file_type == FileType::Symlink {
        // 读链失败不致命：链在、只是读不出目标（权限/服务端限制）。属性面板照旧显示链
        // 自身的元数据、目标一栏留空——比整个面板弹错误框有用。
        if let Ok(t) = ops.read_link(path).await {
            // 跟随链取目标属性（stat 而非 lstat，否则拿到的还是链自己）。
            // 断链最常见 → None，界面显示「目标不可访问」。
            target_meta = ops.stat_meta(path).await.ok();
            link_target = Some(t);
        }
    }
    Ok(EntryDetail {
        path: path.to_string(),
        meta,
        link_target,
        target_meta,
    })
}

/// 递归枚举的条目数上限（M4a 文件夹拖拽传输）。
///
/// 与深度上限（[`REMOVE_TREE_MAX_DEPTH`]）互补：深度挡「无限下钻」，条数挡「一层里
/// 几十万个文件」。拖一个 `node_modules` 进来会产出十万级作业，队列 UI 与内存都撑不住；
/// 超限**如实报错**而不是截断——截断意味着「我传了一部分并告诉你成功了」。
pub const WALK_ENTRY_MAX: usize = 20_000;

/// 递归枚举远端目录下的**文件**（M4a 文件夹拖拽传输的入队清单）。
///
/// 返回相对 `root` 的路径列表（不含 `root` 自身），顺序与遍历顺序一致。
///
/// 三条守卫与 [`remove_tree`] 同源、同理由：
/// ① 列表截断 → 拒绝（看不到全貌就不入队，否则「传完了」这句话是假的）；
/// ② 深度上限 → 拒绝（服务端可以谎报 is_dir 诱导无限下钻）；
/// ③ 条数上限 → 拒绝（见 [`WALK_ENTRY_MAX`]）。
///
/// 软链**不跟随**：链一律作为叶子跳过而不是当文件传——跟随会把「传这个目录」变成
/// 「顺着链把目录外的东西也传走」，而且指向目录的链会造出环。跳过的链计入返回的
/// `skipped_links`，调用方须把它呈现出来（静默跳过等于谎报完整性）。
///
/// `filter` 与列表面板共用同一个匹配器：界面上被过滤掉的东西不会被传，反之亦然。
pub async fn walk_files(
    ops: &dyn SftpOps,
    root: &str,
    filter: &crate::filter::ExcludeFilter,
) -> Result<WalkResult, Error> {
    let mut out = Vec::new();
    let mut dirs = Vec::new();
    let mut skipped_links = Vec::new();
    // (远端绝对路径, 相对 root 的路径, 深度)
    let mut stack = vec![(root.to_string(), String::new(), 0usize)];
    while let Some((dir, rel, depth)) = stack.pop() {
        if depth >= REMOVE_TREE_MAX_DEPTH {
            return Err(Error::Sftp(format!(
                "目录嵌套超过 {REMOVE_TREE_MAX_DEPTH} 层，拒绝继续枚举：{dir}"
            )));
        }
        let listing = ops.list(&dir).await?;
        if listing.truncated {
            return Err(Error::Sftp(format!(
                "目录列表被截断（条目超过上限），看不到全貌，拒绝入队：{dir}"
            )));
        }
        for e in listing.entries {
            if filter.excludes(&e.name, e.is_dir) {
                continue;
            }
            let child_rel = if rel.is_empty() {
                e.name.clone()
            } else {
                format!("{rel}/{}", e.name)
            };
            let child = format!("{dir}/{}", e.name);
            if e.is_symlink {
                // 链既不当文件传也不递归进去（见上）。计入 skipped 供 UI 明示。
                skipped_links.push(child_rel);
                continue;
            }
            if e.is_dir {
                dirs.push(child_rel.clone());
                stack.push((child, child_rel, depth + 1));
            } else {
                if out.len() >= WALK_ENTRY_MAX {
                    return Err(Error::Sftp(format!(
                        "待传文件超过 {WALK_ENTRY_MAX} 个，拒绝入队：{root}（请分批传输或用排除过滤器缩小范围）"
                    )));
                }
                out.push(child_rel);
            }
        }
    }
    Ok(WalkResult {
        files: out,
        dirs,
        skipped_links,
    })
}

/// [`walk_files`] 的结果。`skipped_links` 必须被呈现——静默跳过等于谎报完整性。
///
/// `dirs` 是相对根的子目录清单（含空目录），供**上传**方向在远端先建目录：
/// SFTP 的写入不会自动建父目录，少了这一步「拖一个文件夹上去」会在第一个子目录里
/// 报 no such file。空目录也列出——用户拖上去的是「这个目录」，结构缺一块不算传完。
#[derive(Debug, Clone, Default, Serialize)]
pub struct WalkResult {
    pub files: Vec<String>,
    pub dirs: Vec<String>,
    pub skipped_links: Vec<String>,
}

/// 审计2 #17：递归删除（守卫版）。目录→深度优先删空后 rmdir；文件/软链→直接 REMOVE。
///
/// 为什么抽成 `&dyn SftpOps` 上的自由函数而不是 `RemoteSftp` 的方法：
/// 守卫（截断拒绝/深度上限/根拒绝）是**离线可测**的纯策略，与 `cap_entries` 同理由——
/// 真实服务器的目录树不好造，内存 fake 可以精确构造「截断的列表」「65 层深链」这类
/// 只在异常服务端出现的形状，把它们逐条打死。
///
/// 守卫逐条的理由：
///
/// 1. **列表截断即拒绝**（审计2 #38 的姊妹约束）：递归删除建立在「我看到了这个目录的
///    全貌」之上。一个被截断的列表会留下看不见的条目——删完父目录后它们会变成
///    服务端上的孤儿，而用户被告知的是「删除成功」。看不到全貌就不动手。
/// 2. **深度上限**：见 [`REMOVE_TREE_MAX_DEPTH`]。
/// 3. **根拒绝**：`.`/`""`/`"/"` 是「当前目录/根目录」的指代而不是一个可删除的对象；
///    前端正常路径不会发来（`.` 被列表过滤、面包屑止于 `.`），这条是 IPC 边界的纵深防御。
///
/// 软链不跟随：`lstat` 判类型，链一律只删链自身（SSH_FXP_REMOVE 语义），目标纹丝不动；
/// READDIR 返回的条目类型取自链自身属性（LSTAT 语义），所以目录里的软链也会被当作
/// 叶子删掉而不是递归进目标——「删目录」不会顺着链删到目录之外去。
pub async fn remove_tree(ops: &dyn SftpOps, path: &str) -> Result<(), Error> {
    // 后序迭代的两相栈：Visit 弹出时删文件、压子目录并把自己标 Close；Close 弹出时 rmdir。
    // 先 Close 后 Visit 的压栈顺序保证「子目录全部处理完才轮到父目录的 Close」。
    enum Op {
        Visit { dir: String, depth: usize },
        Close(String),
    }
    let target = path.trim();
    if target.is_empty() || target == "." || target == "/" {
        return Err(Error::Sftp(format!(
            "拒绝删除「{target}」：当前目录与根目录不能作为删除对象，请进入其上级目录后删除该目录自身"
        )));
    }
    let meta = ops.lstat(target).await?;
    if meta.file_type != FileType::Dir {
        return ops.remove(target).await; // 文件、软链（含指向目录的软链）、其他类型：只删对象本身
    }
    let mut stack = vec![Op::Visit {
        dir: target.to_string(),
        depth: 0,
    }];
    while let Some(op) = stack.pop() {
        match op {
            Op::Visit { dir, depth } => {
                if depth >= REMOVE_TREE_MAX_DEPTH {
                    return Err(Error::Sftp(format!(
                        "目录嵌套超过 {REMOVE_TREE_MAX_DEPTH} 层，拒绝继续递归删除：{dir}"
                    )));
                }
                let listing = ops.list(&dir).await?;
                if listing.truncated {
                    // 看不到全貌就不动手：本目录此时一个字都没删，下一层也没进——fail-closed。
                    return Err(Error::Sftp(format!(
                        "目录列表被截断（条目超过上限），看不到全貌，拒绝递归删除：{dir}"
                    )));
                }
                stack.push(Op::Close(dir.clone()));
                for e in listing.entries {
                    let child = format!("{dir}/{name}", name = e.name);
                    if e.is_dir {
                        stack.push(Op::Visit {
                            dir: child,
                            depth: depth + 1,
                        });
                    } else {
                        ops.remove(&child).await?;
                    }
                }
            }
            Op::Close(dir) => ops.remove_dir(&dir).await?,
        }
    }
    Ok(())
}

/// 生产实现：russh-sftp over session channel（经 russh_sftp::client::SftpSession，全异步 API）。
/// 调用方须先在 channel 上 `request_subsystem(true, "sftp").await` 再构造。
pub struct RemoteSftp {
    inner: russh_sftp::client::SftpSession,
}

impl RemoteSftp {
    pub async fn new(channel: russh::Channel<russh::client::Msg>) -> Result<Self, Error> {
        let stream = channel.into_stream();
        let inner = russh_sftp::client::SftpSession::new(stream)
            .await
            .map_err(|e| Error::Sftp(e.to_string()))?;
        Ok(Self { inner })
    }
}

#[async_trait]
impl SftpOps for RemoteSftp {
    async fn list(&self, path: &str) -> Result<ListResult, Error> {
        // read_dir 返回 ReadDir（Iterator<Item = DirEntry>），自动过滤 `.`/`..`
        let entries = self
            .inner
            .read_dir(path)
            .await
            .map_err(|e| Error::Sftp(e.to_string()))?;
        // 审计2 #38：上限闸在 `cap_entries`（纯函数，离线可测）。
        Ok(cap_entries(entries.map(|entry| {
            let meta = entry.metadata();
            (
                entry.file_name(),
                entry.file_type().is_dir(),
                entry.file_type().is_symlink(),
                meta.len(),
                mtime_secs(&meta),
                // 权限位：`FilePermissions::Display` 即 `rwxr-xr-x` 九字符；服务端未回即 None
                Some(meta.permissions().to_string()),
            )
        })))
    }

    async fn stat_size(&self, path: &str) -> Result<u64, Error> {
        let meta = self
            .inner
            .metadata(path)
            .await
            .map_err(|e| Error::Sftp(e.to_string()))?;
        Ok(meta.len())
    }

    async fn stat_meta(&self, path: &str) -> Result<FileMeta, Error> {
        // `metadata` 走 SSH_FXP_STAT（跟随软链），与 `lstat` 的 `symlink_metadata` 相对。
        let m = self
            .inner
            .metadata(path)
            .await
            .map_err(|e| Error::Sftp(e.to_string()))?;
        Ok(FileMeta {
            file_type: file_type_of(m.file_type()),
            size: m.len(),
            mtime: mtime_secs(&m),
            mode: m.permissions.map(|p| p & 0o7777),
            uid: m.uid,
            gid: m.gid,
        })
    }

    async fn read_range(&self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut f = self
            .inner
            .open(path)
            .await
            .map_err(|e| Error::Sftp(e.to_string()))?;
        f.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| Error::Sftp(e.to_string()))?;
        let mut buf = vec![0u8; len];
        let mut filled = 0;
        while filled < len {
            let n = f
                .read(&mut buf[filled..])
                .await
                .map_err(|e| Error::Sftp(e.to_string()))?;
            if n == 0 {
                break; // EOF
            }
            filled += n;
        }
        buf.truncate(filled);
        Ok(buf)
    }

    async fn write_at(&self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};
        // 续传语义：WRITE | CREATE，绝不能带 TRUNCATE
        //（`SftpSession::create` 自带 TRUNCATE，会清空已写分片，禁用）。
        let mut f = self
            .inner
            .open_with_flags(
                path,
                russh_sftp::protocol::OpenFlags::WRITE | russh_sftp::protocol::OpenFlags::CREATE,
            )
            .await
            .map_err(|e| Error::Sftp(e.to_string()))?;
        f.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| Error::Sftp(e.to_string()))?;
        f.write_all(data)
            .await
            .map_err(|e| Error::Sftp(e.to_string()))?;
        f.flush().await.map_err(|e| Error::Sftp(e.to_string()))
    }

    async fn truncate(&self, path: &str, size: u64) -> Result<(), Error> {
        // ⚠ `FileAttributes::default()` **不是全 None**（russh-sftp 2.3.0）：它是
        // `size: 0, uid: 0, gid: 0, permissions: 0o777|S_IFDIR, atime: 0, mtime: 0`。
        // 于是 `FileAttributes { size: Some(n), ..Default::default() }` 发出去的 SETSTAT
        // 位图带着 SIZE|UIDGID|PERMISSIONS|ACMODTIME 四项——等于顺手请求
        // 「chown 到 root、chmod 成 0777 目录位、把时间戳清成 1970」。
        //
        // 实测（linuxserver/openssh-server + internal-sftp，普通用户）：服务端先应用 size、
        // 再在 chown 处以 SSH_FX_PERMISSION_DENIED 失败 —— 于是**尺寸真的改了、调用却返回
        // Permission denied**。这种「做了却报错」是最坏的一种：上层据错误重试或回滚，
        // 而磁盘上的状态已经变了。生产路径 `write_remote_identity` 与非续传上传的清零
        // 都走这里，之前一直吃着这个假错误。
        //
        // 故逐字段显式列全 None，不用 `..Default::default()`。今后新增字段时**必须**
        // 在这里补一个 `None`（编译器会强制提醒，这正是不写 `..Default::default()` 的目的）。
        self.inner
            .set_metadata(
                path,
                russh_sftp::protocol::FileAttributes {
                    size: Some(size),
                    uid: None,
                    user: None,
                    gid: None,
                    group: None,
                    permissions: None,
                    atime: None,
                    mtime: None,
                },
            )
            .await
            .map_err(|e| Error::Sftp(e.to_string()))
    }

    async fn mkdir(&self, path: &str) -> Result<(), Error> {
        self.inner
            .create_dir(path)
            .await
            .map_err(|e| Error::Sftp(e.to_string()))
    }

    async fn remove(&self, path: &str) -> Result<(), Error> {
        self.inner
            .remove_file(path)
            .await
            .map_err(|e| Error::Sftp(e.to_string()))
    }

    async fn remove_dir(&self, path: &str) -> Result<(), Error> {
        self.inner
            .remove_dir(path)
            .await
            .map_err(|e| Error::Sftp(e.to_string()))
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        self.inner
            .rename(from, to)
            .await
            .map_err(|e| Error::Sftp(e.to_string()))
    }

    async fn lstat(&self, path: &str) -> Result<FileMeta, Error> {
        // symlink_metadata 走 SSH_FXP_LSTAT（不跟随软链）；metadata 走 SSH_FXP_STAT
        let m = self
            .inner
            .symlink_metadata(path)
            .await
            .map_err(|e| Error::Sftp(e.to_string()))?;
        Ok(FileMeta {
            file_type: file_type_of(m.file_type()),
            size: m.len(),
            mtime: mtime_secs(&m),
            // `Metadata` = `protocol::FileAttributes`，其 `permissions`/`uid`/`gid` 是 pub
            // `Option<u32>` 原始协议字段。不走 `permissions() -> FilePermissions`：那 9 个
            // bool 只覆盖 rwxrwxrwx，按位重组会静默丢掉 setuid/setgid/sticky（属性面板会把
            // 4755 显示成 755）。`& 0o7777` 剥掉高 4 位文件类型，保留完整低 12 位。
            mode: m.permissions.map(|p| p & 0o7777),
            uid: m.uid,
            gid: m.gid,
        })
    }

    async fn read_link(&self, path: &str) -> Result<String, Error> {
        // russh-sftp 2.3.0 `SftpSession::read_link` → `SftpResult<String>`，直接返回
        self.inner
            .read_link(path)
            .await
            .map_err(|e| Error::Sftp(e.to_string()))
    }

    async fn symlink(&self, target: &str, link_path: &str) -> Result<(), Error> {
        // 参数序看似与库签名 `symlink(path, target)` 相反，实为两次反转相消，勿「修正」：
        // russh-sftp 把第 1 个实参放进线上的 `linkpath` 字段、第 2 个放进 `targetpath`；
        // 而 OpenSSH sftp-server 对 SSH_FXP_SYMLINK 的两个字段是**互换**着读的
        //（draft-ietf-secsh-filexfer 与实现不一致，OpenSSH PROTOCOL §3.1 明文承认）。
        // 因此这里传 (target, link_path) 才能在 OpenSSH 上建出 link_path -> target。
        self.inner
            .symlink(target, link_path)
            .await
            .map_err(|e| Error::Sftp(e.to_string()))
    }

    async fn canonicalize(&self, path: &str) -> Result<String, Error> {
        // russh-sftp 2.3.0 `canonicalize` = SSH_FXP_REALPATH，取 name 应答的第一条 filename；
        // 应答里一条都没有时它自己回 `UnexpectedBehavior("no file")`，照常降级。
        self.inner
            .canonicalize(path)
            .await
            .map_err(|e| Error::Sftp(e.to_string()))
    }
}

/// 入参为 SFTP **协议**类型（`Metadata::file_type()` → `protocol::FileType`），
/// 非 `std::fs::FileType`；该类型自带 `is_dir`/`is_file`/`is_symlink`/`is_other`。
fn file_type_of(ft: russh_sftp::protocol::FileType) -> FileType {
    if ft.is_symlink() {
        FileType::Symlink
    } else if ft.is_dir() {
        FileType::Dir
    } else if ft.is_file() {
        FileType::Regular
    } else {
        FileType::Other
    }
}

/// mtime → Unix 秒；服务端未回该属性时按 0 处理（列表按时间排序时排在最早）。
fn mtime_secs(m: &russh_sftp::client::fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(i: usize) -> (String, bool, bool, u64, i64, Option<String>) {
        (
            format!("f{i}"),
            false,
            false,
            i as u64,
            i as i64,
            Some("rw-r--r--".into()),
        )
    }

    /// 审计2 #38 上限闸核心：恰好 cap 条**不是**截断，cap+1 条才是。
    /// 多取的那 1 条只用来判定，不随载荷返回。
    #[test]
    fn exactly_cap_is_not_truncated() {
        let r = cap_entries((0..LIST_ENTRY_CAP).map(item));
        assert_eq!(r.entries.len(), LIST_ENTRY_CAP);
        assert!(!r.truncated, "恰好 cap 条不算截断");
    }

    #[test]
    fn cap_plus_one_is_truncated_to_cap() {
        let r = cap_entries((0..=LIST_ENTRY_CAP).map(item));
        assert_eq!(r.entries.len(), LIST_ENTRY_CAP, "载荷不得超出上限");
        assert!(r.truncated, "比 cap 多一条必须如实上报");
        assert_eq!(
            r.entries.last().unwrap().name,
            format!("f{}", LIST_ENTRY_CAP - 1)
        );
    }

    /// 空目录：0 条、不截断。
    #[test]
    fn empty_listing_is_not_truncated() {
        let r = cap_entries(std::iter::empty());
        assert!(r.entries.is_empty());
        assert!(!r.truncated);
    }

    /// 超长名是**真实存在的条目**：跳过它必须计 truncated，否则那个条目会被伪装成不存在。
    #[test]
    fn overlong_name_is_skipped_and_marks_truncation() {
        let long = "名".repeat(LIST_NAME_CHARS_MAX + 1);
        let r = cap_entries([(long, false, false, 1, 1, None), item(0)]);
        assert_eq!(r.entries.len(), 1);
        assert!(r.truncated, "超长名被跳过必须如实上报");
    }

    /// 恰好等于名字上限的名字不被跳过（上限是「> 上限才拒」）。
    #[test]
    fn name_at_exact_limit_is_kept() {
        let at_limit = "名".repeat(LIST_NAME_CHARS_MAX);
        let r = cap_entries([(at_limit, false, false, 1, 1, None)]);
        assert_eq!(r.entries.len(), 1);
        assert!(!r.truncated);
    }

    // ── 审计2 #17：remove_tree 守卫（内存 fake，离线可测）─────────────────────────

    /// remove_tree 的测试 fake：`nodes` 是「路径 → (is_dir, is_symlink)」树；`truncated` 里
    /// 的目录其 list 返回截断标记；每次 remove/remove_dir 记入 `ops` 并**真的**删节点——
    /// 「守卫拒绝时一字未动」由此可验证，而不是只看返回值。remove_dir 刻意模拟
    /// SSH_FXP_RMDIR 的「仅空目录」语义：孩子没删光就 rmdir 会报错，后序次序因此可观察。
    struct MemFs {
        nodes: std::sync::Mutex<std::collections::HashMap<String, (bool, bool)>>,
        truncated: Vec<String>,
        ops: std::sync::Mutex<Vec<String>>,
    }

    impl MemFs {
        fn new() -> Self {
            // 树：/a/{f1, b/{f2}}、/c（文件）、/link（软链 → /a）、/e（空目录）
            let mut nodes = std::collections::HashMap::new();
            for (p, is_dir, is_symlink) in [
                ("/a", true, false),
                ("/a/f1", false, false),
                ("/a/b", true, false),
                ("/a/b/f2", false, false),
                ("/c", false, false),
                ("/link", false, true),
                ("/e", true, false),
            ] {
                nodes.insert(p.to_string(), (is_dir, is_symlink));
            }
            Self {
                nodes: std::sync::Mutex::new(nodes),
                truncated: vec![],
                ops: std::sync::Mutex::new(vec![]),
            }
        }

        fn children(&self, dir: &str) -> Vec<(String, bool, bool)> {
            let prefix = if dir == "/" {
                "/".to_string()
            } else {
                format!("{dir}/")
            };
            let mut out: Vec<(String, bool, bool)> = self
                .nodes
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| {
                    p != &dir && p.starts_with(&prefix) && !p[prefix.len()..].contains('/')
                })
                .map(|(p, &(d, s))| (p[prefix.len()..].to_string(), d, s))
                .collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            out
        }

        fn meta(&self, path: &str) -> Result<FileMeta, Error> {
            let &(is_dir, is_symlink) = self
                .nodes
                .lock()
                .unwrap()
                .get(path)
                .ok_or_else(|| Error::Sftp(format!("no such file: {path}")))?;
            Ok(FileMeta {
                file_type: if is_symlink {
                    FileType::Symlink
                } else if is_dir {
                    FileType::Dir
                } else {
                    FileType::Regular
                },
                size: 0,
                mtime: 0,
                mode: None,
                uid: None,
                gid: None,
            })
        }
    }

    #[async_trait]
    impl SftpOps for MemFs {
        async fn list(&self, path: &str) -> Result<ListResult, Error> {
            if !self.nodes.lock().unwrap().contains_key(path) {
                return Err(Error::Sftp(format!("no such file: {path}")));
            }
            let truncated = self.truncated.iter().any(|t| t == path);
            let entries = self
                .children(path)
                .into_iter()
                .map(|(name, is_dir, is_symlink)| Entry {
                    name,
                    is_dir,
                    is_symlink,
                    size: 0,
                    mtime: 0,
                    perms: Some("rw-r--r--".into()),
                })
                .collect();
            Ok(ListResult { entries, truncated })
        }
        async fn stat_size(&self, path: &str) -> Result<u64, Error> {
            self.meta(path).map(|m| m.size)
        }
        async fn stat_meta(&self, path: &str) -> Result<FileMeta, Error> {
            self.meta(path)
        }
        async fn read_range(&self, _: &str, _: u64, _: usize) -> Result<Vec<u8>, Error> {
            Err(Error::Sftp("mem: read_range 未实现".into()))
        }
        async fn write_at(&self, _: &str, _: u64, _: &[u8]) -> Result<(), Error> {
            Err(Error::Sftp("mem: write_at 未实现".into()))
        }
        async fn truncate(&self, _: &str, _: u64) -> Result<(), Error> {
            Err(Error::Sftp("mem: truncate 未实现".into()))
        }
        async fn mkdir(&self, _: &str) -> Result<(), Error> {
            Err(Error::Sftp("mem: mkdir 未实现".into()))
        }
        async fn remove(&self, path: &str) -> Result<(), Error> {
            if self.nodes.lock().unwrap().remove(path).is_none() {
                return Err(Error::Sftp(format!("no such file: {path}")));
            }
            self.ops.lock().unwrap().push(format!("remove({path})"));
            Ok(())
        }
        async fn remove_dir(&self, path: &str) -> Result<(), Error> {
            if !self.children(path).is_empty() {
                return Err(Error::Sftp(format!(
                    "rmdir 只删空目录（后序次序错乱的直接证据）: {path}"
                )));
            }
            if self.nodes.lock().unwrap().remove(path).is_none() {
                return Err(Error::Sftp(format!("no such file: {path}")));
            }
            self.ops.lock().unwrap().push(format!("rmdir({path})"));
            Ok(())
        }
        async fn rename(&self, _: &str, _: &str) -> Result<(), Error> {
            Err(Error::Sftp("mem: rename 未实现".into()))
        }
        async fn lstat(&self, path: &str) -> Result<FileMeta, Error> {
            self.meta(path)
        }
        async fn read_link(&self, path: &str) -> Result<String, Error> {
            // 记录调用：describe_entry 对**非**软链不该问这一句。多一次往返是小事，
            // 真问题是某些服务端对普通文件的 READLINK 直接报错——那会把一次本该成功的
            // 属性读取变成失败，而失败信息指向一个用户没做过的操作。
            self.ops.lock().unwrap().push(format!("read_link({path})"));
            if self.nodes.lock().unwrap().get(path).map(|&(_, s)| s) == Some(true) {
                return Ok("/a".into()); // MemFs 的 /link → /a
            }
            Err(Error::Sftp(format!("not a symlink: {path}")))
        }
        async fn symlink(&self, _: &str, _: &str) -> Result<(), Error> {
            Err(Error::Sftp("mem: symlink 未实现".into()))
        }
        async fn canonicalize(&self, _: &str) -> Result<String, Error> {
            Err(Error::Sftp("mem: canonicalize 未实现".into()))
        }
    }

    /// 文件与软链都只删对象本身；软链目标（/a）纹丝不动。
    #[test]
    fn remove_tree_file_and_symlink_remove_the_link_only() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let fs = MemFs::new();
                remove_tree(&fs, "/c").await.unwrap();
                remove_tree(&fs, "/link").await.unwrap();
                assert_eq!(
                    *fs.ops.lock().unwrap(),
                    vec!["remove(/c)".to_string(), "remove(/link)".to_string()]
                );
                assert!(
                    fs.nodes.lock().unwrap().contains_key("/a"),
                    "软链目标必须原样保留"
                );
                assert!(!fs.nodes.lock().unwrap().contains_key("/c"));
                assert!(!fs.nodes.lock().unwrap().contains_key("/link"));
            });
    }

    /// 目录：后序——文件先删、子目录删空 rmdir、最后父目录。rmdir 的「仅空目录」语义
    /// 使任何次序错乱当场报错，整个序列由 ops 日志逐字钉死。
    #[test]
    fn remove_tree_dir_is_post_order() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let fs = MemFs::new();
                remove_tree(&fs, "/a").await.unwrap();
                assert_eq!(
                    *fs.ops.lock().unwrap(),
                    vec![
                        "remove(/a/f1)".to_string(),
                        "remove(/a/b/f2)".to_string(),
                        "rmdir(/a/b)".to_string(),
                        "rmdir(/a)".to_string(),
                    ]
                );
                assert!(!fs.nodes.lock().unwrap().keys().any(|p| p.starts_with("/a")));
            });
    }

    /// 空目录有 rmdir 路径（审计2 #17 原文：「空目录也没有删除路径」）。
    #[test]
    fn remove_tree_empty_dir_uses_rmdir() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let fs = MemFs::new();
                remove_tree(&fs, "/e").await.unwrap();
                assert_eq!(*fs.ops.lock().unwrap(), vec!["rmdir(/e)".to_string()]);
                assert!(
                    fs.nodes.lock().unwrap().contains_key("/a/f1"),
                    "兄弟条目不得被波及"
                );
            });
    }

    /// 守卫 1：列表截断即拒绝，且**一个字节都没动**——看不到全貌就不动手（fail-closed）。
    #[test]
    fn remove_tree_refuses_truncated_listing_and_deletes_nothing() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let mut fs = MemFs::new();
                fs.truncated.push("/a".into());
                let err = remove_tree(&fs, "/a").await.unwrap_err();
                assert!(err.to_string().contains("截断"), "{err}");
                assert!(fs.ops.lock().unwrap().is_empty(), "拒绝时不得有任何删除");
                assert!(
                    fs.nodes.lock().unwrap().contains_key("/a/f1"),
                    "目录必须原样保留"
                );
            });
    }

    /// 守卫 2：目录嵌套深度上限。65 层纯目录链在第 64 层被拒，此前只压栈不删任何东西。
    #[test]
    fn remove_tree_refuses_dir_chain_over_max_depth() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let fs = MemFs::new();
                fs.nodes.lock().unwrap().clear();
                for i in 0..=REMOVE_TREE_MAX_DEPTH {
                    // 逐层嵌套的链：/d0、/d0/d1、…（children() 按 `dir/` 前缀判定直系子女，
                    // 平铺名 /d0、/d1 不是父子关系——那会把链首当空目录直接 rmdir 掉）
                    let path = (0..=i).map(|j| format!("/d{j}")).collect::<String>();
                    fs.nodes.lock().unwrap().insert(path, (true, false));
                }
                let err = remove_tree(&fs, "/d0").await.unwrap_err();
                assert!(err.to_string().contains("超过"), "{err}");
                assert!(
                    fs.ops.lock().unwrap().is_empty(),
                    "深度超限时不得有任何删除"
                );
                assert_eq!(
                    fs.nodes.lock().unwrap().len(),
                    REMOVE_TREE_MAX_DEPTH + 1,
                    "拒绝必须发生在删第一个节点之前"
                );
            });
    }

    /// 守卫 3：`.`/`""`/`"/"` 不是可删除对象——IPC 边界纵深防御，一律拒绝且不触碰任何节点。
    #[test]
    fn remove_tree_refuses_root_and_self_paths() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                for p in ["", "  ", ".", "/"] {
                    let fs = MemFs::new();
                    let err = remove_tree(&fs, p).await.unwrap_err();
                    assert!(err.to_string().contains("拒绝删除"), "{p:?} → {err}");
                    assert!(fs.ops.lock().unwrap().is_empty(), "{p:?} 不得触发删除");
                }
            });
    }

    /// 不存在的路径：lstat 错误原样上抛（对端报错/会话已断的传播面不在此处吞）。
    #[test]
    fn remove_tree_propagates_lstat_error() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let fs = MemFs::new();
                let err = remove_tree(&fs, "/nope").await.unwrap_err();
                assert!(err.to_string().contains("no such file"), "{err}");
                assert!(fs.ops.lock().unwrap().is_empty());
            });
    }

    /// `describe_entry` 对**非软链**不得多问 readlink/stat。
    ///
    /// 「多一次往返」听着无害，但某些服务端对普通文件的 READLINK 直接报错——那会把一次
    /// 本该成功的属性读取变成失败，且失败信息指向用户没做过的操作。判据用 MemFs 的
    /// 调用流水：非软链路径上不许出现 read_link。
    #[test]
    fn describe_entry_does_not_readlink_non_symlinks() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let fs = MemFs::new();
                // 普通文件
                let d = describe_entry(&fs, "/c").await.unwrap();
                assert_eq!(d.meta.file_type, FileType::Regular);
                assert!(d.link_target.is_none() && d.target_meta.is_none());
                // 目录
                let dir = describe_entry(&fs, "/a").await.unwrap();
                assert_eq!(dir.meta.file_type, FileType::Dir);
                assert!(
                    !fs.ops
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|o| o.starts_with("read_link")),
                    "非软链条目不得触发 readlink，实际流水：{:?}",
                    fs.ops.lock().unwrap()
                );

                // 软链：readlink 必须被问，且两栏都给出
                let l = describe_entry(&fs, "/link").await.unwrap();
                assert_eq!(l.meta.file_type, FileType::Symlink);
                assert_eq!(l.link_target.as_deref(), Some("/a"));
                assert!(l.target_meta.is_some());
                assert!(
                    fs.ops
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|o| o == "read_link(/link)"),
                    "软链必须问 readlink，实际流水：{:?}",
                    fs.ops.lock().unwrap()
                );
            });
    }

    /// 递归枚举：只出**文件**，路径相对根，软链跳过并计入 skipped。
    #[test]
    fn walk_files_lists_files_relative_to_root() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let fs = MemFs::new();
                let none = crate::filter::ExcludeFilter::default();
                let mut r = walk_files(&fs, "/a", &none).await.unwrap();
                r.files.sort();
                assert_eq!(r.files, vec!["b/f2".to_string(), "f1".to_string()]);
                assert!(r.skipped_links.is_empty(), "/a 下没有软链");
                // 空目录：文件列表为空而不是报错（空目录是合法的传输对象，只是没内容）
                let e = walk_files(&fs, "/e", &none).await.unwrap();
                assert!(e.files.is_empty() && e.skipped_links.is_empty());
            });
    }

    /// 软链不跟随、不当文件传，且**必须**出现在 skipped_links 里（静默跳过 = 谎报完整性）。
    #[test]
    fn walk_files_skips_symlinks_and_reports_them() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let fs = MemFs::new();
                // MemFs 没有 "/" 这个节点（它的树从 /a、/c 起），故把链挂到 /a 下面来考察
                fs.nodes
                    .lock()
                    .unwrap()
                    .insert("/a/lnk".to_string(), (false, true));
                let r = walk_files(&fs, "/a", &crate::filter::ExcludeFilter::default())
                    .await
                    .unwrap();
                assert!(
                    r.skipped_links.contains(&"lnk".to_string()),
                    "软链必须被记为跳过，实得 {:?}",
                    r.skipped_links
                );
                assert!(
                    !r.files.contains(&"lnk".to_string()),
                    "软链不得作为文件入队（跟随会把目录外的东西也传走，指向目录的链还会造环）"
                );
                // 常规文件仍在
                assert!(r.files.contains(&"f1".to_string()));
            });
    }

    /// 过滤器与列表面板共用：被排除的目录整棵不进，被排除的文件不入队。
    #[test]
    fn walk_files_honours_the_exclude_filter() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let fs = MemFs::new();
                // 排除目录 b/ → 它下面的 f2 一并不出现（整棵不进，而不是逐个匹配）
                let f = crate::filter::ExcludeFilter::parse("b/");
                let r = walk_files(&fs, "/a", &f).await.unwrap();
                assert_eq!(r.files, vec!["f1".to_string()]);
                // 排除文件
                let f2 = crate::filter::ExcludeFilter::parse("f1");
                let mut r2 = walk_files(&fs, "/a", &f2).await.unwrap();
                r2.files.sort();
                assert_eq!(r2.files, vec!["b/f2".to_string()]);
            });
    }

    /// 列表被截断 → 拒绝入队（看不到全貌就不说「传完了」）。
    #[test]
    fn walk_files_refuses_truncated_listing() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let mut fs = MemFs::new();
                fs.truncated = vec!["/a/b".to_string()];
                let err = walk_files(&fs, "/a", &crate::filter::ExcludeFilter::default())
                    .await
                    .unwrap_err();
                assert!(err.to_string().contains("截断"), "{err}");
            });
    }

    /// 条数上限：超限报错而不是截断（截断 = 传了一部分却报成功）。
    #[test]
    fn walk_files_caps_entry_count() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let fs = MemFs::new();
                {
                    let mut nodes = fs.nodes.lock().unwrap();
                    nodes.insert("/big".to_string(), (true, false));
                    for i in 0..(WALK_ENTRY_MAX + 5) {
                        nodes.insert(format!("/big/f{i}"), (false, false));
                    }
                }
                let err = walk_files(&fs, "/big", &crate::filter::ExcludeFilter::default())
                    .await
                    .unwrap_err();
                assert!(
                    err.to_string().contains(&WALK_ENTRY_MAX.to_string()),
                    "错误须点出上限数字（用户要知道该分批），实得 {err}"
                );
            });
    }

    /// 深度上限：服务端可以谎报 is_dir 诱导无限下钻，这里必须报错而不是永远走下去。
    #[test]
    fn walk_files_caps_depth() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let fs = MemFs::new();
                {
                    let mut nodes = fs.nodes.lock().unwrap();
                    let mut p = String::from("/deep");
                    nodes.insert(p.clone(), (true, false));
                    for _ in 0..(REMOVE_TREE_MAX_DEPTH + 2) {
                        p.push_str("/d");
                        nodes.insert(p.clone(), (true, false));
                    }
                }
                let err = walk_files(&fs, "/deep", &crate::filter::ExcludeFilter::default())
                    .await
                    .unwrap_err();
                assert!(err.to_string().contains("嵌套超过"), "{err}");
            });
    }
}
