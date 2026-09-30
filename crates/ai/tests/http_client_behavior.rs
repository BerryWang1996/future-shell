//! 客户端侧行为：起一个**真的** HTTP server，验证 `ReqwestTransport` 的配置真的生效。
//!
//! # 为什么这份测试必须存在
//!
//! `transport.rs` 里已有的测试都注入假 `Transport`——它们验证的是**调用方**
//! （非 2xx 不进解析器、Retry-After 穿透、配置不合法一个包也不发）。
//! 那一层很有价值，但它整个绕过了 `reqwest` 本身，于是
//! `ReqwestTransport::new()` 里那三行 builder 配置**从未被任何东西验证过**：
//!
//! ```ignore
//! .connect_timeout(CONNECT_TIMEOUT)
//! .timeout(REQUEST_TIMEOUT)
//! .redirect(reqwest::redirect::Policy::none())
//! ```
//!
//! 最要紧的是第三行。它的注释写着「跟随重定向会把认证头带到一个我们没打算认证的
//! 主机上去（这是 SSRF 类问题的经典入口）」——那是一句完全正确的安全论断，
//! 而在这份文件出现之前，**没有任何东西能证明那一行还在**。
//! 把它删掉、或者哪天有人为了「支持某个会 301 的代理」把它改成 `Policy::limited(5)`，
//! 全仓测试一条都不会红，而 API key 会开始跟着 302 走到别人的服务器上。
//!
//! # 为什么不用 wiremock
//!
//! M2 出口第 2 项原文点名 `wiremock`。这里改用一个约 80 行的 stub server，
//! 与本仓在 M2 报文层用「可注入 transport + 录制样本」替代 wiremock 的
//! 那次决定同口径，理由也一样是那条：**少一条依赖**。
//! 本仓有过三次同类取舍并留了档——`criterion`（声明了但全仓无一处 `use`）、
//! `tauri-driver`（没有 lib target，`use` 都编译不进来）、
//! `webpki-roots`（为了不多一条 license 例外而走系统信任库）。
//! wiremock 会拖进几十个 dev 依赖（自带 hyper 栈、regex、deadpool…），
//! 而这里要验证的全部行为——不跟随重定向、认证头不到第二跳、
//! 真实 HTTP 头解析、连接被拒时的错误路径——一个 `TcpListener` 就够。
//!
//! 有一件 wiremock 能做而这里没做的：请求匹配与「未匹配即失败」的报告。
//! 本文件的断言方式是让 stub 把收到的请求原文交回来，由测试自己断言，
//! 表达力更弱但足够——这里要钉的是客户端的**配置**，不是 API 契约的形状
//! （后者由 `wire.rs` 的录制样本覆盖）。

use fs_ai::provider::ProviderError;
use fs_ai::transport::{ReqwestTransport, Transport};
use fs_ai::wire::HttpRequestSpec;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 一个只处理**一个**连接的极简 HTTP server。
///
/// 返回 `(端口, 收到的请求原文)`。请求原文用 `Arc<Mutex<_>>` 交回来而不是通过
/// channel：测试要断言的是「有没有收到」，而 channel 的 `recv()` 在没收到时会挂住，
/// 于是「一个请求都没收到」这个**最重要**的断言反而最难写。
///
/// `respond` 是要写回去的整段响应（含状态行与头）。空 = 直接关闭连接不回任何东西。
fn spawn_stub(
    respond: &'static str,
) -> (u16, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    // 端口交给系统分配（`:0`）。写死端口会让并行跑的两个测试互相抢。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑不上回环");
    let port = listener.local_addr().expect("取不到本地地址").port();
    listener.set_nonblocking(true).expect("设非阻塞失败");
    let handle = tokio::spawn(async move {
        let listener = TcpListener::from_std(listener).expect("接管 std listener 失败");
        // 只接一个连接就退出：本文件的每个用例都只发一个请求，
        // 而一个 `loop` 会让 task 永不结束、`abort()` 之外没法收尾。
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        // 读到请求头结束即可。不读 body——这里的用例都不关心 body 内容，
        // 而按 Content-Length 精确读会让「客户端没发 body」的情形挂住。
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            match sock.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        seen2
            .lock()
            .expect("stub 的锁被别的 panic 毒化了")
            .push(String::from_utf8_lossy(&buf).to_string());
        if !respond.is_empty() {
            let _ = sock.write_all(respond.as_bytes()).await;
            let _ = sock.flush().await;
        }
        // 显式关闭：不关的话 reqwest 会一直等 body（我们的响应里带 Content-Length 时才不会）。
        let _ = sock.shutdown().await;
    });
    (port, seen, handle)
}

