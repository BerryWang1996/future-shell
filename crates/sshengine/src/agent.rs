//! SSH agent 传输（spec §2.1 / 附录 A.2）：unix 走 `SSH_AUTH_SOCK`，Windows 依次探测
//! Pageant 与 Win32-OpenSSH 命名管道。三条管道各自的流类型不同，经 russh 的 `dynamic()`
//! 擦除为同一句柄类型后，上层认证代码无须再关心平台。
use crate::Error;
use russh::keys::agent::client::{AgentClient, AgentStream};

/// 跨平台统一 agent 句柄：不同传输经 `dynamic()` 擦除为同一类型。
pub type SharedAgentClient = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

/// 诊断文案：统一列出三种传输，任何平台都给出完整排查指引（测试无 cfg 门控）。
///
/// 之所以**不按平台裁剪**：agent 不可用时用户看到的往往只有这一行字，而「本机是什么平台」
/// 对拿到日志的人（同事、issue 里的维护者）并不自明；三条一起列出的成本只是多两句话。
pub fn describe_agent_unavailable() -> String {
    concat!(
        "未检测到可用的 SSH agent。请任选其一：启动 ssh-agent 并设置 SSH_AUTH_SOCK；",
        "Windows 上启动 Pageant；或启动 Windows 内置 OpenSSH ssh-agent 服务",
        "（\\\\.\\pipe\\openssh-ssh-agent）"
    )
    .into()
}

/// 传输探测失败 → `Error::Auth`：把底层错误与三管道排查指引一并放进 `notes`。
/// 用 `Auth` 而非 `Ssh`，是为了让上层错误分类保持一致——这确实是一次认证方法的失败。
/// 指引进 `notes` 而非 `remaining`：后者的语义是「服务器还允许什么」，而 agent 不可用是
/// **本地**故障，混进去会把排查方向引向服务端（S38）。
fn unavailable<E: std::fmt::Display>(e: E) -> Error {
    Error::Auth {
        tried: vec!["agent".into()],
        remaining: vec![],
        notes: vec![format!(
            "agent unavailable: {e}; {}",
            describe_agent_unavailable()
        )],
    }
}

/// **存活探测**（S37）：构造成功 ≠ 可用。三条传输的 `connect_*` 都只负责把流对象造出来，
/// 真正的握手推迟到首次 I/O，因此必须真发一次 `request_identities` 才知道对端是不是活着的
/// agent。探测通过后再 `dynamic()` 擦除类型，对外契约由此收紧为「返回 `Ok` 即代表可用」——
/// 集成测试正是按这条契约断言的（`Ok` 分支必须能问出身份列表）。
/// 代价是成功路径多一次本地 IPC 往返，相对一次 SSH 认证可忽略。
///
/// 审计2 #25：这次往返必须有上限。探测走本机 IPC，正常在毫秒级，但会卡住的情形是真实存在的
/// ——管道对端是个半死的进程、Pageant 所在窗口线程正忙、命名管道已建立而服务端不再应答。
/// 没有上限时它就是**认证路径上的一次无限期等待**：`connect.rs` 给每一段认证都设了预算，
/// 唯独 agent 探测漏在预算之外，于是一个本机的坏管道足以让整次连接永远停在「正在认证」。
/// 超时按「本地故障」归到 `Auth::notes`（与其他探测失败同路），让上层照常回退到下一种认证
/// 方法，而不是把整次连接判死——agent 探测失败从来都是可回退的。
async fn live_dynamic<S>(mut c: AgentClient<S>) -> Result<SharedAgentClient, Error>
where
    S: AgentStream + Send + Unpin + 'static,
{
    probe("request_identities", c.request_identities())
        .await?
        .map_err(unavailable)?;
    Ok(c.dynamic())
}

