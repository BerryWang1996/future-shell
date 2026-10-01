//! SFTP 传输基准（手动运行，不进 CI）：走**产品的**连接路径（`connect::connect`）与
//! **产品的**传输引擎（`TransferManager`），量上传/下载各自的吞吐。
//!
//! # 为什么需要它
//!
//! `scale.rs` 的 ≥20 MB/s 门槛测的是 `write_at`/`read_range` 顺序循环，而且对端在本机
//! 容器里——往返只有一两毫秒。真实网络的往返是 20–50 ms，按块顺序等应答的实现会被
//! 往返时间卡死，而这在本机回环上完全看不出来。本基准配合对端注入延迟（`tc netem`）
//! 使用，并与同一对端上 OpenSSH 的 `sftp` 对照，才看得出实现与参照的真实差距。
//!
//! # 用法
//!
//! ```text
//! FS_BENCH_ADDR=127.0.0.1:2299 FS_BENCH_USER=it FS_BENCH_KEY=path/to/id_ed25519 \
//!   [FS_BENCH_MIB=64] cargo test --release -p fs_itest --test sftp_bench -- --ignored --nocapture
//! ```
//!
//! 对端准备（注入延迟的 OpenSSH 容器）、对照方法与 1.0.1 的结果见
//! `docs/verification/performance.md`「SFTP 高延迟对比」。
use fs_connmgr::{AuthRef, Db, HostKeyPolicy, Profile};
use fs_sshengine::events::{HostKeyChoice, Prompt, SessionEvents};
use fs_sshengine::hostkey::{Decision, PresentedKey};
use fs_sshengine::secrets::SecretSource;
use fs_sshengine::sftp::RemoteSftp;
use fs_sshengine::transfer::{Direction, TransferJob, TransferManager, TransferState};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Instant;
use zeroize::Zeroizing;

struct AcceptKeys;
impl SessionEvents for AcceptKeys {
    fn host_key_decision(&self, _: &str, _: u16, _: &PresentedKey, _: &Decision) -> HostKeyChoice {
        HostKeyChoice::AcceptOnce
    }
    fn kbd_interactive(&self, _: &str, _: &str, prompts: &[Prompt]) -> Vec<String> {
        prompts.iter().map(|_| String::new()).collect()
    }
    fn password_prompt(&self) -> String {
        String::new()
    }
    fn status(&self, _: &str) {}
}

struct KeyFile(String);
impl SecretSource for KeyFile {
    fn secret(&self, _rec: u64) -> Result<fs_sshengine::secrets::Secret, fs_sshengine::Error> {
        Ok(fs_sshengine::secrets::Secret {
            kind: fs_sshengine::secrets::SecretKind::PrivateKey,
            bytes: Zeroizing::new(self.0.clone().into_bytes()),
        })
    }
}

fn profile(host: String, port: u16, username: String) -> Profile {
    Profile {
        id: uuid::Uuid::nil(),
        name: "bench".into(),
        group_path: None,
        host,
        port,
        username,
        protocol: fs_connmgr::Protocol::Ssh,
        auth: AuthRef {
            vault_record: Some(1),
            ..Default::default()
        },
        jump: vec![],
        host_key_policy: HostKeyPolicy::Tofu,
        host_key_pins: vec![],
        env: Default::default(),
        term: Default::default(),
        sftp: Default::default(),
        ai_policy: Default::default(),
        serial: Default::default(),
    }
}

/// 跑一件传输到终态，返回耗时。失败直接 panic（基准不吞错）。
/// 事件接收端只能取一次（`TransferManager::events`），由调用方取好传进来。
async fn run(
    mgr: &TransferManager,
    ev: &mut tokio::sync::mpsc::Receiver<fs_sshengine::transfer::TransferEvent>,
    job: TransferJob,
) -> std::time::Duration {
    let t = Instant::now();
    let id = mgr.submit(job).await.expect("submit");
    loop {
        let e = ev.recv().await.expect("事件通道关闭");
        if e.id != id {
            continue;
        }
        match e.state {
            TransferState::Done => return t.elapsed(),
            TransferState::Failed(m) => panic!("传输失败：{m}"),
            TransferState::Cancelled => panic!("被取消"),
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "手动基准：需要 FS_BENCH_ADDR / FS_BENCH_USER / FS_BENCH_KEY"]
async fn sftp_transfer_bench() {
    let addr = std::env::var("FS_BENCH_ADDR").expect("FS_BENCH_ADDR=host:port");
    let user = std::env::var("FS_BENCH_USER").expect("FS_BENCH_USER");
    let key = std::fs::read_to_string(std::env::var("FS_BENCH_KEY").expect("FS_BENCH_KEY"))
        .expect("读私钥文件");
    let mib: usize = std::env::var("FS_BENCH_MIB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);
    let (host, port) = addr.rsplit_once(':').expect("host:port");

    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("bench.db")).await.unwrap();
    let handle = fs_sshengine::connect::connect(
        &profile(host.into(), port.parse().unwrap(), user),
        &KeyFile(key),
        db.pool(),
        Arc::new(AcceptKeys),
    )
    .await
    .expect("连接");
    let ch = handle.channel_open_session().await.unwrap();
    ch.request_subsystem(true, "sftp").await.unwrap();
    let sftp = RemoteSftp::new(ch).await.unwrap();
    let mgr = TransferManager::spawn(Arc::new(sftp), 1);
    let mut ev = mgr.events().await;

    // 确定性伪随机内容（不可压缩，避免任何一层压缩让数字好看）
    let total = mib * 1024 * 1024;
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let payload: Vec<u8> = (0..total)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    let local = dir.path().join("up.bin");
    std::fs::write(&local, &payload).unwrap();

    let up = run(
        &mgr,
        &mut ev,
        TransferJob {
            direction: Direction::Up,
            local: local.clone(),
            remote: "fs-bench.bin".into(),
            resume: false,
            sandbox_root: None,
            target_endpoint: addr.clone(),
            endpoint_aliases: Vec::new(),
            verify: None,
        },
    )
    .await;

    let root = dir.path().join("down");
    std::fs::create_dir_all(&root).unwrap();
    let dest = root.join("fs-bench.bin");
    let down = run(
        &mgr,
        &mut ev,
        TransferJob {
            direction: Direction::Down,
            local: dest.clone(),
            remote: "fs-bench.bin".into(),
            resume: false,
            sandbox_root: Some(root),
            target_endpoint: addr.clone(),
            endpoint_aliases: Vec::new(),
            verify: None,
        },
    )
    .await;

    let got = std::fs::read(&dest).unwrap();
    assert_eq!(
        Sha256::digest(&got),
        Sha256::digest(&payload),
        "下载回来的内容与上传的不一致"
    );
    let mb = total as f64 / 1e6;
    println!(
        "BENCH {mib} MiB：上传 {:.2}s（{:.1} MB/s）/ 下载 {:.2}s（{:.1} MB/s）",
        up.as_secs_f64(),
        mb / up.as_secs_f64(),
        down.as_secs_f64(),
        mb / down.as_secs_f64()
    );
}
