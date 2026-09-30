//! 传后校验执行体（路线图 M1 出口 / UI 规格 §1.4 / 总设计 §2.3）。
//!
//! 语义（钉死）：
//! - Up：传前本地文件哈希为期望 → 传后 exec 远端 `sha256sum` 比对；
//! - Down：传前 exec 远端哈希为期望（app 层经 `remote_sha256_via` 预取，失败置 degraded）→ 传后本地文件哈希比对；
//! - exec 命令缺失（126/127/not found）→ 降级 `SftpOps::stat_size` 对比 → `SizeOnlyMatch`；
//! - 双路径失败 → `Unverified`。禁止静默跳过：四种 outcome 一律经 app 层 `transfer_verified` 事件上报 UI。
//!
//! 本模块 app 层无关：`ExecChannel` 由调用方注入（app 层 over LiveSession.handle，见 Task 21；
//! itest over 容器会话，见 Task 12 Step 6），使整条校验链路单测与容器 itest 可覆盖。
use crate::sftp::SftpOps;
use crate::transfer::{Direction, Sha256Hex};
use async_trait::async_trait;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// 对象安全的一次性命令通道（不占用 PTY——总设计 §2.1 双通道）。
/// 返回 [`ExecOutput`]；`code = None` = 未观测到退出码（超时/通道异常），不是 -1。
#[async_trait]
pub trait ExecChannel: Send + Sync {
    async fn exec_once(&self, cmd: &str) -> Result<ExecOutput, crate::Error>;
}

/// EOF 之后的尾部宽限窗口：见 `run_exec_channel`。
const EXEC_TAIL_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// `run_exec_channel` 的输入：一条会吐出 `ChannelMsg` 的通道。
///
/// 抽这一层**只**为了让 `run_exec_channel` 可测。它此前的入参是具体的
/// `russh::Channel<client::Msg>`，而那个类型在 crate 外没有任何构造途径——于是这个函数
/// 在整个仓库里连一个单测都挂不上。代价是有过账的：S41（在 `Eof` 处 break 导致退出码恒为 -1、
/// SHA256 校验在真机上从未执行）就藏在这里，逃过了全部单测，只有连真服务器的集成测试才撞见。
/// 现在两道时间边界（尾部宽限、总预算）也长在同一个函数里，再让它不可测就是明知故犯：
/// 审计2 #11 要的是「**可验证**的超时和退出语义」，不可测的超时算不上已验证。
#[async_trait]
pub trait ChannelMsgs: Send {
    async fn next_msg(&mut self) -> Option<russh::ChannelMsg>;
}

#[async_trait]
impl ChannelMsgs for russh::Channel<russh::client::Msg> {
    async fn next_msg(&mut self) -> Option<russh::ChannelMsg> {
        self.wait().await
    }
}

