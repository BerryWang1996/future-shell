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

## 2026-09-29 复查

候选分支首次 GitHub CI（2026-09-05，run 33942316287）：windows-2025 全绿，其余三处失败，此后无人处理。
本次逐条修复并补本地复现手段，另发现两处与 CI 无关的问题。

### 修复

1. **ubuntu clippy**：`crates/serial/tests/pty.rs` 在等待伪终端链接超时 panic 的路径上不回收 socat 子进程
   （`zombie_processes`）。文件整体 `cfg(unix)`，Windows 本地 clippy 看不到。改为 spawn 后立即交给 Drop 守卫。
2. **macos-14 构建 helper 即退出**：macOS 自带 bash 3.2 把 `"$PROFILE，"` 后的全角逗号字节读进变量名，
   `set -u` 下报 unbound variable。全仓脚本与工作流共 18 处同形，改为 `${VAR}`；bash 5（Linux/Git Bash）
   无法复现，因此新增静态门禁 `scripts/check-shell-portability.mjs`（6 例自检），接入 CI docs-gates、
   ci-local 与 ci-parity（门禁下限 11→12）。
3. **frontend**：runner 时区为 UTC，两条测试用 `-getTimezoneOffset()` 作期望值得到 `-0`，而被测函数
   刻意归一为 `+0`。改为 `0 - x`，并新增运行期切到 UTC 的用例，使该路径在任何开发机上可复现。
   变异：去掉归一后新用例在东八区本机转红。另把 AiPanel 打开耗时的绝对预算由 150ms 放宽到 1000ms
   （并行全量实测 189ms 即红；真正守时延的是同文件「全程只一次 IPC」的结构断言）。
4. **RDP 音频从未可用**：helper 连接参数 `enable_audio_playback: false`（注释写「阶段 3 再开」，阶段 3
   交付后未翻）。该值使 Client Info PDU 带 INFO_NOAUDIOPLAYBACK，服务器按协议不做音频重定向。已改为 true，
   新增单测并经变异验证。真 Windows 上的实际出声仍属人工核验 V15-d。
5. **依赖安全公告**（09-05 后新发布）：rustls < 0.23.45（RUSTSEC-2026-0285，主工作区与 helper 的 RDP TLS
   均受影响）、cryptoki 0.12.0（RUSTSEC-2026-0286，helper 经 sspi 引入），均以补丁级更新修复；许可材料快照按
   发布流程重新生成。前端开发依赖 5 条公告（vitest mocker 路径穿越等，不进安装包）以兼容更新修复；
   `package-lock.json` 的 resolved 地址统一为 registry.npmjs.org（此前指向本机镜像源；npm 默认会把官方地址
   替换为使用者配置的源，镜像用户不受影响）。
6. **Linux 专属测试缺陷**：RDPDR 联接逃逸测试调用 Windows 的 `mklink`，Linux 上必红，改为按平台建链接。

### 发布策略

维护者裁定不购买商业代码签名证书。`release.yml` 增加仓库变量 `ALLOW_UNSIGNED_RELEASE=true` 显式放行：
缺证书的平台在 tag 构建中警告继续，macOS 包打 ad-hoc 签名，draft Release 正文自动写明未签名平台、
`sha256sum -c` 与 `gh attestation verify` 核对方法和 SmartScreen / Gatekeeper 放行步骤。变量缺失时仍在
签名闸失败。actionlint 通过。README 增加「安装包签名」一节，CONTRIBUTING 记录开关与分支约定。

候选分支按开源惯例更名为 `release/1.0.0`（全量历史版为 `release/1.0.0-full-history`，仅本地）。

### 验证记录（2026-09-29）

| 项目 | 结果 |
|---|---|
| Windows ci-local（20 步，`CARGO_INCREMENTAL=0`） | 全绿；nextest 1,644 通过（5 跳过），cargo test 1,644 通过，helper 48 通过 |
| 前端 Vitest | 117 文件 1,683 项通过；`TZ=UTC` 下同样全绿 |
| svelte-check | 0 errors（RDP 画布 2 条既有可访问性提示） |
| cargo deny 两工作区 / npm audit（生产与开发依赖） | 全部通过 / 0 vulnerabilities |
| Linux：`scripts/linux-ci-mirror.sh`（新增，按 ci.yml 原顺序在容器里跑 ubuntu check 作业） | 前端 dist、helper、fmt ×2、clippy ×2（含 app crate 与 Tauri Linux 全栈）、helper test、nextest 1,657/1,657、cargo test 全绿 |
| 真 xrdp itest（音频开关打开后） | 4 项中 3 项通过（连接认证、键盘→重绘、拒证书）；第 4 项两次都卡在测试容器启动时从 deb.debian.org 安装 xrdp（本机实测约 300 kB/s），与改动无关（该用例在 NLA 认证阶段失败，早于携带音频标志的 Client Info PDU），留待 GitHub runner |
| 版本门禁 / 文档链接 / 验收分类 / CI 对齐 / 性能门禁对齐 | 全部通过 |

macOS 仍无本地替代；macOS 专属代码仅 3 处且均为平台常量分支，其余 Unix 代码已由 Linux 镜像覆盖。

## 正式发版前仍需完成

- **推送修复**：推送 `release/1.0.0`，以它重开 PR 并关闭 #1。
- Windows/macOS/Linux GitHub runner 的 CI 全绿（含 Linux `FS_ITEST=1` 容器 itest）。
- 在仓库 Variables 设 `ALLOW_UNSIGNED_RELEASE=true`，用 `workflow_dispatch` 跑一次 release.yml，
  确认三平台 bundle 与安装/覆盖安装/卸载 smoke 通过。
- 按 [人工核验清单](manual-checklist-1.0.0.md) 记录真实环境结果，尤其是 Windows NLA、
  输入法、音频（本次修复后首次可验）、RDP 目录共享、物理串口，以及真实 AI provider/MCP 客户端。
- PR 合入 `main` 后在该提交上打 `v1.0.0` tag，审阅 draft Release（正文含未签名说明）后发布。
