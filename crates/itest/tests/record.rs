//! 会话录屏的容器集成测试（M4a 出口标准：「回放与原始输出 diff 为空（文本层面），
//! 产物可被 asciinema play 播放」）。
//!
//! 第二半**必须对真实 asciinema 验证**：自研格式解析器（record.rs 的 parse_cast）
//! 与自己的写入器天然自洽——两边犯同一个错时单测全绿，而真实工具立刻拒绝。
//! 这与 ZMODEM 对真实 lrzsz、进程管理对真实 ps 是同一条纪律。
//!
//! `asciinema cat`（打印全部输出，不带时序）与 `asciinema play` 共用同一个解析器：
//! cat 能吃下的文件 play 就能播。需 `FS_ITEST=1` + 可用 Docker。
//!
//! 装 asciinema 前**必须先 `apt-get update`**：镜像自带的包索引可能是陈的，
//! `install` 会直接找不到包。本文件两条用例并发跑时曾因此**间歇性**失败——单独跑
//! 却总是绿（2026-08-22 全量跑 itest 才撞见）。间歇红比稳定红更坏：它会被当成「环境抽风」
//! 而被重跑掩盖过去。
//!
//! 2026-08-26 补：那次的修法（手写「失败就再 update+install 一遍」）**没修够**，
//! 在 `scripts/linux-itest.sh` 的容器路径上又红了一次。重试改交给 apt 自己
//! （`Acquire::Retries=3`），且**不再吞 apt 的 stderr**——上一版把 stdout 与 stderr
//! 一起重定向掉，于是失败时只剩一个退出码 127，apt 说了什么全没了。
//! 详见 [`INSTALL_ASCIINEMA`] 的文档。

use fs_itest::lrzsz::LrzszSshd;
use fs_terminal::record::{parse_cast, Recorder};
use russh::client;
use russh::ChannelMsg;
use std::sync::Arc;

/// 容器内装 asciinema。
///
/// **2026-08-26 重写。** 原来是两处逐字重复的一行长串，且把 apt 的
/// stdout 与 **stderr 一起** `>/dev/null 2>&1` 吞掉。代价在 Linux 容器路径上
/// 现了形：`command -v asciinema` 以 127 退出，而 `run()` 摊出来的
/// stdout 与 stderr **都是空的**——apt 到底说了什么被那条重定向擦干净了，
/// 报错只剩一个退出码。
///
/// 现在：
/// - `set -e` + 只吞 stdout（进度条），**stderr 留着**——失败时 `run()` 的
///   断言消息里就有 apt 的原话；
/// - `Acquire::Retries=3` 交给 apt 自己重试，取代原来那个手写的
///   「失败就再 update+install 一遍」（手写那版只重试一次，且把两条命令的
///   失败合并成一个 `||`，分不清是索引陈了还是包根本不存在）；
/// - 抽成常量：两处逐字重复的长串，改一处漏一处是必然的。
const INSTALL_ASCIINEMA: &str = "set -e; \
     export DEBIAN_FRONTEND=noninteractive; \
     apt-get -o Acquire::Retries=3 update -qq >/dev/null; \
     apt-get -o Acquire::Retries=3 install -y -qq --no-install-recommends asciinema >/dev/null; \
     command -v asciinema >/dev/null";

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

