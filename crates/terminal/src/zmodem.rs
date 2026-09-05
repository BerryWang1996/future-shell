//! ZMODEM 协议核心（M4a：rz/sz 自动收发；路线图 §M4）。
//!
//! 终端里敲 `rz` 或 `sz file`，远端会吐出 ZMODEM 的启动序列；客户端识别到就接管
//! 后续字节，做文件传输，完事再把终端交回去。本模块只做**协议的纯部分**——
//! 帧编解码、转义、CRC、状态判定；套接字与文件 IO 归调用方（app 层）。
//!
//! ## 为什么自己实现而不引 crate
//!
//! 路线图写的是「内嵌 zmodem 实现」。现有 Rust zmodem crate 都很久没动、且都把
//! IO 绑在自己的 Read/Write 上——而我们的字节来自 SSH 通道、要与终端渲染共用同
//! 一条流，必须能「按块喂、按需产出」。协议本身只有几种帧，自己写反而边界清楚，
//! 也让每一条判定都能离线测（ZMODEM 的坑几乎全在转义与 CRC，正是最该被钉住的）。
//!
//! ## 范围（如实标注）
//!
//! 做：ZRQINIT/ZRINIT/ZFILE/ZDATA/ZEOF/ZFIN 这条**主干**，CRC-16 与 CRC-32 双支，
//! 十六进制头与二进制头，`ZDLE` 转义（含控制字符转义，兼容 8 位不干净的链路）。
//!
//! 不做（也不假装做）：崩溃恢复（ZRPOS 续传）、压缩、多文件批量的中途取消
//! （只支持整体取消 ZCAN）。这些在 Xshell 里也少有人用，且每一条都会显著放大
//! 状态机；缺了它们的行为是「退回普通传输」，不是静默出错。

/// ZMODEM 控制字节（zmodem.h 口径）。
pub const ZPAD: u8 = b'*';
pub const ZDLE: u8 = 0x18; // CAN
pub const ZBIN: u8 = b'A'; // 二进制头，CRC-16
pub const ZHEX: u8 = b'B'; // 十六进制头，CRC-16
pub const ZBIN32: u8 = b'C'; // 二进制头，CRC-32

/// 帧类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    ZRQINIT = 0,
    ZRINIT = 1,
    ZSINIT = 2,
    ZACK = 3,
    ZFILE = 4,
    ZSKIP = 5,
    ZNAK = 6,
    ZABORT = 7,
    ZFIN = 8,
    ZRPOS = 9,
    ZDATA = 10,
    ZEOF = 11,
    ZFERR = 12,
    ZCRC = 13,
    ZCHALLENGE = 14,
    ZCOMPL = 15,
    ZCAN = 16,
    ZFREECNT = 17,
    ZCOMMAND = 18,
    ZSTDERR = 19,
}

impl FrameKind {
    pub fn from_u8(b: u8) -> Option<Self> {
        use FrameKind::*;
        Some(match b {
            0 => ZRQINIT,
            1 => ZRINIT,
            2 => ZSINIT,
            3 => ZACK,
            4 => ZFILE,
            5 => ZSKIP,
            6 => ZNAK,
            7 => ZABORT,
            8 => ZFIN,
            9 => ZRPOS,
            10 => ZDATA,
            11 => ZEOF,
            12 => ZFERR,
            13 => ZCRC,
            14 => ZCHALLENGE,
            15 => ZCOMPL,
            16 => ZCAN,
            17 => ZFREECNT,
            18 => ZCOMMAND,
            19 => ZSTDERR,
            _ => return None,
        })
    }
}

/// 数据子包的结束类型（ZDLE 后的那个字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubpacketEnd {
    /// ZCRCG：后面紧跟更多数据，不要求应答
    NoAck,
    /// ZCRCE：本帧最后一个子包
    EndFrame,
    /// ZCRCQ：要求应答后继续
    AckContinue,
    /// ZCRCW：要求应答且本帧结束
    AckEnd,
}

impl SubpacketEnd {
    pub fn from_u8(b: u8) -> Option<Self> {
        match b {
            b'h' => Some(Self::EndFrame),    // ZCRCE
            b'i' => Some(Self::NoAck),       // ZCRCG
            b'j' => Some(Self::AckContinue), // ZCRCQ
            b'k' => Some(Self::AckEnd),      // ZCRCW
            _ => None,
        }
    }
    pub fn to_u8(self) -> u8 {
        match self {
            Self::EndFrame => b'h',
            Self::NoAck => b'i',
            Self::AckContinue => b'j',
            Self::AckEnd => b'k',
        }
    }
}

