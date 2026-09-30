mod agent;
mod ai_mode;
mod commands;
mod connect_failure;
mod events;
mod ipc_log;
mod journal;
mod logfile;
mod mcp;
mod rdp;
mod rdp_share;
mod scheduler;
mod serial_session;
mod sessions;
mod state;
mod update_check;
mod window_layout;
mod zmodem_bridge;

use state::AppState;
use std::sync::Arc;
use tauri::Manager; // S247：App::manage 经 Manager trait 提供，不导入则 E0599（Emitter 随 coldstart_ready 一并去掉：本文件不再 emit）
use tokio::sync::Mutex;

/// MCP 桥进程要读数据目录下的 `mcp.endpoint`（见 main.rs 的 `--mcp-bridge`）。
/// `state` 模块对 bin 不公开（内部结构），经这条 pub 函数出边界。
pub fn bridge_data_dir() -> Option<std::path::PathBuf> {
    state::data_dir_or_err().ok()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    run_with_smoke_exit(None)
}

/// 安装验证入口：只有事件循环就绪之后才开始计时并正常退出。
pub fn run_with_smoke_exit(smoke_exit_ms: Option<u64>) {
    // 审计 P1-21：启动链路上原先四处 `.expect()`（数据目录、日志目录、建库、tauri run）——
    // 任何一处失败，用户看到的是一个双击后毫无反应、连窗口都不出现的程序，panic 信息写在
    // 一个没人会去看的 stderr 上（GUI 进程通常压根没有终端）。
    // 这类失败偏偏都是**用户可自行处理**的（磁盘满、目录权限、库文件损坏、被安全软件拦住），
    // 只要说清楚是什么就能解决；沉默退出把一个可修的问题变成了「这软件坏了」。
    // 故统一收敛成 Result，失败时留下人能看懂的中文说明 + 落一份错误文件，再以非零码退出。
    // 先解析一次数据目录：report_fatal 需要知道错误文件该写去哪（见其说明）。
    // 解析失败本身没关系——那只是「首选位置不可用」，错误文件会回落临时目录。
    let data_dir = state::data_dir_or_err().ok();
    // panic 取证（M4a.1 T93）必须装在 run_inner **之前**：run_inner 里任何一处 panic
    // 都要留下现场，包括它自己第一行。顺序由 `the_panic_hook_is_installed_before_run_inner`
    // 断言钉住（口径同下方单实例锁的源码顺序测试）。
    install_panic_hook(data_dir.clone());
    if let Err(e) = run_inner(smoke_exit_ms) {
        report_fatal(&e, data_dir.as_deref());
        std::process::exit(1);
    }
}

/// panic 现场落盘（M4a.1 T93）。全仓此前 `set_hook` 零处——GUI 进程通常没有终端，
/// 默认 hook 把 panic 写进一个没人看得到的 stderr，用户视角就是「窗口突然没了」。
///
/// ## 为什么**不**经 tracing
///
/// 日志走 `tracing_appender` 的非阻塞写入器，其 `WorkerGuard` 被刻意塞进进程级
/// `OnceLock`（见本文件上方注记：「Rust 从不为 static 跑析构」）。那个设计对「日志要写到
/// 退出那一刻」是对的，但它同时意味着 **panic 后没有任何 flush 时机**：非阻塞写入器把
/// 记录交给后台线程，进程若随即倒下，最后那几条正是丢的那几条。
///
/// 故 crash 文件走**同步直写**，与日志子系统完全无关，复用 [`write_error_file`]
/// （`create_new` + Unix 0600 + 软链处理，与启动失败文件同一把闸）。
/// 「futureshell.log 里也留一行」**显式不覆盖**：异步写入器在 panic 后不保证落盘，
/// 与其给一个时有时无的保证，不如只承诺 crash 文件。
///
/// ## 四要素
///
/// 版本、线程名、panic 位置、backtrace。缺任何一个都会让第一份用户报告变成来回追问：
/// 没版本不知道是哪个构建，没线程名分不清是 UI 还是某条会话泵，没位置只能猜，
/// 没 backtrace 则连调用链都没有。
fn install_panic_hook(data_dir: Option<std::path::PathBuf>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 先落盘再交回默认 hook：默认 hook 在某些配置下会直接 abort，那之后的代码不再执行。
        let path = crash_file_path(data_dir.as_deref());
        let body = crash_report(info);
        // 失败也只能往 stderr 说一句——这里已经在 panic 路径上，不能再 panic。
        match write_error_file(&path, &body) {
            Ok(()) => eprintln!("panic 现场已写入：{}", path.display()),
            Err(e) => eprintln!("（panic 现场写入失败：{e}）\n{body}"),
        }
        previous(info);
    }));
}

/// crash 文件落点：数据目录优先（已收紧 0700），解析不出来才回落临时目录。
/// 名字带 pid + 纳秒：同一次运行可能有多个线程各自 panic，固定名会互相覆盖；
/// 共享的临时目录里固定名还是个现成的软链靶子（口径同 [`fatal_error_path`]）。
fn crash_file_path(data_dir: Option<&std::path::Path>) -> std::path::PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!("crash-{}-{stamp}.log", std::process::id());
    match data_dir {
        Some(d) => d.join(name),
        None => std::env::temp_dir().join(format!("future-shell-{name}")),
    }
}

/// 组装 panic 现场文本（四要素）。抽成纯函数：内容可被单测逐项断言，
/// 而 `set_hook` 里的闭包本身不好测。
fn crash_report(info: &std::panic::PanicHookInfo<'_>) -> String {
    // panic 载荷：&str 与 String 两种最常见，其余类型只能报「非字符串载荷」
    let msg = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "（panic 载荷不是字符串）".to_string());
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "（无位置信息）".to_string());
    let thread = std::thread::current();
    let thread_name = thread.name().unwrap_or("(未命名)").to_string();
    // Backtrace::force_capture 不看 RUST_BACKTRACE：用户不会为了报 bug 先设环境变量再重现一次。
    let backtrace = std::backtrace::Backtrace::force_capture();
    format!(
        "Future Shell 崩溃现场\n\n\
         版本：{version}\n\
         线程：{thread_name}\n\
         位置：{location}\n\
         原因：{msg}\n\n\
         调用栈：\n{backtrace}\n\n\
         这份文件只含程序自身的崩溃现场（版本/线程/位置/调用栈），不含会话内容与凭据。\n\
         报告问题时请连同它一起提供。\n",
        version = env!("CARGO_PKG_VERSION"),
    )
}

