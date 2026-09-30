//! ZMODEM 终端内传输的应用层桥接（M4a：rz/sz）。
//!
//! 协议在 [`fs_terminal::zmodem`]，状态机在 [`fs_terminal::zsession`]，本模块只做
//! 三件外部世界的事：**从终端字节流里认出传输**、**把副作用送出去**（回写 SSH、
//! 落盘、发前端事件）、**在沙箱里定文件名**。
//!
//! ## 为什么拆成「同步拦截器 + 异步驱动」
//!
//! 拦截判定必须与字节到达同步完成：终端没有撤回操作，二进制帧一旦渲染，屏幕上
//! 的乱码就已经在那里了，还可能被帧里的转义字节改掉终端状态（换字符集、切备用
//! 屏），传完之后整块屏幕都是废的。所以 [`ZmodemTap::filter`] 必须当场决定吞或放。
//!
//! 但吞下去之后要做的事全是异步的（往 SSH 写字节要拿会话写锁）。于是状态机在
//! 拦截器里**同步**推进（纯字节逻辑，纳秒级），产出的副作用投进无界队列，由驱动
//! 任务串行执行。队列保序，所以「开文件 → 写数据 → 关文件」的次序天然正确。
//!
//! ## 沙箱路径在装配时定格
//!
//! 落盘根目录在**建会话时**解析一次存进拦截器，而不是每次传输回查设置。理由与
//! `LiveSession::target_endpoint` 同源：设置是可改的，回查会让同一条连接上前后
//! 两次传输落到不同的根目录下——用户找不到文件，而且这种漂移无法自证。
//!
//! ## 不做的事（不假装做了）
//!
//! - 不做断点续传：对端请求非零偏移时明确取消（见 zsession 的 ZRPOS 分支）。
//! - 不做多文件批量：一次会话只处理一个文件，第二个 ZFILE 会被当作新会话开始。
//! - 不自动决定覆盖：目标同名时改名为 `name (1).ext` 而不是覆盖或询问——传输
//!   是异步发生的，弹窗询问期间对端在等，超时会让整条传输失败得莫名其妙。

use fs_terminal::pipe::ByteTap;
use fs_terminal::zmodem::detect_start;
use fs_terminal::zsession::{Action, Direction, ZmodemSession};
use std::borrow::Cow;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

/// 检测滑窗长度：帧头可能被 SSH 读块切开，需带上一块的尾巴一起嗅探。
///
/// 取 64 字节的依据：最长的帧头形态是十六进制头（`**\x18B` + 10 个 hex + 4 个 CRC
/// hex + CR LF XON ≈ 21 字节），64 有三倍余量，又小到不值得担心其代价。
const SNIFF_WINDOW: usize = 64;

/// 检出起始帧后该做什么。
///
/// 这个映射极易搞反，搞反的表现还很隐蔽（远端 `rz` 在等文件，我们却开了个接收
/// 会话，双方各等对方先说话，用户看到终端「卡住」直到 rz 超时）：
/// - `sz`（远端**发**）先发 **ZRQINIT** → 我们接收。
/// - `rz`（远端**收**）先发 **ZRINIT** → 我们发送，但发什么得先问人。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detected {
    /// 立刻开始接收
    Receive,
    /// 远端在等文件：请前端弹「选择本地文件」，本端**不**进入传输态
    NeedLocalFile,
}

fn classify_start(kind: fs_terminal::zmodem::FrameKind) -> Option<Detected> {
    use fs_terminal::zmodem::FrameKind as K;
    match kind {
        K::ZRQINIT => Some(Detected::Receive),
        K::ZRINIT => Some(Detected::NeedLocalFile),
        // detect_start 只认这两种；其余不该走到这里，走到了也不猜
        _ => None,
    }
}

/// 副作用：拦截器（同步）产出，驱动任务（异步）消费。
enum Effect {
    /// 回写 SSH 通道
    Send(Vec<u8>),
    /// 开始接收：在沙箱里定名并建文件
    Begin { name: String, size: Option<u64> },
    /// 追加落盘
    Write(Vec<u8>),
    /// 当前文件收完
    Finish { bytes: u64 },
    /// 索要待发文件在该偏移处的一块。
    ///
    /// 偏移必须带着走、不能假定顺序读：ZRPOS 是 ZMODEM 的重传机制，对端校验出错
    /// 就会要求**回退**到某个偏移重发。顺序读会从上次读到的地方继续，于是重发的
    /// 内容错位——落地文件长度正确、内容从错位点起全错，且不报任何错。
    NeedChunk { offset: u64 },
    /// 发送侧：当前文件被对端收妥。驱动任务据此出队下一个或 finish。
    FileSent,
    /// 会话终结
    Done { ok: bool, message: String },
}

/// 多文件上传的队列项。句柄在入队时打开（begin_send 全量校验的产物），
/// 排队期间一直持有——排队时再开，文件可能已经被人挪走，而那要等到
/// 轮到它才发现，报错时用户已经忘了当初选了什么。
struct PendingFile {
    path: PathBuf,
    /// begin_send 校验时记下的字节数。排队期间文件可能被人改掉——按**入队时**
    /// 的大小发，发不完（文件变短）协议自己会以 ZRPOS 重传暴露；按现查的大小发
    /// 则要在驱动的热路径上做一次 metadata，换不来任何正确性。
    size: u64,
    file: Option<std::io::BufReader<std::fs::File>>,
}

/// 传输态：状态机 + 两端的文件句柄。
struct Transfer {
    session: ZmodemSession,
    /// 接收落盘句柄与最终路径
    sink: Option<std::io::BufWriter<std::fs::File>>,
    sink_path: Option<PathBuf>,
    /// 发送来源句柄
    source: Option<std::io::BufReader<std::fs::File>>,
    /// 接收侧：对端 ZFILE 声明的原始文件名（finalize 时清洗落定）
    recv_name: Option<String>,
    /// 进度节流用：上次已上报的字节数
    reported: u64,
    /// 多文件上传：其余待发文件（含已打开的句柄）
    pending: std::collections::VecDeque<PendingFile>,
    /// 当前是第几个文件（1 起数）
    file_index: u32,
    /// 共几个文件
    file_count: u32,
}