/// 一次性 exec 通道收集助手：stdout / stderr（ext=1）/ ExitStatus。
///
/// **绝不可在 `Eof` 上退出循环。** RFC 4254 §5.3 规定 `exit-status` 在通道关闭*之前*送达，
/// 而 OpenSSH 的实际发送序是 `data…` → `SSH_MSG_CHANNEL_EOF` → `exit-status` →
/// `SSH_MSG_CHANNEL_CLOSE`：EOF 只表示数据流结束，退出码尚未发出。在 Eof 处 break 会把
/// 随后的 exit-status 整条丢掉，`code` 对任何真实 OpenSSH 恒为 -1 —— 于是 `run_verify`
/// 的 Up 路径永远落进降级分支，SHA256 校验在真机上从未真正执行过（发现 S41）。
/// 该缺陷躲过了全部单测，因为单测注入的是脚本化 `ExecChannel`，压根不经过本函数。
///
/// 退出条件因此只有 `Close` 与流结束（`wait()` 返回 `None`）。为不退化成挂死
/// （Task 9 第二裁判 S27 同类），设两道时间边界：
///
/// - **EOF 之后**的尾部宽限窗口 [`EXEC_TAIL_GRACE`]：EOF 之后服务器只剩 exit-status
///   与 close 两条消息要发，迟迟不发即判异常并按已收到的内容返回。
/// - **整条命令**的总预算 [`crate::timeouts::EXEC_TOTAL_TIMEOUT`]（审计2 #11）。这一道
///   原先是没有的，理由是「`sha256sum` 一个大文件可达数分钟」——理由成立，但推不出「可以
///   永远等」。命令本体跑起来之后通道上一个字节都不会有（`sha256sum` 读完整个文件才输出
///   一行），所以「首字节」和「空闲」都不能用作判据，只有总时长可用；不设总时长的实际后果
///   是一个卡死的服务端进程能把调用方永久占住，而调用方（`AppState::exec_adapter_for`
///   一路上来）是持着会话锁的。
///
/// 超时不返回错误、也不清空已收到的内容，而是原样返回 `code = -1` 与部分输出——`run_verify`
/// 的 Up 分支与 `transfer::precommit_gate` 第③关都把它当作**证据缺失**而非不符，于是一次
/// 慢哈希的后果是退到降级校验，不是把一个好文件判死。
/// stdout/stderr 的接收上限（字节）。超限继续丢端、不再累积。
///
/// 计划任务（M4a）会经这条通道跑**任意用户命令**——`cat 一个大日志` 就能产出几百
/// 兆。此前无界 `extend_from_slice` 会把它们全部攒在内存里，末尾的
/// `from_utf8_lossy` 还要再整份复制一次；调用方（历史记录、UI）的「落库前截断」
/// 完全不缓解接收期的峰值。既有消费方（校验的 sha256 行、监控的几行 echo、进程表）
/// 输出都在几 KiB 量级，1 MiB 是它们的一千倍余量。
pub const EXEC_OUT_CAP: usize = 1024 * 1024;