fn run_inner(smoke_exit_ms: Option<u64>) -> Result<(), String> {
    // data_dir_or_err：解析不出用户配置目录即失败，**不再回落 cwd**（P1-21 的另一半，见 state.rs）
    let dir = state::data_dir_or_err()?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("无法创建数据目录 {}：{e}", dir.display()))?;
    // 审计2 #31：数据目录里是库、日志与凭据密文，Unix 上收紧到 0700——多数发行版的
    // 默认 umask 给出 0755，同机其他账户能进目录读文件名、拷日志。已存在（上一版本
    // 建出来的正是最常见情形）也照样收紧一次。Windows 上权限位由 ACL 决定，跳过。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("无法收紧数据目录权限 {}：{e}", dir.display()))?;
    }
    // 单实例闸门（审计2 #12）。位置是这条修复的一半：必须排在**所有**其他启动动作之前。
    //
    // 排在日志之前，是因为日志写入器是「单文件自轮转」（logfile.rs）：两个进程各开一份
    // 写句柄，轮到轮转时各自 set_len(0) 截断**同一个**文件，互相削掉对方的日志。
    // 排在建库、建窗口之前，是因为第二个实例一旦走到那里，它就已经在动同一个 `fs.db`、
    // 也已经能跑传输了：而传输引擎的 per-target 锁是**进程内** static（审计2 #12 的锚点
    // `transfer.rs:128`），两个进程于是能同时往同一个 `.fspart` 里写。
    //
    // 这把锁本就存在（`fs_vault` 的 `vault.lock`），此前只在保险库解锁时惰性抢占；
    // 详见 `fs_vault::lock_data_dir`。行为变更须写进发布说明：同一数据目录**只能**开一个
    // 实例，第二次启动会带着说明退出，而不是像以前那样先跑起来、等用户输完主口令再报
    // 一句与真实原因无关的「解锁失败」。
    //
    // 未做（如实记下）：没有把第二个实例的启动请求转交给已在运行的那个（「聚焦已有窗口」
    // 那种体验）。那需要 `tauri-plugin-single-instance`，而供应链卫生本身正是审计2 #3/#4
    // 的未决条目，不在一次并发安全修复里引新依赖。
    fs_vault::lock_data_dir(&dir).map_err(|e| match e {
        fs_vault::Error::Storage(m) => m,
        other => other.to_string(),
    })?;
    let log_dir = dir.join("logs");
    std::fs::create_dir_all(&log_dir)
        .map_err(|e| format!("无法创建日志目录 {}：{e}", log_dir.display()))?;
    // 审计2 #31：日志目录与数据目录同款收紧到 0700。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&log_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("无法收紧日志目录权限 {}：{e}", log_dir.display()))?;
    }
    // `tauri_runtime_wry` / `wry` 提到 warn：WebView2 的 `ExecuteScript` 失败此前
    // 完全不可观测——`app.emit` 走的正是这条路，投递失败在日志里一个字都没有。
    // 2026-08-19 的渲染帧丢失查不出出口，这是其中一半原因（另一半是 emit 的 Err
    // 被 `let _ =` 吞掉，已在 sessions.rs 修）。
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        "future_shell_app=info,fs_=info,tauri_runtime_wry=warn,wry=warn".into()
    });
    // 审计2 #32：换成尺寸上限的自写轮转（logfile.rs）。旧的 rolling 写入器只按时间
    // 分文件，`max_log_files(7)` 限个数限不住大小——常驻进程一天内的日志可以无限长。
    let log_file =
        logfile::SizeCappedFile::open(&log_dir.join("futureshell.log"), logfile::DEFAULT_CAP)
            .map_err(|e| format!("无法初始化日志文件（目录 {}）：{e}", log_dir.display()))?;
    let (log_writer, guard) =
        tracing_appender::non_blocking::NonBlockingBuilder::default().finish(log_file);
    // guard 决定日志工作线程的生死：丢进进程级 static——Rust 从不为 static 跑析构，
    // 线程随进程存亡，正是「日志要写到退出那一刻」的语义（与此文件下方 VaultGuard
    // 的注记同一口径）。
    static LOG_WORKER_GUARD: std::sync::OnceLock<tracing_appender::non_blocking::WorkerGuard> =
        std::sync::OnceLock::new();
    let _ = LOG_WORKER_GUARD.set(guard);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(log_writer)
        .try_init()
        .map_err(|e| format!("无法初始化日志子系统：{e}"))?;

    // MCP 运行时状态：setup（注册 emitter + manage）与启动任务（按设置起监听）
    // 都要用，故在 Builder 之前建。
    let mcp_runtime = std::sync::Arc::new(crate::mcp::serve::McpRuntime::default());

    let setup_dir = dir.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            // 同步建库 + manage(State) 后再返回 Ok(())：消除前端首帧竞态——
            // 原 spawn 异步建库会使窗口首帧的 profiles_list/settings_get 在 State 注入前到达而失败（spec §2.4 首屏会话列表）。
            let dir = setup_dir;
            let db_path = dir.join("fs.db");
            // 建库失败经 setup 的 Result 上抛 → Builder::run 返回 Err → run_inner 统一收口（P1-21）。
            // 原 `.expect("open db")` 在这里 panic，用户既看不到窗口也看不到原因。
            let db = tauri::async_runtime::block_on(async move {
                fs_connmgr::Db::open(&db_path)
                    .await
                    .map_err(|e| format!("无法打开数据库 {}：{e}", db_path.display()))
            })?;
            let state = Arc::new(AppState {
                data_dir: dir,
                db: Arc::new(db),
                vault: Arc::new(Mutex::new(None)),
                app: app.handle().clone(), // Task 19：事件桥/watchdog/重连循环 emit 用
                registry: Arc::new(sessions::SessionRegistry::default()),
                serial: Arc::new(sessions::SessionRegistry::default()),
                pending: Arc::new(events::PendingPrompts::default()),
                sftp_ops: Arc::new(Mutex::new(std::collections::HashMap::new())),
                transfers: Arc::new(Mutex::new(std::collections::HashMap::new())),
                verify_plans: Arc::new(Mutex::new(std::collections::HashMap::new())),
                frontend_error_seen: Arc::new(std::sync::Mutex::new(
                    std::collections::HashMap::new(),
                )),
                cpu_prev: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
                zmodem: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
                edits: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
                recorders: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
                tunnels: Arc::new(Mutex::new(std::collections::HashMap::new())),
                host_facts: Arc::new(Mutex::new(std::collections::HashMap::new())),
                agents: Arc::new(crate::agent::confirm_port::AgentSupervisor::default()),
                mcp_confirms: Arc::new(mcp::McpConfirms::default()),
            });
            // 审计 P1-16：Vault 闲置自动锁定巡检——`vault.autoLockMinutes` 此前是「写而不读」的
            // 死设置，用户选了 5 分钟却永不锁定。守护任务随进程存活，设置每轮重读、改完即生效。
            commands::vault_cmd::spawn_auto_lock_watcher(state.clone());
            // M3 出口 7 ③：审计链的周期性快照。按**行数**触发（表不写就没什么可快照的），
            // 放在后台任务里——它内部要跑一次全表校验，不该拖住首帧。
            //
            // 只有这条自动路径落快照，用户点的校验不落：见 `audit_cmd` 模块头
            // 「校验本身不得留痕」。自动快照泄露的只是「程序跑过、表长了」。
            let cp_state = state.clone();
            tauri::async_runtime::spawn(async move {
                commands::audit_cmd::checkpoint_on_startup(cp_state.db.pool()).await;
            });
            // M4a 计划任务：每 2 秒一拍的调度循环（判定纯函数在 fs_connmgr::schedule）。
            // 与自锁巡检同款：随进程存活、无停止入口。放在 manage 之前是为了让它与
            // 巡检共享同一个 Arc，而不是从 app.state() 再取一次（那要求 manage 已完成）。
            scheduler::spawn(state.clone());
            // Vault 静默解锁（2026-08-31）：没设应用口令的库主密钥就在系统 keyring 里，
            // 启动时顺手打开，用户不必每次去点那个对他毫无意义的「应用密码（留空）」框。
            // 设过口令的库**不碰**——用户要的是「有一道锁」。失败一律静默回落手动路径。
            let vault_state = state.clone();
            tauri::async_runtime::spawn(async move {
                match commands::vault_cmd::auto_unlock_inner(&vault_state).await {
                    Ok(true) => {}
                    Ok(false) => {}
                    Err(e) => tracing::info!("Vault 静默解锁跳过：{e}"),
                }
            });
            // MCP 运行时（2026-08-28 套接字形态）：emitter 在拿到 AppHandle 的
            // 最早时机塞进 runtime——状态事件从 accept 循环发，那里只有 AppState。
            let _ = mcp_runtime.emitter.set(app.handle().clone());
            let (mcp_shutdown, _mcp_rx) = tokio::sync::watch::channel(false);
            let mcp_shutdown_clone = mcp_shutdown.clone();
            app.manage(crate::commands::mcp_cmd::McpGlobal {
                runtime: mcp_runtime.clone(),
                shutdown: mcp_shutdown,
            });
            // M3：MCP 对外服务（总设计 §4.5，**默认关闭**）。2026-08-28 起为
            // localhost 套接字 + `--mcp-bridge` 形态（原 stdio 形态在 GUI 程序
            // 上被单实例闸死锁，详见 mcp/serve.rs 模块头）。启动时读一次
            // `mcp.enabled`：开着就起监听；设置页的开关经 `mcp_set_enabled`
            // 即时起停。监听挂了只记日志、不 panic（不拖垮 GUI）。
            let mcp_state = state.clone();
            let mcp_runtime = mcp_runtime.clone();
            tauri::async_runtime::spawn(async move {
                if mcp::is_enabled(&mcp_state).await {
                    let global = crate::commands::mcp_cmd::McpGlobal {
                        runtime: mcp_runtime,
                        shutdown: mcp_shutdown_clone,
                    };
                    // 启动路径没有 AppHandle 可取（manage 还没跑），借 manage 时机
                    // 注册的 global 复刻一份只为了调 set_enabled——直接内联字段。
                    if let Err(e) = mcp::serve::set_enabled(
                        mcp_state.clone(),
                        global.runtime.clone(),
                        true,
                        &global.shutdown,
                    )
                    .await
                    {
                        tracing::error!("MCP 监听启动失败：{e}");
                    }
                }
            });
            app.manage(state);
            // RDP 全局状态（会话表 + 待答证书裁决）。与 AppState 分开 manage：
            // RdpGlobal 不需要数据库句柄，且 commands 层从 AppHandle 就能取到
            // （rdp_cmd 的命令签名只要 app 不要 State，同款简化）。
            app.manage(crate::rdp::RdpGlobal(Default::default()));
            // 此处原有一行 `app.emit("coldstart_ready", ())`，注释写着「前端可监听此事件确认后端已
            // 完成初始化」。全仓零监听者（审计：事件契约门禁）。删除而非补一个监听者，理由是它**无法
            // 被安全消费**：`setup()` 跑在 webview 注册任何 listener 之前，事件发出去时前端还没有
            // 收件人，而 Tauri 的事件不做重放。真去写 `await listen("coldstart_ready")` 再往下走的
            // 前端，等到的是永远不来的一条事件——比没有信号更坏。
            // 需要「后端已就绪」语义的地方现成就有：任何 `invoke` 在 `app.manage` 之前都会失败，
            // 前端的 `ping()` 即是这个握手。冷启动计时另见 crates/itest/tests/perf.rs 的口径说明。
            // Task 20 Step 3：性能测试桩——若环境变量 FS_PERF_TEST=1，向 stderr 输出就绪标记供外部计时脚本捕获
            if std::env::var("FS_PERF_TEST").is_ok() {
                eprintln!("__FS_COLDSTART_READY__");
            }
            Ok(())
        })
        .invoke_handler(ipc_log::log_invoke(tauri::generate_handler![
            commands::ping,
            commands::update_cmd::update_check,
            commands::update_cmd::app_version,
            commands::log_frontend_error,
            commands::clipboard_cmd::clipboard_write,
            commands::clipboard_cmd::clipboard_read,
            commands::opener_cmd::open_external,
            commands::opener_cmd::reveal_log_dir,
            commands::opener_cmd::reveal_session_log_dir,
            commands::opener_cmd::reveal_recordings_dir,
            // vault_status / profiles_list 为 Task 17 既有注册，本 Task 仅替换 stub 实现、不重复注册（O14/O27）
            commands::vault_cmd::vault_status,
            commands::vault_cmd::vault_init,
            commands::vault_cmd::vault_unlock,
            commands::vault_cmd::vault_has_file,
            commands::vault_cmd::vault_put_secret,
            commands::vault_cmd::vault_list_secrets,
            commands::vault_cmd::vault_lock,
            commands::vault_cmd::vault_copy_to_clipboard,
            // 审计2 #27/#28：删除、改密/轮换、备份与恢复。这四件事的 core 实现有的早就在
            // （`Store::delete` / `change_passphrase`），只是从未接到 IPC 上——没有注册就等于没有功能。
            commands::vault_cmd::vault_delete_secret,
            commands::vault_cmd::vault_has_passphrase,
            commands::vault_cmd::vault_auto_unlock,
            commands::vault_cmd::vault_file_has_passphrase,
            commands::vault_cmd::vault_change_passphrase,
            commands::vault_cmd::vault_backup_dir,
            commands::vault_cmd::vault_create_backup,
            commands::vault_cmd::vault_list_backups,
            commands::vault_cmd::vault_restore_backup,
            commands::conn_cmd::profiles_list,
            commands::conn_cmd::profile_save,
            commands::conn_cmd::profile_store_password,
            commands::conn_cmd::profile_delete,
            commands::conn_cmd::profiles_export,
            commands::conn_cmd::profiles_import,
            commands::window_cmd::window_new,
            commands::window_cmd::window_arrange,
            commands::window_cmd::window_close_view,
            commands::sftp_cmd::local_mkdir,
            commands::sftp_cmd::local_rename,
            commands::agent_cmd::agent_start,
            commands::agent_cmd::agent_abort,
            commands::agent_cmd::agent_confirm_answer,
            commands::agent_cmd::agent_ask_reply,
            commands::agent_cmd::agent_is_running,
            commands::mcp_cmd::mcp_confirm_answer,
            commands::audit_exec_cmd::audit_command_sent,
            commands::audit_exec_cmd::audit_dangerous_action,
            commands::audit_cmd::audit_verify,
            commands::audit_cmd::audit_verify_quick,
            commands::audit_cmd::audit_export,
            commands::audit_cmd::audit_verify_bundle,
            commands::ai_cmd::ai_status,
            commands::ai_cmd::ai_suggest_command,
            commands::ai_cmd::ai_explain,
            commands::ai_cmd::ai_provider_save,
            commands::ai_cmd::ai_provider_delete,
            commands::key_cmd::key_list,
            commands::import_cmd::foreign_import_preview,
            commands::import_cmd::foreign_import_commit,
            commands::conn_cmd::hostkey_import,
            commands::conn_cmd::hostkey_known_keys,
            commands::settings_cmd::settings_get,
            commands::settings_cmd::settings_set,
            commands::session_cmd::session_open,
            commands::session_cmd::session_close,
            commands::session_cmd::session_reconnect,
            commands::session_cmd::session_reconnect_stop,
            commands::session_cmd::sessions_unclosed,
            commands::session_cmd::sessions_discard_unclosed,
            commands::session_cmd::session_close_all,
            commands::session_cmd::term_input,
            commands::session_cmd::term_resize,
            commands::session_cmd::term_ack,
            commands::session_cmd::term_encoding,
            commands::session_cmd::term_set_encoding,
            commands::session_cmd::term_encodings,
            commands::rdp_cmd::rdp_share_mount,
            commands::rdp_cmd::rdp_share_unmount,
            commands::rdp_cmd::rdp_share_status,
            commands::serial_cmd::serial_list_ports,
            commands::serial_cmd::serial_common_bauds,
            commands::serial_cmd::serial_open,
            commands::serial_cmd::serial_set_baud,
            commands::serial_cmd::serial_params,
            commands::monitor_cmd::session_monitor,
            // M7.1 系统面工具箱：服务管理（列表/状态/日志/启停重启）+ 补丁只读盘点。
            // 只有 session_service_action 改远端状态，它自己写审计。
            commands::services_cmd::session_services,
            commands::services_cmd::session_service_status,
            commands::services_cmd::session_service_journal,
            commands::services_cmd::session_service_action,
            commands::services_cmd::session_packages,
            // M7.3 文件管理器桌面化：远端剪切/复制/粘贴 + 脚本原文预览与后台运行。
            commands::fileops_cmd::fileops_plan,
            commands::fileops_cmd::fileops_paste,
            commands::fileops_cmd::sftp_read_text,
            commands::fileops_cmd::fileops_run_script_background,
            commands::fileops_cmd::fileops_foreground_command,
            commands::monitor_cmd::host_probe,
            commands::monitor_cmd::session_connection_detail,
            commands::monitor_cmd::session_processes,
            commands::monitor_cmd::session_kill_process,
            commands::history_cmd::history_record,
            commands::history_cmd::history_search,
            commands::history_cmd::history_scan_grid,
            commands::history_cmd::history_delete,
            commands::history_cmd::history_clear,
            commands::schedule_cmd::schedule_preview_cron,
            commands::schedule_cmd::schedule_list,
            commands::schedule_cmd::schedule_create,
            commands::schedule_cmd::schedule_update,
            commands::schedule_cmd::schedule_set_enabled,
            commands::schedule_cmd::schedule_delete,
            commands::schedule_cmd::schedule_runs,
            commands::schedule_cmd::schedule_run_now,
            commands::record_cmd::recording_start,
            commands::record_cmd::recording_stop,
            commands::record_cmd::recording_status,
            commands::record_cmd::recordings_list,
            commands::record_cmd::recording_read_events,
            commands::tunnel_cmd::tunnel_start,
            commands::tunnel_cmd::tunnel_stop,
            commands::tunnel_cmd::tunnel_list,
            commands::rdp_cmd::rdp_connect,
            commands::rdp_cmd::rdp_cert_verdict,
            commands::rdp_cmd::rdp_input,
            commands::rdp_cmd::rdp_close,
            commands::rdp_cmd::rdp_resize,
            commands::rdp_cmd::rdp_clipboard_sync,
            commands::rdp_cmd::rdp_frame_ack,
            commands::rdp_cmd::rdp_request_full_frame,
            commands::credential_cmd::credentials_shared_list,
            commands::credential_cmd::credential_share,
            commands::credential_cmd::credential_unshare,
            commands::credential_cmd::credential_is_shared,
            commands::conn_cmd::profile_set_own_password,
            commands::mcp_cmd::mcp_set_enabled,
            commands::mcp_cmd::mcp_status,
            commands::mcp_cmd::mcp_token_get,
            commands::mcp_cmd::mcp_token_regen,
            commands::mcp_cmd::mcp_connection_info,
            commands::rdp_cmd::rdp_reconnect,
            commands::zmodem_cmd::zmodem_status,
            commands::zmodem_cmd::zmodem_send,
            commands::zmodem_cmd::zmodem_finalize,
            commands::zmodem_cmd::zmodem_cancel,
            commands::zmodem_cmd::zmodem_reveal,
            commands::auth_cmd::auth_respond,
            commands::auth_cmd::hostkey_decide,
            commands::sftp_cmd::sftp_list,
            commands::sftp_cmd::sftp_mkdir,
            commands::sftp_cmd::sftp_remove,
            commands::sftp_cmd::sftp_rename,
            commands::sftp_cmd::local_list,
            commands::sftp_cmd::sftp_stat_entry,
            commands::sftp_cmd::sftp_symlink,
            commands::sftp_cmd::local_stat_entry,
            commands::sftp_cmd::sftp_walk,
            commands::sftp_cmd::local_walk,
            commands::editor_cmd::editor_open,
            commands::editor_cmd::editor_check,
            commands::editor_cmd::editor_upload,
            commands::editor_cmd::editor_close,
            commands::editor_cmd::editor_list,
            commands::sftp_cmd::transfer_submit,
            commands::sftp_cmd::transfer_cancel,
        ]))
        // `build` + 手写事件循环，而不是 `.run(ctx)`：后者不给任何观察 `RunEvent` 的机会，
        // 而保险库的跨进程锁必须在退出时显式排空（见下方 `RunEvent::Exit`）。
        .build(tauri::generate_context!())
        .map_err(|e| format!("Tauri 运行时启动失败：{e}"))?
        .run(move |app, event| {
            if let tauri::RunEvent::Ready = event {
                if let Some(ms) = smoke_exit_ms {
                    let handle = app.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(ms));
                        handle.exit(0);
                    });
                }
            }
            if let tauri::RunEvent::Exit = event {
                // 保险库锁的守卫存活在一张与进程等长的 static 注册表里，强引用计数永不归零；
                // 即便归零，Rust 也从不为 static 跑析构 —— 换句话说 `VaultGuard::drop`
                // 在没有这一行的情况下是**一行谁也走不到的死代码**。
                // 它一死，Unix 侧就只剩「下次启动做陈旧锁检测」这一条回收路径，
                // 而那条路径此前在非 Linux 上恒判「持有者还活着」：用户正常退出一次，
                // 之后每一次启动都会被上一次留下的 vault.lock 挡在门外，
                // 且错误还被前端包进「解锁失败（密码错误或 keyring/文件异常）」——
                // 用户只会怀疑自己记错了密码，不会想到去隐藏目录里删一个锁文件。
                fs_vault::release_process_locks();
                // 强退：窗口关掉后，SSH 连接、自动锁定巡检、日志工作线程等后台任务可能仍把
                // tokio runtime 托着不让进程自然退出——表现为「窗口没了，future-shell-app.exe
                // 还在任务管理器里、可执行文件被占住」。Exit 事件是事件循环的收尾信号，
                // 此刻锁已排空、该落盘的都已尽力，直接以 0 退出，不陪后台任务耗着。
                std::process::exit(0);
            }
        });
    Ok(())
}

