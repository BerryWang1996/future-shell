//! 渲染链路的高吞吐门禁（M1 出口「性能全指标」里可在共享 runner 上稳定跑的那两条）。
//!
//! M1 出口清单第 3 项列了六个数字，此前只有 SFTP 吞吐一条落到了断言上（scale.rs）。
//! 其余五条里有两条**不需要 GUI**，因而可以进 CI：
//!
//! - **「200MB 高吞吐输出零丢失」** —— 走的是真实链路：容器里 `head -c 200M /dev/zero`
//!   → 真 sshd → russh channel → `SessionPipe`（生产那一个，不是测试里另搭的管子）
//!   → 渲染帧 + ack。逐字节计数，一个不许少。
//! - **「渲染队列水位 ≤ high-water×1.5」** —— **单独一条用例，且消费者故意慢**。
//!   写在零丢失那条里是不行的：那里的消费者收一帧就 ack，队列根本涨不起来，
//!   变异验证时把背压整个关掉断言照样绿（见该用例的文档注）。水位这条让消费者每帧睡
//!   120 ms，逼出真实的背压回合，并在睡眠期间分 12 次采样——单点采样会系统性地
//!   错过暂停点。
//!
//! - **「loopback 连接到可交互 <2s（不含认证往返）」** —— 认证完成时刻由 connect() 回传后减掉。
//! - **「100 并发会话内存增量 ≤100×5MB、每空闲会话 ≤5MB」** —— 出口标准点名量的是
//!   **core 侧**，而本测试进程跑的正是 core 那几个 crate，「本进程 RSS 增量」就是要量的量。
//!   perf.rs 里那条 memory_under_120mb 量的是**整个应用**（含 WebView），是另一回事。
//!
//! 只剩 **冷启动 ≤3s** 一条留在 perf.rs 的本机自查线（全 `#[ignore]`）：它必须真正拉起
//! Tauri 窗口。本文件**不**假装覆盖它。
//!
//! # 为什么零丢失这条必须走真链路
//!
//! `crates/terminal/tests/flow.rs` 已经把 `Batcher` 的合批与滞回钉得很细，但它喂的是
//! 同步 `push`：调用方永远跟得上，背压路径其实没被压过。真实链路里压力来自
//! **russh 的 channel window 与 TCP**——背压一起来，读取任务就停读，窗口不再扩，
//! 对端 sshd 阻塞在 write 上。「零丢失」要保的正是这条路上没有人偷偷把字节扔掉
//! （`drop_backlog` 是有意的丢帧路径，只在消费者停滞时才允许走；本用例的消费者一直在排水，
//! 所以它一次都不该被触发）。
//!
//! 需 `FS_ITEST=1` + 可用 Docker，与其余 itest 同口径。

use fs_itest::sshd::SshdContainer;
use fs_terminal::flow::FlowConfig;
use fs_terminal::pipe::{PipeOpts, SessionPipe};
use russh::client;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// 连上容器并完成口令认证，返回会话句柄与**认证完成的时刻**。
///
/// 返回时刻是为了让「连接到可交互」那条用例能把认证往返减掉——出口原文写的是
/// 「loopback 连接到可交互 <2s（**不含认证往返**）」。
async fn connect(tag: &str) -> (SshdContainer, client::Handle<AcceptAllKeys>, Instant) {
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
    let authed_at = Instant::now();
    (sshd, session, authed_at)
}

