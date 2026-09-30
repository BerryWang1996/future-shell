// session_cmd.rs
use crate::connect_failure::ConnectFailure;
use crate::events::GuiEvents;
use crate::state::AppState;
use fs_connmgr::{Profile, ProfileRepo};
use std::sync::Arc;
// Emitter：session_close 显式 emit session:closed、watchdog/重连循环 emit session:disconnected（P2-16）
use tauri::{Emitter, State};

/// 标准终端模式（对齐 OpenSSH 客户端的 request_pty 默认集）。
///
/// 空 modes 让远端 PTY 全靠 sshd 默认值——回声/规范输入/信号这些关键位是否设、设成什么，
/// 取决于服务端配置。`sudo` 读密码（经 /dev/tty 关回声）、readline 行编辑这类交互程序，
/// 在规范输入位不确定的 PTY 上可能表现为「输入没反应/卡住」。补齐一套可预期的模式，
/// 与 OpenSSH 行为一致。值均为 RFC 4254 §8 的经典 opcode。
const STANDARD_PTY_MODES: &[(russh::Pty, u32)] = &[
    (russh::Pty::ECHO, 1),   // 回声开
    (russh::Pty::ICANON, 1), // 规范输入（行缓冲）
    (russh::Pty::ISIG, 1),   // 信号
    (russh::Pty::IEXTEN, 1),
    (russh::Pty::ICRNL, 1),    // 输入 CR→NL
    (russh::Pty::OPOST, 1),    // 输出后处理
    (russh::Pty::ONLCR, 1),    // 输出 NL→CRNL
    (russh::Pty::VINTR, 3),    // Ctrl+C = ^C
    (russh::Pty::VEOF, 4),     // Ctrl+D = ^D
    (russh::Pty::VERASE, 127), // 退格 = DEL
    (russh::Pty::VKILL, 21),   // Ctrl+U
    (russh::Pty::VQUIT, 28),   // Ctrl+\
    (russh::Pty::VSUSP, 26),   // Ctrl+Z
    (russh::Pty::VSTART, 17),  // Ctrl+Q
    (russh::Pty::VSTOP, 19),   // Ctrl+S
    (russh::Pty::IUTF8, 1),    // UTF-8
];

/// 「本轮重连」的取消令牌表（审计 P1-7）。
///
/// 缺陷原状：`session_reconnect_stop` 置的是 `LiveSession.stop`——那是**会话生命周期**的信号
///（`session_close` 也用它通知 watchdog 退出），一旦置 true 就永远是 true。用户点一次「停止重连」，
/// 之后所有手动 `session_reconnect` 进入循环第一个检查点就撞上 `stop == true` 直接 return，
/// 表现为「停止之后重连按钮彻底失灵，只能关掉标签重开」——而「停止」在 UI 语义上只是
/// 「别再自动重试了」，不该没收用户手动重试的权利。
///
/// 修法是把两个语义拆成两枚令牌：会话关闭仍用 `LiveSession.stop`（语义不变），本轮循环另起一枚
/// 一次性令牌，`reconnect_loop` 启动时新建、退出时摘除，`session_reconnect_stop` 只取消当前这一枚。
/// 下一次手动重连是新的一轮、新的令牌，不受上一轮取消影响。
///
/// 为什么放在 session_cmd 而不是 `LiveSession` 或 `AppState`：本轮令牌的生命周期与
/// `reconnect_loop` **完全同构**（进循环即生、出循环即灭），既不属于会话本体（会话可以从头到尾
/// 没有任何一轮重连），也不是需要跨模块共享的应用状态；放进那两处只会把一个局部量提升成
/// 全局字段，还要另外回答「谁负责清理」。此外这两个文件属于并行改动方，跨文件加字段会冲突。
///
/// 用 `watch::Sender<bool>` 而非 `AtomicBool`：退避等待最长 30 s，必须能被取消**立刻**打断
///（`select!` 上挂 `changed()`）；原子量只能靠轮询，会把「点了停止还转半分钟」留在原地。
static RECONNECT_ROUNDS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::watch::Sender<bool>>>>,
> = std::sync::LazyLock::new(Default::default);

/// 取消某会话**当前这一轮**重连；返回是否真的取消到了一轮。
/// 无正在跑的循环时为 no-op：此时本就没有自动重试在进行，不需要（也不该）留下任何持久状态。
fn cancel_reconnect_round(session_id: &str) -> bool {
    let token = RECONNECT_ROUNDS.lock().unwrap().get(session_id).cloned();
    match token {
        Some(tx) => {
            let _ = tx.send(true);
            true
        }
        None => false,
    }
}

/// Err 是**结构化的** [`ConnectFailure`]（2026-09-01，路线图 4c）：Tauri 把它序列化成
/// JS 侧的 rejection 对象，前端失败面板据 `remaining/tried/category/has_jump` 决定
/// 挂哪些按钮。`summary` 仍是错误的 Display 原文——errorText / toast / 本条日志都用它，
/// 老式「一句字符串」的消费方一个字都不用改语义。
#[tauri::command]
pub async fn session_open(
    profile_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<String, ConnectFailure> {
    // 连接失败（认证失败/主机密钥拒绝/超时）必须进文件日志：ipc_log 记了「谁、带什么参」，
    // 这里补「为什么没成」。否则用户报「连不上」，日志里只有一条 ipc:invoke，没有失败原因。
    let result = session_open_impl(profile_id.clone(), state).await;
    if let Err(e) = &result {
        tracing::error!(
            target: "future_shell_app::conn",
            profile_id = %profile_id,
            error = %e.summary,
            category = e.category,
            "session_open 失败"
        );
    }
    result
}

async fn session_open_impl(
    profile_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<String, ConnectFailure> {
    // Arc<AppState> 整体透传（原先是 app/registry/db/vault/pending 五个句柄各传各的）：
    // 装配链上新增的 `shutdown_session_subsystems`（P0-4/P0-5）要动 sftp_ops/transfers 两张表，
    // 它们只挂在 AppState 上；继续散着传等于每加一个子系统就要给 5 个函数各加一个参数。
    open_session_core(state.inner().clone(), profile_id).await
}

/// 「按 profile 开一个新会话并装配进注册表」的共用本体，返回新 session_id。
///
/// 抽出来是为了让**两个入口走同一条路**：前端 `session_open`（IPC）与 MCP 的
/// `sessions.open`（[`crate::mcp`]）。连接 + 装配 + journal 这段若各写一份，
/// 「用户点连接」与「外部客户端开会话」的行为就会各自漂移——而漂移的永远是
/// 安全相关的细节（凭据怎么取、主机密钥怎么验、装配顺序）。
pub(crate) async fn open_session_core(
    app_state: Arc<AppState>,
    profile_id: String,
) -> Result<String, ConnectFailure> {
    let session_id = uuid::Uuid::new_v4().to_string();

    // 档案还没取到之前的失败没有主机信息可填（bare）；取到之后的一律带上 host/port/
    // username/has_jump——失败面板顶部固定显示身份，防「在 A 机面板上给 B 机挑私钥」。
    let profile = {
        let repo = ProfileRepo::new(app_state.db.pool());
        repo.get(
            uuid::Uuid::parse_str(&profile_id).map_err(|e| ConnectFailure::bare(e.to_string()))?,
        )
        .await
        .map_err(|e| ConnectFailure::bare(e.to_string()))?
    };
    // 闭包要吃掉 profile；JoinError 那一路在闭包外映射，留一份只读克隆给它。
    let profile_for_err = profile.clone();

    let session_id_c = session_id.clone();
    // russh connect 的 kbd/hostkey 回调为同步阻塞 → spawn_blocking 隔离（F15）；
    // connect() 收 pool: &SqlitePool —— Connector 自持 pool 克隆供 check_server_key 落库（G1 契约）
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            // pending 克隆一份进 GuiEvents，本体留在 AppState 供 watchdog/重连循环使用（P2-16）
            let events: Arc<dyn fs_sshengine::events::SessionEvents> = Arc::new(GuiEvents {
                app: app_state.app.clone(),
                session_id: session_id_c.clone(),
                pending: app_state.pending.clone(),
                target: crate::events::target_label(&profile),
                profile_id: profile.id.to_string(),
            });
            // 凭据按需取单条、进程内不留全量明文快照；vault Arc 移入闭包，secret() 时 try_lock 解密（F20⑤）
            let secrets = VaultSecrets::new(app_state.vault.clone());
            let pool = app_state.db.pool().clone();
            // 引擎错误**结构化**带走（认证类的 tried/remaining/notes 在这里进 DTO）。
            let handle = fs_sshengine::connect::connect(&profile, &secrets, &pool, events)
                .await
                .map_err(|e| Box::new(ConnectFailure::from_error(&e, &profile)))?;
            // 共用装配序列（Task 22 重连同样调用；重连复用同一 session_id）。
            // 装配阶段的失败是 String：归 other，只有 summary 有意义。
            establish_session(
                app_state.clone(),
                session_id_c.clone(),
                &profile,
                handle,
                24,
                80,
            )
            .await
            .map_err(|m| Box::new(ConnectFailure::other(m, &profile)))?;
            // Task 22 Step 2: 成功后 mark_open。审计 P1-12：journal 是旁路，写失败只记日志——
            // 此刻会话已装配完成并登记进注册表，若因一次 SQLite 写错误把 IPC 判成失败，
            // 前端拿不到 session_id、不画标签，后端却留着一个谁也管不到的活会话。
            crate::journal::mark_open_best_effort(&app_state.db, &session_id_c, Some(&profile_id))
                .await;
            Ok::<(), Box<ConnectFailure>>(())
        })
    })
    .await
    .map_err(|e| ConnectFailure::other(e.to_string(), &profile_for_err))?
    .map_err(|b| *b)?;
    Ok(session_id)
}

