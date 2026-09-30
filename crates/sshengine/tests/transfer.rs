//! 传输管理器单测（内存 fake ops）：分块进度、重试、断点续传、下载沙箱 fail-closed，
//! 以及生产级审计钉死的四条数据完整性不变量——
//! 临时件 + 原子提交（P0-2）、per-target 互斥（P0-3）、全局唯一 id（P0-1）、有序关停（P0-5）。
use fs_sshengine::sftp::{Entry, SftpOps};
use fs_sshengine::transfer::{
    next_transfer_id, Direction, Sha256Hex, TransferEvent, TransferJob, TransferManager,
    TransferState, CHUNK,
};
use fs_sshengine::verify::ExecChannel;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// 单条事件的等待上限。最慢的用例是三次尝试的指数退避（1 s + 2 s），30 s 余量充裕。
const EV_TIMEOUT: Duration = Duration::from_secs(30);

/// 取下一条传输事件；超时或通道提前关闭一律判失败，**绝不允许挂死**。
///
/// `TransferManager` 的派发任务长期持有事件发送端，通道在管理器存活期间永不关闭：
/// 裸 `ev.recv().await` 一旦遇上「回归导致终态事件缺失」（例如取消检查被短路、
/// `run_transfer` 提前 return），会永久阻塞——nextest 只会一直打 SLOW、
/// `cargo test` 直接卡住，CI 表现为**挂住而非变红**，等同于测试失效。
/// 该风险由变异证明实测复现（禁用取消检查后本文件挂死 >280 s 未失败）。
async fn next_event(ev: &mut mpsc::Receiver<TransferEvent>) -> TransferEvent {
    match tokio::time::timeout(EV_TIMEOUT, ev.recv()).await {
        Ok(Some(e)) => e,
        Ok(None) => panic!("事件通道意外关闭：终态事件之前发送端已全部析构"),
        Err(_) => panic!("{EV_TIMEOUT:?} 内未收到下一条传输事件——终态事件缺失（挂死回归）"),
    }
}

#[derive(Default)]
struct FakeFs {
    files: Mutex<HashMap<String, Vec<u8>>>,
    fail_next_writes: Mutex<usize>,
    /// `fail_next_writes` 的下载侧对偶：前 n 次 `read_range` 注入失败。
    /// 下载方向此前没有任何故障注入手段，于是「重试之间发生了什么」这一整条路径
    /// 在下载侧无从构造——审计 P2 的 `exec_once` 重入软链检查正落在那里。
    fail_next_reads: Mutex<usize>,
    /// `fail_write_indices` 的下载侧对偶：第 n 次数据读（0 基，`.fsmeta` 不计）注入失败。
    ///
    /// `fail_next_reads` 只能打**开头**连续几次，于是下载侧造不出「先成功若干块、再彻底失败」
    /// ——而那正是「一件失败的传输在磁盘上留下带身份记录的半成品」的唯一来源，
    /// 也就是续传真正要续的那个局面（审计2 #9）。
    fail_read_indices: Mutex<Vec<usize>>,
    /// `fail_read_indices` 的计数器。**与 `read_log` 分开**：多阶段用例中间要
    /// `clear_reads()` 才能看清「第二件传输的第一次读落在哪」，若拿日志长度当索引，
    /// 清空会把注入序号一并倒回去，于是第二阶段莫名其妙又吃一发故障。
    read_seq: Mutex<usize>,
    /// 每次 write_at 的人为耗时：把「取消标志何时被读到」从纯调度巧合变成有确定窗口的事件
    /// （取消/并发/关停用例专用；其余用例保持 0 不引入等待）。
    write_delay_ms: Mutex<u64>,
    /// 第 n 次 write_at（0 基）注入失败：`fail_next_writes` 只能打头几次，
    /// 无法制造「先成功若干块、中途断一次」这种断点续传真正的场景（S46 用例需要）。
    fail_write_indices: Mutex<Vec<usize>>,
    /// 每次 write_at 的 (offset, len) 流水：用于断言重试后**没有退回起点**。
    write_log: Mutex<Vec<(u64, usize)>>,
    /// 「第 n 次数据写（0 基，与 `fail_write_indices` 同一套编号）**之后**，把某个远端文件
    /// 换成这份内容（`None` = 删掉它）」——上传侧的 `mutate_after_read` 对偶。
    ///
    /// 上传方向一次远端读都不发（源在本地），于是 `mutate_after_read` 在这一侧永远不触发，
    /// 「传输途中远端状态被第三方改动」这件事在上传侧此前根本无从构造。审计2 #12 要测的
    /// 恰恰是它：另一个进程开工时会把 `<临时件>.fsmeta` 改写成自己那一份，而它提交成功后
    /// 又会把这份记录删掉——两种结局分别对应 `Some` 与 `None`，缺一不可（见 `precommit_gate`
    /// 文档里关于「① 不适用降级口径」的那段）。
    mutate_after_write: Mutex<Vec<WriteMutation>>,
    /// 模拟「零长度写不物化文件」的服务端：`prepare_part` 靠一次 0 字节
    /// `OPEN(WRITE|CREATE)+write` 把临时件建出来，但这不是 SFTP 强保证的行为。
    /// 置位后本 fake 拒绝物化，用来验证 `commit` 在临时件缺席时**不去删最终目标**。
    swallow_empty_create: Mutex<bool>,
    /// 每个远端路径的 mtime（Unix 秒），`stat_meta` 读它；缺省 0 = 服务端未回该属性。
    /// 用来构造「长度不变但内容被就地改写」——那一格长度这一维完全看不见（审计2 #15）。
    mtimes: Mutex<HashMap<String, i64>>,
    /// 每次 `read_range` 的 (path, offset, len) 流水。
    ///
    /// 下载侧此前**没有**任何读偏移的观测手段，于是「续传到底有没有真的从断点起跑」只能靠
    /// 「最终内容对不对」间接推断——而那条推断在「从 0 重传」时同样成立，两者无从区分。
    /// 审计2 #9 加了身份校验之后这一点变致命：老用例即便退化成全量重传也照样全绿。
    read_log: Mutex<Vec<(String, u64, usize)>>,
    /// 「第 n 次数据读**之后**，把某个远端文件换成这份内容（并可改 mtime）」。
    ///
    /// 这是唯一能确定性构造「源文件在传输途中被改」的手段：那件事只可能发生在两次远端往返
    /// 之间，而测试线程与 worker 之间没有别的同步点，靠 sleep 去撞窗口就是在写随机变红的用例。
    mutate_after_read: Mutex<Vec<ReadMutation>>,
    /// 「第 n 次数据读之后，把这个**本地**文件换成这份内容」——`mutate_after_read` 的本地对偶。
    ///
    /// 下载方向的伴生身份记录落在本地磁盘上，不经 `SftpOps`，因此 `mutate_after_read` 够不着它。
    /// 而「另一个写者在传输途中改写了它」必须发生在两次远端读**之间**才具有确定性：
    /// 靠 sleep 去撞那个窗口写出来的就是随机变红的用例。
    mutate_local_after_read: Mutex<Vec<(usize, std::path::PathBuf, Vec<u8>)>>,
    /// 「rename 到这个路径还要再失败几次」。递减到 0 后放行。
    ///
    /// 现有 fake 只在「目标已存在」时拒绝 rename，于是 `commit` 的让位/回退分支（审计2 #8）
    /// 一条都走不到——而那几条分支处置的正是用户**已有的那个文件**，是本次修复的要害。
    fail_rename_to: Mutex<HashMap<String, usize>>,
    /// 第 n 次数据写（0 基，与 `fail_write_indices` 同一套编号）**报成功但什么都不落盘**。
    ///
    /// 这不是人造场景：SFTP 的写是流水线式的，服务端对一发 `SSH_FXP_WRITE` 回了 OK
    /// 之后仍可能因为配额、写缓存回刷失败或半断的连接而没有真正持久化；客户端这一侧
    /// `offset` 早已按「返回 Ok」推进过了。它是提交前闸门第二关（临时件长度 == total）
    /// 唯一存在的理由，而那一关此前没有任何用例走到过。
    swallow_write_indices: Mutex<Vec<usize>>,
    /// 服务端 REALPATH 的桌面版（审计2 #13）：登记过的路径回登记值，没登记的原样返回。
    ///
    /// 值为 `None` 表示「这条路径上 REALPATH 直接失败」——服务端不支持、chroot 拒绝、
    /// 或者干脆是个不存在的目录。这一格不是锦上添花：规范化失败时的**降级**语义
    ///（退回纯词法键、照常传输）与成功时的**收紧**语义同等重要，而降级路径只能从这里造。
    canon: Mutex<HashMap<String, Option<String>>>,
    /// `sync` 与 `rename` 的先后流水（`"sync <path>"` / `"rename <from> -> <to>"`）。
    /// 「提交前先落盘」是次序问题，只看最终内容证明不了次序。
    commit_log: Mutex<Vec<String>>,
    /// 置位后 `sync` 失败：验证落盘失败时**不改名**、临时件留着、最终目标不被触碰。
    fail_sync: Mutex<bool>,
}

/// (第几次数据读之后, 目标路径, 新内容, 新 mtime)
type ReadMutation = (usize, String, Vec<u8>, Option<i64>);

/// (第几次数据写之后, 目标路径, 新内容；`None` = 删掉它)
type WriteMutation = (usize, String, Option<Vec<u8>>);

impl FakeFs {
    /// 远端某路径的当前内容（不存在 → None）。断言临时件/最终目标状态时统一走它。
    fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.files.lock().unwrap().get(path).cloned()
    }

    /// 到目前为止的数据读流水（`.fsmeta` 已在 `read_range` 里排除）。
    fn reads(&self) -> Vec<(String, u64, usize)> {
        self.read_log.lock().unwrap().clone()
    }

    /// 清空读流水。续传用例要分两段看：第一段制造半成品，第二段才是被断言的那次续传。
    fn clear_reads(&self) {
        self.read_log.lock().unwrap().clear();
    }

    /// 给某条路径登记 REALPATH 应答。`None` = 这条路径上 REALPATH 失败。
    ///
    /// 没登记过的路径原样返回（见 `canonicalize`），因此**只需登记会被问到的那一条**——
    /// 引擎问的是目标的父目录，不是目标本身。
    fn set_canon(&self, path: &str, answer: Option<&str>) {
        self.canon
            .lock()
            .unwrap()
            .insert(path.to_string(), answer.map(str::to_string));
    }
}

