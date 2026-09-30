//! 传后校验执行体单测（路线图 M1 出口「完成后可选 sha256sum 校验…降级 size…禁静默跳过」/ UI 规格 §1.4）。
//! ExecChannel 以脚本化 fake 注入，逐分支驱动 run_verify 的四态。
#[allow(dead_code)]
fn out(code: i32, stdout: String, stderr: String) -> fs_sshengine::verify::ExecOutput {
    fs_sshengine::verify::ExecOutput {
        code: Some(code),
        stdout,
        stderr,
    }
}
use fs_sshengine::sftp::{Entry, FileMeta, FileType, SftpOps};
use fs_sshengine::transfer::{Direction, Sha256Hex};
use fs_sshengine::verify::{
    exec_unavailable, file_sha256, parse_sha256sum, remote_sha256_via, run_verify, shell_quote,
    ExecChannel, VerifyEntry, VerifyOutcome, VerifyPlan,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

/// 内存 fake（结构与 transfer.rs fake 同形；tests/ 文件各自独立拷贝）
#[derive(Default)]
struct FakeFs {
    files: Mutex<HashMap<String, Vec<u8>>>,
}

#[async_trait::async_trait]
impl SftpOps for FakeFs {
    async fn list(
        &self,
        path: &str,
    ) -> Result<fs_sshengine::sftp::ListResult, fs_sshengine::Error> {
        Ok(fs_sshengine::sftp::ListResult {
            entries: self
                .files
                .lock()
                .unwrap()
                .keys()
                .filter(|k| k.starts_with(path))
                .map(|k| Entry {
                    name: k.clone(),
                    is_dir: false,
                    is_symlink: false,
                    size: 0,
                    mtime: 0,
                    perms: None,
                })
                .collect(),
            truncated: false,
        })
    }
    async fn stat_size(&self, path: &str) -> Result<u64, fs_sshengine::Error> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .map(|v| v.len() as u64)
            .ok_or_else(|| fs_sshengine::Error::Sftp(format!("no such file: {path}")))
    }
    async fn stat_meta(&self, path: &str) -> Result<FileMeta, fs_sshengine::Error> {
        // 与 stat_size 同纪律：文件不存在必须报错，不能悄悄回一份 size=0 的元数据
        //（那会让「源文件不见了」在续传身份比对里表现成「一个 0 字节的源」）。
        Ok(FileMeta {
            file_type: FileType::Regular,
            size: self.stat_size(path).await?,
            mtime: 0,
            mode: None,
            uid: None,
            gid: None,
        })
    }
    async fn read_range(
        &self,
        path: &str,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, fs_sshengine::Error> {
        let g = self.files.lock().unwrap();
        let v = g
            .get(path)
            .ok_or_else(|| fs_sshengine::Error::Sftp("enoent".into()))?;
        let start = (offset as usize).min(v.len());
        Ok(v[start..(start + len).min(v.len())].to_vec())
    }
    async fn write_at(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<(), fs_sshengine::Error> {
        let mut g = self.files.lock().unwrap();
        let v = g.entry(path.to_string()).or_default();
        let end = offset as usize + data.len();
        if v.len() < end {
            v.resize(end, 0);
        }
        v[offset as usize..end].copy_from_slice(data);
        Ok(())
    }
    async fn truncate(&self, path: &str, size: u64) -> Result<(), fs_sshengine::Error> {
        let mut g = self.files.lock().unwrap();
        let v = g
            .get_mut(path)
            .ok_or_else(|| fs_sshengine::Error::Sftp(format!("no such file: {path}")))?;
        v.resize(size as usize, 0);
        Ok(())
    }
    async fn sync(&self, _: &str) -> Result<(), fs_sshengine::Error> {
        Ok(())
    }
    async fn mkdir(&self, _: &str) -> Result<(), fs_sshengine::Error> {
        Ok(())
    }
    async fn remove(&self, path: &str) -> Result<(), fs_sshengine::Error> {
        self.files.lock().unwrap().remove(path);
        Ok(())
    }
    async fn rename(&self, from: &str, to: &str) -> Result<(), fs_sshengine::Error> {
        let mut g = self.files.lock().unwrap();
        if let Some(v) = g.remove(from) {
            g.insert(to.to_string(), v);
        }
        Ok(())
    }
    async fn lstat(&self, path: &str) -> Result<FileMeta, fs_sshengine::Error> {
        Ok(FileMeta {
            file_type: FileType::Regular,
            size: self.stat_size(path).await?,
            mtime: 0,
            mode: None,
            uid: None,
            gid: None,
        })
    }
    async fn read_link(&self, _: &str) -> Result<String, fs_sshengine::Error> {
        Ok(String::new())
    }
    async fn symlink(&self, _: &str, _: &str) -> Result<(), fs_sshengine::Error> {
        Ok(())
    }
    async fn canonicalize(&self, p: &str) -> Result<String, fs_sshengine::Error> {
        Ok(p.to_string())
    }
    /// 审计2 #17：校验面同样不删目录——走到即测试面被误用，当场炸掉。
    async fn remove_dir(&self, _: &str) -> Result<(), fs_sshengine::Error> {
        unreachable!("本测试面不删目录")
    }
}

/// `ExecChannel::exec_once` 的返回型：(exit code, stdout, stderr)。
type ExecResult = Result<fs_sshengine::verify::ExecOutput, fs_sshengine::Error>;

/// 脚本化 exec：预设结果队首弹出，驱动 run_verify 各分支。
#[derive(Default)]
struct ScriptedExec {
    results: Mutex<VecDeque<ExecResult>>,
    /// 实际收到的命令行——用于断言「远端路径确实经过 shell_quote」（注入防线的非空证明）
    seen: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ExecChannel for ScriptedExec {
    async fn exec_once(&self, cmd: &str) -> ExecResult {
        self.seen.lock().unwrap().push(cmd.to_string());
        self.results
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted exec 结果未设置")
    }
}

fn entry_up(
    local: &std::path::Path,
    remote: &str,
    expect: Option<Sha256Hex>,
    degraded: bool,
) -> VerifyEntry {
    VerifyEntry {
        direction: Direction::Up,
        remote: remote.into(),
        local: local.to_path_buf(),
        plan: VerifyPlan { expect, degraded },
    }
}

#[test]
fn parse_sha256sum_accepts_coreutils_and_busybox_output() {
    let h = "a".repeat(64);
    assert_eq!(
        parse_sha256sum(&format!("{h}  /path/file.bin\n"))
            .unwrap()
            .0,
        h
    ); // coreutils：两空格
    assert_eq!(parse_sha256sum(&format!("{h} /x\n")).unwrap().0, h); // 单空格亦兼容
    assert!(parse_sha256sum("").is_none());
    assert!(parse_sha256sum("garbage").is_none());
    assert!(
        parse_sha256sum(&format!("{}  f", "A".repeat(64))).is_none(),
        "大写拒绝（逐字比对前提）"
    );
}

#[test]
fn exec_unavailable_classification() {
    assert!(exec_unavailable(127, ""));
    assert!(exec_unavailable(126, ""));
    assert!(exec_unavailable(1, "sh: sha256sum: not found"));
    assert!(exec_unavailable(2, "bash: sha256sum: command not found"));
    assert!(exec_unavailable(
        1,
        "sha256sum: /x: No such file or directory"
    ));
    assert!(
        !exec_unavailable(1, "permission denied"),
        "非缺失类失败不得误判命令缺失"
    );
    assert!(!exec_unavailable(0, ""));
}

#[test]
fn shell_quote_escaping() {
    assert_eq!(shell_quote("a b.bin"), "'a b.bin'");
    assert_eq!(shell_quote("it's.bin"), "'it'\\''s.bin'");
    assert_eq!(
        shell_quote("$(rm -rf /)"),
        "'$(rm -rf /)'",
        "命令替换在单引号内失活"
    );
}

#[tokio::test]
async fn file_sha256_known_vector() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("abc.txt");
    std::fs::write(&p, b"abc").unwrap();
    assert_eq!(
        file_sha256(&p).await.unwrap().0,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// 大于单次分块（1 MiB）的文件必须逐块喂进摘要器——一次性 read 只吃掉首块即完成会得到错哈希。
#[tokio::test]
async fn file_sha256_streams_across_chunk_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("big.bin");
    let payload = vec![0x5au8; (1 << 20) + 4096]; // 1 MiB + 4 KiB
    std::fs::write(&p, &payload).unwrap();
    let expect = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&payload);
        format!("{:x}", h.finalize())
    };
    assert_eq!(
        file_sha256(&p).await.unwrap().0,
        expect,
        "跨分块流式哈希必须与整体一次算出的结果一致"
    );
}

#[tokio::test]
async fn remote_sha256_via_parses_and_errors() {
    let h = "0123456789abcdef".repeat(4);
    let exec = ScriptedExec::default();
    exec.results
        .lock()
        .unwrap()
        .push_back(Ok(out(0, format!("{h}  /r.bin\n"), String::new())));
    assert_eq!(remote_sha256_via(&exec, "/r.bin").await.unwrap().0, h);
    exec.results
        .lock()
        .unwrap()
        .push_back(Ok(out(127, String::new(), "not found".to_string())));
    assert!(
        remote_sha256_via(&exec, "/r.bin").await.is_err(),
        "非零退出 → 错误（降级由 app 层决策）"
    );
    exec.results
        .lock()
        .unwrap()
        .push_back(Ok(out(0, "not-a-hash  x".to_string(), String::new())));
    assert!(remote_sha256_via(&exec, "/r.bin").await.is_err());
}

/// 远端路径是服务端可控输入：拼进 exec 命令行前必须单引号包裹（命令注入防线）。
#[tokio::test]
async fn remote_path_is_shell_quoted_in_exec_cmd() {
    let exec = ScriptedExec::default();
    exec.results.lock().unwrap().push_back(Ok(out(
        0,
        format!("{}  x", "b".repeat(64)),
        String::new(),
    )));
    let _ = remote_sha256_via(&exec, "/tmp/$(id).bin; rm -rf /").await;
    let cmd = exec.seen.lock().unwrap()[0].clone();
    assert_eq!(
        cmd, "sha256sum -- '/tmp/$(id).bin; rm -rf /'",
        "远端路径必须整体落在单引号内，否则 $() 与 ; 会在服务端 shell 生效"
    );
}

#[tokio::test]
async fn up_verify_sha256_match_and_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("u.bin");
    std::fs::write(&local, b"payload").unwrap();
    let expect = file_sha256(&local).await.unwrap();
    let fs = FakeFs::default();
    fs.write_at("/r.bin", 0, b"payload").await.unwrap();

    let exec = ScriptedExec::default();
    exec.results.lock().unwrap().push_back(Ok(out(
        0,
        format!("{}  /r.bin", expect.0),
        String::new(),
    )));
    let e = entry_up(&local, "/r.bin", Some(expect.clone()), false);
    assert!(matches!(
        run_verify(&exec, &fs, &e).await,
        VerifyOutcome::Sha256Match
    ));

    exec.results.lock().unwrap().push_back(Ok(out(
        0,
        format!("{}  /r.bin", "f".repeat(64)),
        String::new(),
    )));
    assert!(matches!(
        run_verify(&exec, &fs, &e).await,
        VerifyOutcome::Mismatch
    ));
}