fn spec(url: String) -> HttpRequestSpec {
    HttpRequestSpec {
        method: "POST",
        url,
        headers: vec![
            // 认证头。下面那条重定向用例的全部意义就是断言**这个串**
            // 一个字节都没到第二跳去。
            (
                "authorization".to_string(),
                "Bearer sk-must-not-leak".to_string(),
            ),
            ("content-type".to_string(), "application/json".to_string()),
        ],
        body: r#"{"hello":"world"}"#.to_string(),
    }
}

/// 核心安全断言：302 **不跟随**，且认证头不到第二跳。
///
/// 起两个 server：第一个回 302 指向第二个，第二个原本会收到跟随过来的请求。
/// 断言两件事——调用方拿到的是 302 本身（而不是第二跳的 200），
/// 且第二个 server 的收件箱是**空的**。
///
/// 只断言第一件事是不够的：`Policy::limited(n)` 在跳数超限时也会把
/// 最后那个 3xx 交回来，那时状态码看起来一样，而 key 已经发出去了。
#[tokio::test]
async fn a_redirect_is_not_followed_and_the_auth_header_never_reaches_the_second_hop() {
    let (hop2_port, hop2_seen, hop2) =
        spawn_stub("HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\n{\"leaked\":true}\n");
    let location = format!("http://127.0.0.1:{hop2_port}/stolen");
    // 302 + Location。Content-Length: 0 让 reqwest 知道这个响应到此为止。
    let redirect =
        format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n");
    // spawn_stub 要 &'static str，这里泄漏一次——测试进程结束即回收，
    // 而为了避免泄漏去改成 String 会让 stub 的签名复杂不少。
    let (hop1_port, hop1_seen, hop1) = spawn_stub(Box::leak(redirect.into_boxed_str()));

    let t = ReqwestTransport::new().expect("建不出客户端");
    let resp = t
        .send(&spec(format!("http://127.0.0.1:{hop1_port}/v1/chat")))
        .await
        .expect("第一跳本身应当成功返回（302 是一个正常响应，不是错误）");

    // ① 拿到的是 302 本身，不是第二跳的 200
    assert_eq!(
        resp.status, 302,
        "跟随了重定向：拿到的状态码是第二跳的。redirect(Policy::none()) 没生效"
    );

    // ② 第二跳一个请求都没收到——这才是真正要钉的那件事
    let _ = hop1.await;
    hop2.abort(); // 它还在 accept 上等，不 abort 会让测试挂到超时
    let box2 = hop2_seen.lock().expect("锁毒化");
    assert!(
        box2.is_empty(),
        "认证头跟着 302 走到了第二跳。收到的请求：{box2:?}"
    );

    // ③ 正向对照：第一跳确实收到了带 key 的请求。
    //    没有这一条的话，「第二跳空」可能只是因为整个请求根本没发出去，
    //    而那时上面两条断言都是绿的、却什么都没证明。
    let box1 = hop1_seen.lock().expect("锁毒化");
    assert_eq!(box1.len(), 1, "第一跳应当收到恰好一个请求");
    assert!(
        box1[0].contains("sk-must-not-leak"),
        "第一跳没收到认证头——那本次用例并没有在验证泄漏，只是在验证一个空请求。收到：{}",
        box1[0]
    );
}