/// 给一次 agent 本机 IPC 往返套上 [`AGENT_PROBE_TIMEOUT`]，超时按 agent 不可用处理。
///
/// 泛型套在 future 上而不是逐处 `tokio::time::timeout(...)`：本模块里需要设限的 await 有四处
/// （三条传输的构造 + 一次身份查询），逐处手写的做法只要新增一条传输就会漏掉。
async fn probe<F: std::future::Future>(stage: &str, fut: F) -> Result<F::Output, Error> {
    tokio::time::timeout(crate::timeouts::AGENT_PROBE_TIMEOUT, fut)
        .await
        .map_err(|_| {
            unavailable(format!(
                "{stage} 超过 {} 秒无响应（本机 agent 管道疑似半死）",
                crate::timeouts::AGENT_PROBE_TIMEOUT.as_secs()
            ))
        })
}

#[cfg(unix)]
pub async fn connect_agent_client() -> Result<SharedAgentClient, Error> {
    // connect_env 读取 SSH_AUTH_SOCK 连接 Unix socket。这条路上「构造」已经相当接近「可用」
    // （变量未设 → EnvVar，套接字文件不存在 → BadAuthSock，无监听者 → ECONNREFUSED），
    // 仍走 live_dynamic 是为了让**三平台契约一致**：陈旧套接字文件配上已死的监听者仍能连上，
    // 且集成测试不必为平台分叉写两套断言。
    live_dynamic(
        probe("connect_env", AgentClient::connect_env())
            .await?
            .map_err(unavailable)?,
    )
    .await
}

#[cfg(windows)]
pub async fn connect_agent_client() -> Result<SharedAgentClient, Error> {
    // 先探测 Pageant，再回退 Win32-OpenSSH ssh-agent 管道。
    //
    // S37：这里**必须**按存活而非按构造分诊。`connect_pageant()` 的实现是
    // `Ok(Self::connect(PageantStream::new().await?))`——只是 new 出一个流对象，WM_COPYDATA
    // 握手要等到首次 I/O 才发生，所以 Pageant 根本没在跑时它照样返回 `Ok`。若按构造分诊，
    // `if let Ok(_)` 恒真，下面的命名管道回退在**任何**一台 Windows 上都是死代码；而绝大多数
    // Windows 用户用的恰恰是系统内置的 Win32-OpenSSH agent（Pageant 需另装 PuTTY），他们会
    // 拿到一句 `early eof`——既不点名 agent 也不点名任何一条传输，`describe_agent_unavailable`
    // 的指引永远送不到人手里。
    // 本机实测（无 Pageant 进程、`sc query ssh-agent` 为 STOPPED）：connect_pageant → Ok，
    // 紧接着 request_identities → "early eof"。故用 live_dynamic 探测，失败即丢弃回退。
    if let Ok(Ok(c)) = probe("connect_pageant", AgentClient::connect_pageant()).await {
        if let Ok(live) = live_dynamic(c).await {
            return Ok(live);
        }
    }
    // 命名管道同理：管道不存在时 open 会立刻报 os error 2（可安全回退到报错），但管道存在而
    // 对端半死时仍需存活探测兜底。
    live_dynamic(
        probe(
            "connect_named_pipe",
            AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent"),
        )
        .await?
        .map_err(unavailable)?,
    )
    .await
}