#[tokio::test]
async fn up_verify_degrades_to_size_when_command_missing() {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("d.bin");
    std::fs::write(&local, b"1234567").unwrap(); // 7 字节
    let expect = file_sha256(&local).await.unwrap();
    let fs = FakeFs::default();
    fs.write_at("/r.bin", 0, b"1234567").await.unwrap();
    let e = entry_up(&local, "/r.bin", Some(expect), false);

    let exec = ScriptedExec::default();
    exec.results.lock().unwrap().push_back(Ok(out(
        127,
        String::new(),
        "sh: sha256sum: not found".to_string(),
    )));
    assert!(
        matches!(
            run_verify(&exec, &fs, &e).await,
            VerifyOutcome::SizeOnlyMatch
        ),
        "命令缺失降级后大小一致 → SizeOnlyMatch（UI 规格 §1.4 显式标注「降级」）"
    );

    fs.write_at("/r.bin", 7, b"XX").await.unwrap(); // 9 字节 ≠ 7
    exec.results
        .lock()
        .unwrap()
        .push_back(Ok(out(127, String::new(), "not found".to_string())));
    assert!(
        matches!(run_verify(&exec, &fs, &e).await, VerifyOutcome::Mismatch),
        "降级后大小不一致 → Mismatch"
    );
}

