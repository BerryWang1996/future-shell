//! Task 14：环形缓冲（UTF-8 安全）回归例。
//!
//! 设计口径（spec §0.1/§2.2）：固定字节容量（默认 256 KiB）、最旧字节先丢、
//! `snapshot_text` 仅 best-effort（跳过首部 UTF-8 续体字节、尾部残缺经 lossy
//! 补 U+FFFD），**不作 AI/MCP 数据源**——AI 抽取走 Task 13 网格通道。
//!
//! 断言纪律（S52）：`String` 类型结构性恒为合法 UTF-8，对其 `from_utf8().is_ok()`
//! 是恒真空断言——本文件一切文本断言皆钉**内容**（严格相等 / 无 U+FFFD /
//! 替换符恰一枚），不钉类型系统已保证的性质。

use fs_terminal::ring::{RingBuffer, DEFAULT_RING_BYTES};

#[test]
fn wraps_at_capacity_dropping_oldest_first() {
    let mut r = RingBuffer::new(16);
    r.push(b"AAAAAAAA"); // 8
    r.push(b"BBBBBBBB"); // 8 → 满
    r.push(b"CCCC"); // 覆盖最旧 4 字节
    assert_eq!(
        r.snapshot_bytes(),
        b"AAAABBBBBBBBCCCC".to_vec(),
        "环绕只丢最旧端，余者逐字节保持"
    );
}

#[test]
fn snapshot_text_skips_leading_utf8_continuation_without_replacement() {
    let mut r = RingBuffer::new(8);
    r.push(&[0x9C, 0x8D]); // 孤悬续体字节（非法起始）
    r.push("好".as_bytes()); // E5 A5 BD
    let t = r.snapshot_text();
    assert_eq!(t, "好", "首部续体字节须被整体跳过，不得以 U+FFFD 顶替");
    assert!(!t.contains('\u{fffd}'), "跳过路径不得产生替换字符");
}

#[test]
fn default_capacity_is_256kib() {
    assert_eq!(
        DEFAULT_RING_BYTES,
        256 * 1024,
        "spec §0.1/§2.2 钉死 256 KiB"
    );
}

#[test]
fn push_larger_than_capacity_keeps_only_the_tail() {
    let mut r = RingBuffer::new(4);
    r.push(b"0123456789"); // 单批超容：只留尾部 4 字节
    assert_eq!(r.snapshot_bytes(), b"6789".to_vec(), "超容批取尾部窗口");
    assert_eq!(r.total_written(), 10, "丢弃字节也计入总写入量");
}

#[test]
fn wrap_splitting_a_multibyte_char_still_yields_clean_text() {
    // 生产形状：环绕切割恰好落在多字节字符中段（TCP 分片 + 定容覆盖的日常）。
    // S100（第二裁判 F1）：选字「常」（U+5E38 = E5 B8 B8）令环绕残体首字节
    // B8 ≥ 0xA0、落在续体域上半——旧例「服」（E6 9C 8D）残体 9C < 0xA0 系偶然
    // 落在下半域，把跳过域收窄至 0x80-0x9F 的 mask 变异可躲过全部旧例
    let mut r = RingBuffer::new(8);
    r.push("AAAA常".as_bytes()); // 4 + 3 = 7 字节（常 = E5 B8 B8）
    r.push(b"BBBBBB"); // 6 字节 → 丢最旧 5：四个 A 与 E5，余 B8 B8 + 6 B
    let bytes = r.snapshot_bytes();
    assert_eq!(
        bytes[0], 0xB8,
        "环绕切割须落在「常」的中段续体上且残体首字节落域内上半（前置形状成立）"
    );
    let t = r.snapshot_text();
    assert_eq!(
        t, "BBBBBB",
        "首部续体残骸须被跳过——不得带 U+FFFD 噪音进录制回放"
    );
}

#[test]
fn total_written_counts_dropped_bytes_and_window_stays_exact() {
    let mut r = RingBuffer::new(4);
    r.push(b"abc"); // 3
    r.push(b"def"); // 丢 ab → cdef
    r.push(b"ghi"); // 丢 cde → fghi
    assert_eq!(r.total_written(), 9, "三批全量计入，含被丢字节");
    assert_eq!(r.snapshot_bytes(), b"fghi".to_vec(), "滑动窗口逐字节可预测");
}

