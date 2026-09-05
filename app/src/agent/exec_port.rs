//! russh 通道 → `fs_ai::exec::ExecChannel` 适配器 + `ExecPort` 实现。
//!
//! **全仓唯一的 `fs_ai::exec::run_command` 生产调用点**（agent 侧 grep 自守钉住
//! fs_ai 里没有第二个；这边由 `the_only_run_command_call_site` 钉住 app 侧唯一）。
//!
//! # 心跳读是硬性要求，不是优化
//!
//! `run_command` 的超时检查在读循环的**顶部**（`exec.rs` 的 `loop { if elapsed >= timeout … ; ch.read().await }`）。
//! 若 `read()` 在静默命令（`sleep 300`）下永久阻塞，控制权永远回不到循环顶——
//! **连普通的 60 秒超时都不会触发**，急停更无从谈起。
//!
//! 所以这里的 `read()` 是 `select!`：真读 vs 250ms 定时器；定时器赢时返回一个
//! **空的 stdout 块**。空块对 `run_command` 是无害的 no-op（`push_str("")`），
//! 但它让循环转一圈、让超时检查可见。
//!
//! 这个坑在 fake 通道下测不出来——fake 的 read 都是即时返回的。所以它配一条
//! 专属测试（`a_silent_command_still_hits_the_timeout`）。

use fs_ai::agent::ports::{ChannelLease, ChannelRegistry, ExecPort, RunClock};
use fs_ai::agent::tool::Tool;
use fs_ai::exec::{Chunk, ExecChannel, ExecError, ExecMode, Observation, RunSpec};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// 心跳间隔。250ms：急停的可见延迟上界 = 一次心跳 + 一次循环迭代。
///
/// 调大它会让急停变钝（用户按了停、命令还在跑），调小会在高频输出时
/// 白白多转几圈循环。250ms 是「人感觉得到的即时」与「无谓唤醒」之间的常见折中。
pub const READ_HEARTBEAT: Duration = Duration::from_millis(250);

/// 一条 russh exec 通道的适配器。
pub struct RusshAgentChannel {
    channel: russh::Channel<russh::client::Msg>,
    exit_status: Option<i32>,
    /// EOF 之后不再读（`read` 返回 `Ok(None)`）。
    eof: bool,
    killed: bool,
    closed: bool,
}

impl RusshAgentChannel {
    pub fn new(channel: russh::Channel<russh::client::Msg>) -> Self {
        Self {
            channel,
            exit_status: None,
            eof: false,
            killed: false,
            closed: false,
        }
    }
}

impl ExecChannel for RusshAgentChannel {
    async fn read(&mut self) -> Result<Option<Chunk>, ExecError> {
        if self.eof {
            return Ok(None);
        }
        loop {
            // 心跳：真读与定时器竞速（helper 里做，那样它可测——见
            // heartbeat_race 的文档）。定时器赢 ⇒ 空块，让 run_command 的循环
            // 转一圈、超时检查才可见。
            let Some(msg) = heartbeat_race(self.channel.wait()).await else {
                return Ok(Some(Chunk::Stdout(String::new())));
            };
            match msg {
                Some(russh::ChannelMsg::Data { data }) => {
                    return Ok(Some(Chunk::Stdout(
                        String::from_utf8_lossy(&data).to_string(),
                    )));
                }
                Some(russh::ChannelMsg::ExtendedData { data, ext: 1 }) => {
                    return Ok(Some(Chunk::Stderr(
                        String::from_utf8_lossy(&data).to_string(),
                    )));
                }
                // **绝不在 Eof 上退出**：RFC 4254 §5.3 规定 exit-status 在通道关闭
                // 之前送达，而它常常**在 Eof 之后**才到。在 Eof 处 break 会让退出码
                // 永远拿不到——这个坑本仓踩过一次（S41，SHA256 校验在真机上从未执行）。
                Some(russh::ChannelMsg::Eof) => continue,
                Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                    self.exit_status = Some(exit_status as i32);
                    continue;
                }
                Some(russh::ChannelMsg::Close) | None => {
                    self.eof = true;
                    return Ok(None);
                }
                // 窗口调整等控制消息：继续读
                Some(_) => continue,
            }
        }
    }

    fn exit_status(&self) -> Option<i32> {
        self.exit_status
    }

    async fn kill(&mut self) {
        // 幂等（`run_command` 的超时臂先 kill 再 close，两者都可能被重入）。
        if self.killed {
            return;
        }
        self.killed = true;
        // SIGTERM 而不是 SIGKILL：给远端进程一个善后的机会（写完当前块、关文件）。
        // 很多 sshd 配置忽略 signal 请求——那时 close 会带走通道，进程随之收到 SIGHUP。
        let _ = self.channel.signal(russh::Sig::TERM).await;
    }

    async fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = self.channel.close().await;
    }
}