/// 列出 agent 里当前装着哪些密钥的**可公开信息**（M4b「密钥/代理管理器」）。
///
/// 与 [`crate::keyinfo::describe_private_key`] 返回同一个类型，理由也相同：
/// 那个类型装不下密钥材料。而 agent 这一侧本来就只给得出公钥——SSH agent 协议
/// 的全部意义就是**私钥不出 agent**，所以这里连「可能泄漏」的前提都不成立。
/// 用同一个类型是为了让界面上两个来源的密钥长得一样，不必分辨。
///
/// agent 不可用时返回 `Err`：那是一句要给用户看的话（「Pageant 没在跑」之类），
/// 不是一个空列表。空列表会被读成「agent 在跑但里面没有密钥」，那是完全不同的处境——
/// 前者要去启动 agent，后者要去 `ssh-add`。
pub async fn list_agent_keys() -> Result<Vec<crate::keyinfo::KeyInfo>, Error> {
    use russh::keys::PublicKeyBase64;
    let mut client = connect_agent_client().await?;
    let ids = probe("request_identities", client.request_identities())
        .await?
        .map_err(unavailable)?;
    Ok(ids
        .into_iter()
        .map(|id| {
            // agent 里可以装两种东西：裸公钥，与 OpenSSH 证书。
            // 证书**不是**公钥的一种写法——它是「公钥 + CA 签名 + 有效期 + principals」。
            // 把它当成普通密钥显示会让用户以为服务器上要装这把公钥，而实际上要装的是 CA。
            // 所以两者分开标注，宁可多一行字也不要让人认错。
            let (key, comment, is_cert) = match id {
                russh::keys::agent::AgentIdentity::PublicKey { key, comment } => {
                    (key, comment, false)
                }
                russh::keys::agent::AgentIdentity::Certificate {
                    certificate,
                    comment,
                } => (certificate.public_key().clone().into(), comment, true),
            };
            let algorithm = key.algorithm().to_string();
            crate::keyinfo::KeyInfo {
                fingerprint: crate::hostkey::fingerprint_sha256(&key.public_key_bytes()),
                algorithm: if is_cert {
                    format!("{algorithm}（证书）")
                } else {
                    algorithm
                },
                // 注释是 ssh-add 时记下的，用户自己写的，长度无上限——同样截断。
                comment: comment.chars().take(120).collect(),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    /// 「管道建好了，对端半死」——写进得去（内核缓冲收下了），读永远不回。
    ///
    /// 这正是审计2 #25 说的那种卡住：`connect_*` 只把流对象造出来，握手推迟到首次 I/O，
    /// 所以真正会挂的是探测本身。用假流而不是真 agent，是因为这个状态在真机上不可复现——
    /// 得先有一个半死的 ssh-agent 进程。
    struct DeafPipe;

    impl AsyncRead for DeafPipe {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
    }

    impl AsyncWrite for DeafPipe {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_deaf_agent_pipe_is_given_up_on_rather_than_waited_on_forever() {
        let budget = crate::timeouts::AGENT_PROBE_TIMEOUT;
        let t0 = tokio::time::Instant::now();
        // `AgentClient` 不实现 Debug，`expect_err` 用不了，故手工拆 Result。
        let r = tokio::time::timeout(budget * 4, live_dynamic(AgentClient::connect(DeafPipe)))
            .await
            .expect("agent 存活探测没有上限：跑过四倍预算仍未返回");
        let Err(e) = r else {
            panic!("对端从不回话，探测不该成功")
        };
        let waited = t0.elapsed();
        assert!(waited >= budget, "还没到预算就放弃了：{waited:?}");
        assert!(
            waited < budget + std::time::Duration::from_secs(1),
            "超过预算很久才放弃：{waited:?}"
        );

        // 归类同样是修复的一部分：agent 不可用是**本地**故障，必须落到 `Auth.notes`，
        // 上层才会照常回退到下一种认证方法。若归到 `remaining`（服务器还允许什么）或直接
        // 抛 `Ssh`，排查方向会被引向服务端，而且整次连接会因为一条坏的本机管道而失败。
        match e {
            Error::Auth {
                tried,
                remaining,
                notes,
            } => {
                assert_eq!(tried, vec!["agent".to_string()]);
                assert!(remaining.is_empty(), "本地故障不该写进 remaining");
                let n = notes.join(" ");
                assert!(n.contains("request_identities"), "没点名卡在哪一步：{n}");
                assert!(n.contains(&budget.as_secs().to_string()), "没写明预算：{n}");
                assert!(n.contains("SSH_AUTH_SOCK"), "丢了三管道排查指引：{n}");
            }
            other => panic!("agent 探测超时被归错类了：{other}"),
        }
    }
}
