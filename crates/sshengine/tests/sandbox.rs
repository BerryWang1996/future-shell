//! 下载沙箱校验单测（spec §3.3）。
use fs_sshengine::sandbox::{
    escape_local_component, folding_conflict, resolve_within, unescape_local_component,
    CASE_INSENSITIVE_FS,
};
use std::path::{Path, PathBuf};

/// S59（med）：S22 的跨进程撞名修复**漏掉了本文件**——沙箱根助手曾是「pid + 计数器」并叠加
/// `create_dir_all`：pid 回收后会交回上一轮残留目录（其中可能已有同名文件/兄弟目录，
/// 令 `resolve_within` 用例跑在前人终态上，假红假绿双向发生）。处置同 S22：名字加纳秒、
/// `create_dir`（已存在即报错）撞名重试。
fn sandbox_root(tag: &str) -> PathBuf {
    static C: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = std::env::temp_dir();
    loop {
        let p = base.join(format!(
            "fs-sandbox-{}-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("系统时钟早于 UNIX 纪元")
                .as_nanos(),
            C.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        match std::fs::create_dir(&p) {
            Ok(()) => return p,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("创建测试临时目录失败 {}: {e}", p.display()),
        }
    }
}

/// S59 回归：沙箱根助手绝不得交回已存在的目录（铺 128 槽 + 毒文件，对偶 connmgr S22 回归）。
#[test]
fn sandbox_root_never_hands_back_an_existing_directory() {
    let base = std::env::temp_dir();
    let seeded: Vec<_> = (0..128u64)
        .map(|n| base.join(format!("fs-sandbox-{}-s59-{}", std::process::id(), n)))
        .collect();
    for p in &seeded {
        std::fs::create_dir_all(p).unwrap();
        std::fs::write(p.join("poison.txt"), b"leftover from a previous run").unwrap();
    }
    for _ in 0..8 {
        let d = sandbox_root("s59");
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
fn traversal_destination_rejected() {
    let root = sandbox_root("trav");
    let evil = root.join("..").join("..").join("evil.bin"); // sandbox/../../evil.bin
    assert!(
        resolve_within(&root, &evil).is_err(),
        "../../x 式穿越必须被拒"
    );
    assert!(
        resolve_within(&root, Path::new("../../x")).is_err(),
        "相对穿越必须被拒"
    );
}

#[test]
fn absolute_outside_destination_rejected() {
    let root = sandbox_root("abs");
    let outside = std::env::temp_dir().join(format!("fs-abs-{}.bin", std::process::id()));
    assert!(
        resolve_within(&root, &outside).is_err(),
        "沙箱外绝对路径必须被拒"
    );
}

#[test]
fn inside_destination_accepted() {
    let root = sandbox_root("ok");
    let dest = resolve_within(&root, &root.join("a.bin")).unwrap();
    assert!(dest.starts_with(root.canonicalize().unwrap()));
    assert_eq!(dest.file_name().unwrap(), "a.bin");
}

/// 沙箱根**前缀**同名的兄弟目录不得被误判为「在沙箱内」：
/// `<tmp>/fs-sandbox-pfx-0` 与 `<tmp>/fs-sandbox-pfx-0-evil` 是两个目录，
/// 若拿字符串前缀而非路径组件比较，后者会被放行。
#[test]
fn sibling_dir_sharing_name_prefix_rejected() {
    let root = sandbox_root("pfx");
    let sibling = PathBuf::from(format!("{}-evil", root.display()));
    std::fs::create_dir_all(&sibling).unwrap();
    assert!(
        resolve_within(&root, &sibling.join("x.bin")).is_err(),
        "同前缀兄弟目录必须被拒（须按路径组件而非字符串前缀比较）"
    );
}

#[test]
#[cfg(unix)]
fn symlink_escape_rejected() {
    let root = sandbox_root("lnk");
    let outside_dir = sandbox_root("lnk-target");
    let link = root.join("escape");
    std::os::unix::fs::symlink(&outside_dir, &link).unwrap();
    assert!(
        resolve_within(&root, &link.join("x.bin")).is_err(),
        "符号链接出沙箱必须被拒"
    );
}

/// 末段自身是软链的越权写（S42）。
/// 上面的 `symlink_escape_rejected` 覆盖的是软链**目录分量**（`<root>/escape/x.bin`），
/// 那条被 parent canonicalize 挡住；末段这条挡不住：父目录 `<root>` 校验完全通过，
/// 而随后 `OpenOptions::create(true).write(true).open()` 会**跟随**末段软链，
/// 把服务端送来的字节写到沙箱外。远端服务器控制着下载文件名，只要沙箱里预先存在同名软链
/// （用户下载目录本就可能有），即构成 §3.3 要挡的越权写。
///
/// 仅 unix 编译：Windows 上建**文件**软链需要 SeCreateSymbolicLinkPrivilege 或开发者模式，
/// 普通开发/CI 账户建不出来（本机实测 ERROR_PRIVILEGE_NOT_HELD）。Windows 侧的等价覆盖见
/// 下方 `final_component_reparse_point_rejected`（改用目录联接，无需特权）。
#[test]
#[cfg(unix)]
fn final_component_symlink_rejected() {
    let root = sandbox_root("leaf");
    let outside_dir = sandbox_root("leaf-target");
    let outside_file = outside_dir.join("victim.conf");
    std::fs::write(&outside_file, b"original").unwrap();
    let dest = root.join("report.pdf"); // 服务端可控的下载文件名
    std::os::unix::fs::symlink(&outside_file, &dest).unwrap();
    let err =
        resolve_within(&root, &dest).expect_err("末段软链必须被拒——否则写入会跟随它落到沙箱外");
    let msg = err.to_string();
    assert!(msg.contains("symlink"), "拒绝理由须点名软链: {msg}");
    assert_eq!(
        std::fs::read(&outside_file).unwrap(),
        b"original",
        "沙箱外文件不得被触碰"
    );
}

/// S42 的 Windows 侧覆盖。文件软链在 Windows 上建不出来（需特权），但**目录联接**
/// （junction，`mklink /J`）普通账户就能建，且 Rust 的 `FileType::is_symlink()` 对
/// `IO_REPARSE_TAG_MOUNT_POINT` 同样返回 true——于是它能走通与文件软链完全相同的那条
/// `symlink_metadata` 分支，把该分支在主平台上钉住（否则 Windows 侧此修补零覆盖）。
///
/// 建不出联接即判红而非跳过：静默跳过正是本项目一路在猎杀的假绿。
/// （前提：TEMP 位于 NTFS——Windows 默认如此。）
#[test]
#[cfg(windows)]
fn final_component_reparse_point_rejected() {
    let root = sandbox_root("leaf-win");
    let outside_dir = sandbox_root("leaf-win-target");
    let dest = root.join("report.pdf"); // 服务端可控的下载文件名
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&dest)
        .arg(&outside_dir)
        .status()
        .expect("mklink 应可执行");
    assert!(
        status.success(),
        "目录联接创建失败（TEMP 是否在 NTFS 上？）"
    );
    let err = resolve_within(&root, &dest).expect_err("末段重解析点必须被拒");
    assert!(
        err.to_string().contains("symlink"),
        "拒绝理由须点名软链: {err}"
    );
}

/// 末段软链判定不得误伤正常目的地：不存在的新文件、以及已存在的普通文件（覆盖下载）
/// 都必须继续放行。跨平台执行——这条守住 S42 修补的过度拒绝一侧。
#[test]
fn regular_and_missing_destinations_still_accepted() {
    let root = sandbox_root("leaf-ok");
    let missing = root.join("new.bin");
    assert_eq!(
        resolve_within(&root, &missing)
            .unwrap()
            .file_name()
            .unwrap(),
        "new.bin",
        "尚不存在的目的地是新建下载的常态，必须放行"
    );
    let existing = root.join("old.bin");
    std::fs::write(&existing, b"stale").unwrap();
    assert_eq!(
        resolve_within(&root, &existing)
            .unwrap()
            .file_name()
            .unwrap(),
        "old.bin",
        "已存在的普通文件是覆盖下载的常态，必须放行"
    );
}

/// 审计2 #14 的核心契约：转义**可逆**，因而单射。
///
/// 逐对断言「这两个不相等」只能覆盖想得到的碰撞——原实现的作者显然也想过一些，
/// 却漏掉了 `a:b`/`a?b`。`decode(encode(x)) == x` 跑遍一整片字符集则是构造性的：
/// 只要它成立，两个不同的输入就不可能有相同的输出（否则解码器无从区分）。
#[test]
fn escape_local_component_round_trips_every_character_we_might_meet() {
    let mut corpus: Vec<String> = Vec::new();
    // 单字符 + 首/中/尾三种位置，覆盖整个 ASCII 面（含控制字符、DEL、全部 Windows 非法字符）
    for b in 0u8..=0x7f {
        let c = b as char;
        corpus.push(c.to_string());
        corpus.push(format!("{c}ab"));
        corpus.push(format!("a{c}b"));
        corpus.push(format!("ab{c}"));
    }
    // 编码器自身的输出形状、保留名、点空格、非 ASCII
    for s in [
        "",
        "%",
        "%%",
        "%25",
        "%2E",
        "%4EUL",
        "NUL",
        "nul",
        "CON.txt",
        "CONIN$",
        "COM\u{b9}",
        "console.txt",
        "...",
        "   ",
        ". .",
        ".hidden",
        "a. b",
        "foo.",
        "foo ",
        "报告.pdf",
        "🙂.bin",
        "a\u{7f}b",
        "a\u{0}b",
    ] {
        corpus.push(s.to_string());
    }

    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for s in &corpus {
        let enc = escape_local_component(s);
        assert_eq!(
            unescape_local_component(&enc).as_deref(),
            Some(s.as_str()),
            "{s:?} → {enc:?} 无法还原"
        );
        if !s.is_empty() {
            assert!(!enc.is_empty(), "{s:?} 的像不得为空");
        }
        if let Some(prev) = seen.insert(enc.clone(), s.clone()) {
            assert_eq!(&prev, s, "碰撞：{prev:?} 与 {s:?} 都编码成 {enc:?}");
        }
    }
}

/// 可逆性保证的是「不碰撞」，这一条保证的是「建得出来」——两件事互相独立。
///
/// 这条界限是变异校验逼出来的：把 `must_escape` 里的控制字符那一项改成 `false`，
/// 上面那条可逆性用例**依然全绿**，而且它绿得有道理——控制字符原样穿过去，
/// 编码照样可逆、照样单射。可逆性根本不是控制字符要被转义的理由。
/// 真正的理由是落地：`a\nb` 在终端里显示成两行，`a\0b` 在 Linux 上压根不是合法路径，
/// `a\b` 在 Windows 上会多长出一层目录，`foo.` 被打开时尾点会被悄悄剥掉。
/// 所以这些得由一条独立的用例来钉，而不是指望单射性顺手覆盖。
#[test]
fn escape_local_component_output_is_always_a_single_openable_file_name() {
    let mut corpus: Vec<String> = Vec::new();
    for b in 0u8..=0x7f {
        let c = b as char;
        corpus.push(c.to_string());
        corpus.push(format!("{c}ab"));
        corpus.push(format!("a{c}b"));
        corpus.push(format!("ab{c}"));
    }
    for s in ["...", "   ", "NUL", "con.txt", "报告 ", "🙂.", "a. b", "%"] {
        corpus.push(s.to_string());
    }

    for s in &corpus {
        let enc = escape_local_component(s);
        assert!(
            !enc.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']),
            "{s:?} → {enc:?} 含路径分隔符或 Windows 非法字符"
        );
        assert!(
            !enc.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}'),
            "{s:?} → {enc:?} 含控制字符"
        );
        assert!(
            !enc.ends_with('.') && !enc.ends_with(' '),
            "{s:?} → {enc:?} 以点或空格结尾，Windows 打开时会把它悄悄剥掉"
        );
        // 单一组件：不多不少正好一段。空输入的像是空串，那是零段。
        assert_eq!(
            Path::new(&enc).components().count(),
            usize::from(!enc.is_empty()),
            "{s:?} → {enc:?} 不是单一路径组件"
        );
    }
}

/// 原实现里每一处**有损**替换，逐条钉成回归。这些不是理论角落：
/// 带冒号的时间戳文件名（`2026-08-12T10:00:00.log`）在 Linux 上极常见。
#[test]
fn escape_local_component_maps_apart_what_the_old_sanitizer_merged() {
    // 有损字符替换：`: * ? " < > | \` 与控制字符统统换成同一个 `_`
    let lossy = [
        "a:b", "a?b", "a*b", "a\"b", "a<b", "a>b", "a|b", "a\\b", "a/b", "a_b",
    ];
    let mut seen = std::collections::HashSet::new();
    for s in lossy {
        assert!(
            seen.insert(escape_local_component(s)),
            "{s:?} 与前面的某个撞了"
        );
    }
    assert_eq!(escape_local_component("a:b"), "a%3Ab");
    assert_eq!(escape_local_component("a_b"), "a_b", "下划线本身不该被动");

    // 尾部点与空格被剥离：`foo.` / `foo ` / `foo` 塌成一个
    assert_eq!(escape_local_component("foo"), "foo");
    assert_eq!(escape_local_component("foo."), "foo%2E");
    assert_eq!(escape_local_component("foo "), "foo%20");
    assert_eq!(escape_local_component("foo.. "), "foo%2E%2E%20");

    // 转义字符自身：`%` 必须先被编码，否则整套编码不可解
    assert_eq!(escape_local_component("%"), "%25");
    assert_eq!(escape_local_component("%3A"), "%253A");
    assert_ne!(
        escape_local_component("%3A"),
        escape_local_component(":"),
        "一个真名叫 %3A 的文件不得与名叫 : 的文件同像"
    );
}

/// 开头与中间的点、空格是**普通字符**，不动。
/// 原实现的 `while ends_with('.')` 是从尾部剥的，`.bashrc` 侥幸没事，
/// 但把「尾部」写成「全部」是重构时极易滑过去的一步，这里钉死边界。
#[test]
fn escape_local_component_escapes_only_the_trailing_run_of_dots_and_spaces() {
    assert_eq!(escape_local_component(".hidden"), ".hidden");
    assert_eq!(escape_local_component("a. b"), "a. b");
    assert_eq!(escape_local_component("report.tar.gz"), "report.tar.gz");
    assert_eq!(escape_local_component("a.."), "a%2E%2E");
    assert_eq!(escape_local_component("..."), "%2E%2E%2E", "整串都是尾串");
    assert_eq!(escape_local_component("   "), "%20%20%20");
    assert_eq!(escape_local_component(".. "), "%2E%2E%20");
}

/// 保留设备名大小写不敏感、与扩展名无关：Windows 上写 NUL/CON 静默丢字节或阻塞打开。
/// 消解方式是编码**首字符**而不是加 `_` 前缀——后者与一个真名叫 `_NUL` 的远端文件撞车，
/// 等于把碰撞从一处挪到了另一处。
#[test]
fn escape_local_component_neutralizes_windows_reserved_device_names() {
    assert_eq!(escape_local_component("NUL"), "%4EUL");
    assert_eq!(escape_local_component("con.txt"), "%63on.txt");
    assert_eq!(escape_local_component("COM1.log"), "%43OM1.log");
    assert_eq!(escape_local_component("Lpt9"), "%4Cpt9");
    assert_eq!(
        escape_local_component("CONIN$"),
        "%43ONIN$",
        "控制台句柄同样要挡"
    );
    assert_eq!(
        escape_local_component("COM\u{b9}"),
        "%43OM\u{b9}",
        "上标数字也是设备号"
    );
    assert_eq!(
        escape_local_component("console.txt"),
        "console.txt",
        "非保留名不得误伤"
    );
    assert_eq!(
        escape_local_component("_NUL"),
        "_NUL",
        "普通名字不受消解影响"
    );
    assert_ne!(
        escape_local_component("NUL"),
        escape_local_component("_NUL"),
        "旧实现的 `_` 前缀正是在这里碰撞的"
    );
    // 尾部点先被编码，`NUL.` 于是不再是保留名——Windows 看到的也确实是普通名字 `NUL%2E`
    assert_eq!(escape_local_component("NUL."), "NUL%2E");
}

/// 解码器只接受自己产出的形状，不做猜测性修补：`%` 后不足两位十六进制、
/// 还原出的字节不是合法 UTF-8，一律回 None。用户手工建的文件名可能长这样。
#[test]
fn unescape_local_component_rejects_malformed_input() {
    for bad in ["%", "%2", "%ZZ", "%2Z", "a%", "%FF", "%C3"] {
        assert_eq!(unescape_local_component(bad), None, "{bad:?} 不该被还原");
    }
    assert_eq!(unescape_local_component("a%2Fb").as_deref(), Some("a/b"));
    assert_eq!(
        unescape_local_component("a%2fb").as_deref(),
        Some("a/b"),
        "小写十六进制也收"
    );
    assert_eq!(unescape_local_component("plain").as_deref(), Some("plain"));
}

/// 审计2 #14 里转义解决不掉的那一半：大小写折叠是**文件系统的等价关系**，不是映射的性质。
/// 纯函数形态，`fold` 由参数注入，两个方向在所有平台上都被断言。
#[test]
fn folding_conflict_only_fires_on_folding_volumes() {
    let existing: Vec<String> = ["Report.pdf", "notes.txt", "report.pdf.fspart"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    assert_eq!(
        folding_conflict(&existing, "report.pdf", true).as_deref(),
        Some("Report.pdf"),
        "折叠卷上这次写会静默盖掉 Report.pdf"
    );
    assert_eq!(
        folding_conflict(&existing, "report.pdf", false),
        None,
        "大小写敏感的卷上它们就是两个文件，判了是误伤"
    );
    assert_eq!(
        folding_conflict(&existing, "Report.pdf", true),
        None,
        "拼法完全相同 = 重新下载覆盖同一个远端文件，必须放行"
    );
    assert_eq!(
        folding_conflict(&existing, "brand-new.bin", true),
        None,
        "无同名不得误报"
    );
    assert_eq!(
        folding_conflict(&existing, "report.pdf.FSPART", true).as_deref(),
        Some("report.pdf.fspart"),
        "临时件同样按折叠判"
    );
}

/// 落盘侧：`resolve_within` 必须拒绝会静默盖掉「只差拼法」的既有文件的目的地。
/// 期望按平台分叉，两侧都是被断言的真实行为——在大小写敏感的卷上放行才是对的。
#[test]
fn destination_colliding_only_by_case_is_rejected_on_folding_volumes() {
    let root = sandbox_root("case-fold");
    std::fs::write(root.join("Report.pdf"), b"first download").unwrap();
    let got = resolve_within(&root, &root.join("report.pdf"));
    if CASE_INSENSITIVE_FS {
        let err = got.expect_err("折叠卷上必须拒绝").to_string();
        assert!(err.contains("report.pdf"), "{err}");
        assert!(err.contains("Report.pdf"), "报错要把两个拼法都点名：{err}");
    } else {
        assert!(got.is_ok(), "大小写敏感的卷上它们是两个文件，必须放行");
    }
    // 同拼法覆盖（重新下载）在任何卷上都必须放行——这是误伤一侧的守卫
    assert!(resolve_within(&root, &root.join("Report.pdf")).is_ok());
}
