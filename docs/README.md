# 文档中心

当前版本为 1.0.0 候选代码。功能实现、实际验证和历史设计分别记录；发版结论以最新验证报告为准。

## 使用与开发

| 文档 | 内容 |
|---|---|
| [项目说明](../README.md) | 当前能力、快速启动、目录与出站连接 |
| [开发与发布指南](../CONTRIBUTING.md) | 环境、构建、测试、发布步骤和维护约定 |
| [更新记录](../CHANGELOG.md) | 版本变更 |
| [便携模式](guides/portable-mode.md) | 数据位置、移动使用与应用标识 |
| [日志与故障排查](guides/logging.md) | 日志位置、IPC 记录、脱敏与诊断边界 |

## 设计与进度

| 文档 | 内容 |
|---|---|
| [系统设计](design/architecture.md) | 架构、数据流、安全和分期设计基线 |
| [UI 规格](design/ui.md) | 界面布局、主题、快捷键和交互基线 |
| [里程碑路线图](roadmap.md) | 范围、出口标准和历史实施记录 |
| [主界面基线](mockups/index.html) / [SFTP 基线](mockups/sftp.html) | 用于人工界面对照的静态原型 |

设计与路线图包含早期取舍和后续设想，不能用其中的阶段状态代替当前能力矩阵。

## 验证与历史

| 文档 | 内容 |
|---|---|
| [1.0.0 发版检查](verification/release-readiness-1.0.0.md) | 本次修复、测试证据、候选产物与剩余条件 |
| [1.0.0 页面与交互审查](verification/ux-review-1.0.0.md) | 57 个界面单元的覆盖、交互修复与验证范围 |
| [1.0.0 人工核验](verification/manual-checklist-1.0.0.md) | 真机操作步骤与通过判据 |
| [M1 验收载体](verification/phase1-acceptance.md) | 历史验收条目，供回归脚本检查 |
| [性能验证](verification/performance.md) | CI 负载测试与 GUI 自查的范围 |
| [历史记录索引](archive/README.md) | 合并的任务、审计和性能记录摘要 |
| [历史审计原文](archive/audits.md) | 已有审计裁决及 42 项发布问题的原始证据 |

## 目录维护

每项说明只维护一个主入口，通过链接引用。当前报告只记录真实运行结果，历史数字不覆盖当前验收。
生成日志、下载工具和临时脚本放在被忽略的 `.cache/`，不放进 `docs/`。
移动或删除文档后运行 `node scripts/check-doc-links.mjs`，并执行受影响的文档门禁。
