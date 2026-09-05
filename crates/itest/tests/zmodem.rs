//! ZMODEM 容器集成测试（M4a 出口标准：对容器执行 rz/sz 分别触发上传/下载，
//! 传输字节校验正确）。
//!
//! 与单测的分工要说清楚：`crates/terminal` 里的单测证明**我们的编解码自洽**
//! （自己发的自己能解回来）。自洽是必要条件而非充分条件——一个把 CRC 字节序写反、
//! 或把子包 CRC 覆盖范围少算一个字节的实现，同样能完美自洽，只是跟世界上任何
//! 别的 zmodem 都对不上。本文件对的是**真实的 lrzsz**，这是唯一能证伪那类错误的口径。
//!
//! 需 `FS_ITEST=1` + 可用 Docker；未设时打印 skip 并返回（与既有 itest 一致）。

use fs_itest::lrzsz::LrzszSshd;
use fs_terminal::zsession::{Action, ZmodemSession};
use russh::client;
use russh::ChannelMsg;
use std::sync::Arc;
use std::time::Duration;

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

/// 设 `FS_ZDEBUG=1` 时打印双向前 48 字节的十六进制。
///
/// 留着这个开关而不是用完就删：本文件落地过程中两个真 bug（ZFILE 的 ZF0 字节、
/// 数据帧没用 ZCRCE 收尾）都是靠**把我们发的字节和真 sz 发的字节逐字节并排看**
/// 才定位的。协议层的失败信息（"对端跳过了该文件"）离真实原因很远，下次再有
/// 互操作问题，第一步仍然是这个。
fn dump(dir: &str, bytes: &[u8]) {
    if std::env::var("FS_ZDEBUG").is_err() {
        return;
    }
    let hex: Vec<String> = bytes.iter().take(48).map(|b| format!("{b:02x}")).collect();
    eprintln!("{dir} {} bytes: {}", bytes.len(), hex.join(" "));
}

/// 整条传输的墙钟预算。1 MiB 走本机容器远用不到 60 秒；到了就是卡住而不是慢，
/// 让它超时红掉比无限挂着强（挂着的 CI 作业没有任何诊断价值）。
const BUDGET: Duration = Duration::from_secs(60);

async fn connect(tag: &str) -> (LrzszSshd, client::Handle<AcceptAllKeys>) {
    let sshd = LrzszSshd::start(tag).await.unwrap();
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
    (sshd, session)
}