/// 启动期致命错误的落地（审计 P1-21 + 审计2 #33）。
///
/// 依赖里既没有 `tauri-plugin-dialog` 也没有 `rfd`，而 `Cargo.toml` 属并行改动方——加依赖会冲突。
/// 故取「不引入任何依赖也一定能送达」的两条通道：
/// ① stderr（从终端启动、或被 CI/脚本拉起时立刻可见）；
/// ② 一份错误文件（GUI 双击启动没有终端，这是用户与技术支持唯一能拿到的现场）。
///
/// 审计2 #33：错误文件不再写「共享临时目录 + 固定文件名」。固定名在共享临时目录里
/// 既会被别的实例互踩（后写的覆盖先写的，用户拿到的不是自己的现场），又是一个现成
/// 的符号链接靶子：预先种一个同名软链，错误报告就成了「往任意文件写内容」。现在：
/// 首选数据目录（用户专属、上面已收紧 0700；文件名固定不带时间戳——出问题的用户往往
/// 会连试几次，固定名字保证他打开的永远是最新一次，也不会堆出一串垃圾）；连数据目录
/// 都解析不出来（失败点恰恰就在那里）才回落临时目录，此时用 pid 后缀 + `create_new`
/// 保底：软链最多让这一次写不进去，stderr 上的全文不受影响。
///
/// 若前端加了 dialog 依赖，这里应升级为弹窗（见最终回复的「需要别处配合」）。
fn report_fatal(msg: &str, data_dir: Option<&std::path::Path>) {
    let body = format!(
        "Future Shell 启动失败\n\n原因：{msg}\n\n\
         常见处理：\n\
         - 已经有一个 Future Shell 在运行：同一数据目录同一时刻只允许一个实例（否则两个进程会\n\
           同时写同一个数据库、同一份日志、乃至同一个传输临时文件），请先关闭另一个窗口；\n\
           若确认没有别的实例在跑，按上面提示里的路径删掉那个 .lock 文件后重试；\n\
         - 磁盘空间不足或目录只读：清理空间，或检查用户配置目录的写权限；\n\
         - 数据库无法打开：多为上次异常退出留下的锁文件或文件损坏，可备份后删除 fs.db 重新启动\n\
           （连接配置会丢失，Vault 中的凭据不受影响）；\n\
         - 安全软件拦截：把程序与其数据目录加入白名单后重试。\n"
    );
    eprintln!("{body}");
    let path = fatal_error_path(data_dir);
    match write_error_file(&path, &body) {
        Ok(()) => eprintln!("详细信息已写入：{}", path.display()),
        Err(e) => eprintln!("（错误文件写入失败：{e}）"),
    }
    // 日志子系统可能尚未初始化（失败点就在它之前），故 tracing 只作补充而非唯一通道。
    tracing::error!(reason = msg, "启动失败，进程即将退出");
}

