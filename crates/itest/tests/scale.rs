//! 规模与资源门禁（审计2 #42 的可门禁那一半）。
//!
//! perf.rs 里的冷启动/内存/CPU 观测是**本机自查线**（GUI + 产物前置，CI 不跑，也不得当
//! 发布证据引用）。本文件补上**能在共享 runner 上稳定跑**的那一半，全部非 `#[ignore]`、
//! 随 `FS_ITEST=1` 的容器化 itest 进入 CI 与发布门禁（ci.yml 的 Linux nextest 步骤）：
//!
//! - **规模正确性**（无墙钟断言，共享 runner 上不 flaky）：千级文件目录、多文件往返完整性。
//! - **吞吐**：64 MiB 双向往返，按**合计**口径断言 ≥20 MB/s——M1 出口标准
//!   「SFTP ≥20MB/s」的载体。此前这里只有一条 300s 的墙钟下限（≈0.2 MiB/s），
//!   比出口原文松两个数量级，等于那半句没有任何断言背书。
//!   为什么是合计而不是单向、以及为什么不再加单向闸，见该用例内的推导与实测数据。
//!
//!   **2026-09-30 复查**（1.0.0 候选首次 CI 的 ubuntu runner 三次 14.3/19.2/18.9 MB/s 未过闸）：
//!   本机同代码复测只有 9–15 MB/s，而 08-23 定闸时的旧提交在同机同环境交替复测同样 9–15——
//!   环境（Docker Desktop 转发）变慢了，不是回归。但同环境 OpenSSH 顺序写读（`sftp -R 1`）
//!   有 34–56 MB/s，差距是真的，两处根因已修：① `write_at` 用 `flush()` 收尾，russh-sftp 的
//!   `flush` 在服务端支持 `fsync@openssh.com` 时**每块都 fsync**（上行 4.5–9 → 17–27 MB/s）；
//!   ② 分块 256 KiB 被 OpenSSH 的 261 120 字节读写上限切成两个请求，下行每块多一次往返
//!   （对齐到 255 KiB 后下行 13–15 → 18–20 MB/s）。剩下的差距在每块一次 OPEN/CLOSE 往返，
//!   要跨块复用句柄，属结构改动，留在 1.0.0 之后。
//!
//!   **1.0.1**：跨块复用句柄已做（上传流水线写入器、下载并发窗口 + 读句柄复用），产品的传输
//!   路径从此是 `TransferManager`，不再是逐块 `write_at` / `read_range`。吞吐用例随之改测
//!   产品路径（生产组装 `TimedSftp(RemoteSftp)`）。同一天 main 上的发布试跑里，逐块口径在
//!   ubuntu runner 上测得 19.3 MB/s——恰好压在闸线上，时过时不过。
//!   其余上限类断言（冷启动、内存）仍留在 perf.rs 的自查线：共享 runner 上测那些只会制造噪声。
//!
//! 与 perf.rs 同口径：需 `FS_ITEST=1` + Docker，未设时打印 skip 并返回。
use fs_itest::sshd::SshdContainer;
use fs_sshengine::sftp::{Entry, RemoteSftp, SftpOps};
use fs_sshengine::timeouts::TimedSftp;
use fs_sshengine::transfer::{Direction, TransferJob, TransferManager, TransferState};
use russh::client;
use std::sync::Arc;
use std::time::Instant;

