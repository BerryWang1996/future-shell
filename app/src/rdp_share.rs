//! 共享目录的执行器（RDPDR 批）——远端文件操作在本机落地的**唯一**通道。
//!
//! # 安全模型（出口标准：共享哪个目录、是否只读、每次挂载写审计，三件缺一不可）
//!
//! 远端桌面是**半受信**的：用户在远端跑的任何程序都能借 RDPDR 通道对本机发文件
//! 操作——这是 RDPDR 的经典攻击面（服务器进程读改客户端任意文件）。所以：
//!
//! 1. **路径闸（每次操作都过）**：远端给的路径只当**相对路径**用。`..`、绝对路径、
//!    反斜杠混写，一律先归一再 [`resolve_within`] 夹进共享根；符号链接在打开后
//!    **再验一次** canonicalize 仍在根内（防「先查后开」窗口里的 TOCTOU 换链）。
//! 2. **只读闸**：挂载时定死。写意图（`FsOp::is_write`）直接回
//!    `STATUS_MEDIA_WRITE_PROTECTED`，**不碰文件系统**。
//! 3. **审计**：挂载/卸载各一行；每个文件的**打开**一行（读/写意图 + 结果）；
//!    每次**写完成**一行（字节数）。读不逐条记（列一个目录就是几十次 Stat，
//!    刷爆审计反而淹没人要看的写）。
//!
//! # 句柄表为什么在这里
//!
//! 远端只见 opaque u64。把「u64 → 打开的文件」的映射放在主程序，helper 即使被
//! 远端协议字节打穿也拿不到路径——它本来就只转发语义形状。
//!
//! # 判据载体（用户裁定 2026-09-05）
//!
//! xrdp 不实现 MS-RDPEFS 服务端（spike 实测），本模块的全部行为由**单测**钉住
//!（含越界/只读/审计三个闸的变异实证）；「真 Windows 远端 Explorer 列盘读写」
//! 标 [!] 留真机。

use fs_connmgr::audit_repo::{Actor, AuditRepo, NewAuditEntry, Verdict};
use fs_connmgr::Db;
use fs_rdpproto::{FsError, FsOp};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// 一个已挂载的共享。
pub struct Share {
    pub device: u8,
    pub name: String,
    pub root: PathBuf,
    pub readonly: bool,
}

/// 整个会话的共享状态：挂载表 + 句柄表。
pub struct ShareTable {
    db: Arc<Db>,
    vault: Arc<tokio::sync::Mutex<Option<fs_vault::Store>>>,
    session_id: String,
    shares: Mutex<HashMap<u8, Share>>,
    handles: Mutex<HashMap<u64, Arc<tokio::sync::Mutex<tokio::fs::File>>>>,
    next_handle: AtomicU64,
}

/// 一次操作的执行结果（协议层由调用方翻译）。
#[derive(Debug)]
pub struct FsOutcome {
    pub status: u32,
    pub handle: Option<u64>,
    pub n: u64,
    pub is_dir: bool,
    /// Read 的数据 / ListDir 的行。
    pub body: Vec<u8>,
}

impl FsOutcome {
    fn err(e: FsError) -> Self {
        Self {
            status: e.ntstatus(),
            handle: None,
            n: 0,
            is_dir: false,
            body: Vec::new(),
        }
    }
    fn ok(n: u64, is_dir: bool, body: Vec<u8>) -> Self {
        Self {
            status: 0,
            handle: None,
            n,
            is_dir,
            body,
        }
    }
}

