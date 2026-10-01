# FutureShell

支持 SSH/SFTP、RDP 与串口的桌面终端工具，采用 Rust、Tauri 2、Svelte 5 和 xterm.js。Apache-2.0。

当前版本为 **1.0.0**（2026-10-01）。三平台安装包（Windows MSI/NSIS、macOS DMG、Linux deb/AppImage）
由发布流水线构建，并通过安装、启动、覆盖重装与卸载的冒烟验证；安装包未使用商业签名（见下文
「安装包签名」），部分真机交互项仍在人工核验中，详见 [发版检查记录](docs/verification/release-readiness-1.0.0.md)。

[文档中心](docs/README.md) · [开发与发布](CONTRIBUTING.md) · [更新记录](CHANGELOG.md)

## 当前能力矩阵（1.0.0）

| 能力 | 状态 |
|---|---|
| SSH（密码 / 私钥 / 本地 Agent / keyboard-interactive）与逐跳认证 | ✅ 已实现 |
| TOFU / 严格 / 指纹钉扎、known_hosts 导入 | ✅ 已实现 |
| 多标签终端、分屏、独立窗口、应用内窗口、主题与配色 | ✅ 已实现 |
| SFTP 浏览、上传下载、断点续传、传后校验、文件属性与编辑 | ✅ 已实现 |
| 本地 / 远程 / 动态端口转发 | ✅ 已实现 |
| Vault、共享凭据、连接档案、导入导出、便携数据目录 | ✅ 已实现 |
| 系统监控、进程与服务管理、命令历史、定时任务 | ✅ 已实现 |
| AI 对话、Agent 执行与确认、审计、MCP Server / Client | ✅ 已实现，需配置模型或外部工具 |
| 终端录制回放、rz/sz、GBK / GB18030 / Big5 等编码 | ✅ 已实现 |
| 串口（UART）连接 | ✅ 已实现，硬件交互仍需真机验收 |
| RDP、SSH 跳板、动态分辨率、文本剪贴板、音频、目录共享 | ✅ 已实现；Windows NLA、输入法、音频和 RDPDR 仍需真机验收 |
| SFTP 跟随交互终端 sudo su 自动提权 | ❌ 未实现 |
| SSH Agent forwarding（将本地代理转发给远端） | ❌ 未实现；本地 Agent 认证已支持 |
| 手机控制 | ❌ 未实现 |

## 仓库结构

| 路径 | 说明 |
|---|---|
| `app/` | Tauri 应用壳、IPC、窗口、安装配置与第三方许可材料 |
| `frontend/` | Svelte 界面、xterm.js 终端与前端测试 |
| `crates/` | 10 个 Rust 核心库及独立的 `itest` 集成测试包 |
| `rdp-helper/` | RDP 子进程，独立 Cargo 工作区、锁文件与质量检查 |
| `scripts/` | 构建、质量检查与平台验证脚本 |
| `.github/` | CI、发布流水线和版本门禁 |
| `docs/` | 设计、操作说明、验证记录、历史归档与界面基线 |

核心库目录为 `vault`、`connmgr`、`sshengine`、`terminal`、`policy`、
`ai`、`mcpbridge`、`audit`、`serial`、`rdpproto`，包名使用 `fs_` 前缀。

## 快速开始

需要 Rust 1.97.1、Node.js 24 和 Tauri CLI 2.11.4；Windows 还需 MSVC Build Tools、
Windows SDK、NASM 与 Git Bash。平台前置和完整命令见 [贡献指南](CONTRIBUTING.md)。

在仓库根目录执行：

```bash
npm --prefix frontend ci
bash scripts/build-rdp-helper.sh --debug
cargo tauri dev
```

Tauri 配置位于 `app/`；开发命令从仓库根目录运行。RDP helper 属于独立工作区，
必须先构建并复制到 `app/binaries/`，主工作区不会自动编译它。

## 安装包签名

FutureShell 不使用商业代码签名证书。Windows 首次运行可能出现 SmartScreen「未知发布者」提示，
macOS 包仅 ad-hoc 签名、未经 Apple 公证。每个 Release 附带 `SHA256SUMS.txt` 与 GitHub 构建来源证明，
下载后可用 `sha256sum -c SHA256SUMS.txt --ignore-missing` 或 `gh attestation verify <文件> --repo <仓库>` 核对。

## 数据与便携模式

默认数据目录为系统配置目录下的 `future-shell/`。在可执行文件旁放置空的
`portable.txt`，可将数据保存到相邻的 `data/`。
完整目录、迁移注意事项见 [便携模式](docs/guides/portable-mode.md)，
故障定位见 [日志说明](docs/guides/logging.md)。

## 出站面

**零遥测**：不上报使用数据、不回传崩溃记录、不主动检查更新。只连接用户配置的目标：

| 目标 | 触发条件 |
|---|---|
| SSH/SFTP、RDP、端口转发 | 用户连接会话或启用转发规则 |
| AI provider / 本地 Ollama | 用户在 AI 面板发起请求 |
| 外部 MCP 工具 | 用户配置并启用对应服务 |
| 更新清单 | 配置更新地址后，手动点击“检查更新” |

更新地址没有默认值；检查只返回链接，不下载或替换程序。凭据和敏感输入按日志脱敏规则处理。
出站依赖约束由 `frontend/src/lib/egress-contract.test.ts` 检查。
