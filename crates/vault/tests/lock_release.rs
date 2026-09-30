//! P0 回归：保险库的跨进程锁必须**真的能被释放**。
//!
//! 历史实现里，「退出时清理锁」这条承诺只写在 `VaultGuard::drop` 的注释里，而那段代码
//! 谁也走不到：守卫的 `Arc` 存活在一张与进程等长的 static 注册表里，强引用计数不会归零；
//! 即便归零，Rust 也从不为 static 跑析构。于是 Unix 侧只剩「下次启动做陈旧锁检测」
//! 这一条回收路径 —— 而那条路径当时在非 Linux 上恒判「持有者还活着」（只查 `/proc`）。
//! 两条路一起断，结果是：macOS 用户**正常退出一次**之后，之后每一次启动都会被上一次
//! 留下的 `vault.lock` 挡在门外，且错误还被前端包进「解锁失败（密码错误或 keyring/文件异常）」，
//! 用户只会怀疑自己记错了密码。
//!
//! 本文件单独成一个测试目标（= 单独一个进程）：`release_process_locks` 清的是**进程全局**
//! 注册表，与别的测试同处一个二进制时会互相拆台（另一个测试的 Store 仍持有守卫，
//! 注册表却已被清空，它下一次 `open` 会去重抢一把自己还握着的锁）。

mod common;
use common::{setup_mock_keyring, tempdir as mk_tempdir};
use fs_vault::Store;
use std::path::Path;

/// Windows 上「锁是否仍被持有」的直接探针：`share_mode(0)` 打开成功 = 没人持有。
///
/// Unix 侧锁的载体是**文件存在性**（建议锁，见 `store::VaultGuard::open_exclusive`），
/// Windows 侧是**内核级独占句柄**、文件本身会留下。两个平台因此需要两种探法，
/// 但断言的是同一件事：这一刻别的实例进不进得来。
#[cfg(windows)]
fn exclusive_open_succeeds(p: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    // 句柄在函数返回时立即关闭，不会挡住后续的重新打开
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(p)
        .is_ok()
}

#[cfg(unix)]
fn exclusive_open_succeeds(p: &Path) -> bool {
    !p.exists()
}

#[test]
fn release_process_locks_actually_releases_the_lock() {
    setup_mock_keyring();
    let dir = mk_tempdir("lockrel");
    let lock = dir.join("vault.lock");

    let store = Store::open_or_create(&dir, None).unwrap();
    assert!(lock.exists(), "打开保险库必须落下跨进程锁");
    assert!(
        !exclusive_open_succeeds(&lock),
        "锁在库打开期间必须真的被持有，否则第二个实例可以同时整文件重写同一个库"
    );

    // 丢掉 Store 并不释放：注册表持强引用是**刻意**的 —— 同一目录反复 open/unlock
    // 是正常用法（解锁 → 改密 → 再解锁），每次都重抢跨进程锁只会自己卡自己，
    // 中间那个「刚放开、还没抢回」的窗口还会让别的实例插进来。
    drop(store);
    assert!(
        !exclusive_open_succeeds(&lock),
        "Store 掉了就松锁 —— 注册表的强引用语义被改坏了"
    );

    // 唯一的释放路径：进程退出时由 app 在 `RunEvent::Exit` 上显式调用（见 app/src/lib.rs）。
    fs_vault::release_process_locks();
    assert!(
        exclusive_open_succeeds(&lock),
        "release_process_locks 没有真正释放锁：VaultGuard::drop 仍是一行死代码，\
         Unix 上用户下次启动会被自己上一次留下的 vault.lock 挡在门外"
    );
    #[cfg(unix)]
    assert!(
        !lock.exists(),
        "Unix 的锁就是这个文件本身，不删掉等于没释放"
    );

    // 释放之后必须还能正常打开（不是把库锁死换来的「释放」）
    let again = Store::open_or_create(&dir, None).unwrap();
    assert!(lock.exists(), "重新打开必须重新落下锁");
    drop(again);
    fs_vault::release_process_locks();
}