/// 终端字节拦截器 + 传输态持有者。
pub struct ZmodemTap {
    session_id: String,
    app: AppHandle,
    /// 落盘根目录（装配时定格，见模块头）
    sandbox: PathBuf,
    /// 自动检测开关：关闭时完全透传（用户不想让终端里的字节触发传输）
    auto: bool,
    /// 当前传输；None = 空闲
    cur: Mutex<Option<Transfer>>,
    /// 跨读块嗅探滑窗
    sniff: Mutex<Vec<u8>>,
    /// 快路径判定：空闲且不需嗅探时避免加锁
    busy: AtomicBool,
    /// 「远端在等文件」是否已通知过前端。
    ///
    /// `rz` 每隔约十秒重发一次 ZRINIT，逐次弹框会把用户埋在对话框里。置位后
    /// 只在传输结束（或取消）时复位。
    need_file_announced: AtomicBool,
    /// 本会话已确定的下载目录（2026-08-26 两段式）。
    ///
    /// 一次 `sz` 批量传多个文件时，第一个文件做的「去哪」决定对**本次会话**
    /// 生效：后续文件不再问去向（冲突仍逐个问）。会话结束即作废——下次重新
    /// 按设置/引导走，不让一个临时决定悄悄变成永久设置。
    session_download_dir: Mutex<Option<PathBuf>>,
    /// 收完待归置的下载（spool 路径 + 对端声明的原名）。
    ///
    /// `Effect::Finish` 收完一个文件时填入；`zmodem_finalize` 命令消费它。
    /// 任何时刻至多一个（协议上 Finish 之后才有下一个 Begin）。
    pending_finalize: Mutex<Option<PendingFinalize>>,
    /// 本会话真正落地的最终路径集合（reveal 的白名单，见 [`Self::resolve_received`]）。
    finalized: Mutex<std::collections::HashSet<PathBuf>>,
    tx: mpsc::UnboundedSender<Effect>,
}

/// 一次待归置的下载。
#[derive(Clone)]
struct PendingFinalize {
    /// spool 文件（sandbox/.incoming/recv-<纳秒>.part）
    spool: PathBuf,
    /// 对端 ZFILE 声明的原始文件名（尚未清洗——清洗在 finalize 时做，
    /// 因为清洗规则属于「最终落地」这一侧，spool 名与它无关）。
    orig_name: String,
    /// 对端声明的字节数（saved 事件的 total 用）。
    bytes: u64,
}

impl ZmodemTap {
    /// 建拦截器并启动驱动任务。
    pub fn new(
        app: AppHandle,
        state: Arc<crate::state::AppState>,
        session_id: String,
        sandbox: PathBuf,
        auto: bool,
    ) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let tap = Arc::new(Self {
            session_id: session_id.clone(),
            app: app.clone(),
            sandbox,
            auto,
            cur: Mutex::new(None),
            sniff: Mutex::new(Vec::new()),
            busy: AtomicBool::new(false),
            need_file_announced: AtomicBool::new(false),
            session_download_dir: Mutex::new(None),
            pending_finalize: Mutex::new(None),
            finalized: Mutex::new(Default::default()),
            tx,
        });
        tokio::spawn(drive(app, state, session_id, Arc::downgrade(&tap), rx));
        tap
    }

    /// 用户选定本地文件，开始上传（对端已在跑 `rz`）。多文件：一次入队，逐个发。
    ///
    /// **入队前全部校验**，任何一个不合法整批拒绝（错误里点名是哪个）。不是
    /// 「传到一半发现下一个打不开」——传一半再报错，用户已经付出的等待就废了，
    /// 而且远端 rz 已经收了一半文件。
    pub fn begin_send(&self, paths: &[PathBuf]) -> Result<(), String> {
        if paths.is_empty() {
            return Err("没有选中任何文件".into());
        }
        // 先全部校验并打开：开句柄失败的那个文件点名报错，一个都不开始传。
        let mut opened = Vec::with_capacity(paths.len());
        for p in paths {
            let meta = match std::fs::metadata(p) {
                Ok(m) => m,
                Err(e) => return Err(format!("读不到本地文件 {}：{e}", p.display())),
            };
            if !meta.is_file() {
                return Err(format!(
                    "{} 不是普通文件（目录请用 SFTP 面板传输）",
                    p.display()
                ));
            }
            match std::fs::File::open(p) {
                Ok(f) => opened.push((p.clone(), meta.len(), f)),
                Err(e) => return Err(format!("打不开本地文件 {}：{e}", p.display())),
            }
        }
        let total = opened.len() as u32;
        let mut cur = self.cur.lock().unwrap();
        if cur.is_some() {
            return Err("该会话已有传输在进行".into());
        }
        let (first_path, first_size, first_file) = opened.remove(0);
        let name = first_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .to_string();
        let session = ZmodemSession::new_send(&name, first_size);
        let hello = session.start();
        *cur = Some(Transfer {
            session,
            sink: None,
            sink_path: None,
            source: Some(std::io::BufReader::new(first_file)),
            recv_name: None,
            reported: 0,
            pending: opened
                .into_iter()
                .map(|(path, size, file)| PendingFile {
                    path,
                    size,
                    file: Some(std::io::BufReader::new(file)),
                })
                .collect(),
            file_index: 1,
            file_count: total,
        });
        drop(cur);
        self.busy.store(true, Ordering::SeqCst);
        let _ = self.tx.send(Effect::Send(hello));
        self.emit_progress_files("sending", &name, 0, Some(first_size), 1, total);
        Ok(())
    }

    /// 带「第 i/N 个」字段的进度事件（多文件上传用）。
    fn emit_progress_files(
        &self,
        phase: &str,
        name: &str,
        done: u64,
        total: Option<u64>,
        file_index: u32,
        file_count: u32,
    ) {
        emit_progress_full(
            &self.app,
            &self.session_id,
            phase,
            name,
            None,
            done,
            total,
            "",
            Some(file_index),
            Some(file_count),
            None,
            None,
            None,
        );
    }

    /// 用户取消当前传输。
    pub fn cancel(&self) -> Result<(), String> {
        let mut cur = self.cur.lock().unwrap();
        let t = cur.as_mut().ok_or("当前没有进行中的传输")?;
        let acts = t.session.cancel();
        drop(cur);
        self.dispatch(acts);
        Ok(())
    }

    /// 是否有传输在进行（前端进度条的存在依据）。
    pub fn active(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// 落盘根目录（给前端显示「下载到哪」与「打开所在目录」用）。
    pub fn sandbox_display(&self) -> String {
        display_path(&self.sandbox)
    }

    /// 自动检测是否开启。
    pub fn auto_enabled(&self) -> bool {
        self.auto
    }

    /// 校验一条「已接收文件」的路径，供 reveal 用。
    ///
    /// 入参来自前端，而 reveal 会打开文件管理器并选中目标——拿它去探测任意路径
    /// 是否存在是可行的，所以必须校验。**2026-08-26 起改为 finalized 白名单**：
    /// 旧判据是「路径在沙箱内」，但两段式之后最终落点可以在任何用户目录，
    /// 沙箱前缀判据失效。白名单只认**本会话真的保存过**的那些精确路径——
    /// 这比旧判据更严（从「沙箱内任意路径」收窄到「实际落地过的文件」），
    /// 安全上是收窄不是让步。
    pub fn resolve_received(&self, path: &str) -> Result<PathBuf, String> {
        if path.trim().is_empty() {
            return Err("路径为空".into());
        }
        let p = PathBuf::from(path);
        if self.finalized.lock().unwrap().contains(&p) {
            Ok(p)
        } else {
            Err("该路径不是本会话保存过的文件".into())
        }
    }

    /// IPC 侧的归置入口（`zmodem_finalize` 命令转调）。
    ///
    /// 命令层负责 remember 写设置与 emit `saved`——那两件要 AppHandle/数据库，
    /// 而本方法保持纯文件操作，便于单测。
    pub fn finalize_from_ipc(
        &self,
        dir: &str,
        name: Option<&str>,
        on_conflict: &str,
    ) -> Result<Option<(String, PathBuf, u64)>, String> {
        let dir = dir.trim();
        if dir.is_empty() {
            return Err("目标目录为空".into());
        }
        finalize_download(self, &self.session_id, Path::new(dir), name, on_conflict)
    }

    /// 把状态机动作翻译成副作用并投递。
    fn dispatch(&self, acts: Vec<Action>) {
        for a in acts {
            let eff = match a {
                Action::Send(b) => Effect::Send(b),
                Action::BeginRecv(info) => Effect::Begin {
                    name: info.name,
                    size: info.size,
                },
                Action::Write(d) => Effect::Write(d),
                Action::FinishRecv { bytes } => Effect::Finish { bytes },
                Action::NeedChunk { offset } => Effect::NeedChunk { offset },
                Action::FileSent => Effect::FileSent,
                Action::Done { ok, message } => Effect::Done { ok, message },
            };
            // 发送失败 = 驱动任务已退出（会话关闭）。此时无处执行副作用，
            // 丢弃是唯一选择；不要在这里 unwrap，那会 panic 在读取任务里。
            let _ = self.tx.send(eff);
        }
    }

    fn emit_state(&self, phase: &str, name: &str, done: u64, total: Option<u64>) {
        emit_progress(
            &self.app,
            &self.session_id,
            phase,
            name,
            None,
            done,
            total,
            "",
        );
    }
}

