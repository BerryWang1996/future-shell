//! RDP helper 进程。
//!
//! 见 `crates/rdpproto/src/lib.rs` 模块头：这个进程存在的理由是 IronRDP 与
//! russh 的依赖树不能相遇，以及——更重要的——**把解析远端不可信字节的那一侧
//! 关进一个碰不到 Vault 的进程里**。
//!
//! # 它能做的事只有一件：在 stdin/stdout 上说 `fs_rdpproto`
//!
//! 没有网络（传输在主程序那侧，线上字节经管道进出）、不读配置文件、
//! 不写磁盘、不看环境变量拿凭据（口令经管道来）。
//!
//! # 两个阶段与 stdout 写权的移交
//!
//! ```text
//! 阶段 A（无会话）：主循环读 stdin、写 stdout（Hello 握手、协议错上报）
//! 阶段 B（Connect 之后）：
//!   主循环（本任务）──读 stdin──► NetIn/NetEof→PipeHandle；Input/裁决→cmd 通道
//!   转发任务（新起）──独占 stdout──┬─引擎上行（Frame/Cert/…）→ 帧
//!                                 └─pipe 待发字节（NetOut）→ 帧
//! ```
//!
//! stdout 写权在 Connect 那一刻**整体移交**给转发任务：帧流不容两个写者交错，
//! 而「谁写 stdout」这件事用一个所有权转移来表达，比用锁或约定都硬。
//! 主循环不再碰 stdout，借位冲突也随之消失（select! 的各分支不再同时借会话）。
//!
//! # 它是被 spawn 的，不是给人用的
//!
//! 没有命令行参数、没有 `--help`。直接在终端里跑它只会得到一句提示然后退出——
//! 因为 stdin 不是一条帧流。**那句提示走 stderr**：stdout 是协议通道，
//! 往里写一个非协议字节就会把帧流冲垮，而主程序那侧看到的是一个莫名其妙的
//! `BadHeader`，离真因很远。

mod audio;
mod clipboard;
mod drive;
mod engine;
mod fb;
mod pipe;
mod wire;

use engine::{EngineCmd, ToMain};
use fs_rdpproto::{FromHelper, ToHelper, PROTOCOL_VERSION};
use pipe::{pipe, PipeHandle};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Notify};
use wire::{FramedReader, FramedWriter, ReadError};

fn main() {
    // 日志钉死到 stderr。**这不是风格选择**：stdout 被协议占着。
    tracing_to_stderr();

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("fs-rdp-helper: 无法建立运行时：{e}");
            std::process::exit(2);
        }
    };
    let code = rt.block_on(run());
    std::process::exit(code);
}

/// 极简日志装配：本进程不引 `tracing-subscriber`（多一棵依赖树、多一份
/// 出站面审计），`tracing` 的事件在没有 subscriber 时是空操作。
/// 需要留痕的地方直接写 stderr——本进程的日志量本来就只有握手与失败几行。
fn tracing_to_stderr() {}