/// 出口标准整条：真实 SSH 输出 → Recorder → cast 文件 → 真实 `asciinema cat`
/// 还原出的文本与原始输出逐字相等。
#[tokio::test(flavor = "multi_thread")]
async fn cast_from_real_ssh_plays_in_real_asciinema() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (sshd, session) = connect("cast").await;

    // 已知内容：含中文与转义序列（对 UTF-8 边界与 JSON 转义两头施压）
    let expected = "line-one\r\n第二行：中文与 emoji 🙂\n\x1b[32mgreen\x1b[0m tail\n";
    let mut ch = session.channel_open_session().await.unwrap();
    ch.exec(
        false,
        "printf 'line-one\\r\\n第二行：中文与 emoji 🙂\\n\\033[32mgreen\\033[0m tail\\n'",
    )
    .await
    .unwrap();

    // 收集原始输出字节（不经 PTY：exec 直连，字节即链路字节）
    let t0 = std::time::Instant::now();
    let mut raw = Vec::new();
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(20), ch.wait()).await {
            Err(_) => panic!("等待输出超时"),
            Ok(None) => break,
            Ok(Some(ChannelMsg::Data { ref data })) => raw.extend_from_slice(data),
            Ok(Some(ChannelMsg::ExtendedData { .. })) => {}
            // 收到退出状态**不能**停：sshd 在子进程退出时就发 exit-status，管道里剩下的输出
            // 可能随后才到（引擎侧 `exit_status_sent_after_eof_is_still_collected` 钉的正是这个）。
            // 在这里停过，CI 上偶发拿到空输出（2026-09-30 PR #5 的 ubuntu runner）。收到关闭为止。
            Ok(Some(ChannelMsg::Close)) => break,
            Ok(Some(_)) => {}
        }
    }
    assert_eq!(raw, expected.as_bytes(), "自检：链路字节应与预期一致");

    // 录制：相对时序用真实流逝毫秒（分散到达的事件时序不为零）
    let dir = std::env::temp_dir().join(format!(
        "fs-cast-itest-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let now0 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let mut rec = Recorder::start(dir.join("s.cast"), 80, 24, now0).unwrap();
    for piece in raw.chunks(5) {
        // 3 字节一块：中文必被劈在块界，钉多字节跨块
        let now = now0 + t0.elapsed().as_millis() as i64 + 5;
        rec.write(piece, now);
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    rec.stop(now0 + 10_000);
    let cast_path = dir.join("s.cast");

    // ① 自家解析器：往返 diff 为空（出口标准前半）
    let content = std::fs::read_to_string(&cast_path).unwrap();
    let (_, events) = parse_cast(&content).unwrap();
    let replayed: String = events
        .iter()
        .filter(|e| e.kind == "o")
        .map(|e| e.data.as_str())
        .collect();
    assert_eq!(
        replayed, expected,
        "回放文本须与原始输出逐字相等（劈块喂入下多字节不得损坏）"
    );
    assert!(
        !content.contains('\u{FFFD}'),
        "cast 文件里不得出现替换符（回放屏幕上的永久乱码）"
    );

    // ② 真实 asciinema：文件送进容器，`asciinema cat` 的输出与预期逐字相等
    sshd.run(INSTALL_ASCIINEMA)
        .await
        .unwrap_or_else(|e| panic!("容器内安装 asciinema 失败：{e}"));
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
    let out = sshd
        .run(&format!(
            "printf '%s' '{b64}' | base64 -d > /tmp/s.cast && asciinema cat /tmp/s.cast"
        ))
        .await
        .unwrap_or_else(|e| panic!("asciinema cat 执行失败：{e}（文件可能不被真实工具接受）"));
    assert_eq!(
        out, expected,
        "真实 asciinema cat 还原的文本须与原始输出逐字相等"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 坏文件被真实 asciinema 拒绝（对照：我们说「能播」，验证工具真的会播、也会拒）。
#[tokio::test(flavor = "multi_thread")]
async fn real_asciinema_rejects_a_broken_cast() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (sshd, _session) = connect("castbad").await;
    sshd.run(INSTALL_ASCIINEMA).await.unwrap();
    let out = sshd
        .run("printf 'this is not json\\n' > /tmp/bad.cast; asciinema cat /tmp/bad.cast 2>&1; echo rc=$?")
        .await
        .unwrap();
    assert!(
        out.contains("rc=1"),
        "坏文件应被 asciinema 拒绝（非零退出），实得 {out:?}"
    );
}

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
