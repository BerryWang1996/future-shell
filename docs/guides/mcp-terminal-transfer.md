# MCP 终端文件传输（1.1.1 开发版）

1.1.1 开发版为当前交互终端增加 `terminal.upload`、`terminal.download`。
工具接线与验证状态见[开发记录](../design/plan-1.0.5-mcp.md)，正式版本以 Release 为准。

## 选择传输通道

| 工具 | 文件实际从哪里传输 |
| --- | --- |
| `sftp.upload` / `sftp.download` | `sessions.details` 显示的原始 SSH 连接端点 |
| `terminal.upload` / `terminal.download` | 当前交互终端内运行的 `rz` / `sz`，可用于经过堡垒机菜单或嵌套 SSH 后的终端 |

交互终端切换主机不会改变 SFTP 或 `command.run` 的端点。终端传输的结果不验证当前主机身份；
开始前用 `terminal.read` 核对目标，并确认已进入干净的 POSIX shell 提示符。

## 一次配置

在连接编辑器的 AI 页允许 MCP 访问 SSH 配置，并指定本机上传目录。
在设置中启用 MCP 服务、勾选两个终端传输工具，并启用“终端内传输（rz / sz）”。
这两个工具默认关闭。关闭自动接收时，显式 MCP 传输仍可预约。

远端需要已有 `rz` / `sz`。工具不会安装程序，也不会自动改用 SFTP 或 Base64 命令。
两个工具均按危险操作处理，沿用执行档位与 MCP 免逐次确认设置；只读及禁止规则仍有效。

## 从新会话开始

1. 调用 `profiles.list` 查找已允许的配置，再将 `profile_id` 传给 `sessions.open`。
2. 使用返回的会话 ID 操作终端；同一配置可打开多个独立会话，无须先在应用里点开服务器。
3. 如需经过交互菜单，用 `terminal.send` 的 `press_enter=true` 提交选项，再读取结果。
4. 确认目标和提示符后，调用下面的传输工具，并用任务 ID 查询结果。

上传参数示例（MCP `tools/call` 的参数）：

```json
{
  "name": "terminal.upload",
  "arguments": {
    "session_id": "返回的会话ID",
    "local_path": "build/package.tar.gz",
    "remote_dir": "/tmp"
  }
}
```

`local_path` 是相对已授权本机上传目录的普通文件路径，`remote_dir` 是远端目录，目标名采用源文件名。
下载使用 `terminal.download` 的 `session_id` 和 `remote_path`，其中 `remote_path` 是单个远端文件路径。
路径中的空格、中文和引号作为字面值处理；`~`、变量及通配符不展开。控制字符和协议不能保真的文件名会被拒绝。

下载保存到对应连接的本机下载沙箱，不覆盖已有文件；结果通过 `local_relative_path` 给出最终相对路径。
安全编码可能将冒号等字符转义，不要用远端原名猜测本机落点。失败时可能保留 partial，按返回的恢复信息检查。
预约被明确拒绝时，错误可包含 `accepted:false` 和临时文件的相对路径；这类请求没有已受理任务 ID，不要对它查询不存在的任务。

## 进度、取消和结果

提交返回 `transfer_id`、`accepted` 和 `observe_with`；`accepted=true` 表示已受理。
用 `transfer.status` 查询，或用 `transfer.list` 查找本次运行的任务。提交响应丢失时先查任务，避免重复上传。

终端传输标识为 `terminal_zmodem`、`interactive_terminal`，`peer_identity_verified=false`。
`remote_path` 表示请求意图；完成时的 `protocol_confirmed` 表示协议及本地 IO 完成，不是远端 SHA-256 或主机身份核验。

调用 `transfer.cancel` 后继续查任务；取消请求不等于远端进程已退出。连接或驱动退出而未获得终态时，结果会明确标为未知。
同一终端的传输占用期间，普通输入和长操作返回 `FS_MCP_CONFLICT`；读屏、查进度及取消仍可调用。
下一次传输前再次检查提示符，不把上一任务的本地终态当作远端 shell 已恢复的证明。

接收实际字节受 4 GiB 上限约束。握手、活动空闲和协议收尾分别有等待预算；这些不是整个请求的硬耗时上限。