/// 驱动一条 ZMODEM 会话跑到底。
///
/// 刻意**不开 PTY**：PTY 的行规范化（ONLCR 等）会改写二进制帧里的 `\n`/`\r`，
/// 表现为随机 CRC 错。真实客户端在终端里跑 rz/sz 时靠 zmodem 自己的 ZDLE 转义
/// 扛过 PTY，而这里用 `exec` 直连管道，测的是协议本身而不是我们的 PTY 设置。
///
/// `on_file_sent`：收到 `Action::FileSent`（当前文件被对端收妥）时怎么走。
/// 单文件测试传 `finish`；多文件测试传「出队下一个」。
///
/// 返回落盘字节（接收方向）、成败、以及终态消息 + 对端 stderr。
///
/// 把 `rz`/`sz` 的 stderr 一并带回来，是因为它们在拒绝一个文件时会把**原因**写在
/// 那里（"skipped"、"file exists"、"garbage count exceeded"…），而协议层只看得到
/// 一个 ZSKIP。丢掉 stderr 就只能靠猜——第一次跑这条测试拿到 ZSKIP 时正是如此。
async fn drive(
    channel: &mut russh::Channel<client::Msg>,
    mut session: ZmodemSession,
    mut feed_chunk: impl FnMut(u64) -> Vec<u8>,
    mut on_file_sent: impl FnMut(&mut ZmodemSession) -> Vec<Action>,
) -> (Vec<u8>, bool, String) {
    let mut received = Vec::new();
    let mut done = (false, String::new());
    let mut stderr = Vec::new();

    // 起手字节（接收侧 ZRINIT / 发送侧 ZRQINIT）
    let hello = session.start();
    channel.data(&hello[..]).await.unwrap();

    let deadline = tokio::time::Instant::now() + BUDGET;
    loop {
        if session.is_finished() {
            break;
        }
        let msg = match tokio::time::timeout_at(deadline, channel.wait()).await {
            Err(_) => panic!(
                "ZMODEM 传输超出 {} 秒预算（已收 {} 字节）",
                BUDGET.as_secs(),
                received.len()
            ),
            Ok(None) => break, // 通道关闭
            Ok(Some(m)) => m,
        };
        let data = match msg {
            ChannelMsg::Data { ref data } => data.to_vec(),
            // stderr 上是 sz/rz 的进度与**拒绝原因**，攒起来供失败时诊断
            ChannelMsg::ExtendedData { ref data, .. } => {
                stderr.extend_from_slice(data);
                continue;
            }
            ChannelMsg::Eof | ChannelMsg::Close => break,
            _ => continue,
        };
        dump("<<", &data);
        let mut actions = session.feed(&data);
        // 动作可能连锁（NeedChunk → 发子包 → 再要下一块），循环到不再产出
        while !actions.is_empty() {
            let mut next = Vec::new();
            for a in actions {
                match a {
                    Action::Send(bytes) => {
                        dump(">>", &bytes);
                        channel.data(&bytes[..]).await.unwrap()
                    }
                    Action::Write(d) => received.extend_from_slice(&d),
                    Action::BeginRecv(_) | Action::FinishRecv { .. } => {}
                    Action::NeedChunk { offset } => {
                        let chunk = feed_chunk(offset);
                        next.extend(session.feed_file_chunk(&chunk));
                    }
                    Action::FileSent => next.extend(on_file_sent(&mut session)),
                    Action::Done { ok, message } => done = (ok, message),
                }
            }
            actions = next;
        }
    }
    let note = if stderr.is_empty() {
        done.1
    } else {
        format!(
            "{}｜对端 stderr: {}",
            done.1,
            String::from_utf8_lossy(&stderr).trim()
        )
    };
    (received, done.0, note)
}

/// 出口标准前半：容器里 `sz` 发文件，我们收，落盘字节逐字节正确。
#[tokio::test(flavor = "multi_thread")]
async fn download_via_real_sz_bytes_match() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (sshd, session) = connect("zmodemdl").await;

    // 造一个 256 KiB 的确定性文件。用 tr 从 /dev/zero 造而不是 /dev/urandom：
    // 内容可复现，失败时能直接看出是哪一段错了（随机内容只能得到「不相等」）。
    sshd.run(
        "head -c 262144 /dev/zero | tr '\\0' 'A' > /tmp/payload.bin \
         && printf 'ZMODEM-TAIL' >> /tmp/payload.bin \
         && sha256sum /tmp/payload.bin",
    )
    .await
    .unwrap();
    let expected: Vec<u8> = {
        let mut v = vec![b'A'; 262144];
        v.extend_from_slice(b"ZMODEM-TAIL");
        v
    };

    let mut channel = session.channel_open_session().await.unwrap();
    // `-b` 二进制模式；`-e` 转义所有控制字符（与我们 ZRINIT 里通告的 ESCCTL 对应）
    channel
        .exec(true, "sz -b -e /tmp/payload.bin")
        .await
        .unwrap();

    let (received, ok, message) = drive(
        &mut channel,
        ZmodemSession::new_receive(),
        |_| Vec::new(),
        |s| s.finish(),
    )
    .await;

    assert!(ok, "接收须成功收尾，实得：{message}");
    assert_eq!(
        received.len(),
        expected.len(),
        "落盘字节数须与源文件一致（实得 {} 期望 {}）",
        received.len(),
        expected.len()
    );
    assert_eq!(received, expected, "落盘内容须与源文件逐字节一致");
}

