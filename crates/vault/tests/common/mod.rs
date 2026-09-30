//! 集成测试共享装置。`tests/common/` 是子目录，cargo 不会把它当成独立测试目标编译。

use std::sync::Once;

static INSTALL: Once = Once::new();

/// 装入进程内 mock 凭据库，**每进程恰好一次**。
///
/// S16（med）：`keyring_core::set_default_store` 是**进程全局**且可重入的开关。
/// 历史实现让每个测试函数各调一次，于是同一个测试二进制里：
///   ① 竞态 —— 测试 A 正在 `Entry::new` 取密钥，测试 B 并发把默认库整个换掉，
///      A 拿到的是另一个库的视图，成败与被测代码无关（偶发假红/假绿）；
///   ② 更糟的是**静默换钥** —— 换库后 `get_password` 返回 `NoEntry`，
///      `open_or_create` 会当作首启**铸一把新主密钥**，于是同目录下先前写入的密文
///      再也解不开：一条与产品代码毫无关系的 `Integrity`，排障时极难归因；
///   ③ 且这一切只被「nextest 每测试独立进程」这一执行器细节挡着 ——
///      任何人一句 `cargo test` 就会踩中，而 `cargo test` 是 Rust 的默认入口。
/// 用 `Once` 收敛成「进程内装一次」：库在整个二进制里恒定，(service, user) 共用一份主密钥，
/// 与真实运行时「一台机器一个 OS 凭据库」的语义也正好一致。
///
/// **同一 `Once` 内还必须把主密钥预先铸好**（下方 ②）。只收敛装载是不够的：
/// mock 库装好后仍是空的，于是多个测试线程各自的首次 `open_or_create` 会同时读到
/// `NoEntry`、各铸一把主密钥、再依次 `set_password` —— 库里只剩最后一位写入者的那把，
/// 先前那些线程手里的 `Store` 却仍用自己那把封记录。落盘的密文与凭据库里的密钥就此错配，
/// 表现为随机某个测试报 `Integrity`。这是与 ① 同源的进程全局竞态，
/// 实测正是 `cargo test -p fs_vault` 下 aad_binding 随机变红的直接原因。
/// 在 `Once` 里先建一次库把条目铸出来，之后所有测试都走「条目已存在」分支，竞态无从发生。
pub fn setup_mock_keyring() {
    INSTALL.call_once(|| {
        // ① keyring 4.x（v1 兼容层）在首次 Entry::new 时一次性装入平台凭据库（会覆盖预设默认库）。
        //    先触发并忽略该一次性逻辑，再装入 mock 库覆盖它。
        let _ = keyring::Entry::new("future-shell-test-trigger", "init");
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
        // ② 走真实建库路径铸一次主密钥，把 (service, user) 条目坐实。
        //    刻意不硬编码 SERVICE/USER 常量：那是 store 的私有实现细节，
        //    抄一份到测试里迟早与产品代码漂移（改了常量测试照绿，才是更坏的结果）。
        drop(fs_vault::Store::open_or_create(&tempdir("seed"), None).unwrap());
    });
}

/// 每次调用返回一个**全新创建**的空目录。
///
/// S22（high）：唯一键不得只用「进程 id + 进程内计数器」。Windows 会积极回收 pid，
/// nextest 又为每个测试各起一个短命进程，且这些目录从不清理 —— `%TEMP%` 里积着历轮的
/// 上千份残留（实测 571 个 `fs-vault-*`），于是新进程一旦拿到被回收的 pid，
/// `create_dir_all` 就会把前人的终态目录原样交回。对 vault 尤其致命：
/// 目录里若已有 `vault.json`，「解锁失败不得建库」「首启建新库」这类断言测的就不是本次行为
/// （**假绿**）；反过来带着别人主密钥封的密文则报 `Integrity`（**假红**）。
/// 实测连续 4 轮 `nextest --workspace` 各随机挂一条即由此而来，完整分析与回归测试见
/// `crates/connmgr/tests/common/mod.rs` 与 `tests/db.rs::tmpdir_never_hands_back_an_existing_directory`。
/// 故：名字加纳秒，且用 `create_dir`（已存在即报错）而非 `create_dir_all`，撞名换名重来。
pub fn tempdir(prefix: &str) -> std::path::PathBuf {
    static C: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = std::env::temp_dir();
    loop {
        let p = base.join(format!(
            "fs-vault-{prefix}-{}-{}-{}",
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