#[tokio::test]
async fn up_verify_unverified_when_exec_and_stat_both_fail() {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("x.bin");
    std::fs::write(&local, b"z").unwrap();
    let fs = FakeFs::default(); // 无远端文件：stat 亦失败
    let exec = ScriptedExec::default();
    exec.results
        .lock()
        .unwrap()
        .push_back(Err(fs_sshengine::Error::Ssh("channel closed".into())));
    let e = entry_up(
        &local,
        "/absent.bin",
        Some(file_sha256(&local).await.unwrap()),
        false,
    );
    assert!(
        matches!(run_verify(&exec, &fs, &e).await, VerifyOutcome::Unverified),
        "双路径失败 → 显式 Unverified（禁静默跳过，路线图 M1 出口）"
    );
}

#[tokio::test]
async fn down_verify_sha256_match_and_degraded_size() {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("dl.bin");
    std::fs::write(&local, b"downloaded").unwrap();
    let expect = file_sha256(&local).await.unwrap();
    let fs = FakeFs::default();
    fs.write_at("/r.bin", 0, b"downloaded").await.unwrap();

    // Down 常规路径：无需 exec——传后本地哈希 vs 传前远端期望
    let exec = ScriptedExec::default();
    let e = VerifyEntry {
        direction: Direction::Down,
        remote: "/r.bin".into(),
        local: local.clone(),
        plan: VerifyPlan {
            expect: Some(expect),
            degraded: false,
        },
    };
    assert!(matches!(
        run_verify(&exec, &fs, &e).await,
        VerifyOutcome::Sha256Match
    ));
    assert!(
        exec.seen.lock().unwrap().is_empty(),
        "Down 常规路径不得再开 exec 通道（期望值传前已取）"
    );

    // Down 降级（传前 exec 失败）：size 对比
    let e_deg = VerifyEntry {
        direction: Direction::Down,
        remote: "/r.bin".into(),
        local: local.clone(),
        plan: VerifyPlan {
            expect: None,
            degraded: true,
        },
    };
    assert!(matches!(
        run_verify(&exec, &fs, &e_deg).await,
        VerifyOutcome::SizeOnlyMatch
    ));
}