/// 出口标准后半：容器里 `rz` 收文件，我们发，容器侧校验落地字节。
///
/// 校验在**容器侧**做（sha256sum）：在客户端比对自己刚发出去的字节，证明的
/// 只是「我发的等于我发的」。
#[tokio::test(flavor = "multi_thread")]
async fn upload_via_real_rz_bytes_match() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (sshd, session) = connect("zmodemul").await;
    // `run` 走 docker exec，是 **root**；而 rz 跑在 SSH 登录用户下。不 chown 的话
    // rz 的 `fopen` 会 EACCES，而它对 fopen 失败的反应是回一个**不带原因的 ZSKIP**
    // ——协议层只看到「对端跳过了该文件」，stderr 上只有一句 `rz waiting to receive.`。
    // 第一次跑这条测试就栽在这里，查了半天协议，问题其实在测试自己的目录权限上。
    sshd.run(&format!(
        "mkdir -p /tmp/inbox && chown {}: /tmp/inbox",
        sshd.username
    ))
    .await
    .unwrap();

    // 待上传内容：含全部 256 种字节值，把 ZDLE 转义路径压满。
    // 只发 ASCII 的测试会漏掉 0x18(ZDLE)/0x11/0x13(XON/XOFF)/0x8D 这几个必须转义的值，
    // 而那正是「自洽但与真实实现对不上」的高发处。
    let payload: Vec<u8> = (0u8..=255).cycle().take(200_000).collect();
    let expected_sha = sha256_hex(&payload);

    let mut channel = session.channel_open_session().await.unwrap();
    channel
        .exec(true, "cd /tmp/inbox && rz -b -e -y")
        .await
        .unwrap();

    let src = payload.clone();
    let (_received, ok, message) = drive(
        &mut channel,
        ZmodemSession::new_send("upload.bin", payload.len() as u64),
        move |offset| {
            let start = offset as usize;
            let end = (start + fs_terminal::zsession::SUBPACKET_BYTES).min(src.len());
            if start >= src.len() {
                Vec::new()
            } else {
                src[start..end].to_vec()
            }
        },
        |s| s.finish(),
    )
    .await;
    assert!(ok, "发送须成功收尾，实得：{message}");

    let out = sshd
        .run("sha256sum /tmp/inbox/upload.bin | cut -d' ' -f1")
        .await
        .unwrap();
    assert_eq!(
        out.trim(),
        expected_sha,
        "容器侧落地文件的 sha256 须与源内容一致"
    );
    let size = sshd.run("stat -c %s /tmp/inbox/upload.bin").await.unwrap();
    assert_eq!(
        size.trim().parse::<usize>().unwrap(),
        payload.len(),
        "容器侧落地字节数须与源内容一致"
    );
}

/// **多文件上传**（2026-08-26 重设计）：一次会话发 3 个文件（其中一个 0 字节），
/// 容器侧逐一 sha256 校验。
///
/// 真 `rz` 是异构对端——它把「ZEOF 之后又来一个 ZFILE」这条路走通了，协议才算对。
/// 0 字节文件刻意排在**中间**：排在头尾都会绕开「0 字节之后还有下一个文件」
/// 这条最脆的衔接（旧实现按 `offset > 0` 判收妥，0 字节文件会让它原地重发）。
#[tokio::test(flavor = "multi_thread")]
async fn multi_file_upload_via_real_rz_bytes_match() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (sshd, session) = connect("zmodemmulti").await;
    sshd.run(&format!(
        "mkdir -p /tmp/inbox-multi && chown {}: /tmp/inbox-multi",
        sshd.username
    ))
    .await
    .unwrap();

    // 三个文件：满字节值谱的 100 KiB、**0 字节**、一段 ASCII。互不相同，
    // 校验错位/串文件（把 A 的内容写到 B 里）会立刻暴露。
    let a: Vec<u8> = (0u8..=255).cycle().take(102_400).collect();
    let b: Vec<u8> = Vec::new();
    let c: Vec<u8> = b"the quick brown fox jumps over the lazy dog\n".repeat(64);

    let mut channel = session.channel_open_session().await.unwrap();
    channel
        .exec(true, "cd /tmp/inbox-multi && rz -b -e -y")
        .await
        .unwrap();

    // 队列状态由闭包共享：feed_chunk 按「当前文件」取块，on_file_sent 切换到下一个。
    // 这正是 app 层 bridge 的队列在测试里的对应物，只是没有文件系统。
    let queue = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
        vec![
            ("multi-a.bin".to_string(), a.clone()),
            ("multi-b-empty.bin".to_string(), b.clone()),
            ("multi-c.bin".to_string(), c.clone()),
        ],
    )));
    let current = std::sync::Arc::new(std::sync::Mutex::new((
        "multi-a.bin".to_string(),
        a.clone(),
    )));

    let q2 = queue.clone();
    let cur_for_feed = current.clone();
    let cur_for_switch = current.clone();
    let (_received, ok, message) = drive(
        &mut channel,
        ZmodemSession::new_send("multi-a.bin", a.len() as u64),
        move |offset| {
            let (_, ref data) = *cur_for_feed.lock().unwrap();
            let start = offset as usize;
            let end = (start + fs_terminal::zsession::SUBPACKET_BYTES).min(data.len());
            if start >= data.len() {
                Vec::new()
            } else {
                data[start..end].to_vec()
            }
        },
        move |s| {
            let mut q = q2.lock().unwrap();
            match q.pop_front() {
                Some((name, data)) => {
                    *cur_for_switch.lock().unwrap() = (name.clone(), data.clone());
                    s.next_file(&name, data.len() as u64)
                }
                None => s.finish(),
            }
        },
    )
    .await;
    assert!(ok, "多文件发送须成功收尾，实得：{message}");

    // 容器侧逐一校验：名字、大小、内容全部对上。
    for (name, data) in [
        ("multi-a.bin", &a),
        ("multi-b-empty.bin", &b),
        ("multi-c.bin", &c),
    ] {
        let expect = if data.is_empty() {
            // sha256 of empty input，容器侧 `sha256sum` 对空文件输出同一串
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string()
        } else {
            sha256_hex(data)
        };
        let got = sshd
            .run(&format!(
                "sha256sum /tmp/inbox-multi/{name} | cut -d' ' -f1"
            ))
            .await
            .unwrap_or_else(|e| panic!("{name} 落地校验失败：{e}"));
        assert_eq!(
            got.trim(),
            expect,
            "{name} 的落地内容与源不一致（0 字节文件没落地？多文件队列错位？）"
        );
    }
}