/// `run_exec_channel` 的接收结果。`code = None` 表示**没拿到退出码**——
/// 超时与通道异常都属于这一类，它们与「退出码是某个真实值」是两回事。
///
/// 此前用 `-1` 同时表示超时与缺 ExitStatus；调用方一旦把它当退出码存下，shell 语境
/// 里它读作 255——把「证据缺失」冒充成「真实观测值」正是本仓「空态不撒谎」口径
/// 要消灭的形状。新增消费方一律判 `None` 分支，不要比较 `-1`。
#[derive(Debug)]
pub struct ExecOutput {
    /// `None` = 未观测到退出码（超时/通道异常）
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub async fn run_exec_channel<C: ChannelMsgs + ?Sized>(channel: &mut C) -> ExecOutput {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut out_capped = false;
    let mut err_capped = false;
    let mut code = -1;
    let mut code_seen = false;
    let mut tail_deadline: Option<tokio::time::Instant> = None;
    let total_deadline = tokio::time::Instant::now() + crate::timeouts::EXEC_TOTAL_TIMEOUT;
    loop {
        // 两道边界取更早的那个：尾部宽限一旦开启必然早于总预算，否则总预算兜底。
        let deadline = match tail_deadline {
            Some(d) => d.min(total_deadline),
            None => total_deadline,
        };
        let next = match tokio::time::timeout_at(deadline, channel.next_msg()).await {
            Ok(m) => m,
            Err(_) => break,
        };
        let Some(msg) = next else { break };
        match msg {
            // 封顶后继续读（丢端），直到拿到退出码：截断输出不能变成截断收尾
            russh::ChannelMsg::Data { data } => {
                if out.len() < EXEC_OUT_CAP {
                    let take = (EXEC_OUT_CAP - out.len()).min(data.len());
                    out.extend_from_slice(&data[..take]);
                    if data.len() > take {
                        out_capped = true;
                    }
                } else {
                    out_capped = true;
                }
            }
            russh::ChannelMsg::ExtendedData { ext, data } => {
                if ext == 1 {
                    if err.len() < EXEC_OUT_CAP {
                        let take = (EXEC_OUT_CAP - err.len()).min(data.len());
                        err.extend_from_slice(&data[..take]);
                        if data.len() > take {
                            err_capped = true;
                        }
                    } else {
                        err_capped = true;
                    }
                }
            }
            russh::ChannelMsg::ExitStatus { exit_status } => {
                code = exit_status as i32;
                code_seen = true;
            }
            russh::ChannelMsg::Eof => {
                tail_deadline = Some(tokio::time::Instant::now() + EXEC_TAIL_GRACE)
            }
            russh::ChannelMsg::Close => break,
            _ => {}
        }
    }
    let mut stdout = String::from_utf8_lossy(&out).into_owned();
    let mut stderr = String::from_utf8_lossy(&err).into_owned();
    if out_capped {
        stdout.push_str("\n…（输出超过 1 MiB，已截断）");
    }
    if err_capped {
        stderr.push_str("\n…（stderr 超过 1 MiB，已截断）");
    }
    ExecOutput {
        code: if code_seen { Some(code) } else { None },
        stdout,
        stderr,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyOutcome {
    /// sha256 逐字比对一致
    Sha256Match,
    /// 命令缺失降级：大小一致（服务端无 sha256sum——UI 须显式标注「降级」）
    SizeOnlyMatch,
    /// 校验失败（哈希或大小不一致）——红色告警、保留本件
    Mismatch,
    /// 双路径失败（exec 与 stat 皆不可用）——显式标注，禁止静默跳过
    Unverified,
}

#[derive(Debug, Clone)]
pub struct VerifyPlan {
    /// 期望内容哈希：Up = 传前本地哈希；Down = 传前远端哈希。None = 预取失败（仅 Down 降级路径）。
    pub expect: Option<Sha256Hex>,
    /// 传前预取哈希的 exec 已失败（Down）：完成时跳过哈希路径、直接 size 对比。
    pub degraded: bool,
}

#[derive(Debug, Clone)]
pub struct VerifyEntry {
    pub direction: Direction,
    pub remote: String,
    pub local: PathBuf,
    pub plan: VerifyPlan,
}

/// 流式哈希（1 MiB 分块；传输校验面向大文件，禁止全量读入）。
pub async fn file_sha256(path: &Path) -> std::io::Result<Sha256Hex> {
    use tokio::io::AsyncReadExt;
    let mut hasher = Sha256::new();
    let mut f = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(Sha256Hex::new(format!("{:x}", hasher.finalize())).expect("sha2 输出恒为 64 位小写 hex"))
}

/// 解析 `sha256sum` 输出首 token（64 位小写 hex；coreutils/busybox 均为 `<hex>  <name>` 格式）。
pub fn parse_sha256sum(out: &str) -> Option<Sha256Hex> {
    out.split_whitespace()
        .next()
        .and_then(|t| Sha256Hex::new(t).ok())
}

/// 命令缺失判定：退出码 126/127，或 stderr 常见 not-found 文案（sh/bash/dash 通用措辞）。
///
/// 本 crate 内**故意**无调用方：`run_verify` 把「命令缺失」与「命令失败」合并成同一个降级
/// 动作（见下方 `Ok(_) | Err(_)` 分支），二者只在**措辞**上有别。留作 app 层 API——
/// Task 21 用它把降级理由从「服务端没有 sha256sum」与「sha256sum 执行失败」区分开写进
/// UI 文案与审计日志（`VerifyOutcome` 是纯枚举、不带理由字段，理由只能在调用方组装）。
/// 勿因「无引用」删除（发现 S48：曾被判为死代码）。
pub fn exec_unavailable(exit_code: i32, stderr: &str) -> bool {
    if matches!(exit_code, 126 | 127) {
        return true;
    }
    let s = stderr.to_ascii_lowercase();
    s.contains("not found") || s.contains("no such file")
}

/// 保守单引号转义：远端路径为服务端可控输入——防命令注入（失败闭合成因之一，总设计 §4.3）。
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 预取远端 sha256（Down 方向传前期望值）；失败由调用方决策（app 层置 degraded）。
pub async fn remote_sha256_via(
    exec: &dyn ExecChannel,
    remote: &str,
) -> Result<Sha256Hex, crate::Error> {
    let o = exec
        .exec_once(&format!("sha256sum -- {}", shell_quote(remote)))
        .await?;
    // code=None（超时/无退出码）与非零一样是失败，且要把「没拿到退出码」说清楚
    match o.code {
        Some(0) => parse_sha256sum(&o.stdout).ok_or_else(|| {
            crate::Error::Transfer(format!("unparsable sha256sum output: {:?}", o.stdout))
        }),
        Some(c) => Err(crate::Error::Transfer(format!(
            "remote sha256sum exit {c}: {}",
            o.stderr
        ))),
        None => Err(crate::Error::Transfer(
            "remote sha256sum 未返回退出码（超时或通道异常）".into(),
        )),
    }
}

/// 校验编排器：传输完成（Done）后调用；返回值即必须上报 UI 的结局。
///
/// **信任边界（勿高估本函数的保证——S49）**：Up 方向的哈希由**服务端**算出，因此它证明的是
/// 「传输链路未损坏」，而非「服务端诚实」。恶意服务端既能让 `sha256sum` 非零退出把校验压到
/// size 降级路径，也能直接伪造哈希与 size——两者难度相同，所以这里不为前者加固：任何
/// 服务端侧计算的校验都只对**意外损坏**有效。降级本身不被隐藏才是真正的保证：`SizeOnlyMatch`
/// 是独立结局而非 `Sha256Match` 的近义词，UI 必须显式标注「降级（仅比对大小）」，
/// 使「服务端拒绝算哈希」对用户可见。Down 方向不受此限——哈希在本地算。
pub async fn run_verify(
    exec: &dyn ExecChannel,
    ops: &dyn SftpOps,
    entry: &VerifyEntry,
) -> VerifyOutcome {
    match entry.direction {
        Direction::Up => {
            let cmd = format!("sha256sum -- {}", shell_quote(&entry.remote));
            match exec.exec_once(&cmd).await {
                Ok(o) if o.code == Some(0) => match parse_sha256sum(&o.stdout) {
                    Some(h) => {
                        if entry.plan.expect.as_ref() == Some(&h) {
                            VerifyOutcome::Sha256Match
                        } else {
                            VerifyOutcome::Mismatch
                        }
                    }
                    None => VerifyOutcome::Unverified,
                },
                // 其他非零退出与通道级失败一律降级 size 对比（禁静默跳过）；
                // 是否「命令缺失」只影响诊断措辞，不影响降级动作，故合并为一支。
                Ok(_) | Err(_) => size_compare(ops, entry).await,
            }
        }
        Direction::Down => {
            if entry.plan.degraded || entry.plan.expect.is_none() {
                size_compare(ops, entry).await
            } else {
                match file_sha256(&entry.local).await {
                    Ok(h) => {
                        if entry.plan.expect.as_ref() == Some(&h) {
                            VerifyOutcome::Sha256Match
                        } else {
                            VerifyOutcome::Mismatch
                        }
                    }
                    Err(_) => VerifyOutcome::Unverified,
                }
            }
        }
    }
}

/// size 对比降级路径：双端大小可取且一致 → SizeOnlyMatch；任一不可取 → Unverified。
async fn size_compare(ops: &dyn SftpOps, entry: &VerifyEntry) -> VerifyOutcome {
    let remote = match ops.stat_size(&entry.remote).await {
        Ok(s) => s,
        Err(_) => return VerifyOutcome::Unverified,
    };
    let local = match tokio::fs::metadata(&entry.local).await {
        Ok(m) => m.len(),
        Err(_) => return VerifyOutcome::Unverified,
    };
    if remote == local {
        VerifyOutcome::SizeOnlyMatch
    } else {
        VerifyOutcome::Mismatch
    }
}
