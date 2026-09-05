# 1.0.0 页面、交互与文案审查

日期：2026-09-05。审查对象为当前工作区，包含此前的 1.0.0 发版修复与文档整理。

## 范围与判据

对 `App`、`ViewWindow` 和 55 个 Svelte 组件共 57 个界面单元逐项盘点入口、用户文案、空状态、禁用条件、错误反馈、提交和退出操作。
重点审查保存与重试、协议设置、键盘操作、文件操作以及执行确认的实现，并通过组件测试和浏览器交互预览验证。
判据是让用户知道“在操作什么、需要填什么、是否已生效、失败后怎么办、如何退出”。

本次修复已落实到代码，不以“完美”或“用户永远不会有疑问”作为不可验证的结论。
浏览器预览使用本地模拟 IPC 和虚构数据，不连接远端、不读取真实凭据，不代替三平台原生环境与硬件验收。

## 修复结果

| 操作范围 | 原问题 | 当前行为 |
|---|---|---|
| 首次进入 | 空白工作区只有状态文字 | 提供新建会话、导入配置入口和侧栏操作说明 |
| 新建与编辑连接 | 标题相同；必填反馈不明确；可以重复保存 | 区分新建/编辑，校验名称、目标、用户名、端口、字号和滚动行数；定位错误字段，保存期间阻止重复提交 |
| 连接密码 | 档案已创建而密码保存失败，重试可能重复新建 | 保留已创建的档案 ID，明确提示密码未保存，重新填写后更新同一档案 |
| 配置异步读取 | 等待默认主机策略时可能覆盖用户刚填写的字段 | 同步回填表单，异步结果仅更新尚未手动修改的默认策略 |
| 协议差异 | RDP 显示 SSH 专用设置；串口保留无效页签 | RDP 显示常规/认证/跳板；SSH 私钥与交互认证选项不出现在 RDP 中；串口显示常规/终端 |
| 设置保存 | 保存失败仅记控制台，后续操作可能报告成功 | 写入返回明确结果；失败提示与重试按钮持续可见；失败不广播新设置，关键调用方不再报告假成功 |
| 外部 MCP 配置 | 空格切分破坏路径；未填完整即写入 | 每行一个参数，保留路径空格；完整填写后保存列表；检查缺项和重复标识 |
| MCP 授权与复制 | 工具只有内部名称；复制无反馈 | 增加中文用途说明；复制成功或失败均有反馈 |
| 模态与页签 | 部分弹窗焦点可逃逸，方向键支持不完整 | 补齐连接、隧道、详情、MCP、软链与发送目标选择的焦点管理；跳过隐藏/inert/禁用控件；设置分类支持方向键和 Home/End |
| 主菜单 | 缺方向键、Escape 和外部点击收起行为 | 支持打开、逐项移动、进入/退出子菜单、关闭后归还焦点；隐藏开发阶段占位菜单 |
| 会话恢复 | 内部编号被当作会话名，目标始终为空 | 展示已保存的名称与地址/串口；缺失配置明确标记；修复弹窗内 Escape 无效 |
| 快速命令集 | 删除最后一项后无法保存；异步失败也关闭 | 支持保存空列表，失败保留草稿，成功才关闭；区分保存条目与保存整个命令集 |
| 中文输入 | 输入法选词 Enter 可能触发提交/发送 | 文本输入、AI 提问、命令栏、历史/搜索等入口忽略组合输入中的 Enter |
| RDP 输入 | 本地共享控件中的按键可能继续发送远端 | 仅转发远程画布/容器输入；Ctrl+Alt+Home 返回本地控件，并释放远端按键状态 |
| 文件操作 | 过滤后无结果显示“空目录”；软链失败关闭表单 | 区分空目录与过滤结果；读取失败提供重试；软链失败保留表单和就地错误 |
| 隧道 | 无会话时缺可见关闭按钮 | 明确先连接 SSH 的步骤，无会话仍能关闭；展示启动中与累计连接数 |
| 计划任务 | 缺常用表达式入口，保存可重复触发 | 提供每小时、每天/工作日 09:00 预设；缺连接时提示；保存防重复，保留执行条件说明 |
| 保险库与主机信任 | 内部术语多，操作依据与下一步不够明确 | 说明本机凭据库与应用密码的真实关系；管理页提供解锁入口；指纹提示说明核对来源与记录信任的效果 |
| 通用文案与视觉 | 出现 MVP/阶段编号/规格引用、HTML 中裸 Markdown、禁用按钮不明显 | 清除面向用户的开发占位与无关实现说明；修正粗体标记；键位动作使用中文名称；禁用按钮统一视觉反馈，长通知可换行 |