#[async_trait::async_trait]
impl SftpOps for FakeFs {
    async fn list(
        &self,
        path: &str,
    ) -> Result<fs_sshengine::sftp::ListResult, fs_sshengine::Error> {
        Ok(fs_sshengine::sftp::ListResult {
            entries: self
                .files
                .lock()
                .unwrap()
                .keys()
                .filter(|k| k.starts_with(path))
                .map(|k| Entry {
                    name: k.clone(),
                    is_dir: false,
                    is_symlink: false,
                    size: 0,
                    mtime: 0,
                    perms: None,
                })
                .collect(),
            truncated: false,
        })
    }
    /// 文件不存在必须报错而非返回 Ok(0)：真实 SFTP 服务端回的是 SSH_FX_NO_SUCH_FILE，
    /// 若 fake 端把「不存在」和「空文件」抹平成同一个 Ok(0)，`is_not_found` 那条
    /// 「只有 ENOENT 才归 0、其余错误一律传播」的判定（审计 P1-6）就无从被测到。
    async fn stat_size(&self, path: &str) -> Result<u64, fs_sshengine::Error> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .map(|v| v.len() as u64)
            .ok_or_else(|| fs_sshengine::Error::Sftp(format!("no such file: {path}")))
    }
    async fn read_range(
        &self,
        path: &str,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, fs_sshengine::Error> {
        // 身份记录的读取既不入流水也不消耗故障额度：它不是数据读。混进去会让
        //「续传从哪个偏移起跑」的断言多出无关的行，也会把「第 n 次数据读失败」的用例
        // 悄悄变成「读身份记录失败」——断言仍绿而覆盖的路径已经完全不同。
        // 理由与下方 `write_at` 对空写与 `.fsmeta` 的豁免完全相同。
        if !path.ends_with(".fsmeta") {
            self.read_log
                .lock()
                .unwrap()
                .push((path.to_string(), offset, len));
            let idx = {
                let mut seq = self.read_seq.lock().unwrap();
                let i = *seq;
                *seq += 1;
                i
            };
            if self.fail_read_indices.lock().unwrap().contains(&idx) {
                return Err(fs_sshengine::Error::Sftp("injected read at index".into()));
            }
            {
                let mut n = self.fail_next_reads.lock().unwrap();
                if *n > 0 {
                    *n -= 1;
                    return Err(fs_sshengine::Error::Sftp("injected read failure".into()));
                }
            }
            // 本次读的结果先取出来，改源的动作放在返回之前——顺序就是「读到了旧内容，
            // 紧接着源被人改掉」，与真实世界里那一瞬完全同构。
            let out = {
                let fs = self.files.lock().unwrap();
                let f = fs
                    .get(path)
                    .ok_or_else(|| fs_sshengine::Error::Sftp("enoent".into()))?;
                let start = (offset as usize).min(f.len());
                f[start..(start + len).min(f.len())].to_vec()
            };
            for (at, p, content, mt) in self.mutate_after_read.lock().unwrap().iter() {
                if *at != idx {
                    continue;
                }
                self.files
                    .lock()
                    .unwrap()
                    .insert(p.clone(), content.clone());
                if let Some(mt) = mt {
                    self.mtimes.lock().unwrap().insert(p.clone(), *mt);
                }
            }
            for (at, p, content) in self.mutate_local_after_read.lock().unwrap().iter() {
                if *at == idx {
                    // 写不动就当场炸：这是用例的**前置条件**，静默跳过会让断言在一个
                    // 根本没发生过并发的场景上空过，还照样是绿的。
                    std::fs::write(p, content)
                        .unwrap_or_else(|e| panic!("测试钩子写不动 {}：{e}", p.display()));
                }
            }
            return Ok(out);
        }
        let fs = self.files.lock().unwrap();
        let f = fs
            .get(path)
            .ok_or_else(|| fs_sshengine::Error::Sftp("enoent".into()))?;
        let start = (offset as usize).min(f.len());
        Ok(f[start..(start + len).min(f.len())].to_vec())
    }
    async fn write_at(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<(), fs_sshengine::Error> {
        // 身份记录（`<临时件>.fsmeta`，审计2 #9）同样不是数据写：不计流水、不吃故障额度。
        // 理由与下面那段空写豁免逐字相同——它落在 `prepare_part` 里，紧挨着物化那一发，
        // 不豁免就会把每个用例的第一发故障注入吃掉。
        if path.ends_with(".fsmeta") {
            let mut fs = self.files.lock().unwrap();
            let f = fs.entry(path.to_string()).or_default();
            let start = offset as usize;
            if f.len() < start + data.len() {
                f.resize(start + data.len(), 0);
            }
            f[start..start + data.len()].copy_from_slice(data);
            return Ok(());
        }
        // 零长度写是**物化**动作（建出临时件），不是数据写：既不计入 write_log 流水，
        // 也不消耗故障注入额度。否则 `fail_next_writes` / `fail_write_indices` 会被
        // 这一发空写吃掉，把「第 n 块数据写失败」的用例悄悄变成「准备临时件失败」，
        // 断言仍绿而覆盖的路径已经完全不同。
        if data.is_empty() {
            if *self.swallow_empty_create.lock().unwrap() {
                return Ok(());
            }
            self.files
                .lock()
                .unwrap()
                .entry(path.to_string())
                .or_default();
            return Ok(());
        }
        let idx = {
            let idx = self.write_log.lock().unwrap().len();
            self.write_log.lock().unwrap().push((offset, data.len()));
            if self.fail_write_indices.lock().unwrap().contains(&idx) {
                return Err(fs_sshengine::Error::Sftp("simulated-at-index".into()));
            }
            // 报成功但不落盘。放在**流水登记之后、写盘之前**：调用方看到的是一发完全正常的写。
            if self.swallow_write_indices.lock().unwrap().contains(&idx) {
                return Ok(());
            }
            idx
        };
        {
            let mut fail = self.fail_next_writes.lock().unwrap();
            if *fail > 0 {
                *fail -= 1;
                return Err(fs_sshengine::Error::Sftp("simulated".into()));
            }
        }
        let delay = *self.write_delay_ms.lock().unwrap();
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        {
            let mut fs = self.files.lock().unwrap();
            let f = fs.entry(path.to_string()).or_default();
            let start = offset as usize;
            if f.len() < start + data.len() {
                f.resize(start + data.len(), 0);
            }
            f[start..start + data.len()].copy_from_slice(data);
        }
        // 改动落在**本次写成功之后**：顺序就是「我这一发写进去了，紧接着别人动了远端状态」，
        // 与 `mutate_after_read` 的 "读到旧内容，紧接着源被改" 一致。
        for (at, p, content) in self.mutate_after_write.lock().unwrap().iter() {
            if *at != idx {
                continue;
            }
            let mut fs = self.files.lock().unwrap();
            match content {
                Some(bytes) => fs.insert(p.clone(), bytes.clone()),
                None => fs.remove(p),
            };
        }
        Ok(())
    }
    async fn truncate(&self, path: &str, size: u64) -> Result<(), fs_sshengine::Error> {
        let mut fs = self.files.lock().unwrap();
        let f = fs
            .get_mut(path)
            .ok_or_else(|| fs_sshengine::Error::Sftp(format!("no such file: {path}")))?;
        f.resize(size as usize, 0);
        Ok(())
    }
    async fn mkdir(&self, _p: &str) -> Result<(), fs_sshengine::Error> {
        Ok(())
    }
    async fn remove(&self, p: &str) -> Result<(), fs_sshengine::Error> {
        self.files.lock().unwrap().remove(p);
        Ok(())
    }
    /// 真的搬运表项，并按 SFTP v3 语义在**目标已存在时拒绝**。
    ///
    /// 原实现是空转返回 Ok：临时件方案（审计 P0-2）的每一条断言都会因此空过——
    /// 「上传后最终目标 == 新内容」在一个 rename 什么都不做的 fake 上根本不可能成立，
    /// 而「最终目标未被破坏」则会因为最终目标压根没被写过而假绿。拒绝已存在目标同样是
    /// 必需的：SSH_FXP_RENAME 没有覆盖语义，`commit` 的「删旧目标后重试一次」分支
    /// 只有在 fake 端如实拒绝时才会被走到。
    async fn sync(&self, path: &str) -> Result<(), fs_sshengine::Error> {
        self.commit_log.lock().unwrap().push(format!("sync {path}"));
        if *self.fail_sync.lock().unwrap() {
            return Err(fs_sshengine::Error::Sftp(format!(
                "injected fsync failure: {path}"
            )));
        }
        Ok(())
    }
    async fn rename(&self, from: &str, to: &str) -> Result<(), fs_sshengine::Error> {
        self.commit_log
            .lock()
            .unwrap()
            .push(format!("rename {from} -> {to}"));
        {
            let mut budget = self.fail_rename_to.lock().unwrap();
            if let Some(n) = budget.get_mut(to) {
                if *n > 0 {
                    *n -= 1;
                    return Err(fs_sshengine::Error::Sftp(format!(
                        "injected rename failure: {to}"
                    )));
                }
            }
        }
        let mut fs = self.files.lock().unwrap();
        if fs.contains_key(to) {
            return Err(fs_sshengine::Error::Sftp(format!(
                "rename failed: destination exists: {to}"
            )));
        }
        let v = fs
            .remove(from)
            .ok_or_else(|| fs_sshengine::Error::Sftp(format!("no such file: {from}")))?;
        fs.insert(to.to_string(), v);
        Ok(())
    }
    async fn lstat(&self, path: &str) -> Result<fs_sshengine::sftp::FileMeta, fs_sshengine::Error> {
        let fs = self.files.lock().unwrap();
        let size = fs.get(path).map(|v| v.len() as u64).unwrap_or(0);
        Ok(fs_sshengine::sftp::FileMeta {
            file_type: if fs.contains_key(path) {
                fs_sshengine::sftp::FileType::Regular
            } else {
                fs_sshengine::sftp::FileType::Other
            },
            size,
            mtime: 0,
            mode: None,
            uid: None,
            gid: None,
        })
    }
    /// 与 `stat_size` 同口径：**不存在必须报错**，不能抹平成 `Ok(size 0)`。
    /// 下载方向的源身份取值走这里（`source_identity`），若这里对不存在的远端文件回 Ok，
    /// 「远端源文件读不到就失败」那条不变量（审计 P1-6）会静默失守。
    ///
    /// `mtime` 取自 `mtimes` 表，缺省 0 = 「服务端没回该属性」，`source_identity` 会把它
    /// 折成 `None`。想构造「长度不变但内容被就地改写」的用例就往这张表里写值。
    async fn stat_meta(
        &self,
        path: &str,
    ) -> Result<fs_sshengine::sftp::FileMeta, fs_sshengine::Error> {
        let fs = self.files.lock().unwrap();
        let size = fs
            .get(path)
            .map(|v| v.len() as u64)
            .ok_or_else(|| fs_sshengine::Error::Sftp(format!("no such file: {path}")))?;
        Ok(fs_sshengine::sftp::FileMeta {
            file_type: fs_sshengine::sftp::FileType::Regular,
            size,
            mtime: self.mtimes.lock().unwrap().get(path).copied().unwrap_or(0),
            mode: None,
            uid: None,
            gid: None,
        })
    }
    async fn read_link(&self, p: &str) -> Result<String, fs_sshengine::Error> {
        Err(fs_sshengine::Error::Sftp(format!("not a symlink: {p}")))
    }
    async fn symlink(&self, _target: &str, _link: &str) -> Result<(), fs_sshengine::Error> {
        Ok(())
    }

    async fn canonicalize(&self, path: &str) -> Result<String, fs_sshengine::Error> {
        match self.canon.lock().unwrap().get(path) {
            Some(Some(real)) => Ok(real.clone()),
            Some(None) => Err(fs_sshengine::Error::Sftp(
                "realpath: 服务端拒绝（本 fake 刻意注入）".into(),
            )),
            None => Ok(path.to_string()),
        }
    }

    /// 审计2 #17：本测试面只走传输锁键，不删目录——目录删除的守卫递归在 sftp.rs 的
    /// MemFs 面测，这里若被走到就是传输层把目录当文件删了，当场炸掉。
    async fn remove_dir(&self, _: &str) -> Result<(), fs_sshengine::Error> {
        unreachable!("算锁键不该删目录")
    }
}

/// 远端 `sha256sum` 的最小实现：对 fake 服务端**内存里那份内容**做哈希。
///
/// 有了它，上传方向的提交前闸门第三关（内容哈希）才走得到。这一关是审计2 #10 在上传侧的
/// 全部分量所在：下载侧的哈希在本地算、天然可测，而上传侧要一条 exec 通道去远端算，
/// 通道不存在时那一关是直接跳过的——于是它此前从未被任何用例执行过。
///
/// 哈希本身借 `verify::file_sha256` 落一趟临时文件来算，而不是引入 `sha2` 作测试依赖：
/// 用的是被测 crate 自己的公开实现，顺带保证两侧口径一致（它正是 `parse_sha256sum` 的对家）。
struct HashingExec(Arc<FakeFs>);

#[async_trait::async_trait]
impl ExecChannel for HashingExec {
    async fn exec_once(
        &self,
        cmd: &str,
    ) -> Result<fs_sshengine::verify::ExecOutput, fs_sshengine::Error> {
        // 命令形如 `sha256sum -- <shell_quote(path)>`。
        let arg = cmd.split_once(" -- ").map(|(_, a)| a).unwrap_or_default();
        let path = arg.trim().trim_matches('\'').replace("'\\''", "'");
        let Some(bytes) = self.0.get(&path) else {
            // 与真实 sha256sum 一致：文件不存在是非零退出，`precommit_gate` 会把它当
            //「没拿到证据」降级放行，而不是当成不符。
            return Ok(fs_sshengine::verify::ExecOutput {
                code: Some(1),
                stdout: String::new(),
                stderr: format!("sha256sum: {path}: No such file"),
            });
        };
        let tmp = write_temp(&bytes);
        let hex = fs_sshengine::verify::file_sha256(&tmp)
            .await
            .expect("临时文件刚写好，哈希不该失败");
        let _ = std::fs::remove_file(&tmp);
        Ok(fs_sshengine::verify::ExecOutput {
            code: Some(0),
            stdout: format!("{}  {path}\n", hex.0),
            stderr: String::new(),
        })
    }
}

/// 上传作业模板。`target_endpoint` 每个用例给一个专属值：目标锁注册表是**进程级** static，
/// 同一测试二进制里的用例是并发跑的，共用端点会让互不相干的用例抢同一把锁而随机变红。
fn up_job(local: std::path::PathBuf, remote: &str, endpoint: &str) -> TransferJob {
    TransferJob {
        direction: Direction::Up,
        local,
        remote: remote.into(),
        resume: false,
        sandbox_root: None,
        target_endpoint: endpoint.into(),
        endpoint_aliases: Vec::new(),
        verify: None,
    }
}

/// 带端点别名的上传作业模板（审计2 #13）。别名是 app 层在会话装配时解析出来的对端地址，
/// 引擎自己不做 DNS——这里手填的正是那份解析结果。
fn up_job_aliased(
    local: std::path::PathBuf,
    remote: &str,
    endpoint: &str,
    aliases: &[&str],
) -> TransferJob {
    TransferJob {
        endpoint_aliases: aliases.iter().map(|s| s.to_string()).collect(),
        ..up_job(local, remote, endpoint)
    }
}

/// 下载作业模板。下载的目标键用的是解析后的本地绝对路径（不掺端点），而每个用例的沙箱根
/// 都是独立的纳秒级临时目录，天然不会互撞。
fn down_job(local: std::path::PathBuf, remote: &str, root: std::path::PathBuf) -> TransferJob {
    TransferJob {
        direction: Direction::Down,
        local,
        remote: remote.into(),
        resume: false,
        sandbox_root: Some(root),
        target_endpoint: "test".into(),
        endpoint_aliases: Vec::new(),
        verify: None,
    }
}

/// `<路径>.fspart` —— 与引擎内部约定一致的临时件名（断言用）。
fn part_of(p: &std::path::Path) -> std::path::PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(".fspart");
    std::path::PathBuf::from(s)
}

/// `<临时件>.fsmeta` —— 续传身份记录（审计2 #9）。入参是**临时件**路径，不是最终目标。
fn meta_of(part: &std::path::Path) -> std::path::PathBuf {
    let mut s = part.as_os_str().to_os_string();
    s.push(".fsmeta");
    std::path::PathBuf::from(s)
}

/// 循环收事件直到本 id 到达终态，返回终态。非终态一律忽略（其他用例的事件不会串进来：
/// 每个用例有自己的 TransferManager 与事件通道）。
///
/// 只适用于**同一通道上只关心一个 id** 的场景：本函数会把别的 id 的事件直接丢弃，
/// 多件并发时请用 `drain_terminals`。
async fn drain_to_terminal(
    ev: &mut mpsc::Receiver<TransferEvent>,
    id: fs_sshengine::transfer::TransferId,
) -> TransferState {
    drain_terminals(ev, &[id]).await.remove(&id).unwrap()
}

/// 一趟收干多件的终态。必须一趟收：串行地「先等 a 的终态、再等 b 的终态」会在等 a 的过程中
/// 把 b 的终态事件当成无关事件丢掉，随后对 b 的等待必然超时——同通道多件的经典自伤。
async fn drain_terminals(
    ev: &mut mpsc::Receiver<TransferEvent>,
    ids: &[fs_sshengine::transfer::TransferId],
) -> HashMap<fs_sshengine::transfer::TransferId, TransferState> {
    let mut out = HashMap::new();
    while out.len() < ids.len() {
        let e = next_event(ev).await;
        if !ids.contains(&e.id) {
            continue;
        }
        match e.state {
            TransferState::Running | TransferState::Retrying { .. } => {}
            terminal => {
                out.insert(e.id, terminal);
            }
        }
    }
    out
}

#[tokio::test]
async fn upload_chunked_with_progress_and_done() {
    let fs = Arc::new(FakeFs::default());
    let mgr = TransferManager::spawn(fs.clone(), 2);
    let local = write_temp("payload".repeat(100_000).as_bytes()); // 700 000 字节
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/up.bin", "ns-chunked"))
        .await
        .expect("submit 应受理");

    // 循环唯一的正常出口是 Done；终态缺失由 next_event 超时判红（不再有 done 布尔位——
    // 它在 loop 结构下恒为 true，是个永不失败的空断言）。其余状态穷举列出，
    // 不用 `_ => {}` 兜底：无故障注入的顺利上传里出现 Retrying/Cancelled 本身就是缺陷。
    let mut progress_events = 0;
    loop {
        let e = next_event(&mut ev).await;
        if e.id != id {
            continue;
        }
        match e.state {
            TransferState::Running => progress_events += 1,
            TransferState::Done => break,
            TransferState::Failed(msg) => panic!("顺利上传不应失败: {msg}"),
            TransferState::Retrying { attempt } => panic!("未注入故障却重试: attempt={attempt}"),
            TransferState::Cancelled => panic!("未取消却收到 Cancelled"),
        }
    }
    assert!(
        progress_events > 1,
        "分块上传须在 Done 之前发出多条进度事件，实得 progress={progress_events}"
    );
    assert_eq!(fs.get("/up.bin").map(|v| v.len()), Some(700_000));
    assert_eq!(
        fs.get("/up.bin.fspart"),
        None,
        "提交后临时件必须已被 rename 走，不得留在远端目录里"
    );
}