struct AcceptAllKeys;
impl russh::client::Handler for AcceptAllKeys {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        _server_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

async fn open_sftp(tag: &str) -> (SshdContainer, RemoteSftp) {
    let sshd = SshdContainer::start(tag).await.unwrap();
    let mut session = client::connect(
        Arc::new(client::Config::default()),
        sshd.addr().await,
        AcceptAllKeys,
    )
    .await
    .unwrap();
    let res = session
        .authenticate_password(&sshd.username, &sshd.password)
        .await
        .unwrap();
    assert!(res.success(), "password auth failed: {res:?}");
    let channel = session.channel_open_session().await.unwrap();
    channel.request_subsystem(true, "sftp").await.unwrap();
    let sftp = RemoteSftp::new(channel).await.unwrap();
    (sshd, sftp)
}

/// 确定性伪随机块：同一 (seed, index) 恒产生同一批字节，两端可各自重算——
/// 不落盘、不进内存放大，只持一个块。
fn chunk(seed: u64, index: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_add(index.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// 千级文件目录：创建 1200 个文件后列表必须**完整**（1200 < LIST_ENTRY_CAP=20000，
/// 不触发截断；若实现把 cap 缩到 1200 以内，此例立即转红——cap 是产品边界，改小了
/// 必须连带改这里与 FE 横幅契约）。
#[tokio::test(flavor = "multi_thread")]
async fn thousand_file_directory_listing_is_complete() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (_sshd, sftp) = open_sftp("scale-listing").await;
    sftp.mkdir("bigdir").await.unwrap();
    for i in 0..1200 {
        sftp.write_at(&format!("bigdir/f{i:04}"), 0, b"x")
            .await
            .unwrap();
    }
    let listing = sftp.list("bigdir").await.unwrap();
    assert!(!listing.truncated, "1200 条不该触发 20000 的上限截断");
    assert_eq!(listing.entries.len(), 1200);
    let mut names: Vec<String> = listing.entries.iter().map(|e| e.name.clone()).collect();
    names.sort();
    for (i, name) in names.iter().enumerate() {
        assert_eq!(*name, format!("f{i:04}"), "第 {i} 个条目缺失或错序");
    }
}

/// 多文件往返完整性：300 个 2 KiB 确定性文件，写上去再整读回来，逐字节一致。
/// 这是「海量小文件」在共享 runner 上可负担的上界（300 次往返 ≈ 数秒）；
/// 更高数量级的吞吐留给 perf.rs 自查线。
#[tokio::test(flavor = "multi_thread")]
async fn three_hundred_small_files_roundtrip_integrity() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (_sshd, sftp) = open_sftp("scale-smallfiles").await;
    sftp.mkdir("small").await.unwrap();
    for i in 0..300u64 {
        let data = chunk(i, 0, 2048);
        sftp.write_at(&format!("small/f{i:03}"), 0, &data)
            .await
            .unwrap();
    }
    for i in 0..300u64 {
        let got = sftp
            .read_range(&format!("small/f{i:03}"), 0, 2048)
            .await
            .unwrap();
        assert_eq!(got, chunk(i, 0, 2048), "文件 {i} 往返不一致");
    }
}

/// 大文件往返：完整性 + **双向吞吐**（M1 出口「SFTP ≥20MB/s」的载体）。
///
/// 修复前这里只有一条 300s 的墙钟下限（≈0.2 MiB/s），比出口原文写的 20MB/s 松两个数量级
/// ——出口那半句没有任何断言背书。而且整个计时区间里夹着 256 次 `assert_eq!` 逐块比对，
/// 算出来的「吞吐」含比对开销，本身也不是可引用的数字。
///
/// 现在分三段：上传、下载、比对。**只有两次传输进计时**，比对挪到计时之外。
///
/// **测的是产品路径**（1.0.1 起）：`TransferManager` + 生产组装 `TimedSftp(RemoteSftp)`，
/// 即用户点「上传 / 下载」时走的那一条——临时件、身份记录、提交前落盘与改名全在计时里。
/// 1.0.0 时这里测逐块 `write_at` / `read_range`，那时它就是产品的切法；1.0.1 之后产品
/// 改走流水线，再测逐块调用就是「另写一个数」。
///
/// 闸值取 20 MB/s（十进制，与出口原文同口径）。若哪天共享 runner 上稳定跑不到，正确的动作
/// 是**如实记录并调整出口标准**，而不是把闸悄悄降到零。
#[tokio::test(flavor = "multi_thread")]
async fn large_file_roundtrip_integrity_and_throughput() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    const CHUNK: usize = fs_sshengine::transfer::CHUNK;
    const N: u64 = 256; // 64 MiB 量级
    const TOTAL: f64 = (CHUNK as u64 * N) as f64;
    /// 出口标准原文的数字（十进制 MB/s）。
    const MIN_MB_PER_SEC: f64 = 20.0;

