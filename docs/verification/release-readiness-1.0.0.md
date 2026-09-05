# 1.0.0 发版检查记录

检查日期：2026-09-05（Asia/Shanghai）。基础提交：`e549664`。
本记录随 1.0.0 候选代码提交保存；Git 提交与候选分支不等同于正式 tag 或公开 Release。

## 范围与结论

版本已统一为 1.0.0。Windows MSI/NSIS 候选包已生成，MSI 提取产物两次启动成功；尚不能宣布三平台正式发版就绪。
本机证据覆盖 Windows 构建与提取后启动；Linux 集成测试通过 Docker 验证，未构建 Linux 安装包；
macOS 和公开发行签名需对应环境。

## 已修复的发布问题

1. 主工作区、helper、Tauri、前端和锁文件仍停在 0.4.0；数据库版本现直接取 Cargo 包版本。
   版本门禁增加 helper、两个 Cargo.lock、npm 锁文件，带 2 个正例和 23 个反例自检。
2. 发布配置指定的 `tauri-cli 2.11.5` 不存在；已改为本机实际使用的 2.11.4。
3. CI 缺 helper 独立工作区的 fmt/clippy/test/deny，以及全新 checkout 所需的 sidecar 构建。
4. bundle 没有等待 notices artifact；已显式添加任务依赖。
5. helper 缺第三方许可材料和 SBOM；现随包分发独立 HTML，两个工作区均生成全目标 SBOM。
6. macOS 通用构建缺 universal helper；现用 lipo 合并并校验 x86_64/arm64。
7. Windows/Linux 安装 smoke 可能选到 helper；macOS 的 find 深度不足，找不到主程序。
   现精确定位主程序并检查 helper，匹配 NSIS 用户目录；deb 包名从产物读取，用 apt 安装运行时依赖。
8. 发布附件使用了正则式 artifact 匹配；现改为 glob 的扩展匹配形式。
9. smoke 的定时成功退出早于初始化完成；现等 RunEvent::Ready，且正常走退出清理。
   新增破损数据库 + 零毫秒退出参数的进程级回归，要求非零退出并产生对应诊断。
10. README 仍将已实现的 AI/MCP 列为禁用；已按当前代码列能力与真机验收边界。
11. 真 xrdp 输入测试没有归还 FrameAck，耗尽 helper 的在途帧预算后等不到新画面；
    Rig 现按生产消费端协议确认已收到的帧，保留击键必须产生新画面的原断言。
12. Linux/串口预检脚本仅排除根 target，可能把 helper 的大量构建缓存一并复制进容器；现排除所有 target 和任务缓存。
13. SBOM 解压在子目录，未被顶层校验和覆盖；现先摊平所有 Release 附件，再生成校验和和来源证明，
    文件名与 GitHub 下载附件一致，来源证明也覆盖校验和文件。收集 SBOM 时排除目标目录，避免重复复制。

14. 完成 [57 个界面单元的页面与交互审查](ux-review-1.0.0.md)，修复保存反馈、键盘操作、协议设置与文案；Windows 候选包已重新构建并再次完成两次提取启动验证。

## 验证记录