/// 提交次序：临时件**先落盘再改名**。`write_at` 为吞吐不再逐块 fsync，整个文件的持久性
/// 系于提交前这一次——次序反了（先改名后落盘）或干脆漏掉，断电后可能得到一个名字是新的、
/// 内容残缺的文件，而用户原来的文件已被顶替。
#[tokio::test]
async fn upload_syncs_the_part_before_renaming_it_into_place() {
    let fs = Arc::new(FakeFs::default());
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let local = write_temp(b"durable-before-rename");
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/durable.bin", "ns-sync-order"))
        .await
        .expect("submit 应受理");
    loop {
        let e = next_event(&mut ev).await;
        if e.id != id {
            continue;
        }
        match e.state {
            TransferState::Done => break,
            TransferState::Failed(msg) => panic!("顺利上传不应失败: {msg}"),
            _ => {}
        }
    }
    let log = fs.commit_log.lock().unwrap().clone();
    let sync_at = log
        .iter()
        .position(|l| l == "sync /durable.bin.fspart")
        .unwrap_or_else(|| panic!("提交前没有对临时件落盘：{log:?}"));
    let rename_at = log
        .iter()
        .position(|l| l == "rename /durable.bin.fspart -> /durable.bin")
        .unwrap_or_else(|| panic!("没有提交改名：{log:?}"));
    assert!(sync_at < rename_at, "必须先落盘再改名：{log:?}");
}

/// 落盘失败即不提交：不改名、最终目标原样、临时件留着（续传/重试还有路可走）。
#[tokio::test]
async fn upload_does_not_commit_when_the_final_sync_fails() {
    let fs = Arc::new(FakeFs::default());
    fs.files
        .lock()
        .unwrap()
        .insert("/keep.bin".into(), b"users-original".to_vec());
    *fs.fail_sync.lock().unwrap() = true;
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let local = write_temp(b"new-content-that-was-not-durable");
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/keep.bin", "ns-sync-fail"))
        .await
        .expect("submit 应受理");
    let msg = loop {
        let e = next_event(&mut ev).await;
        if e.id != id {
            continue;
        }
        match e.state {
            TransferState::Failed(msg) => break msg,
            TransferState::Done => panic!("落盘失败却报完成"),
            _ => {}
        }
    };
    assert!(msg.contains("落盘"), "失败原因要说清是落盘失败：{msg}");
    assert_eq!(
        fs.get("/keep.bin").as_deref(),
        Some(&b"users-original"[..]),
        "落盘失败时用户原文件必须原样"
    );
    assert!(
        !fs.commit_log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("rename")),
        "落盘失败后不得再改名"
    );
    assert!(
        fs.get("/keep.bin.fspart").is_some(),
        "临时件应留着，续传才有得续"
    );
}

#[tokio::test]
async fn retries_then_succeeds() {
    let fs = Arc::new(FakeFs::default());
    *fs.fail_next_writes.lock().unwrap() = 2; // 前两次写失败
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let local = write_temp(b"retry-me");
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/r.bin", "ns-retry"))
        .await
        .expect("submit 应受理");
    let mut saw_retry = false;
    loop {
        let e = next_event(&mut ev).await;
        if e.id != id {
            continue;
        }
        match e.state {
            TransferState::Retrying { .. } => saw_retry = true,
            TransferState::Done => break,
            TransferState::Failed(m) => panic!("{m}"),
            _ => {}
        }
    }
    assert!(saw_retry, "两次写失败后应先发 Retrying 再成功");
    assert_eq!(fs.get("/r.bin").as_deref(), Some(&b"retry-me"[..]));
}

/// 断点续传：半成品必须由**引擎自己**留下，续传才谈得上「续」。
///
/// 老写法是测试手工 `std::fs::write(&part, &payload[..400])` 造一个半成品，然后只断言
/// 「最终内容对」。这条断言在**从 0 全量重传**时同样成立——两种实现的产物逐字节相同，
/// 于是「续传退化成全量重传」这件事在它眼里完全不存在。审计2 #9 加了身份校验之后这一点
/// 变致命：手工造的半成品旁边没有身份记录，新引擎必然判不匹配、必然从 0 重来，
/// 而老断言照样全绿。**判别式只能是读偏移**，故本用例钉 `read_log`。
///
/// 构造：注入三次读失败让第一件传输耗尽三次尝试并以 Failed 收尾——此时磁盘上留下的是
/// 引擎自己写的 `.fspart`（一个分块）+ `.fspart.fsmeta`。第二件同参数重投，必须从 一个分块 起跑。
/// 这同时钉住了两条不变量：失败**不得**清掉身份记录（否则续传永远续不上），
/// 以及成功提交**必须**清掉它（否则目录里堆垃圾）。
#[tokio::test]
async fn resume_downloads_from_offset() {
    let fs = Arc::new(FakeFs::default());
    // 700 000 字节 = 3 个分块（CHUNK / CHUNK / 余下 177 760）
    let payload: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    fs.files
        .lock()
        .unwrap()
        .insert("/big.bin".into(), payload.clone());
    // 读流水：#0 @0 成功 → #1 @CHUNK 失败（重试）→ #2 @CHUNK 失败 → #3 @CHUNK 失败（终态）
    *fs.fail_read_indices.lock().unwrap() = vec![1, 2, 3];
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let root = temp_subdir(); // 下载沙箱根（submit 前必须已存在）
    let dest = root.join("big.bin");
    let part = part_of(&dest);
    let meta = meta_of(&part);
    let mut ev = mgr.events().await;

    let mut first = down_job(dest.clone(), "/big.bin", root.clone());
    first.resume = true;
    let id = mgr.submit(first).await.expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Failed(_) => {}
        other => panic!("三次读全失败应以 Failed 收尾，实得 {other:?}"),
    }
    assert_eq!(
        std::fs::metadata(&part).map(|m| m.len()).ok(),
        Some(CHUNK as u64),
        "第一件应留下一个 一个分块 的半成品（否则第二件续的不是同一个局面）"
    );
    assert!(
        meta.exists(),
        "失败**不得**清掉身份记录，否则下一次续传永远续不上：{}",
        meta.display()
    );

    fs.clear_reads();
    let mut second = down_job(dest.clone(), "/big.bin", root);
    second.resume = true;
    let id2 = mgr.submit(second).await.expect("submit 应受理");
    match drain_to_terminal(&mut ev, id2).await {
        TransferState::Done => {}
        other => panic!("续传不应以 {other:?} 收尾"),
    }
    let reads = fs.reads();
    assert_eq!(
        reads.first().map(|r| r.1),
        Some(CHUNK as u64),
        "续传的第一次数据读必须落在断点 一个分块 上，从 0 起跑即为退化成全量重传（实得流水 {reads:?}）"
    );
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        payload,
        "续传后本地文件须与远端逐字节一致（已落盘的前 一个分块 不得被重写或截断）"
    );
    assert!(
        !part.exists(),
        "提交后临时件必须已被 rename 走：{}",
        part.display()
    );
    assert!(
        !meta.exists(),
        "提交成功后身份记录必须清掉，不得在下载目录里留垃圾：{}",
        meta.display()
    );
}

/// 审计2 #9：临时件的**身份**对不上时必须整份重来，绝不能只因长度合适就接着写。
///
/// 老实现只问一句「`.fspart` 有多长」。远端目录里躺着一个同名残件——上一次传的是**别的**
/// 文件、或者同一个文件的上一版——它的长度 400 完全合法，于是引擎从 400 接着写，
/// 最终交付的是「别人的前 400 字节 + 这次的后 600 字节」。文件能打开、长度对、
/// 校验若关着就一路绿到底，用户拿到一份静默损坏的东西。
///
/// 本用例的判别式是**内容**：从 400 续写必然产出混合物，逐字节比对当场抓住。
/// 同时钉读偏移，把「重来了」这件事本身也证出来。
#[tokio::test]
async fn stale_part_without_matching_identity_is_not_resumed() {
    let fs = Arc::new(FakeFs::default());
    let payload: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    fs.files
        .lock()
        .unwrap()
        .insert("/big.bin".into(), payload.clone());
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let root = temp_subdir();
    let dest = root.join("big.bin");
    let part = part_of(&dest);
    // 另一个文件留下的残件：长度合法（400 < 1000），内容与本次要传的东西毫无关系，
    // 旁边没有身份记录。
    std::fs::write(&part, vec![0xABu8; 400]).unwrap();
    let mut ev = mgr.events().await;
    let mut job = down_job(dest.clone(), "/big.bin", root);
    job.resume = true;
    let id = mgr.submit(job).await.expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Done => {}
        other => panic!("身份不符应当整份重传并成功，实得 {other:?}"),
    }
    let reads = fs.reads();
    assert_eq!(
        reads.first().map(|r| r.1),
        Some(0),
        "身份对不上时必须从 0 重来（实得流水 {reads:?}）"
    );
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        payload,
        "交付内容必须整份等于源；出现「残件前缀 + 本次后缀」的混合物即为静默损坏"
    );
}

/// 临时件已经是**完整**长度（`n == total`）且身份相符 —— 这是「传完了但还没提交」的正常局面，
/// 必须直接进提交，而不是重传一遍。
///
/// 旧实现在这里有个更糟的分支：`n == total` 被归入「续传」，于是 `offset == total`、
/// 一个字节都不读就 `Completed`，把那份**来历不明**的残件当成成品 rename 上去。
/// 长度这一维本来就判不出身份，而这条路径连一次读都没有，损坏是零成本发生的。
/// 修好之后判身份：相符才直接提交（本用例），不符则整份重来（上一条用例）。
#[tokio::test]
async fn complete_part_with_matching_identity_commits_without_refetching() {
    let fs = Arc::new(FakeFs::default());
    let payload: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    fs.files
        .lock()
        .unwrap()
        .insert("/whole.bin".into(), payload.clone());
    // 先让引擎自己写出一份带身份记录的半成品：读流水 #0/#1 收下前两块（共 524 288 字节），
    // #2/#3/#4 三次尝试的收尾块全部失败 → Failed。随后由测试把最后一块补齐，
    // 得到「临时件已完整、身份记录仍在、但没提交」这个正是要测的局面。
    // （直接跑一件成功的传输是造不出它的：成功会顺手 rename 掉临时件并清掉身份记录。）
    *fs.fail_read_indices.lock().unwrap() = vec![2, 3, 4];
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let root = temp_subdir();
    let dest = root.join("whole.bin");
    let part = part_of(&dest);
    let mut ev = mgr.events().await;

    let mut first = down_job(dest.clone(), "/whole.bin", root.clone());
    first.resume = true;
    let id = mgr.submit(first).await.expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Failed(_) => {}
        other => panic!("三次读全失败应以 Failed 收尾，实得 {other:?}"),
    }
    // 把半成品补齐到完整长度，模拟「上一轮其实已经收完，只是没来得及提交」。
    // 身份记录是引擎自己写的，原样留着 —— 这正是本用例要走的那条路。
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&part)
            .unwrap();
        f.write_all(&payload[2 * CHUNK..]).unwrap();
    }
    assert_eq!(std::fs::metadata(&part).unwrap().len(), 700_000);

    fs.clear_reads();
    let mut second = down_job(dest.clone(), "/whole.bin", root);
    second.resume = true;
    let id2 = mgr.submit(second).await.expect("submit 应受理");
    match drain_to_terminal(&mut ev, id2).await {
        TransferState::Done => {}
        other => panic!("完整临时件 + 身份相符应直接提交，实得 {other:?}"),
    }
    assert_eq!(
        fs.reads(),
        Vec::new(),
        "临时件已完整且身份相符时不该再读远端一个字节"
    );
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    assert!(!part.exists(), "提交后临时件必须已被 rename 走");
}

/// 审计2 #15：源文件在传输途中**变短**，必须在提交之前被挡住。
///
/// 改动点落在最后一次数据读**之后**：此时字节已经收全，`exec_once` 会干干净净地返回
/// `Completed`，传输本身没有任何异常可报——提交前闸门是横在这份快照与用户文件之间的
/// 唯一一道门。把改动点放在中途反而测不到这道门：源变短会让后续的读返回空块，
/// `exec_once` 自己就以「远端提前返回空块」失败并进重试，走的是另一条路。
///
/// 为什么变短必须硬失败，哪怕这一份读起来是完整的：引擎无从知道截断发生在第几次读之前。
/// 同一组观测（开工 700 000 字节、收工 100 字节）既可能对应「读完才被截断」，也可能对应
/// 「读到一半被换成了另一个文件」，后者手里这份就是新旧字节的混合物。判不出来时，
/// 唯一不会毁掉用户文件的选择是不提交——临时件留着，用户可以自己看。
#[tokio::test]
async fn source_shrunk_during_download_is_blocked_before_commit() {
    let fs = Arc::new(FakeFs::default());
    let payload: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    fs.files
        .lock()
        .unwrap()
        .insert("/shrink.bin".into(), payload.clone());
    // 读 #0/#1/#2 正常收全，紧接着 #2 之后源被截成 100 字节。
    *fs.mutate_after_read.lock().unwrap() =
        vec![(2, "/shrink.bin".into(), vec![0x11u8; 100], None)];
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let root = temp_subdir();
    let dest = root.join("shrink.bin");
    // 目标位置上预置用户已有的文件：闸门失效时它会被换掉，这是本用例真正要守的东西。
    let sentinel = b"user's own file, must survive".to_vec();
    std::fs::write(&dest, &sentinel).unwrap();
    let part = part_of(&dest);
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(down_job(dest.clone(), "/shrink.bin", root))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Failed(msg) => {
            assert!(msg.contains("变短"), "应以「源文件变短」终结，实得：{msg}")
        }
        other => panic!("源变短必须阻止提交，实得 {other:?}"),
    }
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        sentinel,
        "闸门在 rename 之前，用户原有的文件必须一个字节都没被碰过"
    );
    assert!(
        part.exists(),
        "被挡下的数据留在临时件里等用户处置，不得顺手删掉：{}",
        part.display()
    );
}

/// 审计2 #15 的另一半：源文件在传输途中**变长**必须放行，且交付的是开工那一刻的快照。
///
/// 下载一个还在被追加的日志是正当用法，把它判失败等于砍掉一整类正常场景。放行的前提是
/// 交付物有确定语义：**恰好** `total` 字节，即开工时量到的那个长度，不多不少。
///
/// 改动点放在读 #1 之后（而不是最后一次读之后），因为那才碰得到 `exec_once` 里
/// `want = total - offset` 那道夹取：末块本该只要 175 712 字节，源此刻已经有 800 000 字节，
/// 不夹取就会读满 262 144 字节、把临时件撑到 786 432。夹取被去掉时本用例必红
///（临时件长度 ≠ total，闸门第二关拦下，终态变 Failed）。
#[tokio::test]
async fn source_grown_during_download_yields_the_opening_snapshot() {
    let fs = Arc::new(FakeFs::default());
    let payload: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    fs.files
        .lock()
        .unwrap()
        .insert("/grow.log".into(), payload.clone());
    let mut grown = payload.clone();
    grown.extend(std::iter::repeat_n(0x22u8, 100_000)); // 追加写：前 700 000 字节原样不动
    *fs.mutate_after_read.lock().unwrap() = vec![(1, "/grow.log".into(), grown, None)];
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let root = temp_subdir();
    let dest = root.join("grow.log");
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(down_job(dest.clone(), "/grow.log", root))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Done => {}
        other => panic!("源变长是正当情形，必须照常完成，实得 {other:?}"),
    }
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        payload,
        "交付的必须**恰好**是开工那一刻的前 700 000 字节：短了是残缺，长了是跟着新数据跑"
    );
}