/// CRC-16/XMODEM（多项式 0x1021，初值 0）——ZMODEM 的 CRC-16 支用它。
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// CRC-32（与 zip/PNG 同参数：反射、初值 0xFFFFFFFF、出值取反）。
/// ZMODEM 的 CRC-32 支就是这一个——**不能**用 CRC-16 的写法照搬多项式，
/// 两者的位序与初值都不同（这是自写实现最常出错的一处）。
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// ZDLE 转义（发送侧）。
///
/// 必转：ZDLE 自身、以及 `0x10/0x90/0x11/0x91/0x13/0x93`（DLE 与 XON/XOFF——
/// 链路上的流控字符若原样穿过会被中间设备吃掉或触发流控）。转义形式是
/// `ZDLE, byte ^ 0x40`。
///
/// `escape_all` 为真时连全部控制字符一并转义（对应 ZMODEM 的 `ESCCTL` 选项，
/// 用于 8 位不干净的链路）。我们发送时**默认开**：多几个字节的代价，换掉一类
/// 「某些跳板机吞控制字符导致传输莫名失败」的现场。
pub fn escape(data: &[u8], escape_all: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 8);
    for &b in data {
        if needs_escape(b, escape_all) {
            out.push(ZDLE);
            out.push(b ^ 0x40);
        } else {
            out.push(b);
        }
    }
    out
}

fn needs_escape(b: u8, escape_all: bool) -> bool {
    if b == ZDLE {
        return true;
    }
    match b & 0x7f {
        0x10 | 0x11 | 0x13 => true,         // DLE / XON / XOFF
        _ => escape_all && (b & 0x60) == 0, // 其余控制字符（含高位形）
    }
}

/// ZDLE 反转义（接收侧）。返回 `None` = 数据以半个转义序列结尾（需要更多字节）。
///
/// 这个「不完整」返回是必须的：SSH 读块可以恰好切在 ZDLE 与被转义字节之间，
/// 把它当成数据字节会静默污染一个字节——而 CRC 会在几百字节后才报错，
/// 排查时完全看不出是切块导致的。
pub fn unescape(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == ZDLE {
            if i + 1 >= data.len() {
                return None; // 半个转义序列
            }
            out.push(data[i + 1] ^ 0x40);
            i += 2;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    Some(out)
}

/// 一个已解析的帧头。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    pub kind: FrameKind,
    /// 4 字节参数（各帧语义不同：ZDATA/ZEOF 是偏移，ZRINIT 是能力位）
    pub flags: [u8; 4],
}

impl FrameHeader {
    /// 取 flags 的 32 位小端值（ZMODEM 的位置字段是小端）。
    pub fn position(&self) -> u32 {
        u32::from_le_bytes(self.flags)
    }

    /// 编码为十六进制头（`ZPAD ZPAD ZDLE ZHEX <type+flags 的 hex> CR LF XON`）。
    ///
    /// 十六进制头用于**控制帧**：它只含可打印字符，即使链路把 8 位截成 7 位也能过。
    /// 收方在终端里看到的就是那串 `**\x18B00...`——这也是为什么终端上能肉眼看到
    /// ZMODEM 启动。
    pub fn to_hex(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(5);
        body.push(self.kind as u8);
        body.extend_from_slice(&self.flags);
        let crc = crc16(&body);
        let mut out = vec![ZPAD, ZPAD, ZDLE, ZHEX];
        for b in &body {
            out.extend_from_slice(hex_byte(*b).as_slice());
        }
        out.extend_from_slice(hex_byte((crc >> 8) as u8).as_slice());
        out.extend_from_slice(hex_byte((crc & 0xff) as u8).as_slice());
        out.extend_from_slice(b"\r\n");
        out.push(0x11); // XON：唤醒可能被 XOFF 停住的对端
        out
    }

    /// 编码为二进制 CRC-32 头（`ZPAD ZDLE ZBIN32 <转义后的 type+flags+crc32>`）。
    /// 数据帧头用二进制：省字节，且此时链路已被证明是 8 位干净的（控制帧已通）。
    pub fn to_bin32(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(5);
        body.push(self.kind as u8);
        body.extend_from_slice(&self.flags);
        let crc = crc32(&body);
        let mut raw = body;
        raw.extend_from_slice(&crc.to_le_bytes());
        let mut out = vec![ZPAD, ZDLE, ZBIN32];
        out.extend_from_slice(&escape(&raw, true));
        out
    }