/// 错误文件的落点（审计2 #33）：数据目录（用户专属、上面已收紧 0700）优先；连数据
/// 目录都解析不出来（失败点恰恰就在那里）才回落临时目录，此时名字带 pid——共享目录
/// 里固定名既会被别的实例互踩，又是一个现成的软链靶子。
fn fatal_error_path(data_dir: Option<&std::path::Path>) -> std::path::PathBuf {
    match data_dir {
        Some(d) => d.join("startup-error.txt"),
        None => std::env::temp_dir().join(format!(
            "future-shell-startup-error-{}.txt",
            std::process::id()
        )),
    }
}

/// `create_new` 先试；已存在（含被种下的软链——`create_new` 不跟随符号链接）则删掉
/// 重试一次。删→建之间仍有一个极窄窗口，最坏是这一次写不进去，绝不会写成
/// 「写进别人指的地方」。
fn write_error_file(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    // 错误文件里是排障现场（路径、原因），与日志同密级：0600（审计2 #31 口径）。
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let write = |mut f: std::fs::File| -> std::io::Result<()> {
        f.write_all(body.as_bytes())?;
        f.sync_data()
    };
    match opts.open(path) {
        Ok(f) => write(f),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // remove_file 删的是链接本身而非其目标；create_new 永不跟进软链。
            std::fs::remove_file(path)?;
            match opts.open(path) {
                Ok(f) => write(f),
                Err(e2) => Err(e2),
            }
        }
        Err(e) => Err(e),
    }
}