/// 审计2 #15 的要害一格：长度没变、内容被**就地改写**。
///
/// 长度这一维在这里完全瞎掉——开工 700 000 字节、收工 700 000 字节，②「临时件长度 == total」
/// 那关也照过不误。而临时件里躺着的是 `旧内容[0..2*CHUNK] + 新内容[2*CHUNK..700000]`，
/// 一份货真价实的混合物：能打开、长度对、结构大概率还合法，用户拿到手要到很久以后才发现。
/// 抓住它的唯一廉价证据就是 mtime，故本用例的判别式是 mtime 那一格。
///
/// 传后校验开着时哈希那关也能拦（期望值是开工前算的），但那是**第三层**证据、要多读一遍
/// 整个文件，而且用户可以关掉它。mtime 这一关免费且不可关，必须自己被钉住。
#[tokio::test]
async fn source_rewritten_in_place_during_download_is_blocked_before_commit() {
    let fs = Arc::new(FakeFs::default());
    let payload: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    // 同长度、逐字节不同的另一版内容。
    let rewritten: Vec<u8> = payload.iter().map(|b| b ^ 0xFF).collect();
    fs.files
        .lock()
        .unwrap()
        .insert("/inplace.bin".into(), payload.clone());
    fs.mtimes
        .lock()
        .unwrap()
        .insert("/inplace.bin".into(), 1000);
    // 读 #1 之后换内容并推进 mtime：随后的读 #2 取的就是新内容，临时件成为混合物。
    *fs.mutate_after_read.lock().unwrap() =
        vec![(1, "/inplace.bin".into(), rewritten.clone(), Some(2000))];
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let root = temp_subdir();
    let dest = root.join("inplace.bin");
    let sentinel = b"user's own file, must survive".to_vec();
    std::fs::write(&dest, &sentinel).unwrap();
    let part = part_of(&dest);
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(down_job(dest.clone(), "/inplace.bin", root))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Failed(msg) => assert!(
            msg.contains("就地改写"),
            "应以「源被就地改写」终结，实得：{msg}"
        ),
        other => panic!("长度不变的就地改写必须阻止提交，实得 {other:?}"),
    }
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        sentinel,
        "用户原有的文件必须原封不动"
    );
    // 把「被挡下的确实是一份混合物」也证出来：它既不等于旧版也不等于新版。
    let leftover = std::fs::read(&part).expect("临时件必须保留");
    assert_ne!(
        leftover, payload,
        "临时件不该等于旧版（读 #2 取的是新内容）"
    );
    assert_ne!(
        leftover, rewritten,
        "临时件也不该等于新版（读 #0/#1 取的是旧内容）——它正是那份混合物"
    );
}

/// 审计2 #10：内容哈希不符必须**否决提交**，而不是在替换之后贴一张红标签。
///
/// 原顺序是 `commit` → app 层 `run_verify` → 发 `transfer_verified`：校验跑的时候用户原有的
/// 文件已经没了，`Mismatch` 不阻止任何事，且临时件此刻也已被 rename 消耗，无从回退。
/// 校验若不能否决提交，它就不是校验。
#[tokio::test]
async fn hash_mismatch_blocks_commit_and_keeps_the_original() {
    let fs = Arc::new(FakeFs::default());
    let payload: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    fs.files
        .lock()
        .unwrap()
        .insert("/hash.bin".into(), payload.clone());
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let root = temp_subdir();
    let dest = root.join("hash.bin");
    let sentinel = b"user's own file, must survive".to_vec();
    std::fs::write(&dest, &sentinel).unwrap();
    let part = part_of(&dest);
    let mut ev = mgr.events().await;
    let mut job = down_job(dest.clone(), "/hash.bin", root);
    // 格式合法但对不上的期望值：模拟「传输途中数据损坏」——引擎能观测到的正是这一格。
    job.verify = Some(Sha256Hex::new("a".repeat(64)).expect("64 位小写 hex"));
    let id = mgr.submit(job).await.expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Failed(msg) => assert!(
            msg.contains("内容哈希不符"),
            "应以「内容哈希不符」终结，实得：{msg}"
        ),
        other => panic!("哈希不符必须阻止提交，实得 {other:?}"),
    }
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        sentinel,
        "校验排在 rename 之前，用户原有的文件必须完好"
    );
    assert_eq!(
        std::fs::read(&part).unwrap(),
        payload,
        "被判不符的数据留在临时件里等待覆盖或删除，不得静默删除"
    );
}

/// 审计2 #16：下载临时件（以及伴生身份记录）在 Unix 上必须以 0600 落地。
///
/// `.fspart` 里躺的正是用户刚从远端取回的东西——私钥、数据库、配置。默认 umask 0022 下
/// 它会是 0644，同机任何本地账户都能一路读完；而 `commit` 走 rename，rename 不改 inode 的
/// mode，于是这份权限一路继承到用户最后拿到的**交付文件**上。
///
/// 断言前必须先把 umask 钉成一个**宽松**的已知值：在一台 umask 本就是 0077 的开发机上，
/// 「什么都不做」同样能得出 0600，用例会假绿。umask 是进程级的，会短暂影响并发跑的其他
/// 用例——本二进制里没有第二处断言文件权限，且 0022 比多数机器的缺省更宽松，不会把别处
/// 变严；读完立刻还原。
#[cfg(unix)]
#[tokio::test]
async fn download_part_and_identity_files_are_private_0600() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: umask(2) 无内存安全含义，unsafe 仅因为它是 FFI。
    let prev_umask = unsafe { libc::umask(0o022) };
    let fs = Arc::new(FakeFs::default());
    let payload: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    fs.files
        .lock()
        .unwrap()
        .insert("/secret.pem".into(), payload);
    // 三次尝试的首读全部失败 → Failed，临时件与身份记录都停在磁盘上等着被检查。
    *fs.fail_read_indices.lock().unwrap() = vec![0, 1, 2];
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let root = temp_subdir();
    let dest = root.join("secret.pem");
    let part = part_of(&dest);
    let meta = meta_of(&part);
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(down_job(dest, "/secret.pem", root))
        .await
        .expect("submit 应受理");
    let state = drain_to_terminal(&mut ev, id).await;
    let modes = [&part, &meta].map(|p| {
        std::fs::metadata(p)
            .unwrap_or_else(|e| panic!("{} 应当在场: {e}", p.display()))
            .permissions()
            .mode()
            & 0o777
    });
    // SAFETY: 同上；断言之前还原，免得断言失败时把 umask 留给后续用例。
    unsafe { libc::umask(prev_umask) };
    assert!(
        matches!(state, TransferState::Failed(_)),
        "三次读全失败应以 Failed 收尾，实得 {state:?}"
    );
    assert_eq!(
        modes,
        [0o600, 0o600],
        "临时件与身份记录都必须只有属主可读写（顺序：{}、{}）",
        part.display(),
        meta.display()
    );
}

/// 提交前闸门第二关：临时件长度对不上 `total` 时必须否决提交（审计2 #10）。
///
/// 构造的是「服务端对一发写回了 OK，但那一块并没有真正落盘」——SFTP 的写是流水线式的，
/// 配额、写缓存回刷失败、半断的连接都能造出这一格，而客户端的 `offset` 早已按「返回 Ok」
/// 推进过了。被吞掉的是**收尾那一块**，于是临时件停在 524 288 字节，比应传的 700 000 少一截。
///
/// 这是唯一走到第二关的用例。没有它，把那一关整个删掉，本文件其余用例全绿——一条专门
/// 用来兜住「写入被吞掉」的防线处在无人看守的状态。传后校验开着时第三关也能拦，
/// 但用户可以关掉它；第二关近乎免费且不可关，必须自己被钉住。
#[tokio::test]
async fn swallowed_tail_write_is_caught_by_part_length_before_commit() {
    let fs = Arc::new(FakeFs::default());
    let original = vec![0xBBu8; 4096];
    fs.files
        .lock()
        .unwrap()
        .insert("/tail.bin".into(), original.clone());
    // 写 #0/#1 正常，#2（收尾块）报成功但不落盘。
    *fs.swallow_write_indices.lock().unwrap() = vec![2];
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let payload: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    let local = write_temp(&payload);
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/tail.bin", "ns-tail"))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Failed(msg) => assert!(
            msg.contains("临时件长度"),
            "应以「临时件长度 ≠ 应传」终结，实得：{msg}"
        ),
        other => panic!("收尾写被吞掉必须阻止提交，实得 {other:?}"),
    }
    assert_eq!(
        fs.get("/tail.bin"),
        Some(original),
        "闸门排在 rename 之前，远端原有的文件必须完好"
    );
    assert_eq!(
        fs.get("/tail.bin.fspart").map(|v| v.len()),
        Some(2 * CHUNK),
        "缺了一截的临时件留在原地，不得静默提交，也不得静默删除"
    );
}

/// 提交前闸门第三关（**上传侧**）：内容哈希不符必须否决提交（审计2 #10）。
///
/// 被吞掉的这次换成**中间**那一块。SFTP 的写带偏移，后一块照样写在 524 288 处，服务端
/// 于是把中间那 一个分块 零填出来——临时件长度**恰好** 700 000，第二关一路放行。
/// 这一格只有内容这一维看得见：文件长度对、能打开、中间躺着 一个分块 的 0。
///
/// 上传方向的哈希要一条 exec 通道去远端算；通道缺席时这一关是直接跳过的，所以它此前
/// 从未被执行过——`spawn_with_verifier` 这条构造路径在整个测试套件里也没有第二个调用点。
/// 顺带钉住：闸门失败时远端最终文件仍是原样，损坏的数据留在临时件里。
#[tokio::test]
async fn swallowed_middle_write_is_caught_by_upload_hash_before_commit() {
    let fs = Arc::new(FakeFs::default());
    let original = vec![0xBBu8; 4096];
    fs.files
        .lock()
        .unwrap()
        .insert("/hole.bin".into(), original.clone());
    *fs.swallow_write_indices.lock().unwrap() = vec![1];
    let exec: Arc<dyn ExecChannel> = Arc::new(HashingExec(fs.clone()));
    let mgr = TransferManager::spawn_with_verifier(fs.clone(), 1, Some(exec));
    let payload: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    let local = write_temp(&payload);
    // 期望值取自**本地源文件**，与 app 层传前算好的那一份同源。
    let expect = fs_sshengine::verify::file_sha256(&local)
        .await
        .expect("本地源文件刚写好");
    let mut ev = mgr.events().await;
    let mut job = up_job(local, "/hole.bin", "ns-hole");
    job.verify = Some(expect);
    let id = mgr.submit(job).await.expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Failed(msg) => assert!(
            msg.contains("内容哈希不符"),
            "长度这一维看不见零洞，只有哈希能拦；实得：{msg}"
        ),
        other => panic!("零洞必须阻止提交，实得 {other:?}"),
    }
    assert_eq!(
        fs.get("/hole.bin"),
        Some(original),
        "远端原有的文件必须完好——校验若不能否决提交，它就只是给已经造成的损失贴标签"
    );
    let leftover = fs.get("/hole.bin.fspart").expect("临时件必须保留");
    assert_eq!(
        leftover.len(),
        700_000,
        "长度恰好对得上，这正是第二关拦不住它的原因"
    );
    assert_eq!(
        &leftover[CHUNK..2 * CHUNK],
        &vec![0u8; CHUNK][..],
        "中间那一块应当是服务端零填出来的洞——这就是被拦下的那份静默损坏"
    );
}

/// 上传方向的提交前哈希**相符**时必须照常提交（同一条路径的反面）。
///
/// 没有这一条，把第三关整个改成「一律失败」也能让上面那条用例继续绿——一道只会说「不」的
/// 闸门等于把上传功能关掉，而那种回归恰恰不会被任何「必须拦住」型断言发现。
#[tokio::test]
async fn matching_upload_hash_commits_normally() {
    let fs = Arc::new(FakeFs::default());
    fs.files
        .lock()
        .unwrap()
        .insert("/ok.bin".into(), vec![0xCCu8; 4096]);
    let exec: Arc<dyn ExecChannel> = Arc::new(HashingExec(fs.clone()));
    let mgr = TransferManager::spawn_with_verifier(fs.clone(), 1, Some(exec));
    let payload: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    let local = write_temp(&payload);
    let expect = fs_sshengine::verify::file_sha256(&local)
        .await
        .expect("本地源文件刚写好");
    let mut ev = mgr.events().await;
    let mut job = up_job(local, "/ok.bin", "ns-ok");
    job.verify = Some(expect);
    let id = mgr.submit(job).await.expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Done => {}
        other => panic!("哈希相符的上传必须照常完成，实得 {other:?}"),
    }
    assert_eq!(fs.get("/ok.bin"), Some(payload), "远端最终文件 = 新内容");
    assert_eq!(
        fs.get("/ok.bin.fspart"),
        None,
        "提交后临时件必须已被 rename 走"
    );
    assert_eq!(
        fs.get("/ok.bin.fspart.fsmeta"),
        None,
        "提交成功后身份记录必须清掉，不得在远端目录里留垃圾"
    );
}

/// 另一个写者的 `prepare_part` 会写下的那份记录：同样格式、同样大小，但源不是我们这一个。
///
/// 手写 JSON 而不是构造 `PartIdentity`：那是个私有类型，集成测试够不着。这反而更贴近要防的
/// 东西——落盘格式是跨进程契约，本用例连同 `resume_downloads_from_offset` 一起，
/// 把字段名钉成了不能随手改的东西。
fn foreign_identity(dir: &str) -> Vec<u8> {
    format!(
        r#"{{"v":1,"dir":"{dir}","src":"/somebody/elses/source.bin","size":700000,"mtime":null,"hash":null}}"#
    )
    .into_bytes()
}

/// 审计2 #12：伴生身份记录在传输途中被**另一个写者**改写 → 必须拒绝提交（上传侧）。
///
/// `TARGET_LOCKS` 是 `fs_sshengine` 里的**进程内** static。它拦不住同一台机器上的第二个实例
/// （app 侧的数据目录独占锁把这一种堵死了，见 `fs_vault::lock_data_dir`），更拦不住另一台
/// 机器上的第二个客户端——而 `.fspart` 的路径完全由**目标**决定，于是两个写者会往同一个
/// 临时件里各写各的，各自的 `exec_once` 都会干干净净地报 `Completed`。
///
/// 唯一的公共状态是伴生记录：第二个写者只要开工，`prepare_part` 的「从 0 重来」分支就必然
/// 把它改写成自己那一份（源不同 → 记录不同）。于是「记录变了」就是「有别人正在写同一个
/// 临时件」的确凿证据，这就是提交前闸门第 ① 关的全部判据。
///
/// 改写点落在写 #1 之后（三块里的中间那块）：既在途中，又保证 ①之后的 ②③④ 全部通得过——
/// 若不是 ① 拦下的，这件传输会一路 Done。因此终态本身就区分得出是哪一关说的话，
/// 消息断言只是把它写明白。
#[tokio::test]
async fn upload_refuses_to_commit_when_another_writer_rewrote_the_part_identity() {
    let fs = Arc::new(FakeFs::default());
    let original = vec![0xBBu8; 4096];
    fs.files
        .lock()
        .unwrap()
        .insert("/shared.bin".into(), original.clone());
    *fs.mutate_after_write.lock().unwrap() = vec![(
        1,
        "/shared.bin.fspart.fsmeta".into(),
        Some(foreign_identity("up")),
    )];
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let payload: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    let local = write_temp(&payload);
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/shared.bin", "ns-shared"))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Failed(msg) => assert!(
            msg.contains("伴生身份记录已不再属于本次传输"),
            "应由提交前闸门第 ① 关终结，实得：{msg}"
        ),
        other => panic!("临时件被第二个写者接管后必须拒绝提交，实得 {other:?}"),
    }
    assert_eq!(
        fs.get("/shared.bin"),
        Some(original),
        "闸门排在 rename 之前，远端原有的文件必须一个字节都没被碰过"
    );
    assert_eq!(
        fs.get("/shared.bin.fspart").map(|v| v.len()),
        Some(700_000),
        "来历已经不明的临时件留在原地等人处置，不得静默提交，也不得静默删除"
    );
}