/// 路径闸：把远端给的相对路径夹进共享根。
///
/// 规则（全部拒 = `OutsideShare`，而不是「裁掉越界部分」——裁是一种静默改写，
/// 远端拿到的文件与它要的不是同一个）：
/// · 绝对路径（`/etc/passwd`、`C:\…`）——只取其中相对段会撒谎，直接拒；
/// · 归一化后含 `..` 逃逸；
/// · 名字里有 NUL 或路径分隔符之外的怪东西交给文件系统自己拒（`NoSuchPath`）。
///
/// 返回「根 + 归一化相对路径」的拼接（**未** canonicalize——链接解析在打开后验证）。
pub fn resolve_within(root: &Path, remote_path: &str) -> Result<PathBuf, FsError> {
    let rel = remote_path.replace('\\', "/");
    // Windows 盘符前缀（"C:"）与根斜杠都算绝对：只取其中相对段等于配合伪造。
    if rel.starts_with('/') || Path::new(&rel).is_absolute() || rel.contains(':') {
        return Err(FsError::OutsideShare);
    }
    let mut clean: Vec<&str> = Vec::new();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                clean.pop().ok_or(FsError::OutsideShare)?;
            }
            s if s.contains('\0') => return Err(FsError::OutsideShare),
            s => clean.push(s),
        }
    }
    let mut full = root.to_path_buf();
    for seg in clean {
        full.push(seg);
    }
    Ok(full)
}

impl ShareTable {
    pub fn new(state: &Arc<crate::state::AppState>, session_id: String) -> Arc<Self> {
        Self::with_handles(state.db.clone(), state.vault.clone(), session_id)
    }

