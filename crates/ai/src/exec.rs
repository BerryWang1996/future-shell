//! `run_command` 契约（总设计 §4.4；MCP 的 `command.run` 共享同一份）。
//!
//! # 这里只有纯逻辑
//!
//! 真正跑命令要 SSH 通道，那是 app 层的事。本模块只放**契约本身**：
//! 超时与截断的语义、观察值的形状、当前终端模式的哨兵包装与解析。
//! 拆开的理由与 `transport.rs` 那次一样——这几件事全都能离线测，
//! 而一旦它们和「开一个 channel」缠在一起，就只能靠容器测试覆盖，
//! 于是「截断边界差一个字节」这类问题永远没人测。
//!
//! # 两种执行模式不是同一件事
//!
//! * **exec 通道**：开一个 SSH exec channel，拿干净的 stdout/stderr/exit code。
//!   这是默认模式。代价是**没有 TTY**——`sudo`/`apt` 这类要读密码的命令会
//!   快速失败（"a terminal is required…"），而不是挂住。
//! * **当前终端**：附一个哨兵行来取退出码。
//!
//! # ⚠ 当前终端模式**目前也没有 TTY**（2026-08-28 核实）
//!
//! 上面这段原本写的是「把命令注入用户正在看的那个 PTY……收益是 TTY 真的在
//! 那儿」。那是**设计意图，实现没有兑现**：`app/src/agent/exec_port.rs` 的
//! `exec()` 对两种模式都走 `channel_open_session()` + `channel.exec(...)`，
//! 全程没有 `request_pty`（那个 `true` 是 want_reply 不是 PTY），
//! `wrap_for_terminal` 也只是在命令后面接了个 `printf` 哨兵。
//!
//! 后果是**工具 schema 在骗模型**：它写着「需要 TTY（sudo 等）时用
//! current_terminal」，模型照做，`sudo` 在那边同样快速失败。已把 schema
//! 改成实话（见 `agent/tool.rs`），并由
//! `current_terminal_must_not_promise_a_tty` 钉住。
//!
//! 要真兑现，得让实现走 `session.write`（`term_input` 用的那个写半部，
//! 那才是用户 PTY）——但那会引入「输出与用户自己的操作混在一起」「用户正在
//! 敲字时注入」两类新问题，是一次独立的设计，不在本次修复范围。
//!
//! 两者共享超时与截断，取退出码的方式不同：exec 通道有协议级 exit-status，
//! 哨兵是为「将来真的注入 PTY」准备的（PTY 只能给出 shell 自己的退出状态）。

use serde::{Deserialize, Serialize};

/// 默认超时。总设计 §4.4：60 秒，可按 profile 配置。
///
/// 流式/长驻命令（`tail -f`、`journalctl -f`、`kubectl logs -f`）**按设计会超时**。
/// 那不是缺陷：一个永不返回的命令在「拿到观察值再决定下一步」的循环里
/// 就是死锁，而把超时调大只是把死锁推后。Agent 的指引里会说明改用后台模式。
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// 默认输出上限。总设计 §4.4：32 KiB。
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 32 * 1024;

/// 当前终端模式的哨兵前缀。
///
/// 用一个不像任何真实输出的串：命令自己的输出里若出现同样的行，
/// 我们会把它当成退出码。`__FS_RC__` 这个形状（双下划线包夹 + 项目缩写）
/// 在真实的运维输出里不会出现，而 `RC=0` 或 `exit=0` 会。
pub const SENTINEL_PREFIX: &str = "__FS_RC__=";

/// 截断标记的模板。`{n}` 处填**被删掉**的字节数。
///
/// 写「被删掉多少」而不是「原本多长」：下游读这段的是模型，
/// 它需要知道的是「我看不到的那部分有多大」。
const TRUNCATION_NOTE: &str = "\n[…{n} bytes truncated…]\n";

/// 一次执行的观察值。字段就是总设计 §4.4 列的那四样。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Observation {
    pub stdout: String,
    pub stderr: String,
    /// 退出码。`None` = 拿不到（超时被杀、或哨兵没出现）。
    ///
    /// **不用 -1 之类的哨兵值**：`-1` 会被下游当成一个真的退出码去解释，
    /// 而「超时了所以没有退出码」和「命令返回了 255」是完全不同的两件事。
    pub exit_code: Option<i32>,
    /// stdout 或 stderr 被截断过。
    pub truncated: bool,
    /// 超时被杀。此时 stdout/stderr 是**已收到的那部分**（partial_output）。
    pub timed_out: bool,
}

/// 执行模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecMode {
    /// 独立 exec 通道（默认）。
    #[default]
    Exec,
    /// 注入用户当前的终端（PTY），靠哨兵取退出码。
    CurrentTerminal,
}

/// 一次执行的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSpec {
    pub command: String,
    pub timeout_secs: u64,
    pub max_output_bytes: usize,
    pub mode: ExecMode,
}