/// 唯一的 `zmodem:progress` 发送点。
///
/// 三处调用方（拦截器、驱动的开始/收尾）走同一个函数，是为了让载荷**键集恒定**：
/// 各处各发一份自己需要的键，前端就得对每个键都写 `?? undefined` 的兜底，而
/// 「这个 phase 到底有没有 path」会变成只能靠读后端代码才能知道的隐性契约。
/// 键恒定之后，前端的类型就是完整的事实。
///
/// `file_index` / `file_count` 同理：多文件上传（2026-08-26）加的两键**每个事件都带**，
/// 非上传场景为 `null`——不是缺席。缺席会让前端在「这条事件有没有这两个键」上
/// 分叉，而 `null` 的语义就是「本事件不适用」，类型上一目了然。
///
/// 事件名保持字面量内联：仓库的跨语言契约扫描从调用点提取事件名，抽成常量它就
/// 扫不到，前后端事件名不一致的守卫随之失效。
#[allow(clippy::too_many_arguments)] // 参数即载荷字段，合并成结构体只是把同样的东西挪个地方
fn emit_progress(
    app: &AppHandle,
    session_id: &str,
    phase: &str,
    name: &str,
    path: Option<&Path>,
    done: u64,
    total: Option<u64>,
    message: &str,
) {
    emit_progress_full(
        app, session_id, phase, name, path, done, total, message, None, None, None, None, None,
    )
}

/// [`emit_progress`] 的全参版（多文件上传的「第 i/N 个」+ 下载决策的三个补充键）。
///
/// 三组补充键**每个事件都带**（null = 不适用）：file_index/file_count（多文件上传）、
/// suggestedDir/spoolPath/conflict（`needs-decision` 阶段）。键集恒定的理由见
/// [`emit_progress`] 的模块注释。
#[allow(clippy::too_many_arguments)]
fn emit_progress_full(
    app: &AppHandle,
    session_id: &str,
    phase: &str,
    name: &str,
    path: Option<&Path>,
    done: u64,
    total: Option<u64>,
    message: &str,
    file_index: Option<u32>,
    file_count: Option<u32>,
    suggested_dir: Option<&str>,
    spool_path: Option<&str>,
    conflict: Option<bool>,
) {
    let _ = app.emit(
        "zmodem:progress",
        serde_json::json!({
            "sessionId": session_id,
            "phase": phase,
            "name": name,
            "path": path.map(display_path),
            "done": done,
            "total": total,
            "message": message,
            "file_index": file_index,
            "file_count": file_count,
            "suggestedDir": suggested_dir,
            "spoolPath": spool_path,
            "conflict": conflict,
        }),
    );
}

impl ByteTap for ZmodemTap {
    fn filter<'a>(&self, chunk: &'a [u8]) -> Cow<'a, [u8]> {
        // 空闲且不自动检测：整条链路零成本透传
        if !self.auto && !self.busy.load(Ordering::SeqCst) {
            return Cow::Borrowed(chunk);
        }
        let mut cur = self.cur.lock().unwrap();
        if let Some(t) = cur.as_mut() {
            // 传输中：全部字节喂状态机，屏幕上什么都不出
            let acts = t.session.feed(chunk);
            let finished = t.session.is_finished();
            let tail = if finished {
                t.session.take_remainder()
            } else {
                Vec::new()
            };
            let (done, total) = (t.session.transferred(), t.session.declared_size());
            let dir = t.session.direction();
            // 里程碑事件带上「第 i/N 个」：不带的话，多文件传输中每个 64 KiB 里程碑
            // 都会把进度条上的计数洗回 null（前端有 prev 兜底，但事件本身说全才是对的）。
            let (fi, fc) = (t.file_index, t.file_count);
            // 进度节流：每 64 KiB 或有里程碑动作时才发事件。逐块发会在千兆链路上
            // 每秒糊上千条 IPC，前端忙于重绘反而拖慢传输。
            let milestone = done.saturating_sub(t.reported) >= 64 * 1024;
            if milestone {
                t.reported = done;
            }
            if finished {
                *cur = None;
                self.busy.store(false, Ordering::SeqCst);
                self.sniff.lock().unwrap().clear();
            }
            drop(cur);
            self.dispatch(acts);
            if milestone && dir == Direction::Send {
                self.emit_progress_files("sending", "", done, total, fi, fc);
            } else if milestone {
                self.emit_state("receiving", "", done, total);
            }
            return if tail.is_empty() {
                Cow::Borrowed(&[])
            } else {
                Cow::Owned(tail)
            };
        }

        // 空闲：嗅探帧起始。滑窗与本块拼一起再找，否则被读块切开的帧头认不出来。
        let mut sniff = self.sniff.lock().unwrap();
        let s = sniff.len();
        let mut probe = std::mem::take(&mut *sniff);
        probe.extend_from_slice(chunk);
        match detect_start(&probe) {
            None => {
                // 只留尾巴，别让滑窗随输出无界增长
                let keep = probe.len().saturating_sub(SNIFF_WINDOW);
                *sniff = probe[keep..].to_vec();
                Cow::Borrowed(chunk)
            }
            Some((kind, at)) => match classify_start(kind) {
                None => {
                    // detect_start 认了但分类不认：宁可透传也不猜
                    let keep = probe.len().saturating_sub(SNIFF_WINDOW);
                    *sniff = probe[keep..].to_vec();
                    Cow::Borrowed(chunk)
                }
                Some(Detected::NeedLocalFile) => {
                    // 远端 `rz` 在等文件。**不**进入传输态：进去就得一直吞字节等人
                    // 选文件，人不选就永远吞下去，终端从此哑掉。让 ZRINIT 原样显示
                    // （屏幕上闪一串 `**B0100...`，Xshell 同样如此），同时通知前端
                    // 弹选择框；用户选完由 `begin_send` 正式起会话。
                    sniff.clear();
                    drop(sniff);
                    drop(cur);
                    if !self.need_file_announced.swap(true, Ordering::SeqCst) {
                        tracing::info!(
                            session_id = %self.session_id,
                            "检出 ZRINIT：远端在等文件（rz），请求前端选择本地文件"
                        );
                        self.emit_state("need-file", "", 0, None);
                    }
                    Cow::Borrowed(chunk)
                }
                Some(Detected::Receive) => {
                    sniff.clear();
                    drop(sniff);
                    let mut session = ZmodemSession::new_receive();
                    let hello = session.start();
                    let acts = session.feed(&probe[at..]);
                    let (done, total) = (session.transferred(), session.declared_size());
                    let finished = session.is_finished();
                    if !finished {
                        *cur = Some(Transfer {
                            session,
                            sink: None,
                            sink_path: None,
                            source: None,
                            recv_name: None,
                            reported: 0,
                            pending: Default::default(),
                            file_index: 1,
                            file_count: 1,
                        });
                        self.busy.store(true, Ordering::SeqCst);
                    }
                    drop(cur);
                    tracing::info!(
                        session_id = %self.session_id, ?kind, at,
                        "检出 ZRQINIT：远端 sz 发文件，转入接收模式"
                    );
                    let _ = self.tx.send(Effect::Send(hello));
                    self.dispatch(acts);
                    self.emit_state("receiving", "", done, total);
                    // 帧起始之前的字节仍属终端。`at` 是在 `probe` 里的下标：落在滑窗
                    // 段内（at < s）说明帧头始于上一块，本块无一字节属于终端。
                    if at >= s {
                        Cow::Borrowed(&chunk[..at - s])
                    } else {
                        Cow::Borrowed(&[])
                    }
                }
            },
        }
    }
}