/// `ExecPort` 的真实现：每次执行开一条新通道，走 `run_command` 的契约。
pub struct RusshExecPort {
    session: Arc<crate::sessions::LiveSession>,
    registry: Arc<ChannelRegistry>,
    /// 每命令超时。**本批恒为 `DEFAULT_TIMEOUT_SECS`**——总设计 §4.4 写着
    /// 「可按 profile 配置」，而那个配置项还没有载体（profile 表里没有这个字段）。
    /// 留一个没人调的 with_timeout 等于把「可配」写成一句没人兑现的承诺；
    /// 真要配时连同 profile 字段一起加。
    timeout_secs: u64,
    /// 外部 MCP 挂载表（M3 出口 6）。`external_mcp` 用它按 `server_id` 找到
    /// 外部 server 的启动配置。SSH 那几路（run/read/list/get/put）不碰它。
    db: Arc<fs_connmgr::Db>,
}

impl RusshExecPort {
    pub fn new(
        session: Arc<crate::sessions::LiveSession>,
        registry: Arc<ChannelRegistry>,
        db: Arc<fs_connmgr::Db>,
    ) -> Self {
        Self {
            session,
            registry,
            timeout_secs: fs_ai::exec::DEFAULT_TIMEOUT_SECS,
            db,
        }
    }

    /// 开一条 exec 通道并跑一条命令。租约随返回值析构（计数归零由 Drop 兜底；
    /// 通道 close 的保证在 `run_command` 的单一返回点）。
    async fn exec(&self, command: &str, mode: ExecMode, clock: &RunClock) -> Observation {
        let _lease: ChannelLease = self.registry.lease();
        // 每命令独立计时——上一条的耗时不算进这一条。
        clock.reset();

        let handle = self.session.handle.lock().await;
        let channel = match handle.channel_open_session().await {
            Ok(c) => c,
            Err(e) => {
                return Observation {
                    stderr: format!("[通道错误] 开通道失败：{e}\n"),
                    ..Default::default()
                }
            }
        };
        drop(handle);
        let mut ch = RusshAgentChannel::new(channel);
        // CurrentTerminal 模式在本层组合哨兵；Exec 模式直接发命令。
        //
        // **两种模式都没有 TTY**：上面 channel_open_session() + exec() 从不
        // request_pty（那个 true 是 want_reply）。所以 sudo/passwd 在两边都会
        // 快速失败——这一点必须与 tool.rs 的 schema 保持一致，否则就是在骗
        // 模型（2026-08-28 修：schema 曾写「需要 TTY 时用 current_terminal」）。
        // 要真给 TTY，得走 session.write（term_input 用的那个用户 PTY 写半部），
        // 那是一次独立设计，见 fs_ai::exec 模块头。
        let wire_cmd = match mode {
            ExecMode::Exec => command.to_string(),
            ExecMode::CurrentTerminal => fs_ai::exec::wrap_for_terminal(command),
        };
        if let Err(e) = ch.channel.exec(true, wire_cmd.as_str()).await {
            return Observation {
                stderr: format!("[通道错误] exec 请求失败：{e}\n"),
                ..Default::default()
            };
        }
        let spec = RunSpec {
            command: command.to_string(),
            timeout_secs: self.timeout_secs,
            max_output_bytes: fs_ai::exec::DEFAULT_MAX_OUTPUT_BYTES,
            mode,
        };
        // ← 全仓唯一的 run_command 生产调用点
        let mut obs = fs_ai::exec::run_command(&spec, &mut ch, clock).await;
        if mode == ExecMode::CurrentTerminal {
            // 先剥回显再找哨兵（顺序不能反：不剥的话 parse_sentinel 会在回显里
            // 命中格式串 `%d`，解析失败 → 表现为「命令跑完了却等到超时」）。
            let cleaned = fs_ai::exec::strip_command_echo(&obs.stdout, command);
            if let Some((before, code)) = fs_ai::exec::parse_sentinel(&cleaned) {
                obs.stdout = before;
                obs.exit_code = Some(code);
            } else {
                obs.stdout = cleaned;
            }
        }
        obs
    }
}