// 测试模块置于文件末尾：clippy 的 `items_after_test_module` 会把「测试模块之后还有产品代码」
// 判为错误——那种排布下，新加的函数很容易被误写进 `#[cfg(test)]` 的作用域里而不参与发布构建。
#[cfg(test)]
mod handler_registry_tests {
    //! 「写了命令但忘了注册」是这套 IPC 里**最安静**的一类缺陷：`#[tauri::command]` 照常编译，
    //! `cargo clippy` 一声不吭，前端 `invoke("x")` 在运行时才拿到一句
    //! `Command x not found`——而那句话往往被就近的 `catch` 包成「读取失败」之类的业务文案。
    //!
    //! 本 Task 新增的 `hostkey_known_keys` 正是这条路径上的东西：它是「指纹钉扎」策略唯一的
    //! 添加入口，漏注册就等于这个策略仍然不可达，而所有前端测试（IPC 被 mock 掉）照样全绿。
    //! 故不为单个命令写断言，而是把「定义即注册」本身钉成不变量。
    //!
    //! 从磁盘读源码而不是 `include_str!`：后者无法按目录展开，加一个 `*_cmd.rs` 文件就会被漏掉，
    //! 而漏掉恰恰是本测试要防的那件事。测试运行时 `CARGO_MANIFEST_DIR` 必然指向仓库内的 crate 根。