/// 驱动任务：串行执行副作用。
async fn drive(
    app: AppHandle,
    state: Arc<crate::state::AppState>,
    session_id: String,
    tap: std::sync::Weak<ZmodemTap>,
    mut rx: mpsc::UnboundedReceiver<Effect>,
) {
    while let Some(eff) = rx.recv().await {
        match eff {
            Effect::Send(bytes) => {
                let Some(session) = state.registry.get(&session_id) else {
                    tracing::debug!(%session_id, "会话已关闭，ZMODEM 回写丢弃");
                    continue;
                };
                let guard = session.write.lock().await;
                if let Err(e) = guard.data_bytes(bytes).await {
                    tracing::warn!(%session_id, error = %e, "ZMODEM 回写失败");
                }
            }
            Effect::Begin { name, size } => {
                let Some(tap) = tap.upgrade() else { return };
                match open_spool(&tap.sandbox) {
                    Ok((file, path)) => {
                        tracing::info!(%session_id, ?path, size, "ZMODEM 开始接收（spool）");
                        let mut cur = tap.cur.lock().unwrap();
                        if let Some(t) = cur.as_mut() {
                            t.sink = Some(std::io::BufWriter::new(file));
                            t.sink_path = Some(path.clone());
                            t.recv_name = Some(name.clone());
                        }
                        drop(cur);
                        // spool 期间给用户看的是**对端声明的名字**——.incoming 里的
                        // recv-*.part 对人毫无意义；真正落定的名字 finalize 时再报。
                        emit_progress(&app, &session_id, "receiving", &name, None, 0, size, "");
                    }
                    Err(e) => {
                        tracing::warn!(%session_id, name, error = %e, "ZMODEM 建暂存文件失败，取消传输");
                        // 建不出文件就别让对端白发几百兆：立刻取消
                        if let Ok(()) = tap.cancel() {}
                    }
                }
            }
            Effect::Write(data) => {
                let Some(tap) = tap.upgrade() else { return };
                let mut cur = tap.cur.lock().unwrap();
                if let Some(t) = cur.as_mut() {
                    if let Some(sink) = t.sink.as_mut() {
                        if let Err(e) = sink.write_all(&data) {
                            tracing::warn!(%session_id, error = %e, "ZMODEM 落盘写失败");
                        }
                    }
                }
            }
            Effect::Finish { bytes } => {
                let Some(tap) = tap.upgrade() else { return };
                let (spool, orig_name) = {
                    let mut cur = tap.cur.lock().unwrap();
                    cur.as_mut()
                        .map(|t| {
                            if let Some(mut sink) = t.sink.take() {
                                // flush 的错误必须看：BufWriter 的 Drop 会吞掉它，
                                // 那样「传输成功」的提示配上一个短了几 KiB 的文件。
                                if let Err(e) = sink.flush() {
                                    tracing::warn!(%session_id, error = %e, "ZMODEM 收尾 flush 失败");
                                }
                            }
                            (t.sink_path.clone(), t.recv_name.clone())
                        })
                        .unwrap_or((None, None))
                };
                let (Some(spool), Some(orig_name)) = (spool, orig_name) else {
                    continue; // 没有 Begin 的 Finish：协议异常，无物可归置
                };
                let on_disk = std::fs::metadata(&spool).ok().map(|m| m.len());
                if let (Some(disk), got) = (on_disk, bytes) {
                    if disk != got {
                        tracing::warn!(%session_id, disk, got, "落盘字节数与协议计数不符");
                    }
                }
                // 上一个文件的决策还没被消费（用户没答、下一个文件已收完）：
                // 按兜底策略先归置它——建议目录 + keep-both。**不能直接覆盖**，
                // 覆盖会让它的 spool 成为 .incoming 里再没人引用的孤儿，
                // 而用户那边看起来是「少了一个文件」。
                if let Some(stale) = tap.pending_finalize.lock().unwrap().take() {
                    let dir = tap
                        .session_download_dir
                        .lock()
                        .unwrap()
                        .clone()
                        .unwrap_or_else(|| default_download_dir(&tap.sandbox));
                    let _ = std::fs::create_dir_all(&dir);
                    if let Ok(Some((name, path, bytes))) = run_finalize(
                        &tap.session_download_dir,
                        &tap.finalized,
                        &session_id,
                        stale.clone(),
                        &dir,
                        None,
                        "keep-both",
                    ) {
                        emit_progress(
                            &app,
                            &session_id,
                            "saved",
                            &name,
                            Some(&path),
                            bytes,
                            Some(bytes),
                            "",
                        );
                    } else {
                        // 归置失败至少留痕；spool 还在 .incoming，可手工找回
                        tracing::warn!(%session_id, spool = %stale.spool.display(), "未决下载兜底归置失败");
                    }
                }
                *tap.pending_finalize.lock().unwrap() = Some(PendingFinalize {
                    spool: spool.clone(),
                    orig_name: orig_name.clone(),
                    bytes,
                });
                // 归置决策。设置读取要查库（async），这里正好在驱动任务里。
                decide_and_maybe_finalize(&app, &state, &tap, &session_id).await;
            }
            Effect::FileSent => {
                let Some(tap) = tap.upgrade() else { return };
                // 出队下一个或收尾。整段持锁：next_file 与换 source 必须原子，
                // 中途放开的话 filter() 会在「新 ZFILE 已发、旧句柄还在」的窗口里
                // 喂数据——喂的还是上一个文件的内容。
                let acts = {
                    let mut cur = tap.cur.lock().unwrap();
                    let Some(t) = cur.as_mut() else { continue };
                    match t.pending.pop_front() {
                        Some(next) => {
                            let name = next
                                .path
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("file")
                                .to_string();
                            let size = next.size;
                            t.source = next.file;
                            t.reported = 0;
                            t.file_index += 1;
                            let acts = t.session.next_file(&name, size);
                            emit_progress_full(
                                &app,
                                &session_id,
                                "sending",
                                &name,
                                None,
                                0,
                                Some(size),
                                "",
                                Some(t.file_index),
                                Some(t.file_count),
                                None,
                                None,
                                None,
                            );
                            acts
                        }
                        None => t.session.finish(),
                    }
                };
                tap.dispatch(acts);
            }
            Effect::NeedChunk { offset } => {
                let Some(tap) = tap.upgrade() else { return };
                let mut buf = vec![0u8; fs_terminal::zsession::SUBPACKET_BYTES];
                let acts = {
                    let mut cur = tap.cur.lock().unwrap();
                    let Some(t) = cur.as_mut() else { continue };
                    let n = match t.source.as_mut() {
                        None => 0,
                        Some(src) => {
                            // 按状态机给的偏移定位再读。顺序读在重传（ZRPOS 回退）
                            // 时会错位，且错位不报错——落地文件长度对、内容全错。
                            use std::io::Seek as _;
                            if let Err(e) = src.seek(std::io::SeekFrom::Start(offset)) {
                                tracing::warn!(%session_id, offset, error = %e, "ZMODEM 定位待发文件失败");
                                0
                            } else {
                                read_full(src, &mut buf)
                            }
                        }
                    };
                    t.session.feed_file_chunk(&buf[..n])
                };
                tap.dispatch(acts);
            }
            Effect::Done { ok, message } => {
                let Some(tap) = tap.upgrade() else { return };
                let mut cur = tap.cur.lock().unwrap();
                let path = cur.as_mut().and_then(|t| {
                    if let Some(mut sink) = t.sink.take() {
                        let _ = sink.flush();
                    }
                    t.sink_path.clone()
                });
                *cur = None;
                drop(cur);
                tap.busy.store(false, Ordering::SeqCst);
                // 复位「已通知」闩：下一次 rz 要能再弹一次选择框
                tap.need_file_announced.store(false, Ordering::SeqCst);
                tracing::info!(%session_id, ok, message, "ZMODEM 传输结束");
                emit_progress(
                    &app,
                    &session_id,
                    if ok { "done" } else { "failed" },
                    "",
                    path.as_deref(),
                    0,
                    None,
                    &message,
                );
            }
        }
    }
}