/// 出口原文：「200MB 高吞吐输出零丢失且渲染队列水位 ≤ high-water×1.5」。
#[tokio::test(flavor = "multi_thread")]
async fn two_hundred_megabytes_arrive_intact_and_the_queue_stays_under_the_watermark() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    const TOTAL: u64 = 200 * 1024 * 1024;

    let (_sshd, session, _) = connect("render-200m").await;
    let channel = session.channel_open_session().await.unwrap();
    // `head -c` 而不是 `yes`/`cat`：字节数必须是**确定**的，否则「零丢失」无从比对。
    // 用 0x41（'A'）而不是 /dev/zero 的 NUL：NUL 在若干层里都是「空」的同义词，
    // 真丢了也看不出来；可打印字符丢一块，计数与内容都会说话。
    channel
        .exec(false, "tr '\\0' 'A' < /dev/zero | head -c 209715200")
        .await
        .unwrap();

    let flow = FlowConfig::default();
    let high = flow.queue_bytes_high;
    let mut pipe = SessionPipe::spawn(
        channel.into_stream(),
        PipeOpts {
            grid_rows: 24,
            grid_cols: 80,
            scrollback_lines: 1000,
            flow,
            ring_bytes: 256 * 1024,
            session_log: None,
            record: None,
            tap: None,
            decoder: Default::default(),
        },
    );
    let mut rx = pipe.render_rx();

    let started = Instant::now();
    let mut got: u64 = 0;
    let mut peak_queue = 0usize;
    let mut frames = 0u64;
    let mut non_a = 0u64;
    // 上限是墙钟而非帧数：链路卡死时 recv 会一直挂着，没有超时就是测试挂死而不是转红。
    while got < TOTAL {
        let Ok(Some((seq, frame))) = tokio::time::timeout(Duration::from_secs(60), rx.recv()).await
        else {
            panic!(
                "60s 内没有新帧：已收 {got} / {TOTAL} 字节（{} 帧），链路卡住或提前结束",
                frames
            );
        };
        got += frame.len() as u64;
        frames += 1;
        non_a += frame.iter().filter(|&&b| b != b'A').count() as u64;
        // 采样点必须在 ack **之前**：ack 会把水位排下去，排完再采永远采到低水位，
        // 这条断言就成了摆设。
        peak_queue = peak_queue.max(pipe.queue_bytes());
        pipe.ack(seq);
    }
    let elapsed = started.elapsed();

    assert_eq!(
        got, TOTAL,
        "渲染链路字节数不符：收到 {got}，应为 {TOTAL}（{frames} 帧）"
    );
    assert_eq!(
        non_a, 0,
        "收到 {non_a} 个非 'A' 字节：链路把内容改坏了（不只是数量对得上）"
    );
    let cap = (high as f64 * 1.5) as usize;
    assert!(
        peak_queue <= cap,
        "渲染队列峰值水位 {peak_queue} B 超过 high-water×1.5 = {cap} B（high={high}）：\
         背压没有把生产侧压住"
    );
    println!("200 MiB 渲染链路：{elapsed:?}、{frames} 帧、峰值水位 {peak_queue} B / 闸 {cap} B");
}