impl RunSpec {
    /// 按默认契约构造。
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            mode: ExecMode::Exec,
        }
    }
}

/// 截断结果：处理后的文本，以及有没有真的截。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Truncated {
    pub text: String,
    pub truncated: bool,
}

/// 按 §4.4 截断：首尾保留、中间挖掉、插入标记。
///
/// 三个容易漏掉的点，各自都有测试钉着：
///
/// 1. **标记本身占预算。** 不占的话「输出上限 32 KiB」这个承诺就不成立——
///    结果会是 32 KiB 加上标记，而下游按 32 KiB 分配缓冲的地方就溢出了。
/// 2. **在字符边界切。** 按字节切会把一个多字节字符劈成两半，
///    下游解码得到替换字符，而中文/日文的运维输出里这几乎必然发生。
///    这与 `context.rs` 的屏幕截断同一原则。
/// 3. **头略多于尾。** 预算除不尽时多的那一个字节给头部：命令输出的开头
///    通常更重要（错误信息、表头、版本号），而尾部往往是重复的行。
///
/// `max` 小到装不下标记时，退化成「只保留头部」——那时插一个比正文还长的
/// 标记没有意义。
pub fn truncate_output(s: &str, max: usize) -> Truncated {
    if s.len() <= max {
        return Truncated {
            text: s.to_string(),
            truncated: false,
        };
    }
    // 标记的实际长度取决于被删的字节数，而被删的字节数又取决于标记长度——
    // 互相依赖。用「先按最大可能长度估算」来断开这个环：数字位数至多与
    // 总长度的位数相同，估大一点只会让保留的正文少几个字节，不会破坏上限。
    let note_len = TRUNCATION_NOTE.len() - 3 + digits(s.len());
    if max <= note_len {
        // 装不下标记：只留头部。这时用户看到的是一段没有任何提示的截断文本,
        // 但 `truncated: true` 仍然如实上报——调用方据此知道这不是全部。
        return Truncated {
            text: floor_char_boundary_str(s, max).to_string(),
            truncated: true,
        };
    }
    let budget = max - note_len;
    let head_len = budget - budget / 2; // 除不尽时头多一个
    let tail_len = budget / 2;

    let head = floor_char_boundary_str(s, head_len);
    let tail = ceil_char_boundary_str(s, s.len() - tail_len);
    let cut = s.len() - head.len() - tail.len();
    let note = TRUNCATION_NOTE.replace("{n}", &cut.to_string());
    Truncated {
        text: format!("{head}{note}{tail}"),
        truncated: true,
    }
}

/// 十进制位数。用于估算截断标记里那个数字占多少字节。
fn digits(mut n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let mut d = 0;
    while n > 0 {
        d += 1;
        n /= 10;
    }
    d
}