    /// 二进制头，CRC-16（`ZBIN`）。
    ///
    /// 本实现自己发数据帧一律用 `to_bin32`（CRC-32 检错能力强得多）。这个函数存在
    /// 是为了**能构造出对端可能发来的形态**去测解析路径——真实的 `sz` 在对端没有
    /// 通告 `CANFC32` 时就发 `ZBIN`，而「只测自己发得出的形态」正是让协议实现在
    /// 真机上翻车的典型盲区。头部 CRC-16 按 zmodem 惯例是**大端**。
    pub fn to_bin16(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(5);
        body.push(self.kind as u8);
        body.extend_from_slice(&self.flags);
        let crc = crc16(&body);
        let mut raw = body;
        raw.extend_from_slice(&crc.to_be_bytes());
        let mut out = vec![ZPAD, ZDLE, ZBIN];
        out.extend_from_slice(&escape(&raw, true));
        out
    }
}

fn hex_byte(b: u8) -> [u8; 2] {
    const H: &[u8; 16] = b"0123456789abcdef";
    [H[(b >> 4) as usize], H[(b & 0xf) as usize]]
}

fn from_hex_byte(hi: u8, lo: u8) -> Option<u8> {
    let d = |c: u8| -> Option<u8> {
        Some(match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => return None,
        })
    };
    Some((d(hi)? << 4) | d(lo)?)
}

/// 解析结果：解析出的帧 + 消耗的字节数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// 解析出一个帧头
    Header(FrameHeader, usize),
    /// 字节不足，需要更多输入（**不得**当成错误——SSH 读块随时会切断帧）
    Incomplete,
    /// CRC 不匹配等真实错误（消耗的字节数用于跳过坏帧）
    Bad(usize),
}

/// 从缓冲区开头解析一个帧头（要求首字节即 ZPAD）。
pub fn parse_header(buf: &[u8]) -> Parsed {
    // ZPAD [ZPAD] ZDLE <fmt>
    let mut i = 0;
    if buf.is_empty() {
        return Parsed::Incomplete;
    }
    if buf[0] != ZPAD {
        return Parsed::Bad(1);
    }
    i += 1;
    if i < buf.len() && buf[i] == ZPAD {
        i += 1;
    }
    if i >= buf.len() {
        return Parsed::Incomplete;
    }
    if buf[i] != ZDLE {
        return Parsed::Bad(i + 1);
    }
    i += 1;
    if i >= buf.len() {
        return Parsed::Incomplete;
    }
    let fmt = buf[i];
    i += 1;
    match fmt {
        ZHEX => {
            // type(2) + flags(8) + crc(4) 个十六进制字符
            const NEED: usize = 2 + 8 + 4;
            if buf.len() < i + NEED {
                return Parsed::Incomplete;
            }
            let mut body = [0u8; 5];
            for (k, slot) in body.iter_mut().enumerate() {
                match from_hex_byte(buf[i + k * 2], buf[i + k * 2 + 1]) {
                    Some(v) => *slot = v,
                    None => return Parsed::Bad(i + NEED),
                }
            }
            let crc_hi = from_hex_byte(buf[i + 10], buf[i + 11]);
            let crc_lo = from_hex_byte(buf[i + 12], buf[i + 13]);
            let (Some(hi), Some(lo)) = (crc_hi, crc_lo) else {
                return Parsed::Bad(i + NEED);
            };
            let want = ((hi as u16) << 8) | lo as u16;
            if crc16(&body) != want {
                return Parsed::Bad(i + NEED);
            }
            let mut consumed = i + NEED;
            // 尾随 CR/LF/XON 一并吞掉（对端实现对这三个字节的组合不完全一致）
            while consumed < buf.len() && matches!(buf[consumed], b'\r' | b'\n' | 0x11 | 0x91) {
                consumed += 1;
            }
            match FrameKind::from_u8(body[0]) {
                Some(kind) => Parsed::Header(
                    FrameHeader {
                        kind,
                        flags: [body[1], body[2], body[3], body[4]],
                    },
                    consumed,
                ),
                None => Parsed::Bad(consumed),
            }
        }
        ZBIN | ZBIN32 => {
            let crc_len = if fmt == ZBIN32 { 4 } else { 2 };
            // 转义后长度不定：边扫边反转义，直到收满 5 + crc_len 字节
            let mut raw = Vec::with_capacity(5 + crc_len);
            let mut j = i;
            while raw.len() < 5 + crc_len {
                if j >= buf.len() {
                    return Parsed::Incomplete;
                }
                if buf[j] == ZDLE {
                    if j + 1 >= buf.len() {
                        return Parsed::Incomplete;
                    }
                    raw.push(buf[j + 1] ^ 0x40);
                    j += 2;
                } else {
                    raw.push(buf[j]);
                    j += 1;
                }
            }
            let body = &raw[..5];
            let ok = if fmt == ZBIN32 {
                crc32(body) == u32::from_le_bytes([raw[5], raw[6], raw[7], raw[8]])
            } else {
                crc16(body) == u16::from_be_bytes([raw[5], raw[6]])
            };
            if !ok {
                return Parsed::Bad(j);
            }
            match FrameKind::from_u8(body[0]) {
                Some(kind) => Parsed::Header(
                    FrameHeader {
                        kind,
                        flags: [body[1], body[2], body[3], body[4]],
                    },
                    j,
                ),
                None => Parsed::Bad(j),
            }
        }
        _ => Parsed::Bad(i),
    }
}