/// SSH 环境变量名合法性（审计 P1-22）：`[A-Za-z_][A-Za-z0-9_]*`。
///
/// 名字要原样进 SSH_MSG_CHANNEL_REQUEST("env") 并被远端 shell 当作变量名使用，含空格、`=`、
/// 换行的键在服务端的处理是未定义的（轻则整条被丢，重则被拼进 shell 上下文）。校验放在发送前，
/// 非法项跳过 + warn，而不是整条会话失败——用户配置里一个手滑的键不该导致连不上。
fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// session_open 与 session_reconnect（Task 22）共用的会话装配。
/// rows/cols 统一 u32（russh request_pty/window_change 收 u32；F5）。
/// `state` 提供 app/registry/db/vault/pending 及子系统关停入口，供末尾的断线 watchdog
/// 与其启动的自动重连循环使用（P2-16）。
/// `profile` 全量传入（原先只传 profile_id 字符串）：TERM 与环境变量要在这里生效（审计 P1-22）。
pub async fn establish_session(
    state: Arc<AppState>,
    session_id: String,
    profile: &Profile,
    handle: russh::client::Handle<fs_sshengine::connect::Connector>,
    rows: u32,
    cols: u32,
) -> Result<(), String> {
    let app = state.app.clone();
    let registry = state.registry.clone();
    // S291：先装配新通道（channel/pty/shell 均可失败），**成功后**才收尾旧条目并替换注册表。
    // 复审一波中危 #2：原实现在 registry.remove 之后才做可失败的 channel 装配，任一失败即
    // 注册表空 + 无新 watchdog → 会话孤儿化、自动重连静默停摆。推迟 remove 后失败不孤儿化。
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| e.to_string())?;
    // 审计 P1-22：TERM 取自连接配置（ProfileDialog 存了却从不生效——用户把 TERM 改成 `xterm`
    // 或 `vt100` 以适配老旧服务端，程序照旧发 xterm-256color，远端 tput/ncurses 的行为与用户
    // 的设定完全对不上）。空串按未设置处理，缺省仍是 xterm-256color。
    let term = profile
        .term
        .term
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("xterm-256color");
    // request_pty(want_reply, term, col_width, row_height, pix_w, pix_h, modes)——列在前、行在后
    channel
        .request_pty(false, term, cols, rows, 0, 0, STANDARD_PTY_MODES)
        .await
        .map_err(|e| e.to_string())?;
    // 审计 P1-22：环境变量必须在 request_shell **之前**发——RFC 4254 §6.4 的 env 请求只对
    // 此后启动的进程生效，shell 起来之后再发就只是白发一遍。
    // 服务端拒绝是**常态**而非异常：sshd 默认 `AcceptEnv` 白名单极窄（通常只有 LANG/LC_*），
    // 大多数键会被静默丢弃。故 want_reply=false 且失败只记 debug——因为对端没接受几个变量
    // 就让整个会话建不起来，是拿一个可选增强去否决核心功能。
    for (key, value) in &profile.env {
        if !is_valid_env_key(key) {
            tracing::warn!(%session_id, key, "环境变量名不合法（需 [A-Za-z_][A-Za-z0-9_]*），已跳过");
            continue;
        }
        if let Err(e) = channel.set_env(false, key.as_str(), value.as_str()).await {
            tracing::debug!(%session_id, key, error = %e, "set_env 未被接受（服务端 AcceptEnv 白名单所致属常态）");
        }
    }
    channel
        .request_shell(false)
        .await
        .map_err(|e| e.to_string())?;
    // Channel 非 Clone：split() 拆读写。读半部桥接为 owned AsyncRead 进 SessionPipe；
    // 写半部（data_bytes/window_change/close）入注册表（F5）。
    let (read_half, write_half) = channel.split();
    // 终结信号通道（补丁 M-4 置换 P2-16 裸 Notify）：桥接任务见通道终结恰发一次 LinkEnd——
    // 通道 EOF + ExitStatus(0) → RemoteExitZero（正常结束）；非零退出 / 信号死亡 / 传输级 EOF → 断线
    let (end_tx, end_rx) = tokio::sync::mpsc::channel::<crate::sessions::LinkEnd>(1);
    let reader = crate::sessions::ChannelReader::spawn(read_half, end_tx);
    // M4a 终端内传输（rz/sz）：拦截器必须在 pipe 之前造好（它是 PipeOpts.tap）。
    // 沙箱与开关在此定格，不在传输时回查设置——见 zmodem_bridge 模块头。
    let zmodem_tap = build_zmodem_tap(&app, &state, &session_id, &profile.id.to_string()).await;
    // M4a 会话录屏：录制器**常驻挂载**（idle 零开销），手动启停只翻内部标志——
    // 不必在运行中的管道上动结构。存进 state.recorders 供命令侧启停。
    let recorder = std::sync::Arc::new(std::sync::Mutex::new(fs_terminal::record::Recorder::idle(
        std::path::PathBuf::new(),
    )));
    state
        .recorders
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(session_id.clone(), recorder.clone());
    // M7.4：档案里的终端编码在这里**第一次有了消费方**。此前 `term.encoding` 可填可存却
    // 无人读——用户选了 GBK 照样满屏乱码。标签认不出时按 UTF-8 起步并留日志：连接不该
    // 因为一个编码名拼错就失败（界面上那个下拉只给固定几项，走到这里说明是手改过的 JSON），
    // 而用户改回去之后 `term_set_encoding` 不必重连即可生效。
    let decoder = match fs_terminal::decode::StreamDecoder::new(profile.term.encoding.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(%e, session_id = %session_id, "档案里的终端编码无法识别，按 UTF-8 起步");
            fs_terminal::decode::StreamDecoder::default()
        }
    };
    let mut pipe = fs_terminal::SessionPipe::spawn(
        reader,
        fs_terminal::PipeOpts {
            grid_rows: rows as u16,
            grid_cols: cols as u16,
            // 审计2 #35：滚动回看行数取自连接配置（档案校验已拦 >MAX；0/None 按默认）。
            // 旧实现恒用固定默认值——用户在档案里改的 scrollback 从不生效。
            scrollback_lines: profile
                .term
                .scrollback_lines
                .map(|v| v as usize)
                .unwrap_or(fs_terminal::grid::DEFAULT_SCROLLBACK_LINES),
            flow: Default::default(),
            ring_bytes: fs_terminal::ring::DEFAULT_RING_BYTES,
            // M4a 会话纯文本日志：settings 键 session.log 决定开关/目录/模板/追加。
            // 未开启时为 None（零开销）；建不出来（目录不可写）亦为 None，不阻断连接。
            session_log: state
                .build_session_log(&session_id, &profile.host, &profile.username)
                .await,
            // 未启用（沙箱不可用）时为 None：整条 fan-out 零开销，不是「装一个恒透传的」
            tap: zmodem_tap
                .clone()
                .map(|t| t as std::sync::Arc<dyn fs_terminal::pipe::ByteTap>),
            record: Some(recorder),
            decoder,
        },
    );
    let rx = pipe.render_rx(); // take 语义：rx 交给泵任务，pipe 本体进注册表
                               // 审计 P0-4/P0-5（重连的另一半）：旧连接的 SFTP 通道与传输管理器必须在此关停。
                               // 只换注册表条目而不动这两张表，旧代次的 worker 会带着一条已死的 russh Handle
                               // 继续跑——用户看到「终端重连好了，文件传输却永远卡在 0%」。放在这里而不是
                               // registry.remove 那一段：shutdown 含最长 5 s 的有界等待（await），而下面
                               // 「remove 旧 + spawn 泵 + insert 新」刻意保持为同步段以收窄 session_close 插队窗口。
    state.shutdown_session_subsystems(&session_id).await;
    // 传输目标锁的端点别名（审计2 #13）：一次 DNS 往返，必须在下面那段刻意保持同步的
    // 「remove 旧 + spawn 泵 + insert 新」之**前**做完，否则一个 await 会重新拉开
    // session_close 的插队窗口。失败退回空集，绝不挡连接。
    let alias_set = crate::state::transfer_endpoint_aliases_of(profile).await;
    // S291（二审复活残余闭环）：先等在途 term_input 收尾——这是收尾阶段**唯一的** pipe 相关 await，
    // 必须在旧条目仍在注册表时进行。如此刻 session_close 抢入，它仍能 registry.remove 到旧条目
    // 并置其 stop（reconnect_loop 后置检查可捕获）；若把此 await 留在 registry.remove 之后，
    // session_close 的 registry.remove 将返回 None、拿不到 Arc 置 stop，后置检查无从捕获 → 复活。
    if let Some(old) = registry.get(&session_id) {
        drop(old.write.lock().await); // 等在途 term_input 收尾（old 为克隆 Arc，动锁不动注册表条目）
    }
    // 新管道就绪后才收尾旧会话并替换注册表（P2-26）；此后「remove 旧 + spawn 泵 + insert 新」
    // 全为同步段（无 await），把 session_close 可插队窗口收窄至指令级。首连无旧条目，此分支为空操作。
    if let Some(old) = registry.remove(&session_id) {
        old.pipe.shutdown(); // shutdown 协同停读取任务 + stop 臂合批收尾（S214）；旧 render 泵随旧管道 rx 断开退出
    }
    crate::sessions::spawn_render_pump(app.clone(), session_id.clone(), rx);
    let (stop, _stop_rx) = tokio::sync::watch::channel(false);
    // insert 返回新分配的连接代次（审计 P0-4）：此后 sftp_ops/transfers 一律按
    // (session_id, generation) 查表，上一代的缓存条目再也不会被命中。
    let generation = registry.insert(
        session_id.clone(),
        Arc::new(crate::sessions::LiveSession {
            pipe: Arc::new(pipe),
            write: tokio::sync::Mutex::new(write_half),
            // 句柄单存（方案甲 P2-15/30）：Handle 非 Clone，Task 21 SFTP 经 registry 取用，不另设句柄表
            handle: tokio::sync::Mutex::new(handle),
            profile_id: profile.id.to_string(),
            // 传输目标锁的端点键，在此定格（见 sessions::LiveSession::target_endpoint）
            target_endpoint: crate::state::transfer_endpoint_of(profile),
            // 别名解析要一次 DNS 往返，只在装配时做一次（见 state::transfer_endpoint_aliases_of）
            endpoint_aliases: alias_set,
            stop,
            reconnecting: std::sync::atomic::AtomicBool::new(false),
            last_reason: std::sync::Mutex::new(None),
        }),
    );
    // ZMODEM 拦截器登记**必须在** shutdown_session_subsystems 之后：那一步会清掉
    // 本 session_id 的旧拦截器（重连时旧的持着已死代次的状态），先登记就会被它抹掉，
    // 表现为「重连后终端能用、rz/sz 却说未启用传输」。
    if let Some(t) = zmodem_tap {
        state
            .zmodem
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(session_id.clone(), t);
    }
    tracing::info!(%session_id, generation, term, "会话已登记（新连接代次）");
    // host facts 采集（M2 出口第 15 项）：一律 spawn，**绝不 await**。
    // 出口原文「采集失败不阻塞连接」不只是「捕获错误」——即便成功，一次 exec 也是
    // 几百毫秒的 SSH 往返，而这段时间用户正盯着「正在连接」。
    // 放在登记之后：采集要经 registry 取 exec 适配器，登记之前那张表里还没有这条会话。
    crate::state::spawn_host_facts_collection(state.clone(), session_id.clone());
    // 会话终结 watchdog（补丁 M-4）：RemoteExitZero → session:closed{reason:"remote_exit"}（正常结束）；
    // 其余断链 → session:disconnected{reason, attempt:0} + 自动重连循环（P2-16）
    spawn_connection_watchdog(state, session_id, generation, end_rx);
    Ok(())
}