/// 出口原文后半：「渲染队列水位 ≤ high-water×1.5」。
///
/// **与上一条分开，且消费者必须故意慢。** 上一条用例的消费者收一帧就立刻 ack，
/// 队列根本没机会涨——变异验证时把 `backpressure_active()` 整个关掉，峰值水位仍只有
/// 1.82 MB（闸 3.15 MB），断言照样绿。那就是一条**不可能失败的断言**，
/// 比没有断言更坏：它会被当成「背压已验证」。
///
/// 出口那句话描述的场景本来就是「产出快过渲染」。所以这里让消费者每帧睡一觉
/// （≈3 MB/s，远慢于链路），逼出真实的背压回合：
/// - 背压正常：读取任务在 high-water 处停读 → russh window 不再扩 → 对端阻塞在 write，
///   峰值水位落在 [high, high + 一个读块] 内（flow.rs 记的暂停点区间）。
/// - 背压失效：读取任务照读不误，队列无上界增长，立刻越过 high×1.5。
///
/// 体量取 16 MiB 而非 200 MiB：这条测的是水位不是吞吐，慢消费者下 200 MiB 要跑十分钟。
/// 零丢失那条仍按出口原文跑满 200 MB。
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_consumer_cannot_push_the_render_queue_past_the_watermark() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    const TOTAL: u64 = 16 * 1024 * 1024;
    /// 每帧消费耗时。须显著慢于链路投递，否则队列涨不起来、断言又变成摆设。
    const CONSUMER_DELAY: Duration = Duration::from_millis(120);

    let (_sshd, session, _) = connect("render-watermark").await;
    let channel = session.channel_open_session().await.unwrap();
    // 注意 `\\0`：Rust 字面量里写 `\0` 会是一个**真的 NUL 字节**，命令到了 shell 那头
    // 就被截断，通道立刻 EOF——本用例第一版正是这样，表现为「0 帧、90s 超时」，
    // 而错误信息指向的是链路而不是这行字符串。
    channel
        .exec(false, "tr '\\0' 'A' < /dev/zero | head -c 16777216")
        .await
        .unwrap();

    let flow = FlowConfig::default();
    let high = flow.queue_bytes_high;
    let flow_frames_high = flow.queue_frames_high;
    let mut pipe = SessionPipe::spawn(
        channel.into_stream(),
        PipeOpts {
            grid_rows: 24,
            grid_cols: 80,
            scrollback_lines: 1000,
            flow,
            ring_bytes: 256 * 1024,
            session_log: None,
            record: None,
            tap: None,
            decoder: Default::default(),
        },
    );
    let mut rx = pipe.render_rx();

    let cap = (high as f64 * 1.5) as usize;
    /// 每帧的睡眠切成几段。**只在醒来后采一次是不够的**：读取任务在 high 处停、
    /// 在 low 处续，单点采样落在排水中段的概率远大于落在暂停点上——第一版 549 帧里
    /// 只有 2 帧采到了 ≥high，自检 `peak >= high` 因此濒临假红。切段采样与负载无关，
    /// 纯粹是把观测频率提上去。
    const SAMPLES_PER_FRAME: u32 = 12;

    let frames_high = flow_frames_high;
    let mut got: u64 = 0;
    let mut peak = 0usize;
    let mut peak_frames = 0usize;
    let mut frames = 0u64;
    while got < TOTAL {
        let Ok(Some((seq, frame))) = tokio::time::timeout(Duration::from_secs(90), rx.recv()).await
        else {
            panic!("90s 内没有新帧：已收 {got} / {TOTAL} 字节（{frames} 帧）");
        };
        got += frame.len() as u64;
        frames += 1;
        // 慢消费：睡在 ack **之前**，让队列在这段时间里真的堆起来
        for _ in 0..SAMPLES_PER_FRAME {
            tokio::time::sleep(CONSUMER_DELAY / SAMPLES_PER_FRAME).await;
            let q = pipe.queue_bytes();
            peak = peak.max(q);
            peak_frames = peak_frames.max(pipe.frames_pending());
            assert!(
                q <= cap,
                "第 {frames} 帧期间渲染队列水位 {q} B 超过 high-water×1.5 = {cap} B（high={high}）：\
                 背压没有把生产侧压住，慢消费者下队列在无上界增长"
            );
        }
        pipe.ack(seq);
    }

    println!(
        "慢消费 {TOTAL} B：{frames} 帧、峰值字节水位 {peak} B / high {high} B / 闸 {cap} B、\
         峰值帧水位 {peak_frames} / frames-high {frames_high}"
    );

    // 反向自检：本用例的前提是「确实压出了背压」，否则上面那条 `q <= cap` 什么也没验证。
    //
    // 判据必须是**双维**的。第一版只写了 `peak >= high`（字节维），3 连跑红了 1 次——
    // 不是产品的问题，是自检写错了：背压是双维的，`queue_frames_high` 默认只有 16，
    // 而慢消费下合批出来的帧有几十 KiB，**帧维远早于字节维触发**。
    // 字节水位因此可以合法地一直停在 high 之下（本机实测 1.49 MB / high 2.10 MB），
    // 而背压其实一直在起作用。只看字节维就会把「背压正常工作」误判成「测试没压出背压」。
    assert!(
        peak >= high || peak_frames >= frames_high,
        "两个维度都没摸到 high（字节 {peak}/{high}、帧 {peak_frames}/{frames_high}）：\
         消费者不够慢或链路太慢，水位断言这一跑没有验证任何东西\
         （对照：关掉背压时第 4 帧就会冲到 4.0 MB）"
    );
}

