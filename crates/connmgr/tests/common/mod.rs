//! 集成测试共享装置。`tests/common/` 是子目录，cargo 不会把它当成独立测试目标编译。

/// 返回一个**全新创建**的空临时目录。
///
/// S22（high）：历史实现以「进程 id + 进程内计数器」为唯一键，再 `create_dir_all` 落地 ——
/// 这两项在**跨进程、跨轮次**上都不唯一：
///   ① Windows 会积极回收 pid，而 nextest 为每个测试各起一个短命进程，同一轮里
///      不同测试拿到同一个 pid 是常态；两边的计数器又都从 0 起，路径于是逐字相同。
///   ② 这些目录从不清理，`%TEMP%` 里积着历轮跑的成百上千份残留（实测 175 个
///      `fs-connmgr-*-0`，其中 5 个的 `fs.db` 里 `min_app_version = '999.0.0'` ——
///      那是 `refuses_db_requiring_newer_app` 故意写坏的库）。
///   ③ `create_dir_all` 对**已存在**的目录返回 `Ok`，于是测试悄悄跑在前人的终态上。
///
/// 后果是双向的：假红已实测 —— 连续 4 轮 `cargo nextest run --workspace` 各随机挂一条，
/// 报错为 `AppTooOld { min: "999.0.0", cur: "0.1.0" }` 这类与被测代码毫无关系的错，
/// 且每轮挂的还不是同一条（db / repo / vault 都中过），排障时极易误判成产品代码有并发问题；
/// 更坏的一半是**假绿** —— 「解锁失败不得建库」「备份裁剪到 5 份」这类断言一旦跑在
/// 已有库/已有备份的目录上，测的就不再是本次代码的行为，缺陷会被残留数据盖住。
///
/// 修法：名字里加纳秒时间戳，且用 `create_dir`（**目录已存在即报错**）而非 `create_dir_all`，
/// 撞名就换个名字重来 —— 唯一性交给文件系统裁决，不靠「pid 应该不会重」这种猜测。
pub fn tmpdir(prefix: &str) -> std::path::PathBuf {
    static C: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = std::env::temp_dir();
    loop {
        let p = base.join(format!(
            "fs-{prefix}-{}-{}-{}",
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
