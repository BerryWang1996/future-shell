# 日志与故障排查

## 日志在哪

- **数据目录**：`<data_dir>/logs/futureshell.log`
  - Windows：`%APPDATA%\future-shell\logs\futureshell.log`
  - macOS：`~/Library/Application Support/future-shell/logs/futureshell.log`
  - Linux：`~/.config/future-shell/logs/futureshell.log`
- 应用内有入口：菜单「打开日志目录」（`reveal_log_dir`），直接弹出该目录。
- 单文件、尺寸上限自轮转（`app/src/logfile.rs`），超过上限自动截断重写——不是无限增长。
- 数据目录与日志目录在 Unix 上均为 `0700`（审计2 #31），Windows 走 ACL。

## 每一条 IPC 调用都会落盘（`ipc:invoke` / `ipc:done`）

边界记账层在 `app/src/ipc_log.rs`，包在 `invoke_handler` 外层，**新增命令自动被覆盖**，
不用挨个命令手动打日志。每条前端 `invoke("命令名", 实参)` 都会写两行：

```
ipc:invoke  command=session_open  args={"profileId":"…"}
ipc:done    command=session_open  elapsed_ms=1234
```

- `command`：用户触发的命令名（`session_open`=双击连接、`profile_save`=保存档案、…）。
- `args`：实参全文，**已脱敏**（见下）。
- `elapsed_ms`：分发耗时——明显偏大 = 该命令卡住。

**定位「缺少参数 / missing required key」这类错误就看 `ipc:invoke` 的 `args`**：
Tauri 在进入任何命令函数**之前**反序列化实参，键名写错/缺字段会在这一步失败，错误只变成
前端一句 toast、命令体一行没跑——只有这条边界日志能留下「前端到底发了什么」。

## 命令内部失败也落盘（`error` 级）

`ipc:invoke` 只记入口。命令体内部的失败（连接失败/认证失败/校验不过/落库失败）由命令自身
在返回 `Err` 时打 `tracing::error!`，例如：

```
session_open 失败  profile_id=…  error=认证失败：…（3 次尝试后放弃）
profile_save 失败  error=名称不能为空
```

目前已在 `session_open` / `profile_save` 两个最常见入口补上；其余命令的错误串仍会原样走到
前端 toast，若某条命令的内部失败在日志里查不到原因，给那条命令补一条 `tracing::error!` 即可。

## 脱敏规则（`ipc_log.rs::redact`）

实参落盘前，命中以下键名的值整值掩成 `<redacted>`（大小写/下划线/连字符归一后匹配）：

`password` / `passphrase` / `secret` / `dataB64`（终端输入）/ `responses`（kbd 应答）/
`text`（剪贴板）/ `content`（导入原文）/ `current`·`newPassphrase`（改密）/ `private_key` /
`env`（环境变量整对象）。

**宁可多掩一个无害字段，不可漏一个秘密**。新增含秘密的 IPC 参数时，必须在
`SENSITIVE_KEYS` 加一条并补 `ipc_log.rs` 的单测——`sensitive_key_list_has_no_regression`
会钉住最小集不缩水。

## 已知边界（如实记下）

- **异步命令的返回值不在边界层记录**：Tauri v2 的 `respond_async` 在另一任务里结算，
  `invoke_handler` 包装闭包拿不到返回值（resolver 的 responder 藏在私有字段）。所以
  `ipc:done` 只记耗时不记成败；成败原因由「入口实参 + 命令自身的 `error!` 日志」共同覆盖。
- 日志级别默认 `future_shell_app=info,fs_=info`（`lib.rs` 的 `EnvFilter`），`debug!/trace!`
  默认不写。要看更细的栈，用环境变量 `RUST_LOG=future_shell_app=debug cargo tauri dev`。