/// 真实 HTTP 头解析：`Retry-After` 从**网络字节**里解出来，不是注入的。
///
/// `transport.rs` 已有一条注入版的 Retry-After 测试，但那条喂的是一个
/// 已经填好 `retry_after_secs` 的假响应——它验证的是下游怎么用这个值，
/// 而不是「我们真的会去读这个头」。
#[tokio::test]
async fn retry_after_is_parsed_from_the_wire() {
    let (port, _seen, h) = spawn_stub(
        "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 42\r\nContent-Length: 2\r\n\r\n{}",
    );
    let t = ReqwestTransport::new().expect("建不出客户端");
    let resp = t
        .send(&spec(format!("http://127.0.0.1:{port}/v1/chat")))
        .await
        .expect("429 是一个正常响应");
    let _ = h.await;
    assert_eq!(resp.status, 429);
    assert_eq!(
        resp.retry_after_secs,
        Some(42),
        "没从真实响应头里读出 Retry-After"
    );
    assert_eq!(resp.body, "{}");
}

/// 请求头真的按 `spec.headers` 发出去，且方法按 `spec.method` 分派。
///
/// 后者曾经是个真 bug：结构体上有 `method` 字段而实现写死 `post`。
/// 那条已经修了并有单测，但那个单测断言的是**代码路径**；这里断言的是
/// 对端**真的收到**了 `POST`——两者的区别在于，前者在 reqwest 层出问题时仍然绿。
#[tokio::test]
async fn the_request_line_and_headers_go_out_as_specified() {
    let (port, seen, h) = spawn_stub("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
    let t = ReqwestTransport::new().expect("建不出客户端");
    let _ = t
        .send(&spec(format!("http://127.0.0.1:{port}/v1/chat")))
        .await
        .expect("应当成功");
    let _ = h.await;
    let got = seen.lock().expect("锁毒化");
    let req = &got[0];
    assert!(
        req.starts_with("POST /v1/chat HTTP/1.1"),
        "请求行不对：{req}"
    );
    // 头名大小写不敏感，但 reqwest 会规范化成小写发出
    let lower = req.to_ascii_lowercase();
    assert!(
        lower.contains("authorization: bearer sk-must-not-leak"),
        "认证头没发出去：{req}"
    );
    assert!(
        lower.contains("content-type: application/json"),
        "content-type 没发出去：{req}"
    );
    // body 也在（stub 读到头结束就停，但 TCP 缓冲里通常连着 body 一起到）
    // ——不断言 body，因为那取决于分片，是个会随机红的断言。
}

/// 连接被立刻拒绝时，得到的是 `Transport` 错误而**不是** panic，
/// 且错误串里没有 spec 的内容。
///
/// 后半句是 `transport.rs` 里那句注释的对偶：「刻意只带 e 的显示串，不带 spec：
/// spec 里有 key」。一个把整个 spec 塞进错误串的实现会让 API key 出现在
/// 日志、Toast、以及用户贴给别人的报错里。
#[tokio::test]
async fn a_refused_connection_yields_a_transport_error_without_leaking_the_key() {
    // 绑一个端口再立刻释放，拿到一个几乎肯定没人监听的号。
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("绑不上");
        l.local_addr().expect("取地址失败").port()
    };
    let t = ReqwestTransport::new().expect("建不出客户端");
    let err = t
        .send(&spec(format!("http://127.0.0.1:{port}/v1/chat")))
        .await
        .expect_err("没人监听的端口不该成功");
    match &err {
        ProviderError::Transport { detail } => {
            assert!(
                !detail.contains("sk-must-not-leak"),
                "错误串里带了 API key：{detail}"
            );
        }
        other => panic!("期望 Transport 错误，得到 {other:?}"),
    }
    // 反向对照：错误串**非空**。空串同样满足上面那条断言，而它意味着
    // 用户拿到的是一句「请求失败：」——没有任何可据以排查的信息。
    let ProviderError::Transport { detail } = &err else {
        unreachable!()
    };
    assert!(!detail.trim().is_empty(), "错误串是空的");
}