async fn run() -> i32 {
    let mut rx = FramedReader::new(tokio::io::stdin());
    let mut tx = FramedWriter::new(tokio::io::stdout());

    // ── 握手：**必须是第一条** ──────────────────────────────────────────
    //
    // 版本对不上就立刻退出，不进事件循环。helper 是随安装包一起发的，
    // 正常永远同版本；对不上意味着用户手工替换过二进制、或安装包升级到一半
    // 失败了。那时给一句准确的「版本不匹配」，比让一个陌生的解析错误从
    // 半小时后的某个 RDP PDU 里冒出来强得多。
    match rx.next::<ToHelper>().await {
        Ok(pkt) => match pkt.header {
            ToHelper::Hello { version } if version == PROTOCOL_VERSION => {
                if tx
                    .send(
                        &FromHelper::Hello {
                            version: PROTOCOL_VERSION,
                        },
                        &[],
                    )
                    .await
                    .is_err()
                {
                    return 1;
                }
            }
            ToHelper::Hello { version } => {
                eprintln!(
                    "fs-rdp-helper: 协议版本不匹配（主程序 {version}，本 helper {PROTOCOL_VERSION}）"
                );
                return 3;
            }
            other => {
                eprintln!("fs-rdp-helper: 第一条消息必须是 Hello，实得 {other:?}");
                return 3;
            }
        },
        Err(ReadError::Eof) => {
            eprintln!(
                "fs-rdp-helper: 这个程序由 FutureShell 启动，\
                 经 stdin/stdout 说一套内部协议，不能直接在终端里运行。"
            );
            return 0;
        }
        Err(e) => {
            eprintln!("fs-rdp-helper: 握手失败：{e}");
            return 1;
        }
    }

    // ── 阶段 A：等 Connect（其余消息在此阶段都是协议错） ─────────────────
    let params = // 单轮循环体直接内联（clippy：loop 从不循环）
    {
        match rx.next::<ToHelper>().await {
            Ok(pkt) => match pkt.header {
                ToHelper::Connect(p) => p,
                ToHelper::Shutdown => return 0,
                other => {
                    let _ = tx
                        .send(
                            &FromHelper::Failed {
                                kind: fs_rdpproto::FailureKind::Internal,
                                message: format!("连接建立前不该收到 {other:?}"),
                            },
                            &[],
                        )
                        .await;
                    return 4;
                }
            },
            Err(ReadError::Eof) => return 0,
            Err(e) => {
                eprintln!("fs-rdp-helper: {e}");
                return 1;
            }
        }
    };

    // ── 阶段 B：引擎 + 转发任务，stdout 写权移交 ────────────────────────
    let (stream, handle) = pipe();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (verdict_tx, verdict_rx) = oneshot::channel();
    let (up_tx, up_rx) = mpsc::unbounded_channel::<ToMain>();
    let engine_done = Arc::new(Notify::new());
    let done_for_engine = engine_done.clone();

    let engine_task = tokio::spawn(engine::run(stream, params, cmd_rx, up_tx, verdict_rx));
    let forwarder = tokio::spawn(forward_to_stdout(
        tx,
        up_rx,
        handle.clone(),
        done_for_engine,
    ));

    // 主循环：只读 stdin。引擎退出（无论成败）经 engine_done 唤醒本循环收尾。
    let mut verdict_first = Some(verdict_tx);
    let mut stdin_gone = false;
    loop {
        tokio::select! {
            pkt = rx.next::<ToHelper>(), if !stdin_gone => {
                match pkt {
                    Ok(pkt) => match pkt.header {
                        ToHelper::NetIn => handle.push_net(pkt.body),
                        ToHelper::NetEof => handle.close(),
                        ToHelper::CertVerdict { accept } => {
                            // 首次裁决走 oneshot（引擎在 TLS 与 CredSSP 之间专程等它）；
                            // 之后的重复裁决原样转发，引擎侧记日志。
                            match verdict_first.take() {
                                Some(v) => { let _ = v.send(accept); }
                                None => { let _ = cmd_tx.send(EngineCmd::CertVerdict); }
                            }
                        }
                        ToHelper::Input(ev) => { let _ = cmd_tx.send(EngineCmd::Input(ev)); }
                        ToHelper::Resize { width, height } => {
                            let _ = cmd_tx.send(EngineCmd::Resize { width, height });
                        }
                        // 剪贴板（阶段 2）：三条都是纯转发，引擎侧才有 CLIPRDR 状态
                        ToHelper::ClipboardOffer => {
                            let _ = cmd_tx.send(EngineCmd::ClipboardOffer(pkt.body));
                        }
                        ToHelper::ClipboardData => {
                            let _ = cmd_tx.send(EngineCmd::ClipboardData(pkt.body));
                        }
                        ToHelper::ClipboardPull => {
                            let _ = cmd_tx.send(EngineCmd::ClipboardPull);
                        }
                        ToHelper::FrameAck => {
                            let _ = cmd_tx.send(EngineCmd::FrameAck);
                        }
                        ToHelper::RequestFullFrame => {
                            let _ = cmd_tx.send(EngineCmd::RequestFullFrame);
                        }
                        // RDPDR：挂载/卸载/文件操作应答。带体的应答（Read 的数据）
                        // 与头一起转发——引擎侧的 bridge 按待答形状组装响应。
                        ToHelper::MountDrive { device, name, readonly } => {
                            let _ = cmd_tx.send(EngineCmd::MountDrive { device, name, readonly });
                        }
                        ToHelper::UnmountDrive { device } => {
                            let _ = cmd_tx.send(EngineCmd::UnmountDrive { device });
                        }
                        ToHelper::DriveIoResult { .. } => {
                            let _ = cmd_tx.send(EngineCmd::DriveIoResult(pkt.header, pkt.body));
                        }
                        ToHelper::Shutdown => break,
                        ToHelper::Connect(_) | ToHelper::Hello { .. } => {
                            eprintln!("fs-rdp-helper: 会话进行中收到 {pkt:?}");
                            return 3;
                        }
                    },
                    Err(ReadError::Eof) => {
                        // 主程序关了 stdin：让引擎优雅退出，然后等它跑完。
                        let _ = cmd_tx.send(EngineCmd::Shutdown);
                        stdin_gone = true; // 停止再读；只剩 engine_done 一个唤醒源
                    }
                    Err(e) => {
                        eprintln!("fs-rdp-helper: stdin 帧流损坏：{e}");
                        let _ = cmd_tx.send(EngineCmd::Shutdown);
                        stdin_gone = true;
                    }
                }
            }
            _ = engine_done.notified() => break,
        }
    }

    // 收尾：等引擎退出（优雅关机帧可能还在 pipe 里等着发出）。不等待的话，
    // Shutdown 帧还没 drain 出去进程就没了——主程序那侧看到的是 TCP 直接断。
    let _ = cmd_tx.send(EngineCmd::Shutdown);
    let _ = engine_task.await;
    let _ = forwarder.await;
    0
}

