//! 本地路径沙箱校验（spec §3.3：SFTP 下载目录沙箱）。
//! Rust 侧是唯一可信边界：前端传入的本地路径与服务端文件名皆为不可信输入，必须在此校验。
use crate::Error;
use std::path::{Path, PathBuf};

/// 本机文件系统是否大小写不敏感。Windows 与 macOS 的默认卷都是。
///
/// 用 `cfg!` 宏而不是 `#[cfg]` 属性：值参与运算而非裁剪代码，依赖它的分支在**所有**平台上
/// 都参与编译与变异校验，不会有一半逻辑在 Linux 上变成死代码。
///
/// 放在 `sandbox` 而不是 `transfer`：它描述的是「本地落盘那一侧」的性质，而落盘一侧有两处
/// 必须用**同一个**取值——同目标互斥的键（`transfer::local_lock_keys`，决定两件并发传输算不算
/// 同一个目标）与落点碰撞检查（`folding_conflict`，决定这一次写会不会盖掉别人）。两处各写
/// 一份 `cfg!` 迟早分叉，分叉出来的窗口正是「锁认为是两个目标、文件系统认为是一个」。
pub const CASE_INSENSITIVE_FS: bool = cfg!(any(target_os = "windows", target_os = "macos"));

// ---------------------------------------------------------------------------
// 远端文件名 → 本地文件名：**可逆**转义（审计2 #14）
// ---------------------------------------------------------------------------

/// Windows 保留设备名（判定与大小写、扩展名均无关）。
///
/// 除文档常列的 CON/PRN/AUX/NUL/COM1-9/LPT1-9 外，还收了两类容易漏的：
/// - `COM¹ COM² COM³ / LPT¹ LPT² LPT³`——`RtlIsDosDeviceName_U` 把上标数字也认成设备号；
/// - `CONIN$ / CONOUT$`——`CreateFileW` 同样把它们解析成控制台句柄。
///
/// 宁可多收不可少收：多收的代价只是某个名字的首字符被转义（仍可逆、仍不碰撞），
/// 少收的代价是服务端可控的文件名把字节写进设备（`NUL` 静默丢弃、`CON` 阻塞）。
const RESERVED_STEMS: &[&str] = &[
    "CON",
    "PRN",
    "AUX",
    "NUL",
    "COM1",
    "COM2",
    "COM3",
    "COM4",
    "COM5",
    "COM6",
    "COM7",
    "COM8",
    "COM9",
    "COM\u{b9}",
    "COM\u{b2}",
    "COM\u{b3}",
    "LPT1",
    "LPT2",
    "LPT3",
    "LPT4",
    "LPT5",
    "LPT6",
    "LPT7",
    "LPT8",
    "LPT9",
    "LPT\u{b9}",
    "LPT\u{b2}",
    "LPT\u{b3}",
    "CONIN$",
    "CONOUT$",
];

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// 这些字符不能原样出现在本地文件名里：`\` 与 `/` 会在 `PathBuf::push` 时长出目录层级，
/// 其余是 Windows 的非法字符，控制字符与 DEL 则在各平台的文件管理器里都属不可显示。
fn must_escape(c: char) -> bool {
    matches!(
        c,
        '%' | '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
    ) || (c as u32) < 0x20
        || c == '\u{7f}'
}