/// 审计2 #39：terminal input IPC 的规模与等待边界。
/// 普通键盘输入很小，但大型粘贴或异常 WebView 调用可以制造大分配（全量 base64 解码）
/// 并长时间占用会话写路径（写锁与 data_bytes 都是无界 await）。
/// 上限值选 1 MiB：任何真实的「往终端里粘一份文件内容」都远小于它，越界的只可能是异常输入。
pub(crate) const MAX_TERM_INPUT_BYTES: usize = 1024 * 1024; // 1 MiB
/// `MAX_TERM_INPUT_BYTES` 对应的标准 base64（含 padding）编码长度上限，
/// 在解码**之前**闸掉：先解码再数长度就白做了那次大分配。
const MAX_TERM_INPUT_BASE64: usize = (MAX_TERM_INPUT_BYTES / 3 + 1) * 4;
/// 写锁等待预算。锁等不到说明另一路输入/窗口调整占着写半部长达 3 秒——
/// 继续等只会让 IPC 请求无限排队；如实报错比静默挂起强。
const WRITE_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
/// data_bytes 等待预算：同上，占着写路径的对端 5 秒不回即放弃这条输入。
const DATA_BYTES_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[tauri::command]
pub async fn term_input(
    session_id: String,
    data_b64: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let bytes = decode_input(&data_b64)?;
    // M7.4：串口会话不在 SSH 注册表里。**先查 SSH 后回落串口**（SSH 是绝大多数），
    // 回落到的那一路走同一条 `term_input` 是刻意的——前端不必知道这条标签底下是什么传输，
    // 键盘、粘贴、广播、快捷键、组合命令栏全部原样可用。
    if state.registry.get(&session_id).is_none() {
        if let Some(sp) = state.serial.get(&session_id) {
            return sp.link.write(bytes).await;
        }
    }
    let session = state.registry.get(&session_id).ok_or("no session")?;
    // 写半部 Mutex 串行化保证输入字节顺序；data_bytes 收 impl Into<Bytes>（Vec<u8> 可入；F5）
    // S289：guard 具名绑定——链式临时量在块尾与 session 的 drop 次序冲突（E0597）
    // 审计2 #39：锁与写入各带等待预算（见常量注释），超时如实报错。
    let guard = tokio::time::timeout(WRITE_LOCK_TIMEOUT, session.write.lock())
        .await
        .map_err(|_| format!("写锁等待超时（{} 秒）", WRITE_LOCK_TIMEOUT.as_secs()))?;
    tokio::time::timeout(DATA_BYTES_TIMEOUT, guard.data_bytes(bytes))
        .await
        .map_err(|_| format!("写入等待超时（{} 秒）", DATA_BYTES_TIMEOUT.as_secs()))?
        .map_err(|e| e.to_string())
}