/// 同一关的另一半：伴生记录在传输途中**消失**（另一个写者先提交成功，`cleanup_identity`
/// 顺手把它删了）——同样必须拒绝提交。
///
/// 记录不在了意味着手里这份临时件早已被对方 rename 走，我们此刻写的是一个重新创建出来的
/// 同名文件；提交它就是把两次传输的字节混着交给用户。
///
/// 这一条是「取不到证据 → 降级放行」那条口径在第 ① 关的**反例**，必须单独有一个 witness：
/// ③④ 取不到答案时还有别的层兜着，而 ① 是「这份临时件还是不是我的」这个问题的唯一答案
/// 来源——降级等于永远回答「是」，这一关就不存在了。把判据从 `!= Some(ident)` 放宽成
/// 「有记录且不等」的回归，只有本用例抓得住（上一条用例照样绿）。
#[tokio::test]
async fn upload_refuses_to_commit_when_the_part_identity_vanished() {
    let fs = Arc::new(FakeFs::default());
    let original = vec![0xBBu8; 4096];
    fs.files
        .lock()
        .unwrap()
        .insert("/gone.bin".into(), original.clone());
    *fs.mutate_after_write.lock().unwrap() = vec![(1, "/gone.bin.fspart.fsmeta".into(), None)];
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let payload: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    let local = write_temp(&payload);
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/gone.bin", "ns-gone"))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Failed(msg) => assert!(
            msg.contains("伴生身份记录已不再属于本次传输"),
            "记录消失必须与被改写同等对待，实得：{msg}"
        ),
        other => panic!("拿不出所有权证明就不许提交，实得 {other:?}"),
    }
    assert_eq!(
        fs.get("/gone.bin"),
        Some(original),
        "远端原有的文件必须完好"
    );
    assert_eq!(
        fs.get("/gone.bin.fspart").map(|v| v.len()),
        Some(700_000),
        "临时件留在原地"
    );
}

/// 同一关的下载侧（审计2 #12）。伴生记录这一侧落在**本地**磁盘上，不经 `SftpOps`，
/// 因此走的是 `read_local_identity` 这条与上传侧完全不同的取证路径——两侧各需一个 witness，
/// 只测一侧的话把另一侧的 `read_*_identity` 换成「恒返回本次 ident」也照样全绿。
#[tokio::test]
async fn download_refuses_to_commit_when_another_writer_rewrote_the_part_identity() {
    let fs = Arc::new(FakeFs::default());
    let payload: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    fs.files
        .lock()
        .unwrap()
        .insert("/pull.bin".into(), payload.clone());
    let root = temp_subdir();
    let dest = root.join("pull.bin");
    let part = part_of(&dest);
    // 读 #0 正常收下第一块，紧接着 #1 之后本地伴生记录被另一个写者改写。
    *fs.mutate_local_after_read.lock().unwrap() =
        vec![(1, meta_of(&part), foreign_identity("down"))];
    // 目标位置上预置用户已有的文件：闸门失效时它会被换掉，这是本用例真正要守的东西。
    let sentinel = b"user's own file, must survive".to_vec();
    std::fs::write(&dest, &sentinel).unwrap();
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(down_job(dest.clone(), "/pull.bin", root))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Failed(msg) => assert!(
            msg.contains("伴生身份记录已不再属于本次传输"),
            "应由提交前闸门第 ① 关终结，实得：{msg}"
        ),
        other => panic!("本地临时件被第二个写者接管后必须拒绝提交，实得 {other:?}"),
    }
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        sentinel,
        "闸门在 rename 之前，用户原有的文件必须一个字节都没被碰过"
    );
    assert!(
        part.exists(),
        "来历已经不明的数据留在临时件里等用户处置：{}",
        part.display()
    );
}

/// 审计2 #8：提交失败时，用户原有的文件必须被**原样挪回**。
///
/// 原实现在 rename 首次失败后发 `remove(remote)` ——一个不可逆动作，建立在「首次失败的原因
/// 是目标已存在」这个在 SFTP v3 协议层面根本判不出来的前提上（没有 FILE_ALREADY_EXISTS，
/// OpenSSH 一律回通用的 SSH_FX_FAILURE）。真实语义于是变成：只要 rename 失败，不管什么原因，
/// 先删掉用户的文件再赌一把。配额超限、权限被撤、只读文件系统——每一种都是「删了也换不回来」。
///
/// 修法不是「把原因判准」，而是让正确性不依赖原因：让位动作改成**可回退的改名**。
/// 本用例注入两发 rename 失败，走的正是「让位 → 重试仍失败 → 从 .fsbak 挪回」这条路。
#[tokio::test]
async fn failed_commit_restores_the_original_file_from_backup() {
    let fs = Arc::new(FakeFs::default());
    let original = vec![0xEEu8; 4096];
    fs.files
        .lock()
        .unwrap()
        .insert("/roll.bin".into(), original.clone());
    // 两发：① 首次 rename(part → remote)；② 让位之后的重试。第三发（从 .fsbak 挪回）放行。
    fs.fail_rename_to
        .lock()
        .unwrap()
        .insert("/roll.bin".into(), 2);
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let fresh = b"brand new contents".to_vec();
    let local = write_temp(&fresh);
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/roll.bin", "ns-roll"))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Failed(msg) => assert!(
            msg.contains("原样挪回"),
            "应当明说原文件已复原，实得：{msg}"
        ),
        other => panic!("两发 rename 失败应以 Failed 收尾，实得 {other:?}"),
    }
    assert_eq!(
        fs.get("/roll.bin"),
        Some(original),
        "提交失败后用户原有的文件必须逐字节复原——删掉它是不可回滚的数据丢失"
    );
    assert_eq!(
        fs.get("/roll.bin.fspart"),
        Some(fresh),
        "新数据完整留在临时件里，用户重来一次即可，不必重传"
    );
    assert_eq!(
        fs.get("/roll.bin.fsbak"),
        None,
        "复原成功后备份必须已被消耗，不得在远端目录里留垃圾"
    );
}

/// 审计2 #8 的最坏分支：连「挪回去」这一步都失败了。
///
/// 此时不存在任何一种能把两份数据都保住又让路径归位的做法，唯一正确的处置是**两份都不动**
/// 并且把它们各自在哪讲清楚。这条分支的价值全在那句话上：用户看到的必须是一份可执行的
/// 手工恢复说明，而不是一句「提交失败」。故本用例把两个落点和「切勿删除」一起钉住。
#[tokio::test]
async fn unrecoverable_commit_failure_keeps_both_copies_and_says_where() {
    let fs = Arc::new(FakeFs::default());
    let original = vec![0xDDu8; 4096];
    fs.files
        .lock()
        .unwrap()
        .insert("/stuck.bin".into(), original.clone());
    // 三发：①首次 rename、②让位后重试、③从 .fsbak 挪回。（让位那一发的 to 是 .fsbak，不受影响。）
    fs.fail_rename_to
        .lock()
        .unwrap()
        .insert("/stuck.bin".into(), 3);
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let fresh = b"brand new contents".to_vec();
    let local = write_temp(&fresh);
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/stuck.bin", "ns-stuck"))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Failed(msg) => {
            for needle in ["/stuck.bin.fsbak", "/stuck.bin.fspart", "切勿删除"] {
                assert!(
                    msg.contains(needle),
                    "未能复原时必须讲清两份数据各在哪、且不要删；缺「{needle}」：{msg}"
                );
            }
        }
        other => panic!("三发 rename 失败应以 Failed 收尾，实得 {other:?}"),
    }
    assert_eq!(
        fs.get("/stuck.bin.fsbak"),
        Some(original),
        "原文件必须完好地停在备份路径上"
    );
    assert_eq!(
        fs.get("/stuck.bin.fspart"),
        Some(fresh),
        "新数据必须完好地停在临时件上"
    );
    assert_eq!(
        fs.get("/stuck.bin"),
        None,
        "最终路径此刻是空的——正因如此，那句话里的两个落点是用户唯一的线索"
    );
}

/// 审计 P0-2（下载侧）：目标原本比源文件**长**时，下载后必须恰好等于源长度。
///
/// 原实现用 `truncate(false)` 直接打开 `<dest>` 覆写，远端 100 字节盖住本地 1000 字节的
/// 前 100 字节，后面 900 字节旧内容原样留着——文件能打开、能解析，只是尾部挂着上一版的
/// 残骸，用户往往到运行时才发现。断言长度**和**内容：只比长度会漏掉内容错位。
#[tokio::test]
async fn download_over_longer_local_file_is_exactly_source_length() {
    let fs = Arc::new(FakeFs::default());
    let remote_payload: Vec<u8> = (0..100u32).map(|i| (i % 251) as u8).collect();
    fs.files
        .lock()
        .unwrap()
        .insert("/small.bin".into(), remote_payload.clone());
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let root = temp_subdir();
    let dest = root.join("small.bin");
    std::fs::write(&dest, vec![0xEEu8; 1000]).unwrap(); // 本地旧文件远长于远端
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(down_job(dest.clone(), "/small.bin", root))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Done => {}
        other => panic!("覆写下载不应以 {other:?} 收尾"),
    }
    let got = std::fs::read(&dest).unwrap();
    assert_eq!(got.len(), 100, "下载后长度必须恰好等于远端源，不得留旧尾巴");
    assert_eq!(got, remote_payload, "下载后内容必须逐字节等于远端源");
    assert!(!part_of(&dest).exists(), "提交后临时件不得残留");
}

/// 审计 P0-2（上传侧）：传输全程失败后，远端**最终文件**必须原封不动。
///
/// 原实现在传输开始前就 `truncate(remote, 0)` 把用户的远端文件清零，然后才开始传——
/// 网线一拔、笔记本一合盖，用户的原文件就没了，且本地那份还在、远端那份是 0 字节，
/// 恢复全靠用户自己发现。临时件方案下最终文件在 `commit` 之前一个字节都不会被碰。
/// 三次尝试各失败一次（write_at 索引 0/1/2），因此走的是「彻底失败」路径。
#[tokio::test]
async fn failed_upload_leaves_remote_final_file_intact() {
    let fs = Arc::new(FakeFs::default());
    let original = vec![0xAAu8; 4096];
    fs.files
        .lock()
        .unwrap()
        .insert("/keep.bin".into(), original.clone());
    *fs.fail_write_indices.lock().unwrap() = vec![0, 1, 2]; // 三次尝试的首块全部失败
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let local = write_temp(b"brand-new-and-much-shorter");
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/keep.bin", "ns-keep"))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Failed(_) => {}
        other => panic!("三次尝试全失败应以 Failed 收尾，实得 {other:?}"),
    }
    assert_eq!(
        fs.get("/keep.bin"),
        Some(original),
        "上传失败绝不允许动到远端最终文件——既不得截断，也不得写入半截新内容"
    );
}

/// 空源文件（0 字节）上传必须正常完成，且不得毁掉远端同名旧文件。
///
/// 回归的是临时件方案（P0-2）自带的一个静默毁数据路径：`prepare_part` 原先只在临时件
/// **已存在**时动手，源文件为空时 `exec_once` 一发 `write_at` 都不会送出，
/// `<remote>.fspart` 于是从头到尾不存在；`commit` 的 rename 必然失败，接着走进
/// 「删旧目标再试一次」分支，把用户远端已有的同名文件删掉再报 Failed——
/// 用户传一个空文件，代价是原文件没了。
#[tokio::test]
async fn upload_empty_file_succeeds_and_replaces_remote() {
    let fs = Arc::new(FakeFs::default());
    fs.files
        .lock()
        .unwrap()
        .insert("/empty.bin".into(), vec![0xCDu8; 8192]);
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let local = write_temp(b"");
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/empty.bin", "ns-empty"))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Done => {}
        other => panic!("空文件上传应当成功，实得 {other:?}"),
    }
    assert_eq!(
        fs.get("/empty.bin"),
        Some(Vec::new()),
        "远端最终文件必须被替换成 0 字节的新内容"
    );
    assert_eq!(
        fs.get("/empty.bin.fspart"),
        None,
        "提交后临时件必须已被 rename 走"
    );
}

/// 临时件缺席时，`commit` 绝不能为了「腾地方重试 rename」去删最终目标。
///
/// 删目标是不可逆动作，而 rename 失败的原因未必是「目标已存在」——临时件被别的进程清掉、
/// 权限被撤、路径消失都会走到同一个分支。此时删了目标也换不回来，只是白白毁掉用户的原文件。
/// 用「零长度写不物化文件」的服务端来制造临时件缺席：SFTP 并不强保证 0 字节写会建出文件，
/// 这是真实存在的可移植性风险，而不是纯人造场景。
#[tokio::test]
async fn commit_never_deletes_target_when_part_is_absent() {
    let fs = Arc::new(FakeFs::default());
    let original = vec![0xEFu8; 4096];
    fs.files
        .lock()
        .unwrap()
        .insert("/guard.bin".into(), original.clone());
    *fs.swallow_empty_create.lock().unwrap() = true;
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let local = write_temp(b"");
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/guard.bin", "ns-guard"))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Failed(_) => {}
        other => panic!("临时件缺席时提交应失败，实得 {other:?}"),
    }
    assert_eq!(
        fs.get("/guard.bin"),
        Some(original),
        "提交失败绝不允许把最终目标删掉——那是一次无法回滚的数据丢失"
    );
}

#[tokio::test]
async fn download_outside_sandbox_is_rejected() {
    let fs = Arc::new(FakeFs::default());
    fs.files
        .lock()
        .unwrap()
        .insert("/x.bin".into(), b"payload".to_vec());
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let root = temp_subdir();
    let outside = std::env::temp_dir().join(format!("fs-escape-{}.bin", std::process::id()));
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(down_job(outside.clone(), "/x.bin", root))
        .await
        .expect("submit 应受理");
    loop {
        let e = next_event(&mut ev).await;
        if e.id != id {
            continue;
        }
        match e.state {
            TransferState::Failed(msg) => {
                assert!(msg.contains("sandbox"), "失败原因应指明沙箱越界: {msg}");
                assert!(
                    !outside.exists(),
                    "越界目的文件不得被创建: {}",
                    outside.display()
                );
                assert!(
                    !part_of(&outside).exists(),
                    "越界目的的临时件同样不得被创建"
                );
                return;
            }
            TransferState::Done => panic!("越出沙箱的下载不得成功"),
            // 沙箱拒绝是策略性永久失败：重试三次结论恒定，只会白等 1+2 s 退避
            // 并向 UI 推两条「正在重试」的假象（S47）。
            TransferState::Retrying { attempt } => {
                panic!("沙箱越界不得重试（收到 attempt={attempt}）")
            }
            TransferState::Running | TransferState::Cancelled => {}
        }
    }
}