/// Down 方向哈希不一致必须报 Mismatch，且本地文件读不到时报 Unverified（不得静默当成一致）。
#[tokio::test]
async fn down_verify_mismatch_and_unverified() {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("dl2.bin");
    std::fs::write(&local, b"downloaded").unwrap();
    let fs = FakeFs::default();
    fs.write_at("/r.bin", 0, b"downloaded").await.unwrap();
    let exec = ScriptedExec::default();

    let wrong = Sha256Hex::new("c".repeat(64)).unwrap();
    let e = VerifyEntry {
        direction: Direction::Down,
        remote: "/r.bin".into(),
        local: local.clone(),
        plan: VerifyPlan {
            expect: Some(wrong.clone()),
            degraded: false,
        },
    };
    assert!(matches!(
        run_verify(&exec, &fs, &e).await,
        VerifyOutcome::Mismatch
    ));

    let e_missing = VerifyEntry {
        direction: Direction::Down,
        remote: "/r.bin".into(),
        local: dir.path().join("absent.bin"),
        plan: VerifyPlan {
            expect: Some(wrong),
            degraded: false,
        },
    };
    assert!(
        matches!(
            run_verify(&exec, &fs, &e_missing).await,
            VerifyOutcome::Unverified
        ),
        "本地文件不可读 → Unverified，绝不能落进 Match 分支"
    );
}
