//! 历史命令的跨 crate 端到端链路（M4a 出口标准的「数据来源 = M1 网格接口 +
//! M4a 本地历史库 + 检索命中」那一半）。
//!
//! 走的是产品自己的三段：终端字节 → `SessionPipe` 的回滚文本（M1 网格接口）→
//! `fs_terminal::history` 提取 → `fs_connmgr::HistoryRepo` 入库 → 检索命中。
//!
//! **不需要 Docker**，故不设 `FS_ITEST` 闸门：它测的是本机进程内的数据通路，
//! 每次都该跑。放在 itest crate 只因为它跨了 terminal 与 connmgr 两个 crate，
//! 哪一个 crate 的单测都装不下整条链。
//!
//! 「一键重发到当前会话」那一半在 `frontend/src/components/HistoryDialog.test.ts`
//! （点条目 → onSend 拿到命令 → App 走 term_input）。如实分开记：这里证明不了
//! 前端的点击行为，那里也证明不了后端的提取与入库。

use fs_connmgr::HistoryRepo;
use fs_terminal::pipe::{PipeOpts, SessionPipe};
use sqlx::SqlitePool;
use std::time::Duration;
use tokio::io::{duplex, AsyncWriteExt};

fn opts() -> PipeOpts {
    PipeOpts {
        grid_rows: 24,
        grid_cols: 120,
        scrollback_lines: fs_terminal::grid::DEFAULT_SCROLLBACK_LINES,
        flow: Default::default(),
        ring_bytes: 64 * 1024,
        session_log: None,
        tap: None,
        decoder: Default::default(),
        record: None,
    }
}

async fn mem_db() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!("../connmgr/migrations")
        .run(&pool)
        .await
        .unwrap();
    pool
}

/// 一段像样的交互记录：提示符行、命令、输出交替。
const TRANSCRIPT: &str = "\
alice@web-01:~$ systemctl status nginx\r
● nginx.service - A high performance web server\r
   Active: active (running) since Thu 2026-08-21 10:03:11 UTC; 2h ago\r
alice@web-01:~$ tail -n 100 /var/log/nginx/error.log\r
2026/08/21 11:02:33 [error] 1234#0: *1 open() failed\r
alice@web-01:~$ export PATH=$HOME/bin:$PATH\r
alice@web-01:~$ systemctl status nginx\r
";

/// 出口标准：终端字节经网格回滚提取出命令、入库、并能检索命中。
#[tokio::test(flavor = "multi_thread")]
async fn terminal_output_becomes_searchable_history() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts());
    let _rx = pipe.render_rx(); // 必须取走：不取则渲染队列满后背压停读
    writer.write_all(TRANSCRIPT.as_bytes()).await.unwrap();
    // 让读取任务把字节喂进网格
    tokio::time::sleep(Duration::from_millis(120)).await;

    // ① M1 网格接口：回滚文本
    let scrollback = pipe.scrollback_text(500);
    assert!(
        scrollback.contains("systemctl status nginx"),
        "回滚里应有刚写入的内容，实得：\n{scrollback}"
    );

    // ② 提取（启发式，见 fs_terminal::history 模块头）
    let commands = fs_terminal::history::extract_commands(&scrollback);
    assert!(
        commands.iter().any(|c| c == "systemctl status nginx"),
        "应提取出 systemctl 那条，实得 {commands:?}"
    );
    assert!(
        commands.iter().any(|c| c.starts_with("tail -n 100")),
        "应提取出 tail 那条，实得 {commands:?}"
    );
    // 同一条命令在记录里出现两次，提取结果里只应有一份
    assert_eq!(
        commands
            .iter()
            .filter(|c| *c == "systemctl status nginx")
            .count(),
        1,
        "提取须去重，实得 {commands:?}"
    );
    // 输出行不得被当成命令
    assert!(
        !commands.iter().any(|c| c.contains("nginx.service")),
        "输出行被误提取了：{commands:?}"
    );
    assert!(
        !commands.iter().any(|c| c.contains("[error]")),
        "日志行被误提取了：{commands:?}"
    );

    // ③ 入库
    let pool = mem_db().await;
    let repo = HistoryRepo::new(&pool);
    for (i, c) in commands.iter().enumerate() {
        repo.record(c, "web-01", "p1", "grid", 1_700_000_000 + i as i64)
            .await
            .unwrap();
    }

    // ④ 检索命中
    let hits = repo.search("systemctl", None, 50).await.unwrap();
    assert_eq!(hits.len(), 1, "实得 {hits:?}");
    assert_eq!(hits[0].command, "systemctl status nginx");
    assert_eq!(hits[0].source, "grid", "提取来的必须标成 grid（近似值）");
    assert_eq!(hits[0].host, "web-01");

    // 按主机过滤：换个主机名就查不到
    assert!(
        repo.search("systemctl", Some("other-host"), 50)
            .await
            .unwrap()
            .is_empty(),
        "按主机过滤须生效"
    );

    // 两个来源共存且可区分：同一条命令再以 sent 记一次，检索得到两条
    repo.record(
        "systemctl status nginx",
        "web-01",
        "p1",
        "sent",
        1_700_001_000,
    )
    .await
    .unwrap();
    let both = repo
        .search("systemctl status nginx", None, 50)
        .await
        .unwrap();
    assert_eq!(both.len(), 2, "两种来源应各占一行，实得 {both:?}");
    // 最近使用的排前面 → sent 那条（时间更晚）
    assert_eq!(both[0].source, "sent", "检索须按最近使用倒序");
}

/// ANSI 控制序列不该进入提取结果——回滚文本是网格渲染后的纯文本，
/// 但真实输出里满是颜色序列，这条钉住「网格确实剥掉了它们」。
#[tokio::test(flavor = "multi_thread")]
async fn ansi_sequences_do_not_leak_into_extracted_commands() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts());
    let _rx = pipe.render_rx();
    // 带颜色的提示符 + 带颜色的输出，是真实 shell 的常态
    writer
        .write_all(b"\x1b[32malice@web\x1b[0m:\x1b[34m~\x1b[0m$ git status\r\n\x1b[31mmodified: a.txt\x1b[0m\r\n")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;

    let commands = fs_terminal::history::extract_commands(&pipe.scrollback_text(100));
    assert!(
        commands.iter().any(|c| c == "git status"),
        "应提取出干净的 `git status`，实得 {commands:?}"
    );
    assert!(
        commands.iter().all(|c| !c.contains('\x1b')),
        "提取结果里不得残留 ANSI 转义，实得 {commands:?}"
    );
}

/// 没有提示符的纯输出流不产出任何历史（宁可漏，不要造假命令让人一键重发）。
#[tokio::test(flavor = "multi_thread")]
async fn plain_output_yields_no_history() {
    let (mut writer, reader) = duplex(64 * 1024);
    let mut pipe = SessionPipe::spawn(reader, opts());
    let _rx = pipe.render_rx();
    writer
        .write_all(
            b"Starting service...\r\nListening on 0.0.0.0:8080\r\nRequest handled in 12ms\r\n",
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;
    let commands = fs_terminal::history::extract_commands(&pipe.scrollback_text(100));
    assert!(
        commands.is_empty(),
        "纯输出不该产出候选命令，实得 {commands:?}"
    );
}