## 覆盖清单

下列分组完整覆盖原有 57 个界面单元。未修改的组件也检查了入口、操作含义和现有状态处理；不将静态检查冒充实际远端执行。

| 分组 | 已检查的页面/组件 |
|---|---|
| 主框架与窗口（9） | App、ViewWindow、MenuBar、ToolBar、Sidebar、TabBar、StatusBar、VirtualWindow、VWindowLayer |
| 会话与连接（7） | ProfileDialog、ConnectFailurePanel、ConnectionDetailDialog、SessionRecoveryDialog、EncodingDialog、SerialBaudDialog、TunnelDialog |
| 终端与命令（8） | TerminalPane、ComposeBar、QuickBar、QuickCommandsDialog、HistoryDialog、SearchOverlay、ReplayDialog、ScriptRunDialog |
| 文件与远程桌面（7） | SftpPane、FilePickerDialog、FilePropsDialog、PasteConflictDialog、TransferQueueDrawer、ZmodemBar、RdpPane |
| 设置与导入（5） | SettingsDialog、KeymapEditor、HighlightEditor、SchemeEditor、ForeignImportDialog |
| 凭据与信任（6） | VaultDialog、VaultManagerDialog、KeyManagerDialog、AuthPromptDialog、HostKeyDialog、RdpCertDialog |
| AI、监控与审计（6） | AiPanel、AgentPanel、McpConfirmDialog、MonitorPanel、ScheduleDialog、AuditDialog |
| 通用交互（9） | ActionConfirmDialog、CloseConfirmDialog、ConfirmDialog、DeleteConfirmDialog、ErrorDetailDialog、TextInputDialog、Toast、Markdown、Icon |

## 验证证据

- 前端全量回归：117 个测试文件、1,682 项通过，比本次审查前增加 23 项；包括真实组件事件、IPC 失败、焦点、中文输入法和 RDP 本地控件隔离。
- Svelte/TypeScript：0 errors；保留 RDP `role="application"` 容器的 2 条既有静态可访问性提示。容器需要接收远端键盘输入，本次补充了返回本地控件的键盘出口。
- 主题对比度检查全部通过；生产构建与 Windows 候选安装包状态见[发版检查](release-readiness-1.0.0.md)。
- 浏览器实际检查：主界面与菜单键盘导航、新建连接/协议切换/必填失败、设置保存失败与 MCP 表单、无会话隧道、连接详情、快速命令集、恢复会话、文件选择、密钥管理、保险库、AI、Agent、审计、主机指纹确认、MCP 强确认、计划任务。检查 1280×720 常规尺寸及应用允许的最小 800×600 尺寸，确认表单可滚动、关键操作可达。
- Windows MSI/NSIS 已重新构建，MSI 在隔离的便携数据目录中连续启动两次，退出码均为 0。候选包未签名。
- 临时预览、清单和执行日志集中在被忽略的 `.cache/ux-review/`，未向项目根目录散放临时文件。

## 发布边界

这次审查覆盖前端行为与文案，未改变 Rust 协议实现或依赖。
SSH/SFTP/RDP/串口真实端到端证据沿用同日[发版检查](release-readiness-1.0.0.md)；macOS/Linux 安装包、系统输入法差异、辅助技术及真实串口硬件仍按[人工核验清单](manual-checklist-1.0.0.md)验证。
新增或修改页面时，应继续保留必填/空状态/保存失败/取消/键盘操作这几类检查。