/// 未设 sandbox_root 的下载必须 fail-closed（spec §3.3）——缺省不等于放行。
#[tokio::test]
async fn download_without_sandbox_root_is_rejected() {
    let fs = Arc::new(FakeFs::default());
    fs.files
        .lock()
        .unwrap()
        .insert("/y.bin".into(), b"payload".to_vec());
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let dest = temp_path();
    let mut ev = mgr.events().await;
    let mut job = down_job(dest.clone(), "/y.bin", std::env::temp_dir());
    job.sandbox_root = None;
    let id = mgr.submit(job).await.expect("submit 应受理");
    loop {
        let e = next_event(&mut ev).await;
        if e.id != id {
            continue;
        }
        match e.state {
            TransferState::Failed(msg) => {
                assert!(
                    msg.contains("sandbox_root not set"),
                    "缺省沙箱根须显式拒绝并点名成因: {msg}"
                );
                assert!(!dest.exists(), "被拒的下载不得留下半截文件");
                assert!(!part_of(&dest).exists(), "被拒的下载不得留下临时件");
                return;
            }
            TransferState::Done => panic!("未设沙箱根的下载不得成功"),
            // 同 download_outside_sandbox_is_rejected：策略性永久失败不得进重试路径（S47）。
            TransferState::Retrying { attempt } => {
                panic!("缺省沙箱根不得重试（收到 attempt={attempt}）")
            }
            TransferState::Running | TransferState::Cancelled => {}
        }
    }
}

/// 收干一件的终态并返回失败原因，同时钉住「策略性永久失败不得进重试路径」（S47）。
/// 沙箱类拒绝重试三次结论恒定，只会白等 1+2 s 并向 UI 推两条「正在重试」的假象。
async fn expect_failure_without_retry(
    ev: &mut mpsc::Receiver<TransferEvent>,
    id: fs_sshengine::transfer::TransferId,
) -> String {
    loop {
        let e = next_event(ev).await;
        if e.id != id {
            continue;
        }
        match e.state {
            TransferState::Failed(msg) => return msg,
            TransferState::Retrying { attempt } => {
                panic!("策略性拒绝不得重试（收到 attempt={attempt}）")
            }
            TransferState::Done | TransferState::Cancelled => {
                panic!("本用例要求以 Failed 收尾，实得 {:?}", e.state)
            }
            TransferState::Running => {}
        }
    }
}

// ── 审计 P2：末段软链防护必须落在真正接字节的那个路径上 ────────────────────────────
//
// 下面这几条刻意跑**整件下载**，而不是直接调 `resolve_within`。理由是：单测能证明
// 「守卫会拒绝一条软链」，却永远证明不了「守卫被安在了该守的路径上」。`tests/sandbox.rs`
// 里的 `final_component_symlink_rejected` 正是这样一条真断言 + 假安全感——它守的 `<dest>`
// 在整个下载期间一个字节都收不到（铁律 ② 让字节全落在 `<dest>.fspart` 上，`<dest>` 只在
// commit 里被 rename 触碰一次，而 rename 不跟随软链）。这个错位只有把一件真实下载
// 跑穿沙箱才暴露得出来。

/// 在 `link` 处种一条指向沙箱外的软链，返回被指向的那个沙箱外位置。
///
/// unix 用**文件软链**指向一个已存在的沙箱外文件——修复前引擎会跟随它就地改写受害文件，
/// 这条路径能把越权写演示到字节级。
///
/// Windows 建不出文件软链（需 SeCreateSymbolicLinkPrivilege 或开发者模式，本机实测
/// ERROR_PRIVILEGE_NOT_HELD），只能用普通账户就能建的**目录联接**；好在
/// `FileType::is_symlink()` 对 `IO_REPARSE_TAG_MOUNT_POINT` 同样返回 true，走的是同一条
/// 判定分支。代价是证明力较弱，必须如实说明：临时件路径上挂的是个目录，修复前那句 open
/// 本来就会因为「打不开目录」而失败，于是「沙箱外没被写」在修复前后都成立，**它不是判别式**。
/// 真正的判别式是**拒绝理由**——修复后是本引擎的软链判定（含 `symlink`），修复前是一句
/// 操作系统的「拒绝访问」。即：Windows 侧钉的是「防护确实跑在临时件路径上」，
/// 逃逸本身由 unix 侧证明。
fn plant_symlink_outside_sandbox(link: &std::path::Path) -> std::path::PathBuf {
    // 临时件可能已被 prepare_part 建出来占位，先挪开（remove_file 不跟随软链）
    let _ = std::fs::remove_file(link);
    #[cfg(unix)]
    {
        let outside = temp_subdir().join("victim.conf");
        std::fs::write(&outside, b"original").unwrap();
        std::os::unix::fs::symlink(&outside, link).unwrap();
        outside
    }
    #[cfg(windows)]
    {
        let outside = temp_subdir();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(&outside)
            .status()
            .expect("mklink 应可执行");
        // 建不出联接即判红而非跳过：静默跳过正是本项目一路在猎杀的假绿。
        assert!(
            status.success(),
            "目录联接创建失败（TEMP 是否在 NTFS 上？）"
        );
        outside
    }
}

/// 与 `plant_symlink_outside_sandbox` 配套：断言被指向的沙箱外位置一个字节都没变。
fn assert_outside_untouched(outside: &std::path::Path) {
    #[cfg(unix)]
    assert_eq!(
        std::fs::read(outside).unwrap(),
        b"original",
        "沙箱外文件一个字节都不得被改写：{}",
        outside.display()
    );
    #[cfg(windows)]
    assert_eq!(
        std::fs::read_dir(outside).unwrap().count(),
        0,
        "沙箱外目录不得被写入任何东西：{}",
        outside.display()
    );
}

/// 新建下载：临时件路径上预先躺着一条指向沙箱外的软链。
/// 修复前 `create(true).truncate(true).open()` 跟随它，把服务端送来的字节写到沙箱外。
/// **远端服务器控制下载文件名，也就控制了临时件名**——它挑一个用户沙箱里已有的名字即可。
#[tokio::test]
async fn download_never_follows_a_symlink_planted_at_the_part_path() {
    let fs = Arc::new(FakeFs::default());
    fs.files
        .lock()
        .unwrap()
        .insert("/report.pdf".into(), vec![7u8; 4096]);
    let mgr = TransferManager::spawn(fs.clone(), 1);

    let root = temp_subdir();
    let dest = root.join("report.pdf"); // 服务端可控的下载文件名
    let outside = plant_symlink_outside_sandbox(&part_of(&dest));

    let mut ev = mgr.events().await;
    let id = mgr
        .submit(down_job(dest.clone(), "/report.pdf", root))
        .await
        .expect("submit 应受理");
    let msg = expect_failure_without_retry(&mut ev, id).await;
    assert!(msg.contains("symlink"), "拒绝理由须点名软链: {msg}");
    assert_outside_untouched(&outside);
    assert!(!dest.exists(), "被拒的下载不得留下最终文件");
    assert!(
        std::fs::symlink_metadata(part_of(&dest))
            .unwrap()
            .file_type()
            .is_symlink(),
        "软链本身应原样留着——引擎既不跟随它，也不该越权替用户处置沙箱里的既有条目"
    );
}

/// 续传是**另一条**绕过，得单独钉：软链指向一个已存在的文件时，`metadata` 跟随后交回的是
/// **软链目标的长度**，`resume` 于是拿别人的文件长度当断点，从那个偏移往沙箱外的文件里
/// 接着追加远端字节——受害文件被就地改写而非覆盖，前半截原样留着，比整份被覆盖更难察觉。
///
/// 沙箱外文件刻意比远端源文件**短**：否则 `stale` 判定（临时件比源文件长）会把它引去
/// 清零分支，测到的就不是续传这条路了。
#[tokio::test]
async fn resumed_download_never_follows_a_symlink_planted_at_the_part_path() {
    let fs = Arc::new(FakeFs::default());
    fs.files
        .lock()
        .unwrap()
        .insert("/report.pdf".into(), vec![7u8; 4096]);
    let mgr = TransferManager::spawn(fs.clone(), 1);

    let root = temp_subdir();
    let dest = root.join("report.pdf");
    let outside = plant_symlink_outside_sandbox(&part_of(&dest));

    let mut ev = mgr.events().await;
    let mut job = down_job(dest.clone(), "/report.pdf", root);
    job.resume = true;
    let id = mgr.submit(job).await.expect("submit 应受理");
    let msg = expect_failure_without_retry(&mut ev, id).await;
    assert!(msg.contains("symlink"), "拒绝理由须点名软链: {msg}");
    assert_outside_untouched(&outside);
    assert!(!dest.exists(), "被拒的下载不得留下最终文件");
}

/// **悬空**软链——修复前最恶劣的一条：`metadata` 跟随后报 NotFound，于是 `existing = None`、
/// 不进续传分支，直接 `create(true)` 顺着软链在沙箱外**凭空造出**目标文件并灌入远端字节。
/// 另两条至少要求沙箱外已有一个受害文件，这条连这个前提都不需要。
///
/// 仅 unix：目录联接必须指向一个已存在的目录，造不出悬空态。
#[tokio::test]
#[cfg(unix)]
async fn download_never_creates_the_target_of_a_dangling_symlink_at_the_part_path() {
    let fs = Arc::new(FakeFs::default());
    fs.files
        .lock()
        .unwrap()
        .insert("/report.pdf".into(), vec![7u8; 4096]);
    let mgr = TransferManager::spawn(fs.clone(), 1);

    let root = temp_subdir();
    let dest = root.join("report.pdf");
    let outside = temp_subdir().join("planted.conf"); // 沙箱外，尚不存在
    std::os::unix::fs::symlink(&outside, part_of(&dest)).unwrap();

    let mut ev = mgr.events().await;
    let id = mgr
        .submit(down_job(dest.clone(), "/report.pdf", root))
        .await
        .expect("submit 应受理");
    let msg = expect_failure_without_retry(&mut ev, id).await;
    assert!(msg.contains("symlink"), "拒绝理由须点名软链: {msg}");
    assert!(
        !outside.exists(),
        "悬空软链的目标不得被凭空创建：{}",
        outside.display()
    );
    assert!(!dest.exists(), "被拒的下载不得留下最终文件");
}

/// 防护还必须在**每次重入**时重查，而不只是开工时查一遍：重试之间隔着 1 s / 2 s 真实退避，
/// 那正是「趁虚而入把临时件换成软链」的窗口。本条是 `exec_once` 里那道检查的唯一证明——
/// 前三条都在 `prepare_part` 就被拦下，永远走不到重入路径。
///
/// 构造：注入一次读失败让首轮失败 → 收到 `Retrying{attempt:1}` 的当口把临时件换成软链 →
/// 其后两轮重入都应被拒，终态理由点名软链。
#[tokio::test]
async fn download_never_follows_a_symlink_planted_between_retries() {
    let fs = Arc::new(FakeFs::default());
    fs.files
        .lock()
        .unwrap()
        .insert("/report.pdf".into(), vec![7u8; 4096]);
    *fs.fail_next_reads.lock().unwrap() = 1; // 首轮必失败，制造出退避窗口
    let mgr = TransferManager::spawn(fs.clone(), 1);

    let root = temp_subdir();
    let dest = root.join("report.pdf");
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(down_job(dest.clone(), "/report.pdf", root))
        .await
        .expect("submit 应受理");

    let mut outside = None;
    let msg = loop {
        let e = next_event(&mut ev).await;
        if e.id != id {
            continue;
        }
        match e.state {
            // 首轮失败的退避窗口（send 在 sleep 之前，故此刻 worker 必在睡）——种雷
            TransferState::Retrying { attempt: 1 } => {
                assert!(
                    part_of(&dest).exists(),
                    "首轮之前 prepare_part 应已把临时件建出来（否则这条用例种的不是同一个雷）"
                );
                outside = Some(plant_symlink_outside_sandbox(&part_of(&dest)));
            }
            TransferState::Retrying { .. } | TransferState::Running => {}
            TransferState::Failed(m) => break m,
            other => panic!("临时件被换成软链后必须失败，实得 {other:?}"),
        }
    };
    assert!(
        msg.contains("symlink"),
        "重入时的拒绝理由同样须点名软链（否则说明重入路径压根没查）: {msg}"
    );
    assert_outside_untouched(&outside.expect("应当收到过 Retrying{attempt:1}"));
    assert!(!dest.exists(), "被拒的下载不得留下最终文件");
}

/// 用户单件取消：置标志后传输须以 Cancelled 终态收尾，且**不得**再补一条 Done。
/// 竞态被两道保险压住：① submit 后立刻 cancel（current_thread 运行时上 spawn 的搬运任务
/// 尚未被调度，取消标志在首个分块之前就已置位）；② 每次 write_at 人为耗时 30 ms，
/// 即便在多线程运行时上抢先起跑，取消也稳赢 4 个分块中的任何一个边界。
#[tokio::test]
async fn cancel_stops_transfer_with_cancelled_state() {
    let fs = Arc::new(FakeFs::default());
    *fs.write_delay_ms.lock().unwrap() = 30;
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let local = write_temp(&vec![7u8; 1024 * 1024]); // 1 MiB = 4 个分块
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/c.bin", "ns-cancel"))
        .await
        .expect("submit 应受理");
    mgr.cancel(id).await;
    // 唯一正常出口 = Cancelled 终态；缺失则 next_event 在 30 s 内判红而非挂死（S40）。
    // 同样不留 cancelled_seen 布尔位：loop 结构下它恒为 true，断言恒真等于没断言。
    loop {
        let e = next_event(&mut ev).await;
        if e.id != id {
            continue;
        }
        match e.state {
            TransferState::Cancelled => break,
            TransferState::Done => panic!("取消后不得再报 Done"),
            TransferState::Failed(m) => panic!("取消不应表现为失败: {m}"),
            TransferState::Running | TransferState::Retrying { .. } => {}
        }
    }
    assert!(
        fs.get("/c.bin.fspart").map(|v| v.len()).unwrap_or(0) < 1024 * 1024,
        "取消必须真的中断分块循环——临时件不应收到完整 1 MiB"
    );
    assert_eq!(
        fs.get("/c.bin"),
        None,
        "取消的传输不得提交：最终目标必须完全没被创建过"
    );
}