#[test]
fn zero_capacity_never_panics_and_stays_empty() {
    let mut r = RingBuffer::new(0);
    r.push(b"");
    r.push(b"small");
    r.push(&[0xFF; 1024]); // 远超 0 容量
    assert!(r.snapshot_bytes().is_empty(), "0 容量缓冲恒空");
    assert_eq!(r.snapshot_text(), "", "0 容量文本快照恒空串");
    assert_eq!(r.total_written(), 5 + 1024, "计数不随容量退化");
}

#[test]
fn truncated_tail_char_is_exactly_one_replacement_best_effort() {
    // best-effort 契约的精确形状：尾部残缺字符恰补一枚 U+FFFD（不多不少），
    // 而非整字吞没或多枚噪音——录制回放里「可见有损」优于「静默丢字」
    let mut r = RingBuffer::new(5);
    r.push(b"AA");
    r.push(&[0xE6, 0x9C]); // 「服」的前两字节，尾部残缺
    assert_eq!(
        r.snapshot_text(),
        "AA\u{fffd}",
        "尾部残缺序列经 lossy 恰补一枚替换字符"
    );
}

#[test]
fn overcapacity_push_onto_nonempty_window_resets_to_exact_cap_tail() {
    // S78（第一裁判 C1）：钉住 ≥cap 支路的 `buf.clear()`——非空窗口收到 ≥cap
    // 单批时旧前缀必须清零（否则窗口永久超容、256 KiB 上界失效）；旧窗口为空
    // 时 clear 为空操作（t4/t7 形状），钉不住此变异，故补非空窗口一拍。
    let mut r = RingBuffer::new(4);
    r.push(b"ab"); // 旧窗口非空
    r.push(b"0123456789"); // 单批超容：只留尾部 4 字节
    assert_eq!(
        r.snapshot_bytes(),
        b"6789".to_vec(),
        "超容批须清零旧前缀、只留尾部 cap 字节"
    );
    r.push(b"qrst"); // 后续小批仍守约（窗口不得线性膨胀）
    assert_eq!(
        r.snapshot_bytes(),
        b"qrst".to_vec(),
        "超容后窗口须即回 cap，后续小批不得叠加扩张"
    );
    assert_eq!(r.total_written(), 16, "2+10+4 全量计入");
}