impl ExecPort for RusshExecPort {
    fn run<'a>(
        &'a self,
        tool: &'a Tool,
        clock: &'a RunClock,
    ) -> Pin<Box<dyn Future<Output = Observation> + Send + 'a>> {
        Box::pin(async move {
            match tool {
                Tool::RunCommand { command, mode } => self.exec(command, *mode, clock).await,
                // 其余工具由 dispatch 分派到专用方法，不会走到这里。
                _ => Observation::default(),
            }
        })
    }

    fn read_remote_file<'a>(
        &'a self,
        path: &'a str,
        max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Observation> + Send + 'a>> {
        Box::pin(async move {
            // 用 `head -c` 而不是 `cat`：远端一个 10GB 的文件不该把这条通道
            // 变成一次 10GB 的传输——上限在**远端**生效，而不是读回来再截。
            let cmd = format!("head -c {max_bytes} -- {}", shell_quote(path));
            self.exec(&cmd, ExecMode::Exec, &RunClock::new(noop_abort()))
                .await
        })
    }

    fn list_remote_dir<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Observation> + Send + 'a>> {
        Box::pin(async move {
            let cmd = format!("ls -la -- {}", shell_quote(path));
            self.exec(&cmd, ExecMode::Exec, &RunClock::new(noop_abort()))
                .await
        })
    }

    fn sftp_get<'a>(
        &'a self,
        remote: &'a str,
        local_dest: &'a str,
    ) -> Pin<Box<dyn Future<Output = Observation> + Send + 'a>> {
        Box::pin(async move {
            // M3 本批：sftp 两路先给出**明确的未实装**而不是假成功。
            // 假成功的代价是 Agent 以为文件到了、继续基于它推理。
            Observation {
                stderr: format!(
                    "[未实装] sftp_get（{remote} → {local_dest}）在本批未接线；\
                     传输沙箱与路径分级随 M3 传输面一起落地。\n"
                ),
                exit_code: None,
                ..Default::default()
            }
        })
    }

    fn sftp_put<'a>(
        &'a self,
        local_src: &'a str,
        remote_dest: &'a str,
    ) -> Pin<Box<dyn Future<Output = Observation> + Send + 'a>> {
        Box::pin(async move {
            Observation {
                stderr: format!("[未实装] sftp_put（{local_src} → {remote_dest}）在本批未接线。\n"),
                exit_code: None,
                ..Default::default()
            }
        })
    }

    fn external_mcp<'a>(
        &'a self,
        server_id: &'a str,
        tool: &'a str,
        arguments_json: &'a str,
    ) -> Pin<Box<dyn Future<Output = Observation> + Send + 'a>> {
        let server_id = server_id.to_string();
        let tool = tool.to_string();
        let arguments_json = arguments_json.to_string();
        let db = self.db.clone();
        Box::pin(async move {
            // 按 server_id 找挂载配置。配置缺失/找不到 ⇒ 明确失败，不是假成功。
            let cfg = match crate::mcp::mount_config_for(&db, &server_id).await {
                Ok(Some(c)) => c,
                Ok(None) => {
                    return Observation {
                        stderr: format!(
                            "[外部 MCP] 找不到名为 `{server_id}` 的挂载配置。\
                             请在设置里添加该外部服务器。\n"
                        ),
                        exit_code: None,
                        ..Default::default()
                    }
                }
                Err(e) => {
                    return Observation {
                        stderr: format!("[外部 MCP] 读取挂载配置失败：{e}\n"),
                        exit_code: None,
                        ..Default::default()
                    }
                }
            };
            match fs_mcpbridge::client::call_tool_once(&cfg, &tool, &arguments_json).await {
                Ok(obs) => obs,
                Err(e) => Observation {
                    stderr: format!("[外部 MCP] {e}\n"),
                    exit_code: None,
                    ..Default::default()
                },
            }
        })
    }
}

/// 心跳竞速：把一个「可能永远不返回」的读与 [`READ_HEARTBEAT`] 定时器赛跑。
///
/// 返回 `None` = 定时器赢了（该发一个空块让循环转一圈）。
///
/// **抽成独立函数是为了它可测**：真实的 `RusshAgentChannel::read` 要一条 russh
/// 通道，而那个类型在测试里造不出来——于是第一版的心跳测试用的是它自己写的
/// 假通道，**测的是那个 fake 会不会心跳，而不是真实现会不会**。变异验证当场
/// 揭穿了这一点：把真实现的心跳臂换成永不触发，测试照样绿。
async fn heartbeat_race<T>(fut: impl std::future::Future<Output = T>) -> Option<T> {
    tokio::select! {
        v = fut => Some(v),
        _ = tokio::time::sleep(READ_HEARTBEAT) => None,
    }
}