/// 出口原文：「loopback 连接到可交互 <2s（**不含认证往返**）」。
///
/// 「可交互」的判据取**远端第一个可见字节**：开 session channel → 申请 PTY →
/// 起 shell → 收到提示符。到此为止用户可以打字了；再往前的握手与认证被
/// `connect()` 返回的时刻减掉。
#[tokio::test(flavor = "multi_thread")]
async fn loopback_reaches_interactive_within_two_seconds_after_auth() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    use russh::ChannelMsg;

    let (_sshd, session, authed_at) = connect("render-interactive").await;

    let mut channel = session.channel_open_session().await.unwrap();
    channel
        .request_pty(false, "xterm-256color", 80, 24, 0, 0, &[])
        .await
        .unwrap();
    channel.request_shell(false).await.unwrap();

    // 等第一个可见字节。sshd 的 motd/提示符都算——它们出现即意味着 shell 已经在跑。
    let first_byte = loop {
        match tokio::time::timeout(Duration::from_secs(20), channel.wait()).await {
            Err(_) => panic!("20s 内远端一个字节都没吐出来：shell 没起来"),
            Ok(None) => panic!("通道在收到任何输出前就关闭了"),
            Ok(Some(ChannelMsg::Data { .. })) | Ok(Some(ChannelMsg::ExtendedData { .. })) => {
                break Instant::now()
            }
            Ok(Some(_)) => {}
        }
    };

    let to_interactive = first_byte.duration_since(authed_at);
    println!("认证完成 → 可交互：{to_interactive:?}");
    assert!(
        to_interactive < Duration::from_secs(2),
        "认证完成到可交互耗时 {to_interactive:?}，超过出口标准 2s"
    );
}

