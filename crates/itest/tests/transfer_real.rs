//! 传输引擎对真 OpenSSH 的集成测试（1.0.1 补）：生产组装 `TransferManager` +
//! `TimedSftp(RemoteSftp)`，而不是 `write_at` / `read_range` 的直接调用。
//!
//! # 为什么补这一份
//!
//! 1.0.0 的容器 itest 里没有一条走「传输引擎 + 真 SFTP」：引擎的 46 条用例全在测试替身上，
//! 真服务端的用例只调底层方法。1.0.1 的上传改成同一句柄上的流水线写之后，替身走的是逐块
//! 写的默认实现——**流水线写入器只有连真服务端才会执行**，这里是它唯一的行为载体。
//!
//! 需 `FS_ITEST=1` + Docker；未设时打印 skip 并返回（与既有 itest 一致）。
use fs_itest::sshd::SshdContainer;
use fs_sshengine::sftp::{RemoteSftp, SftpOps};
use fs_sshengine::timeouts::TimedSftp;
use fs_sshengine::transfer::{
    Direction, TransferEvent, TransferJob, TransferManager, TransferState,
};
use russh::client;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

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

/// 连上容器、开 sftp 子系统，按**生产方式**组装（`app/src/state.rs` 的 `TimedSftp::new(RemoteSftp)`）。
/// 连接句柄一并返回：中断用例要靠它把连接从底下掐断。
async fn production_ops(sshd: &SshdContainer) -> (client::Handle<AcceptAllKeys>, Arc<dyn SftpOps>) {
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
    let ch = session.channel_open_session().await.unwrap();
    ch.request_subsystem(true, "sftp").await.unwrap();
    let sftp = RemoteSftp::new(ch).await.unwrap();
    (session, Arc::new(TimedSftp::new(Arc::new(sftp))))
}

/// 确定性伪随机内容（不可压缩）。
fn payload(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
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
        target_endpoint: "itest".into(),
        endpoint_aliases: Vec::new(),
        verify: None,
    }
}

async fn to_terminal(ev: &mut mpsc::Receiver<TransferEvent>, id: u64) -> TransferState {
    loop {
        let e = tokio::time::timeout(Duration::from_secs(300), ev.recv())
            .await
            .expect("300 s 内没等到下一条传输事件")
            .expect("事件通道关闭");
        if e.id != id {
            continue;
        }
        match e.state {
            TransferState::Running | TransferState::Retrying { .. } => continue,
            other => return other,
        }
    }
}

/// 远端文件整读回来（分块走 `read_range`）。
async fn read_all(ops: &Arc<dyn SftpOps>, path: &str) -> Vec<u8> {
    let size = ops.stat_size(path).await.unwrap() as usize;
    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        let want = (size - out.len()).min(fs_sshengine::transfer::CHUNK);
        let got = ops.read_range(path, out.len() as u64, want).await.unwrap();
        assert!(!got.is_empty(), "读 {path} 在 {} 处提前结束", out.len());
        out.extend_from_slice(&got);
    }
    out
}

/// 多块文件上传再下载，逐字节一致。长度故意不是分块的整数倍，让最后一块是短块。
#[tokio::test(flavor = "multi_thread")]
async fn pipelined_upload_and_download_roundtrip() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("xfer-roundtrip").await.unwrap();
    let (_session, ops) = production_ops(&sshd).await;
    let mgr = TransferManager::spawn(ops.clone(), 1);
    let mut ev = mgr.events().await;

    let data = payload(5 * 1024 * 1024 + 12_345, 0xA11CE);
    let dir = tempfile::tempdir().unwrap();
    let up_src = dir.path().join("src.bin");
    std::fs::write(&up_src, &data).unwrap();

    let id = mgr
        .submit(job(Direction::Up, up_src, "roundtrip.bin"))
        .await
        .unwrap();
    match to_terminal(&mut ev, id).await {
        TransferState::Done => {}
        other => panic!("传输应成功，实得 {other:?}"),
    }
    assert_eq!(
        read_all(&ops, "roundtrip.bin").await,
        data,
        "上传后远端内容与本地不一致"
    );

    let down_root = dir.path().join("down");
    std::fs::create_dir_all(&down_root).unwrap();
    let dest = down_root.join("roundtrip.bin");
    let id = mgr
        .submit(job(Direction::Down, dest.clone(), "roundtrip.bin"))
        .await
        .unwrap();
    match to_terminal(&mut ev, id).await {
        TransferState::Done => {}
        other => panic!("传输应成功，实得 {other:?}"),
    }
    assert_eq!(std::fs::read(&dest).unwrap(), data, "下载回来的内容不一致");
}