/// 缓冲区开头那个帧头声明的**数据子包 CRC 宽度**：`Some(true)` = CRC-32。
///
/// 存在理由：ZMODEM 里数据子包的 CRC 宽度不是全局协商出来的，而是跟着**启动该帧
/// 的帧头格式**走——`ZBIN` 头后面跟 CRC-16 子包，`ZBIN32` 头后面跟 CRC-32 子包。
/// 状态机若把宽度写死成 CRC-32，遇到用 `ZBIN` 发数据的对端就会把每个子包都判成
/// CRC 错，表现为「握手成功、随即无限重传」——最难查的那类失败，因为前几帧全对。
///
/// 十六进制头（`ZHEX`）的头部 CRC 恒为 16 位，但它只用于控制帧，不会紧跟数据子包；
/// 对它返回 `Some(false)` 只是如实描述该头自身的宽度，调用方只在 ZDATA/ZFILE 上取值。
///
/// 单独开一个函数而不是给 `FrameHeader` 加字段：加字段会让全仓每一处
/// `FrameHeader { kind, flags }` 字面量都要改，而那些地方（构造要发出去的帧）
/// 本来就由 `to_hex`/`to_bin32` 决定格式，多一个字段只会多一处可以写矛盾的地方。
pub fn header_crc32(buf: &[u8]) -> Option<bool> {
    let mut i = 0;
    if buf.first() != Some(&ZPAD) {
        return None;
    }
    i += 1;
    if buf.get(i) == Some(&ZPAD) {
        i += 1;
    }
    if buf.get(i) != Some(&ZDLE) {
        return None;
    }
    match buf.get(i + 1) {
        Some(&ZBIN32) => Some(true),
        Some(&ZBIN) | Some(&ZHEX) => Some(false),
        _ => None,
    }
}

/// 在终端输出里找 ZMODEM 启动序列（`rz`/`sz` 触发的判据）。
///
/// 判据是「ZPAD ZPAD ZDLE ZHEX」+ 帧类型为 ZRQINIT（sz，远端要发文件给我们）
/// 或 ZRINIT（rz，远端准备收我们的文件）。**只认这两种**：拿 `**\x18B` 前缀
/// 当判据会把普通文本里的巧合当成传输开始（终端里 `cat` 一个二进制文件足以命中）。
pub fn detect_start(buf: &[u8]) -> Option<(FrameKind, usize)> {
    for start in 0..buf.len() {
        if buf[start] != ZPAD {
            continue;
        }
        match parse_header(&buf[start..]) {
            Parsed::Header(h, used) => {
                if matches!(h.kind, FrameKind::ZRQINIT | FrameKind::ZRINIT) {
                    return Some((h.kind, start + used));
                }
            }
            Parsed::Incomplete => return None, // 等更多字节，别在半个头上下结论
            Parsed::Bad(_) => continue,
        }
    }
    None
}

/// ZFILE 帧的文件信息（子包内容：`name\0size mtime mode ...`，空格分隔）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileInfo {
    pub name: String,
    pub size: Option<u64>,
}

