//! 失败的启动不得被 --smoke-exit-ms 0 的定时器伪装成成功。
//! 使用独立便携目录和破损数据库，不接触用户配置或凭据。
#[cfg(windows)]
#[test]
fn zero_delay_smoke_does_not_hide_a_startup_failure() {
    use std::os::windows::process::CommandExt;
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("future-shell-app.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_future-shell-app"), &exe).unwrap();
    std::fs::write(dir.path().join("portable.txt"), "").unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir(&data).unwrap();
    std::fs::write(data.join("fs.db"), b"deliberately invalid SQLite database").unwrap();
    let mut child = std::process::Command::new(exe)
        .args(["--smoke-exit-ms", "0"])
        .env("WEBVIEW2_USER_DATA_FOLDER", dir.path().join("webview"))
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW：不另开控制台。
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            // Tauri 的 setup hook 失败可能 panic（101），也可能返回到统一错误出口（1）。
            assert!(
                matches!(status.code(), Some(1 | 101)),
                "破损数据库必须启动失败：{status}"
            );
            let diagnosed = std::fs::read_dir(&data).unwrap().flatten().any(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                (name == "startup-error.txt" || name.starts_with("crash-"))
                    && std::fs::read_to_string(entry.path())
                        .is_ok_and(|text| text.contains("file is not a database"))
            });
            assert!(diagnosed, "失败原因必须明确指向破损数据库");
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("破损数据库启动在 60 秒内未退出");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