/// 取消要让**对端也**退出传输态：只在本端置个标志，容器里的 sz 会继续发到超时，
/// 而那条 SSH 通道在此期间是不可用的（用户看到的是「取消了但终端还是死的」）。
#[tokio::test(flavor = "multi_thread")]
async fn cancel_makes_remote_sz_exit() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (sshd, session) = connect("zmodemcancel").await;
    sshd.run("head -c 4194304 /dev/zero | tr '\\0' 'B' > /tmp/big.bin")
        .await
        .unwrap();

    let mut channel = session.channel_open_session().await.unwrap();
    channel.exec(true, "sz -b -e /tmp/big.bin").await.unwrap();

    let mut zs = ZmodemSession::new_receive();
    let hello = zs.start();
    channel.data(&hello[..]).await.unwrap();

    // 收到第一批数据就取消
    let deadline = tokio::time::Instant::now() + BUDGET;
    let mut cancelled = false;
    let mut saw_close = false;
    loop {
        let msg = match tokio::time::timeout_at(deadline, channel.wait()).await {
            Err(_) => panic!("取消后 {} 秒内对端仍未收尾", BUDGET.as_secs()),
            Ok(None) => break,
            Ok(Some(m)) => m,
        };
        match msg {
            ChannelMsg::Data { ref data } => {
                let acts = if cancelled {
                    // 取消之后不再解析对端字节，只等通道收尾
                    Vec::new()
                } else {
                    zs.feed(data)
                };
                for a in acts {
                    match a {
                        Action::Send(b) => channel.data(&b[..]).await.unwrap(),
                        Action::Write(_) if !cancelled => {
                            cancelled = true;
                            for c in zs.cancel() {
                                if let Action::Send(b) = c {
                                    channel.data(&b[..]).await.unwrap();
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            ChannelMsg::Eof | ChannelMsg::Close | ChannelMsg::ExitStatus { .. } => {
                saw_close = true;
                break;
            }
            _ => {}
        }
    }
    assert!(
        cancelled,
        "测试自身须真的发出过取消（否则下面那条平凡通过）"
    );
    assert!(
        saw_close,
        "发出取消序列后对端 sz 须自行退出并关闭通道（否则通道会一直被占着）"
    );
}

/// 纯函数 sha256（避免为一次测试引入 sha2 之外的依赖；本仓已有 sha2）。
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