/// 审计2 #39：解码 + 尺寸上限，抽成纯函数让上限离线可测（命令体要 AppState 才跑得到）。
/// 两道闸：编码长度预筛（免大分配）→ 解码后按**真实字节数**硬限（b64 预筛是收紧的
/// 上界而非精确值，见 [`MAX_TERM_INPUT_BASE64`] 注释）。
fn decode_input(data_b64: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    if data_b64.len() > MAX_TERM_INPUT_BASE64 {
        return Err(format!("输入超过 {} 字节上限", MAX_TERM_INPUT_BYTES));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_TERM_INPUT_BYTES {
        return Err(format!("输入超过 {} 字节上限", MAX_TERM_INPUT_BYTES));
    }
    Ok(bytes)
}

#[tauri::command]
pub async fn term_resize(
    session_id: String,
    cols: u16,
    rows: u16,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    // 串口没有 window-change 协议（对端根本不知道终端多大），但**网格必须跟着变**：
    // 它是 AI 取文与滚动缓冲的载体，尺寸不对会让取到的文本按错误的列宽折行。
    if state.registry.get(&session_id).is_none() {
        if let Some(sp) = state.serial.get(&session_id) {
            sp.pipe.resize(rows, cols);
            return Ok(());
        }
    }
    let session = state.registry.get(&session_id).ok_or("no session")?;
    session.pipe.resize(rows, cols);
    // window_change(col_width, row_height, pix_w, pix_h) 收 u32（F5）；S289：guard 具名绑定（同 term_input）
    // 审计2 #39 同源边界：与 term_input 共用一把写锁，等待同样要有预算——异常 WebView
    // 调用频发的 resize 不该被别的写持有者无限排队。
    let guard = tokio::time::timeout(WRITE_LOCK_TIMEOUT, session.write.lock())
        .await
        .map_err(|_| format!("写锁等待超时（{} 秒）", WRITE_LOCK_TIMEOUT.as_secs()))?;
    guard
        .window_change(cols as u32, rows as u32, 0, 0)
        .await
        .map_err(|e| e.to_string())
}

/// 前端 xterm.js write 回调回报**已渲染帧的 seq**（累计确认，S295）。
///
/// 参数从 `bytes: u32` 改为 `seq: u64`：字节口径下丢一帧即永久冻结（队首欠账
/// 弹不掉、帧维水位单调爬升至锁死，见 `fs_terminal::flow` 模块头）；序号口径下
/// 丢帧的债只欠到下一个 ack 为止。前端不再上报字节数 ⇒ 它无从污染字节账本。
#[tauri::command]
pub async fn term_ack(
    session_id: String,
    seq: u64,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if state.registry.get(&session_id).is_none() {
        if let Some(sp) = state.serial.get(&session_id) {
            sp.pipe.ack(seq); // 背压/合批与 SSH 完全同一套
            return Ok(());
        }
    }
    let session = state.registry.get(&session_id).ok_or("no session")?;
    session.pipe.ack(seq);
    Ok(())
}

/// 当前会话的终端编码（规范名）。状态栏与设置页回显用。
#[tauri::command]
pub async fn term_encoding(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    if let Some(sp) = state.serial.get(&session_id) {
        return Ok(sp.pipe.encoding().to_string());
    }
    let session = state.registry.get(&session_id).ok_or("no session")?;
    Ok(session.pipe.encoding().to_string())
}

/// 运行时换终端编码（M7.4 出口标准：**切换不需要重连**）。
///
/// 只改这一条活会话的解码器；要长期生效由前端另存进档案（两件事分开：临时看一眼 GBK
/// 不该顺手改掉档案，而改了档案也不该逼用户重连）。
#[tauri::command]
pub async fn term_set_encoding(
    session_id: String,
    label: String,
    state: State<'_, Arc<AppState>>,
) -> Result<String, String> {
    // 串口尤其需要换编码：裸板的中文提示基本都是 GBK。
    if let Some(sp) = state.serial.get(&session_id) {
        sp.pipe.set_encoding(Some(&label))?;
        return Ok(sp.pipe.encoding().to_string());
    }
    let session = state.registry.get(&session_id).ok_or("no session")?;
    session.pipe.set_encoding(Some(&label))?;
    Ok(session.pipe.encoding().to_string())
}

/// 界面上可选的终端编码清单（值 + 人读名）。
///
/// 由 Rust 侧给而不是前端自己写死：能不能解出来是 `StreamDecoder` 说了算，两边各写一份
/// 迟早分叉——分叉的那一半就是「列表里有、选了却报错」。
#[tauri::command]
pub fn term_encodings() -> Vec<(String, String)> {
    fs_terminal::decode::TERM_ENCODINGS
        .iter()
        .map(|(v, h)| (v.to_string(), h.to_string()))
        .collect()
}

#[tauri::command]
pub async fn session_close(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    // M7.4 串口：自己那条收尾路径（无 SSH 子系统、无重连循环、无 journal 记账）。
    // 事件由串口任务在 Closed 时发（session:closed），与 SSH 这一路语义相同。
    if let Some(sp) = state.serial.get(&session_id) {
        sp.link.close().await;
        sp.pipe.shutdown();
        return Ok(());
    }
    // 主动移除 pending 的 TOFU/kbd tx：等待中的回调收到 RecvError → 按拒绝收尾，连接任务不再挂起（F20②）
    state.pending.clear_session(&session_id);
    // 会话都要关了，正在跑的重连轮次没有继续的意义（P1-7：这里取消的是「本轮」，
    // 会话级退出仍由下面的 stop 通知——两个信号各司其职）
    cancel_reconnect_round(&session_id);
    if let Some(session) = state.registry.remove(&session_id) {
        // 通知 watchdog 与自动重连循环退出（F20③）：watchdog 见 registry 移除 / stop=true 即静默退出
        let _ = session.stop.send(true);
        // 审计 P0-4/P0-5：顺序是「先关传输子系统 → 再关管道 → 最后随 Arc drop 关 SSH 连接」。
        // 原实现只把 sftp_ops/transfers 从表里 remove 掉——那只是让引用消失，派发任务与每个
        // 传输任务都是独立 spawn 出去的、各自持有本地文件句柄，删表项一个也停不了。
        // 用户点了「关闭标签」，后台还在继续改他的文件，直到 SSH 连接自己断开为止。
        // 反过来先关管道/连接也不行：worker 会在半途撞上 IO 错误，写了一半的临时件失去有序收尾。
        state.shutdown_session_subsystems(&session_id).await;
        // 关闭管道：shutdown 协同停止读取任务，stop 臂发起合批收尾（S214），render 泵随 rx 断开退出（EOF 泵仅自退、不代发 session:closed）
        session.pipe.shutdown();
    }
    // session:closed 来源之一（reason=user_closed，用户主动关闭）；另一来源为 watchdog 的通道 EOF +
    // ExitStatus(0) 路径（reason=remote_exit，补丁 M-4）。渲染泵 EOF 仍不代发（P2-16）；异常断线走 session:disconnected
    let _ = state.app.emit(
        "session:closed",
        serde_json::json!({ "session_id": session_id, "reason": "user_closed" }),
    );
    // 审计 P1-12：会话此刻已确实关闭，journal 写失败不得反过来把 IPC 判成失败——
    // 那会让前端保留一个点什么都没反应的死标签。
    crate::journal::mark_closed_best_effort(&state.db, &session_id).await;
    Ok(())
}

/// 手动「立即重连」入口（banner 按钮）：占 per-session 重连锁后委托 Task 19 的 reconnect_loop。
/// CAS 失败 = watchdog 自动重连循环在跑 → no-op（banner 仍按退避刷新 attempt，不重复拨号；P2-26）。
#[tauri::command]
pub async fn session_reconnect(
    session_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let Some(session) = state.registry.get(&session_id) else {
        return Err("no session".into());
    };
    if session
        .reconnecting
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        return Ok(());
    }
    let app_state = state.inner().clone();
    // 首断原因复用（补丁 M-4）：watchdog 置入的 last_reason；缺省（未经断线即手动触发）按 transport_eof
    let reason = session
        .last_reason
        .lock()
        .unwrap()
        .unwrap_or(crate::sessions::DisconnectReason::TransportEof);
    tauri::async_runtime::spawn(async move {
        // reconnect_loop 的 Drop 守卫保证任一退出路径释放 reconnecting 锁与本轮取消令牌
        reconnect_loop(app_state, session_id, session, reason).await;
    });
    Ok(())
}

/// 「停止重连」入口：取消**当前这一轮**重连循环，循环在下一检查点（或退避等待中）立即退出。
///
/// 审计 P1-7：原实现置的是 `LiveSession.stop`——会话生命周期信号，一旦置起就再也不会复位，
/// 此后任何手动 `session_reconnect` 都会在循环第一个检查点被它挡回来，「停止」变成了「永久禁用重连」。
/// 现在只取消本轮令牌：本轮结束、令牌随之摘除，下一次手动重连是干净的一轮。
///
/// 无循环在跑时为 no-op（不留持久状态）——代价是「停止」与「watchdog 刚决定要启动一轮」之间
/// 存在一个极窄窗口，落在窗口里的一次点击会失效。用户再点一次即可，这远好于用一个永久标志
/// 去覆盖这个窗口、然后把手动重连一并锁死。
#[tauri::command]
pub async fn session_reconnect_stop(session_id: String) -> Result<(), String> {
    if !cancel_reconnect_round(&session_id) {
        tracing::debug!(%session_id, "停止重连：当前无重连轮次在跑，忽略");
    }
    Ok(())
}