#[test]
fn leading_invalid_non_continuation_bytes_show_replacement_not_swallowed() {
    // S79（第一裁判 C2）：钉住 snapshot_text 跳过 regime 的「仅」字边界——
    // 仅跳首部【孤悬续体字节】（0x80-0xBF）；首部非法的非续体起始字节
    // （0xC0/0xC1/0xF5-0xFF）须经 lossy 显形 U+FFFD（可见有损优于静默吞字），
    // 不得被一并跳过。旧套件头部非法字节恰全是续体（9C/8D），钉不住此边界。
    let mut r = RingBuffer::new(8);
    r.push(&[0xFF, 0x41]); // 0xFF：非法非续体起始字节
    assert_eq!(
        r.snapshot_text(),
        "\u{fffd}A",
        "首部 0xFF 须经 lossy 显形替换字符，不得被静默吞掉"
    );
    let mut r2 = RingBuffer::new(8);
    r2.push(&[0xC0, 0x80, 0x42]); // 0xC0 非法起始 + 80 续体残骸
    assert_eq!(
        r2.snapshot_text(),
        "\u{fffd}\u{fffd}B",
        "0xC0 须显形（80 续体随其残缺序列各补一枚），不得被整段跳过"
    );
    // S100（第二裁判 F1）：跳过域「0x80-0xBF」的域内上半（0xA0-0xBF）零钉——
    // 旧例先导续体全 < 0xA0（9C/8D 系偶然），mask 变异 `(b & 0xE0) == 0x80`
    //（域收窄至 0x80-0x9F）可躲过新旧全部他例。域内上下界各钉一例：
    // 变异体下 0xA0/0xBF & 0xE0 = 0xA0 ≠ 0x80 → 不跳 → U+FFFD 噪音 → 必红
    let mut r3 = RingBuffer::new(8);
    r3.push(&[0xA0, 0x41]); // 0xA0：域内上半下界
    assert_eq!(
        r3.snapshot_text(),
        "A",
        "0xA0 仍在续体域内（0x80-0xBF）须跳过，不得显形 U+FFFD"
    );
    let mut r4 = RingBuffer::new(8);
    r4.push(&[0xBF, 0x41]); // 0xBF：域内上界
    assert_eq!(
        r4.snapshot_text(),
        "A",
        "0xBF 仍在续体域内（0x80-0xBF）须跳过，不得显形 U+FFFD"
    );
    // S138（第六裁判 L2 low）：S79 注释点名的首部非法非续体起始字节集
    // {0xC0, 0xC1, 0xF5-0xFF} 中仅 0xC0/0xFF 有先导位钉例（r1/r2），0xC1 与
    // 0xF5、0xF6-0xFE 三段先导位零钉——析取式变异
    // `(b & 0xC0) == 0x80 || b == 0xC1` / `|| b == 0xF5` /
    // `|| matches!(b, 0xF6..=0xFE)` 于全 18 旧例常绿（把这些字节当续体跳过）。
    // 点名集余下边界三钉：须皆经 lossy 显形 U+FFFD → 三变异体各必红一例
    let mut r5 = RingBuffer::new(8);
    r5.push(&[0xC1, 0x41]); // 0xC1：overlong 编码非法起始（点名集下段）
    assert_eq!(
        r5.snapshot_text(),
        "\u{fffd}A",
        "0xC1 非法起始非续体：须显形 U+FFFD 不得跳过（钉变异 `|| b == 0xC1`）"
    );
    let mut r6 = RingBuffer::new(8);
    r6.push(&[0xF5, 0x41]); // 0xF5：> U+10FFFF 非法起始（上段下界）
    assert_eq!(
        r6.snapshot_text(),
        "\u{fffd}A",
        "0xF5 非法起始非续体：须显形 U+FFFD 不得跳过（钉变异 `|| b == 0xF5`）"
    );
    let mut r7 = RingBuffer::new(8);
    r7.push(&[0xFE, 0x41]); // 0xFE：非法起始域上界（0xFF 已由 r1 钉）
    assert_eq!(
        r7.snapshot_text(),
        "\u{fffd}A",
        "0xFE 非法起始非续体：须显形 U+FFFD 不得跳过（钉变异 `|| matches!(b, 0xF6..=0xFE)`）"
    );
}

#[test]
fn ascii_and_control_leads_are_never_treated_as_continuations() {
    // S104（第三裁判 F2）：掩码 `(b & 0xC0) == 0x80` 的唯一零钉象限是 b7=0∧b6=0
    // （0x00-0x3F：ASCII/控制字节）——丢 b7 高位检查的变异 `(b & 0x40) == 0`
    // 与全部旧例的导/止字节同真值（旧例先导续体 9C/8D/B8/A0/BF 皆 b6=0 且 b7=1，
    // 止点 E5/42/41/FF/C0 皆 b6=1），唯此象限是差异点：变异体
    // 下 ASCII 先导字节被误判续体而静默吞没。终端快照先导最高频形状恰是 ESC
    // （spec §2.2 L121「控制序列原样留存」），吞之即录制回放失真。象限下界
    // （0x00）/ ESC / LF / 上界（0x3F）四钉——变异体下皆被吞 → 必红
    let mut r = RingBuffer::new(8);
    r.push(&[0x1B, 0x41]); // ESC + 'A'
    assert_eq!(
        r.snapshot_text(),
        "\u{1b}A",
        "ESC（0x1B）非续体字节，须原样留存，不得被吞"
    );
    let mut r2 = RingBuffer::new(8);
    r2.push(&[0x0A, 0x42]); // LF + 'B'
    assert_eq!(
        r2.snapshot_text(),
        "\nB",
        "LF（0x0A）非续体字节，须原样留存，不得被吞"
    );
    let mut r3 = RingBuffer::new(8);
    r3.push(&[0x00, 0x31]); // NUL（象限下界）+ 数字 '1'
    assert_eq!(
        r3.snapshot_text(),
        "\u{0}1",
        "NUL（0x00，象限下界）非续体字节，须原样留存，不得被吞"
    );
    let mut r4 = RingBuffer::new(8);
    r4.push(&[0x3F, 0x43]); // '?'（象限上界）+ 'C'
    assert_eq!(
        r4.snapshot_text(),
        "?C",
        "0x3F（象限上界）非续体字节，须原样留存，不得被吞"
    );
}