    use super::{fatal_error_path, write_error_file};

    fn commands_declared() -> Vec<(String, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
        let mut out = Vec::new();
        for e in std::fs::read_dir(&dir).expect("commands 目录必须存在") {
            let path = e.unwrap().path();
            if path.extension().and_then(|s| s.to_str()) != Some("rs") {
                continue;
            }
            let file = path.file_name().unwrap().to_string_lossy().to_string();
            let src = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = src.lines().collect();
            for (i, l) in lines.iter().enumerate() {
                if l.trim() != "#[tauri::command]" {
                    continue;
                }
                // 属性与 fn 之间可能还夹着别的属性行，逐行下探到函数签名为止
                let name = lines[i + 1..]
                    .iter()
                    .find_map(|s| s.split_once("fn "))
                    .map(|(_, rest)| {
                        rest.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                            .next()
                            .unwrap_or("")
                            .to_string()
                    })
                    .expect("#[tauri::command] 之后必须跟一个 fn");
                assert!(!name.is_empty(), "{file}:{} 解析不出命令名", i + 1);
                out.push((file.clone(), name));
            }
        }
        assert!(
            out.len() > 30,
            "只扫到 {} 条命令，扫描逻辑多半失效了",
            out.len()
        );
        out
    }

    /// 每个 `#[tauri::command]` 都必须出现在 `invoke_handler` 的 `generate_handler!` 里。
    #[test]
    fn every_command_is_registered_in_invoke_handler() {
        let lib = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
        )
        .unwrap();
        let block = lib
            .split_once("generate_handler![")
            .expect("找不到 generate_handler!")
            .1
            .split_once("])")
            .expect("generate_handler! 未闭合")
            .0;
        let missing: Vec<String> = commands_declared()
            .into_iter()
            // 以 `::name,` 匹配，避免 `hostkey_import` 命中 `hostkey_import_xxx` 之类的前缀重叠
            .filter(|(_, n)| !block.contains(&format!("::{n},")))
            .map(|(f, n)| format!("{f}::{n}"))
            .collect();
        assert!(
            missing.is_empty(),
            "以下命令已定义但未注册，前端 invoke 会在运行时报 `Command not found`：{missing:?}"
        );
    }

    // ── 单实例闸门的**位置**（审计2 #12）──────────────────────────────────────
    //
    // `run_inner` 是这套代码里少数几乎无法在单测里执行的函数（它以 `tauri::Builder::run` 收尾，
    // 那是个要真开窗口的阻塞调用），而这条修复的正确性有一半在**顺序**上：抢锁若排在建日志、
    // 建库、建窗口之后，第二个实例在被挡住之前就已经动过这三样东西了。顺序既跑不出来又极易
    // 在日后的重构里被无声打乱，故按源码位置钉死——与上面那条「定义即注册」同一路数。

    /// `run_inner` 的函数体源码。
    ///
    /// 必须切片而不是整份文件里搜：`fs_vault::lock_data_dir(&dir)` 这个字符串在本测试自己的
    /// 源码里也出现，整份搜必然自我命中，于是闸门被整条删掉之后断言依然全绿——那正是这类
    /// 源码级门禁最经典的自伤。
    fn run_inner_body() -> String {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
        )
        .unwrap();
        let from = src
            .find("\nfn run_inner(smoke_exit_ms: Option<u64>) -> Result<(), String> {")
            .expect("找不到 run_inner 的定义（改了签名请同步本测试）");
        let to = from
            + src[from..]
                .find("\nfn report_fatal(")
                .expect("找不到 run_inner 之后的下一个自由函数（改了排布请同步本测试）");
        src[from..to].to_string()
    }

    /// 抢锁必须排在**所有**会碰共享状态的启动动作之前。
    #[test]
    fn the_single_instance_lock_is_taken_before_anything_else_touches_the_data_dir() {
        let body = run_inner_body();
        let lock = body.find("fs_vault::lock_data_dir(&dir)").expect(
            "run_inner 里没有单实例闸门：第二个实例会照常建窗口、开同一个 fs.db、写同一份日志，\
             并往同一个 .fspart 里写——而传输引擎的 per-target 锁是进程内 static（审计2 #12）",
        );
        for (what, anchor, why) in [
            (
                "日志",
                "let log_dir = dir.join(\"logs\");",
                "两个进程各开一份写句柄，各自 set_len(0) 轮转同一个日志文件，互相削掉对方的日志",
            ),
            (
                "数据库",
                "fs_connmgr::Db::open",
                "两个进程会同时打开并写同一个 fs.db",
            ),
            (
                "窗口",
                "tauri::Builder::default()",
                "窗口一建出来，第二个实例就已经能跑传输了——而 per-target 锁只在进程内有效",
            ),
        ] {
            let at = body
                .find(anchor)
                .unwrap_or_else(|| panic!("找不到锚点 `{anchor}`（改了启动链路请同步本测试）"));
            assert!(lock < at, "单实例闸门排在了{what}之后：{why}");
        }
    }

    // ── panic 取证（M4a.1 T93）────────────────────────────────────────────────
    //
    // 全仓此前 `set_hook` 零处：GUI 进程通常没有终端，默认 hook 把 panic 写进没人看得到的
    // stderr，用户视角是「窗口突然没了」，我们手上一份现场都没有。

    /// `run()` 的函数体源码（切片，理由同 `run_inner_body`：整份文件搜会自我命中）。
    fn run_body() -> String {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
        )
        .unwrap();
        let from = src
            .find("\npub fn run() {")
            .expect("找不到 run 的定义（改了签名请同步本测试）");
        let to = from
            + src[from..]
                .find("\n/// panic 现场落盘")
                .expect("找不到 run 之后的下一项（改了排布请同步本测试）");
        src[from..to].to_string()
    }

    /// `std::panic::set_hook` 是**进程级**的：两个各自装 hook 的测试并行跑会互相抢占，
    /// 表现是「A 测试捕到了 B 测试的 panic 现场」（实测踩到，且只在全量跑时出现——
    /// 单独 `--lib crash` 永远是绿的，是最容易被漏掉的一类）。
    ///
    /// 故所有装 hook 的测试串行化。注意这把锁只能约束**本模块**里的测试；
    /// 别处若新增会 panic 的测试（如 `logfile::tests::survives_a_poisoned_lock`），
    /// 它的 panic 仍可能被这里装着的 hook 捕获——所以下面的断言一律对
    /// **自己的**探针内容做匹配，而不是「捕到了什么就断言什么」。
    static PANIC_HOOK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// hook 必须装在 `run_inner()` **之前**：否则 run_inner 自己那一段的 panic 没有现场，
    /// 而启动链路恰恰是最容易 panic 的一段（建库、开窗口、抢锁）。
    #[test]
    fn the_panic_hook_is_installed_before_run_inner() {
        let body = run_body();
        let hook = body.find("install_panic_hook(").expect(
            "run() 里没有装 panic hook：GUI 进程没有终端，panic 会写进无人可见的 stderr，\
             第一份用户崩溃报告将不含任何现场（M4a.1 T93）",
        );
        let inner = body
            .find("run_inner(smoke_exit_ms)")
            .expect("run() 里找不到 run_inner() 调用（改了启动链路请同步本测试）");
        assert!(
            hook < inner,
            "panic hook 装在了 run_inner() 之后：启动链路（建库/开窗口/抢锁）的 panic 没有现场"
        );
    }

    /// crash 文件**不能**经 tracing：非阻塞写入器的 WorkerGuard 被塞进进程级 OnceLock
    /// 永不析构，panic 后没有任何 flush 时机，最后那几条正是丢的那几条。
    #[test]
    fn the_crash_report_does_not_go_through_the_async_logger() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
        )
        .unwrap();
        let from = src
            .find("fn install_panic_hook(")
            .expect("找不到 install_panic_hook");
        let to = from
            + src[from..]
                .find("\n/// crash 文件落点")
                .expect("找不到 install_panic_hook 之后的下一项");
        let body = &src[from..to];
        assert!(
            body.contains("write_error_file"),
            "crash 现场没走同步直写（write_error_file）——换成别的写法前请先读该函数的注释"
        );
        assert!(
            !body.contains("tracing::"),
            "crash 现场经了 tracing：那是非阻塞写入器，panic 后不保证落盘（见 install_panic_hook 注释）"
        );
    }

    /// 四要素齐全：版本、线程、位置、调用栈。缺一个都会让第一份用户报告变成来回追问。
    #[test]
    fn the_crash_report_carries_version_thread_location_and_backtrace() {
        // 在**子线程**里 panic 并捕获，顺带验证线程名真的被带出来（主线程名恒为 "main"，
        // 分不出「取到了」与「恰好写死了 main」）。
        //
        // Arc 在线程外建、clone 进闭包：在线程内建会让闭包借着一个线程返回时就死掉的局部
        // （E0597）。hook 恢复后再读，读的是同一份 Arc。
        let _guard = PANIC_HOOK_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = captured.clone();
        std::thread::Builder::new()
            .name("fs-crash-probe".to_string())
            .spawn(move || {
                let prev = std::panic::take_hook();
                std::panic::set_hook(Box::new(move |info| {
                    // 只收自己的探针：hook 装着的窗口里别处线程也可能 panic（进程级 hook）
                    let report = super::crash_report(info);
                    if report.contains("探针：故意崩一次") {
                        *sink.lock().unwrap() = report;
                    }
                }));
                let _ = std::panic::catch_unwind(|| panic!("探针：故意崩一次"));
                std::panic::set_hook(prev);
            })
            .expect("起探针线程失败")
            .join()
            .expect("探针线程 join 失败");
        let named = captured.lock().unwrap().clone();

        assert!(
            named.contains(env!("CARGO_PKG_VERSION")),
            "缺版本号——不知道是哪个构建崩的。实得：{named}"
        );
        assert!(
            named.contains("fs-crash-probe"),
            "缺线程名——分不清是 UI 还是某条会话泵崩的。实得：{named}"
        );
        assert!(
            named.contains("lib.rs:"),
            "缺 panic 位置——只能猜。实得：{named}"
        );
        assert!(
            named.contains("探针：故意崩一次"),
            "缺 panic 原因。实得：{named}"
        );
        // backtrace 至少要有本 crate 的帧；force_capture 不看 RUST_BACKTRACE
        assert!(
            named.contains("future_shell_app") || named.contains("core::panicking"),
            "缺调用栈（force_capture 应当不依赖 RUST_BACKTRACE）。实得：{named}"
        );
        // 现场里不许出现会话内容/凭据类字样——这份文件是要用户直接发给我们的
        assert!(
            named.contains("不含会话内容与凭据"),
            "缺「这份文件不含什么」的说明：用户要能放心把它发出来。实得：{named}"
        );
    }

    /// 非字符串载荷不得让 hook 自己崩（panic 路径上再 panic 会直接 abort，现场全丢）。
    #[test]
    fn the_crash_report_survives_a_non_string_payload() {
        let _guard = PANIC_HOOK_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = captured.clone();
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // 同上：只收「非字符串载荷」那一发，别处的 panic 不写进来
            let report = super::crash_report(info);
            if report.contains("panic 载荷不是字符串") {
                *sink.lock().unwrap() = report;
            }
        }));
        let _ = std::panic::catch_unwind(|| std::panic::panic_any(42u32));
        std::panic::set_hook(prev);
        let body = captured.lock().unwrap().clone();
        assert!(
            body.contains("panic 载荷不是字符串"),
            "非字符串载荷应如实说明而不是留空。实得：{body}"
        );
        assert!(body.contains(env!("CARGO_PKG_VERSION")), "其余要素仍须齐全");
    }

    /// 落点：数据目录优先；解析不出来才回落临时目录，且名字带 pid（共享目录里固定名
    /// 既会被别的实例互踩，又是现成的软链靶子——口径同 fatal_error_path）。
    #[test]
    fn the_crash_file_path_prefers_the_data_dir_and_is_unique() {
        let d = std::path::Path::new("/data");
        let a = super::crash_file_path(Some(d));
        assert!(a.starts_with(d), "有数据目录时应落在其中，实得 {a:?}");
        let name = a.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("crash-"), "文件名应以 crash- 起头：{name}");
        assert!(
            name.contains(&std::process::id().to_string()),
            "文件名应含 pid（多线程各自 panic 时不互相覆盖）：{name}"
        );
        // 两次调用不得同名（纳秒时间戳）
        let b = super::crash_file_path(Some(d));
        assert_ne!(a, b, "两次 panic 的现场不得写到同一个文件（会互相覆盖）");
        // 无数据目录：回落临时目录且带产品前缀
        let t = super::crash_file_path(None);
        assert!(
            t.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("future-shell-crash-"),
            "回落临时目录时名字要带产品前缀（共享目录）：{t:?}"
        );
    }

    // ── IPC 追踪层接线（审计2 补充：用户「报错查无日志」的根因）───────────────
    //
    // `log_invoke` 是唯一能让「用户点了什么、传了什么参」落进日志文件的边界点。若哪天有人
    // 把 `.invoke_handler(...)` 又改回裸 `generate_handler![…]`，日志重归空白而门禁全绿。
    // 与「单实例闸门位置」同路数：按源码切片断言，避免整份文件搜导致自伤。
    #[test]
    fn the_invoke_handler_is_wrapped_by_the_ipc_trace_layer() {
        let body = run_inner_body();
        assert!(
            body.contains(".invoke_handler(ipc_log::log_invoke(tauri::generate_handler!["),
            "invoke_handler 没有包上 ipc_log::log_invoke——IPC 调用不再落日志，\
             用户操作与报错又将从日志里消失（见 ipc_log.rs 头注）"
        );
        // 包裹必须完整闭合（多一个 `)`）：少配对是编译错，但这里钉的是「确为包裹而非注释里提到」
        assert!(
            body.contains("commands::sftp_cmd::transfer_cancel,\n        ]))"),
            "log_invoke 的闭合括号丢失：命令注册列表没有被包进追踪层"
        );
    }

    // ── 启动错误文件（审计2 #33）──────────────────────────────────────────────

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fs-startup-err-test-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn write_error_file_writes_when_absent_and_replaces_when_present() {
        let dir = scratch_dir("replace");
        let path = dir.join("startup-error.txt");
        write_error_file(&path, "first").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        // 第二次（同名文件已存在）走 AlreadyExists → 删除重试路径：内容必须是最新一次，
        // 而不是 create_new 失败后什么都不写。
        write_error_file(&path, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "错误文件未按 0600 建出：{mode:o}");
        }
    }

    /// 软链靶子：预先在目标路径种一个指向别处的符号链接，错误文件写入不得跟进。
    #[cfg(unix)]
    #[test]
    fn write_error_file_does_not_follow_a_planted_symlink() {
        let dir = scratch_dir("symlink");
        let target = dir.join("victim.txt");
        std::fs::write(&target, "DO NOT TOUCH").unwrap();
        let path = dir.join("startup-error.txt");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        write_error_file(&path, "report").unwrap();
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "DO NOT TOUCH",
            "错误写入跟进了软链，写进了别人的文件"
        );
        // 软链已被替换成真正的常规文件，内容为本次报告
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert!(!meta.file_type().is_symlink(), "路径上仍是一个软链");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "report");
    }

    // ── 审计2 #31/#32/#33 的接线守卫 ─────────────────────────────────────────

    /// 日志写入器必须换成了尺寸上限轮转（审计2 #32）——这是**接线**守卫：
    /// logfile.rs 的 6 条单测守的是写入器本身，但它们全部通过也挡不住 run_inner
    /// 里把旧的 `tracing_appender::rolling` 换回来（两边各自全绿）。审计点名的是
    /// 「按日期滚动的 writer 限个数不限大小」，所以守卫同时断言新 writer 在、
    /// 旧 rolling 不在。
    #[test]
    fn the_log_writer_is_size_capped_not_date_rolled() {
        let body = run_inner_body();
        assert!(
            body.contains("logfile::SizeCappedFile::open"),
            "run_inner 不再使用尺寸上限的日志写入器——审计2 #32 的接线被换掉了"
        );
        assert!(
            !body.contains("tracing_appender::rolling"),
            "run_inner 里仍有按日期滚动的写入器——它限文件个数限不住单文件大小（审计2 #32）"
        );
    }

    /// 主线程栈必须抬过 MSVC 的 1 MiB 默认值（2026-08-27 的整程序崩溃）。
    ///
    /// 这是**接线**守卫：`app/src/rdp.rs` 的单测守的是那一个 future 的大小，
    /// 但它绿着也挡不住有人把 build.rs 里的 `/STACK` 删掉——而那一行没了，
    /// 下一个 400 KB 级的命令 future 会以完全相同的方式让整个程序消失
    /// （SEH 栈溢出：panic 钩子不触发、一份现场都不留，见 rdp.rs 的记述）。
    ///
    /// 断言到**数值**而不只是「这行在」：`/STACK:65536` 也能让这行存在。
    /// 链接参数没有运行时可观察面（它写在 PE 头里，而测试二进制不是那个 bin），
    /// 所以按源码钉是这里能拿到的最强守卫；真实产物的 SizeOfStackReserve
    /// 由发布前的手工核验兜底（docs/verification 的打包一节）。
    #[test]
    fn stack_reserve_is_raised_above_the_msvc_default() {
        const BUILD_RS: &str = include_str!("../build.rs");
        assert!(
            BUILD_RS.contains("cargo:rustc-link-arg-bins=/STACK:"),
            "build.rs 里没有 MSVC 的 /STACK 链接参数——Windows 主线程会退回 1 MiB 默认栈"
        );
        let declared: usize = BUILD_RS
            .lines()
            .find_map(|l| l.trim().strip_prefix("const MAIN_THREAD_STACK: usize = "))
            .and_then(|rhs| rhs.strip_suffix(';'))
            .and_then(|expr| {
                // 形如 `8 * 1024 * 1024`
                expr.split('*')
                    .map(|t| t.trim().parse::<usize>().ok())
                    .try_fold(1usize, |acc, n| n.map(|n| acc * n))
            })
            .expect("build.rs 里读不到 MAIN_THREAD_STACK 的字面量——守卫与实现分家了");
        assert!(
            declared >= 4 * 1024 * 1024,
            "主线程栈只留了 {declared} 字节。Tauri 在主线程上构造命令 future，\
             而它是全进程最窄的一条栈（Rust 自建线程 2 MiB / Linux 主线程 8 MiB）——\
             低于 4 MiB 等于把 2026-08-27 那次崩溃的条件又摆回来了。"
        );
    }

    /// 数据目录与日志目录的 0700 收紧（审计2 #31）是 `#[cfg(unix)]` 块，Windows 上
    /// 编译不进去、行为测试无从下手——按源码钉住「两处收紧都在」。
    #[test]
    fn data_and_logs_dirs_are_tightened_to_0700_on_unix() {
        let body = run_inner_body();
        let n = body.matches("Permissions::from_mode(0o700)").count();
        assert_eq!(
            n, 2,
            "run_inner 里 0700 收紧块应为 2 处（数据目录、日志目录），实际 {n}——审计2 #31"
        );
    }

    /// 错误文件落点（审计2 #33）：数据目录优先；临时目录回落必须带 pid 后缀。
    #[test]
    fn fatal_error_path_prefers_data_dir_then_pid_suffixed_temp_fallback() {
        let dir = std::path::Path::new("C:/data");
        assert_eq!(fatal_error_path(Some(dir)), dir.join("startup-error.txt"));
        let fallback = fatal_error_path(None);
        assert_eq!(fallback.parent(), Some(std::env::temp_dir().as_path()));
        let name = fallback.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("future-shell-startup-error-") && name.ends_with(".txt"),
            "临时回落名必须带 pid 后缀：{name}"
        );
        assert_ne!(
            name, "future-shell-startup-error.txt",
            "临时回落不许再用固定名（别的实例互踩 + 软链靶子）"
        );
    }
}