#[tauri::command]
pub async fn sessions_unclosed(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::journal::JournalRow>, String> {
    crate::journal::unclosed(&state.db)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn session_close_all(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let ids: Vec<String> = state
        .registry
        .sessions
        .lock()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    for id in ids {
        // 与 session_close 一致：清 pending、停本轮重连、置停止，再关子系统/管道并记账（F20②）
        state.pending.clear_session(&id);
        cancel_reconnect_round(&id);
        if let Some(session) = state.registry.remove(&id) {
            let _ = session.stop.send(true);
            // 审计 P0-4/P0-5：与 session_close 同序（先子系统、后管道、最后连接），G3 契约要求二者一致
            state.shutdown_session_subsystems(&id).await;
            session.pipe.shutdown();
        }
        // 审计 P1-12：journal 旁路。退出前的批量关闭尤其不能因一次写库失败中途 return——
        // 那会把后面还没关的会话全部漏掉（原实现的 `?` 正是这个效果）。
        crate::journal::mark_closed_best_effort(&state.db, &id).await;
    }
    Ok(())
}

/// 自动重连上限：指数退避 1→2→4…→30s 封顶，达上限停止并提示（F20③「最大次数」）。
pub const MAX_RECONNECT_ATTEMPTS: u32 = 10;

/// 重连退避的封顶值：再久用户就该自己按「立即重连」了，继续翻倍只是让界面看起来死掉。
pub const RECONNECT_DELAY_CAP: std::time::Duration = std::time::Duration::from_secs(30);

/// 退避序列的下一档：翻倍、30 s 封顶（M1 出口原文「断线自动重连（**指数退避**）」）。
///
/// 抽成纯函数的理由（2026-08-23 补）：这条规则原本是循环体里**两处**重复的
/// `delay = (delay * 2).min(30s)`，没有任何测试覆盖——出口原文点名要「指数退避」，
/// 而当时能证明的只有「有个 sleep」。两处重复还意味着改一处漏一处不会被发现。
///
/// 不做抖动（jitter）：本产品的重连是**单机单会话**行为，不存在惊群；抖动只会让
/// 「第几次重试、还要等多久」变得不可预期，而 banner 上正显示着这个数字。
// 上限与封顶是两件事：10 次尝试用完就**放弃**（弹提示让用户决定），而不是无限期以 30s 重试。
// 编译期断言（同 schedule.rs:62 的先例）：区间破了直接编不过，比运行期测试更早、也不会被
// `--lib` 之外的跑法漏掉。太少 → 一次网络抖动就放弃；太多 → 一台已下线的机器重试到天荒地老。
const _: () = assert!(MAX_RECONNECT_ATTEMPTS >= 3);
const _: () = assert!(MAX_RECONNECT_ATTEMPTS <= 20);
// 退避封顶必须落在「用户还愿意等」与「别把服务器打疼」之间，且须小于总重试预算的量级。
const _: () = assert!(RECONNECT_DELAY_CAP.as_secs() >= 5);
const _: () = assert!(RECONNECT_DELAY_CAP.as_secs() <= 60);

pub fn next_reconnect_delay(current: std::time::Duration) -> std::time::Duration {
    (current * 2).min(RECONNECT_DELAY_CAP)
}

/// 会话终结监视（每连接一个，`establish_session` 末尾 spawn；P2-16 引入、补丁 M-4 区分终结语义）。
/// 等待 `ChannelReader` 的终结信号 `LinkEnd`，三分支：
/// - `RemoteExitZero`（通道 EOF + ExitStatus(0)，shell 正常退出）= 正常结束：按 session_close
///   同式清理（清 pending、置 stop、关子系统、shutdown 管道、移出注册表）后显式 emit
///   `session:closed{reason:"remote_exit"}`，前端据此移除标签——**不进重连**；
/// - `RemoteExitNonZero(code)` / `RemoteSignaled` / `TransportEof` = 断线：emit
///   `session:disconnected{reason, attempt:0}`（**非 closed，前端不得移除标签**），占重连锁后
///   启动 `reconnect_loop`（S290：信号死亡归断线语义，不再误判正常结束）；
/// - recv 得 None（桥接任务未发信号即退出 = session_close 已 shutdown 管道）/ registry 已移除 /
///   stop=true → 用户主动关闭路径，静默退出。
///
/// 依据：0.62.4 的 `client::Handle::join` 为 `&mut self`，经 LiveSession.handle Mutex 长持会与
/// SFTP 经 `&self` 开通道互相阻塞，故取读桥接终结路径而非 join；keepalive 超限（keepalive_max
/// 回包连续失败）由 russh 终止连接任务、读端随即传输级 EOF。
/// 装配 ZMODEM 拦截器（M4a rz/sz）。
///
/// 返回 `None` 的两种情形，都不阻断连接——终端内传输是增强，不是核心：
/// ① 设置里关掉了（`term.zmodem` 的 `enabled` 为 false）；
/// ② 沙箱目录解析不出来（没有可写落点，装了也只会在传输开始时才失败）。
///
/// 沙箱在此解析一次并定格进拦截器。不在传输时回查设置的理由见
/// `zmodem_bridge` 模块头：设置可改，回查会让同一条连接上前后两次传输落到不同
/// 目录下，用户找不到文件且无法自证。
async fn build_zmodem_tap(
    app: &tauri::AppHandle,
    state: &Arc<AppState>,
    session_id: &str,
    profile_id: &str,
) -> Option<Arc<crate::zmodem_bridge::ZmodemTap>> {
    let cfg = state.zmodem_config().await;
    if !cfg.enabled {
        return None;
    }
    // 按 profile_id 取沙箱而不是 session_id：拦截器要在会话登记之前造好，
    // 那时注册表里还查不到这个 session（见 state::sandbox_root_for_profile）。
    let sandbox = match state.zmodem_sandbox_for(profile_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(%session_id, error = %e, "ZMODEM 沙箱不可用，本会话不启用终端内传输");
            return None;
        }
    };
    Some(crate::zmodem_bridge::ZmodemTap::new(
        app.clone(),
        state.clone(),
        session_id.to_string(),
        sandbox,
        cfg.auto_receive,
    ))
}

fn spawn_connection_watchdog(
    state: Arc<AppState>,
    session_id: String,
    generation: u64,
    mut end_rx: tokio::sync::mpsc::Receiver<crate::sessions::LinkEnd>,
) {
    tauri::async_runtime::spawn(async move {
        use crate::sessions::{DisconnectReason, LinkEnd};
        let app = state.app.clone();
        let registry = state.registry.clone();
        // 代次守卫（审计4 潜伏竞态）：watchdog 只对本代次会话动作——重连会把同 session_id
        // 换成新代次，旧 watchdog 若无代次检查会在新会话插入后误判其为断线。现路径虽不可达
        //（自动重连由本 watchdog 内联驱动、recv 先于重插入），仍作纵深防御。
        let end = match end_rx.recv().await {
            Some(end) => end,
            // 无终结信号（end_tx 随桥接任务 drop 而非 send）：两种来源——(a) session_close
            //（用户主动关闭，会话已移出注册表，桥接因 reader drop 静默退出）；(b) 读取任务
            // 异常死亡（panic——会话仍在注册表且 stop 未置，桥接同样因 reader drop 静默退出）。
            // (a) 静默退出（session_close 已自发 session:closed）；(b) 必须按断线处理，
            // 否则永久静默冻结、无 banner（审计3）。
            None => match registry.get_with_generation(&session_id) {
                Some((g, session)) if g == generation && !*session.stop.borrow() => {
                    tracing::warn!(%session_id, "会话终结无信号且会话仍存活（读取任务异常退出），按传输级断线处理");
                    LinkEnd::TransportEof
                }
                _ => return, // session_close / 已 stop / 已换代 → 静默退出
            },
        };
        let Some((g, session)) = registry.get_with_generation(&session_id) else {
            return;
        }; // 会话已被 session_close 移除
        if g != generation || *session.stop.borrow() {
            return;
        } // 已换代或用户主动关闭
        match end {
            LinkEnd::RemoteExitZero => {
                tracing::info!(%session_id, "remote shell exited with status 0; closing session");
                // 与 session_close 同式清理（F20②）；用户主动关闭之外的另一 session:closed 来源（补丁 M-4）
                state.pending.clear_session(&session_id);
                cancel_reconnect_round(&session_id);
                if let Some(s) = registry.remove(&session_id) {
                    let _ = s.stop.send(true);
                    // 审计 P0-4/P0-5：本分支原先只做 registry.remove + pipe.shutdown，SFTP 通道与
                    // 传输管理器被整个漏掉——远端 shell 自己 exit 时，后台传输 worker 仍在写用户
                    // 的本地文件，且此后没有任何路径会再来清它（会话条目已不在注册表里）。
                    state.shutdown_session_subsystems(&session_id).await;
                    s.pipe.shutdown();
                }
                // Task 22 Step 2 接线点：正常结束须记账，防启动恢复误报（P1-12：旁路，失败只记日志）
                crate::journal::mark_closed_best_effort(&state.db, &session_id).await;
                let _ = app.emit(
                    "session:closed",
                    serde_json::json!({ "session_id": session_id, "reason": "remote_exit" }),
                );
                return;
            }
            // RemoteExitNonZero(code) / RemoteSignaled / TransportEof = 断线：非 closed，前端不得移除标签
            end => {
                // S294：emit 前复查 stop，收窄「session_close 已过上方 stop 检查、尚未 emit」的微秒级竞态
                //（复审一波低；残余 TOCTOU 良性——user_closed 后追发的 disconnected 指向已移除标签，前端忽略）
                if *session.stop.borrow() {
                    return;
                }
                let reason = end
                    .disconnect_reason()
                    .unwrap_or(DisconnectReason::TransportEof);
                session.last_reason.lock().unwrap().replace(reason);
                let mut payload = serde_json::json!({
                    "session_id": session_id, "reason": reason.as_str(), "attempt": 0 });
                if let Some(code) = end.exit_code() {
                    payload["exit_code"] = serde_json::json!(code); // exit_nonzero 附远端退出码
                }
                tracing::warn!(%session_id, reason = reason.as_str(), "session disconnected; starting auto-reconnect");
                let _ = app.emit("session:disconnected", payload);
            }
        }
        if session
            .reconnecting
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return; // 已有重连循环在跑（手动 session_reconnect 先行占锁），不重复启动
        }
        let reason = session
            .last_reason
            .lock()
            .unwrap()
            .unwrap_or(DisconnectReason::TransportEof);
        reconnect_loop(state, session_id, session, reason).await;
    });
}