#[test]
fn window_ending_in_continuation_bytes_is_fully_skipped_to_empty() {
    // S105（第三裁判 F1）：跳过循环上界 `start < len` 的 −1 边界变异
    // （`start + 1 < len`）零钉——旧例先导续体游程从不触及 index len−1
    // （尾部皆带非续体后缀提前止跳），窗口以续体字节**收尾**的形状无钉。
    // 钉纯续体窗口：「常」（E5 B8 B8）单批 ≥cap 留尾 [B8, B8]，跳过须穷尽
    // 得空串；变异体留末字节 → lossy 显形 U+FFFD → 必红
    let mut r = RingBuffer::new(2);
    r.push("常".as_bytes()); // E5 B8 B8，单批 ≥cap → 尾 2 字节
    assert_eq!(
        r.snapshot_bytes(),
        vec![0xB8, 0xB8],
        "前置形状：窗口恰以纯续体字节对收尾"
    );
    assert_eq!(
        r.snapshot_text(),
        "",
        "续体游程触及窗口末尾时须全部跳过，不得残留末位 U+FFFD"
    );
}

#[test]
fn drain_branch_fires_at_exact_free_plus_one_boundary() {
    // S118（第四裁判确认 med）：排水支路谓词 `bytes.len() > free`（ring.rs L45）
    // 的边界值 `== free + 1` 在旧 12 例从未采样——凡入排水支路者溢出量恒 ≥2
    //（t1 溢 4、t5 溢 5、t6 两拍各溢 2/3），凡不入者余量恒 ≥0，故 `> free + 1`
    // 变异体在全部旧例存活。机制：边界一拍漏排后 free = cap.saturating_sub(len)
    // 饱和归零，其后每笔 1 字节批 `1 > 0 + 1` 恒假 → 次次漏排 → 窗口单调无界增长，
    // 256 KiB 上限失守，S103 不变式 `buf.len() ≤ cap` 静默变假（旧套件无断言观测）。
    // 边界钉例：cap=4，窗口 3 字节（free=1），二批 2 字节恰 == free + 1——
    // 基线须排最旧 1 字节得 "bcde"；变异体漏排得 "abcde"（len 5 > cap）→ 必红
    let mut r = RingBuffer::new(4);
    r.push(b"abc"); // 窗口 len 3，free = 1
    r.push(b"de"); // 2 == free + 1：排水边界点本身
    assert_eq!(
        r.snapshot_bytes(),
        b"bcde".to_vec(),
        "批长恰为 free + 1 时须排最旧 1 字节（最小排水量），窗口不得超容"
    );
    assert_eq!(r.total_written(), 5, "两批全量计入，含被排字节");
}

#[test]
fn window_never_exceeds_capacity_under_single_byte_push_storm() {
    // S118 姊妹钉：排水漏排变异是单调的——首次边界漏排后 free 饱和归零，其后
    // 每笔 1 字节批 `1 > 0 + 1` 恒假 → 永久漏排、窗口线性膨胀（实测 cap=8 +
    // 40 单笔 → len 40）。旧例最多三拍快照，S103 不变式（buf.len() ≤ cap）写在
    // src 注释里却无例观测。41 笔单字节 + 逐拍在界断言把不变式钉成活体：
    // 变异体第 9 拍（len 9）即红。笔数取 41（奇）非 40：偶数笔下 M-BL 类
    // 多排变异（drain(0..drop+1)）与基线丢弃总量偶然相等（16×2 = 32×1）、
    // 末窗巧同而逃逸；奇数笔令末拍落在排水事件上，长度与内容皆可分辨
    let mut r = RingBuffer::new(8);
    for i in 0..41u8 {
        r.push(&[b'a' + (i % 26)]);
        let n = r.snapshot_bytes().len();
        assert!(n <= 8, "S103 不变式：第 {i} 批后窗口长 {n} 不得超 cap 8");
    }
    assert_eq!(r.total_written(), 41, "41 笔单字节全量计入");
    assert_eq!(
        r.snapshot_bytes(),
        b"hijklmno".to_vec(),
        "饱和窗口恰在 cap 上且内容为序末 8 字节（i=33..41 → 'h'..'o'）"
    );
}