/// **上传中途掐断连接**：远端临时件必须是本地文件的**连续前缀**，续传后内容完整。
///
/// 这是流水线写入正确性的核心前提：写请求在同一句柄上按偏移顺序发出、服务端按序处理，
/// 所以断线时已落盘的一定是连续前缀；续传以远端大小为断点（`transfer::prepare_part`）。
/// 若换成多句柄并发写，这里会读到一段不属于任何前缀的数据（空洞或错位），续传就会
/// 把它当成已传好的部分提交上去——默认配置下没有任何一关能发现。
#[tokio::test(flavor = "multi_thread")]
async fn interrupted_pipelined_upload_leaves_a_contiguous_prefix_and_resumes() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("xfer-interrupt").await.unwrap();
    let data = payload(128 * 1024 * 1024, 0xB0B);
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("big.bin");
    std::fs::write(&src, &data).unwrap();

    // ① 第一条连接：上传到 16 MiB 左右时把连接从底下掐断
    {
        let (session, ops) = production_ops(&sshd).await;
        let mgr = TransferManager::spawn(ops, 1);
        let mut ev = mgr.events().await;
        let id = mgr
            .submit(job(Direction::Up, src.clone(), "big.bin"))
            .await
            .unwrap();
        let mut cut = false;
        let end = loop {
            let e = tokio::time::timeout(Duration::from_secs(300), ev.recv())
                .await
                .expect("300 s 内没等到传输事件")
                .expect("事件通道关闭");
            if e.id != id {
                continue;
            }
            match e.state {
                TransferState::Running if !cut && e.bytes_done >= 16 * 1024 * 1024 => {
                    cut = true;
                    let _ = session
                        .disconnect(russh::Disconnect::ByApplication, "itest cut", "en")
                        .await;
                }
                TransferState::Running | TransferState::Retrying { .. } => {}
                other => break other,
            }
        };
        assert!(cut, "传输在掐断之前就结束了（{end:?}），本用例没测到中断");
        assert!(
            matches!(end, TransferState::Failed(_)),
            "连接断了传输却没有失败：{end:?}"
        );
    }

    // ② 第二条连接：临时件必须是本地文件的连续前缀
    let (_session, ops) = production_ops(&sshd).await;
    let part = read_all(&ops, "big.bin.fspart").await;
    assert!(
        !part.is_empty() && part.len() < data.len(),
        "临时件长度 {} 不在 (0, {}) 内：中断点不在中途，本用例无效",
        part.len(),
        data.len()
    );
    if let Some(i) = part.iter().zip(&data).position(|(a, b)| a != b) {
        panic!(
            "临时件在偏移 {i} 处与本地不一致（临时件长 {}）：远端不是连续前缀，续传会把错的数据当成已传好的部分",
            part.len()
        );
    }

    // ③ 续传完成后内容完整
    let mgr = TransferManager::spawn(ops.clone(), 1);
    let mut ev = mgr.events().await;
    let mut resume = job(Direction::Up, src.clone(), "big.bin");
    resume.resume = true;
    let id = mgr.submit(resume).await.unwrap();
    match to_terminal(&mut ev, id).await {
        TransferState::Done => {}
        other => panic!("传输应成功，实得 {other:?}"),
    }
    assert_eq!(
        read_all(&ops, "big.bin").await,
        data,
        "续传后远端内容与本地不一致"
    );
}