/// S291：重连循环早退终态事件——profile 取不到 / 配置损坏等无法继续重连时发 gave_up，
/// 防标签永困「断线重连中」而无任何后续事件（复审一波中危 #4A）。复用 disconnected 载荷形状。
fn emit_reconnect_gave_up(
    app: &tauri::AppHandle,
    session_id: &str,
    reason: crate::sessions::DisconnectReason,
) {
    let _ = app.emit(
        "session:disconnected",
        serde_json::json!({
            "session_id": session_id, "reason": reason.as_str(),
            "attempt": MAX_RECONNECT_ATTEMPTS, "gave_up": true }),
    );
}

/// 指数退避自动重连循环：watchdog 异常路径与手动 `session_reconnect`（Task 22）共用。
/// 调用方必须已 CAS 占领 `session.reconnecting`；Drop 守卫保证任一退出路径释放（P2-26）。
/// 重连复用同一 session_id，恢复 PTY 尺寸、不恢复屏幕（spec §2.1，UI 明示）。
/// `reason` = 首断原因（watchdog 由 `LinkEnd` 归类 / 手动重连读 `LiveSession.last_reason`）：
/// 随每次 attempt 的 `session:disconnected` 续发；达上限时 reason 改记最后一次 `connect()`
/// 失败的归类（服务器拒绝 → `refused` / IO 类 → `io_error`，补丁 M-4），banner 据此措辞。
///
/// 审计 P1-7：本轮循环持有自己的一次性取消令牌（`RECONNECT_ROUNDS`），与会话级 `stop` 并列。
/// 两个信号在每个检查点与退避 `select!` 上**都要看**：stop = 会话关了（连同本轮一起结束）；
/// 本轮令牌 = 用户只是不想再自动重试了（会话仍在，日后可手动重连）。
pub async fn reconnect_loop(
    state: Arc<AppState>,
    session_id: String,
    session: Arc<crate::sessions::LiveSession>,
    reason: crate::sessions::DisconnectReason,
) {
    let app = state.app.clone();
    let registry = state.registry.clone();
    // 本轮取消令牌：进循环即生、出循环即灭（下方 Drop 守卫）
    let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    let cancel_tx = Arc::new(cancel_tx);
    RECONNECT_ROUNDS
        .lock()
        .unwrap()
        .insert(session_id.clone(), cancel_tx.clone());

    struct RoundGuard {
        session: Arc<crate::sessions::LiveSession>,
        session_id: String,
        token: Arc<tokio::sync::watch::Sender<bool>>,
    }
    impl Drop for RoundGuard {
        fn drop(&mut self) {
            self.session
                .reconnecting
                .store(false, std::sync::atomic::Ordering::Release);
            // 只摘**自己那一枚**：本轮退出与下一轮登记之间没有互斥，按 session_id 无条件 remove
            // 会把新一轮的令牌摘掉，那一轮的「停止重连」随即失灵——正是 P1-7 换了个位置复发。
            let mut map = RECONNECT_ROUNDS.lock().unwrap();
            if map
                .get(&self.session_id)
                .is_some_and(|cur| Arc::ptr_eq(cur, &self.token))
            {
                map.remove(&self.session_id);
            }
        }
    }
    let _guard = RoundGuard {
        session: session.clone(),
        session_id: session_id.clone(),
        token: cancel_tx.clone(),
    };

    // 取原 profile（重连恢复 PTY 尺寸，spec §2.1；网格尺寸改为每轮拨号前新读，见 S293）
    let repo = fs_connmgr::ProfileRepo::new(state.db.pool());
    let Ok(pid) = uuid::Uuid::parse_str(&session.profile_id) else {
        // S291：配置损坏 → 终态事件，防标签永困断线态（复审一波中危 #4A）
        emit_reconnect_gave_up(&app, &session_id, reason);
        return;
    };
    let Ok(profile) = repo.get(pid).await else {
        // S291：profile 已删除 → 终态事件，防标签永困断线态（复审一波中危 #4A）
        emit_reconnect_gave_up(&app, &session_id, reason);
        return;
    };

    // 凭据按需取单条、无全量明文快照（F20⑤）
    let secrets = VaultSecrets::new(state.vault.clone());
    let mut stop_rx = session.stop.subscribe();
    let mut delay = std::time::Duration::from_secs(1);
    // 达上限时记录的最终失败原因：缺省沿用首断原因（循环未及拨号即被取消/退出的情形）
    let mut give_up_reason = reason;
    for attempt in 1u32..=MAX_RECONNECT_ATTEMPTS {
        // 取消 + 存活性检查：会话已关（stop / registry 移除）或用户停了本轮，即退出（F20③、P1-7）
        if *stop_rx.borrow_and_update() || *cancel_rx.borrow_and_update() {
            return;
        }
        if registry.get(&session_id).is_none() {
            return;
        }
        let _ = app.emit(
            "session:disconnected",
            serde_json::json!({
            "session_id": session_id, "reason": reason.as_str(), "attempt": attempt }),
        );
        // 退避等待可被两枚信号中任一枚打断（select! 不阻塞 worker；F20③）
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = stop_rx.changed() => { return; }
            _ = cancel_rx.changed() => { return; }
        }
        if *stop_rx.borrow_and_update() || *cancel_rx.borrow_and_update() {
            return;
        }
        let events: Arc<dyn fs_sshengine::events::SessionEvents> = Arc::new(GuiEvents {
            app: app.clone(),
            session_id: session_id.clone(),
            pending: state.pending.clone(),
            target: crate::events::target_label(&profile),
            profile_id: profile.id.to_string(),
        });
        // connect() 收 pool: &SqlitePool（Connector 自持克隆供落库；G1 契约）。
        // 同步 kbd/hostkey 回调经 block_in_place + 120s 超时等待 UI，不 pin worker（F20④）。
        let pool = state.db.pool().clone();
        match fs_sshengine::connect::connect(&profile, &secrets, &pool, events).await {
            Ok(handle) => {
                // S291：防复活——connect 期间用户可能已关闭会话（session_close 移除条目 + 置 stop）；
                // 插入前最后校验，任一不满足即弃连接返回（复审一波中危 #3）。
                if *stop_rx.borrow_and_update() {
                    return;
                }
                if registry.get(&session_id).is_none() {
                    return;
                }
                // S293：每轮拨号前新读网格尺寸——重连窗口内的 term_resize 已更新旧 pipe 网格，
                // 循环前一次性快照会漏掉最新 resize（复审一波低 #5）。
                let (rows, cols) = (
                    session.pipe.grid_rows() as u32,
                    session.pipe.grid_cols() as u32,
                );
                // S291：establish_session 失败视同 attempt 失败 → 退避重试而非静默 return
                //（establish 已推迟 remove、失败不孤儿化；复审一波中危 #2/#4B）。
                // 成功则新 watchdog 接管，本循环返回后 Drop 守卫释放重连锁与本轮令牌。
                match establish_session(
                    state.clone(),
                    session_id.clone(),
                    &profile,
                    handle,
                    rows,
                    cols,
                )
                .await
                {
                    Ok(()) => {
                        // S291（二审复活残余闭环）：establish 期间 session_close 可能抢入（channel 装配窗口）——
                        // 它已移除条目并置旧会话 stop。此时新条目虽已 insert（复活），须立即回收并静默返回，
                        // 不发虚假「已重连」。stop_rx 监听旧会话 stop，session_close 置位即在此捕获。
                        // 注意此处**只看 stop 不看本轮令牌**：连接已经建成，用户「停止自动重试」的诉求
                        // 已经不复存在，把刚接好的会话再拆掉才是违背意图。
                        if *stop_rx.borrow_and_update() || registry.get(&session_id).is_none() {
                            if let Some(s) = registry.remove(&session_id) {
                                s.pipe.shutdown(); // s drop 时释放写半部 + handle，关闭复活的连接
                            }
                            return;
                        }
                        // `state` 是这条事件里唯一的**状态迁移**标记，且只有此处会带它：
                        // events.rs 的 SessionEvents::status() 同样发 session:status，但那些是连接
                        // 过程的进度文案（第几跳、尝试哪种认证方法、拒连理由），发生在**还没连上**的
                        // 时刻。前端原先无条件把任何一条 session:status 判成「已连接」，重连期第一行
                        // 进度就会把标签点绿。前端改判 state 之后，这个字段不能再省（frontend/src/lib/
                        // session-status.ts 与 lib/connect-state.test.ts 的跨语言守卫钉住本行）。
                        let _ = app.emit(
                            "session:status",
                            serde_json::json!({
                            "session_id": session_id,
                            "message": "已重连（远端为新 shell，屏幕内容未恢复）",
                            "state": "connected" }),
                        );
                        return;
                    }
                    Err(e) => {
                        // 装配失败归 IO 类；诊断入日志防归类后蒸发（S294）
                        tracing::warn!(%session_id, attempt, %e, "reconnect establish failed; retrying");
                        give_up_reason = crate::sessions::DisconnectReason::IoError;
                        delay = next_reconnect_delay(delay);
                    }
                }
            }
            Err(e) => {
                // 失败归类（补丁 M-4）：IO/存储类 → io_error；Connect/Auth/HostKey/Ssh 等 → refused（服务器拒绝）。
                // S294：失败原因（含 vault notes 等诊断）入日志，防归类后蒸发（复审一波低）。
                tracing::warn!(%session_id, attempt, %e, "reconnect connect failed");
                use fs_sshengine::Error as EngineError;
                give_up_reason = match e {
                    EngineError::Io(_) | EngineError::Storage(_) | EngineError::Sqlx(_) => {
                        crate::sessions::DisconnectReason::IoError
                    }
                    _ => crate::sessions::DisconnectReason::Refused,
                };
                delay = next_reconnect_delay(delay);
            }
        }
    }
    // 达上限：停止自动重连，前端可经 session_reconnect 手动再触发（F20③「停止入口」）
    let _ = app.emit(
        "session:disconnected",
        serde_json::json!({
        "session_id": session_id, "reason": give_up_reason.as_str(),
        "attempt": MAX_RECONNECT_ATTEMPTS, "gave_up": true }),
    );
}