#[test]
fn continuation_domain_is_inclusive_at_its_0x80_lower_edge() {
    // S121（第五裁判确认 low）：续体域 0x80-0xBF 的**全域下界 0x80 本身**从无例采样——
    // S100 钉的是上半域两界（0xA0/0xBF），旧例先导续体为 9C/8D/B8（皆 > 0x80），
    // 域下界外的 0x7F（DEL，ASCII 控制）亦无例。任何把 0x80 排出跳过域的变异
    //（如 `b > 0x80 && (b & 0xC0) == 0x80` 之流）在全部旧例存活。三钉封死下界两侧：
    // 0x7F 域外恰下须留存（不得被跳），0x80 域下界、0x81 域内恰上皆须跳过
    let mut r = RingBuffer::new(8);
    r.push(&[0x7F, 0x41]); // 0x7F：域外恰下（DEL）
    assert_eq!(
        r.snapshot_text(),
        "\u{7f}A",
        "0x7F 非续体字节（域外恰下），须原样留存，不得被跳"
    );
    let mut r2 = RingBuffer::new(8);
    r2.push(&[0x80, 0x42]); // 0x80：全域下界本身
    assert_eq!(
        r2.snapshot_text(),
        "B",
        "0x80 是续体域下界（0x80-0xBF 闭域），须被跳过，不得显形 U+FFFD"
    );
    let mut r3 = RingBuffer::new(8);
    r3.push(&[0x81, 0x43]); // 0x81：域内恰上
    assert_eq!(
        r3.snapshot_text(),
        "C",
        "0x81 在续体域内（0x80-0xBF），须被跳过，不得显形 U+FFFD"
    );
}

#[test]
fn single_byte_pure_continuation_window_is_skipped_to_empty() {
    // S122（第五裁判 utf8-contract 镜确认 low）：snapshot_text 的窗口长退化形
    // 钉例覆盖两端而漏中间——t7 钉 len0（0 容量恒空串）、t12 钉 len2（纯续体对
    // 跳尽得空串），而 len==1 窗口从无例采样（全 15 例窗口长集合 {0,2,3,4,5,8}，
    // 无 1）。语句插入类变异「退化快速路径」`if self.buf.len() < 2 { return lossy }`
    //（看似无害的防御捷径：t7 空窗口 lossy([])=="" 仍绿、t12 len2 走原路径仍绿）
    // 对全套件旧例零红，仅在 len1 纯续体窗口发散为孤悬 U+FFFD。钉例形状系生产
    // 日常：TCP 分片 + 小窗截断恰落于多字节字符末续体
    let mut r = RingBuffer::new(1);
    r.push("常".as_bytes()); // E5 B8 B8 单批 ≥ cap → ≥cap 早退支路留尾 1 字节 B8
    assert_eq!(
        r.snapshot_bytes(),
        vec![0xB8],
        "前置形状：窗口恰为单枚纯续体字节"
    );
    assert_eq!(
        r.snapshot_text(),
        "",
        "len1 纯续体窗口须整体跳过得空串，不得经快速路径退化为 U+FFFD"
    );
}