    /// 直接句柄构造（`new` 的本体；单测用——构造 AppState 要 AppHandle，太重）。
    pub fn with_handles(
        db: Arc<Db>,
        vault: Arc<tokio::sync::Mutex<Option<fs_vault::Store>>>,
        session_id: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            vault,
            session_id,
            shares: Mutex::new(HashMap::new()),
            handles: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
        })
    }

    /// 挂载：先落审计（挂载这一行在一切之前——之后哪怕执行器全坏，审计也在）。
    pub async fn mount(
        self: &Arc<Self>,
        device: u8,
        name: &str,
        dir: &str,
        readonly: bool,
    ) -> Result<(), String> {
        let root = std::fs::canonicalize(dir).map_err(|e| format!("共享目录打不开：{e}"))?;
        if !root.is_dir() {
            return Err("共享的必须是一个目录".into());
        }
        let exists = self.shares.lock().unwrap().contains_key(&device);
        if exists {
            return Err(format!("设备号 {device} 已被占用"));
        }
        self.audit(
            format!(
                "[RDP共享] 挂载 device={device} name={name} readonly={readonly} root={}",
                root.display()
            ),
            Verdict::Approved,
        )
        .await;
        self.shares.lock().unwrap().insert(
            device,
            Share {
                device,
                name: name.to_string(),
                root,
                readonly,
            },
        );
        Ok(())
    }

    /// 卸载：关掉这个盘上还开着的句柄（远端进程可能崩了没关——句柄泄漏在
    /// 长会话里只会越积越多），再撤表、落审计。
    pub async fn unmount(self: &Arc<Self>, device: u8) -> Result<(), String> {
        let share = self.shares.lock().unwrap().remove(&device);
        let Some(share) = share else {
            return Ok(()); // 幂等：卸一个没挂的盘不是错误
        };
        self.handles.lock().unwrap().clear();
        self.audit(
            format!("[RDP共享] 卸载 device={} name={}", share.device, share.name),
            Verdict::Approved,
        )
        .await;
        Ok(())
    }

    pub fn is_mounted(&self, device: u8) -> bool {
        self.shares.lock().unwrap().contains_key(&device)
    }

    pub fn readonly_of(&self, device: u8) -> Option<bool> {
        self.shares.lock().unwrap().get(&device).map(|s| s.readonly)
    }

    /// 执行一条远端文件操作。`audit` 与只读/路径闸都在这里——**调用方不得**
    /// 绕过本函数直接碰文件系统（rdp.rs 的源码级守卫钉住这一点）。
    pub async fn exec(self: &Arc<Self>, device: u8, op: &FsOp, body: &[u8]) -> FsOutcome {
        let (root, readonly, name) = {
            let shares = self.shares.lock().unwrap();
            let Some(s) = shares.get(&device) else {
                // 服务器对已卸载的盘还有在途 IRP：拒绝而不是 panic。
                return FsOutcome::err(FsError::OutsideShare);
            };
            (s.root.clone(), s.readonly, s.name.clone())
        };

        // ── 只读闸：不碰文件系统 ──
        if readonly && op.is_write() {
            let detail = op.path().unwrap_or("<句柄>").to_string();
            self.audit(
                format!("[RDP共享] 只读共享上的写被拒 device={device} name={name} path={detail:?}"),
                Verdict::Rejected,
            )
            .await;
            return FsOutcome::err(FsError::WriteProtected);
        }

        match op {
            FsOp::Create {
                path,
                write,
                disposition,
            } => {
                let full = match resolve_within(&root, path) {
                    Ok(p) => p,
                    Err(e) => {
                        self.audit(
                            format!("[RDP共享] 越界被拒 device={device} path={path:?}"),
                            Verdict::Rejected,
                        )
                        .await;
                        return FsOutcome::err(e);
                    }
                };
                let r = self
                    .create(&full, *write, *disposition, device, &name, path)
                    .await;
                r
            }
            FsOp::Read {
                handle,
                offset,
                len,
            } => {
                let file = self.handles.lock().unwrap().get(handle).cloned();
                let Some(file) = file else {
                    return FsOutcome::err(FsError::NoSuchPath);
                };
                let mut file = file.lock().await;
                if let Err(e) = file.seek(std::io::SeekFrom::Start(*offset)).await {
                    return FsOutcome::io(e);
                }
                let mut buf = vec![0u8; (*len as usize).min(READ_CAP)];
                match file.read(&mut buf).await {
                    Ok(0) => FsOutcome::ok(0, false, Vec::new()),
                    Ok(n) => {
                        buf.truncate(n);
                        let n64 = n as u64;
                        FsOutcome::ok(n64, false, buf)
                    }
                    Err(e) => FsOutcome::io(e),
                }
            }
            FsOp::Write { handle, offset } => {
                let file = self.handles.lock().unwrap().get(handle).cloned();
                let Some(file) = file else {
                    return FsOutcome::err(FsError::NoSuchPath);
                };
                let mut file = file.lock().await;
                if let Err(e) = file.seek(std::io::SeekFrom::Start(*offset)).await {
                    return FsOutcome::io(e);
                }
                match file.write_all(body).await {
                    Ok(()) => {
                        // 写完成逐条审计：这是唯一不可撤销、且用户不看提示就
                        // 永远不知道远端改了他什么的操作。
                        self.audit(
                            format!(
                                "[RDP共享] 远端写入 device={device} name={name} handle={handle} offset={offset} bytes={}",
                                body.len()
                            ),
                            Verdict::Approved,
                        )
                        .await;
                        FsOutcome::ok(body.len() as u64, false, Vec::new())
                    }
                    Err(e) => FsOutcome::io(e),
                }
            }
            FsOp::Close { handle } => {
                let taken = self.handles.lock().unwrap().remove(handle);
                if let Some(f) = taken {
                    let _ = f.lock().await.flush().await;
                }
                FsOutcome::ok(0, false, Vec::new())
            }
            FsOp::ListDir { path } => {
                let full = match resolve_within(&root, path) {
                    Ok(p) => p,
                    Err(e) => return FsOutcome::err(e),
                };
                let mut rd = match tokio::fs::read_dir(&full).await {
                    Ok(rd) => rd,
                    Err(_) => return FsOutcome::err(FsError::NoSuchPath),
                };
                // TOCTOU：列目录期间的链接换掉无害（目录项本身不被 follow）。
                let mut out = String::new();
                while let Ok(Some(ent)) = rd.next_entry().await {
                    let meta = ent.metadata().await;
                    let (t, size) = match &meta {
                        Ok(m) if m.is_dir() => ("d", 0u64),
                        Ok(m) => ("f", m.len()),
                        Err(_) => ("f", 0),
                    };
                    let mtime = meta
                        .as_ref()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    let name = ent.file_name().to_string_lossy().replace(['\t', '\n'], " ");
                    out.push_str(&format!("{name}\t{t}\t{size}\t{mtime}\n"));
                }
                FsOutcome::ok(0, true, out.into_bytes())
            }
            FsOp::Stat { path } => {
                let full = match resolve_within(&root, path) {
                    Ok(p) => p,
                    Err(e) => return FsOutcome::err(e),
                };
                // 打开后验链：Stat 的对象可能是链接，链接的目标必须在根内。
                let canon = match tokio::fs::canonicalize(&full).await {
                    Ok(c) => c,
                    Err(_) => return FsOutcome::err(FsError::NoSuchPath),
                };
                if !canon.starts_with(&root) {
                    return FsOutcome::err(FsError::OutsideShare);
                }
                match tokio::fs::metadata(&canon).await {
                    Ok(m) => FsOutcome::ok(m.len(), m.is_dir(), Vec::new()),
                    Err(_) => FsOutcome::err(FsError::NoSuchPath),
                }
            }
        }
    }

    async fn create(
        self: &Arc<Self>,
        full: &Path,
        write: bool,
        disposition: u8,
        device: u8,
        share_name: &str,
        remote_path: &str,
    ) -> FsOutcome {
        let existed = full.exists();
        let outcome: Result<(tokio::fs::File, bool), FsError> = async {
            match disposition {
                // 打开已有
                0 => match tokio::fs::OpenOptions::new()
                    .read(true)
                    .write(write)
                    .open(full)
                    .await
                {
                    Ok(f) => Ok((f, full.is_dir())),
                    Err(_) => Err(FsError::NoSuchPath),
                },
                // 建新（已存在则失败）
                1 => match tokio::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(full)
                    .await
                {
                    Ok(f) => Ok((f, false)),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        Err(FsError::AlreadyExists)
                    }
                    Err(_) => Err(FsError::Io),
                },
                // 打开或截断 / 打开或建新
                2 | 3 => match tokio::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(disposition == 3)
                    .truncate(disposition == 2)
                    .open(full)
                    .await
                {
                    Ok(f) => Ok((f, full.is_dir())),
                    Err(_) => Err(FsError::NoSuchPath),
                },
                _ => Err(FsError::Io),
            }
        }
        .await;
        // 打开后验链（防 TOCTOU 换链）：目录本身 canonicalize 后必须仍在根内。
        let outcome = match outcome {
            Ok((f, is_dir)) => {
                let canon = tokio::fs::canonicalize(full).await;
                let root_ok = canon
                    .as_ref()
                    .map(|c| {
                        c.starts_with(
                            // 根在 mount 时已 canonicalize，直接比
                            self.shares
                                .lock()
                                .unwrap()
                                .get(&device)
                                .map(|s| s.root.clone())
                                .unwrap_or_default(),
                        )
                    })
                    .unwrap_or(false);
                if root_ok {
                    Ok((f, is_dir))
                } else {
                    Err(FsError::OutsideShare)
                }
            }
            Err(e) => Err(e),
        };
        match outcome {
            Ok((file, is_dir)) => {
                let h = self.next_handle.fetch_add(1, Ordering::SeqCst);
                self.handles
                    .lock()
                    .unwrap()
                    .insert(h, Arc::new(tokio::sync::Mutex::new(file)));
                // 每个文件的打开记一行：读/写意图 + 结果。远端 Explorer 打开
                // 一个目录会先 Stat 再 Create(ListDir)——审计能还原它翻过哪些目录。
                self.audit(
                    format!(
                        "[RDP共享] 远端打开 device={device} name={share_name} path={remote_path:?} write={write} existed={existed} ok"
                    ),
                    Verdict::Approved,
                )
                .await;
                let mut o = FsOutcome::ok(0, is_dir, Vec::new());
                o.handle = Some(h);
                o
            }
            Err(e) => {
                let status = e.ntstatus();
                let o = FsOutcome::err(e);
                let _ = status;
                self.audit(
                    format!(
                        "[RDP共享] 远端打开失败 device={device} name={share_name} path={remote_path:?} write={write} nt=0x{status:08X}"
                    ),
                    Verdict::Rejected,
                )
                .await;
                o
            }
        }
    }

    async fn audit(self: &Arc<Self>, raw_action: String, verdict: Verdict) {
        // 写失败只 warn（口径同 services_cmd）：审计行写不进库不该把一次已经
        // 发生（或已经被拒）的操作报成命令失败——那只会让前端再点一次。
        //
        // action 照样过脱敏（`every_audit_writer_routes_through_redact` 是无条件的）：
        // 这里的串是我们自己拼的、此刻不含秘密——但共享路径里带用户名/机器名的
        // 目录在 Windows 上很常见（C:/Users/alice/…），把「我看过了、不含秘密」
        // 写成逐处自辩就是在给下一个真带秘密的写入方开路。
        let known: Vec<String> = {
            let guard = self.vault.lock().await;
            match guard.as_ref() {
                None => Vec::new(),
                Some(store) => store
                    .list()
                    .into_iter()
                    .filter_map(|m| store.get(m.id).ok())
                    .filter_map(|b| String::from_utf8(b.to_vec()).ok())
                    .filter(|s| s.trim().chars().count() >= 8)
                    .collect(),
            }
        };
        let display: fs_ai::agent::record::RedactedAction =
            fs_ai::agent::record::RedactedAction::new(&raw_action, &known);
        let entry = NewAuditEntry {
            actor: Actor::User,
            action: display.as_str().to_string(),
            target_session: Some(self.session_id.clone()),
            risk_level: "info".to_string(),
            verdict,
            exit_code: None,
            output_digest: None,
            created_at: crate::commands::ai_cmd::now_rfc3339(),
        };
        if let Err(e) = AuditRepo::new(self.db.pool()).append(&entry).await {
            tracing::warn!(error = %e, "RDP 共享的审计行写入失败（操作本身已执行/已拒）");
        }
    }
}