/// 面向用户显示的路径文本。
///
/// Windows 上 `resolve_within` 走 `canonicalize`，返回的是 verbatim 形式
/// `\\?\C:\Users\...`。那个前缀是给 Win32 API 看的（绕过 260 字符与路径规范化），
/// 直接摆到界面上用户只会以为程序出错了，复制去资源管理器地址栏也不一定认。
/// 落盘仍用原始 `PathBuf`（前缀有其用处，长路径要靠它），只有显示走这里剥掉。
fn display_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        // UNC 的 verbatim 形式是 `\\?\UNC\server\share`，剥掉前缀后要把
        // `UNC\` 还原成 `\\`，否则得到一个 `UNC\server\share` 的怪路径。
        Some(rest) => match rest.strip_prefix(r"UNC\") {
            Some(unc) => format!(r"\\{unc}"),
            None => rest.to_string(),
        },
        None => s.to_string(),
    }
}

/// 读满一块（`Read::read` 允许短读，直接用会把文件切成大量小子包）。
fn read_full(src: &mut std::io::BufReader<std::fs::File>, buf: &mut [u8]) -> usize {
    use std::io::Read as _;
    let mut n = 0;
    while n < buf.len() {
        match src.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    n
}

/// 在沙箱的 `.incoming/` 下建一个 spool 文件（两段式下载的第一段）。
///
/// **为什么先落 spool 再归置（2026-08-26 重设计）**：rz/sz 的协议超时是几十秒级，
/// 不能弹框等用户选目录再应答协议帧——用户想一下「存哪」，对端就超时了。
/// 所以协议侧照旧即时收（沙箱恒可写），收完再问；重命名/移动是本地操作，没有时限。
///
/// spool 名是 `recv-<纳秒>.part`：不含对端提供的任何字符（对端名字里的 `..`、
/// 分隔符、Windows 保留名在这里全部失效），真正的文件名在 finalize 时才清洗落定。
fn open_spool(sandbox: &Path) -> Result<(std::fs::File, PathBuf), String> {
    let incoming = sandbox.join(".incoming");
    std::fs::create_dir_all(&incoming).map_err(|e| format!("建暂存目录失败：{e}"))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dest = incoming.join(format!("recv-{nanos}.part"));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&dest)
        .map(|f| (f, dest))
        .map_err(|e| format!("建暂存文件失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fs-zbridge-{tag}-{:?}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// S339：起始帧到方向的映射不能反。
    ///
    /// 搞反的表现很隐蔽：远端 `rz` 在等文件，我们却开了个接收会话，双方各等对方
    /// 先说话，用户看到终端「卡住」直到 rz 超时——既不报错也没有任何提示。
    /// 我在初版实现里就是对两种起始帧一律开接收会话。
    #[test]
    fn start_frame_maps_to_the_right_direction() {
        use fs_terminal::zmodem::FrameKind as K;
        assert_eq!(
            classify_start(K::ZRQINIT),
            Some(Detected::Receive),
            "ZRQINIT = 远端 sz 要发文件 → 我们接收"
        );
        assert_eq!(
            classify_start(K::ZRINIT),
            Some(Detected::NeedLocalFile),
            "ZRINIT = 远端 rz 在等文件 → 要先问人选哪个文件"
        );
        // 其余帧不猜方向
        for k in [K::ZFILE, K::ZDATA, K::ZFIN, K::ZACK, K::ZNAK] {
            assert_eq!(classify_start(k), None, "{k:?} 不该被当作起始帧分类");
        }
    }

    /// S339：`detect_start` 认的帧集合与 `classify_start` 能分类的集合一致。
    ///
    /// 两处一旦分叉，就会出现「检出了但不知道该干什么」的字节——那时透传是唯一
    /// 安全选择，但更该做的是不让它们分叉。
    #[test]
    fn every_detectable_start_frame_is_classifiable() {
        use fs_terminal::zmodem::{FrameHeader, FrameKind as K};
        for k in [K::ZRQINIT, K::ZRINIT] {
            let bytes = FrameHeader {
                kind: k,
                flags: [0; 4],
            }
            .to_hex();
            let (got, _at) = fs_terminal::zmodem::detect_start(&bytes)
                .unwrap_or_else(|| panic!("{k:?} 须被 detect_start 认出"));
            assert!(
                classify_start(got).is_some(),
                "detect_start 认出 {got:?} 却无法分类——两处判定已分叉"
            );
        }
    }

    /// S338（2026-08-26 迁到归置侧）：keep-both 下同名不覆盖，改名避让。
    ///
    /// 覆盖是静默数据丢失：用户传第二个同名文件，第一个就没了，而 UI 上两次都
    /// 显示「成功」。旧 open_sink 的这条判定随逻辑整体迁到 apply_policy。
    #[test]
    fn same_name_does_not_overwrite() {
        let dir = scratch("dup");
        std::fs::write(dir.join("a.txt"), b"original").unwrap();
        let spool = dir.join(".spool-source");
        std::fs::write(&spool, b"new").unwrap();
        let p = apply_policy(&spool, &dir, "a.txt", ConflictPolicy::KeepBoth).unwrap();
        assert_eq!(p.file_name().unwrap().to_str().unwrap(), "a (1).txt");
        assert_eq!(
            std::fs::read(dir.join("a.txt")).unwrap(),
            b"original",
            "原文件内容不得被动过"
        );
        assert_eq!(std::fs::read(&p).unwrap(), b"new", "新内容落到避让名上");

        let spool2 = dir.join(".spool-source2");
        std::fs::write(&spool2, b"new2").unwrap();
        let p2 = apply_policy(&spool2, &dir, "a.txt", ConflictPolicy::KeepBoth).unwrap();
        assert_eq!(p2.file_name().unwrap().to_str().unwrap(), "a (2).txt");
    }

    /// overwrite 真的覆盖：名字不变、内容换新。
    #[test]
    fn overwrite_replaces_content_under_the_same_name() {
        let dir = scratch("ow");
        std::fs::write(dir.join("a.txt"), b"old").unwrap();
        let spool = dir.join(".spool-ow");
        std::fs::write(&spool, b"new").unwrap();
        let p = apply_policy(&spool, &dir, "a.txt", ConflictPolicy::Overwrite).unwrap();
        assert_eq!(
            p.file_name().unwrap().to_str().unwrap(),
            "a.txt",
            "覆盖不改名"
        );
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
        assert!(!spool.exists(), "spool 应已被移走，不得留双份");
    }

    /// 无扩展名与多点名的改名形态（随 S338 迁移）。
    #[test]
    fn rename_keeps_extension_shape() {
        let dir = scratch("ext");
        std::fs::write(dir.join("noext"), b"x").unwrap();
        let spool = dir.join(".spool-a");
        std::fs::write(&spool, b"y").unwrap();
        let p = apply_policy(&spool, &dir, "noext", ConflictPolicy::KeepBoth).unwrap();
        assert_eq!(p.file_name().unwrap().to_str().unwrap(), "noext (1)");

        std::fs::write(dir.join("a.tar.gz"), b"x").unwrap();
        let spool2 = dir.join(".spool-b");
        std::fs::write(&spool2, b"y").unwrap();
        let p2 = apply_policy(&spool2, &dir, "a.tar.gz", ConflictPolicy::KeepBoth).unwrap();
        assert_eq!(
            p2.file_name().unwrap().to_str().unwrap(),
            "a.tar (1).gz",
            "只按最后一个点切分（与资源管理器一致）"
        );
    }

    /// 归置文件名清洗（随 S338 迁移）。
    #[test]
    fn final_name_is_sanitized_and_path_components_rejected() {
        let safe = sanitize_final_name("we:ird?name*.txt").unwrap();
        assert!(
            !safe.contains(':') && !safe.contains('?') && !safe.contains('*'),
            "非法字符须被清洗，实得 {safe:?}"
        );
        // 空名落到占位名，不造隐藏怪文件
        assert_eq!(sanitize_final_name("").unwrap(), "received");
        // **路径成分是拒绝，不是清洗**：清洗一个 ".." 出来一个别的名字，
        // 用户找不到也理解不了他的文件去了哪。
        assert!(sanitize_final_name("..").is_err());
        assert!(sanitize_final_name("a/b.txt").is_err());
        assert!(sanitize_final_name("a\\b.txt").is_err());
    }

    /// S338：面向用户的路径文本不带 `\\?\` 前缀。
    ///
    /// 落盘要用 verbatim 形式（长路径靠它），但把 `\\?\C:\...` 摆到界面上，
    /// 用户只会以为程序出错了，复制去资源管理器也不一定认。
    #[test]
    fn display_path_strips_windows_verbatim_prefix() {
        assert_eq!(
            display_path(Path::new(r"\\?\C:\Users\me\downloads\a.txt")),
            r"C:\Users\me\downloads\a.txt"
        );
        // UNC 的 verbatim 形式要还原成 `\\server\share`，不能剩一个 `UNC\` 前缀
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\a.txt")),
            r"\\server\share\a.txt"
        );
        // 普通路径原样返回（含 Unix 形态）
        assert_eq!(display_path(Path::new("/home/me/a.txt")), "/home/me/a.txt");
        assert_eq!(display_path(Path::new(r"C:\a\b.txt")), r"C:\a\b.txt");
    }

    /// S338：真实归置路径经过 `display_path` 后不含 verbatim 前缀
    /// （上一条钉的是纯函数，这条钉的是它确实接在了实际路径上）。
    #[test]
    fn real_sink_path_displays_without_prefix() {
        let dir = scratch("disp");
        let spool = dir.join(".spool");
        std::fs::write(&spool, b"x").unwrap();
        let p = apply_policy(&spool, &dir, "shown.txt", ConflictPolicy::KeepBoth).unwrap();
        let shown = display_path(&p);
        assert!(
            !shown.starts_with(r"\\?\"),
            "显示用路径不得带 verbatim 前缀，实得 {shown:?}"
        );
        assert!(shown.ends_with("shown.txt"), "实得 {shown:?}");
    }

    /// **skip 策略：spool 被删、目标目录不出现该文件。**
    ///
    /// 「跳过」的用户语义是「不要这个文件」。把它改成「也落过去」的变异
    /// 必须在这里转红——那等于把用户明确丢弃的东西写进他的下载目录。
    #[test]
    fn skip_deletes_the_spool_and_lands_nothing() {
        let dir = scratch("skip");
        let spool = dir.join(".incoming").join("recv-1.part");
        std::fs::create_dir_all(spool.parent().unwrap()).unwrap();
        std::fs::write(&spool, b"payload").unwrap();
        let sd = Mutex::new(None::<PathBuf>);
        let fin = Mutex::new(std::collections::HashSet::<PathBuf>::new());
        let out = run_finalize(
            &sd,
            &fin,
            "t",
            PendingFinalize {
                spool: spool.clone(),
                orig_name: "a.txt".into(),
                bytes: 7,
            },
            &dir,
            None,
            "skip",
        )
        .unwrap()
        .expect("skip 也算一次完成（字节计 0）");
        assert_eq!(out.2, 0, "skip 的字节数为 0");
        assert!(!spool.exists(), "spool 应被删除");
        assert!(
            !dir.join("a.txt").exists(),
            "skip 不得把文件落到目标目录——用户明确不要它"
        );
        assert!(
            fin.lock().unwrap().is_empty(),
            "skip 的路径不得进 reveal 白名单（没有这个文件可 reveal）"
        );
    }

    /// spool 名不含对端提供的任何字符：`..`、分隔符、保留名在 spool 阶段全部无效。
    #[test]
    fn spool_name_is_generated_not_derived_from_peer() {
        let dir = scratch("spoolname");
        let (_f, p) = open_spool(&dir).unwrap();
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("recv-") && name.ends_with(".part"),
            "spool 名应是 recv-<纳秒>.part，实得 {name:?}"
        );
        assert!(
            p.starts_with(dir.join(".incoming")),
            "spool 应落在 .incoming/ 下"
        );
    }

    /// 归置后：目录决定被记住（同会话后续文件不再问去向），
    /// 且最终路径进了 reveal 白名单。
    #[test]
    fn finalize_records_session_dir_and_reveal_whitelist() {
        let dir = scratch("record");
        let spool = dir.join(".spool");
        std::fs::write(&spool, b"x").unwrap();
        let sd = Mutex::new(None::<PathBuf>);
        let fin = Mutex::new(std::collections::HashSet::<PathBuf>::new());
        let (name, path, bytes) = run_finalize(
            &sd,
            &fin,
            "t",
            PendingFinalize {
                spool,
                orig_name: "rec.bin".into(),
                bytes: 1,
            },
            &dir,
            None,
            "keep-both",
        )
        .unwrap()
        .unwrap();
        assert_eq!(name, "rec.bin");
        assert_eq!(bytes, 1);
        assert_eq!(
            sd.lock().unwrap().as_deref(),
            Some(dir.as_path()),
            "归置目录应被记住为本会话的下载目录"
        );
        assert!(
            fin.lock().unwrap().contains(&path),
            "最终路径应进 reveal 白名单"
        );
    }

    /// S338：`read_full` 补齐短读——否则一次短读就把文件切成大量小子包，
    /// 传输速率会掉一个数量级。
    #[test]
    fn read_full_fills_the_buffer_across_short_reads() {
        let dir = scratch("read");
        let path = dir.join("blob.bin");
        let payload: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        std::fs::write(&path, &payload).unwrap();
        // BufReader 容量刻意小于目标块，制造短读
        let mut src = std::io::BufReader::with_capacity(7, std::fs::File::open(&path).unwrap());
        let mut buf = vec![0u8; 1024];
        let n = read_full(&mut src, &mut buf);
        assert_eq!(n, 1024, "须读满整块（容量 7 的 BufReader 会逐次短读）");
        assert_eq!(&buf[..n], &payload[..1024]);
        // 末块短于目标：返回实际字节数，不得死循环
        let mut src2 = std::io::BufReader::new(std::fs::File::open(&path).unwrap());
        let mut big = vec![0u8; 5000];
        assert_eq!(
            read_full(&mut src2, &mut big),
            4096,
            "文件末尾返回实读字节数"
        );
    }
}

// ───────────────── 下载两段式：归置决策与执行（2026-08-26 重设计） ─────────────────

/// 下载去向的出厂建议：系统下载目录下的 FutureShell 子目录；取不到下载目录
/// （极少见的无 HOME 环境）回落本会话沙箱——那里**一定**可写，因为协议侧刚刚
/// 就是在它里面收的。
fn default_download_dir(sandbox: &Path) -> PathBuf {
    dirs::download_dir()
        .map(|d| d.join("FutureShell"))
        .unwrap_or_else(|| sandbox.to_path_buf())
}

/// 收完一个文件后的四路归置决策。
///
/// 优先级：**本会话已定的目录 > 设置（fixed） > 引导/询问**。会话目录在第一次
/// finalize 时定格——一次 `sz` 批量传多个文件，第二个起不再问去向（冲突仍逐个问）。
/// 冲突判定在「要不要问」之后：fixed/会话目录且无冲突 → 直接落，不打扰。
async fn decide_and_maybe_finalize(
    app: &AppHandle,
    state: &std::sync::Arc<crate::state::AppState>,
    tap: &ZmodemTap,
    session_id: &str,
) {
    let cfg = state.zmodem_config().await;
    // 优先级 ①②：本会话已定 > 设置的 fixed 模式。
    // 两步分开写而不是 `.clone().or_else(…再锁同一把锁…)`：后者的外层
    // 临时守卫活到整条语句结束，or_else 里再锁同一把 std Mutex 就是自死锁。
    let from_setting = cfg.download.as_ref().and_then(|d| {
        (d.mode == crate::state::ZmodemDownloadMode::Fixed).then(|| PathBuf::from(&d.dir))
    });
    let dir: Option<PathBuf> = match tap.session_download_dir.lock().unwrap().clone() {
        Some(d) => Some(d),
        None => {
            // 定格为会话目录（取设置值；None 则保持 None = 要走引导/询问）
            let next = from_setting.clone();
            *tap.session_download_dir.lock().unwrap() = next.clone();
            next
        }
    };

    let Some(pending) = tap.pending_finalize.lock().unwrap().clone() else {
        return;
    };
    // 展示名沿用旧避让逻辑的清洗（不改变盘上事实，只让文案里不出现裸控制字符）
    let display_name = fs_sshengine::sandbox::escape_local_component(&pending.orig_name);
    let display_name = if display_name.trim().is_empty() {
        "received".to_string()
    } else {
        display_name
    };

    let Some(dir) = dir else {
        // 优先级 ③：没有去向决定——首次（无设置）或每次询问。
        let reason = if cfg.download.is_none() {
            "first-run"
        } else {
            "choose-dir"
        };
        let suggested = default_download_dir(&tap.sandbox);
        emit_progress_full(
            app,
            session_id,
            "needs-decision",
            &display_name,
            None,
            pending.bytes,
            Some(pending.bytes),
            reason,
            None,
            None,
            Some(&suggested.display().to_string()),
            Some(&pending.spool.display().to_string()),
            None,
        );
        return;
    };

    // 有去向：看冲突。目录本身不存在不打断——归置时建（用户挑过的目录可能
    // 只是还没建出来，比如 Downloads/FutureShell 首次使用）。
    let target = dir.join(&display_name);
    if !target.exists() {
        // 无冲突：直接归置，不打扰
        let (name, path, bytes) = match finalize_download(tap, session_id, &dir, None, "keep-both")
        {
            Ok(Some((n, p, b))) => (n, p, b),
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(%session_id, error = %e, "ZMODEM 直接归置失败");
                emit_progress(app, session_id, "failed", &display_name, None, 0, None, &e);
                return;
            }
        };
        emit_progress(
            app,
            session_id,
            "saved",
            &name,
            Some(&path),
            bytes,
            Some(bytes),
            "",
        );
        return;
    }

    // 冲突：问。带上建议目录与冲突标记；spool 路径让前端在「关闭=兜底归置」时回传场景使用。
    emit_progress_full(
        app,
        session_id,
        "needs-decision",
        &display_name,
        None,
        pending.bytes,
        Some(pending.bytes),
        "conflict",
        None,
        None,
        Some(&dir.display().to_string()),
        Some(&pending.spool.display().to_string()),
        Some(true),
    );
}

