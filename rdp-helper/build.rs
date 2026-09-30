/// 主线程栈保留量（字节）。8 MiB —— 与主程序 `app/build.rs` 同款。
///
/// helper 的主线程跑着 `rt.block_on(run())`（main.rs），整个事件循环的
/// future 都压在这条栈上；而 MSVC 默认只给主线程 1 MiB——全进程最窄。
/// 2026-08-27 主程序侧那次整程序崩溃（详见 app/src/rdp.rs 的
/// `FrameReader::next` 文档）证明了这个默认值对带大 future 的事件循环
/// 是不够的。`app/build.rs` 的 `cargo:rustc-link-arg-bins` 够不到这个
/// 独立工作区，故这里单独抬高一份。
///
/// 同款源码守卫在 `src/main.rs` 的 tests 里（读本文件断言 ≥4 MiB）。
const MAIN_THREAD_STACK: usize = 8 * 1024 * 1024;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // MSVC 与 GNU 两套链接器的写法不同，写错了是整个链接失败（不是静默无效）。
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            println!("cargo:rustc-link-arg-bins=/STACK:{MAIN_THREAD_STACK}");
        } else {
            println!("cargo:rustc-link-arg-bins=-Wl,--stack,{MAIN_THREAD_STACK}");
        }
    }
}