/// 出口原文：「100 并发会话（50 渲染+50 后台化）**后端进程（core 侧）**常驻内存增量
/// ≤100×5MB、每空闲会话 ≤5MB」。
///
/// # 为什么这条能进 CI 而冷启动那条不能
///
/// 出口标准点名量的是 **core 侧**的增量，不是整个 Tauri 应用。而本测试进程跑的正是
/// core 那几个 crate（fs_sshengine 建连、fs_terminal 跑 SessionPipe），所以
/// 「本进程的 RSS 增量」就是要量的那个量——不需要 GUI、不需要构建产物。
/// perf.rs 里那条 `memory_under_120mb` 量的是**整个应用**（含 WebView），
/// 那才是必须留在本机自查线的东西，两者不是一回事。
///
/// # 口径与它测不到的东西
///
/// - 增量而非绝对值：基线在开会话**之前**采，减掉 tokio 运行时、russh 缓冲池等固定开销。
/// - 「50 渲染 + 50 后台化」：渲染侧的 50 条挂 `SessionPipe`（合批/环形缓冲/网格全在），
///   后台化的 50 条只保留 SSH 通道不建管道——这正是 app 侧把标签切走后的形态。
/// - **测不到**渲染侧 WebView 的内存（那在另一个进程里，出口标准也明说了「单列观测值
///   不设硬闸」）。
/// - RSS 是操作系统口径：分配器可能已经把内存还给了池子而没还给系统，故只设**上限**，
///   偏低不报警。
/// - **测不到「保留但没碰过」的内存**：变异验证时把每会话的环形缓冲从 256 KiB 强制抬到
///   8 MiB（50 条 = 400 MB），本用例**照样绿**——`RingBuffer` 的分配是惰性的，
///   空闲会话一个字节都不写，页从未被提交，RSS 自然不动。这不是漏检，是口径本身：
///   出口标准问的是「**常驻**内存」，没被提交的页本来就不常驻。
///   真正占住的内存能抓到——同一位置改成每会话写满 6 MiB，每会话读数从 0.20 MB 跳到
///   3.20 MB（16 倍）；写满 14 MiB 则 717.8 MB 超预算 500 MB，用例转红。
#[tokio::test(flavor = "multi_thread")]
async fn a_hundred_idle_sessions_stay_within_the_core_side_memory_budget() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    const RENDERED: usize = 50;
    const BACKGROUNDED: usize = 50;
    const PER_SESSION_MB: u64 = 5;

    // **每条会话一条独立 SSH 连接**，不是一条连接上开 100 个 channel。两个理由：
    // ① 产品就是这么做的——app 侧每次 `session_open` 建一条连接（见 app/src/sessions.rs），
    //    每条各带一份加密上下文与缓冲，这才是要量的那个开销；
    // ② 一条连接上开 100 个 channel 根本开不出来：OpenSSH 的 `MaxSessions` 默认 10，
    //    第 11 条起回 `ChannelOpenFailure(ConnectFailed)`（本用例第一版就撞在这里）。
    let (_sshd, first, _) = connect("render-100sess").await;
    let addr = _sshd.addr().await;
    let (user, pass) = (_sshd.username.clone(), _sshd.password.clone());

    // 基线：先跑通「代表性的一条」再采样——第一条会话会把 russh 的加密上下文、
    // tokio 的线程栈、各种 lazy static 一次性拉起来，把这些算进「每会话开销」
    // 会让 100 条的数字被第 1 条污染。
    let warm = first.channel_open_session().await.unwrap();
    warm.exec(false, "true").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let base = self_rss_bytes();
    let mut peak = base;

    let mut sessions = Vec::with_capacity(RENDERED + BACKGROUNDED);
    let mut pipes = Vec::with_capacity(RENDERED);
    let mut channels = Vec::with_capacity(BACKGROUNDED);
    for i in 0..(RENDERED + BACKGROUNDED) {
        // 串行建连：并发握手会撞 sshd 的 MaxStartups（默认 10:30:100，超了随机丢弃）。
        let mut s = client::connect(Arc::new(client::Config::default()), addr, AcceptAllKeys)
            .await
            .unwrap_or_else(|e| panic!("第 {i} 条连接失败：{e}"));
        let ok = s.authenticate_password(&user, &pass).await.unwrap();
        assert!(ok.success(), "第 {i} 条认证失败");
        let ch = s.channel_open_session().await.unwrap();
        // 空闲会话：起一条不产出的命令（sleep），链路建着但没有字节流动——
        // 出口原文说的就是「每**空闲**会话」。
        ch.exec(false, "sleep 600").await.unwrap();
        if i < RENDERED {
            pipes.push(SessionPipe::spawn(
                ch.into_stream(),
                PipeOpts {
                    grid_rows: 24,
                    grid_cols: 80,
                    scrollback_lines: 1000,
                    flow: FlowConfig::default(),
                    ring_bytes: 256 * 1024,
                    session_log: None,
                    record: None,
                    tap: None,
                    decoder: Default::default(),
                },
            ));
        } else {
            channels.push(ch);
        }
        sessions.push(s);
        // 每条都采：**峰值**才是能用的量，终值不是。见下方 peak 的说明。
        peak = peak.max(self_rss_bytes());
    }
    // 让 100 条都真正建好并静默下来，其间继续采样
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        peak = peak.max(self_rss_bytes());
    }
    let after = self_rss_bytes();

    let total = RENDERED + BACKGROUNDED;
    // **取峰值而不是终值。**
    //
    // 第一版用的是「结束时 RSS − 基线」，结果是负数：本机实测 RSS 一路涨到第 40 条的
    // 34.1 MB，然后在第 60 条时**塌回 10.2 MB**，收尾 7.8 MB——低于 24.3 MB 的基线。
    // 那不是内存被释放了，是 Windows 的工作集修剪器（working-set trimmer）把页收走了；
    // RSS 是「当前驻留」的操作系统口径，本来就会被 OS 随时回收。
    // 于是 `after - base` 恒为 0（saturating_sub 兜底），断言**结构上不可能失败**。
    //
    // 峰值是这个预算该用的量：修剪只会让终值更好看，而峰值是这批会话真正同时占住过的上界，
    // 判「超预算」时它是保守方向。
    let delta = peak.saturating_sub(base);
    let delta_mb = delta as f64 / 1024.0 / 1024.0;
    let per_mb = delta_mb / total as f64;
    println!(
        "core 侧 {total} 会话（{RENDERED} 渲染 + {BACKGROUNDED} 后台）：\
         基线 {:.1} MB、峰值 {:.1} MB、终值 {:.1} MB（OS 修剪后）；\
         峰值增量 {delta_mb:.1} MB，每会话 {per_mb:.2} MB",
        base as f64 / 1024.0 / 1024.0,
        peak as f64 / 1024.0 / 1024.0,
        after as f64 / 1024.0 / 1024.0,
    );

    // 自检①：会话必须真的建起来了，否则「内存没涨」是因为什么都没发生
    assert_eq!(pipes.len(), RENDERED);
    assert_eq!(channels.len(), BACKGROUNDED);
    assert_eq!(sessions.len(), RENDERED + BACKGROUNDED);
    // 自检②：采样必须真的观测到了增长。峰值不高于基线只有两种解释——
    // 100 条会话一点内存都没用（不可能），或者采样机制在这台机器上失灵。
    // 两种都意味着下面的预算断言什么也没验证，必须出声而不是默默绿。
    assert!(
        peak > base,
        "峰值 RSS {peak} B 不高于基线 {base} B：采样机制失灵，预算断言这一跑没有验证任何东西"
    );

    let budget_mb = total as u64 * PER_SESSION_MB;
    assert!(
        delta_mb <= budget_mb as f64,
        "core 侧内存峰值增量 {delta_mb:.1} MB 超过预算 {budget_mb} MB（{total}×{PER_SESSION_MB} MB）"
    );
    assert!(
        per_mb <= PER_SESSION_MB as f64,
        "每空闲会话 {per_mb:.2} MB 超过出口标准 {PER_SESSION_MB} MB"
    );

    // 显式持有到断言之后：提前 drop 会让 100 条会话在采样前就拆掉，
    // 于是「增量很小」变成必然——那正是这条用例最容易变成假绿的方式。
    drop(pipes);
    drop(channels);
    drop(sessions);
}