/// 归置的冲突策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    Overwrite,
    KeepBoth,
    Skip,
}

impl ConflictPolicy {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "overwrite" => Ok(Self::Overwrite),
            "keep-both" => Ok(Self::KeepBoth),
            "skip" => Ok(Self::Skip),
            other => Err(format!(
                "未知的冲突处理方式 {other:?}（应为 overwrite/keep-both/skip）"
            )),
        }
    }
}

/// 执行一次归置。消费 `pending_finalize`；返回 `Ok(Some((名字, 最终路径, 字节数)))`。
/// `Ok(None)` = 没有待归置的下载（不是错误——前端重复确认时会发生）。
///
/// 这是 `zmodem_finalize` 命令与「无冲突直接落」共用的唯一实现：两份归置逻辑
/// 分叉的那一侧，就是能把文件挪到任意地方的那一侧。
fn finalize_download(
    tap: &ZmodemTap,
    session_id: &str,
    dir: &Path,
    name_override: Option<&str>,
    policy: &str,
) -> Result<Option<(String, PathBuf, u64)>, String> {
    let Some(pending) = tap.pending_finalize.lock().unwrap().take() else {
        return Ok(None);
    };
    run_finalize(
        &tap.session_download_dir,
        &tap.finalized,
        session_id,
        pending,
        dir,
        name_override,
        policy,
    )
}