/// 单引号包裹 + 内部单引号转义。模型给的路径要进 shell，**不能**直接拼。
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 读类工具用的空急停信号（它们不吃急停时钟——一次 ls 的时长以毫秒计）。
fn noop_abort() -> Arc<dyn fs_ai::agent::ports::AbortSignal> {
    struct Never;
    impl fs_ai::agent::ports::AbortSignal for Never {
        fn requested(&self) -> bool {
            false
        }
        fn fired<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
            Box::pin(std::future::pending())
        }
    }
    Arc::new(Never)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **心跳读的存在性**：静默命令下 `read()` 必须周期性让出，否则
    /// `run_command` 的超时检查（循环顶）永远到不了。
    ///
    /// 这条用一个「永不出数据」的假通道验证语义：读会在 250ms 时给空块，
    /// 于是超时臂在 1 秒预算下按时命中。没有心跳的话这个测试会挂到超时。
    #[tokio::test]
    async fn a_silent_command_still_hits_the_timeout() {
        struct SilentChannel {
            killed: usize,
            closed: usize,
        }
        impl ExecChannel for SilentChannel {
            async fn read(&mut self) -> Result<Option<Chunk>, ExecError> {
                // 模拟本模块 read() 的心跳行为：永不出真数据，但每 1ms 给空块
                tokio::time::sleep(Duration::from_millis(1)).await;
                Ok(Some(Chunk::Stdout(String::new())))
            }
            fn exit_status(&self) -> Option<i32> {
                None
            }
            async fn kill(&mut self) {
                self.killed += 1;
            }
            async fn close(&mut self) {
                self.closed += 1;
            }
        }
        struct RealClock(std::time::Instant);
        impl fs_ai::exec::Clock for RealClock {
            fn elapsed_secs(&self) -> u64 {
                self.0.elapsed().as_secs()
            }
        }
        let mut ch = SilentChannel {
            killed: 0,
            closed: 0,
        };
        let spec = RunSpec {
            timeout_secs: 1,
            ..RunSpec::new("sleep 300")
        };
        let obs =
            fs_ai::exec::run_command(&spec, &mut ch, &RealClock(std::time::Instant::now())).await;
        assert!(obs.timed_out, "静默命令该按时超时——没有心跳的话这里会挂住");
        assert_eq!(obs.exit_code, None);
        assert_eq!(ch.killed, 1, "超时要杀远端进程");
        assert_eq!(ch.closed, 1, "通道要关");
    }

    /// **真实心跳路径**：一个永不返回的读必须被定时器截断（返回 None）。
    ///
    /// 这条测的是 read() 真正用的那个函数。上一条（a_silent_command…）测的是
    /// 「给 run_command 一个会心跳的通道，超时臂会不会走」——两条缺一不可：
    /// 前者证明这一层会让出，后者证明让出之后那一层会正确收尾。
    #[tokio::test]
    async fn a_read_that_never_returns_is_cut_by_the_heartbeat() {
        let started = std::time::Instant::now();
        let r = heartbeat_race(std::future::pending::<u8>()).await;
        assert!(r.is_none(), "永不返回的读该被心跳截断");
        // 真的等了大约一个心跳（而不是立刻返回 None——那样是另一种错）
        assert!(
            started.elapsed() >= READ_HEARTBEAT,
            "心跳不该提前触发：实测 {:?}",
            started.elapsed()
        );
    }

    /// 有数据时心跳不该抢走它。
    #[tokio::test]
    async fn a_read_that_returns_wins_over_the_heartbeat() {
        let r = heartbeat_race(async { 7u8 }).await;
        assert_eq!(r, Some(7));
    }

    /// 心跳常量：改它就是改急停的可见延迟上界。
    #[test]
    fn the_heartbeat_is_a_quarter_second() {
        assert_eq!(READ_HEARTBEAT, Duration::from_millis(250));
    }

    /// shell 引用：模型给的路径不能直接拼进命令。
    #[test]
    fn paths_from_the_model_are_shell_quoted() {
        assert_eq!(shell_quote("/tmp/a b"), "'/tmp/a b'");
        // 单引号闭合攻击：`'; rm -rf / ; '`
        let evil = "/tmp/'; rm -rf / ; '";
        let q = shell_quote(evil);
        assert!(q.starts_with('\'') && q.ends_with('\''));
        // 内部单引号被转义成 '\'' ——闭合不了外层引号
        assert!(q.contains("'\\''"), "单引号没被转义：{q}");
    }

    /// **app 层的 run_command 唯一调用点**（fs_ai 侧由 agent/mod.rs 的自守钉住）。
    #[test]
    fn the_only_run_command_call_site_in_app_is_this_file() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut hits: Vec<String> = Vec::new();
        walk(&root, &mut |p, src| {
            // 剥注释：解释「唯一调用点」的注释里必然写着这个名字
            let code: String = src
                .lines()
                .map(|l| match l.find("//") {
                    Some(i) => &l[..i],
                    None => l,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if code.contains("exec::run_command(") {
                hits.push(p.file_name().unwrap().to_string_lossy().to_string());
            }
        });
        assert_eq!(
            hits,
            vec!["exec_port.rs".to_string()],
            "run_command 的 app 侧调用点必须唯一：{hits:?}"
        );
    }

    fn walk(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, f);
            } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                if let Ok(s) = std::fs::read_to_string(&p) {
                    f(&p, &s);
                }
            }
        }
    }
}
