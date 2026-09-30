//! ZMODEM 会话状态机（M4a：rz/sz 的收发驱动）。
//!
//! 协议原语在 [`crate::zmodem`]；本模块把它们串成两条流程，并且**只做纯状态推进**：
//! 喂入对端字节 → 产出「要回给对端的字节」+「要落盘/要读取的动作」。文件 IO 与
//! SSH 写入归 app 层。
//!
//! 这样切分的理由与 sessionlog 同源：ZMODEM 的坑在字节层（转义、CRC、切块），
//! 把 IO 混进来就只能靠端到端跑真容器才能测，而那种测试跑一次要几十秒、失败时
//! 还分不清是协议错还是网络抖。现在每条判定都能用一个 `&[u8]` 离线钉住。
//!
//! ## 状态机的两条流程
//!
//! **接收（远端 `sz`）**：对端发 ZRQINIT → 我们回 ZRINIT → 对端 ZFILE（文件名/大小）
//! → 我们回 ZRPOS(0) → 对端 ZDATA + 子包流 → 我们攒盘 → 对端 ZEOF → 我们回 ZRINIT
//! → 对端 ZFIN → 我们回 ZFIN 并结束。
//!
//! **发送（远端 `rz`）**：对端发 ZRINIT → 我们发 ZFILE → 对端回 ZRPOS → 我们发
//! ZDATA + 子包流 → 发 ZEOF → 对端回 ZRINIT → 我们发 ZFIN → 对端回 ZFIN，结束。

use crate::zmodem::{
    cancel_sequence, encode_subpacket, parse_file_info, parse_header, parse_subpacket, FileInfo,
    FrameHeader, FrameKind, Parsed, Parsed2, SubpacketEnd,
};

/// 单个数据子包的载荷上限（zmodem 惯例 1 KiB；再大对端未必吃得下）。
pub const SUBPACKET_BYTES: usize = 1024;

/// 接收侧的单文件上限（防「对端声称 8 EiB」把盘写满）。
/// 4 GiB 远超终端里手动 sz 的合理用量；更大的文件应该走 SFTP 面板。
pub const MAX_RECV_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// 状态机产出的动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 把这些字节写回对端（SSH 通道）
    Send(Vec<u8>),
    /// 开始接收一个文件（app 层据此建文件；名字已基名化）
    BeginRecv(FileInfo),
    /// 追加数据到当前接收文件
    Write(Vec<u8>),
    /// 当前文件接收完成（偏移 = 期望的总字节数，app 层可与实际落盘量比对）
    FinishRecv { bytes: u64 },
    /// 需要读取待发文件的下一块（app 层填 `feed_file_chunk`）
    NeedChunk { offset: u64 },
    /// 发送侧：当前文件已被对端收妥（ZEOF 后收到 ZRINIT）。
    ///
    /// app 层据此二选一：队列里还有 → [`ZmodemSession::next_file`]；
    /// 没有了 → [`ZmodemSession::finish`]。这个决定必须由 app 层做，
    /// 因为状态机不碰文件系统、不知道队列还剩什么。
    FileSent,
    /// 整个会话结束（正常完成或被取消）
    Done { ok: bool, message: String },
}

/// 会话方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// 远端 sz：我们收
    Receive,
    /// 远端 rz：我们发
    Send,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// 等对端的 init/ZFILE
    Handshake,
    /// 已收到 ZFILE 帧头，正在等紧随其后的文件信息子包。
    ///
    /// 这个状态必须显式存在：SSH 读块会把 ZFILE 头和它的信息子包切开，
    /// 而「子包不全就把帧头拼回缓冲区重放」的写法只在「重放出的头不比原头长」
    /// 时才收敛——那是编码长度的巧合，不是不变量。显式状态则不消耗任何字节，
    /// 循环靠「未消耗即退出」自然停下。
    AwaitFileInfo,
    /// 收数据中
    Receiving,
    /// 发数据中
    Sending,
    Finished,
}

/// ZMODEM 会话。
pub struct ZmodemSession {
    dir: Direction,
    phase: Phase,
    buf: Vec<u8>,
    /// 当前文件已收/已发字节数
    offset: u64,
    /// 接收侧：对端声明的大小（用于进度与上限判定）
    declared: Option<u64>,
    /// 发送侧：待发文件名与大小
    out_name: String,
    out_size: u64,
    /// 发送侧：当前 ZDATA 帧是否还开着（尚未发出 ZCRCE 帧尾子包）。
    ///
    /// 必须记这一位：ZMODEM 的数据帧要用 ZCRCE（或 ZCRCW）子包**显式收尾**，
    /// 之后才允许出现下一个帧头。一路 ZCRCG 流到底然后直接拍一个 ZEOF 上去是
    /// 协议违规——真实的 `rz` 会重新同步并回一个 `ZRPOS(已收字节数)`，看起来像
    /// 「对端要求断点续传」，与真实原因（帧没收尾）毫无关系。
    frame_open: bool,
    /// **当前文件**的 ZEOF 已发出（随后收到对端 ZRINIT 即表示收妥）。
    ///
    /// 用显式标志而不是「offset > 0」判，是因为 0 字节文件：它发完 ZEOF 时
    /// offset 仍是 0，旧判据会把对端「收妥」的 ZRINIT 误认成**初始** ZRINIT，
    /// 于是把同一个文件再发一遍——多文件队列里 0 字节文件会变成原地重发。
    eof_sent: bool,
    /// 当前文件的 ZFILE 已发出。
    ///
    /// 对端（真 rz）在会话起步时会把 ZRINIT **连发两遍**：一遍是它自己的启动
    /// 通告，一遍是对我们 ZRQINIT 的应答，且常常落在同一个 SSH 读块里。没有
    /// 这个闩锁的话，状态机会对每个 ZRINIT 都回一个 ZFILE——同一个文件的
    /// ZFILE 发两遍，对端把第二遍当成「下一个文件」重新打开（截断同名文件），
    /// 落地内容全错。单文件传输恰好掩盖了它（对端把多余的那个 ZFILE 当噪声
    /// 丢掉或重新收敛），多文件第一次真跑就现形。
    zfile_sent: bool,
    /// 数据帧用 CRC-32（握手后固定：我们发的 ZRINIT/ZFILE 都声明支持）
    crc32_mode: bool,
}

impl ZmodemSession {
    /// 新建接收会话（远端 `sz`）。
    pub fn new_receive() -> Self {
        Self {
            dir: Direction::Receive,
            phase: Phase::Handshake,
            buf: Vec::new(),
            offset: 0,
            declared: None,
            out_name: String::new(),
            out_size: 0,
            frame_open: false,
            eof_sent: false,
            zfile_sent: false,
            crc32_mode: true,
        }
    }

    /// 新建发送会话（远端 `rz`）：`name` 会被基名化后发给对端。
    pub fn new_send(name: &str, size: u64) -> Self {
        let base = name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("file")
            .trim()
            .to_string();
        Self {
            dir: Direction::Send,
            phase: Phase::Handshake,
            buf: Vec::new(),
            offset: 0,
            declared: None,
            out_name: if base.is_empty() { "file".into() } else { base },
            out_size: size,
            frame_open: false,
            eof_sent: false,
            zfile_sent: false,
            crc32_mode: true,
        }
    }

    pub fn direction(&self) -> Direction {
        self.dir
    }

    pub fn is_finished(&self) -> bool {
        self.phase == Phase::Finished
    }

    /// 已传输字节数（进度显示）。
    pub fn transferred(&self) -> u64 {
        self.offset
    }

    /// 对端声明的文件大小（接收侧；发送侧为自己的大小）。
    pub fn declared_size(&self) -> Option<u64> {
        match self.dir {
            Direction::Receive => self.declared,
            Direction::Send => Some(self.out_size),
        }
    }

