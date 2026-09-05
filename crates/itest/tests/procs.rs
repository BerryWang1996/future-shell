//! 服务器进程管理的容器集成测试（M4a 出口标准：进程列表采集 + 终止操作 itest
//! 经 exec 通道，非 Linux 降级路径单测）。
//!
//! 与 `crates/sshengine` 里的解析单测分工：那边用手写的 `ps` 输出钉解析规则，
//! 这边证明**真实 `ps` 的输出形状确实被那套规则覆盖**。两者缺一不可——单测的
//! 样本是我自己写的，我写错的地方它一样测不出来。
//!
//! 需 `FS_ITEST=1` + 可用 Docker；未设时打印 skip 并返回（与既有 itest 一致）。

use fs_itest::sshd::SshdContainer;
use fs_sshengine::procs::{kill_command, parse_process_list, process_list_command};
use russh::client;
use std::sync::Arc;

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

/// 走一次 exec 通道，返回 (exit, stdout, stderr)。
async fn exec(
    session: &client::Handle<AcceptAllKeys>,
    cmd: &str,
) -> fs_sshengine::verify::ExecOutput {
    let mut ch = session.channel_open_session().await.unwrap();
    ch.exec(false, cmd).await.unwrap();
    fs_sshengine::verify::run_exec_channel(&mut ch).await
}

async fn connect(tag: &str) -> (SshdContainer, client::Handle<AcceptAllKeys>) {
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
    (sshd, session)
}

/// 出口标准前半：真实 `ps` 的输出能被解析出结构化进程表。
#[tokio::test(flavor = "multi_thread")]
async fn process_list_parses_real_ps_output() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (_sshd, session) = connect("procslist").await;
    let o = exec(&session, process_list_command()).await;
    let (code, stdout, stderr) = (o.code, o.stdout, o.stderr);
    assert_eq!(code, Some(0), "ps 应成功（stderr: {stderr}）");

    let procs = parse_process_list(&stdout);
    assert!(
        procs.len() >= 3,
        "容器里至少该有若干进程，实得 {} 条；原始输出：\n{stdout}",
        procs.len()
    );
    // PID 1 必然存在（容器主进程），且命令行非空——这两条同时成立才说明字段
    // 没有整体错位（错位时 pid 常还能解析出来，command 却落到别的列上）
    let init = procs
        .iter()
        .find(|p| p.pid == 1)
        .unwrap_or_else(|| panic!("PID 1 必然存在；解析结果：{procs:?}"));
    assert!(
        !init.command.trim().is_empty(),
        "PID 1 的命令行不该为空（字段错位的典型症状），实得 {init:?}"
    );
    assert!(
        !init.user.trim().is_empty(),
        "用户列不该为空，实得 {init:?}"
    );
    // 至少有一条能解析出 RSS：全为 None 说明数值列整体没对上
    assert!(
        procs.iter().any(|p| p.rss_kb.is_some()),
        "应至少有一条解析出 RSS，否则数值列整体错位；解析结果：{procs:?}"
    );
    // 用户名不得带 ps 的截断标记 `+`（`-o user:32` 的理由）
    assert!(
        procs.iter().all(|p| !p.user.ends_with('+')),
        "用户名被截断了（应由 -o user:32 避免）：{:?}",
        procs
            .iter()
            .filter(|p| p.user.ends_with('+'))
            .collect::<Vec<_>>()
    );
}

