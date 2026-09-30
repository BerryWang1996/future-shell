//! helper 主线程栈保留量的**产物级**守卫。理由与写法与
//! `app/tests/stack_reserve.rs` 完全相同，唯一差别：helper 是独立工作区，
//! `app/build.rs` 的链接参数够不到这里，必须自己钉一份。
//! （helper 的 `rt.block_on(run())` 把整个事件循环压在主线程 1 MiB 栈上，
//! 曾与主程序侧同款的 64 KiB 栈数组 future 共存——见 wire.rs 的记述。）

#[cfg(windows)]
#[test]
fn shipped_exe_reserves_8mib_for_the_main_thread() {
    let exe = std::path::Path::new(env!("CARGO_BIN_EXE_fs-rdp-helper"));
    let bytes = std::fs::read(exe).unwrap_or_else(|e| panic!("读不到 {exe:?}：{e}"));

    let lfanew = u32::from_le_bytes(bytes[0x3C..0x40].try_into().unwrap()) as usize;
    assert_eq!(&bytes[lfanew..lfanew + 4], b"PE\0\0", "PE 签名不在预期位置");
    let magic = u16::from_le_bytes(bytes[lfanew + 24..lfanew + 26].try_into().unwrap());
    assert_eq!(magic, 0x20B, "应为 PE32+（64 位），实得 magic {magic:#x}");
    let reserve = u64::from_le_bytes(
        bytes[lfanew + 24 + 0x48..lfanew + 24 + 0x50]
            .try_into()
            .unwrap(),
    );

    assert_eq!(
        reserve,
        8 * 1024 * 1024,
        "helper 出货 exe 的主线程栈保留量是 {reserve} 字节（应为 8388608）。\
         /STACK 没有抵达这个二进制——helper 的整个事件循环（含 engine 的 \
         select! 主循环）都压在主线程栈上，1 MiB 是 2026-08-27 主程序侧\
         崩溃的同一根因条件。"
    );
}