/// 解析 ZFILE 子包载荷。
///
/// 文件名做**基名化**：对端给的名字可能是 `../../etc/passwd` 或绝对路径，
/// 直接落盘就是任意写。只取最后一段，且拒绝 `.`/`..`——这是 rz 场景下最直接的
/// 攻击面（用户在被入侵的主机上敲了一句 sz）。
pub fn parse_file_info(payload: &[u8]) -> FileInfo {
    let nul = payload
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(payload.len());
    let raw_name = String::from_utf8_lossy(&payload[..nul]).to_string();
    let base = raw_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let name = if base.is_empty() || base == "." || base == ".." {
        String::new()
    } else {
        base
    };
    let size = payload
        .get(nul + 1..)
        .map(|rest| String::from_utf8_lossy(rest).to_string())
        .and_then(|s| {
            s.split_whitespace()
                .next()
                .and_then(|t| t.parse::<u64>().ok())
        });
    FileInfo { name, size }
}

/// 编码一个数据子包（`<escaped data> ZDLE <end> <escaped crc>`）。
/// CRC 的计算**包含**结束类型字节——这是 ZMODEM 规范里最容易漏的一点，
/// 漏了会让对端每个子包都报 CRC 错。
pub fn encode_subpacket(data: &[u8], end: SubpacketEnd, crc32_mode: bool) -> Vec<u8> {
    let mut out = escape(data, true);
    out.push(ZDLE);
    out.push(end.to_u8());
    let mut crc_input = data.to_vec();
    crc_input.push(end.to_u8());
    if crc32_mode {
        out.extend_from_slice(&escape(&crc32(&crc_input).to_le_bytes(), true));
    } else {
        out.extend_from_slice(&escape(&crc16(&crc_input).to_be_bytes(), true));
    }
    out
}

/// 解析一个数据子包。返回 (数据, 结束类型, 消耗字节数)。
pub fn parse_subpacket(buf: &[u8], crc32_mode: bool) -> Parsed2 {
    let crc_len = if crc32_mode { 4 } else { 2 };
    let mut data = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == ZDLE {
            if i + 1 >= buf.len() {
                return Parsed2::Incomplete;
            }
            let next = buf[i + 1];
            if let Some(end) = SubpacketEnd::from_u8(next) {
                // 结束标记：其后是 CRC（转义态）
                let mut crc_raw = Vec::with_capacity(crc_len);
                let mut j = i + 2;
                while crc_raw.len() < crc_len {
                    if j >= buf.len() {
                        return Parsed2::Incomplete;
                    }
                    if buf[j] == ZDLE {
                        if j + 1 >= buf.len() {
                            return Parsed2::Incomplete;
                        }
                        crc_raw.push(buf[j + 1] ^ 0x40);
                        j += 2;
                    } else {
                        crc_raw.push(buf[j]);
                        j += 1;
                    }
                }
                let mut crc_input = data.clone();
                crc_input.push(next);
                let ok = if crc32_mode {
                    crc32(&crc_input)
                        == u32::from_le_bytes([crc_raw[0], crc_raw[1], crc_raw[2], crc_raw[3]])
                } else {
                    crc16(&crc_input) == u16::from_be_bytes([crc_raw[0], crc_raw[1]])
                };
                if !ok {
                    return Parsed2::Bad(j);
                }
                return Parsed2::Subpacket(data, end, j);
            }
            data.push(next ^ 0x40);
            i += 2;
        } else {
            data.push(buf[i]);
            i += 1;
        }
    }
    Parsed2::Incomplete
}

/// 子包解析结果（与帧头的 [`Parsed`] 分开：载荷类型不同，合成一个枚举只会让
/// 调用方每次都要 match 一个不可能出现的分支）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed2 {
    Subpacket(Vec<u8>, SubpacketEnd, usize),
    Incomplete,
    Bad(usize),
}