#[test]
fn trailing_lone_continuation_without_leader_shows_replacement_not_stripped() {
    // S123（第五裁判 mutation 镜确认 low）：t8 是全套件唯一的尾部 regime 钉例，
    // 其尾形状 [E6, 9C] 的残缺序列必含起始字节 E6——这使「剥除尾部续体游程、
    // 仅当其前方字节非 UTF-8 起始字节（< 0xC2）」的实现变异在全部 15 例上输出
    // 与基线逐例相同（t8 中 9C 前方 E6 ≥ 0xC2 → 保留 → 仍产 "AA\u{fffd}"），
    // 契约「尾部残缺经 lossy 显形 / 可见有损优于静默吞字」的尾半句零承重。
    // 钉无前导起始字节的尾部孤悬续体：基线 lossy → "A\u{fffd}"，变异体静默
    // 剥除 → "A" → 必红
    let mut r = RingBuffer::new(4);
    r.push(&[0x41, 0x80]); // 'A' + 孤悬续体（前方 0x41 非起始字节）
    assert_eq!(
        r.snapshot_text(),
        "A\u{fffd}",
        "尾部孤悬续体须经 lossy 显形 U+FFFD，不得被静默剥除"
    );
}

#[test]
fn leading_continuation_run_beyond_utf8_max_len_is_fully_skipped() {
    // S124（第五裁判 mutation 镜确认 low；S105 残余缺口）：t12 钉住跳过上界的
    // −1 变异（`start + 1 < len`），但未钉「上界不存在任何固定常数界」——全 15
    // 例先导续体游程最长仅为 2（t2: 9C 8D；t5/t12: B8 B8；t10/S121: 单枚），
    // 故有界变异 `start < N`（N ∈ {2,3,4,5}）在全部旧例上与基线逐例相同；
    // N=3/4 恰由「UTF-8 字符最长 4 字节 → 环绕孤悬续体至多 3」这一合理但错误
    // 的推理可导出（二进制垃圾流中 0x80-0xBF 长游程不受此限——生产形状：
    // cat 二进制文件后窗口头落在高位字节游程上）。游程长 6 一枚钉死 N ≤ 5 全族
    let mut r = RingBuffer::new(8);
    r.push(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x41]); // 先导续体游程长 6 + 'A'
    assert_eq!(
        r.snapshot_text(),
        "A",
        "任意长先导续体游程须无界全跳，不得残留 U+FFFD 噪音"
    );
}

#[test]
fn leading_valid_start_truncated_sequence_is_surfaced_not_skipped() {
    // S137（第六裁判 L1 low）：snapshot_text 跳过 regime 的「仅跳首部孤悬续体
    // 字节」契约对【合法起始字节打头的残缺序列】零钉——t8 的尾部 [E6,9C] 残缺
    // 与首部残缺在 lossy 下输出不同，但没有任何一例的首部落在「合法起始 + 不
    // 足续体 + ASCII」形状上。「尽力 best-effort」变异体（跳过首部续体游程后，
    // 若窗口首字节是合法多字节起始 0xC2-0xF4 且其后续体游程不足所需长度，则
    // 连这段残缺也一并跳过）于全 20 旧例逐例同基线（怀疑者实证），唯此形状
    // 发散：基线 lossy 显形 U+FFFD，变异体静默吞字
    let mut r = RingBuffer::new(8);
    r.push(&[0xE6, 0x9C, 0x41]); // 3 字节字符「本」系残缺（E6 9C + ASCII）：合法起始 E6 + 续体不足
    assert_eq!(
        r.snapshot_text(),
        "\u{fffd}A",
        "首部合法起始的残缺序列须经 lossy 显形 U+FFFD，不得被「尽力 best-effort」跳过（E6 续体需 2 仅 1）"
    );
    let mut r2 = RingBuffer::new(8);
    r2.push(&[0xF0, 0x9F, 0x98, 0x41]); // 4 字节字符残缺（F0 9F 98 + ASCII）：续体需 3 仅 2
    assert_eq!(
        r2.snapshot_text(),
        "\u{fffd}A",
        "4 字节起始的残缺序列同须显形，不得跳过（F0 续体需 3 仅 2）"
    );
    let mut r3 = RingBuffer::new(8);
    r3.push(&[0x80, 0xE6, 0x9C, 0x41]); // 复合：先导孤悬续体 80（须跳）+ 合法起始残缺（须显形）
    assert_eq!(
        r3.snapshot_text(),
        "\u{fffd}A",
        "先导续体跳尽后，合法起始残缺仍须显形——两 regime 不得合并吞字"
    );
}