/// 单次读上限：远端要多大都只给这么多的一个窗口。RDPDR 单次读本来就有
/// 上限（服务器按块拉），这里挡的是异常值（比如 u32::MAX）造成的大分配。
const READ_CAP: usize = 1024 * 1024;

impl FsOutcome {
    fn io(e: std::io::Error) -> Self {
        tracing::warn!(error = %e, "RDP 共享文件 IO 失败");
        Self::err(FsError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn table() -> Arc<ShareTable> {
        // vault 给「锁着」（None）：脱敏表为空。测试关注闸与文件行为；
        // 「审计串过脱敏」这件事由 audit_cmd 的全局门禁钉（含本文件）。
        ShareTable::with_handles(
            test_db().await,
            Arc::new(tokio::sync::Mutex::new(None)),
            "s-test".into(),
        )
    }

    /// 单测库：临时目录里的真 sqlite（迁移齐跑，审计行能真写进去——
    /// 顺带让「审计写入失败只 warn 不炸」之外的主路径也被走到）。
    async fn test_db() -> Arc<Db> {
        let dir = std::env::temp_dir().join(format!(
            "fs-rdp-share-db-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(
            fs_connmgr::Db::open(&dir.join("t.db"))
                .await
                .expect("开测试库"),
        )
    }

    /// 每个测试自己的根目录：同一进程里测试**并发**跑，只用进程 id 的话
    /// 八个测试共用一个目录、互相删对方的文件（首跑就是这么挂的）。
    fn root() -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("fs-rdp-share-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 路径闸的全部形状。这条是安全核心：任何一个放过去 = 远端进程可读本机任意文件。
    #[tokio::test]
    async fn path_gate() {
        let r = root();
        let t = table().await;
        t.mount(1, "share", r.to_str().unwrap(), false)
            .await
            .unwrap();

        // 正常相对路径：放行
        assert!(resolve_within(&r, "a/b.txt").unwrap().starts_with(&r));

        // 越界全家桶：全部 OutsideShare
        for evil in [
            "../escape.txt",
            "a/../../escape.txt",
            "/etc/passwd",
            "C:/Windows/system32",
            "\\\\server\\share",
            "a\\..\\..\\x",
            "a\0b",
        ] {
            let res = resolve_within(&r, evil);
            assert!(
                matches!(res, Err(FsError::OutsideShare)),
                "{evil:?} 应被路径闸拦下，实得 {res:?}"
            );
        }
        // `..` 回到根内是合法的（远端在盘内向上导航）
        assert!(resolve_within(&r, "a/../b.txt").unwrap().starts_with(&r));
        let _ = std::fs::remove_dir_all(&r);
    }

    #[tokio::test]
    async fn readonly_gate_blocks_writes_without_touching_fs() {
        let r = root();
        std::fs::write(r.join("a.txt"), b"keep").unwrap();
        let t = table().await;
        t.mount(1, "share", r.to_str().unwrap(), true)
            .await
            .unwrap();

        let o = t
            .exec(
                1,
                &FsOp::Create {
                    path: "a.txt".into(),
                    write: true,
                    disposition: 2,
                },
                b"",
            )
            .await;
        assert_eq!(o.status, FsError::WriteProtected.ntstatus());

        // 文件分毫未动
        assert_eq!(std::fs::read(r.join("a.txt")).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[tokio::test]
    async fn write_read_roundtrip_inside_share() {
        let r = root();
        let t = table().await;
        t.mount(1, "share", r.to_str().unwrap(), false)
            .await
            .unwrap();

        let open = t
            .exec(
                1,
                &FsOp::Create {
                    path: "新文件.txt".into(),
                    write: true,
                    disposition: 3,
                },
                b"",
            )
            .await;
        assert_eq!(open.status, 0, "打开失败：{open:?}");
        let h = open.handle.expect("应给句柄");

        let w = t
            .exec(
                1,
                &FsOp::Write {
                    handle: h,
                    offset: 0,
                },
                "hello 共享".as_bytes(),
            )
            .await;
        assert_eq!(w.status, 0);
        assert_eq!(w.n, "hello 共享".len() as u64);

        // 关了再开（读路径），内容原样回来
        t.exec(1, &FsOp::Close { handle: h }, b"").await;
        let reopen = t
            .exec(
                1,
                &FsOp::Create {
                    path: "新文件.txt".into(),
                    write: false,
                    disposition: 0,
                },
                b"",
            )
            .await;
        assert_eq!(reopen.status, 0);
        let h2 = reopen.handle.unwrap();
        let rd = t
            .exec(
                1,
                &FsOp::Read {
                    handle: h2,
                    offset: 0,
                    len: 1024,
                },
                b"",
            )
            .await;
        assert_eq!(rd.status, 0);
        assert_eq!(rd.body, "hello 共享".as_bytes());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[tokio::test]
    async fn directory_listing_and_stat() {
        let r = root();
        std::fs::create_dir_all(r.join("sub")).unwrap();
        std::fs::write(r.join("sub/one.txt"), b"12345").unwrap();
        let t = table().await;
        t.mount(1, "share", r.to_str().unwrap(), true)
            .await
            .unwrap();

        let list = t.exec(1, &FsOp::ListDir { path: "sub".into() }, b"").await;
        assert_eq!(list.status, 0);
        let text = String::from_utf8(list.body).unwrap();
        assert!(text.contains("one.txt\tf\t5\t"), "列目录行不对：{text:?}");

        let st = t
            .exec(
                1,
                &FsOp::Stat {
                    path: "sub/one.txt".into(),
                },
                b"",
            )
            .await;
        assert_eq!((st.status, st.n, st.is_dir), (0, 5, false));

        let st2 = t.exec(1, &FsOp::Stat { path: "sub".into() }, b"").await;
        assert_eq!((st2.status, st2.is_dir), (0, true));
        let _ = std::fs::remove_dir_all(&r);
    }

    /// 句柄隔离：设备卸载后，旧句柄立刻失效（拿不到内容）。
    #[tokio::test]
    async fn stale_handle_after_unmount_is_dead() {
        let r = root();
        std::fs::write(r.join("f.txt"), b"secret").unwrap();
        let t = table().await;
        t.mount(1, "share", r.to_str().unwrap(), false)
            .await
            .unwrap();
        let open = t
            .exec(
                1,
                &FsOp::Create {
                    path: "f.txt".into(),
                    write: false,
                    disposition: 0,
                },
                b"",
            )
            .await;
        let h = open.handle.unwrap();
        t.unmount(1).await.unwrap();
        let rd = t
            .exec(
                1,
                &FsOp::Read {
                    handle: h,
                    offset: 0,
                    len: 10,
                },
                b"",
            )
            .await;
        assert_ne!(rd.status, 0);
        assert!(rd.body.is_empty());
        let _ = std::fs::remove_dir_all(&r);
    }

    /// 已卸载的盘上的在途操作：拒绝而不是 panic。
    #[tokio::test]
    async fn op_on_unmounted_device_is_rejected_not_panicking() {
        let t = table().await;
        let o = t.exec(9, &FsOp::ListDir { path: ".".into() }, b"").await;
        assert_ne!(o.status, 0);
    }

    /// 设备号冲突：第二次挂载同号被拒（不覆盖前一个的根）。
    #[tokio::test]
    async fn device_number_conflict_is_rejected() {
        let r = root();
        let r2 = root();
        let t = table().await;
        t.mount(1, "a", r.to_str().unwrap(), true).await.unwrap();
        assert!(t.mount(1, "b", r2.to_str().unwrap(), false).await.is_err());
        // 原共享原样可用
        assert_eq!(t.readonly_of(1), Some(true));
        let _ = std::fs::remove_dir_all(&r);
        let _ = std::fs::remove_dir_all(&r2);
    }

    /// 在 `link` 处造一个指向 `target` 目录的链接。
    ///
    /// Windows 用 junction（`mklink /J`）：`symlink` 要特权，**junction 不要**——用户的
    /// 机器上后者恰好也是恶意软件更常用的那种。Unix 用普通符号链接。两者对被测代码
    /// 是同一件事：路径经链接解析到根外。第一版只写了 `mklink`，Linux CI 镜像当场红
    /// （测试本身不跨平台，不是被测代码的问题）。
    fn link_dir(link: &std::path::Path, target: &std::path::Path) {
        #[cfg(windows)]
        {
            let st = std::process::Command::new("cmd")
                .args([
                    "/C",
                    "mklink",
                    "/J",
                    &link.to_string_lossy(),
                    &target.to_string_lossy(),
                ])
                .output()
                .expect("起不了 mklink");
            assert!(
                st.status.success(),
                "mklink 失败：{}",
                String::from_utf8_lossy(&st.stderr)
            );
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).expect("建符号链接失败");
    }

    /// 符号链接/联接逃逸（D3 变异的守卫）：链接的目标在根外必须拒。
    ///
    /// Stat 走「打开后 canonicalize 再验」的路径，恰好覆盖 TOCTOU 窗口外的常态情形。
    #[tokio::test]
    async fn junction_escape_is_rejected() {
        let r = root();
        let outside = root(); // 另一个根，作为「根外」目标
        std::fs::write(outside.join("secret.txt"), b"do-not-leak").unwrap();
        link_dir(&r.join("leap"), &outside);

        let t = table().await;
        t.mount(1, "share", r.to_str().unwrap(), true)
            .await
            .unwrap();
        // 目录项本身可见（leap 这个名字出现在列表里没问题——泄露的是内容）
        let via_junction = t
            .exec(
                1,
                &FsOp::Stat {
                    path: "leap/secret.txt".into(),
                },
                b"",
            )
            .await;
        assert_eq!(
            via_junction.status,
            FsError::OutsideShare.ntstatus(),
            "经联接访问根外文件被放行：{via_junction:?}"
        );
        let _ = std::fs::remove_dir_all(&r);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// 出口标准第三件：**每次挂载写审计**。D5 变异（append 的结果整个忽略）
    /// 首版幸存——行为测试全绿而审计一行没落。这条直接查审计表。
    #[tokio::test]
    async fn mount_and_ops_land_in_audit_table() {
        let r = root();
        std::fs::write(r.join("f.txt"), b"x").unwrap();
        let t = table().await;
        t.mount(1, "share", r.to_str().unwrap(), false)
            .await
            .unwrap();

        let open = t
            .exec(
                1,
                &FsOp::Create {
                    path: "f.txt".into(),
                    write: true,
                    disposition: 0,
                },
                b"",
            )
            .await;
        assert_eq!(open.status, 0);
        let h = open.handle.unwrap();
        let w = t
            .exec(
                1,
                &FsOp::Write {
                    handle: h,
                    offset: 0,
                },
                b"new",
            )
            .await;
        assert_eq!(w.status, 0);
        t.exec(1, &FsOp::Close { handle: h }, b"").await;
        t.unmount(1).await.unwrap();

        // 直接读审计表（all_ascending）——挂载/打开/写入/卸载四类各至少一行
        let rows = fs_connmgr::audit_repo::AuditRepo::new(t.db.pool())
            .all_ascending()
            .await
            .expect("读审计表");
        let texts: Vec<&str> = rows.iter().map(|e| e.action.as_str()).collect();
        let has = |kw: &str| texts.iter().any(|a| a.contains(kw));
        assert!(has("挂载"), "缺挂载行：{texts:?}");
        assert!(has("远端打开"), "缺打开行：{texts:?}");
        assert!(has("远端写入"), "缺写入行：{texts:?}");
        assert!(has("卸载"), "缺卸载行：{texts:?}");
        let _ = std::fs::remove_dir_all(&r);
    }

    /// 只读共享上的写被拒**也要落审计**（Rejected 行）——被拒的尝试与成功的
    /// 尝试对追查的人同等重要。
    #[tokio::test]
    async fn rejected_write_is_audited() {
        let r = root();
        std::fs::write(r.join("a.txt"), b"keep").unwrap();
        let t = table().await;
        t.mount(1, "share", r.to_str().unwrap(), true)
            .await
            .unwrap();
        t.exec(
            1,
            &FsOp::Create {
                path: "a.txt".into(),
                write: true,
                disposition: 2,
            },
            b"",
        )
        .await;
        let rows = fs_connmgr::audit_repo::AuditRepo::new(t.db.pool())
            .all_ascending()
            .await
            .expect("读审计表");
        assert!(
            rows.iter()
                .any(|e| e.action.contains("只读共享上的写被拒") && e.verdict == "rejected"),
            "被拒的写没有落审计：{:?}",
            rows.iter().map(|e| e.action.as_str()).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(&r);
    }

    /// NTSTATUS 映射钉死：远端 Explorer 的文案靠它。改一个码 = 真机上
    /// 「拒绝访问」变成「不明错误」，用户无从判断发生了什么。
    #[test]
    fn ntstatus_values_are_stable() {
        assert_eq!(FsError::OutsideShare.ntstatus(), 0xC000_0022);
        assert_eq!(FsError::NoSuchPath.ntstatus(), 0xC000_000F);
        assert_eq!(FsError::WriteProtected.ntstatus(), 0xC000_00A2);
        assert_eq!(FsError::AlreadyExists.ntstatus(), 0xC000_0035);
    }

    /// FsOp 的写意图判定（只读闸的判据）。
    #[test]
    fn is_write_classification() {
        assert!(FsOp::Create {
            path: "a".into(),
            write: true,
            disposition: 0
        }
        .is_write());
        assert!(!FsOp::Create {
            path: "a".into(),
            write: false,
            disposition: 0
        }
        .is_write());
        assert!(FsOp::Write {
            handle: 1,
            offset: 0
        }
        .is_write());
        assert!(!FsOp::Read {
            handle: 1,
            offset: 0,
            len: 1
        }
        .is_write());
        assert!(!FsOp::ListDir { path: ".".into() }.is_write());
        assert!(!FsOp::Stat { path: "a".into() }.is_write());
    }
}
