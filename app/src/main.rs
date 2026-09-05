#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // MCP 桥模式（2026-08-28）：外部 MCP 客户端（Claude Desktop 等）的配置里
    // 把本 exe 当 command 拉起。桥**只做字节中继**：它的 stdio <-> 主程序的
    // localhost 套接字。三件事刻意不做——不开窗口、不初始化 Tauri/数据库、
    // **不碰单实例闸**（闸是给「第二个完整实例」的，桥不是实例，是中继；
    // 桥过闸会把「主程序开着时外部客户端连不上」变成永久状态）。
    // 与 --smoke-exit-ms 同款位置：必须排在 future_shell_app::run() 之前。
    if std::env::args().any(|a| a == "--mcp-bridge") {
        std::process::exit(mcp_bridge_main());
    }

    // 审计2 #5（安装包 smoke test）：CI runner 没有交互会话，装完安装包要验证
    // 「二进制起得来、webkit/GTK 等动态依赖齐全、窗口真的建得出来」，就需要进程在
    // 事件循环跑起来之后**自己退出**。计时器必须等 RunEvent::Ready 才启动；
    // 若在 main 里就计时，启动卡住也会到点返回 0，给安装 smoke 制造假通过。
    let smoke_exit_ms = parse_smoke_exit_ms(std::env::args().skip(1));
    future_shell_app::run_with_smoke_exit(smoke_exit_ms)
}

/// 解析 `--smoke-exit-ms N`。纯函数，便于离线测试（见下方 mod tests）。
/// 非法/缺失的毫秒值一律 None——smoke flag 是运维钩子，坏值不得改变启动行为。
fn parse_smoke_exit_ms(args: impl Iterator<Item = String>) -> Option<u64> {
    let mut it = args;
    while let Some(a) = it.next() {
        if a == "--smoke-exit-ms" {
            return it.next().and_then(|v| v.parse::<u64>().ok());
        }
    }
    None
}

/// MCP 桥进程主体。退出码：0 = 对端正常断开；1 = 环境问题（找不到主程序/
/// 缺 token）。MCP 客户端会把非零退出码显示给用户，环境问题要给**人话**。
fn mcp_bridge_main() -> i32 {
    // 数据目录解析要最小化：桥不建目录、不开库——只读 endpoint 文件。
    let Some(dir) = future_shell_app::bridge_data_dir() else {
        eprintln!("fs-mcp-bridge: 无法定位数据目录");
        return 1;
    };
    let endpoint = dir.join("mcp.endpoint");
    let Ok(port) = std::fs::read_to_string(&endpoint).map_err(|e| {
        eprintln!(
            "fs-mcp-bridge: 读不到 {}（{e}）——主程序没开，或 MCP 未启用",
            endpoint.display()
        )
    }) else {
        return 1;
    };
    let Ok(port) = port.trim().parse::<u16>() else {
        eprintln!("fs-mcp-bridge: endpoint 文件内容不是端口：{port:?}");
        return 1;
    };
    let Some(token) = std::env::var("FS_MCP_TOKEN").ok() else {
        eprintln!("fs-mcp-bridge: 缺 FS_MCP_TOKEN 环境变量——在外部客户端配置的 env 里填入设置页展示的 token");
        return 1;
    };

    // 阻塞式三件套（桥不需要 async 的任何东西，sync 代码更短更直白）：
    // 连接 → 发 token 行 → 双向中继到任一侧断开。
    use std::io::Write;
    use std::net::TcpStream;
    let Ok(mut sock) = TcpStream::connect(("127.0.0.1", port)) else {
        eprintln!("fs-mcp-bridge: 连不上 127.0.0.1:{port}——主程序没开，或刚关掉");
        return 1;
    };
    let _ = sock.set_nodelay(true);
    if writeln!(sock, "{token}")
        .and_then(|_| sock.flush())
        .is_err()
    {
        eprintln!("fs-mcp-bridge: 发送 token 失败");
        return 1;
    }

    // 双向中继：两个线程各抄一个方向。任一侧 EOF/出错即整体退出——
    // MCP 是请求-响应式会话，单向活着没有意义。
    let sock2 = match sock.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fs-mcp-bridge: 复制套接字失败：{e}");
            return 1;
        }
    };
    let up = std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut s = sock;
        let _ = std::io::copy(&mut stdin, &mut s);
        let _ = s.shutdown(std::net::Shutdown::Write);
    });
    let mut out = std::io::stdout().lock();
    let mut s2 = sock2;
    let mut reader = std::io::BufReader::new(&mut s2);
    let _ = std::io::copy(&mut reader, &mut out);
    let _ = out.flush();
    let _ = up.join();
    0
}

#[cfg(test)]
mod tests {
    use super::parse_smoke_exit_ms;

    fn p(args: &[&str]) -> Option<u64> {
        parse_smoke_exit_ms(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn smoke_flag_parses_millis() {
        assert_eq!(p(&["--smoke-exit-ms", "15000"]), Some(15000));
        assert_eq!(p(&["--smoke-exit-ms", "0"]), Some(0));
    }

    #[test]
    fn non_numeric_or_missing_value_yields_none() {
        assert_eq!(p(&["--smoke-exit-ms", "abc"]), None);
        assert_eq!(p(&["--smoke-exit-ms"]), None);
        assert_eq!(p(&["--smoke-exit-ms", "-5"]), None);
    }

    #[test]
    fn absent_or_unknown_flags_yield_none() {
        assert_eq!(p(&[]), None);
        assert_eq!(p(&["--other"]), None);
        assert_eq!(p(&["--smoke-exit-ms-typo", "1"]), None);
    }
}
