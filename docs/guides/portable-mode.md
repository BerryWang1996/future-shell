# 便携模式与应用数据


把整个程序目录解压到 U 盘或任意**可写**位置，在**可执行文件同目录**放一个空文件
`portable.txt`，数据（Vault、连接库、日志）就会落到旁边的 `data/` 而不是
`%APPDATA%` / `~/.config`。

```
E:\FutureShell\
  future-shell-app.exe
  fs-rdp-helper.exe
  portable.txt        ← 放这个文件即启用
  data\               ← 首次启动时自动建立
```

三点值得说清楚：

- **锚点是可执行文件所在目录，不是当前工作目录。** 双击、命令行、快捷方式启动
  得到的是同一个答案；用 cwd 的话三种启动方式三个答案，数据会散落一地。
- **默认是关的，且只认文件。** 没放 `portable.txt` 就绝不启用；`portable.txt` 是个
  **目录**时也不算——那多半是解压出错，把它当成开启会让数据落到你没打算用的地方。
- **位置不可写时明确失败，不悄悄退回 `%APPDATA%`。** 你放了标记文件，意图是
  「数据跟着程序走」；这时悄悄落到别处，等于在你不知情时把数据留在这台机器上——
  拔下 U 盘走人，Vault 还在人家硬盘里。所以宁可报错并告诉你换个位置。

> 便携模式下 Vault 建议用**应用密码**而不是系统 keyring（换机器时 keyring 里的
> 密钥不会跟着走）。总设计 §183 的应用密码本来就是为这个场景准备的。

> **安全提醒**：便携模式把加密的 Vault 放在可移动介质上。Vault 本身是加密的，
> 但介质丢失意味着攻击者可以离线穷举你的应用密码——请用足够强的密码。

## 发布标识（bundle identifier）

`app/tauri.conf.json` 的 `identifier` 是 **`io.futureshell.app`**。这是打包/安装层面的稳定身份，
不是随手写的开发占位符（历史值 `dev.futureshell.app` 已废弃）。它决定：

- Windows：MSI/NSIS 的产品标识与注册表项，以及 WebView2 的用户数据目录
  （`%LOCALAPPDATA%\io.futureshell.app\EBWebView`——localStorage / IndexedDB 落在这里）；
- macOS：`.app` 的 CFBundleIdentifier，同时是 codesign / notarization 的锚点；
- Linux：`.desktop` 与 deb/AppImage 的包标识。

**改动它会影响已安装用户**：新旧标识在系统看来是两个不同产品，装新版不会覆盖旧版，且旧标识
目录下的 WebView 本地存储不会被继承。所以一旦发过正式版，这个值就当成不可变量对待——要改必须
配套写迁移步骤。

需要说明的是，应用**自己的**数据目录（`fs.db`、日志）走的是
`app/src/state.rs::data_dir_or_err()`——默认 `config_dir()/future-shell`，
便携模式下改为可执行文件旁的 `data/`。两者都与 identifier 解耦，
更改打包标识时，也应独立核验连接配置与凭据库的兼容性。