    /// 会话启动时要主动发出的字节。
    ///
    /// 接收侧发 ZRINIT（告诉对端「可以发了，我支持 CRC-32」）；发送侧发 ZRQINIT
    /// （请求对端进入接收态）——注意**发送侧也要发**：远端 `rz` 已经在等，但它可能
    /// 先发了 ZRINIT 而我们尚未就绪，一次 ZRQINIT 让双方对齐。
    pub fn start(&self) -> Vec<u8> {
        match self.dir {
            Direction::Receive => zrinit(),
            Direction::Send => FrameHeader {
                kind: FrameKind::ZRQINIT,
                flags: [0; 4],
            }
            .to_hex(),
        }
    }

    /// 喂入对端字节，推进状态机。
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Action> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            if self.phase == Phase::Finished {
                break;
            }
            let before = self.buf.len();
            match self.phase {
                Phase::Receiving => self.step_receiving(&mut out),
                Phase::AwaitFileInfo => self.step_file_info(&mut out),
                _ => self.step_frames(&mut out),
            }
            // 没有消耗任何字节 = 需要更多输入，退出循环等下一块
            if self.buf.len() == before {
                break;
            }
        }
        out
    }

    /// 发送侧：app 层把读到的文件块喂进来（对应 [`Action::NeedChunk`]）。
    /// `data` 为空 = 文件已读完 → 收尾当前数据帧并发 ZEOF。
    pub fn feed_file_chunk(&mut self, data: &[u8]) -> Vec<Action> {
        if self.phase != Phase::Sending {
            return Vec::new();
        }
        let eof = FrameHeader {
            kind: FrameKind::ZEOF,
            flags: (self.offset as u32).to_le_bytes(),
        };
        if data.is_empty() {
            self.eof_sent = true;
            self.phase = Phase::Handshake; // 等对端对 ZEOF 的回应
            let mut acts = Vec::new();
            // 数据帧必须显式收尾才能发下一个帧头。走到这里而帧还开着，说明文件
            // 比声明的大小短（文件被人动过、或声明本身不准）——补一个**空的**
            // ZCRCE 子包收尾。空子包是合法的，而不收尾就是协议违规。
            if self.frame_open {
                acts.push(Action::Send(encode_subpacket(
                    &[],
                    SubpacketEnd::EndFrame,
                    self.crc32_mode,
                )));
                self.frame_open = false;
            }
            acts.push(Action::Send(eof.to_hex()));
            return acts;
        }
        // 最后一块用 ZCRCE 收尾帧，其余用 ZCRCG（不要求逐包应答，吞吐才上得去）。
        // 「最后一块」按声明大小判：这是唯一能在**发出这一块时**就知道的判据，
        // 而帧尾必须跟在最后一块数据后面，等到下一次调用才发就晚了。
        let last = self.offset + data.len() as u64 >= self.out_size;
        let end = if last {
            SubpacketEnd::EndFrame
        } else {
            SubpacketEnd::NoAck
        };
        let pkt = encode_subpacket(data, end, self.crc32_mode);
        self.offset += data.len() as u64;
        if last {
            // 帧已收尾，紧接着发 ZEOF——不再多问一次「还有没有下一块」。
            self.eof_sent = true;
            self.frame_open = false;
            self.phase = Phase::Handshake; // 等对端对 ZEOF 的回应
            let eof_at_end = FrameHeader {
                kind: FrameKind::ZEOF,
                flags: (self.offset as u32).to_le_bytes(),
            };
            return vec![Action::Send(pkt), Action::Send(eof_at_end.to_hex())];
        }
        vec![
            Action::Send(pkt),
            Action::NeedChunk {
                offset: self.offset,
            },
        ]
    }

    /// 多文件发送：切换到下一个文件并发它的 ZFILE。
    ///
    /// 只在收到 [`Action::FileSent`] 之后调（即上一个文件已被对端收妥）。
    /// 重置的是**单文件**状态（offset / eof_sent / frame_open）；CRC 宽度不重置
    /// ——会话级协商结果对整个会话有效。
    pub fn next_file(&mut self, name: &str, size: u64) -> Vec<Action> {
        let base = name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("file")
            .trim()
            .to_string();
        self.out_name = if base.is_empty() { "file".into() } else { base };
        self.out_size = size;
        self.offset = 0;
        self.eof_sent = false;
        self.zfile_sent = true; // 下一个 ZFILE 由本方法自己发出
        self.frame_open = false;
        self.phase = Phase::Handshake; // 等 ZRPOS
        vec![Action::Send(self.zfile())]
    }

    /// 多文件发送：队列已空，发 ZFIN 收尾整个会话。
    pub fn finish(&mut self) -> Vec<Action> {
        self.phase = Phase::Finished;
        vec![
            Action::Send(
                FrameHeader {
                    kind: FrameKind::ZFIN,
                    flags: [0; 4],
                }
                .to_hex(),
            ),
            Action::Done {
                ok: true,
                message: "发送完成".into(),
            },
        ]
    }

    /// 取走会话结束后残留在缓冲区里的字节。
    ///
    /// ZFIN 之后对端通常紧跟 `OO`（over-and-out）与 shell 提示符，而这些字节是在
    /// 同一个读块里到达的。传输拦截器在会话活跃期间吞掉全部字节，若不把残留交还
    /// 终端，用户传完文件会看到「光标停在空行、提示符不见了」——按一下回车才回来。
    /// 那种表现极易被当成卡死。
    pub fn take_remainder(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }

    /// 主动取消（用户点「取消」）。
    pub fn cancel(&mut self) -> Vec<Action> {
        self.phase = Phase::Finished;
        vec![
            Action::Send(cancel_sequence()),
            Action::Done {
                ok: false,
                message: "已取消".into(),
            },
        ]
    }

    /// 解析帧头并按方向推进。
    ///
    /// 解析成功时**先按帧头格式更新数据子包的 CRC 宽度**再分派：宽度跟着启动该帧
    /// 的帧头走（`ZBIN`→CRC-16、`ZBIN32`→CRC-32），写死成 CRC-32 遇到用 `ZBIN`
    /// 发数据的对端会把每个子包判成 CRC 错——握手全对、随即无限重传。
    ///
    /// 帧前噪声（对端在帧之间夹的 CR/LF/XON、`sz` 启动前的终端回显）不需要单独
    /// 跳：`parse_header` 对「首字节不是 ZPAD」返回 `Bad(1)`，下面的 `Bad` 分支
    /// 丢掉这一字节，外层循环再来一轮。曾经在这里放过一个额外的跳噪声循环，
    /// 变异测试证明它杀不掉——两套机制做同一件事，删掉短的那套。
    fn step_frames(&mut self, out: &mut Vec<Action>) {
        match parse_header(&self.buf) {
            Parsed::Incomplete => {}
            Parsed::Bad(used) => {
                // 这里的 drain 是噪声推进的唯一动力：改成不消耗就会死循环在噪声上
                self.buf.drain(..used.min(self.buf.len()));
            }
            Parsed::Header(h, used) => {
                // 数据帧（ZDATA / ZFILE）之后紧跟子包，其 CRC 宽度由本帧头格式决定。
                // 控制帧（ZRINIT/ZACK/…）恒用十六进制头，不该把宽度带回 16 位。
                if matches!(h.kind, FrameKind::ZDATA | FrameKind::ZFILE) {
                    if let Some(c32) = crate::zmodem::header_crc32(&self.buf[..used]) {
                        self.crc32_mode = c32;
                    }
                }
                self.buf.drain(..used);
                self.on_header(h, out);
            }
        }
    }

    fn on_header(&mut self, h: FrameHeader, out: &mut Vec<Action>) {
        match (self.dir, h.kind) {
            // ── 接收侧 ──
            (Direction::Receive, FrameKind::ZRQINIT) => out.push(Action::Send(zrinit())),
            // 文件信息在紧随其后的子包里，可能还没到齐 → 切到显式等待态
            (Direction::Receive, FrameKind::ZFILE) => {
                self.phase = Phase::AwaitFileInfo;
                self.step_file_info(out);
            }
            (Direction::Receive, FrameKind::ZDATA) => {
                self.phase = Phase::Receiving;
            }
            (Direction::Receive, FrameKind::ZEOF) => {
                out.push(Action::FinishRecv { bytes: self.offset });
                out.push(Action::Send(zrinit()));
            }
            (Direction::Receive, FrameKind::ZFIN) => {
                self.phase = Phase::Finished;
                out.push(Action::Send(
                    FrameHeader {
                        kind: FrameKind::ZFIN,
                        flags: [0; 4],
                    }
                    .to_hex(),
                ));
                out.push(Action::Done {
                    ok: true,
                    message: "接收完成".into(),
                });
            }

            // ── 发送侧 ──
            (Direction::Send, FrameKind::ZRINIT) => {
                if self.eof_sent {
                    // ZEOF 之后对端回 ZRINIT = 当前文件收妥。**不直接收尾**：
                    // 队列里可能还有下一个文件，那是 app 层才知道的事
                    // （状态机不碰文件系统）。交给它决定：next_file 或 finish。
                    out.push(Action::FileSent);
                } else if self.zfile_sent {
                    // 当前文件的 ZFILE 已经发过了——这是对端重复的 ZRINIT
                    // （起步时它连发两遍，见字段注释），忽略，不重发 ZFILE。
                } else {
                    self.zfile_sent = true;
                    out.push(Action::Send(self.zfile()));
                }
            }
            (Direction::Send, FrameKind::ZRPOS) => {
                // ZRPOS 是「从这个偏移开始发」，**不是**一个可选的续传特性开关：
                // 它既是首次开传（位置 0），也是 ZMODEM 唯一的重传机制——对端一旦
                // 校验出错就发 ZRPOS(已收字节数) 要求回退重发。早先这里把非零位置
                // 当成「请求断点续传」直接取消，等于任何一次线路误码都让传输失败；
                // 而真实的 `rz` 在我们数据帧没收尾时也会发 ZRPOS，于是那条取消分支
                // 还会把「帧没收尾」这个自家 bug 报成「对端要求续传」。
                let pos = h.position() as u64;
                if pos > self.out_size {
                    // 超过文件长度就是无从满足的请求，取消并说清楚
                    self.phase = Phase::Finished;
                    out.push(Action::Send(cancel_sequence()));
                    out.push(Action::Done {
                        ok: false,
                        message: format!(
                            "对端请求从 {pos} 字节处发送，超过文件长度 {}，已取消",
                            self.out_size
                        ),
                    });
                    return;
                }
                self.phase = Phase::Sending;
                self.offset = pos;
                self.frame_open = true;
                // ZDATA 头必须带上起始位置：对端按它记账，写 0 会让回退重发之后
                // 的字节被拼到错误的偏移上（文件长度对、内容错位）。
                out.push(Action::Send(
                    FrameHeader {
                        kind: FrameKind::ZDATA,
                        flags: (pos as u32).to_le_bytes(),
                    }
                    .to_bin32(),
                ));
                out.push(Action::NeedChunk { offset: pos });
            }
            (Direction::Send, FrameKind::ZFIN) => {
                self.phase = Phase::Finished;
                out.push(Action::Done {
                    ok: true,
                    message: "发送完成".into(),
                });
            }

            // ── 双向 ──
            (_, FrameKind::ZCAN | FrameKind::ZABORT | FrameKind::ZFERR) => {
                self.phase = Phase::Finished;
                out.push(Action::Done {
                    ok: false,
                    message: "对端中止了传输".into(),
                });
            }
            (_, FrameKind::ZSKIP) => {
                self.phase = Phase::Finished;
                out.push(Action::Done {
                    ok: false,
                    message: "对端跳过了该文件".into(),
                });
            }
            // 其余帧忽略（ZACK/ZSINIT 等在本主干流程里无需响应）
            _ => {}
        }
    }

    /// 等 ZFILE 的文件信息子包。子包不全时**不消耗字节**，`feed` 的循环自然退出。
    fn step_file_info(&mut self, out: &mut Vec<Action>) {
        match parse_subpacket(&self.buf, self.crc32_mode) {
            Parsed2::Incomplete => {}
            Parsed2::Bad(used) => {
                self.buf.drain(..used.min(self.buf.len()));
                self.phase = Phase::Handshake;
                out.push(Action::Send(
                    FrameHeader {
                        kind: FrameKind::ZNAK,
                        flags: [0; 4],
                    }
                    .to_hex(),
                ));
            }
            Parsed2::Subpacket(payload, _, used) => {
                self.buf.drain(..used);
                self.phase = Phase::Handshake;
                let info = parse_file_info(&payload);
                if info.name.is_empty() {
                    // 名字不可用（纯 `..`／空）→ 取消，一个字节都不落盘
                    self.phase = Phase::Finished;
                    out.push(Action::Send(cancel_sequence()));
                    out.push(Action::Done {
                        ok: false,
                        message: "对端提供的文件名不合法，已取消".into(),
                    });
                    return;
                }
                if info.size.is_some_and(|s| s > MAX_RECV_BYTES) {
                    self.phase = Phase::Finished;
                    out.push(Action::Send(cancel_sequence()));
                    out.push(Action::Done {
                        ok: false,
                        message: format!(
                            "文件大小超过上限 {} MiB，已取消（大文件请用文件面板传输）",
                            MAX_RECV_BYTES / 1024 / 1024
                        ),
                    });
                    return;
                }
                self.declared = info.size;
                self.offset = 0;
                out.push(Action::BeginRecv(info));
                out.push(Action::Send(
                    FrameHeader {
                        kind: FrameKind::ZRPOS,
                        flags: [0; 4],
                    }
                    .to_hex(),
                ));
            }
        }
    }

    /// 收数据中：解析子包直到帧结束。
    fn step_receiving(&mut self, out: &mut Vec<Action>) {
        match parse_subpacket(&self.buf, self.crc32_mode) {
            Parsed2::Incomplete => {}
            Parsed2::Bad(used) => {
                self.buf.drain(..used.min(self.buf.len()));
                // 数据坏了要让对端重发本帧：ZRPOS 回当前偏移
                out.push(Action::Send(
                    FrameHeader {
                        kind: FrameKind::ZRPOS,
                        flags: (self.offset as u32).to_le_bytes(),
                    }
                    .to_hex(),
                ));
            }
            Parsed2::Subpacket(data, end, used) => {
                self.buf.drain(..used);
                self.offset += data.len() as u64;
                if !data.is_empty() {
                    out.push(Action::Write(data));
                }
                match end {
                    SubpacketEnd::NoAck => {}
                    SubpacketEnd::AckContinue => out.push(Action::Send(
                        FrameHeader {
                            kind: FrameKind::ZACK,
                            flags: (self.offset as u32).to_le_bytes(),
                        }
                        .to_hex(),
                    )),
                    SubpacketEnd::EndFrame => {
                        self.phase = Phase::Handshake; // 等 ZEOF / 下一个 ZDATA
                    }
                    SubpacketEnd::AckEnd => {
                        self.phase = Phase::Handshake;
                        out.push(Action::Send(
                            FrameHeader {
                                kind: FrameKind::ZACK,
                                flags: (self.offset as u32).to_le_bytes(),
                            }
                            .to_hex(),
                        ));
                    }
                }
            }
        }
    }

    /// ZFILE 帧（含文件信息子包）。
    ///
    /// 两处刻意与真实 `sz` 对齐（抓真 `sz` 的 ZFILE 逐字节比对得来的）：
    ///
    /// - `flags[3]`（zmodem 的 **ZF0**，转换选项）显式写 `ZCBIN`「二进制、不做换行
    ///   转换」。lrzsz 的 `rz` 在 ZF0 为 0 时会自己默认成 ZCBIN，所以这一项不是
    ///   它必需的；显式写是为了不依赖对端的默认——别的实现未必这么默认。
    /// - 文件信息按 `<十进制长度> <八进制 mtime> <八进制 mode>` 三段写，`mode` 带上
    ///   `UNIXFILE`(0100000) 位。`rz` 用 `sscanf("%ld%lo%o")` 只读这三段，且拿
    ///   `mode & UNIXFILE` 判「来自 Unix」——不置这一位的话部分接收端会把文件名
    ///   转成小写（CP/M 兼容路径）。mtime 写 0 表示未知：`rz` 只在非 0 时才 `utime`，
    ///   写一个假时间戳反而会把文件日期设成 1970。
    fn zfile(&self) -> Vec<u8> {
        /// ZF0 = ZCBIN：二进制传输，不做换行/字符集转换。
        const ZCBIN: u8 = 1;
        let mut out = FrameHeader {
            kind: FrameKind::ZFILE,
            flags: [0, 0, 0, ZCBIN],
        }
        .to_bin32();
        let mut payload = self.out_name.as_bytes().to_vec();
        payload.push(0);
        // 0100644 八进制 = 普通文件 + rw-r--r--
        payload.extend_from_slice(format!("{} 0 100644", self.out_size).as_bytes());
        out.extend_from_slice(&encode_subpacket(
            &payload,
            SubpacketEnd::AckEnd,
            self.crc32_mode,
        ));
        out
    }
}