/// 凭据源：按需向 Vault 取单条记录，进程内不留全量明文快照（F20⑤/spec §3.2）。
/// secret() 为同步回调（russh 认证状态机调用），经 try_lock 取解锁态 Store；
/// 明文以 Zeroizing 返回，用毕即擦。
pub struct VaultSecrets {
    vault: Arc<tokio::sync::Mutex<Option<fs_vault::Store>>>,
}

impl VaultSecrets {
    pub fn new(vault: Arc<tokio::sync::Mutex<Option<fs_vault::Store>>>) -> Self {
        Self { vault }
    }
}

/// vault 的用途枚举 → 引擎的用途枚举（审计2 #20）。
///
/// 两侧各有一份 enum 是刻意的：引擎不依赖 vault 的存储实现（见 `secrets::SecretSource`）。
/// 代价是这里成了唯一的接缝，而接缝是会漂的——加一个变体、改一处映射，编译器只在
/// **本函数**里报错，别处一声不吭。故 `secret_kind_mapping_is_total_and_faithful`
/// 用 `wire_tag`/`aad_tag` 这两个已被 AAD 认证、永不改名的串把三对映射逐条钉住。
fn to_engine_kind(k: fs_vault::SecretKind) -> fs_sshengine::secrets::SecretKind {
    match k {
        fs_vault::SecretKind::Password => fs_sshengine::secrets::SecretKind::Password,
        fs_vault::SecretKind::PrivateKey => fs_sshengine::secrets::SecretKind::PrivateKey,
        fs_vault::SecretKind::ApiKey => fs_sshengine::secrets::SecretKind::ApiKey,
    }
}

impl fs_sshengine::secrets::SecretSource for VaultSecrets {
    fn secret(&self, id: u64) -> Result<fs_sshengine::secrets::Secret, fs_sshengine::Error> {
        // 审计 P1-16：取密算 Vault 活动——一次拨号可能连取多条凭据，把它算作「用户在用 Vault」，
        // 否则一场长时间的认证过程会被自动锁定从中间打断。
        crate::commands::vault_cmd::touch_vault_activity();
        // S289：Error::Auth 为三字段（tried/remaining/notes，S38）。vault busy / 未解锁 / 记录缺失
        // 属「本地原因未能真正发起」，语义入 notes；remaining 是「服务器还允许什么」，留空。
        let guard = self
            .vault
            .try_lock()
            .map_err(|_| fs_sshengine::Error::Auth {
                tried: vec![],
                remaining: vec![],
                notes: vec!["vault busy（并发访问，请重试）".into()],
            })?;
        // 锁着 → 类型化变体（2026-08-31）：引擎据此走「弹框问本次口令」的降级，
        // 不再把连接判死成一句 auth failed。此前它伪装成 Auth 终态，
        // 用户看到的是 tried:[]/notes:["vault 未解锁"] 的原始串（真机日志在案）。
        let store = guard
            .as_ref()
            .ok_or(fs_sshengine::Error::SecretSourceLocked)?;
        // get_typed 而非 get + list：kind 与明文必须来自**同一次**查表与同一次解封，
        // 否则中间任何一次 save/delete 都能让「按 A 的类别使用 B 的材料」发生（审计2 #20）。
        let (kind, bytes) = store.get_typed(id).map_err(|e| fs_sshengine::Error::Auth {
            tried: vec![],
            remaining: vec![],
            notes: vec![format!("vault record {id}: {e}")],
        })?;
        Ok(fs_sshengine::secrets::Secret {
            kind: to_engine_kind(kind),
            bytes,
        })
    }
}