/// 按 UTF-8 逐字节写 `%XX`。写字节而不是码点，解码端才能原样拼回一个合法 UTF-8 串。
fn push_escaped(out: &mut String, c: char) {
    let mut buf = [0u8; 4];
    for b in c.encode_utf8(&mut buf).as_bytes() {
        out.push('%');
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 把**一个**远端路径段（服务端可控输入）转义成一个安全的本地文件名。
///
/// 关键性质是**单射**：不同的远端名字必得不同的本地名字。审计2 #14 命中的原实现恰恰不是——
/// 它把 `: * ? " < > |`、反斜杠、控制字符统统换成同一个 `_`，又把尾部的点和空格直接删掉，
/// 于是 `a:b`、`a?b`、`a_b` 三个在 POSIX 上完全合法且互不相同的文件名，落到本地是同一个
/// `a_b`；`foo.` 与 `foo` 同理。两件先后下载 → 后者静默覆盖前者，两件都报 Done，
/// 用户以为收到了两个文件。带冒号的时间戳文件名（`2026-08-12T10:00:00.log`）在 Linux 上
/// 极常见，这不是只有恶意服务端才碰得到的角落。
///
/// 改法是把「不能原样落地」的字符**编码**掉而不是**抹掉**：仿百分号编码，`%XX` 写 UTF-8 字节，
/// 并且**先把 `%` 自己编码成 `%25`**——这一条是全部可逆性的支点：输出里每一个 `%` 后面必然
/// 跟着两位十六进制，于是 `unescape_local_component` 是它严格的左逆，
/// `unescape(escape(x)) == Some(x)` 恒成立，单射由此是构造出来的而不是碰巧的。
///
/// 三类必须编码的东西：
/// 1. 会长出目录层级或 Windows 不接受的字符（见 `must_escape`）；
/// 2. **结尾**那一串点与空格——Windows 在打开文件时会把它们悄悄剥掉，`foo.` 与 `foo` 于是
///    指向同一个文件。开头与中间的点空格是普通字符，不动（`.bashrc`、`a. b` 原样保留）；
/// 3. 保留设备名：编码**首字符**（`NUL` → `%4EUL`）。原实现加 `_` 前缀，那与一个真名叫
///    `_NUL` 的远端文件撞车——把碰撞从一处挪到了另一处；编码首字符则不会，因为一个真名叫
///    `%4EUL` 的文件会被编码成 `%254EUL`。
///
/// 空输入回空串（调用方负责先剔除空段）；非空输入的输出必然非空——原实现会把 `...`、`   `
/// 清成空串并整条路径拒绝，那是一次可用性损失，现在它们各自有了确定且可逆的像。
pub fn escape_local_component(segment: &str) -> String {
    // 结尾那一串点与空格的起点。`take_while` 从尾部倒着走，`last()` 取到的就是这一串的头。
    // 整个串都是点空格时（`...`）起点为 0，全部编码；没有尾串时 `last()` 为 None，
    // 取 `len()` 让下面的 `i >= tail` 恒不成立。
    let tail = segment
        .char_indices()
        .rev()
        .take_while(|(_, c)| *c == '.' || *c == ' ')
        .last()
        .map_or(segment.len(), |(i, _)| i);
    let mut out = String::with_capacity(segment.len());
    for (i, c) in segment.char_indices() {
        if must_escape(c) || i >= tail {
            push_escaped(&mut out, c);
        } else {
            out.push(c);
        }
    }
    // 保留性看的是**编码后**的名字——那才是 Windows 会去解析的那一串。取首段（首个 `.` 之前）
    // 判定：`CON.txt` 同样命中。`NUL.` 走到这里已经是 `NUL%2E`，其首段不含字面 `.`，
    // 因而整体不匹配任何保留名——正确，因为 Windows 看到的也确实是 `NUL%2E` 这个普通名字。
    if is_reserved_stem(out.split('.').next().unwrap_or("")) {
        // 保留名全是 ASCII 字母数字加 `$`/上标数字，其中没有 `%`，所以首字符必是一个未被编码的
        // 字面字符，直接编码它即可，不会把某个 `%XX` 序列劈成两半。
        let mut chars = out.chars();
        let first = chars.next().expect("保留名非空，故编码后的首字符必然存在");
        let rest: String = chars.collect();
        let mut fixed = String::with_capacity(out.len() + 2);
        push_escaped(&mut fixed, first);
        fixed.push_str(&rest);
        out = fixed;
    }
    out
}

fn is_reserved_stem(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    RESERVED_STEMS.contains(&upper.as_str())
}

/// `escape_local_component` 的左逆：把本地文件名还原成远端路径段。
///
/// 它是**单射性的证明工具**，也是产品能力：抽屉里那个 `%3A` 看不懂的时候，
/// 还原得回 `:` 才说明这套编码没有丢信息。测试里 `unescape(escape(x)) == Some(x)`
/// 跑遍一整片字符集，比逐对断言「这两个不相等」强得多——后者只能覆盖想得到的碰撞。
///
/// 非本函数产出的输入（用户手工建的文件名）可能不合法：`%` 后不足两位十六进制、
/// 或还原出的字节不是合法 UTF-8，都回 None，不做任何猜测性修补。
pub fn unescape_local_component(escaped: &str) -> Option<String> {
    let src = escaped.as_bytes();
    let mut bytes = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'%' {
            let hi = hex_val(*src.get(i + 1)?)?;
            let lo = hex_val(*src.get(i + 2)?)?;
            bytes.push((hi << 4) | lo);
            i += 3;
        } else {
            bytes.push(src[i]);
            i += 1;
        }
    }
    String::from_utf8(bytes).ok()
}

/// 在**大小写折叠**的卷上，`want` 这个名字会不会打开一个拼法不同的既有文件。命中即返回那个既有名。
///
/// 这是审计2 #14 里**转义解决不掉**的那一半：`Report.pdf` 与 `report.pdf` 在服务端是两个文件，
/// 在 Windows/macOS 的默认卷上是**一个**文件。任何清洗函数都改变不了这一点——它是文件系统的
/// 等价关系，不是我们的映射的性质。Task 35 已经把大小写折进同目标锁，堵住了「两件并发交错写」；
/// 剩下的是「先后下载，后者静默覆盖前者」，只能在落点这里判。
///
/// 折叠用 `to_lowercase()`，与 `transfer::local_lock_keys` 逐字相同——两处判据必须一致，
/// 否则会出现「锁认为是两个目标、文件系统认为是一个」的窗口。
///
/// 拼法**完全相同**不算碰撞：那是重新下载/续传覆盖同一个远端文件的常态，必须放行。
/// `fold = false`（大小写敏感的卷）恒不判定：那种卷上两个名字就是两个文件，判了是误伤。
pub fn folding_conflict(existing: &[String], want: &str, fold: bool) -> Option<String> {
    if !fold {
        return None;
    }
    let want_folded = want.to_lowercase();
    existing
        .iter()
        .find(|e| e.as_str() != want && e.to_lowercase() == want_folded)
        .cloned()
}

/// `folding_conflict` 的落盘侧：拒绝会静默盖掉「只差拼法」的既有文件的下载目的地。
///
/// 方向是 fail-closed 拒绝而不是自动改名。改名要么带上哈希后缀（名字变难看，且重下同一个
/// 远端文件时得保证仍落回同一条路径，等于要一份持久映射），要么加序号（重下就每次多一份垃圾）。
/// 拒绝不需要任何状态，报错里两个拼法都点名，用户自己挪走或改名即可继续——比静默丢一个文件好。
///
/// 只在 `dest` 确实存在时才去列目录：新建下载是绝对多数，那条路上只多一次 `stat`。
/// 折叠卷上 `exists()` 本身就是按折叠回答的，所以「不存在」即可断定无碰撞。
/// 列目录失败（权限等）放行——本函数只回答「会不会撞上」这一个问题，
/// 路径不可访问会由紧随其后的 open 如实报出来，在这里抢答只会把失败原因说歪。
fn reject_folding_conflict(dest: &Path, fold: bool) -> Result<(), Error> {
    if !fold || !dest.exists() {
        return Ok(());
    }
    let (Some(parent), Some(name)) = (dest.parent(), dest.file_name()) else {
        return Ok(());
    };
    let want = name.to_string_lossy().into_owned();
    let Ok(rd) = std::fs::read_dir(parent) else {
        return Ok(());
    };
    let names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    match folding_conflict(&names, &want, fold) {
        Some(other) => Err(Error::Transfer(format!(
            "destination would silently replace an existing file that differs only in spelling \
             on this case-insensitive volume: {want} vs {other} (in {})",
            parent.display()
        ))),
        None => Ok(()),
    }
}

/// 校验下载目的位于沙箱根之内（spec §3.3）。
/// `root` 必须已存在（调用方先 create_dir_all）；`dest` 的父目录经 canonicalize
/// （解析符号链接与 `..`）后仍须在 `root` 之内，否则返回 `Error::Transfer` 拒绝。
/// 返回规范化后的绝对目的路径。
///
/// 包含判定用 `Path::starts_with`（**按路径组件**而非字符串前缀）：`<tmp>/sbx-0-evil`
/// 是 `<tmp>/sbx-0` 的字符串前缀延长但不是其子目录，字符串比较会把它误放行。
///
/// **末段自身也必须查**（发现 S42）：canonicalize 只施加于 `parent`，末段是原样 join 回去的。
/// 若沙箱内已存在一个名为 `report.pdf`、指向 `/etc/cron.d/x` 的**文件软链**，父目录校验完全
/// 通过，而随后 `OpenOptions::create(true).write(true).open()` 会**跟随**该软链，把服务端
/// 送来的字节写到沙箱外——恰是 §3.3 要挡的越权写。原 `symlink_escape_rejected` 用例只覆盖了
/// 软链**目录**分量（`<root>/escape/x.bin`），那条被 parent canonicalize 挡住，末段这条没有。
/// 故此处补一次 `symlink_metadata`（不跟随）判定：末段存在且为软链即拒绝。
///
/// 不处理硬链接：远端服务器（本函数要防的输入源）无法在本地建硬链接，能建的本地攻击者
/// 已持有该用户全部权限，挡它没有意义；且 `nlink > 1` 判据在 Windows/网络盘上误报率高。
/// 同理不追求 TOCTOU 无缝——检查与 open 之间的替换同样只有本地同权限进程能做到。
pub fn resolve_within(root: &Path, dest: &Path) -> Result<PathBuf, Error> {
    let root_c = root
        .canonicalize()
        .map_err(|e| Error::Transfer(format!("sandbox root unavailable: {e}")))?;
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            Error::Transfer(format!("destination has no parent dir: {}", dest.display()))
        })?;
    let parent_c = parent
        .canonicalize()
        .map_err(|e| Error::Transfer(format!("destination parent unavailable: {e}")))?;
    if !parent_c.starts_with(&root_c) {
        return Err(Error::Transfer(format!(
            "destination escapes download sandbox: {}",
            dest.display()
        )));
    }
    let file_name = dest.file_name().ok_or_else(|| {
        Error::Transfer(format!(
            "destination is not a file path: {}",
            dest.display()
        ))
    })?;
    let resolved = parent_c.join(file_name);
    // 末段软链 = 沙箱外写入通道（S42）。
    reject_symlink_leaf(&resolved)?;
    // 只差大小写拼法的既有文件 = 静默覆盖（审计2 #14 中转义解决不掉的那一半）。
    // 放在这里而不是放在调用方：本函数是下载落点的**唯一**收口——引擎的 `Down` 分支与
    // app 层的 `transfer_submit` 都必经此处，而两个调用方各写一遍迟早漏一个。
    reject_folding_conflict(&resolved, CASE_INSENSITIVE_FS)?;
    Ok(resolved)
}