/// 取消序列：连发 8 个 CAN + 8 个退格（zmodem 惯例，让对端与终端都回到常态）。
pub fn cancel_sequence() -> Vec<u8> {
    let mut v = vec![ZDLE; 8];
    v.extend_from_slice(&[0x08; 8]);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// S327：CRC 两支各自正确——CRC-32 用 CRC-16 的写法照搬多项式是自写实现
    /// 最常见的错，两个已知向量把它钉死。
    #[test]
    fn crc_known_vectors() {
        // CRC-16/XMODEM("123456789") = 0x31C3（公开向量）
        assert_eq!(crc16(b"123456789"), 0x31C3);
        // CRC-32("123456789") = 0xCBF43926（公开向量，与 zip 同参数）
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc16(b""), 0);
        assert_eq!(crc32(b""), 0);
    }

    /// S327：转义/反转义往返，且流控字符必被转义。
    #[test]
    fn escape_roundtrip_and_flow_control() {
        let data: Vec<u8> = (0u8..=255).collect();
        let esc = escape(&data, true);
        assert_eq!(unescape(&esc).unwrap(), data, "全字节域往返必须一致");

        // ZDLE 与 XON/XOFF/DLE（含高位形）一律转义
        for b in [ZDLE, 0x10, 0x11, 0x13, 0x90, 0x91, 0x93] {
            let e = escape(&[b], false);
            assert_eq!(e.len(), 2, "0x{b:02x} 必须被转义");
            assert_eq!(e[0], ZDLE);
            assert_eq!(e[1], b ^ 0x40);
        }
        // 普通可打印字符不转义
        assert_eq!(escape(b"abc", true), b"abc".to_vec());
    }

    /// S327：**半个转义序列**返回 None——SSH 读块会恰好切在 ZDLE 之后，
    /// 当成数据字节会静默污染一字节，而 CRC 要几百字节后才报错。
    #[test]
    fn unescape_reports_incomplete_tail() {
        assert!(unescape(&[b'a', ZDLE]).is_none());
        assert_eq!(unescape(&[b'a', ZDLE, b'P']).unwrap(), vec![b'a', 0x10]);
    }

    /// S328：十六进制头往返。
    #[test]
    fn hex_header_roundtrip() {
        let h = FrameHeader {
            kind: FrameKind::ZRQINIT,
            flags: [0, 0, 0, 0],
        };
        let bytes = h.to_hex();
        assert_eq!(&bytes[..4], &[ZPAD, ZPAD, ZDLE, ZHEX]);
        match parse_header(&bytes) {
            Parsed::Header(got, used) => {
                assert_eq!(got, h);
                assert_eq!(used, bytes.len(), "尾随 CR/LF/XON 须一并消耗");
            }
            other => panic!("期望解析出帧头，实得 {other:?}"),
        }
    }

    /// S328：二进制 CRC-32 头往返（含位置字段小端）。
    #[test]
    fn bin32_header_roundtrip_with_position() {
        let h = FrameHeader {
            kind: FrameKind::ZDATA,
            flags: 0x0001_0203u32.to_le_bytes(),
        };
        assert_eq!(h.position(), 0x0001_0203);
        let bytes = h.to_bin32();
        match parse_header(&bytes) {
            Parsed::Header(got, used) => {
                assert_eq!(got, h);
                assert_eq!(used, bytes.len());
                assert_eq!(got.position(), 0x0001_0203);
            }
            other => panic!("期望解析出帧头，实得 {other:?}"),
        }
    }

    /// S328：字节不足判 Incomplete 而非 Bad——把切块当错误会让传输在正常
    /// 读块边界上随机失败。
    #[test]
    fn truncated_header_is_incomplete_not_bad() {
        let full = FrameHeader {
            kind: FrameKind::ZRINIT,
            flags: [1, 2, 3, 4],
        }
        .to_hex();
        for cut in 1..full.len() - 3 {
            // 尾随 CR/LF/XON 被吞，故末几字节切断仍可能解析成功——只验前半段
            assert_eq!(
                parse_header(&full[..cut]),
                Parsed::Incomplete,
                "截断 {cut} 字节处应判 Incomplete"
            );
        }
    }

    /// S328：CRC 错判 Bad（并给出可跳过的字节数）。
    #[test]
    fn corrupt_crc_is_bad() {
        let mut bytes = FrameHeader {
            kind: FrameKind::ZRINIT,
            flags: [0; 4],
        }
        .to_hex();
        let n = bytes.len();
        bytes[n - 4] ^= 0x01; // 动 CRC 的十六进制字符
        assert!(matches!(parse_header(&bytes), Parsed::Bad(_)));
    }

    /// S329：启动检测只认 ZRQINIT/ZRINIT——拿 `**\x18B` 前缀当判据会把
    /// `cat` 二进制文件的巧合当成传输开始。
    #[test]
    fn detect_start_only_on_init_frames() {
        let sz = FrameHeader {
            kind: FrameKind::ZRQINIT,
            flags: [0; 4],
        }
        .to_hex();
        let rz = FrameHeader {
            kind: FrameKind::ZRINIT,
            flags: [0; 4],
        }
        .to_hex();
        let other = FrameHeader {
            kind: FrameKind::ZACK,
            flags: [0; 4],
        }
        .to_hex();

        let mut stream = b"some output\r\n".to_vec();
        stream.extend_from_slice(&sz);
        let (kind, used) = detect_start(&stream).expect("须检出 ZRQINIT");
        assert_eq!(kind, FrameKind::ZRQINIT);
        assert_eq!(used, stream.len());

        assert_eq!(detect_start(&rz).map(|x| x.0), Some(FrameKind::ZRINIT));
        assert_eq!(detect_start(&other), None, "ZACK 不是启动帧");
        // 裸前缀不算（普通输出里的巧合）
        assert_eq!(detect_start(b"**\x18B not a frame"), None);
        assert_eq!(detect_start(b"plain text"), None);
    }

    /// S330：子包往返，且 CRC **包含结束类型字节**（漏了会让对端每包报错）。
    #[test]
    fn subpacket_roundtrip_crc_covers_end_byte() {
        for crc32_mode in [false, true] {
            for end in [
                SubpacketEnd::NoAck,
                SubpacketEnd::EndFrame,
                SubpacketEnd::AckContinue,
                SubpacketEnd::AckEnd,
            ] {
                let data: Vec<u8> = (0u8..=200).collect();
                let enc = encode_subpacket(&data, end, crc32_mode);
                match parse_subpacket(&enc, crc32_mode) {
                    Parsed2::Subpacket(got, got_end, used) => {
                        assert_eq!(got, data);
                        assert_eq!(got_end, end);
                        assert_eq!(used, enc.len());
                    }
                    other => panic!("crc32={crc32_mode} end={end:?} 解析失败：{other:?}"),
                }
            }
        }
    }

    /// S330：子包被切断判 Incomplete；CRC 坏判 Bad。
    #[test]
    fn subpacket_truncation_and_corruption() {
        let enc = encode_subpacket(b"hello world", SubpacketEnd::EndFrame, true);
        for cut in 1..enc.len() {
            assert_eq!(
                parse_subpacket(&enc[..cut], true),
                Parsed2::Incomplete,
                "截断 {cut} 处应判 Incomplete"
            );
        }
        let mut bad = enc.clone();
        let n = bad.len();
        bad[n - 1] ^= 0xff;
        assert!(matches!(parse_subpacket(&bad, true), Parsed2::Bad(_)));
    }

    /// S331：ZFILE 文件名**基名化**——rz 场景下对端给的名字可能是
    /// `../../etc/passwd`，直接落盘就是任意写。
    #[test]
    fn file_info_basenames_and_rejects_traversal() {
        let mk = |s: &str| {
            let mut v = s.as_bytes().to_vec();
            v.push(0);
            v.extend_from_slice(b"1024 0 0 0");
            v
        };
        assert_eq!(parse_file_info(&mk("a.txt")).name, "a.txt");
        assert_eq!(parse_file_info(&mk("../../etc/passwd")).name, "passwd");
        assert_eq!(parse_file_info(&mk("/abs/path/x.bin")).name, "x.bin");
        assert_eq!(parse_file_info(&mk("C:\\win\\y.dat")).name, "y.dat");
        assert_eq!(parse_file_info(&mk("..")).name, "", "`..` 判非法");
        assert_eq!(parse_file_info(&mk(".")).name, "");
        // 大小如实解析
        assert_eq!(parse_file_info(&mk("a.txt")).size, Some(1024));
        // 无大小字段时为 None（不编造 0——0 会被当成空文件）
        let mut only_name = b"a.txt".to_vec();
        only_name.push(0);
        assert_eq!(parse_file_info(&only_name).size, None);
    }

    /// 取消序列的形状（对端与终端都靠它回常态）。
    #[test]
    fn cancel_sequence_shape() {
        let c = cancel_sequence();
        assert_eq!(c.len(), 16);
        assert!(c[..8].iter().all(|&b| b == ZDLE));
        assert!(c[8..].iter().all(|&b| b == 0x08));
    }
}