/// [`finalize_download`] 的核心：归置一个**显式给定**的待决下载。
/// 单独存在是因为「上一个决策未答、下一个文件已收完」的兜底路径手里已经有
/// PendingFinalize，不能再走 take（那会拿到刚放进去的新值或 None）。
///
/// 收锁引用而不是 `&ZmodemTap`：这样单测能直接构造两个 `Mutex` 把 skip/keep-both
/// 全路径钉住，不必起一个真 Tauri 应用。
fn run_finalize(
    session_dir: &Mutex<Option<PathBuf>>,
    finalized: &Mutex<std::collections::HashSet<PathBuf>>,
    session_id: &str,
    pending: PendingFinalize,
    dir: &Path,
    name_override: Option<&str>,
    policy: &str,
) -> Result<Option<(String, PathBuf, u64)>, String> {
    let policy = ConflictPolicy::parse(policy)?;
    // 目录不存在则建（出厂建议 Downloads/FutureShell 首次使用时还没有）；
    // 建完再验「确实是目录」——建出来的是目录，已存在的是文件则拒绝。
    // 不限制在浏览根内：下载去向是用户的自由，与上传源（必须在家目录内）
    // 方向相反；但名字要清洗且拒绝任何路径成分。
    std::fs::create_dir_all(dir).map_err(|e| format!("建目标目录失败：{e}"))?;
    let meta = std::fs::metadata(dir).map_err(|e| format!("读不到目标目录：{e}"))?;
    if !meta.is_dir() {
        return Err(format!("{} 不是目录", dir.display()));
    }
    let raw_name = name_override
        .map(str::to_string)
        .unwrap_or(pending.orig_name);
    let safe = sanitize_final_name(&raw_name)?;

    // 记住本会话的目录决定（后续文件不再问去向）
    *session_dir.lock().unwrap() = Some(dir.to_path_buf());

    match policy {
        ConflictPolicy::Skip => {
            // 跳过 = 删 spool。用户明确不要这个文件；留着只是 .incoming 里的垃圾。
            let _ = std::fs::remove_file(&pending.spool);
            tracing::info!(%session_id, name = %safe, "ZMODEM 下载被跳过，暂存已清理");
            let p = dir.join(&safe);
            Ok(Some((safe, p, 0)))
        }
        ConflictPolicy::Overwrite | ConflictPolicy::KeepBoth => {
            let dest = apply_policy(&pending.spool, dir, &safe, policy)?;
            finalized.lock().unwrap().insert(dest.clone());
            let final_name = dest
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&safe)
                .to_string();
            Ok(Some((final_name, dest, pending.bytes)))
        }
    }
}

