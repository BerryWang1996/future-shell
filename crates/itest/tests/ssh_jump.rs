use fs_itest::sshd::SshdContainer;
use russh::client;
use russh::ChannelMsg;
use std::sync::Arc;
use std::time::Duration;

struct AcceptAllKeys;
impl russh::client::Handler for AcceptAllKeys {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _k: &russh::keys::PublicKey) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn jump_to_self_via_direct_tcpip() {
    if std::env::var("FS_ITEST").is_err() {
        eprintln!("skip: set FS_ITEST=1");
        return;
    }
    let sshd = SshdContainer::start("jump").await.unwrap();
    // 镜像默认 AllowTcpForwarding no，不先开通则 direct-tcpip 直接回
    // ChannelOpenFailure(AdministrativelyProhibited)（实测）
    sshd.enable_tcp_forwarding().await.unwrap();
    let addr = sshd.addr().await;

    // 第一跳：TCP 直连 + 密码认证（AuthResult 是枚举：用 success() 方法）
    let mut hop = client::connect(Arc::new(client::Config::default()), addr, AcceptAllKeys)
        .await
        .unwrap();
    assert!(hop
        .authenticate_password(&sshd.username, &sshd.password)
        .await
        .unwrap()
        .success());

    // 经第一跳开 direct-tcpip 通道回到容器自身（验证 forward() 的底层原语；端口参数 u32）。
    // 目标写「容器内 127.0.0.1:2222」而非 addr：direct-tcpip 由**服务端**发起 TCP 连接，
    // 用的是容器自己的网络命名空间——宿主映射端口（addr.port()，形如 32812）在那里不存在，
    // 填它只会连接被拒，且失败点离真正原因很远。
    let channel = hop
        .channel_open_direct_tcpip(
            "127.0.0.1",
            u32::from(fs_itest::sshd::INTERNAL_SSH_PORT),
            "127.0.0.1",
            0,
        )
        .await
        .unwrap();
    let stream = channel.into_stream();

    // 第二跳：在转发流上用三参 connect_stream 再握手 + 认证 + exec
    let mut inner =
        client::connect_stream(Arc::new(client::Config::default()), stream, AcceptAllKeys)
            .await
            .unwrap();
    assert!(inner
        .authenticate_password(&sshd.username, &sshd.password)
        .await
        .unwrap()
        .success());

    let mut ch = inner.channel_open_session().await.unwrap();
    ch.exec(false, "echo jumped").await.unwrap();

    // 每次 wait() 套 timeout_at + 显式 None 分支，且 exit-status 在循环外断言：
    // 裸 `while let Some(..) = ch.wait().await` 遇沉默服务器会无限期挂起（表现为 CI 任务级
    // 超时而非一条测试失败），循环内就地断言退出码则在服务器不发 ExitStatus 时被静默跳过（S27/S28）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut out = Vec::new();
    let mut status = None;
    loop {
        match tokio::time::timeout_at(deadline, ch.wait()).await {
            Ok(Some(ChannelMsg::Data { data })) => out.extend_from_slice(&data),
            // 退出状态可能先于剩余输出到达（sshd 在子进程退出时即发），记下后继续收，
            // 直到通道关闭。在这里 break 过，CI 上偶发拿到空输出（2026-09-30）。
            Ok(Some(ChannelMsg::ExitStatus { exit_status })) => status = Some(exit_status),
            Ok(Some(ChannelMsg::Close)) => break,
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!(
                "timeout reading jumped exec output; got so far: {:?}",
                String::from_utf8_lossy(&out)
            ),
        }
    }
    let out = String::from_utf8_lossy(&out).trim().to_string();
    assert_eq!(
        status,
        Some(0),
        "exit status missing or nonzero; stdout={out}"
    );
    assert_eq!(out, "jumped");
}