/// `&s[..=idx]` 向下取到字符边界。`str::floor_char_boundary` 尚未稳定，故自己写。
fn floor_char_boundary_str(s: &str, mut idx: usize) -> &str {
    if idx >= s.len() {
        return s;
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}

/// `&s[idx..]` 向上取到字符边界。
fn ceil_char_boundary_str(s: &str, mut idx: usize) -> &str {
    if idx >= s.len() {
        return "";
    }
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    &s[idx..]
}

/// 当前终端模式：给命令附上哨兵。
///
/// `cmd; printf '__FS_RC__=%d\n' $?`
///
/// 用 `;` 而不是 `&&`：命令失败时我们**更**需要退出码，而 `&&` 会让
/// printf 不执行——那时哨兵永不出现，只能等超时。这个错误很隐蔽：
/// 成功路径全都正常，只有失败时表现为「卡 60 秒然后说超时」。
///
/// 用 `printf` 而不是 `echo`：`echo` 的 `-n`/`-e` 行为在不同 shell 下不一致，
/// 而这一行的输出格式是要被解析的。
pub fn wrap_for_terminal(command: &str) -> String {
    format!("{command}; printf '{SENTINEL_PREFIX}%d\\n' $?\n")
}

/// 从终端输出里找哨兵，返回 `(哨兵之前的输出, 退出码)`。
///
/// 找**最后一个**哨兵：用户的终端里可能有上一次执行留下的哨兵行，
/// 而我们要的是这一次的。找第一个会让第二次执行读到第一次的退出码——
/// 那是个静默的错误答案，比读不到糟得多。
///
/// 没找到返回 `None`——调用方据此继续等或判超时。**不猜 0**：
/// 「还没结束」和「成功结束了」在 Agent 循环里导向完全不同的下一步。
pub fn parse_sentinel(output: &str) -> Option<(String, i32)> {
    let idx = output.rfind(SENTINEL_PREFIX)?;
    let after = &output[idx + SENTINEL_PREFIX.len()..];
    // 哨兵行到行尾。远端可能是 \r\n（PTY 常态），故两个都当终止符。
    let end = after.find(['\r', '\n']).unwrap_or(after.len());
    let code: i32 = after[..end].trim().parse().ok()?;
    Some((output[..idx].to_string(), code))
}

/// 从哨兵行里剥掉命令回显那一行。
///
/// PTY 会把我们注入的整行命令回显出来，包括 `; printf '__FS_RC__=%d\n' $?` 这一段。
/// 不剥的话，观察值的第一行是我们自己写的包装代码——模型会把它当成命令的输出去解释,
/// 而更糟的是 `parse_sentinel` 会在**回显里**找到 `__FS_RC__=` 这个串
/// （回显的是格式串 `%d` 而不是数字，所以 parse 失败、返回 None，
/// 表现为「命令跑完了却一直等到超时」）。
///
/// 判据是「这一行同时含命令原文与哨兵前缀」——只按哨兵前缀判会把真正的哨兵行也剥掉。
pub fn strip_command_echo(output: &str, command: &str) -> String {
    let mut out = String::with_capacity(output.len());
    for line in output.split_inclusive('\n') {
        let t = line.trim_end_matches(['\r', '\n']);
        if t.contains(SENTINEL_PREFIX) && t.contains(command) {
            continue;
        }
        out.push_str(line);
    }
    out
}

/* ── 执行器：超时、截断、以及「通道一定回收」───────────────────────────── */

/// 一条 exec 通道。真实实现在 app 层（russh），这里只要它的形状。
///
/// 拆成 trait 的理由与 `transport.rs` 的 `Transport` 一样：
/// 「超时了要杀掉通道」「无论走哪条路径都要回收」这两件事**必须**能离线测——
/// 而它们恰好是最容易漏的：成功路径上谁都会记得关，异常路径上没人记得。
#[allow(async_fn_in_trait)]
pub trait ExecChannel {
    /// 读一段输出。`Ok(None)` = 对端已结束（EOF）。
    async fn read(&mut self) -> Result<Option<Chunk>, ExecError>;
    /// 协议级退出码。EOF 之后才有意义；拿不到返回 `None`。
    fn exit_status(&self) -> Option<i32>;
    /// 杀掉远端进程并关闭通道。**必须幂等**：超时路径会先杀再关。
    async fn kill(&mut self);
    /// 关闭通道，释放本地资源。**必须幂等**。
    async fn close(&mut self);
}

/// 一段输出，带来源。stdout 与 stderr 分开累计（§4.4 观察值要求两者都有）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    Stdout(String),
    Stderr(String),
}

/// 执行期的错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecError {
    #[error("通道错误：{0}")]
    Channel(String),
}

/// 超时判据。注入而不是直接读时钟：一条「60 秒后超时」的测试不能真等 60 秒，
/// 而把超时改成 1 毫秒来测又会让测试变成一场竞态赌博。
pub trait Clock {
    /// 距开始已过去多少秒。
    fn elapsed_secs(&self) -> u64;
}