/// ZRINIT：声明「可以收、支持 CRC-32、支持 ESCCTL」。
///
/// flags 位（zmodem.h）：`CANFC32 = 0x20`（CRC-32）、`CANFDX = 0x01`（全双工）、
/// `CANOVIO = 0x02`（重叠 IO）、`ESCCTL = 0x40`（转义全部控制字符）。
/// 声明 ESCCTL 是为了让对端也转义控制字符——链路中间有吞控制字符的设备时，
/// 不声明就会表现为「传大文件偶发 CRC 错」。
fn zrinit() -> Vec<u8> {
    FrameHeader {
        kind: FrameKind::ZRINIT,
        flags: [0, 0, 0, 0x20 | 0x01 | 0x02 | 0x40],
    }
    .to_hex()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zmodem::{crc32, ZDLE, ZPAD};

    fn hdr(kind: FrameKind, pos: u32) -> Vec<u8> {
        FrameHeader {
            kind,
            flags: pos.to_le_bytes(),
        }
        .to_hex()
    }

    /// 从一批动作里抽出我们发出的 ZFILE 的文件名（对端视角看到的）。
    fn zfile_names(actions: &[Action]) -> Vec<String> {
        let mut out = Vec::new();
        let mut stream = sends(actions);
        while let Parsed::Header(h, used) = parse_header(&stream) {
            let rest = stream.split_off(used);
            if h.kind == FrameKind::ZFILE {
                if let Parsed2::Subpacket(payload, _, _) = parse_subpacket(&rest, true) {
                    out.push(
                        String::from_utf8_lossy(&payload)
                            .split('\0')
                            .next()
                            .unwrap_or("")
                            .to_string(),
                    );
                }
            }
            stream = rest;
        }
        out
    }

    /// **多文件发送：两文件队列走完整帧序，第二个 ZFILE 真的发出去了。**
    ///
    /// 驱动方式即 app 层的新契约：ZEOF→ZRINIT 产 FileSent → next_file → ZRPOS →
    /// 数据 → ZEOF → FileSent → finish。若哪一环丢了，剩下的环节会以
    /// 「对端在等一个永不到来的帧」的形式挂住——离真因很远，故把整条链钉在
    /// 一个测试里逐环断言。
    #[test]
    fn multi_file_send_queues_the_next_zfile_after_file_sent() {
        let a: Vec<u8> = (0u8..=255).cycle().take(1500).collect();
        let b: Vec<u8> = vec![7u8; 300];
        let mut s = ZmodemSession::new_send("a.bin", a.len() as u64);

        // 初始 ZRINIT → 第一个 ZFILE
        let names = zfile_names(&s.feed(&hdr(FrameKind::ZRINIT, 0)));
        assert_eq!(names, vec!["a.bin".to_string()], "第一个 ZFILE 该是 a.bin");

        // a：ZRPOS + 数据
        s.feed(&hdr(FrameKind::ZRPOS, 0));
        for part in a.chunks(SUBPACKET_BYTES) {
            s.feed_file_chunk(part);
        }
        // 收妥 → FileSent → next_file(b)
        let acts = s.feed(&hdr(FrameKind::ZRINIT, 0));
        assert!(acts.iter().any(|a| matches!(a, Action::FileSent)));
        let names = zfile_names(&s.next_file("b.bin", b.len() as u64));
        assert_eq!(names, vec!["b.bin".to_string()], "第二个 ZFILE 该是 b.bin");
        assert_eq!(s.declared_size(), Some(300), "声明大小须随文件切换");
        assert_eq!(s.transferred(), 0, "offset 须随文件归零");

        // b：ZRPOS + 数据
        s.feed(&hdr(FrameKind::ZRPOS, 0));
        let mut rebuilt = Vec::new();
        for part in b.chunks(SUBPACKET_BYTES) {
            for act in s.feed_file_chunk(part) {
                if let Action::Send(bytes) = act {
                    if bytes.first() == Some(&crate::zmodem::ZPAD) {
                        continue; // 帧头（ZEOF）
                    }
                    if let Parsed2::Subpacket(d, _, _) = parse_subpacket(&bytes, true) {
                        rebuilt.extend_from_slice(&d);
                    }
                }
            }
        }
        assert_eq!(rebuilt, b, "第二个文件的数据须逐字节正确");

        // 收妥 → FileSent → 队列空 → finish
        let acts = s.feed(&hdr(FrameKind::ZRINIT, 0));
        assert!(acts.iter().any(|a| matches!(a, Action::FileSent)));
        assert!(s
            .finish()
            .iter()
            .any(|a| matches!(a, Action::Done { ok: true, .. })));
        assert!(s.is_finished());
    }

    /// **0 字节文件：ZEOF 后的 ZRINIT 必须被识别为「收妥」而不是「初始」。**
    ///
    /// 这正是 `eof_sent` 标志存在的理由——旧判据 `offset > 0` 对 0 字节文件恒假，
    /// 会把收妥误判成初始 ZRINIT 再发一遍同名 ZFILE；在多文件队列里表现为
    /// 0 字节文件原地重发、永远到不了下一个文件。
    #[test]
    fn a_zero_byte_file_is_acknowledged_not_resent() {
        let mut s = ZmodemSession::new_send("empty.bin", 0);
        s.feed(&hdr(FrameKind::ZRINIT, 0)); // 初始 → ZFILE
        s.feed(&hdr(FrameKind::ZRPOS, 0)); // 进 Sending
        let acts = s.feed_file_chunk(&[]); // 空读 = 文件已尽 → ZEOF
        let saw_zeof = acts.iter().any(|a| match a {
            Action::Send(b) => {
                matches!(parse_header(b), Parsed::Header(h, _) if h.kind == FrameKind::ZEOF)
            }
            _ => false,
        });
        assert!(saw_zeof, "0 字节也须发 ZEOF，实得 {acts:?}");
        // 收妥的 ZRINIT → FileSent（**不是**再发一个 ZFILE）
        let acts = s.feed(&hdr(FrameKind::ZRINIT, 0));
        assert!(
            acts.iter().any(|a| matches!(a, Action::FileSent)),
            "0 字节文件的收妥 ZRINIT 被误判成初始（eof_sent 判据失效）"
        );
        // 且队列空时 finish 收得了尾
        assert!(s
            .finish()
            .iter()
            .any(|a| matches!(a, Action::Done { ok: true, .. })));
    }

    /// **对端连发两遍 ZRINIT（真 rz 的起步行为）只回一个 ZFILE。**
    ///
    /// 对端在会话起步时发一遍自己的通告、再应答我们的 ZRQINIT 又一遍，两帧常常
    /// 落在同一个 SSH 读块里。没有 `zfile_sent` 闩锁的实现会回两个 ZFILE——对端
    /// 把第二个当成「下一个文件」重新打开（截断同名文件），落地内容全错。
    /// 这个 bug 单文件传输掩盖得住（itest 单文件用例一直绿），多文件第一次真跑
    /// 就现形（multi-a.bin 落地为空）。
    #[test]
    fn a_doubled_zrinit_in_one_chunk_yields_exactly_one_zfile() {
        let mut s = ZmodemSession::new_send("dup.bin", 10);
        // 两个 ZRINIT 拼在一个读块里喂入
        let doubled = [hdr(FrameKind::ZRINIT, 0), hdr(FrameKind::ZRINIT, 0)].concat();
        let acts = s.feed(&doubled);
        let names = zfile_names(&acts);
        assert_eq!(
            names,
            vec!["dup.bin".to_string()],
            "重复的 ZRINIT 不得触发第二个 ZFILE，实得 {names:?}"
        );
        // 后续正常推进不受影响
        let acts = s.feed(&hdr(FrameKind::ZRPOS, 0));
        assert!(
            acts.iter()
                .any(|a| matches!(a, Action::NeedChunk { offset: 0 })),
            "闩锁不应影响 ZRPOS 之后的正常推进，实得 {acts:?}"
        );
    }

    /// 0 字节文件**排在队列中间**：它后面的文件照常到达。
    #[test]
    fn a_zero_byte_file_in_the_middle_does_not_eat_the_rest() {
        let mut s = ZmodemSession::new_send("empty.bin", 0);
        s.feed(&hdr(FrameKind::ZRINIT, 0));
        s.feed(&hdr(FrameKind::ZRPOS, 0));
        s.feed_file_chunk(&[]); // 0 字节 → ZEOF
        let acts = s.feed(&hdr(FrameKind::ZRINIT, 0)); // 收妥 → FileSent
        assert!(acts.iter().any(|a| matches!(a, Action::FileSent)));

        // 切到下一个文件，帧序必须照常推进
        let names = zfile_names(&s.next_file("after.bin", 4));
        assert_eq!(names, vec!["after.bin".to_string()]);
        s.feed(&hdr(FrameKind::ZRPOS, 0));
        let mut got = Vec::new();
        for act in s.feed_file_chunk(&[1, 2, 3, 4]) {
            if let Action::Send(bytes) = act {
                if bytes.first() != Some(&crate::zmodem::ZPAD) {
                    if let Parsed2::Subpacket(d, _, _) = parse_subpacket(&bytes, true) {
                        got.extend_from_slice(&d);
                    }
                }
            }
        }
        assert_eq!(got, vec![1, 2, 3, 4], "0 字节文件之后的文件数据须照常可发");
    }

    fn sends(actions: &[Action]) -> Vec<u8> {
        let mut v = Vec::new();
        for a in actions {
            if let Action::Send(b) = a {
                v.extend_from_slice(b);
            }
        }
        v
    }

    fn writes(actions: &[Action]) -> Vec<u8> {
        let mut v = Vec::new();
        for a in actions {
            if let Action::Write(b) = a {
                v.extend_from_slice(b);
            }
        }
        v
    }

    /// S332：**接收全流程**——对端 sz 的完整帧序列进来，落盘字节逐字节正确。
    /// 这是 ZMODEM 的核心判据（出口标准「传输字节校验正确」的离线等价物）。
    #[test]
    fn receive_full_flow_bytes_exact() {
        let payload: Vec<u8> = (0u8..=255).cycle().take(3000).collect();
        let mut s = ZmodemSession::new_receive();
        assert!(!s.start().is_empty(), "接收侧启动须发 ZRINIT");

        let mut acts = s.feed(&hdr(FrameKind::ZRQINIT, 0));
        assert!(!sends(&acts).is_empty(), "ZRQINIT 应答 ZRINIT");

        // ZFILE + 文件信息子包
        let mut zfile = FrameHeader {
            kind: FrameKind::ZFILE,
            flags: [0; 4],
        }
        .to_bin32();
        let mut info = b"payload.bin".to_vec();
        info.push(0);
        info.extend_from_slice(b"3000 0 0 0");
        zfile.extend_from_slice(&encode_subpacket(&info, SubpacketEnd::AckEnd, true));
        acts = s.feed(&zfile);
        assert!(
            acts.iter().any(|a| matches!(a, Action::BeginRecv(i) if i.name == "payload.bin" && i.size == Some(3000))),
            "须产出 BeginRecv 且名字/大小如实，实得 {acts:?}"
        );

        // ZDATA + 三个子包（最后一个 EndFrame）
        let mut data_stream = FrameHeader {
            kind: FrameKind::ZDATA,
            flags: [0; 4],
        }
        .to_bin32();
        for (i, part) in payload.chunks(1024).enumerate() {
            let last = (i + 1) * 1024 >= payload.len();
            data_stream.extend_from_slice(&encode_subpacket(
                part,
                if last {
                    SubpacketEnd::EndFrame
                } else {
                    SubpacketEnd::NoAck
                },
                true,
            ));
        }
        acts = s.feed(&data_stream);
        assert_eq!(writes(&acts), payload, "落盘字节须与载荷逐字节一致");
        assert_eq!(s.transferred(), 3000);

        acts = s.feed(&hdr(FrameKind::ZEOF, 3000));
        assert!(
            acts.iter()
                .any(|a| matches!(a, Action::FinishRecv { bytes: 3000 })),
            "ZEOF 须产出 FinishRecv，实得 {acts:?}"
        );

        acts = s.feed(&hdr(FrameKind::ZFIN, 0));
        assert!(
            acts.iter()
                .any(|a| matches!(a, Action::Done { ok: true, .. })),
            "ZFIN 须收尾成功，实得 {acts:?}"
        );
        assert!(s.is_finished());
    }

    /// S332：**逐字节喂**同样正确——SSH 读块会把帧切在任意位置，
    /// 这条是「切块安全」的最强形式。
    #[test]
    fn receive_survives_byte_by_byte_feeding() {
        let payload: Vec<u8> = (0u8..200).collect();
        let mut stream = hdr(FrameKind::ZRQINIT, 0);
        let mut zfile = FrameHeader {
            kind: FrameKind::ZFILE,
            flags: [0; 4],
        }
        .to_bin32();
        let mut info = b"x.bin".to_vec();
        info.push(0);
        info.extend_from_slice(b"200 0 0 0");
        zfile.extend_from_slice(&encode_subpacket(&info, SubpacketEnd::AckEnd, true));
        stream.extend_from_slice(&zfile);
        stream.extend_from_slice(
            &FrameHeader {
                kind: FrameKind::ZDATA,
                flags: [0; 4],
            }
            .to_bin32(),
        );
        stream.extend_from_slice(&encode_subpacket(&payload, SubpacketEnd::EndFrame, true));
        stream.extend_from_slice(&hdr(FrameKind::ZEOF, 200));
        stream.extend_from_slice(&hdr(FrameKind::ZFIN, 0));

        let mut s = ZmodemSession::new_receive();
        let mut got = Vec::new();
        let mut done = false;
        for b in &stream {
            for a in s.feed(&[*b]) {
                match a {
                    Action::Write(d) => got.extend_from_slice(&d),
                    Action::Done { ok, .. } => done = ok,
                    _ => {}
                }
            }
        }
        assert_eq!(got, payload, "逐字节喂入后落盘字节仍须逐字节一致");
        assert!(done, "逐字节喂入仍须走到 Done");
    }

    /// S333：**发送全流程**——对端 rz 的应答序列进来，我们产出的字节流
    /// 能被同一套解析器还原成原文件。
    #[test]
    fn send_full_flow_produces_parsable_stream() {
        let file: Vec<u8> = (0u8..=255).cycle().take(2500).collect();
        let mut s = ZmodemSession::new_send("/tmp/up.bin", file.len() as u64);
        assert!(!s.start().is_empty(), "发送侧启动须发 ZRQINIT");

        // 对端 ZRINIT → 我们发 ZFILE
        let acts = s.feed(&hdr(FrameKind::ZRINIT, 0));
        let zfile_bytes = sends(&acts);
        assert!(!zfile_bytes.is_empty(), "ZRINIT 后须发 ZFILE");
        // 本地路径不得出现在**线上字节**里。必须直接查原始字节：走
        // `parse_file_info` 还原会被接收侧的基名化掩盖，那样测的是解析器而不是
        // 发送侧——变异测试正是这样抓到这条断言曾经是假的。
        assert!(
            !zfile_bytes.windows(5).any(|w| w == b"/tmp/"),
            "ZFILE 线上字节不得泄漏本地路径，实得 {:?}",
            String::from_utf8_lossy(&zfile_bytes)
        );
        match parse_header(&zfile_bytes) {
            Parsed::Header(h, used) => {
                assert_eq!(h.kind, FrameKind::ZFILE);
                match parse_subpacket(&zfile_bytes[used..], true) {
                    Parsed2::Subpacket(p, _, _) => {
                        let info = parse_file_info(&p);
                        assert_eq!(info.name, "up.bin", "文件名须基名化");
                        assert_eq!(info.size, Some(2500));
                    }
                    other => panic!("ZFILE 子包解析失败：{other:?}"),
                }
            }
            other => panic!("ZFILE 头解析失败：{other:?}"),
        }

        // 对端 ZRPOS(0) → 我们进 Sending 并索要第一块
        let acts = s.feed(&hdr(FrameKind::ZRPOS, 0));
        assert!(
            acts.iter()
                .any(|a| matches!(a, Action::NeedChunk { offset: 0 })),
            "ZRPOS 后须索要偏移 0 的数据块，实得 {acts:?}"
        );

        // 喂文件块 → 逐条收集我们发出的字节。**不**把它们拼成一片再解析：
        // 流里既有子包也有帧头（最后一块之后紧跟 ZEOF），拼一片再当子包解会在
        // 帧头处炸掉，而那是测试自己的形状问题，不是实现的问题。
        let mut rebuilt = Vec::new();
        let mut saw_eof = false;
        for part in file.chunks(SUBPACKET_BYTES) {
            for a in s.feed_file_chunk(part) {
                let Action::Send(b) = a else { continue };
                if b.first() == Some(&crate::zmodem::ZPAD) {
                    if let Parsed::Header(h, _) = parse_header(&b) {
                        saw_eof |= h.kind == FrameKind::ZEOF;
                    }
                    continue;
                }
                match parse_subpacket(&b, true) {
                    Parsed2::Subpacket(d, _, _) => rebuilt.extend_from_slice(&d),
                    other => panic!("发出的子包无法还原：{other:?}"),
                }
            }
        }
        assert!(saw_eof, "最后一块之后须紧跟 ZEOF（帧尾与 ZEOF 同批发出）");
        assert_eq!(rebuilt, file, "发出的字节流须能还原成原文件（逐字节）");
        assert_eq!(s.transferred(), 2500);

        // 对端 ZRINIT（收妥）→ FileSent（队列归 app 层管，状态机不直接收尾）
        let acts = s.feed(&hdr(FrameKind::ZRINIT, 0));
        assert!(acts.iter().any(|a| matches!(a, Action::FileSent)));
        // 队列空 → finish 收尾
        let acts = s.finish();
        assert!(acts
            .iter()
            .any(|a| matches!(a, Action::Done { ok: true, .. })));
        assert!(s.is_finished());
    }

    /// S340：数据子包的 CRC 宽度跟着帧头格式走，不是写死 CRC-32。
    ///
    /// 写死的后果最难查：握手（十六进制头）全对，一进数据就每个子包都 CRC 错，
    /// 表现为「连上了、然后无限重传」。这里用 `ZBIN`（CRC-16）头发一整个文件，
    /// 落盘字节必须仍然逐字节正确。
    #[test]
    fn data_subpacket_crc_width_follows_frame_header_format() {
        let payload: Vec<u8> = (0u8..=255).cycle().take(1500).collect();
        let mut s = ZmodemSession::new_receive();

        // ZFILE 用 CRC-16 的 ZBIN 头 + CRC-16 的信息子包
        let mut zfile = FrameHeader {
            kind: FrameKind::ZFILE,
            flags: [0; 4],
        }
        .to_bin16();
        let mut info = b"crc16.bin".to_vec();
        info.push(0);
        info.extend_from_slice(b"1500 0 0 0");
        zfile.extend_from_slice(&encode_subpacket(&info, SubpacketEnd::AckEnd, false));
        let acts = s.feed(&zfile);
        assert!(
            acts.iter()
                .any(|a| matches!(a, Action::BeginRecv(i) if i.name == "crc16.bin")),
            "CRC-16 的 ZFILE 须被正确解析，实得 {acts:?}"
        );

        // ZDATA 同样 CRC-16
        let mut data = FrameHeader {
            kind: FrameKind::ZDATA,
            flags: [0; 4],
        }
        .to_bin16();
        data.extend_from_slice(&encode_subpacket(&payload, SubpacketEnd::EndFrame, false));
        let acts = s.feed(&data);
        assert_eq!(
            writes(&acts),
            payload,
            "CRC-16 数据子包须逐字节落盘（写死 CRC-32 时这里会全判成 CRC 错）"
        );

        // 同一会话里对端切回 CRC-32 也要跟得上（zmodem 允许逐帧变）
        let mut d32 = FrameHeader {
            kind: FrameKind::ZDATA,
            flags: [0; 4],
        }
        .to_bin32();
        d32.extend_from_slice(&encode_subpacket(b"tail32", SubpacketEnd::EndFrame, true));
        let acts = s.feed(&d32);
        assert_eq!(
            writes(&acts),
            b"tail32".to_vec(),
            "帧内切换 CRC 宽度须跟得上"
        );
    }

    /// S340：`header_crc32` 对三种头格式的判定。
    #[test]
    fn header_crc32_reports_declared_width() {
        use crate::zmodem::header_crc32;
        let h = FrameHeader {
            kind: FrameKind::ZDATA,
            flags: [0; 4],
        };
        assert_eq!(header_crc32(&h.to_bin32()), Some(true), "ZBIN32 = CRC-32");
        assert_eq!(header_crc32(&h.to_bin16()), Some(false), "ZBIN = CRC-16");
        assert_eq!(
            header_crc32(&h.to_hex()),
            Some(false),
            "ZHEX 头自身是 CRC-16"
        );
        assert_eq!(header_crc32(b"not a frame"), None);
        assert_eq!(header_crc32(b""), None);
    }

    /// S336：ZFIN 之后同一读块里的 shell 字节要能交还终端。
    ///
    /// 不交还的表现是「传完文件提示符消失、敲回车才回来」，用户读作卡死。
    #[test]
    fn trailing_shell_bytes_are_returned_after_session_ends() {
        let mut s = ZmodemSession::new_receive();
        let mut stream = hdr(FrameKind::ZFIN, 0);
        stream.extend_from_slice(b"OO\r\nuser@host:~$ ");
        let acts = s.feed(&stream);
        assert!(acts
            .iter()
            .any(|a| matches!(a, Action::Done { ok: true, .. })));
        assert_eq!(
            String::from_utf8_lossy(&s.take_remainder()),
            "OO\r\nuser@host:~$ ",
            "ZFIN 之后的 shell 字节须原样交还"
        );
        assert!(
            s.take_remainder().is_empty(),
            "取走后不得重复交还（会话重放提示符）"
        );
    }

    /// S335：ZFILE 头与它的信息子包被读块切开，且头是**十六进制**编码时仍正确。
    ///
    /// 这条钉的是 `Phase::AwaitFileInfo` 的存在理由：早先的实现把帧头拼回缓冲区
    /// 重放，只在「重放出的二进制头不比原十六进制头长」时才收敛——是编码长度的
    /// 巧合。这里用 hex 头 + 单字节喂入把那条路径压满。
    #[test]
    fn zfile_header_split_from_info_subpacket_hex_header() {
        let mut info = b"split.bin".to_vec();
        info.push(0);
        info.extend_from_slice(b"7 0 0 0");
        let mut stream = FrameHeader {
            kind: FrameKind::ZFILE,
            flags: [0; 4],
        }
        .to_hex();
        let split_at = stream.len(); // 帧头与子包的边界
                                     // 十六进制头声明的子包宽度是 CRC-16（lrzsz 的 rz 即如此），所以这里必须用
                                     // CRC-16 编码信息子包。配 CRC-32 是个真实对端不会发出的自相矛盾的帧——
                                     // 早先这条测试正是那么造的，等到 CRC 宽度跟随帧头之后立刻暴露。
        stream.extend_from_slice(&encode_subpacket(&info, SubpacketEnd::AckEnd, false));

        let mut s = ZmodemSession::new_receive();
        // 先只喂帧头：不得产出 BeginRecv，也不得卡死或吐错
        let first = s.feed(&stream[..split_at]);
        assert!(
            !first.iter().any(|a| matches!(a, Action::BeginRecv(_))),
            "信息子包未到时不得开始接收，实得 {first:?}"
        );
        // 再逐字节喂子包
        let mut began = None;
        for b in &stream[split_at..] {
            for a in s.feed(&[*b]) {
                if let Action::BeginRecv(i) = a {
                    began = Some(i);
                }
            }
        }
        assert_eq!(
            began.map(|i| (i.name, i.size)),
            Some(("split.bin".to_string(), Some(7))),
            "切开喂入后仍须解析出文件名与大小"
        );
        // 缓冲区不得膨胀（旧实现的重放写法在这里会反复拼头）
        assert!(
            s.buf.len() < 64,
            "缓冲区不得因重放而膨胀，实得 {} 字节",
            s.buf.len()
        );
    }

    /// S334：不合法文件名（穿越/空）**不落盘**，直接取消。
    #[test]
    fn rejects_traversal_filename_without_writing() {
        let mut s = ZmodemSession::new_receive();
        let mut zfile = FrameHeader {
            kind: FrameKind::ZFILE,
            flags: [0; 4],
        }
        .to_bin32();
        let mut info = b"../../etc/shadow".to_vec();
        info.push(0);
        info.extend_from_slice(b"10 0 0 0");
        zfile.extend_from_slice(&encode_subpacket(&info, SubpacketEnd::AckEnd, true));
        let acts = s.feed(&zfile);
        // 基名化后是 "shadow"，合法——这条验的是**空名**才取消；穿越由基名化消解
        assert!(
            acts.iter()
                .any(|a| matches!(a, Action::BeginRecv(i) if i.name == "shadow")),
            "穿越路径应被基名化为 shadow 后正常接收，实得 {acts:?}"
        );

        // 纯 `..` 无法基名化 → 取消且无 BeginRecv
        let mut s2 = ZmodemSession::new_receive();
        let mut zf2 = FrameHeader {
            kind: FrameKind::ZFILE,
            flags: [0; 4],
        }
        .to_bin32();
        let mut i2 = b"..".to_vec();
        i2.push(0);
        i2.extend_from_slice(b"10 0 0 0");
        zf2.extend_from_slice(&encode_subpacket(&i2, SubpacketEnd::AckEnd, true));
        let acts2 = s2.feed(&zf2);
        assert!(
            !acts2.iter().any(|a| matches!(a, Action::BeginRecv(_))),
            "不合法名字不得开始接收"
        );
        assert!(acts2
            .iter()
            .any(|a| matches!(a, Action::Done { ok: false, .. })));
        assert!(
            sends(&acts2)
                .windows(8)
                .any(|w| w.iter().all(|&b| b == ZDLE)),
            "须发取消序列"
        );
    }

    /// S334：超上限的声明大小直接取消（防「对端声称 8 EiB」把盘写满）。
    #[test]
    fn rejects_oversized_declaration() {
        let mut s = ZmodemSession::new_receive();
        let mut zfile = FrameHeader {
            kind: FrameKind::ZFILE,
            flags: [0; 4],
        }
        .to_bin32();
        let mut info = b"huge.bin".to_vec();
        info.push(0);
        info.extend_from_slice(format!("{} 0 0 0", MAX_RECV_BYTES + 1).as_bytes());
        zfile.extend_from_slice(&encode_subpacket(&info, SubpacketEnd::AckEnd, true));
        let acts = s.feed(&zfile);
        assert!(!acts.iter().any(|a| matches!(a, Action::BeginRecv(_))));
        assert!(
            acts.iter().any(
                |a| matches!(a, Action::Done { ok: false, message } if message.contains("上限"))
            ),
            "须以「超过上限」收尾，实得 {acts:?}"
        );
    }

    /// S341：非零 ZRPOS 是**重传请求**，必须照办（回退到该偏移继续发）。
    ///
    /// 早先这里断言的是「明确拒绝续传」。那个判定是错的：ZRPOS 是 ZMODEM 唯一的
    /// 重传机制，对端一旦校验出错就发 `ZRPOS(已收字节数)`，拒绝它等于任何一次
    /// 线路误码都让传输失败。拿真实 `rz` 一跑就暴露了——它在我们数据帧没收尾时
    /// 也发 ZRPOS，于是那条拒绝分支把自家的帧收尾 bug 报成「对端要求续传」。
    #[test]
    fn honors_nonzero_zrpos_as_retransmit_request() {
        let mut s = ZmodemSession::new_send("f.bin", 100);
        s.feed(&hdr(FrameKind::ZRINIT, 0));
        let acts = s.feed(&hdr(FrameKind::ZRPOS, 50));
        assert!(
            acts.iter()
                .any(|a| matches!(a, Action::NeedChunk { offset: 50 })),
            "须索要偏移 50 处的数据（而不是取消），实得 {acts:?}"
        );
        assert!(!s.is_finished(), "重传请求不得终结会话");
        assert_eq!(s.transferred(), 50, "内部偏移须回退到对端要求的位置");
        // ZDATA 头必须带上该偏移，否则对端把后续字节拼到错位置
        let sent = sends(&acts);
        let at = sent
            .windows(3)
            .position(|w| w == [crate::zmodem::ZPAD, ZDLE, b'C'])
            .expect("须发出 ZBIN32 的 ZDATA 头");
        match parse_header(&sent[at..]) {
            Parsed::Header(h, _) => {
                assert_eq!(h.kind, FrameKind::ZDATA);
                assert_eq!(h.position(), 50, "ZDATA 头须声明起始位置 50");
            }
            other => panic!("ZDATA 头解析失败：{other:?}"),
        }
    }

    /// S341：超过文件长度的 ZRPOS 无从满足，取消并说清楚。
    #[test]
    fn rejects_zrpos_beyond_file_length() {
        let mut s = ZmodemSession::new_send("f.bin", 100);
        s.feed(&hdr(FrameKind::ZRINIT, 0));
        let acts = s.feed(&hdr(FrameKind::ZRPOS, 5000));
        assert!(
            acts.iter().any(|a| matches!(a, Action::Done { ok: false, message } if message.contains("超过文件长度"))),
            "越界的 ZRPOS 须取消并说明，实得 {acts:?}"
        );
        assert!(s.is_finished());
    }

    /// S341：数据帧必须以 ZCRCE 收尾，之后才允许出现 ZEOF 帧头。
    ///
    /// 一路 ZCRCG 流到底然后直接拍 ZEOF 是协议违规。真实 `rz` 的反应是重新同步并
    /// 回 `ZRPOS(已收字节数)`——表面看像「对端要求续传」，与真实原因毫无关系，
    /// 这条 bug 就是这么被误诊了一轮的。
    #[test]
    fn data_frame_is_closed_with_zcrce_before_zeof() {
        let file: Vec<u8> = (0u8..=255).cycle().take(2500).collect();
        let mut s = ZmodemSession::new_send("f.bin", file.len() as u64);
        s.feed(&hdr(FrameKind::ZRINIT, 0));
        s.feed(&hdr(FrameKind::ZRPOS, 0));

        let mut ends = Vec::new();
        let mut saw_eof_header = false;
        for part in file.chunks(SUBPACKET_BYTES) {
            for a in s.feed_file_chunk(part) {
                if let Action::Send(b) = a {
                    // 帧头（以 ZPAD 起头）与子包分开看
                    if b.first() == Some(&crate::zmodem::ZPAD) {
                        if let Parsed::Header(h, _) = parse_header(&b) {
                            if h.kind == FrameKind::ZEOF {
                                saw_eof_header = true;
                                assert_eq!(h.position(), 2500, "ZEOF 须声明总字节数");
                            }
                        }
                    } else if let Parsed2::Subpacket(_, end, _) = parse_subpacket(&b, true) {
                        // ZEOF 之前不得再出现子包
                        assert!(!saw_eof_header, "ZEOF 之后不得再发数据子包");
                        ends.push(end);
                    }
                }
            }
        }
        assert!(saw_eof_header, "读完文件须发出 ZEOF");
        assert_eq!(
            ends.last(),
            Some(&SubpacketEnd::EndFrame),
            "最后一个子包须以 ZCRCE 收尾，实得 {ends:?}"
        );
        assert!(
            ends[..ends.len() - 1]
                .iter()
                .all(|e| *e == SubpacketEnd::NoAck),
            "中间的子包应为 ZCRCG（逐包应答会把吞吐压死），实得 {ends:?}"
        );
    }

    /// S341：文件比声明的大小短时（文件被人动过）也要先收尾帧再发 ZEOF。
    #[test]
    fn short_file_still_closes_the_frame() {
        let mut s = ZmodemSession::new_send("f.bin", 10_000);
        s.feed(&hdr(FrameKind::ZRINIT, 0));
        s.feed(&hdr(FrameKind::ZRPOS, 0));
        s.feed_file_chunk(b"only-these-bytes"); // 远少于声明的 10000
        let acts = s.feed_file_chunk(&[]); // app 层读到 EOF

        let mut closed = false;
        let mut eof = false;
        for a in &acts {
            if let Action::Send(b) = a {
                if b.first() == Some(&crate::zmodem::ZPAD) {
                    if let Parsed::Header(h, _) = parse_header(b) {
                        assert!(closed, "ZEOF 必须排在帧尾子包之后");
                        if h.kind == FrameKind::ZEOF {
                            eof = true;
                        }
                    }
                } else if let Parsed2::Subpacket(d, SubpacketEnd::EndFrame, _) =
                    parse_subpacket(b, true)
                {
                    assert!(d.is_empty(), "补的帧尾子包应为空");
                    closed = true;
                }
            }
        }
        assert!(closed, "须补一个空的 ZCRCE 子包收尾，实得 {acts:?}");
        assert!(eof, "须发 ZEOF，实得 {acts:?}");
    }

    /// 坏 CRC 的数据子包 → 回 ZRPOS 要求重发（而不是把坏字节写进文件）。
    #[test]
    fn bad_data_crc_requests_resend_not_write() {
        let mut s = ZmodemSession::new_receive();
        let mut zfile = FrameHeader {
            kind: FrameKind::ZFILE,
            flags: [0; 4],
        }
        .to_bin32();
        let mut info = b"a.bin".to_vec();
        info.push(0);
        info.extend_from_slice(b"10 0 0 0");
        zfile.extend_from_slice(&encode_subpacket(&info, SubpacketEnd::AckEnd, true));
        s.feed(&zfile);
        s.feed(
            &FrameHeader {
                kind: FrameKind::ZDATA,
                flags: [0; 4],
            }
            .to_bin32(),
        );

        let mut pkt = encode_subpacket(b"0123456789", SubpacketEnd::EndFrame, true);
        let n = pkt.len();
        pkt[n - 1] ^= 0xff; // 破坏 CRC
        let acts = s.feed(&pkt);
        assert!(writes(&acts).is_empty(), "坏 CRC 的数据不得落盘");
        match parse_header(&sends(&acts)) {
            Parsed::Header(h, _) => assert_eq!(h.kind, FrameKind::ZRPOS, "须回 ZRPOS 要求重发"),
            other => panic!("期望 ZRPOS，实得 {other:?}"),
        }
    }

    /// 对端中止（ZCAN）与跳过（ZSKIP）都以失败收尾且给出原因。
    #[test]
    fn peer_abort_and_skip_end_with_reason() {
        for (kind, want) in [(FrameKind::ZCAN, "中止"), (FrameKind::ZSKIP, "跳过")] {
            let mut s = ZmodemSession::new_receive();
            let acts = s.feed(&hdr(kind, 0));
            assert!(
                acts.iter().any(
                    |a| matches!(a, Action::Done { ok: false, message } if message.contains(want))
                ),
                "{kind:?} 须以含「{want}」的原因收尾，实得 {acts:?}"
            );
        }
    }

    /// 用户取消：发取消序列并立即结束。
    #[test]
    fn user_cancel_sends_can_and_finishes() {
        let mut s = ZmodemSession::new_receive();
        let acts = s.cancel();
        assert!(s.is_finished());
        assert_eq!(sends(&acts).len(), 16, "取消序列 8×CAN + 8×BS");
        assert!(acts
            .iter()
            .any(|a| matches!(a, Action::Done { ok: false, .. })));
    }

    /// 帧前噪声（帧之间夹的 CR/LF/XON、`sz` 启动前的终端回显）被**消耗掉**，
    /// 而不是留在缓冲区里越堆越多。
    ///
    /// 两条断言分工：识别到帧证明噪声没挡住解析；缓冲区被排空证明噪声真的被丢弃
    /// 了——只断言前者的话，「噪声原地不动但恰好被后续机制绕过」也会通过（早先
    /// 的版本就是这样让一个多余的跳噪声循环逃过变异测试的）。
    #[test]
    fn noise_before_frame_is_consumed_not_accumulated() {
        let mut s = ZmodemSession::new_receive();
        let noise = b"\r\n\x11leftover output from the shell\r\n";
        let mut stream = noise.to_vec();
        stream.extend_from_slice(&hdr(FrameKind::ZRQINIT, 0));
        let acts = s.feed(&stream);
        assert!(!sends(&acts).is_empty(), "噪声后的 ZRQINIT 仍须被识别");
        assert!(
            s.buf.is_empty(),
            "噪声与已解析的帧须被排空，实得残留 {} 字节：{:?}",
            s.buf.len(),
            String::from_utf8_lossy(&s.buf)
        );

        // 只有噪声、没有帧时也必须排空（否则纯输出流会让缓冲区无界增长）
        let mut s2 = ZmodemSession::new_receive();
        s2.feed(b"just ordinary terminal output, no frames here\n");
        assert!(s2.buf.is_empty(), "纯噪声须被丢弃，否则缓冲区无界增长");
    }

    /// ZRINIT 声明 CRC-32 与 ESCCTL——不声明 ESCCTL 会在吞控制字符的链路上
    /// 表现为「传大文件偶发 CRC 错」。
    #[test]
    fn zrinit_declares_crc32_and_escctl() {
        match parse_header(&zrinit()) {
            Parsed::Header(h, _) => {
                assert_eq!(h.kind, FrameKind::ZRINIT);
                assert_ne!(h.flags[3] & 0x20, 0, "须声明 CANFC32");
                assert_ne!(h.flags[3] & 0x40, 0, "须声明 ESCCTL");
            }
            other => panic!("{other:?}"),
        }
    }

    /// 自检：测试里用的 CRC-32 与协议模块同源（防测试自造一份而与实现不符）。
    #[test]
    fn test_helpers_use_same_crc() {
        assert_eq!(crc32(b"abc"), crate::zmodem::crc32(b"abc"));
        assert_eq!(ZPAD, b'*');
    }
}