/// 覆写上传后远端只能有新内容（原 S43，现由临时件 + rename 保证）。
///
/// `write_at` 用 WRITE|CREATE 且刻意不带 TRUNCATE（带了会毁断点续传），因此把一个更长的
/// 旧文件覆写成短文件时，若直接往最终路径写就只会盖住前缀、旧尾巴原样留下。S43 当时的解法
/// 是「传前先 truncate 最终文件」，但那反过来制造了「传一半断线即毁原文件」的窗口（审计 P0-2）；
/// 现在改为写 `<remote>.fspart` 再 rename 覆盖，两个问题一并根治。本用例同时覆盖 `commit`
/// 的「rename 撞上已存在目标 → remove 后重试一次」分支（FakeFs 按 SFTP v3 语义拒绝覆盖）。
/// 断言逐字节相等而非只比长度：长度相等但内容错位同样是损坏。
#[tokio::test]
async fn upload_over_larger_remote_file_leaves_only_new_content() {
    let fs = Arc::new(FakeFs::default());
    fs.files
        .lock()
        .unwrap()
        .insert("/ovr.bin".into(), vec![0xAAu8; 300_000]); // 远端旧文件远长于新文件
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let new_content = b"short-file".to_vec();
    let local = write_temp(&new_content);
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/ovr.bin", "ns-ovr"))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Done => {}
        other => panic!("覆写上传不应以 {other:?} 收尾"),
    }
    assert_eq!(
        fs.get("/ovr.bin"),
        Some(new_content),
        "覆写上传后远端必须**只有**新内容——旧文件尾部不得残留"
    );
    assert_eq!(fs.get("/ovr.bin.fspart"), None, "提交后临时件不得残留");
}

/// 审计 P0-3：两个任务写同一远端目标时，后到者必须以「目标忙」快速失败，不得交错写坏文件。
///
/// 断言写成「恰好一个 Done + 一个含『目标忙』的 Failed」而不指定哪个 id 赢：谁先抢到锁取决于
/// 调度，钉死顺序等于给自己埋一个随机变红的用例。人为写延时 150 ms × 4 块 ≈ 600 ms，
/// 保证赢家在输家起跑时仍持有锁。
#[tokio::test]
async fn concurrent_writes_to_same_target_are_rejected_as_busy() {
    let fs = Arc::new(FakeFs::default());
    *fs.write_delay_ms.lock().unwrap() = 150;
    let mgr = TransferManager::spawn(fs.clone(), 2);
    let mut ev = mgr.events().await;
    let a = mgr
        .submit(up_job(
            write_temp(&vec![1u8; 1024 * 1024]),
            "/busy.bin",
            "ns-busy",
        ))
        .await
        .expect("submit 应受理");
    let b = mgr
        .submit(up_job(
            write_temp(&vec![2u8; 1024 * 1024]),
            "/busy.bin",
            "ns-busy",
        ))
        .await
        .expect("submit 应受理");
    let mut terminals = drain_terminals(&mut ev, &[a, b]).await;
    let mut done = 0;
    let mut busy = 0;
    for id in [a, b] {
        match terminals.remove(&id).expect("两件都须有终态") {
            TransferState::Done => done += 1,
            TransferState::Failed(msg) => {
                assert!(
                    msg.contains("目标忙"),
                    "同目标冲突的失败原因必须点名『目标忙』，实得: {msg}"
                );
                assert!(
                    msg.contains("/busy.bin"),
                    "失败原因须点名是哪个目标在忙: {msg}"
                );
                busy += 1;
            }
            other => panic!("同目标并发的终态只应是 Done 或 Failed，实得 {other:?}"),
        }
    }
    assert_eq!((done, busy), (1, 1), "同目标两件必须恰好一成一拒");
}

/// 终态计数：给定一组 id，统计 (Done 数, 「目标忙」失败数)，其余终态一律 panic。
/// 谁赢取决于调度，钉死哪个 id 赢等于给自己埋一个随机变红的用例。
async fn tally_done_and_busy(
    ev: &mut mpsc::Receiver<TransferEvent>,
    ids: &[u64],
    expect_label: &str,
) -> (usize, usize) {
    let mut terminals = drain_terminals(ev, ids).await;
    let (mut done, mut busy) = (0, 0);
    for id in ids {
        match terminals.remove(id).expect("每件都须有终态") {
            TransferState::Done => done += 1,
            TransferState::Failed(msg) => {
                assert!(
                    msg.contains("目标忙"),
                    "同目标冲突的失败原因必须点名『目标忙』，实得: {msg}"
                );
                assert!(
                    msg.contains(expect_label),
                    "失败原因须点名是哪个目标在忙（且用用户键入的原样路径）: {msg}"
                );
                busy += 1;
            }
            other => panic!("同目标并发的终态只应是 Done 或 Failed，实得 {other:?}"),
        }
    }
    (done, busy)
}

/// 审计 P1：目标锁必须跨**连接**生效——同一台服务器的两条连接写同一路径，只许一件成。
///
/// 这里刻意用两个 `TransferManager`（每个管理器对应一条连接/一个代次）配同一个远端文件系统，
/// 因为现实里的触发场景就是这样：
/// ① 同一台服务器开两个标签页（Xshell/Xftp 用户的日常：一个跑命令、一个专管传文件）；
/// ② 或者同一个标签页重连了一次——`shutdown_session_subsystems` 只等一个**有界**宽限期，
///    超时就会打印「仍有任务未到达终态；临时件保留」然后放行，此刻旧代次的 worker 还在写。
/// 旧实现把 `session_id`/`generation` 编进锁键，这两种情形都会得到两把不同的锁：
/// 两个 worker 交错写同一个 `.fspart`，各自 rename 一次，**两件都报 Done**，
/// 用户拿到的是两份内容按块拼起来的文件，而 UI 上一切正常。
#[tokio::test]
async fn two_connections_to_one_server_cannot_write_the_same_target() {
    let fs = Arc::new(FakeFs::default());
    *fs.write_delay_ms.lock().unwrap() = 150;
    // 两条连接 = 两个管理器；同一台服务器 = 同一个 endpoint 串（app 层由 host:port 产出）
    let conn_a = TransferManager::spawn(fs.clone(), 1);
    let conn_b = TransferManager::spawn(fs.clone(), 1);
    let mut ev_a = conn_a.events().await;
    let mut ev_b = conn_b.events().await;
    const ENDPOINT: &str = "twoconn.example.com:22";
    let a = conn_a
        .submit(up_job(
            write_temp(&vec![1u8; 1024 * 1024]),
            "/twoconn.bin",
            ENDPOINT,
        ))
        .await
        .expect("submit 应受理");
    let b = conn_b
        .submit(up_job(
            write_temp(&vec![2u8; 1024 * 1024]),
            "/twoconn.bin",
            ENDPOINT,
        ))
        .await
        .expect("submit 应受理");
    // 两个管理器各有各的事件通道，得分别收
    let (done_a, busy_a) = tally_done_and_busy(&mut ev_a, &[a], "/twoconn.bin").await;
    let (done_b, busy_b) = tally_done_and_busy(&mut ev_b, &[b], "/twoconn.bin").await;
    assert_eq!(
        (done_a + done_b, busy_a + busy_b),
        (1, 1),
        "同一台服务器的两条连接写同一路径，必须恰好一成一拒——两件都成即意味着它们\
         交错写了同一个 .fspart，用户拿到的是两份内容拼起来的文件"
    );
    // 落地内容必须是某一份的**完整**内容，不能是两份交错的产物
    let landed = fs.get("/twoconn.bin").expect("赢家必须已提交最终目标");
    assert!(
        landed == vec![1u8; 1024 * 1024] || landed == vec![2u8; 1024 * 1024],
        "最终文件必须原样等于其中一件的内容；出现第三种内容即为交错写坏"
    );
    assert_eq!(fs.get("/twoconn.bin.fspart"), None, "提交后临时件不得残留");
}

/// 审计 P1（同一缺陷的路径维）：同一个文件的不同写法必须落到同一把锁上。
///
/// 端点修对了只解决「哪台主机」。远端路径由前端「当前目录 + 条目名」拼出，`cwd = "/"` 时
/// 拼出的就是 `//tmp/a.bin`；用户从另一处发起同一个文件的传输拿到的是 `/tmp/a.bin`。
/// 键若按原样字符串比，这就是两把锁——照样交错写同一个 `.fspart`、照样两件都报 Done。
#[tokio::test]
async fn different_spellings_of_one_remote_path_share_one_lock() {
    let fs = Arc::new(FakeFs::default());
    *fs.write_delay_ms.lock().unwrap() = 150;
    let mgr = TransferManager::spawn(fs.clone(), 2);
    let mut ev = mgr.events().await;
    const ENDPOINT: &str = "spelling.example.com:22";
    let a = mgr
        .submit(up_job(
            write_temp(&vec![1u8; 1024 * 1024]),
            "/spell/a.bin",
            ENDPOINT,
        ))
        .await
        .expect("submit 应受理");
    // 同一个文件的另一种写法：重复斜杠 + `.` + 一段 `x/..`
    let b = mgr
        .submit(up_job(
            write_temp(&vec![2u8; 1024 * 1024]),
            "//spell/./x/../a.bin",
            ENDPOINT,
        ))
        .await
        .expect("submit 应受理");
    let (done, busy) = tally_done_and_busy(&mut ev, &[a, b], "a.bin").await;
    assert_eq!(
        (done, busy),
        (1, 1),
        "`/spell/a.bin` 与 `//spell/./x/../a.bin` 是同一个远端文件，必须恰好一成一拒"
    );
}

/// 审计2 #13（路径别名·软链目录）：`/link/x.bin` 与 `/real/x.bin` 是同一个文件。
///
/// 纯词法归一到此为止——两串字符没有任何共同点，只有服务端知道 `/link` 就是 `/real`。
/// 于是引擎向服务端要一次 REALPATH，**只问父目录**（叶子可能还不存在，见
/// `SftpOps::canonicalize`），把答案作为**另一维**锁键挂上去。
///
/// 关键在「另一维」而不是「换一维」：若用规范名取代词法名，一件解析成功、一件解析失败的
/// 两件传输就会各锁各的，比现在还糟。
#[tokio::test]
async fn a_symlinked_remote_directory_and_its_real_path_share_one_lock() {
    let fs = Arc::new(FakeFs::default());
    *fs.write_delay_ms.lock().unwrap() = 150;
    // 服务端视角：/link 是指向 /real 的软链；/real 解析回自己（真实 REALPATH 就是这样）
    fs.set_canon("/link", Some("/real"));
    fs.set_canon("/real", Some("/real"));
    let mgr = TransferManager::spawn(fs.clone(), 2);
    let mut ev = mgr.events().await;
    const ENDPOINT: &str = "symdir.example.com:22";
    let a = mgr
        .submit(up_job(
            write_temp(&vec![1u8; 1024 * 1024]),
            "/link/sym.bin",
            ENDPOINT,
        ))
        .await
        .expect("submit 应受理");
    let b = mgr
        .submit(up_job(
            write_temp(&vec![2u8; 1024 * 1024]),
            "/real/sym.bin",
            ENDPOINT,
        ))
        .await
        .expect("submit 应受理");
    let (done, busy) = tally_done_and_busy(&mut ev, &[a, b], "sym.bin").await;
    assert_eq!(
        (done, busy),
        (1, 1),
        "软链目录与真目录下的同名文件是同一个文件，必须恰好一成一拒"
    );
}

/// 审计2 #13（路径别名·相对 vs 绝对）：`rel.bin` 与 `/home/u/rel.bin` 是同一个文件。
///
/// 相对路径由服务端按 SFTP 会话的起始目录解析，客户端无从消解（`normalize_remote` 的
/// 文档明说了这一点，并刻意**不**合并二者——猜错的方向是把两个不同文件锁成一个）。
/// REALPATH(`.`) 正是那个缺失的基准。
#[tokio::test]
async fn a_relative_remote_path_and_its_absolute_twin_share_one_lock() {
    let fs = Arc::new(FakeFs::default());
    *fs.write_delay_ms.lock().unwrap() = 150;
    fs.set_canon(".", Some("/home/u"));
    fs.set_canon("/home/u", Some("/home/u"));
    let mgr = TransferManager::spawn(fs.clone(), 2);
    let mut ev = mgr.events().await;
    const ENDPOINT: &str = "relabs.example.com:22";
    let a = mgr
        .submit(up_job(
            write_temp(&vec![1u8; 1024 * 1024]),
            "rel.bin",
            ENDPOINT,
        ))
        .await
        .expect("submit 应受理");
    let b = mgr
        .submit(up_job(
            write_temp(&vec![2u8; 1024 * 1024]),
            "/home/u/rel.bin",
            ENDPOINT,
        ))
        .await
        .expect("submit 应受理");
    let (done, busy) = tally_done_and_busy(&mut ev, &[a, b], "rel.bin").await;
    assert_eq!(
        (done, busy),
        (1, 1),
        "`rel.bin` 与它在起始目录下的绝对写法是同一个文件，必须恰好一成一拒"
    );
}

/// 审计2 #13（主机别名）：按域名连的那条与按 IP 连的那条是同一台服务器。
///
/// `transfer_endpoint` 只做词法归一，`example.com:22` 与 `10.9.9.9:22` 在它眼里毫无关系。
/// app 层在装配时解析一次，把地址挂成 `endpoint_aliases`；锁按并集取，于是两条连接在
/// 地址那一维重合。**域名那一维仍然保留**——DNS 轮询让两次解析给出不同地址集时，
/// 靠的就是它兜底。
#[tokio::test]
async fn a_domain_and_its_resolved_ip_share_one_lock() {
    let fs = Arc::new(FakeFs::default());
    *fs.write_delay_ms.lock().unwrap() = 150;
    let mgr = TransferManager::spawn(fs.clone(), 2);
    let mut ev = mgr.events().await;
    // 按域名连的那条：别名里带着解析出的地址
    let a = mgr
        .submit(up_job_aliased(
            write_temp(&vec![1u8; 1024 * 1024]),
            "/hostalias.bin",
            "hostalias.example.com:22",
            &["10.9.9.9:22"],
        ))
        .await
        .expect("submit 应受理");
    // 按 IP 连的那条：host 本来就是地址，别名集与主键同值（`try_lock_targets` 去重）
    let b = mgr
        .submit(up_job_aliased(
            write_temp(&vec![2u8; 1024 * 1024]),
            "/hostalias.bin",
            "10.9.9.9:22",
            &["10.9.9.9:22"],
        ))
        .await
        .expect("submit 应受理");
    let (done, busy) = tally_done_and_busy(&mut ev, &[a, b], "/hostalias.bin").await;
    assert_eq!(
        (done, busy),
        (1, 1),
        "同一台服务器的两种连法必须共用一把锁——两件都成即意味着它们交错写了同一个 .fspart"
    );
    let landed = fs.get("/hostalias.bin").expect("赢家必须已提交最终目标");
    assert!(
        landed == vec![1u8; 1024 * 1024] || landed == vec![2u8; 1024 * 1024],
        "最终文件必须原样等于其中一件的内容；出现第三种内容即为交错写坏"
    );
}