/// 出口原文第一句：「**真实交互：htop/vim 可用**」。
///
/// 这半句此前零载体（itest 下 grep `htop`/`vim` 无命中）。全屏 TUI 是终端实现里最容易
/// 悄悄坏掉的一类：它依赖 PTY 申请成功、`TERM` 传对、**窗口尺寸真的送到了远端**、
/// 备用屏幕进出、光标绝对定位。其中任何一条断了，普通的 `ls` 照样好好的
/// ——所以「echo 跑得通」证明不了这一条。
///
/// # 测什么 / 不测什么
///
/// 测到网格为止的整条字节路径：真 PTY → 真 TUI 程序 → `SessionPipe` → vt100 网格。
/// **不测** xterm.js 的像素呈现（在 WebView 里，itest 够不着）；网格是 best-effort 文本提取，
/// 只用来判断「屏幕上确实出现了该出现的东西」，不做逐格比对。
///
/// 用 busybox `vi` 而不是真 vim/htop：镜像自带，不必在测试里 apt-get
/// （record.rs 已经为装包的间歇性失败付过一次学费）。要验的三条性质——尺寸生效、
/// 全屏重绘、退出还原——换成 vim/htop 走的是同一条路径。
#[tokio::test(flavor = "multi_thread")]
async fn a_full_screen_tui_over_a_real_pty_reaches_the_grid() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    use tokio::io::AsyncWriteExt;

    const ROWS: u16 = 30;
    const COLS: u16 = 100;

    let (_sshd, session, _) = connect("render-tui").await;
    let channel = session.channel_open_session().await.unwrap();
    // 尺寸刻意取非默认的 100×30：远端若没收到 pty 尺寸就会退到 sshd 的 24×80，
    // 下面的 `stty size` 断言当场说话（S292 记的正是这个回归形状）。
    channel
        .request_pty(false, "xterm-256color", COLS as u32, ROWS as u32, 0, 0, &[])
        .await
        .unwrap();
    channel.request_shell(false).await.unwrap();

    let (reader, mut writer) = tokio::io::split(channel.into_stream());
    let pipe = SessionPipe::spawn(
        reader,
        PipeOpts {
            grid_rows: ROWS,
            grid_cols: COLS,
            scrollback_lines: 1000,
            flow: FlowConfig::default(),
            ring_bytes: 256 * 1024,
            session_log: None,
            record: None,
            tap: None,
            decoder: Default::default(),
        },
    );

    /// 轮询网格直到谓词成立，超时即带上当前屏幕内容报错（光说「超时」没法诊断）。
    async fn wait_grid(pipe: &SessionPipe, what: &str, pred: impl Fn(&str) -> bool) -> String {
        for _ in 0..150 {
            let g = pipe.grid_text();
            if pred(&g) {
                return g;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "15s 内网格没有出现「{what}」。当前屏幕：\n----\n{}\n----",
            pipe.grid_text()
        );
    }

    // ① 尺寸真的到了远端：`stty size` 打印 "行 列"
    writer.write_all(b"stty size\n").await.unwrap();
    let g = wait_grid(&pipe, "stty size 的输出", |g| g.contains("30 100")).await;
    assert!(
        g.contains("30 100"),
        "远端 PTY 尺寸不是 30×100（多半退回了 sshd 默认 24×80）"
    );

    // ② 全屏 TUI 真的重绘了整屏：busybox vi 在空行上画 `~`
    writer.write_all(b"vi\n").await.unwrap();
    let g = wait_grid(&pipe, "vi 的空行标记", |g| {
        g.lines()
            .filter(|l| l.trim_start().starts_with('~'))
            .count()
            >= 5
    })
    .await;
    let tildes = g
        .lines()
        .filter(|l| l.trim_start().starts_with('~'))
        .count();
    assert!(
        tildes >= 5,
        "全屏编辑器没有重绘出空行标记（只数到 {tildes} 行 `~`）：\
         备用屏幕或光标绝对定位没走通"
    );

    // ③ 退出后屏幕还原、shell 仍然听话——TUI 退出时若没恢复终端状态，
    //    后续一切输出都会错位。
    //
    // 按键必须**分次发并留出反应时间**：一口气 `\x1b:q!\r` 会被 vi 当成一串普通输入
    // （第一版就是这样——`echo …` 整条被打进了编辑缓冲区，屏幕上只剩半截 "T-OK-MARKER"）。
    // 真人也是这么操作的：先退出插入模式，看到命令行冒号，再打命令。
    writer.write_all(b"\x1b").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    writer.write_all(b":q!\r").await.unwrap();
    // 先等 vi 真的退出（空行标记消失），再发 shell 命令；否则命令又会落进编辑器
    wait_grid(&pipe, "vi 退出（`~` 标记消失）", |g| {
        g.lines()
            .filter(|l| l.trim_start().starts_with('~'))
            .count()
            < 5
    })
    .await;

    writer
        .write_all(b"echo TUI-EXIT-OK-MARKER\n")
        .await
        .unwrap();
    wait_grid(&pipe, "退出后的回显", |g| {
        g.contains("TUI-EXIT-OK-MARKER")
    })
    .await;

    println!("PTY 全屏 TUI：尺寸 {ROWS}×{COLS} 生效、vi 重绘 {tildes} 行、退出后 shell 正常");
    pipe.shutdown();
}

/// 本进程当前 RSS（字节）。sysinfo 的跨平台口径，与 perf.rs 的 `memory()` 同源。
fn self_rss_bytes() -> u64 {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = Pid::from_u32(std::process::id());
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::new().with_memory(),
    );
    sys.process(pid).expect("采不到本进程").memory()
}