/// 出口标准后半：终止操作经 exec 通道生效，且进程确实消失。
#[tokio::test(flavor = "multi_thread")]
async fn kill_removes_the_process() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (_sshd, session) = connect("prockill").await;

    // 目标进程养在一条**保持打开**的 exec 通道里。
    //
    // 不用 `sleep 600 & echo $!`：后台进程随那条 exec 通道关闭一起没了（sshd 给
    // 会话发 SIGHUP），于是列表里根本找不到它——第一版就是这么失败的，而失败信息
    // 「刚起的 PID 应出现在进程列表里」指向的是采集，与真实原因无关。
    //
    // `echo $$; exec sleep 600`：`exec` 用 sleep 的映像替换 shell 自身，**pid 不变**，
    // 所以打印出来的就是 sleep 的真实 pid。通道留着不关，进程就一直在。
    let mut victim = session.channel_open_session().await.unwrap();
    victim.exec(false, "echo $$; exec sleep 600").await.unwrap();
    let pid: u32 = {
        let mut buf = String::new();
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), victim.wait())
                .await
                .expect("等目标进程报出 pid 超时")
            {
                Some(russh::ChannelMsg::Data { ref data }) => {
                    buf.push_str(&String::from_utf8_lossy(data));
                    if let Some(line) = buf.lines().next() {
                        if let Ok(p) = line.trim().parse() {
                            break p;
                        }
                    }
                }
                Some(_) => continue,
                None => panic!("目标进程通道提前关闭，缓冲：{buf:?}"),
            }
        }
    };

    // 列表里应能看见它，且命令行认得出是 sleep
    let o = exec(&session, process_list_command()).await;
    let stdout = o.stdout;
    let found = parse_process_list(&stdout)
        .into_iter()
        .find(|p| p.pid == pid)
        .unwrap_or_else(|| panic!("刚起的 PID {pid} 应出现在进程列表里"));
    assert!(
        found.command.contains("sleep"),
        "PID {pid} 的命令行应含 sleep，实得 {:?}",
        found.command
    );

    // 终止：命令由白名单构造，不含任何用户可控自由文本
    let cmd = kill_command(pid, "TERM").expect("TERM 应被接受");
    let ko = exec(&session, &cmd).await;
    let (kc, ke) = (ko.code, ko.stderr);
    assert_eq!(kc, Some(0), "kill 应成功（stderr: {ke}）");

    // **直接观测**目标进程的死亡：它那条通道会收到退出状态/信号并关闭。
    // 比轮询 ps「找不到了」强——后者也可能是采集本身出了问题。
    let mut died = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while let Ok(msg) = tokio::time::timeout_at(deadline, victim.wait()).await {
        match msg {
            None => {
                died = true;
                break;
            }
            Some(russh::ChannelMsg::ExitSignal { .. })
            | Some(russh::ChannelMsg::ExitStatus { .. }) => {
                died = true;
                break;
            }
            Some(_) => continue,
        }
    }
    assert!(died, "PID {pid} 收到 TERM 后其通道应终结");

    // 再确认它确实从进程表里消失了（信号异步生效，轮询而不是 sleep 一个魔数）
    let mut gone = false;
    for _ in 0..30 {
        let o2 = exec(&session, process_list_command()).await;
        let out = o2.stdout;
        if !parse_process_list(&out).iter().any(|p| p.pid == pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(gone, "PID {pid} 应已从进程表消失");
}

/// 杀一个不存在的 pid：远端非零退出 + 明确的 stderr。
///
/// 这条钉的是「失败要带原因」——`No such process` 与 `Operation not permitted`
/// 对用户是两件完全不同的事，统一成「终止失败」等于把唯一的线索抹掉。
#[tokio::test(flavor = "multi_thread")]
async fn killing_a_missing_pid_reports_why() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (_sshd, session) = connect("prockillmiss").await;
    // 取一个几乎不可能存在的 pid（远超容器内 pid 上限的常见值）
    let cmd = kill_command(4_000_000, "TERM").unwrap();
    let o = exec(&session, &cmd).await;
    let (code, err) = (o.code, o.stderr);
    assert_ne!(code, Some(0), "杀不存在的进程应非零退出");
    assert!(
        !err.trim().is_empty(),
        "应有 stderr 说明原因，否则 UI 只能显示一句无信息量的「终止失败」"
    );
}

/// 非 Linux / 无 POSIX `ps` 的降级路径：空 stdout + 非零退出必须被判成**失败**，
/// 而不是一张空进程表。
///
/// 这里不真去找一台非 Linux 机器，而是让 `ps` 不可用来复现同一条件——判定逻辑
/// 吃的就是 (exit_code, stdout, stderr) 三元组，与操作系统无关。
#[tokio::test(flavor = "multi_thread")]
async fn missing_ps_is_reported_as_failure_not_empty_list() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let (_sshd, session) = connect("procnops").await;
    // PATH 清空 → 两条回落命令都找不到 ps
    let o = exec(&session, "PATH=/nonexistent; ps -eo pid=,args=").await;
    let (code, stdout, stderr) = (o.code, o.stdout, o.stderr);
    assert!(parse_process_list(&stdout).is_empty(), "不该解析出任何进程");
    let reason = fs_sshengine::procs::classify_empty(code, &stdout, &stderr)
        .expect("必须判为失败，而不是「这台机器上没有进程」");
    assert!(
        reason.contains("失败") || reason.contains("ps"),
        "失败原因须可读，实得 {reason}"
    );
}