/// 别名维是**加固**而非前置条件：服务端不支持 REALPATH（或 chroot 拒绝）时照常传输。
///
/// 这条守的是「修一个并发洞，顺手把所有传输都挡死」这种最难被察觉的回归——
/// 它在单机测试里表现为「一切正常」，只在那些老服务端上炸。
#[tokio::test]
async fn a_server_that_refuses_realpath_still_transfers() {
    let fs = Arc::new(FakeFs::default());
    fs.set_canon("/norp", None);
    let payload = vec![7u8; 4096];
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(
            write_temp(&payload),
            "/norp/x.bin",
            "norealpath.example.com:22",
        ))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Done => {}
        other => panic!("REALPATH 不可用只该少一维互斥，不该让传输失败：{other:?}"),
    }
    assert_eq!(fs.get("/norp/x.bin"), Some(payload), "内容必须完整落地");
}

/// REALPATH 失败时**词法那一维必须还在**——这是「降级」与「放弃」的分界线。
///
/// 上一条守的是「别把传输挡死」，很容易用「解析不出来就整个不上锁」来满足；那样单件传输
/// 一切正常，只有在老服务端上并发写同一路径时才交错写坏文件，而那正是本条要修的缺陷本身。
/// 两条一起才把这个方向钉住：既不能挡死，也不能退化成不上锁。
#[tokio::test]
async fn a_realpath_failure_still_keeps_the_lexical_lock() {
    let fs = Arc::new(FakeFs::default());
    *fs.write_delay_ms.lock().unwrap() = 150;
    fs.set_canon("/nolock", None);
    let mgr = TransferManager::spawn(fs.clone(), 2);
    let mut ev = mgr.events().await;
    const ENDPOINT: &str = "nolock.example.com:22";
    let a = mgr
        .submit(up_job(
            write_temp(&vec![1u8; 1024 * 1024]),
            "/nolock/x.bin",
            ENDPOINT,
        ))
        .await
        .expect("submit 应受理");
    let b = mgr
        .submit(up_job(
            write_temp(&vec![2u8; 1024 * 1024]),
            "/nolock/x.bin",
            ENDPOINT,
        ))
        .await
        .expect("submit 应受理");
    let (done, busy) = tally_done_and_busy(&mut ev, &[a, b], "/nolock/x.bin").await;
    assert_eq!(
        (done, busy),
        (1, 1),
        "REALPATH 不可用只该少一维，词法那一维必须照常互斥"
    );
}

/// 大小写别名（审计2 #13 的本地一侧）：`Report.pdf` 与 `report.pdf` 在
/// Windows/macOS 的默认卷上是**同一个文件**，在 Linux 上是两个。
///
/// 期望值按平台分叉，但两侧都是被断言的真实行为，不是「这台机器上碰巧如此」：
/// 大小写不敏感卷上必须恰好一成一拒（否则两件交错写同一个 `.fspart`），
/// 大小写敏感卷上必须两件都成（否则互不相干的两个文件被误挡）。
/// 这条同时钉住调用点传的是 `CASE_INSENSITIVE_FS` 而不是写死的常量。
#[tokio::test]
async fn local_case_aliases_share_one_lock_only_on_case_insensitive_volumes() {
    let fs = Arc::new(FakeFs::default());
    *fs.write_delay_ms.lock().unwrap() = 150;
    let payload = vec![9u8; 1024 * 1024];
    fs.files
        .lock()
        .unwrap()
        .insert("/case.bin".into(), payload.clone());
    let root = temp_subdir();
    let mgr = TransferManager::spawn(fs.clone(), 2);
    let mut ev = mgr.events().await;
    let a = mgr
        .submit(down_job(root.join("Report.pdf"), "/case.bin", root.clone()))
        .await
        .expect("submit 应受理");
    let b = mgr
        .submit(down_job(root.join("report.pdf"), "/case.bin", root.clone()))
        .await
        .expect("submit 应受理");
    let mut terminals = drain_terminals(&mut ev, &[a, b]).await;
    let (mut done, mut busy) = (0, 0);
    for id in [a, b] {
        match terminals.remove(&id).expect("两件都须有终态") {
            TransferState::Done => done += 1,
            TransferState::Failed(msg) if msg.contains("目标忙") => busy += 1,
            other => panic!("只应是 Done 或「目标忙」，实得 {other:?}"),
        }
    }
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        assert_eq!(
            (done, busy),
            (1, 1),
            "大小写不敏感卷上两种拼法是同一个文件，必须恰好一成一拒"
        );
    } else {
        assert_eq!(
            (done, busy),
            (2, 0),
            "大小写敏感卷上是两个不同文件，不得互相误挡"
        );
    }
}

/// 一件孤零零的传输不得被自己的别名键挡住。
///
/// 键集里有重复项是**常态**而非边角：路径本来就规范时，规范名与词法名逐字相同；
/// 按 IP 连接时，别名与主端点也逐字相同。少一步去重，每一次上传都会当场报「目标忙」。
#[tokio::test]
async fn a_lone_upload_is_never_blocked_by_its_own_alias_keys() {
    let fs = Arc::new(FakeFs::default());
    fs.set_canon("/selfd", Some("/canon"));
    let payload = vec![3u8; 4096];
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job_aliased(
            write_temp(&payload),
            "/selfd/only.bin",
            "self.example.com:22",
            // 主端点自身也混在别名里（IP 连接时的真实形态），外加一个重复项
            &["self.example.com:22", "10.1.1.1:22", "10.1.1.1:22"],
        ))
        .await
        .expect("submit 应受理");
    match drain_to_terminal(&mut ev, id).await {
        TransferState::Done => {}
        other => panic!("单件传输不该被自己的键集挡住：{other:?}"),
    }
    assert_eq!(fs.get("/selfd/only.bin"), Some(payload));
}

/// 审计 P0-1：传输 id 必须是**进程级**唯一。
///
/// 旧实现把计数器挂在管理器上（per-session），两个会话的首件传输都拿到 id=1：
/// 「取消 id=1」会打到另一个会话、传后校验表互相覆盖、UI 两行并作一行。
#[tokio::test]
async fn transfer_ids_are_globally_unique_across_managers() {
    let a = next_transfer_id();
    let b = next_transfer_id();
    assert!(b > a, "连续取号必须严格递增：{a} → {b}");

    let fs1 = Arc::new(FakeFs::default());
    let fs2 = Arc::new(FakeFs::default());
    let m1 = TransferManager::spawn(fs1.clone(), 1);
    let m2 = TransferManager::spawn(fs2.clone(), 1);
    let mut e1 = m1.events().await;
    let mut e2 = m2.events().await;
    let id1 = m1
        .submit(up_job(write_temp(b"one"), "/one.bin", "ns-uniq-1"))
        .await
        .expect("submit 应受理");
    let id2 = m2
        .submit(up_job(write_temp(b"two"), "/two.bin", "ns-uniq-2"))
        .await
        .expect("submit 应受理");
    assert_ne!(
        id1, id2,
        "两个独立管理器的首件传输不得撞号（实得 {id1} / {id2}）"
    );
    // 收干终态再退出，避免任务在事件发送端上悬着（也顺带验证两边都真的跑通了）。
    assert!(matches!(
        drain_to_terminal(&mut e1, id1).await,
        TransferState::Done
    ));
    assert!(matches!(
        drain_to_terminal(&mut e2, id2).await,
        TransferState::Done
    ));
}

/// 审计 P0-5：`shutdown` 必须真的停住传输，而不是让调用方把管理器从 map 里删掉就当没事。
/// ① 停收新作业（submit 返回 Err）② 在途任务在宽限期内到达终态（active_len 归零）。
#[tokio::test]
async fn shutdown_rejects_new_jobs_and_drains_in_flight() {
    let fs = Arc::new(FakeFs::default());
    *fs.write_delay_ms.lock().unwrap() = 100; // 1 MiB = 4 块 ≈ 400 ms，保证 shutdown 时仍在途
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let _ev = mgr.events().await;
    mgr.submit(up_job(
        write_temp(&vec![9u8; 1024 * 1024]),
        "/sd.bin",
        "ns-shutdown",
    ))
    .await
    .expect("关停前的 submit 应受理");
    assert_eq!(mgr.active_len().await, 1, "在途传输必须登记在案");

    assert!(
        mgr.shutdown(Duration::from_secs(10)).await,
        "宽限期 10 s 内在途传输必须全部收尾"
    );
    assert_eq!(
        mgr.active_len().await,
        0,
        "关停返回 true 时在途件数必须为 0"
    );
    assert!(
        mgr.submit(up_job(write_temp(b"late"), "/late.bin", "ns-shutdown"))
            .await
            .is_err(),
        "关停后必须拒收新作业，绝不能返回一个永远不会有终态的 id"
    );
    assert_eq!(
        fs.get("/sd.bin"),
        None,
        "被关停打断的传输不得提交：最终目标必须完全没被创建过"
    );
}

/// 重试必须从已完成的字节续跑，不得退回起点（S46）。
/// 原实现每次尝试都从 `job.resume_offset` 重新起跑：9 GB 传到 8.9 GB 断一次就白扔 8.9 GB，
/// 而断点续传恰恰是本 crate 的招牌能力。断言钉在 write_at 的 **offset 流水**上，
/// 而非「最终内容正确」——后者在两种实现下都成立（重传只是覆盖同样的字节），
/// 于是回归会静默通过。
#[tokio::test]
async fn retry_resumes_from_achieved_offset() {
    let fs = Arc::new(FakeFs::default());
    // 第 1 次写（0 基索引 1，即第二个分块）失败一次；其余全部成功。
    *fs.fail_write_indices.lock().unwrap() = vec![1];
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let payload: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect(); // 3 个分块
    let local = write_temp(&payload);
    let mut ev = mgr.events().await;
    let id = mgr
        .submit(up_job(local, "/resume.bin", "ns-resume"))
        .await
        .expect("submit 应受理");
    let mut retry_bytes_done = None;
    loop {
        let e = next_event(&mut ev).await;
        if e.id != id {
            continue;
        }
        match e.state {
            TransferState::Retrying { .. } => retry_bytes_done = Some(e.bytes_done),
            TransferState::Done => break,
            TransferState::Failed(m) => panic!("单次写失败应被重试吸收: {m}"),
            TransferState::Cancelled => panic!("未取消却收到 Cancelled"),
            TransferState::Running => {}
        }
    }
    let log = fs.write_log.lock().unwrap().clone();
    assert_eq!(log[0].0, 0, "首块必须从 0 起（流水基线）");
    assert_eq!(
        log[1].0, CHUNK as u64,
        "第二块（注入失败的那次）应落在 一个分块 处"
    );
    assert_eq!(
        log[2].0, CHUNK as u64,
        "重试必须从已完成的 一个分块 续跑，不得退回 0（实得流水 {log:?}）"
    );
    assert_eq!(
        retry_bytes_done,
        Some(CHUNK as u64),
        "Retrying 事件的 bytes_done 须报实际进度，报 0 会让 UI 进度条假摔回起点"
    );
    assert_eq!(
        fs.get("/resume.bin"),
        Some(payload),
        "续传重试后远端内容仍须逐字节正确"
    );
}

/// 终态后必须摘除 per-id 取消标志（S50）。
/// `submit` 只插不删会让 `cancels` 随会话寿命无界增长——Xftp 式批量传输单次可达十万件。
/// 先断言「传输在途时确实被登记」（否则归零断言可以靠「压根没登记」空过），
/// 再断言全部达终态后归零。
#[tokio::test]
async fn active_len_drops_to_zero_after_terminal_state() {
    let fs = Arc::new(FakeFs::default());
    *fs.write_delay_ms.lock().unwrap() = 50; // 串行 3 件 ≥150 ms，登记断言有充裕余量
    let mgr = TransferManager::spawn(fs.clone(), 1);
    let mut ev = mgr.events().await;
    let mut ids = Vec::new();
    for i in 0..3 {
        let local = write_temp(b"payload");
        ids.push(
            mgr.submit(up_job(local, &format!("/a{i}.bin"), "ns-active"))
                .await
                .expect("submit 应受理"),
        );
    }
    assert_eq!(mgr.active_len().await, 3, "在途传输必须登记在案");
    let mut done = 0;
    while done < 3 {
        let e = next_event(&mut ev).await;
        match e.state {
            TransferState::Done => done += 1,
            TransferState::Failed(m) => panic!("顺利上传不应失败: {m}"),
            _ => {}
        }
    }
    // 摘除发生在 run_transfer 返回之后、即 Done 事件之后，故轮询而非即刻断言。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if mgr.active_len().await == 0 {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "全部达终态 10 s 后取消标志仍未摘除，实得 {} 条",
            mgr.active_len().await
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn write_temp(bytes: &[u8]) -> std::path::PathBuf {
    let p = temp_path();
    std::fs::write(&p, bytes).unwrap();
    p
}
/// S59（med）：S22 的跨进程撞名修复**漏掉了本文件**——`temp_path`/`temp_subdir` 仍是旧式
/// 「pid + 进程内计数器」命名（`temp_subdir` 还叠加 `create_dir_all`）：Windows 回收 pid 后
/// 新测试进程会继承上一轮同名残留（`%TEMP%` 实测积有数千份 `fs-*` 残留），下载目标/沙箱根
/// 跑在前人终态上，假红假绿双向发生。处置同 S22：名字加纳秒；目录创建改 `create_dir` 撞名重试。
/// （`temp_path` 只产名字不建文件，写入方 `fs::write` 自带截断，纳秒名已足以消除残留语义。）
fn temp_path() -> std::path::PathBuf {
    static C: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "fs-xfer-{}-{}-{}.bin",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时钟早于 UNIX 纪元")
            .as_nanos(),
        C.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ))
}
fn temp_subdir() -> std::path::PathBuf {
    static D: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = std::env::temp_dir();
    loop {
        let p = base.join(format!(
            "fs-sbx-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时钟早于 UNIX 纪元")
                .as_nanos(),
            D.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        match std::fs::create_dir(&p) {
            Ok(()) => return p,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("创建测试临时目录失败 {}: {e}", p.display()),
        }
    }
}

/// S59 回归：沙箱子目录助手绝不得交回已存在的目录（铺 128 槽 + 毒文件，对偶 connmgr S22 回归）。
#[test]
fn temp_subdir_never_hands_back_an_existing_directory() {
    let base = std::env::temp_dir();
    let seeded: Vec<_> = (0..128u64)
        .map(|n| base.join(format!("fs-sbx-{}-{}", std::process::id(), n)))
        .collect();
    for p in &seeded {
        std::fs::create_dir_all(p).unwrap();
        std::fs::write(p.join("poison.txt"), b"leftover from a previous run").unwrap();
    }
    for _ in 0..8 {
        let d = temp_subdir();
        assert_eq!(
            std::fs::read_dir(&d).unwrap().count(),
            0,
            "助手交回了一个已存在且非空的目录：{}（S59）",
            d.display()
        );
    }
    for p in &seeded {
        let _ = std::fs::remove_dir_all(p);
    }
}

#[test]
fn sha256_hex_validation() {
    use fs_sshengine::transfer::Sha256Hex;
    assert!(Sha256Hex::new("a".repeat(64)).is_ok());
    assert!(Sha256Hex::new("0123456789abcdef".repeat(4)).is_ok());
    assert!(
        Sha256Hex::new("A".repeat(64)).is_err(),
        "大写必须拒绝（与服务端 sha256sum 输出逐字比对的前提）"
    );
    assert!(Sha256Hex::new("abc").is_err());
    assert!(Sha256Hex::new("g".repeat(64)).is_err());
}