/// 跑一条命令，按 §4.4 契约产出观察值。
///
/// # 三条不变式
///
/// 1. **通道一定回收。** 正常结束、超时、读出错——三条路径的出口都过 `close()`。
///    这是 M3 出口原文「通道回收无悬挂」的机制：不靠「记得写 close」，
///    而是让函数只有一个返回点。
/// 2. **超时先杀后关。** 只 close 不 kill 会留下一个仍在跑的远端进程——
///    本地看不见它，而它可能正在写一个大文件。
/// 3. **超时也要交回已收到的输出。** `{partial_output, timed_out: true}` 是
///    契约明文。丢掉部分输出等于让 Agent 少一次判断依据，而它下一步很可能
///    就是重跑同一条命令。
pub async fn run_command<C: ExecChannel, K: Clock>(
    spec: &RunSpec,
    ch: &mut C,
    clock: &K,
) -> Observation {
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut timed_out = false;
    let mut read_error: Option<String> = None;

    loop {
        // 超时检查在**读之前**：读可能阻塞，检查放在后面就等于多等一整个读周期。
        if clock.elapsed_secs() >= spec.timeout_secs {
            timed_out = true;
            break;
        }
        match ch.read().await {
            Ok(Some(Chunk::Stdout(s))) => stdout.push_str(&s),
            Ok(Some(Chunk::Stderr(s))) => stderr.push_str(&s),
            Ok(None) => break, // EOF：命令正常结束
            Err(ExecError::Channel(e)) => {
                // 读出错不是超时。分开记：Agent 对这两种的下一步不同——
                // 超时可以改用后台模式重试，通道错误重试同一条命令多半还是错。
                read_error = Some(e);
                break;
            }
        }
        // 累计输出已经远超上限时提早停：继续读一个 100 MB 的 `cat` 只是在
        // 浪费带宽与内存，反正超出的部分马上要被截掉。留 2 倍余量是为了
        // 让「刚好在边界」的输出仍然完整走完 EOF，从而拿到退出码。
        if stdout.len() + stderr.len() > spec.max_output_bytes * 2 {
            break;
        }
    }

    // 退出码：超时或读错时**没有**退出码（不猜 0，也不用 -1 之类的哨兵值）。
    let exit_code = if timed_out || read_error.is_some() {
        None
    } else {
        ch.exit_status()
    };

    if timed_out {
        // 先杀后关。只 close 会留下一个仍在跑的远端进程。
        ch.kill().await;
    }
    ch.close().await;

    if let Some(e) = read_error {
        // 通道错误写进 stderr 而不是单开一个字段：观察值是给模型看的，
        // 而模型对「stderr 里有一句通道错误」的理解好过一个它没见过的字段。
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&format!("[通道错误] {e}\n"));
    }

    // 截断：两个流各自受同一个上限约束，而不是共享一个总额。
    // 共享总额的话，一个话多的 stderr 会把 stdout 挤没——而 stdout 通常是
    // 我们真正要的那一份。
    let so = truncate_output(&stdout, spec.max_output_bytes);
    let se = truncate_output(&stderr, spec.max_output_bytes);
    Observation {
        stdout: so.text,
        stderr: se.text,
        exit_code,
        truncated: so.truncated || se.truncated,
        timed_out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /* ── 执行器：超时、截断、通道回收 ─────────────────────────────────── */

    /// 假通道。记下 kill/close 各被调了几次——「通道回收无悬挂」这条出口
    /// 要的就是这个计数。
    struct FakeChannel {
        chunks: Vec<Result<Option<Chunk>, ExecError>>,
        idx: usize,
        status: Option<i32>,
        killed: usize,
        closed: usize,
        /// 拆除动作的**发生顺序**（"kill" / "close"）。
        ///
        /// 只记次数不记顺序，是这个 fake 曾经的缺陷：把 `kill()`/`close()` 对调
        /// 之后两个计数完全不变，于是「先杀后关」这条不变式在 182 条测试里
        /// 一条都没被钉住——交叉审计用变异实证了这一点。次数回答「有没有做」，
        /// 顺序回答「做对了没有」，而这里错的恰恰是后者：只 close 不 kill 会
        /// 留下一个仍在跑的远端进程，而 close 先于 kill 时通道已经没了，
        /// kill 打在空处，效果与不 kill 相同。
        teardown: Vec<&'static str>,
    }

    impl FakeChannel {
        fn new(chunks: Vec<Result<Option<Chunk>, ExecError>>, status: Option<i32>) -> Self {
            Self {
                chunks,
                idx: 0,
                status,
                killed: 0,
                closed: 0,
                teardown: Vec::new(),
            }
        }
        fn out(s: &str) -> Result<Option<Chunk>, ExecError> {
            Ok(Some(Chunk::Stdout(s.to_string())))
        }
        fn err(s: &str) -> Result<Option<Chunk>, ExecError> {
            Ok(Some(Chunk::Stderr(s.to_string())))
        }
        fn eof() -> Result<Option<Chunk>, ExecError> {
            Ok(None)
        }
    }

    impl ExecChannel for FakeChannel {
        async fn read(&mut self) -> Result<Option<Chunk>, ExecError> {
            let r = self
                .chunks
                .get(self.idx)
                .cloned()
                // 读完了还在读 ⇒ EOF。真实通道也是这个行为。
                .unwrap_or_else(FakeChannel::eof);
            self.idx += 1;
            r
        }
        fn exit_status(&self) -> Option<i32> {
            self.status
        }
        async fn kill(&mut self) {
            self.killed += 1;
            self.teardown.push("kill");
        }
        async fn close(&mut self) {
            self.closed += 1;
            self.teardown.push("close");
        }
    }

    /// 可编程时钟：每次问都往前走一格。`step = 0` 表示时间不走（永不超时）。
    struct FakeClock {
        secs: std::cell::Cell<u64>,
        step: u64,
    }
    impl FakeClock {
        fn frozen() -> Self {
            Self {
                secs: std::cell::Cell::new(0),
                step: 0,
            }
        }
        fn ticking(step: u64) -> Self {
            Self {
                secs: std::cell::Cell::new(0),
                step,
            }
        }
    }
    impl Clock for FakeClock {
        fn elapsed_secs(&self) -> u64 {
            let v = self.secs.get();
            self.secs.set(v + self.step);
            v
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        // 这几条测试里没有真正的 IO，future 一次 poll 就完成——用一个最小的
        // 执行器而不是拉 tokio 进来（本 crate 的单测不该依赖运行时的调度细节）。
        use std::task::{Context, Poll, Waker};
        let mut f = Box::pin(f);
        let mut cx = Context::from_waker(Waker::noop());
        // 一次 poll，不循环。写成 loop 会被 clippy 指出「永不循环」——它是对的：
        // Pending 那一支 panic，所以循环体只可能走一次。
        // 而 panic 而不是重试是刻意的：假通道不做真 IO，出现 Pending 说明
        // 被测代码里混进了真的等待，那时静静重试会把测试变成一个忙等死循环。
        match f.as_mut().poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("这些用例里不该出现 Pending——假通道不做真 IO"),
        }
    }

    #[test]
    fn a_normal_run_collects_both_streams_and_the_exit_code() {
        let mut ch = FakeChannel::new(
            vec![
                FakeChannel::out("line1\n"),
                FakeChannel::err("warn\n"),
                FakeChannel::out("line2\n"),
                FakeChannel::eof(),
            ],
            Some(0),
        );
        let o = block_on(run_command(
            &RunSpec::new("ls"),
            &mut ch,
            &FakeClock::frozen(),
        ));
        assert_eq!(o.stdout, "line1\nline2\n");
        assert_eq!(o.stderr, "warn\n");
        assert_eq!(o.exit_code, Some(0));
        assert!(!o.timed_out);
        assert!(!o.truncated);
    }

    #[test]
    fn a_nonzero_exit_code_comes_through() {
        let mut ch = FakeChannel::new(vec![FakeChannel::eof()], Some(127));
        let o = block_on(run_command(
            &RunSpec::new("nope"),
            &mut ch,
            &FakeClock::frozen(),
        ));
        assert_eq!(o.exit_code, Some(127));
    }

    /// **出口原文：通道回收无悬挂。** 三条路径的出口都必须过 close()。
    #[test]
    fn the_channel_is_always_closed_on_every_path() {
        // ① 正常结束
        let mut ch = FakeChannel::new(vec![FakeChannel::eof()], Some(0));
        block_on(run_command(
            &RunSpec::new("ls"),
            &mut ch,
            &FakeClock::frozen(),
        ));
        assert_eq!(ch.closed, 1, "正常路径没关通道");
        assert_eq!(ch.killed, 0, "正常结束不该杀进程");
        assert_eq!(ch.teardown, vec!["close"], "正常路径只该有一次 close");

        // ② 超时
        let mut ch = FakeChannel::new(vec![FakeChannel::out("partial")], Some(0));
        // timeout=1 而不是 2：时钟每问一次走 1 秒，而 chunks 只有一个——
        // timeout=2 时第二轮就读到 EOF 了，走的是正常结束而非超时路径。
        // 这个时序第一次写错过，测试红了才发现「我以为在测超时，其实在测 EOF」。
        let spec = RunSpec {
            timeout_secs: 1,
            ..RunSpec::new("sleep 999")
        };
        block_on(run_command(&spec, &mut ch, &FakeClock::ticking(1)));
        assert_eq!(ch.closed, 1, "超时路径没关通道——这就是「悬挂」");
        assert_eq!(
            ch.killed, 1,
            "超时必须先杀远端进程：只 close 会留下一个仍在跑的进程"
        );
        // **顺序**，不只是次数。两个计数对「kill 与 close 对调」完全不敏感——
        // 交叉审计用变异实证过：对调之后 182 条测试全绿。而对调是有真实后果的：
        // 通道一旦 close，kill 就打在空处，等于没杀。
        assert_eq!(
            ch.teardown,
            vec!["kill", "close"],
            "超时路径必须**先杀后关**：close 之后再 kill 等于没杀"
        );

        // ③ 读出错
        let mut ch = FakeChannel::new(
            vec![
                FakeChannel::out("a"),
                Err(ExecError::Channel("EOF 之前断了".into())),
            ],
            Some(0),
        );
        block_on(run_command(
            &RunSpec::new("x"),
            &mut ch,
            &FakeClock::frozen(),
        ));
        assert_eq!(ch.closed, 1, "出错路径没关通道");
        assert_eq!(
            ch.teardown,
            vec!["close"],
            "读出错不是超时，不该杀进程——远端可能已经正常结束了"
        );
    }

    /// 超时的观察值：`timed_out: true` + partial_output + **没有**退出码。
    #[test]
    fn a_timeout_keeps_partial_output_and_reports_no_exit_code() {
        let mut ch = FakeChannel::new(
            vec![
                FakeChannel::out("已经收到的这些\n"),
                FakeChannel::out("还有这些\n"),
            ],
            Some(0), // 通道声称有退出码，但超时了就不该用它
        );
        // timeout=2：读完两个 chunk 恰好耗到 2 秒，第三轮判超时。
        // 给 3 的话第三轮读到 EOF，测的就是正常结束了。
        let spec = RunSpec {
            timeout_secs: 2,
            ..RunSpec::new("tail -f /var/log/x")
        };
        let o = block_on(run_command(&spec, &mut ch, &FakeClock::ticking(1)));
        assert!(o.timed_out);
        // 契约明文：partial_output。丢掉它等于让 Agent 少一次判断依据，
        // 而它下一步很可能就是重跑同一条命令。
        assert!(
            o.stdout.contains("已经收到的这些"),
            "超时丢掉了已收到的输出"
        );
        // 通道有 exit_status，但超时时不能用——「被杀掉」和「自己退出了」不是一回事。
        assert_eq!(o.exit_code, None, "超时却报了退出码");
    }

    /// 读出错与超时是**两种**结局，不得混为一谈。
    #[test]
    fn a_channel_error_is_not_a_timeout() {
        let mut ch = FakeChannel::new(vec![Err(ExecError::Channel("连接被重置".into()))], Some(0));
        let o = block_on(run_command(
            &RunSpec::new("ls"),
            &mut ch,
            &FakeClock::frozen(),
        ));
        // Agent 对这两种的下一步不同：超时可以改用后台模式重试，
        // 通道错误重试同一条命令多半还是错。
        assert!(!o.timed_out, "通道错误被报成了超时");
        assert_eq!(o.exit_code, None);
        assert!(
            o.stderr.contains("通道错误"),
            "错误原因没交回去：{}",
            o.stderr
        );
        assert!(o.stderr.contains("连接被重置"));
    }

    /// 两个流各自受同一个上限，不共享总额。
    #[test]
    fn stdout_and_stderr_get_independent_budgets() {
        // 一个话多的 stderr 不该把 stdout 挤没——而 stdout 通常是我们真正要的那份。
        let spec = RunSpec {
            max_output_bytes: 100,
            ..RunSpec::new("noisy")
        };
        let mut ch = FakeChannel::new(
            vec![
                FakeChannel::out(&"O".repeat(80)),
                FakeChannel::err(&"E".repeat(500)),
                FakeChannel::eof(),
            ],
            Some(0),
        );
        let o = block_on(run_command(&spec, &mut ch, &FakeClock::frozen()));
        // stdout 只有 80 字节，在上限内 ⇒ 一个字节都不该少
        assert_eq!(o.stdout.len(), 80, "stdout 被 stderr 挤掉了");
        assert!(!o.stdout.contains('…'), "stdout 不该被截");
        // stderr 超了 ⇒ 截，但它必须拿到**自己那份完整预算**。
        //
        // 只断言 `o.stderr.len() <= 100` 是不够的——把 stderr 的预算改成
        // `max - stdout.len()`（即两个流共享总额）时那条仍然成立（20 ≤ 100），
        // 变异幸存。而共享总额的后果正是这个函数要避免的：一个话多的 stderr
        // 会把 stdout 挤没，反过来一个话多的 stdout 会让 stderr 只剩几十字节，
        // 而错误信息恰好总在 stderr 里。
        assert!(o.stderr.len() <= 100);
        assert!(
            o.stderr.len() > 50,
            "stderr 只拿到 {} 字节——它的预算被 stdout 占掉了（两个流共享了总额）",
            o.stderr.len()
        );
        assert!(o.truncated, "有一个流被截了就该上报");
    }

    /// 输出远超上限时提早停，但**仍然要拿到退出码**（如果已经 EOF）。
    #[test]
    fn a_flood_of_output_stops_early_but_stays_truncated_not_lost() {
        let spec = RunSpec {
            max_output_bytes: 50,
            ..RunSpec::new("cat big")
        };
        let chunks: Vec<_> = (0..100)
            .map(|_| FakeChannel::out(&"x".repeat(10)))
            .collect();
        let mut ch = FakeChannel::new(chunks, Some(0));
        let o = block_on(run_command(&spec, &mut ch, &FakeClock::frozen()));
        assert!(o.truncated);
        assert!(o.stdout.len() <= 50);
        assert!(!o.timed_out, "提早停不是超时");
        // 提早停时没走到 EOF，所以没有退出码可拿——但它**没有**被报成超时，
        // 上一条断言已经钉住这点。这里再钉一次通道仍然关掉了。
        assert_eq!(ch.closed, 1);
    }

    #[test]
    fn short_output_is_untouched() {
        let r = truncate_output("hello", 100);
        assert_eq!(r.text, "hello");
        assert!(!r.truncated);
        // 边界：正好等于上限不截
        let r = truncate_output("12345", 5);
        assert_eq!(r.text, "12345");
        assert!(!r.truncated);
    }

    #[test]
    fn truncation_keeps_head_and_tail_with_a_marker() {
        let s = "A".repeat(1000);
        let r = truncate_output(&s, 200);
        assert!(r.truncated);
        assert!(r.text.starts_with("AAAA"), "头部没保留");
        assert!(r.text.ends_with("AAAA"), "尾部没保留");
        assert!(r.text.contains("bytes truncated"), "没有截断标记");
    }

    /// **标记本身占预算。** 不占的话「上限 32 KiB」就不成立。
    #[test]
    fn the_marker_counts_against_the_budget() {
        for max in [80usize, 200, 1024, 32 * 1024] {
            let s = "x".repeat(max * 3);
            let r = truncate_output(&s, max);
            assert!(
                r.text.len() <= max,
                "max={max} 时结果 {} 字节，超了上限——标记没算进预算",
                r.text.len()
            );
        }
    }

    /// 标记里的数字必须是**真的**被删掉的字节数。
    #[test]
    fn the_marker_reports_the_real_number_of_dropped_bytes() {
        let s = "y".repeat(5000);
        let r = truncate_output(&s, 500);
        // 从标记里把数字抠出来，与「原长 - 保留长」对比
        let n: usize = r
            .text
            .split("[…")
            .nth(1)
            .and_then(|t| t.split(' ').next())
            .and_then(|t| t.parse().ok())
            .expect("标记里没有数字");
        let kept = r.text.len() - (TRUNCATION_NOTE.len() - 3 + digits(n));
        assert_eq!(
            n,
            s.len() - kept,
            "标记说删了 {n} 字节，实际保留 {kept} / 原长 {}",
            s.len()
        );
    }

    /// 头略多于尾（预算除不尽时）。
    ///
    /// 第一版只断言 `h >= t && h - t <= 1`，而把 `head = budget - budget/2` 改成
    /// `head = budget/2`（对半分、余下那个字节被丢掉）时**两条都还成立**——
    /// 变异幸存。所以这里改成扫一段连续的 `max`：其中必有若干让预算为奇数，
    /// 那时头必须多**恰好一个**。
    ///
    /// 不直接算「哪个 max 会让预算为奇数」是刻意的：那要求测试重算一遍
    /// `note_len`，也就是把实现抄一遍——而抄错的那一天，测试会跟着实现一起错。
    #[test]
    fn the_head_gets_the_extra_byte() {
        // 用可区分的头尾找出切点
        let s = format!("{}{}", "H".repeat(500), "T".repeat(500));
        let mut saw_extra = false;
        for max in 200..212 {
            let r = truncate_output(&s, max);
            let h = r.text.chars().take_while(|c| *c == 'H').count();
            let t = r.text.chars().rev().take_while(|c| *c == 'T').count();
            assert!(h >= t, "max={max}：头 {h} 应不少于尾 {t}——开头通常更重要");
            assert!(
                h - t <= 1,
                "max={max}：头尾只该差至多一个字节，实测 {h} vs {t}"
            );
            if h == t + 1 {
                saw_extra = true;
            }
        }
        assert!(
            saw_extra,
            "连续 12 个 max 里没有一个让头多拿那一字节——\
             预算除不尽时余下的那个字节被丢掉了（`head = budget - budget/2` 被改成了对半分）"
        );
    }

    /// **在字符边界切。** 按字节切会把多字节字符劈开。
    #[test]
    fn multibyte_characters_are_never_split() {
        // 中文，每字 3 字节
        let s = "测".repeat(1000);
        for max in 60..400 {
            let r = truncate_output(&s, max);
            // 结果必须是合法 UTF-8（String 本身保证）且不含替换字符
            assert!(
                !r.text.contains('\u{FFFD}'),
                "max={max} 时出现了替换字符——字符被劈开了"
            );
            assert!(r.text.len() <= max, "max={max} 时超了上限");
        }
    }

    /// 上限小到装不下标记：只留头部，但仍如实上报 truncated。
    #[test]
    fn a_budget_too_small_for_the_marker_degrades_to_head_only() {
        let s = "z".repeat(1000);
        let r = truncate_output(&s, 10);
        assert!(r.truncated, "截了就必须上报，哪怕没地方放标记");
        assert_eq!(r.text.len(), 10);
        assert!(!r.text.contains("truncated"), "装不下就不该硬塞标记");
    }

    #[test]
    fn wrap_uses_semicolon_not_and_so_failures_still_report() {
        let w = wrap_for_terminal("false");
        // `&&` 会让命令失败时 printf 不执行 → 哨兵永不出现 → 只能等超时。
        // 那个错误很隐蔽：成功路径全正常，只有失败时表现为「卡 60 秒」。
        assert!(w.contains("; printf"), "必须用 `;` 而不是 `&&`");
        assert!(!w.contains("&& printf"));
        assert!(w.contains(SENTINEL_PREFIX));
        assert!(w.ends_with('\n'), "要有换行，否则 shell 不会执行这一行");
    }

    #[test]
    fn sentinel_parses_both_success_and_failure() {
        let (out, code) = parse_sentinel("hello\n__FS_RC__=0\n").expect("该解析出来");
        assert_eq!(out, "hello\n");
        assert_eq!(code, 0);

        let (out, code) = parse_sentinel("boom\n__FS_RC__=127\n").expect("该解析出来");
        assert_eq!(out, "boom\n");
        assert_eq!(code, 127);
    }

    /// PTY 常见的 `\r\n` 行尾。
    #[test]
    fn sentinel_handles_crlf() {
        let (out, code) = parse_sentinel("a\r\n__FS_RC__=3\r\n").expect("该解析出来");
        assert_eq!(out, "a\r\n");
        assert_eq!(code, 3);
    }

    /// **找最后一个哨兵。** 上一次执行留下的哨兵不能被当成这一次的。
    #[test]
    fn the_last_sentinel_wins_not_the_first() {
        let text = "first run\n__FS_RC__=0\nsecond run\n__FS_RC__=1\n";
        let (out, code) = parse_sentinel(text).expect("该解析出来");
        assert_eq!(code, 1, "读到了上一次执行的退出码——那是个静默的错误答案");
        assert!(out.contains("second run"));
    }

    /// 没有哨兵返回 None，**不猜 0**。
    #[test]
    fn a_missing_sentinel_is_none_not_zero() {
        assert_eq!(parse_sentinel("still running…\n"), None);
        assert_eq!(parse_sentinel(""), None);
        // 前缀在但值不是数字（比如回显的格式串 `%d`）
        assert_eq!(parse_sentinel("__FS_RC__=%d\n"), None);
        assert_eq!(parse_sentinel("__FS_RC__=abc\n"), None);
        // 前缀在但还没到行尾就断了（帧边界）——宁缺毋假
        assert_eq!(parse_sentinel("__FS_RC__="), None);
    }

    /// 命令回显要剥掉，否则 parse_sentinel 会在回显里找到前缀。
    #[test]
    fn the_command_echo_line_is_stripped() {
        let cmd = "df -h";
        let raw = format!(
            "{}\ndf output line\n__FS_RC__=0\n",
            wrap_for_terminal(cmd).trim_end()
        );
        // 不剥的话：rfind 找到的是真哨兵（它在后面），所以能解析对；
        // 但观察值的第一行是我们自己的包装代码，模型会把它当命令输出解释。
        let clean = strip_command_echo(&raw, cmd);
        assert!(!clean.contains("printf"), "回显没剥掉：{clean}");
        assert!(clean.contains("df output line"), "把正文也剥掉了：{clean}");
        assert!(clean.contains("__FS_RC__=0"), "真哨兵行被剥掉了：{clean}");
        let (out, code) = parse_sentinel(&clean).expect("剥完还要能解析");
        assert_eq!(code, 0);
        assert_eq!(out.trim(), "df output line");
    }

    /// 回显在**前**、真哨兵在后时，剥掉回显不能顺手把真哨兵也剥掉。
    #[test]
    fn stripping_the_echo_keeps_the_real_sentinel_line() {
        // 只按哨兵前缀判会把两行都剥掉，那时 parse_sentinel 返回 None,
        // 表现为「命令跑完了却一直等到超时」。
        let cmd = "echo hi";
        let raw = format!("{}; printf '__FS_RC__=%d\\n' $?\nhi\n__FS_RC__=0\n", cmd);
        let clean = strip_command_echo(&raw, cmd);
        assert!(parse_sentinel(&clean).is_some(), "真哨兵被剥掉了：{clean}");
    }

    #[test]
    fn defaults_match_the_spec() {
        // 总设计 §4.4 逐字：60 秒、32 KiB。改这两个数要连带改文档。
        assert_eq!(DEFAULT_TIMEOUT_SECS, 60);
        assert_eq!(DEFAULT_MAX_OUTPUT_BYTES, 32 * 1024);
        let s = RunSpec::new("ls");
        assert_eq!(s.timeout_secs, 60);
        assert_eq!(s.max_output_bytes, 32 * 1024);
        assert_eq!(s.mode, ExecMode::Exec, "默认是独立 exec 通道");
    }

    /// 超时的观察值：`timed_out: true` + 已收到的部分输出 + **没有**退出码。
    #[test]
    fn a_timed_out_observation_has_no_exit_code() {
        let o = Observation {
            stdout: "partial…".to_string(),
            timed_out: true,
            exit_code: None,
            ..Default::default()
        };
        // 这条钉的是「不用 -1 之类的哨兵值」这个选择：-1 会被下游当成
        // 一个真的退出码去解释，而「超时所以没有退出码」和「返回了 255」
        // 是完全不同的两件事。
        assert!(o.exit_code.is_none());
        assert!(!o.stdout.is_empty(), "超时也要交回已收到的部分");
    }

    /// 观察值的 serde 形状（MCP 那边要按这个形状回工具结果）。
    #[test]
    fn observation_serializes_with_the_contract_field_names() {
        let json = serde_json::to_value(Observation {
            stdout: "o".into(),
            stderr: "e".into(),
            exit_code: Some(2),
            truncated: true,
            timed_out: false,
        })
        .expect("序列化");
        for k in ["stdout", "stderr", "exit_code", "truncated", "timed_out"] {
            assert!(json.get(k).is_some(), "缺字段 {k}");
        }
        assert_eq!(json["exit_code"], 2);
    }

    #[test]
    fn exec_mode_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&ExecMode::CurrentTerminal).unwrap(),
            "\"current_terminal\""
        );
    }
}
