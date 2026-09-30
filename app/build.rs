/// 主线程栈保留量（字节）。8 MiB —— 与 Linux 主线程默认值对齐。
///
/// 数值同时被 `stack_reserve_is_raised_above_the_msvc_default` 断言，
/// 改这里必须让那条判据一起改，别让守卫和实现悄悄分家。
const MAIN_THREAD_STACK: usize = 8 * 1024 * 1024;

fn main() {
    // ── Windows 主线程栈：从 MSVC 默认的 1 MiB 抬到 8 MiB ────────────────────
    //
    // **起因是一次真实的整程序崩溃**（2026-08-27）：用户点 RDP 连接，窗口瞬间
    // 消失，没有 crash 文件、没有 Windows 事件日志、日志停在 `ipc:invoke` 那行。
    // stderr 上只有一句 `thread 'main' has overflowed its stack`。
    // 根因与完整的尺寸级联记在 `app/src/rdp.rs` 的 `FrameReader::next` 文档里。
    //
    // ## 为什么 1 MiB 对这个进程本来就太小
    //
    // Tauri 把 WebView2 的 IPC 分发放在**主线程**上（WebResourceRequested 回调
    // 同步进入 `run_invoke_handler`）。命令 future 是在这条栈上构造、再搬进
    // tokio 任务的 —— 一条 async 调用链上所有跨 await 存活的局部量，会在这里
    // 叠成一份连续的栈帧，而且 release 下 tauri 走单态化路径，同一个 future
    // 在移交过程中存在多份拷贝。
    //
    // 偏偏主线程是全进程**最窄**的一条栈：Rust 自建线程默认 2 MiB，Linux 主线程
    // 8 MiB，只有 Windows 主线程按 PE 头走 MSVC 的 1 MiB 默认值。所以同一份代码
    // 在别处跑得好好的，一到 Windows 装机版就炸。
    //
    // ## 代价
    //
    // `/STACK` 改的是**保留量**（虚拟地址空间预留），提交页仍按需增长 ——
    // 常驻内存不变，64 位地址空间里多预留 7 MiB 没有实际成本。
    //
    // ## 这不替代根因修复
    //
    // 抬栈挡的是「下一个还没写出来的大 future」；RDP 那条链的缓冲已经挪回堆上，
    // 并由 `frame_reader_future_must_not_carry_its_buffer_on_the_stack` 钉住。
    // 两者互不替代：只抬栈是把问题推到更大的 future 上，只修根因则下一次
    // 有人写出同样的东西时，用户还是先看到「整个程序消失」。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // MSVC 与 GNU 两套链接器的写法不同，写错了是整个链接失败（不是静默无效）。
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            println!("cargo:rustc-link-arg-bins=/STACK:{MAIN_THREAD_STACK}");
        } else {
            println!("cargo:rustc-link-arg-bins=-Wl,--stack,{MAIN_THREAD_STACK}");
        }
    }

    tauri_build::build()
}