/// 最终文件名的清洗与路径成分拒绝（纯函数，供单测钉）。
///
/// ① 拒绝任何含分隔符或「..」的名字——escape_local_component 管非法字符，
///    管不了把 ".." 当整个名字用的写法；
/// ② 清洗 Windows 非法字符与控制字符；
/// ③ 空/纯点名字落到占位名 `received`。
fn sanitize_final_name(raw: &str) -> Result<String, String> {
    if raw.contains('/') || raw.contains('\\') || raw == ".." {
        return Err(format!("文件名 {raw:?} 含路径成分，已拒绝"));
    }
    let safe = fs_sshengine::sandbox::escape_local_component(raw);
    Ok(if safe.trim().is_empty() {
        "received".to_string()
    } else {
        safe
    })
}

/// 按冲突策略把 spool 落到 `dir/safe`（纯文件操作，供单测钉）。
fn apply_policy(
    spool: &Path,
    dir: &Path,
    safe: &str,
    policy: ConflictPolicy,
) -> Result<PathBuf, String> {
    match policy {
        ConflictPolicy::Overwrite => {
            let dest = dir.join(safe);
            if dest.exists() {
                std::fs::remove_file(&dest).map_err(|e| format!("移除旧文件失败：{e}"))?;
            }
            move_spool(spool, &dest)?;
            Ok(dest)
        }
        // 同名避让：沿用旧 open_sink 的 (n) 序列。create_new 不可用（目标在
        // 沙箱外），用 exists 探测——这里没有并发写者（同一会话串行归置）。
        ConflictPolicy::KeepBoth => {
            let (stem, ext) = match safe.rsplit_once('.') {
                Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
                _ => (safe.to_string(), String::new()),
            };
            for n in 0..1000u32 {
                let candidate = if n == 0 {
                    format!("{stem}{ext}")
                } else {
                    format!("{stem} ({n}){ext}")
                };
                let dest = dir.join(&candidate);
                if dest.exists() {
                    continue;
                }
                move_spool(spool, &dest)?;
                return Ok(dest);
            }
            Err("同名文件过多（已试 1000 个候选名）".into())
        }
        ConflictPolicy::Skip => unreachable!("skip 在调用方分支处理，不落文件"),
    }
}

/// 移动 spool 到最终位置：先试 rename（同盘零拷贝），失败（跨盘/权限）回落
/// copy + delete。copy 的删除在 copy 成功之后——半路失败时宁可留双份也不留零份。
fn move_spool(spool: &Path, dest: &Path) -> Result<(), String> {
    if std::fs::rename(spool, dest).is_ok() {
        return Ok(());
    }
    std::fs::copy(spool, dest).map_err(|e| format!("复制到 {} 失败：{e}", dest.display()))?;
    let _ = std::fs::remove_file(spool);
    Ok(())
}