/// Task 22 Step 2: 清空未关闭会话记录（用户选择"全部跳过"时调用）
#[tauri::command]
pub async fn sessions_discard_unclosed(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    crate::journal::discard_all(&state.db)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 审计2 #20：vault 的用途枚举 ↔ 引擎的用途枚举，这条接缝必须是**全的且忠实的**。
    ///
    /// 两侧各有一份 enum 是刻意的（引擎不依赖 vault 的存储实现），代价是编译器只保证
    /// `to_engine_kind` 的 match 覆盖了所有输入，**管不着它映得对不对**：把 PrivateKey
    /// 写成 Password 照样编译通过，而后果是一份私钥被当作口令发给服务器——恰是本项要修的那件事。
    ///
    /// 拿 `aad_tag`/`wire_tag` 当判据，是因为这两个串都是**永不改名**的（vault 侧已被 AEAD
    /// 的 AAD 认证，改名等价于既有密文全部作废）。把「两个枚举对应」这件事化归到「两个稳定
    /// 常量相等」，比逐对写 `assert_eq!(to_engine_kind(A), B)` 强一层：后者只是把同一份映射
    /// 表在测试里抄了第二遍，抄错的方向与实现错的方向一致时它照样绿。
    ///
    /// 遍历 `SecretKind::ALL` 而不是列举三条：将来加第四个变体时，本用例会直接变红，
    /// 而不是安静地只覆盖前三个。
    #[test]
    fn secret_kind_mapping_is_total_and_faithful() {
        for k in fs_vault::SecretKind::ALL {
            let mapped = to_engine_kind(k);
            assert_eq!(
                k.aad_tag(),
                mapped.wire_tag(),
                "vault 的 {k:?} 被映成了引擎的 {mapped:?}，两侧稳定标签不一致——\
                 这条接缝一旦错位，凭据就会被按错误的用途使用"
            );
            // 两侧的解析器也必须认得对方的串，否则「标签相等」只是巧合
            assert_eq!(
                fs_vault::SecretKind::from_aad_tag(mapped.wire_tag()),
                Some(k)
            );
            assert_eq!(
                fs_sshengine::secrets::SecretKind::from_wire_tag(k.aad_tag()),
                Some(mapped)
            );
        }
    }

    /// 审计 P1-22：环境变量名校验。非法键必须被跳过而不是原样发给服务端——
    /// 含 `=`/空格/换行的键在 SSH env 请求里的处理是未定义的。
    #[test]
    fn env_key_validation() {
        for ok in ["PATH", "_x", "LC_ALL", "A1_B2", "_"] {
            assert!(is_valid_env_key(ok), "should accept: {ok}");
        }
        for bad in [
            "", "1ABC",    // 数字开头
            "A B",     // 空格
            "A=B",     // 等号（会被拼成另一个变量）
            "A\nB",    // 换行（注入 SSH 请求的经典载体）
            "A-B",     // 连字符
            "PATH;rm", // 分号
            "中文",    // 非 ASCII
        ] {
            assert!(!is_valid_env_key(bad), "should reject: {bad:?}");
        }
    }

    /// 审计 P1-7：「停止重连」只能取消当前那一轮，不得留下任何影响下一轮的持久状态。
    /// 这里直接钉住令牌表的行为：无轮次在跑时 stop 是 no-op（返回 false），
    /// 而不是像原实现那样把一个会话级标志永久置起。
    #[test]
    fn reconnect_stop_is_noop_without_running_round() {
        let sid = "p1-7-no-round";
        assert!(!cancel_reconnect_round(sid));
        // 登记一轮 → 可取消一次
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let tx = Arc::new(tx);
        RECONNECT_ROUNDS
            .lock()
            .unwrap()
            .insert(sid.to_string(), tx.clone());
        assert!(cancel_reconnect_round(sid));
        assert!(*rx.borrow_and_update());
        // 本轮结束摘除令牌后，再次 stop 又回到 no-op —— 下一轮不受上一轮取消影响
        RECONNECT_ROUNDS.lock().unwrap().remove(sid);
        assert!(!cancel_reconnect_round(sid));
    }

    // ───────────── 审计2 #39：terminal input 规模与等待边界 ─────────────

    /// 正常输入照常解码（回车、UTF-8 中文都是终端日常）。
    #[test]
    fn decode_input_accepts_normal_input() {
        use base64::Engine;
        for raw in ["\r", "echo hi\n", "中文输入"] {
            let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
            assert_eq!(decode_input(&b64).unwrap(), raw.as_bytes(), "{raw:?}");
        }
    }

    /// 恰好上限的输入被接受：上限是「> 上限才拒」。
    #[test]
    fn decode_input_accepts_exact_limit() {
        use base64::Engine;
        let raw = vec![b'x'; MAX_TERM_INPUT_BYTES];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        assert_eq!(decode_input(&b64).unwrap().len(), MAX_TERM_INPUT_BYTES);
    }

    /// 越界输入在**解码之前**被拒：错误文案里带字节上限，且不会发生那次大分配。
    #[test]
    fn decode_input_rejects_oversized_before_decode() {
        let b64 = "A".repeat(MAX_TERM_INPUT_BASE64 + 1);
        let err = decode_input(&b64).unwrap_err();
        assert!(err.contains("上限"), "{err}");
        assert!(err.contains(&MAX_TERM_INPUT_BYTES.to_string()), "{err}");
    }

    /// b64 预筛是收紧的上界而非精确值（ceil 取整放进去的那点余量）：
    /// 解码后的真实字节数仍要硬验一次——这条用例的输入恰好在两者之间的缝隙里。
    #[test]
    fn decode_input_enforces_byte_limit_after_decode() {
        use base64::Engine;
        // 1048577 = MAX + 1，其 b64 长度恰好等于 MAX_TERM_INPUT_BASE64（见常量注释）
        let raw = vec![b'x'; MAX_TERM_INPUT_BYTES + 1];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        assert_eq!(b64.len(), MAX_TERM_INPUT_BASE64);
        assert!(decode_input(&b64).is_err(), "解码后越界必须被拒");
    }

    /// 非法 base64 原样报错（不因尺寸闸的加入而改变语义）。
    #[test]
    fn decode_input_rejects_invalid_base64() {
        assert!(decode_input("!!!not-base64!!!").is_err());
    }

    /// 审计2 #39 的等待边界是「接线」而非可单元测的行为（锁竞争要真实会话才起得来），
    /// 故以**源码位置守卫**钉住：term_input 与 term_resize 各持写锁等待预算、
    /// term_input 的 data_bytes 持写入等待预算。三处 timeout 接线一旦被删，
    /// 本用例直接变红——这类「无声回归」正是单元测试照不到的那块。
    #[test]
    fn term_input_and_resize_have_timeout_wiring() {
        // `file!()` 在 workspace 构建下是相对 workspace 根的路径，而测试进程的 cwd
        // 是包目录——两者拼不拢。用 MANIFEST_DIR + 固定相对路径，与 cwd 解耦。
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/session_cmd.rs"),
        )
        .expect("读不到本文件");
        // 针与源码行**整行相等**才计数：断言行自身就写有针的文字，子串匹配会把自己数进去。
        let line_eq = |needle: &str| src.lines().filter(|l| l.trim() == needle).count();
        assert_eq!(
            line_eq("tokio::time::timeout(DATA_BYTES_TIMEOUT, guard.data_bytes(bytes))"),
            1,
            "term_input 的 data_bytes 必须带等待预算"
        );
        // 两处写锁等待：term_input 一处、term_resize 一处（同一缺陷类别的同源修复）
        assert_eq!(
            line_eq("let guard = tokio::time::timeout(WRITE_LOCK_TIMEOUT, session.write.lock())"),
            2,
            "term_input 与 term_resize 都必须带写锁等待预算"
        );
    }

    /// 审计2 #35 源守卫：会话管道装配必须消费档案 `term.scrollback_lines`，不得退回
    /// 固定默认值（旧实现恒传 DEFAULT_SCROLLBACK_LINES，档案设置从不生效——死配置）。
    /// 同上方 #39 守卫：整行相等计数，防断言自计。
    #[test]
    fn establish_session_uses_profile_scrollback() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/session_cmd.rs"),
        )
        .expect("读不到本文件");
        let line_eq = |needle: &str| src.lines().filter(|l| l.trim() == needle).count();
        // 字段写入恰一处（PipeOpts 装配），且缺省回退与装配同行
        assert_eq!(
            line_eq("scrollback_lines: profile"),
            1,
            "scrollback 必须由档案 term.scrollback_lines 驱动，且只在装配处写入"
        );
        assert_eq!(
            line_eq(".unwrap_or(fs_terminal::grid::DEFAULT_SCROLLBACK_LINES),"),
            1,
            "缺省回退必须与装配同行（unwrap_or 默认值）"
        );
        // 固定默认值不得再出现在装配路径上（PipeOpts 构造块内）。旧实现
        // `crate::grid::DEFAULT_SCROLLBACK_LINES` 裸传即此变异——直接匹配该文本行
        // 会被本测试自身命中，故以拆字构造针（自计陷阱，同 sftp_cmd 守卫）。
        let bare = format!("crate::grid::DEFAULT{}", "_SCROLLBACK_LINES,");
        assert_eq!(
            line_eq(&bare),
            0,
            "装配处不得再出现裸固定默认值（档案值必须生效）"
        );
    }

    /// 审计2 交叉对抗补充：PTY 必须带标准终端模式，不能是空 modes。
    ///
    /// 空 modes 让远端 PTY 全靠 sshd 默认——回声/规范输入/信号位不确定时，`sudo` 读密码、
    /// readline 行编辑这类交互程序可能「输入没反应/卡住」。守卫钉两件事：模式表非空且含
    /// 关键位（ECHO/ICANON/ISIG/VINTR），且装配处引用的是这张表而不是 `&[]`。
    #[test]
    fn pty_request_carries_standard_terminal_modes() {
        for (mode, _) in STANDARD_PTY_MODES {
            let _ = mode; // 模式项非空由下面的关键位断言保证；这里只做遍历不 panic
        }
        let has = |m: russh::Pty| STANDARD_PTY_MODES.iter().any(|(x, _)| *x == m);
        assert!(has(russh::Pty::ECHO), "缺失 ECHO 位");
        assert!(has(russh::Pty::ICANON), "缺失 ICANON 位（规范输入）");
        assert!(has(russh::Pty::ISIG), "缺失 ISIG 位（信号）");
        assert!(has(russh::Pty::VINTR), "缺失 VINTR 位（中断字符）");
        assert!(!STANDARD_PTY_MODES.is_empty(), "模式表为空");

        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/session_cmd.rs"),
        )
        .expect("读不到本文件");
        let line_eq = |needle: &str| src.lines().filter(|l| l.trim() == needle).count();
        assert_eq!(
            line_eq(".request_pty(false, term, cols, rows, 0, 0, STANDARD_PTY_MODES)"),
            1,
            "request_pty 必须引用 STANDARD_PTY_MODES，不得退回空 modes"
        );
    }
    /// 指数退避的序列（M1 出口原文「断线自动重连（**指数退避**）」，2026-08-23 补）。
    ///
    /// 此前这条规则是循环体里两处重复的内联表达式，零测试——出口点名要「指数退避」，
    /// 而能证明的只有「有个 sleep」。
    #[test]
    fn reconnect_delay_doubles_then_caps_at_thirty_seconds() {
        use std::time::Duration;
        // 从 1s 起翻倍：1 → 2 → 4 → 8 → 16 → 30（封顶，不是 32）
        let mut d = Duration::from_secs(1);
        let mut seq = vec![d.as_secs()];
        for _ in 0..8 {
            d = super::next_reconnect_delay(d);
            seq.push(d.as_secs());
        }
        assert_eq!(
            seq,
            vec![1, 2, 4, 8, 16, 30, 30, 30, 30],
            "退避须翻倍并在 30s 封顶（第 6 档是 30 而非 32——封顶发生在越界那一次，不是之后）"
        );
    }

    #[test]
    fn reconnect_delay_never_exceeds_the_cap_from_any_start() {
        use std::time::Duration;
        // 从任意起点（含已超封顶的畸形值）出发，一步之内必须落回封顶之内
        for start in [0u64, 1, 17, 29, 30, 31, 100, 3600] {
            let next = super::next_reconnect_delay(Duration::from_secs(start));
            assert!(
                next <= super::RECONNECT_DELAY_CAP,
                "起点 {start}s 的下一档是 {}s，超过了封顶 {}s",
                next.as_secs(),
                super::RECONNECT_DELAY_CAP.as_secs()
            );
        }
        // 0 是退化输入：翻倍仍是 0，不该变成封顶值（那会让「立刻重试」变成「等 30 秒」）
        assert_eq!(
            super::next_reconnect_delay(Duration::ZERO),
            Duration::ZERO,
            "0 翻倍应仍为 0"
        );
    }
}