    let (_sshd, sftp) = open_sftp("scale-large").await;
    let ops: Arc<dyn SftpOps> = Arc::new(TimedSftp::new(Arc::new(sftp)));
    let mgr = TransferManager::spawn(ops.clone(), 1);
    let mut ev = mgr.events().await;
    let dir = tempfile::tempdir().unwrap();

    // ⓪ 预热（不计时）：容器刚起来时首批往返要付 sshd 冷启动、SFTP 子系统拉起、
    // 页缓存未命中的账。出口标准问的是稳态吞吐，不是冷启动——把这笔账算进去，
    // 测出来的数字会随宿主当时的调度抖三倍（实测同一台机器 18.5～53.4 MB/s）。
    let warm: Vec<u8> = (0..8u64).flat_map(|i| chunk(0xBEEF, i, CHUNK)).collect();
    let warm_src = dir.path().join("warmup.bin");
    std::fs::write(&warm_src, &warm).unwrap();
    run(&mgr, &mut ev, job(Direction::Up, warm_src, "warmup.bin")).await;
    let warm_back = dir.path().join("warm-back").join("warmup.bin");
    std::fs::create_dir_all(warm_back.parent().unwrap()).unwrap();
    run(&mgr, &mut ev, job(Direction::Down, warm_back, "warmup.bin")).await;

    let payload: Vec<u8> = (0..N).flat_map(|i| chunk(0xC0FFEE, i, CHUNK)).collect();
    let src = dir.path().join("big.bin");
    std::fs::write(&src, &payload).unwrap();
    let dest = dir.path().join("down").join("big.bin");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();

    // ① 上行、② 下行：各自一件完整的传输
    let up = run(&mgr, &mut ev, job(Direction::Up, src, "big.bin")).await;
    let down = run(&mgr, &mut ev, job(Direction::Down, dest.clone(), "big.bin")).await;

    // ③ 比对（不计时）：下载回来的文件逐字节等于源
    let got = std::fs::read(&dest).unwrap();
    assert_eq!(got.len(), payload.len(), "下载回来的长度不符");
    if let Some(i) = got.iter().zip(&payload).position(|(a, b)| a != b) {
        panic!("下载回来的内容在偏移 {i} 处与源不符");
    }

    let up_mbps = TOTAL / 1e6 / up.as_secs_f64();
    let down_mbps = TOTAL / 1e6 / down.as_secs_f64();
    let total_mbps = 2.0 * TOTAL / 1e6 / (up + down).as_secs_f64();
    println!(
        "SFTP 64 MiB：上行 {up:?}（{up_mbps:.1} MB/s）/ 下行 {down:?}（{down_mbps:.1} MB/s）\
         / 合计 {total_mbps:.1} MB/s"
    );