| 项目 | 结果 |
|---|---|
| 前端全量 Vitest | 117 文件，1,682 项通过（含页面交互审查新增的 23 项回归） |
| 修改后的前端定向回归 | 5 文件，74 项通过 |
| Svelte 类型检查 | 0 errors；RDP application 画布有 2 条既有可访问性提示 |
| 前端生产构建 / 对比度 | 通过 |
| npm 生产依赖安全审计 | 官方源 0 vulnerabilities；本机镜像不提供 audit endpoint |
| 主工作区 fmt / clippy / cargo deny | 通过 |
| 主工作区 cargo test | 原全量 1,643 项通过；修改后应用 375 项通过 |
| 主工作区 nextest | 1,644 项通过，5 项显式跳过；未设 FS_ITEST 时容器项仅经过环境门控 |
| RDP helper fmt / clippy / test / cargo deny / release build | 通过，47 项测试 |
| 版本门禁及自检 | 通过 |
| GitHub Actions actionlint | 通过 |
| Rust 两工作区及前端 SBOM | 已生成 |
| 发布附件整理及校验和 | 本地模拟解包：2 个安装包、3 份许可、14 份 SBOM 全部进入校验和，19 个文件验证通过；远程来源证明仍待 runner 执行 |
| Linux 串口伪终端 | 4 项集成测试通过：收发、波特率、重连、GBK；物理硬件未验 |
| Windows MSI / NSIS | 构建通过；MSI 隔离提取成功，主程序/helper/三份许可材料齐全，连续两次启动退出码均为 0 |
| Linux FS_ITEST=1 | 19 个测试文件分批验证完成：78 项通过，3 项 GUI 性能观测按既有 ignore 跳过；最后一批退出码 0 |

Linux 测试覆盖真实 OpenSSH/xrdp、认证和主机密钥、跳板、SFTP、录制回放、并发终端、
系统管理、端口转发及 rz/sz；MCP 客户端使用测试服务。记录兼容性测试曾因 Debian 下载连接被拒失败，
保持代码和断言不变重跑后通过。前半批日志为 `linux-itest-final.log`，后半批（含 record 重跑）为
`linux-itest-remaining.log`；不是一次完整 CI 运行的通过记录。

本地日志位于 `.cache/release-1.0.0/`（不入库）。本次清理了 Cargo 报告的约 40 GiB
`future-shell-app` 可再生产物，解决 C 盘满导致的链接和构建失败；没有修改用户业务数据。
C 盘满曾使 Docker 存储只读，经用户授权重启并恢复后完成上述测试，原有 `mysql-8` 容器正常运行。

## Windows 候选产物

产物保存在 `.cache/release-1.0.0/artifacts/`，该目录不入库。以下为包含本次交互修复的最新候选包。
本次构建与两次启动日志在 `.cache/ux-review/`；之前的候选包保存在 `.cache/archive/2026-09-05/pre-ux-artifacts/`。

| 文件 | 大小 | SHA-256 |
|---|---:|---|
| FutureShell_1.0.0_x64-setup.exe | 11,836,627 字节 | `f625ec3d7debcacb8ecdc46185dd2743d6881d91c2b62a541204f52a8556bbe0` |
| FutureShell_1.0.0_x64_en-US.msi | 17,108,992 字节 | `b4c9a5aa2d547e39f825d4ec2dc2dd0bd9d058bc5fb36a686e4f3e8944cc1141` |

同目录提供 `SHA256SUMS.txt`；两份安装包的 Authenticode 状态均为 `NotSigned`。
MSI 采用管理提取，在独立便携目录和独立 WebView 数据目录内连续启动两次，
确认初始化完成后退出、数据库创建成功、再次启动不受上次锁影响；主程序文件版本为 1.0.0。
本次没有执行 MSI/NSIS 的完整安装卸载，也未覆盖本机既有 0.4.0 安装。

## 正式发版前仍需完成

- 2026-09-05 推送候选分支前核对：GitHub Actions 已启用，仓库级 Actions secrets 列表为空，
  远程没有已有的工作流运行记录。Windows/macOS 签名所需凭据尚未配置，不能将本地验证视为正式发布通过。
- Windows/macOS/Linux GitHub runner 的 CI、bundle 和安装/覆盖安装/卸载 smoke 完整通过。
- 配置并实际验证 Windows Authenticode、macOS 签名及公证；现有 tag 流程强制要求签名，未放宽。
- 按 [人工核验清单](manual-checklist-1.0.0.md) 记录真实环境结果，尤其是 Windows NLA、
  输入法、音频、RDP 目录共享、物理串口，以及真实 AI provider/MCP 客户端。
- 最后审阅候选包和检查记录，再决定是否打 `v1.0.0` tag。

本地安装包未签名时仅用于验证，不等同于通过仓库签名要求的公开发行产物。