/// 拒绝「末段自身是符号链接」的**写入落点**。
///
/// 抽成独立函数，是因为这条防护的正确性只取决于一件事：**查的那个路径，必须就是待会儿真正
/// 被写的那个路径**。而这两者在本引擎里一度并不是同一个——审计 P2 命中的正是这个错位：
///
/// - `resolve_within` 查的是 `<dest>`；
/// - 可下载全程一个字节都不往 `<dest>` 写。字节全部落在 `<dest>.fspart` 上
///   （见 `transfer::local_part`），`<dest>` 只在最后被 `rename` 触碰一次，
///   而 `rename` 恰恰**不跟随**软链。
///
/// 于是防护守住了一条没人写的路径，真正接字节的那条无人看管：沙箱里预先躺着一个
/// `report.pdf.fspart → /home/victim/.ssh/authorized_keys` 的软链，
/// `OpenOptions::create(true).write(true).open()` 就会跟随它把服务端送来的字节写到沙箱外，
/// 而 `resolve_within(<root>, <root>/report.pdf)` 一路绿灯——**远端服务器控制着下载文件名，
/// 也就控制着临时件名**。悬空软链更糟：目标文件当场被凭空创建出来。
///
/// 单测能证明「守卫会拒绝一个软链」，却永远证明不了「守卫被安在了该守的路径上」——
/// 后者只有把一件真实下载跑穿沙箱才测得出来（见 `tests/transfer.rs` 的
/// `download_never_follows_a_symlink_planted_at_the_part_path`）。
///
/// 不存在（`Err`）放行：那是新建下载的常态。其余 stat 错误（权限等）同样放行——本函数只回答
/// 「是不是软链」这一个问题；路径不可访问会由紧随其后的 open 如实报出来，在这里抢答只会
/// 把失败原因说歪。
pub fn reject_symlink_leaf(path: &Path) -> Result<(), Error> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Err(Error::Transfer(format!(
            "destination is a symlink (writes would follow it out of the sandbox): {}",
            path.display()
        ))),
        _ => Ok(()),
    }
}
