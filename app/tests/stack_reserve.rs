//! 主线程栈保留量的**产物级**守卫（2026-08-27 整程序崩溃的纵深防御之一）。
//!
//! 与 `app/src/lib.rs` 里 `stack_reserve_is_raised_above_the_msvc_default`
//! 的源码守卫互补：那个钉的是 build.rs 里写着链接参数；这个钉的是
//! **刚链出来的这个 exe 的 PE 头里真有 8 MiB**。两者之间的缝隙——改名 bin、
//! 改 tauri.conf.json 的 mainBinaryName、cargo/tauri 升级改变产物路径、
//! 链接参数失效——只有这个能抓住。
//!
//! 只在 Windows 主机上有效（PE 头只存在于 Windows 产物）；其他平台编译为空。

#[cfg(windows)]
#[test]
fn shipped_exe_reserves_8mib_for_the_main_thread() {
    let exe = std::path::Path::new(env!("CARGO_BIN_EXE_future-shell-app"));
    let bytes = std::fs::read(exe).unwrap_or_else(|e| panic!("读不到 {exe:?}：{e}"));

    // DOS 头 e_lfanew（0x3C）→ PE 签名 → 可选头（PE32+ 的 SizeOfStackReserve
    // 在可选头起始 +0x48，8 字节）
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
        "出货 exe 的主线程栈保留量是 {reserve} 字节（应为 8388608）。\
         /STACK 链接参数没有抵达这个二进制——2026-08-27 的整程序崩溃\
         （点一次 RDP 连接窗口就消失）正是 1 MiB 栈撑不住 Tauri 在主线程上\
         构造的命令 future 造成的。"
    );
}