/// 阶段 B 的 stdout 转发任务：引擎上行 + pipe 待发字节 → 帧。
/// 引擎结束（up 通道关闭）后补发 Closed 并收尾。
async fn forward_to_stdout(
    mut tx: FramedWriter<tokio::io::Stdout>,
    mut up_rx: mpsc::UnboundedReceiver<ToMain>,
    handle: PipeHandle,
    engine_done: Arc<Notify>,
) {
    let mut closed = false;
    loop {
        tokio::select! {
            msg = up_rx.recv() => match msg {
                Some(m) => {
                    if matches!(m.header, FromHelper::Closed) {
                        closed = true;
                    }
                    if tx.send(&m.header, &m.body).await.is_err() {
                        break;
                    }
                }
                None => break, // 引擎退出
            },
            _ = handle.wait_outbound() => {
                let bytes = handle.drain_out();
                if !bytes.is_empty() && tx.send(&FromHelper::NetOut, &bytes).await.is_err() {
                    break;
                }
            }
        }
    }
    // 最后一批待发字节（优雅关机帧常常是引擎最后写的）。
    let rest = handle.drain_out();
    if !rest.is_empty() {
        let _ = tx.send(&FromHelper::NetOut, &rest).await;
    }
    if !closed {
        let _ = tx.send(&FromHelper::Closed, &[]).await;
    }
    engine_done.notify_one();
}

#[cfg(test)]
mod tests {
    /// 主线程栈必须抬过 MSVC 的 1 MiB 默认值（与 app/src/lib.rs 的
    /// `stack_reserve_is_raised_above_the_msvc_default` 同款守卫；理由见
    /// build.rs 的注释与 app/src/rdp.rs 的崩溃记述）。
    /// 真实产物的 PE 头由 `tests/stack_reserve.rs` 集成测试直接读二进制断言。
    #[test]
    fn stack_reserve_is_raised_above_the_msvc_default() {
        let build_rs = include_str!("../build.rs");
        assert!(
            build_rs.contains("cargo:rustc-link-arg-bins=/STACK:"),
            "build.rs 里没有 MSVC 的 /STACK 链接参数——helper 主线程会退回 1 MiB 默认栈"
        );
        let declared: usize = build_rs
            .lines()
            .find_map(|l| l.trim().strip_prefix("const MAIN_THREAD_STACK: usize = "))
            .and_then(|rhs| rhs.strip_suffix(';'))
            .and_then(|expr| {
                expr.split('*')
                    .map(|t| t.trim().parse::<usize>().ok())
                    .try_fold(1usize, |acc, n| n.map(|n| acc * n))
            })
            .expect("build.rs 里读不到 MAIN_THREAD_STACK 的字面量——守卫与实现分家了");
        assert!(
            declared >= 4 * 1024 * 1024,
            "helper 主线程栈只留了 {declared} 字节——低于 4 MiB 等于把 \
             2026-08-27 那次崩溃的条件又摆回来了（helper 是独立工作区，\
             app 侧的 /STACK 够不到这里）"
        );
    }
}