    // 闸设在**合计**上，不设在单向上。这不是为了好过，是因为单向数字测不准：
    // 本机 5 连跑里 4 次是「上行 ~25 / 下行 ~50」，1 次是「上行 38 / 下行 18.7」，
    // 而两种模式的**总耗时几乎相同**（3.97–5.35s）。那一次不是下行退化，是写回缓存
    // 把 flush 的账从写挪到了读——写得越快，随后的读越要等落盘。加预热也消不掉
    // （预热前后同样出现）。合计口径下同样 11 连跑只在 24.1–34.4 MB/s 之间（1.4 倍），
    // 单向口径是 2.7 倍。拿抖 2.7 倍的量设闸，只会得到一个被反复重跑掩盖的红。
    assert!(
        total_mbps >= MIN_MB_PER_SEC,
        "SFTP 合计吞吐 {total_mbps:.1} MB/s 低于出口标准 {MIN_MB_PER_SEC} MB/s\
         （上行 {up:?} / 下行 {down:?}）。1.0.0 的逐块口径在 ubuntu runner 上 19.3–22 MB/s；\n         1.0.1 改测产品路径（流水线）。跌破即两种可能：真实回归，或环境比定闸时慢一个档——\n         后者的正确动作是如实记录并重估出口标准，不是把闸调低"
    );
    // 这里**不再**加单向闸。原本写了一条 5 MB/s 的「病态闸」防「一向塌了、另一向富余」，
    // 变异验证时发现它**永远不可能触发**——合计闸已经蕴含了单向下界：
    //   合计 = 2T/(t_up+t_down) ≥ 20  ⇒  t_up+t_down ≤ T/10
    //   而 t_down ≤ t_up+t_down ≤ T/10  ⇒  单向 = T/t_down ≥ 10 MB/s
    // 即：任何能过合计闸的跑法，两个方向都已经 ≥10 MB/s。要让单向闸可触发就得把它
    // 抬到 10 以上，那又退回成前面测过的那个抖 2.7 倍的口径。留一条永不可能红的断言，
    // 比没有断言更坏：它会被当成一层防护。
}

fn job(direction: Direction, local: std::path::PathBuf, remote: &str) -> TransferJob {
    TransferJob {
        direction,
        sandbox_root: match direction {
            Direction::Up => None,
            Direction::Down => local.parent().map(|p| p.to_path_buf()),
        },
        local,
        remote: remote.into(),
        resume: false,
        target_endpoint: "scale".into(),
        endpoint_aliases: Vec::new(),
        verify: None,
    }
}

/// 跑一件传输到终态并返回耗时；失败直接 panic（吞吐门禁不吞错）。
async fn run(
    mgr: &TransferManager,
    ev: &mut tokio::sync::mpsc::Receiver<fs_sshengine::transfer::TransferEvent>,
    job: TransferJob,
) -> std::time::Duration {
    let t = Instant::now();
    let id = mgr.submit(job).await.expect("submit");
    loop {
        let e = tokio::time::timeout(std::time::Duration::from_secs(300), ev.recv())
            .await
            .expect("300 s 内没等到下一条传输事件")
            .expect("事件通道关闭");
        if e.id != id {
            continue;
        }
        match e.state {
            TransferState::Done => return t.elapsed(),
            TransferState::Running | TransferState::Retrying { .. } => {}
            other => panic!("传输未成功：{other:?}"),
        }
    }
}

/// 列表条目的字段完整性抽查：千级目录里随便挑几个，名字/目录位/大小都得对——
/// 规模对了但字段错了同样是坏列表（与 thousand_file_directory_listing_is_complete 互补）。
#[tokio::test(flavor = "multi_thread")]
async fn listing_entries_carry_correct_metadata_at_scale() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (_sshd, sftp) = open_sftp("scale-meta").await;
    sftp.mkdir("meta").await.unwrap();
    sftp.mkdir("meta/subdir").await.unwrap();
    sftp.write_at("meta/known.bin", 0, &[7u8; 1234])
        .await
        .unwrap();
    let listing = sftp.list("meta").await.unwrap();
    let find = |n: &str| -> &Entry { listing.entries.iter().find(|e| e.name == n).expect(n) };
    assert!(find("subdir").is_dir, "subdir 目录位丢失");
    assert!(!find("known.bin").is_dir);
    assert_eq!(find("known.bin").size, 1234, "大小字段丢失");
    // 权限位必须随列表返回：这是「我有没有权限读写这个文件」在界面上的唯一依据。
    // 真 OpenSSH 对新建文件默认 0644（rw-r--r--）。
    assert_eq!(
        find("known.bin").perms.as_deref(),
        Some("rw-r--r--"),
        "权限位未随列表返回或格式不对"
    );
}
