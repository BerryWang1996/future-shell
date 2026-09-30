# FutureShell UI 设计规格

> 本文是界面设计基线；交付范围与真机验收状态见 [1.0.0 发版检查](../verification/release-readiness-1.0.0.md)。

版本 v1 · 2026-07-26 · 隶属总设计 `docs/design/architecture.md`

## §0 设计目标与对标

**对标对象**（用户指定）：

| 产品 | 借鉴点 | 超越点 |
|---|---|---|
| Xshell 7（SSH 面完全体；TELNET/RLOGIN/SERIAL/RAW/本地 Shell 不支持，见路线图 §2.1 不对标项声明） | 整体布局语言：菜单栏 + 工具栏 + 可停靠会话管理器 + 标签 MDI + 底部组合（Compose）命令栏 + 状态栏；每会话配色方案；快速命令按钮集；键盘配置文件 | 现代化视觉（扁平、高 DPI、暗色优先）、CSS 主题引擎可换肤（Xshell 换肤能力弱）、AI 原生面板 |
| Xftp 7（SFTP 面完全体；FTP/FTPS 不支持，文件传输仅走 SFTP） | 双栏文件浏览器 + 底部传输队列抽屉、地址面包屑、同步浏览 | 标签内终端/SFTP 无缝分屏（Xshell+Xftp 合体，免切换程序） |
| FinalShell（完全体） | 底部服务器监控面板（CPU/内存/网络实时图）、命令片段库、主机列表状态灯 | 监控与终端/AI 上下文联动（异常指标一键 AI 诊断，M2+M4b）、片段库可 AI 生成 |

**设计原则**：

1. **Xshell 式肌肉记忆**：老用户零学习成本——会话管理器默认左侧停靠（与 Xshell 通行布局一致）、组合命令栏在底部、标签即会话。
2. **信息密度优先**：运维工具，不是消费级 App。默认紧凑密度，4px 间距栅格，16px 图标。
3. **暗色优先 + 多皮肤**：默认暗色主题 Obsidian；应用主题与终端配色方案**双轴独立**切换；自带 ≥4 套应用主题、≥10 套终端配色。
4. **状态一目了然**：连接态（连接中/已连接/断开/错误）在标签、侧栏树、状态栏三处一致呈现。
5. **核心路径零遮挡**：终端渲染区是绝对主角；所有面板可折叠/隐藏（会话管理器浮动/拖右 M4b，MVP 固定左停靠，§1.1）。

> 注：本规格基于知识库中的 Xshell 7 / FinalShell 形态编写（本会话 web 调研工具网关故障，未能现场抓取官方页面佐证）。停靠方向以总设计 §2.4 第 2 项（左侧会话管理器侧栏）为准并与之对齐；如后续实机核验发现 Xshell 通行默认不同，以实机为准一次性修订三处（本规格、总设计、实现计划）。

---

## §1 窗口布局

### §1.1 总体骨架（默认态）

```
┌────────────────────────────────────────────────────────────────────────────┐
│ 标题栏：FutureShell — [会话名]                                     _ □ ×   │
├────────────────────────────────────────────────────────────────────────────┤
│ 菜单栏：文件(F) 编辑(E) 视图(V) 工具(T) 窗口(W) 帮助(H)                     │
├────────────────────────────────────────────────────────────────────────────┤
│ 工具栏：[＋新建][🗀]│[✂][⧉][📋]│[🔍][📷]│[⚡快速命令][📢广播]│[🎨皮肤][⚙]│
├─────────────────────────────┬──────────────────────────────────────────────┤
│  会话管理器（左停靠）        │ 标签栏：● 生产-Web-01 │● 数据库-02│⚠ 测试-03│＋│
│  🔍 搜索会话                ├──────────────────────────────────────────────┤
│  ▼ 📁 工作                  │                                              │
│    ▼ 📁 生产                │   终端渲染区（xterm.js）                      │
│      🟢 Web-01              │                                              │
│      🟢 Web-02              │   user@web-01:~$ htop                        │
│    ▶ 📁 数据库              │   ▓▓▓▓▓░░░░  CPU 45%                         │
│  ▼ 📁 个人                  │   ...                                        │
│      ⚪ nas                 │                                              │
│  ─────────────────          │                                              │
│  [＋新建][📥导入][📤导出]    │                                              │
├─────────────────────────────┴──────────────────────────────────────────────┤
│ 组合命令栏：[目标: 当前会话 ▾] [ user@web-01:~$ 输入命令…          ] [发送⏎]│
├────────────────────────────────────────────────────────────────────────────┤
│ 状态栏：🟢 已连接 192.168.1.10:22 │ UTF-8 │ xterm-256color │ 24×80 │ 🔒 │Caps│
└────────────────────────────────────────────────────────────────────────────┘
```

- **会话管理器默认左侧停靠**（与总设计 §2.4 第 2 项及 Xshell 通行布局一致）；**MVP 固定左停靠**（拖至右侧/浮动 M4b，路线图 §2.1 矩阵与 §2.2 M4b 范围），可隐藏（View 菜单 / `Ctrl+Shift+S`）。宽度默认 240px，可拉伸，记忆上次宽度（停靠侧记忆 M4b，settings 键 `ui.sidebarDock`）。
- 会话管理器树节点右侧可挂「状态灯」（已连接绿、离线灰空心、断开灰、错误红）；双击 = 打开会话（已打开则聚焦对应标签）。
- 窄屏（<1100px）时会话管理器自动转为抽屉式覆盖层（**M4b**；MVP 窄屏下仅可经 `Ctrl+Shift+S` 手动隐藏侧栏，不做自动覆盖层；路线图 §2.2 M4b 范围/出口已补 matchMedia 1099px 验收）。
- **骨架图仅示意**：工具栏按钮以 §2.2 为权威，菜单以 §2.1 为权威（如骨架中的「剪切」按钮 MVP 无对应命令，相位注记以各项所在章节为准）。

### §1.2 标签 MDI

- 标签 ↔ session_id 1:1（总设计 §2.4）；标签内容：状态图标 + 名称 + 关闭 ×（悬停显示）。
- 标签过多时：左右滚动箭头 + 下拉列表（`Ctrl+Tab` MRU 切换）。
- 窗口（Window）菜单支持「新建窗口 / 层叠 / 水平平铺 / 垂直平铺」（**全菜单项 Phase 4（M4b）**，MVP 不提供多窗口）——同一会话可在多个视图中并排查看（平铺为视图克隆，输入同步到同一会话）。
- 标签拖出主窗口 → 新建独立窗口（Phase 4b；MVP 先做同窗拖拽排序）。

### §1.3 底部监控抽屉（FinalShell 式，可折叠）

- 位置：终端区与组合命令栏之间，默认折叠（高度 0），展开 120px。
- 内容：当前会话主机的 CPU / 内存 / 磁盘 IO / 上下行网速 四条迷你实时图（ring buffer 60s）+ 系统负载摘要行。
- **相位标注**：数据采集 M4a 实装；MVP 仅预留本折叠插槽（展开显示占位文案「服务器监控随 M4a 实装」）。
- 数据来源（M4a）：经会话 exec 通道周期采集（`/proc/stat`、`free`、`/proc/net/dev`；非 Linux 降级为 `top`/`vm_stat` 解析或隐藏），间隔 2s，**不占用 PTY 通道**（总设计 §2.1 双通道）；采集失败静默降级。
- **标签右键菜单（独立定义，§3.2/§2.11 等处均引用本条）**：MVP 项 = 关闭标签 / 关闭其他标签（M4b，MVP 禁用态预埋） / ─ / 配色方案 ▾（子菜单：全部方案（内置 12 套）+『跟随默认』清除项（清除本会话临时覆盖、回落全局/Profile 合成值）；选中 = 会话级临时覆盖，§3.2，不写库、关标签即释放（断线重连期间保留，§5 断线态标签存活））；M4a 追加 = 高亮关键字集 ▾（§2.11）/ 关闭监控 / 刷新间隔 ▾。

### §1.4 标签内 SFTP 分屏（Xftp 合体）

- 终端标签工具行：「终端 | 文件 | 分屏」三态（总设计 §2.4）。
- 分屏：左终端 / 右双栏 SFTP，分隔条可拖拽，比例记忆。
- SFTP 双栏各自带地址面包屑与 返回/上级/刷新 按钮；**远程栏**另带 新建目录/删除（二次确认）/重命名（单项）——本地栏仅浏览与上传，本地侧新建目录/删除/重命名是否对偶列入 M4b 评估（路线图 §4 M4b 范围）；两栏均支持拖拽传输（本地⇄远程）、多选、排序；隐藏文件开关单处呈现（本地栏工具行）、双栏同时生效。
- 「传输队列」抽屉（TransferQueueDrawer）：主窗口全局抽屉，位于终端区与组合命令栏之间、与监控抽屉（§1.3）并列分层；默认折叠，有新传输自动展开；**跨标签持久可见**（传输为会话级，不随标签切换消失）；进度条、速率、ETA、失败重试按钮、全部取消（M4a：MVP 单件取消/重试在，抽屉批量取消归 M4a，路线图 §2.2 增量）。
- 传后校验（可选开关，默认开）：传输完成后经 exec 通道计算远端 sha256sum 与本地比对；远端无 `sha256sum` 命令（含禁 exec）时降级为 size 比对并在 UI 显式标注「降级」；禁止静默跳过（总设计 §2.3、路线图 M1 出口）。
- 同步浏览说明：本规格分屏仅含双栏独立浏览 + 导航联动（M4a 同步浏览，路线图 §2.2）；文件镜像同步（文件夹同步/比较）属 M4b 评估项，MVP/4a 不含。

---

## §2 组件清单

### §2.1 菜单栏（完整项，MVP 必须项加粗）

- **文件**：**新建会话(N) Ctrl+N**、新建文件夹（M4a：分组随会话 `group_path` 自动建立，MVP 该项禁用态标注，手动新建文件夹归 M4a）、打开会话目录…（M4a：在文件管理器中显示该会话的日志/录屏/下载沙箱所在目录，经 tauri-plugin-opener 的 reveal-item-in-dir）、**导入(JSON)**、导入(Xshell/FinalShell 格式，M4b)、**导出**、**退出**
- **编辑**：**复制 Ctrl+C / Ctrl+Shift+C**、**粘贴 Ctrl+V / Ctrl+Shift+V**、复制为纯文本、全选、**查找 Ctrl+F**、查找下一个 F3、清除屏幕 Ctrl+L、清除滚动缓冲
- **视图**：**会话管理器 Ctrl+Shift+S**、**监控抽屉 Ctrl+Shift+M**、组合命令栏、状态栏、工具栏、**全屏 F11**、字体放大 Ctrl+= / 缩小 Ctrl+- 、语言（Phase 4b；MVP 硬编码单语言简体中文，不呈现切换项）、**应用主题**（子菜单：全部主题）
- **工具**：**选项(设置) Ctrl+,**、**Vault 解锁/锁定**、快速命令集编辑器（M4a）、高亮关键字…（M4a）、**配色方案编辑器**（Phase 4b）、密钥/代理管理器（M4b：Vault 密钥浏览[仅指纹/类型/注释]与 agent 传输开关，路线图 M4b 范围）、**AI 助手 Ctrl+Shift+A**（Phase 2）
  > `Alt+P` 保留给「会话属性」（Xshell 习惯，见 §4），设置入口不与之冲突。
- **窗口**（全菜单项 Phase 4（M4b）；MVP 的关闭路径仅 标签× / 中键 / Ctrl+W / 窗口关闭 四路，见 §2.9）：新建窗口、层叠、水平平铺、垂直平铺、关闭全部标签、会话列表…
- **帮助**：**文档**、**快捷键一览**、**诊断日志目录**、**关于**

### §2.2 工具栏图标（16px，轮廓风格，悬停 tooltip + 快捷键）

`新建会话` `打开目录(M4a，MVP 隐藏；= 菜单「打开会话目录」同一 reveal-item-in-dir 入口，§2.1)` | `复制` `粘贴` | `查找` `截图(Phase4b，路线图 M4b 范围)` | `快速命令集 ▾(M4a，MVP 禁用态并标注「即将推出」)` `广播开关 📢(M4a)` | `皮肤 🎨 ▾` `设置 ⚙` `AI ✨(Phase2)`

- 工具栏可右键自定义（勾选显隐、拖拽排序），配置持久化于 settings 键 `ui.toolbarLayout`（**M4b**：MVP 工具栏为固定项集、右键无菜单；路线图 M4b 范围/出口对偶）。

### §2.3 会话管理器树

- 节点：分组（可折叠 = MVP；**右键菜单整体归 M4a**——重命名/新建子组/删除[非空需确认]三项同相位：MVP 的分组是会话 `group_path` 自动派生的视图、无独立分组实体可改名或删除，故 MVP 的组节点**没有右键菜单**，也不做禁用态菜单；见 §2.1 同注记与路线图 M4a 出口「手动新建文件夹：新建/重命名/删除分组后会话归属即时更新、重启持久」）与会话。
- 会话右键菜单：**连接**、在新窗口连接（Phase 4（M4b））、**属性(编辑)**、复制、移动到…、导出、删除（二次确认）。
- 搜索框：实时过滤名称/主机/分组路径，高亮命中。
- 拖拽：会话拖入分组 = 移动；拖文件到会话节点 = 打开 SFTP 并上传提示；**会话未连接时**先走正常连接流程、成功后入队上传，连接失败经 Toast（§8）报错并丢弃该批待传项（§1.4 传输队列呈现「等待连接」态）。
- 底部三按钮：新建（新建会话 → ProfileDialog，MVP；手动新建文件夹归 M4a，见 §2.1 同注记）/ 导入 / 导出（导入/导出保持 MVP）。

### §2.4 Profile 编辑器（对话框，分类页签）

页签：**常规**（名称/分组/主机/端口/用户名）· **认证**（方式多选：密码→Vault 选取、私钥→Vault 选取、Agent、kbd-interactive + 单提示自动应答开关、跳板链编辑器[**M4a；MVP 仅只读呈现与按计划跳转，可视化编辑（从已有 Profile 选取/增删/排序）定档 M4a**，路线图 §2.1 同相位留痕]）· **主机密钥**（策略 TOFU/严格/钉扎 + 指纹列表管理 + 「导入 known_hosts…」按钮：选 OpenSSH known_hosts 文件 → 入 host_keys 表[source=imported]，结果反馈「导入 N 条 / 跳过 M 条 / 冲突 K 条」，M1）· **终端**（TERM/编码[**v1 仅 UTF-8，字段禁用态标注「仅 UTF-8（其他编码 M4+）」**]/滚动行数/字体/配色方案[默认=全局]/高亮关键字集勾选[M4a，§2.11]）· **SFTP**（本地目录/远程目录/下载沙箱）· **日志**（连接自动转录开关/目录/命名模板/追加或覆盖，M4a；MVP 不呈现此页签）· **AI**（自动执行档位 off/read_only/with_confirm、MCP 允许开关；Phase 2/3 启用，**MVP 以禁用态呈现并标注「即将推出」**，与 §2.12 口径统一）· **环境变量**（键值表）。

### §2.5 安全相关模态（总设计 §2.1/§3.2）

- **主机密钥 TOFU**：首次连接——主机、端口、密钥类型（事件载荷 `key_type` 字段）、SHA256 指纹（等宽大字体）、「接受并记录 / 仅本次 / 拒绝」（「仅本次」= 内存态接受，不写 host_keys，会话结束即失效，总设计 §2.1；M1 itest 覆盖该第四态）；密钥变更——红色警示条 + 新旧指纹对比，默认焦点在「拒绝」。
- **kbd-interactive**：按 prompt 列表动态渲染输入框（echo=false → 密码框），显示服务端 name/instruction（sshengine 从 russh kbd-interactive 回调原样透传 `{ name?, instruction?, prompts }`；字段为空时折叠对应区域）。
- **Vault 解锁**：首次 = 设置向导（OS keyring 路径说明 + 可选应用密码）；日常 = 密码输入（错误抖动提示）；锁定入口在工具菜单与状态栏 Vault 段（点击即锁）；MVP 不引入系统托盘。

### §2.6 组合命令栏（Xshell Compose Bar）

- 目标选择器：`当前会话` / `全部会话` / `当前分组` / `手选…(多选弹窗)`（广播在 M4a 打通，MVP 仅当前会话，UI 先到位并禁用其他项并标注「即将推出」）。
- 输入框：`↑↓` 历史（每目标独立 100 条）、`Tab` 不补全（避免与远端冲突）、`Enter` 发送、`Ctrl+Enter` 不附加换行发送、发送后缀可选 `CR / LF / CRLF / 无`（**默认 CR**；`Enter` = 内容 + 后缀，`Ctrl+Enter` = 仅内容；默认值持久化于 settings；§2.10 快速命令发送复用同一后缀规则；M4a 实时键入广播出口「字节级一致」以本规则为准）。
- 发送后立即聚焦回终端。

### §2.7 状态栏分段（左→右）

`会话状态灯+文字` · `host:port` · `编码(只读恒显 UTF-8)` · `TERM` · `行×列` · `Vault 锁状态 🔒/🔓` · `Caps` `Num` · `键盘模式(本地/远程)`（本地 = 快捷键作用于应用 UI；远程 = 按键原样发往会话，含终端常截获的 Ctrl+S/Ctrl+Q 等；**切换入口 = 点击状态栏本段（三平台主入口）；Scroll Lock 为 Windows 肌肉记忆快捷（macOS 多数键盘含 Magic Keyboard 无此键，Linux 视键盘而定），二者等价**；MVP 默认远程，默认值可于 §2.12 键盘与鼠标页签配置）· `AI 状态(Phase2)`
- 点击编码 → 切换菜单（M4+；MVP 无菜单，点击无响应）；点击 Vault → 锁定；点击状态灯 → 连接详情弹层（认证方式、密钥指纹、延时）（**M4+；MVP 点击无响应**，弹层随 M4a 接线，字段形状由总设计 §2.1 载体注锁定）。点击键盘模式 → 当前会话本地⇄远程即时切换（标签文字同步刷新，不发远端、无需确认）。
- **按键仲裁规则**（终端获焦时，钉死优先级）：「远程」键盘模式下，**白名单之外的按键一律原样透传，不再按修饰键组合二次筛选**（**R118**，Task 17 第一轮复审改判：原表述「仅影响无修饰键或单 Ctrl 修饰的普通按键」被实现读作「其余一律本地截获」，而 `Alt+B`/`Alt+F`（readline 词移动）、`Alt+Backspace`（删词）、`Alt+.`（取上条末参）、`Alt+数字`（tmux/emacs 前缀）及 Windows/Linux 上报为 Ctrl+Alt 的 AltGr 合成字符在 §4 键表中均无绑定，截获即静默吞键、永不达远端，与「远程 = 按键原样发往会话」的模式定义自相矛盾；放宽安全的前提不变式是「凡 §4 有绑定的键必在下列白名单内」，由 `shortcuts.test.ts` 守卫用例钉死）。白名单判定须排除 AltGr：`Ctrl+Shift+*` 与 `Alt+P` 两支要求 alt 与 ctrl 不同按，否则 AltGr+Shift+字母（Ą）、AltGr+P 等会被误收白名单。以下快捷键白名单**恒本地截获**（不作按键原样透传）；截获后触发的应用动作可经 term_input 下发等效字节，如 `Ctrl+L`→edit.clearScreen 发送 `\x0c`：`Ctrl+Shift+*` 全家（C/V/S/M/A/B/Q/Tab）、`Ctrl+F`、`Ctrl+W`、`Ctrl+Tab`、`Ctrl+,`、`Ctrl+N`、`Ctrl+L`、`Ctrl+=`/`Ctrl+-`/`Ctrl+0`（字号缩放）、`F3`（查找下一个）、`F11`、`Alt+P`（会话属性）、`Scroll Lock`（模式切换本身；平台可用性见 line 131 括注，不可用时以状态栏段点击替代，功能等价）。`Ctrl+C` 双义性（§2.8 选区复制）**优先于**远程原样发送：非空选区 → 复制并清除选区；无选区 → 远程模式下原样发 `\x03`。`Ctrl+V` 同为**条件**白名单：§2.12「Ctrl+V 粘贴开关」开（默认）时**优先于**远程原样发送——恒本地截获走粘贴流程（含多行确认），不向会话发 `\x16`；开关关时该键退出白名单，按普通单 Ctrl 键交模式仲裁（远程模式原样发 `\x16`），粘贴仅剩 `Ctrl+Shift+V`。实现上此判定必须落在终端按键处理器内就地完成：远程模式下一旦放行给 xterm，其 `evaluateKeyboardEvent` 会把 Ctrl+V 映射为 `\x16` 并 `preventDefault`，textarea 的 `paste` 事件永不触发，开关将成死档。菜单/输入框焦点不受模式影响，恒本地语义。

### §2.8 终端交互细节

- 选择即复制（可选开关，默认关，防误触）；右键行为（设置「右键行为」开关，默认 = 粘贴）：**非空选区上右键 → 弹终端上下文菜单**；无选区右键 → 粘贴（Xshell 肌肉记忆）；可整体切换为「恒弹上下文菜单」模式。
- **终端上下文菜单条目表（本表为条目集权威，按 Phase 渐进增加）**：复制 / 粘贴 / 全选 / ─ / 搜索… `Ctrl+F` / ─ / AI 解读 ✨（Phase 2，仅选区非空时可用，路线图 M2 出口触发路径）/ ─ / 清屏 `Ctrl+L`。M4a 追加「高亮关键字」相关项（§2.11）。此规则与「右键=粘贴」默认值共存：默认配置下 AI 解读经「选区右键」恒可达。
- URL 识别：`Ctrl+点击` 打开浏览器（Tauri opener，白名单 http/https/mailto）。
- 响铃（M1 基础）：BEL/`\a` → 标签橙色铃铛角标（§5）+ 可选视觉闪烁/系统通知；检测取自 xterm.js `onBell` 事件（**前端侧**，core 管道不新增 bell 事件；**后台/非激活标签保留活 Terminal 实例、仅暂停渲染**，bell 与 write-ack 照常工作——总设计 §2.2「serialize 休眠」仅用于关闭前快照/启动恢复的可选优化，不作后台标签默认处置；渲染侧内存单列观测值，路线图 M1 出口同口径）；` bell ` 关键词触发「输出提醒」属 M4a 关键字规则（§2.11），Xshell 同名特性。
- Ctrl+C 双义性（终端获焦时，Xshell 肌肉记忆）：存在非空选区 → 复制并清除选区；无选区 → 经 term_input 发送 `\x03`（SIGINT）。`Ctrl+Shift+C` 恒为复制。此规则仅终端焦点成立；菜单/输入框焦点下 `Ctrl+C` 走标准剪贴板语义。
- 滚动：鼠标滚轮 / `Shift+PgUp/PgDn` / 触摸板；搜索条浮于终端右上（`Ctrl+F`），不占布局。
- 粘贴确认气泡键盘契约（气泡定义与「记住本会话」见 §4）：`Enter` 确认 / `Esc` 取消；气泡 `tabindex=-1`，弹出即自动聚焦，确认/取消后**焦点归还终端**；焦点契约引 §7。

### §2.9 确认类模态

- **关闭确认**（总设计 §2.4 第 6 项，逐字对齐）：触发 = 关闭含活动会话或排队/进行中传输的标签或窗口（MVP 四路：标签 × / 中键点击 / `Ctrl+W` / 窗口关闭同一入口；「关闭全部标签」为 M4b 追加的 Window 菜单项（§2.1，MVP 菜单中禁用态预埋、不可触发），M4b 启用后复用同一确认入口）；内容 = 会话数 + 会话名列表（最多列 10 条，超出折叠为「…等 N 个」）+ 传输数（排队 X / 进行中 Y）；按钮 = 「关闭并断开」（破坏性样式）/「取消」（默认焦点）；确认后断开会话并取消传输。全部空闲（无活动会话、无排队传输）时直接关闭不弹。落地组件 `CloseConfirmDialog.svelte`。标签右键菜单「关闭标签」项与标签 × 同走 requestCloseTab（tab scope），系同一路径的控件面，不另计一路；四路指路径分类而非控件个数。

### §2.10 快速命令按钮条（M4a）

- 停靠位置：标签栏之下、终端区之上的横向条，经工具栏按钮显隐（默认隐，打开首个集合后记忆）。
- 按钮形态：`[图标] 名称`，16px 图标；横向排列、溢出左右滚动；单击 = 立即发送到当前目标（跟随 §2.6 目标选择器；选择器的 MVP 禁用态见 §2.6）；右键 = 编辑/复制/删除。
- 与下拉集的关系：工具栏「快速命令集 ▾」（§2.2）= 全部集合的浏览下拉（只读 + 「编辑…」入口）；按钮条 = 当前「钉住」的集合，二者同一数据源。编辑器入口：工具菜单 → 快速命令集编辑器（集合列表 / 命令列表 / 命令内容 + 排序）。
- 参数占位符：`${name:默认值}` 语法；点击含占位符的按钮 → 行内轻量气泡逐项填写（非模态，不遮终端），`Enter` 替换后发送，`Esc` 取消。

### §2.11 高亮关键字集（M4a）

- 规则数据结构：`{ id, name, enabled, match: { kind: 正则|字面量, pattern, 区分大小写 }, scope: 全局|Profile, action: 高亮|提醒|二者, color? }`。
- 编辑器入口：工具菜单 → 「高亮关键字…」；集合全局共享，Profile 终端页签勾选本会话启用哪些集合。
- 命中行为：终端输出匹配 → 按规则颜色着色（缺省用 `--fs-warn`/`--fs-danger` 色系）；action 含「提醒」→ 标签橙色铃铛角标（与 §5 bell 角标同一呈现通道，清除规则同 bell：切回该标签或手动清除）。**执行位置**：匹配与着色在前端（基于 xterm.js `registerDecoration`/`onWriteParsed`），提醒角标复用 §5 前端通道；规则 CRUD 与持久化经 core（connmgr 库；单测归属：前端匹配器[正则/字面量双路] + core 规则仓库 CRUD）。

### §2.12 选项对话框（SettingsDialog，页签结构）

页签：**通用**（语言[Phase 4b；MVP 硬编码单语言简体中文，该项禁用]、密度、启动行为、更新检查[M4b]）· **键盘与鼠标**（键盘模式默认值[远程/本地，默认远程；作用于新建会话，会话内切换不持久化；全量重绑 M4a 含自定义切换键]、右键行为[粘贴⇄上下文菜单]、选择即复制、多行粘贴确认、Ctrl+V 粘贴开关[默认开 = 终端焦点下 Ctrl+V 即粘贴，关则仅 Ctrl+Shift+V；§2.1/§4 默认值的配置载体]；全量重绑 M4a）· **终端外观**（全局字体/默认配色方案/背景透明度/bell 行为）· **安全与 Vault**（Vault 自动锁定策略、剪贴板定时清除[默认开、默认 30s 时长可配、整项可关，逐字对齐总设计 §3.2]、主机密钥默认策略、下载沙箱根目录）· AI（Phase 2：模型源配置/默认档位/AI 执行总开关[总设计 §4.3：一键禁止一切 AI 执行入口]/按 Provider「允许发送屏幕上下文」开关[总设计 §4.2，云端默认开]；MVP 此页签禁用并标注「即将推出」）· 高级（日志目录、诊断导出、用户主题目录）。
**加粗页签 = MVP 必含**（通用/键盘与鼠标/终端外观/安全与 Vault）；AI 与高级页签 MVP 以禁用/精简态呈现。入口：工具 → 选项 `Ctrl+,` / 工具栏「设置 ⚙」。

### §2.13 会话恢复对话框（SessionRecoveryDialog）

- 触发：启动时检测到上次存在未正常关闭的会话（载体 = connmgr 库 `session_journal` 表的 `closed_cleanly` 标记：open 写 0 / close 写 1，启动检 `closed_cleanly=0`；总设计 §2.4 第 5 项 / §3.4 同句声明；路线图 M1 出口「未关闭会话启动恢复（仅询问，不自动连）」），在主界面就绪后弹出。
- 内容：未关闭会话列表（会话名 / host:port / 上次活跃时间），多选（默认全选）；按钮「重连所选」/「全部跳过」（**默认焦点在「全部跳过」**，对齐不自动重连原则）。
- 行为：重连 = 对所选走正常连接流程（逐个失败不阻塞其余，错误经 Toast §8 提示）；跳过 = 清除恢复标记不连接；`Esc` = 全部跳过。
- 落地组件 `SessionRecoveryDialog.svelte`（§8）。

---

## §3 皮肤系统（双轴）

### §3.1 轴一：应用主题（Chrome）

全部经由 CSS 自定义属性令牌驱动（`--fs-*`），主题 = 一组令牌赋值；切换即时生效、无刷新。

**语义令牌**（**本表为完整权威集**；`docs/mockups/index.html` 的 CSS 为视觉参考实现，实现时逐条搬入，mockup 缺项（如 `--fs-density`、`--fs-radius-lg`）以本表为准补齐）：

```
--fs-bg-app        应用背景（最底层）
--fs-bg-panel      面板/侧栏背景
--fs-bg-elevated   浮层/对话框背景
--fs-bg-input      输入框背景
--fs-bg-hover      悬停高亮（菜单项/树节点/行）
--fs-bg-tab-active / --fs-bg-tab-inactive   标签页背景
--fs-fg-primary    主文字
--fs-fg-secondary  次文字（对比度 ≥4.5:1 vs 面板背景）
--fs-fg-disabled   禁用文字（WCAG 非文本对比豁免项，不作为唯一信息通道）
--fs-border        边框/分割线
--fs-border-strong 强边框（滚动条、浮层描边）
--fs-accent        强调色（按钮、选中、焦点）
--fs-accent-hover  强调色悬停态
--fs-accent-fg     强调色上的文字
--fs-accent-dim    强调色淡背景（选中行/徽标底）
--fs-ok / --fs-warn / --fs-danger / --fs-info   状态色
--fs-selection     文本选择背景
--fs-term-bg       终端区背景锚（默认主题与终端配色方案联动，可被方案覆盖）
--fs-radius(控件圆角 4px) / --fs-radius-lg(对话框圆角 8px) / --fs-shadow / --fs-density(行高系数：紧凑 1.2 / 宽松 1.5)
--fs-bg-tooltip / --fs-fg-tooltip(悬停 tooltip 浮层；缺省实现复用 --fs-bg-elevated + --fs-fg-primary，主题可覆盖)
```

**自带主题（MVP 4 套，必须）**：

| 主题 | 基调 | 强调色 | 场景 |
|---|---|---|---|
| **Obsidian**（默认） | 近黑 #0f1115 系，冷灰层次 | 蓝 #4f8cff | 长时间暗室运维 |
| **Daylight** | 白 #fafbfc 系，浅灰层次 | 蓝 #2563eb | 白天/投影演示 |
| **Slate Blue** | 深蓝灰 #131a26 系 | 青 #22d3ee | 低刺激夜间 |
| **High Contrast** | 纯黑/纯白，加粗边框 | 黄 #ffd400 | 无障碍/强光环境 |

- 主题元数据：`{ id, name, dark: bool, tokens: {...} }`，内置主题编译进前端；用户自定义主题 = Phase 4b：配色/主题编辑器 + 用户 JSON 主题目录热加载（路线图 §1 M4「范围 4b」/「出口标准 4b」）；MVP 仅预留主题元数据类型 `themes.ts` `AppTheme` 供 4b 加载器复用——`themes/` 目录与运行期热加载器均属 4b，M1 范围/出口无该项。
- 跟随系统明暗（可选 `auto`：按 `prefers-color-scheme` 在 Daylight/Obsidian 间切换）。

### §3.2 轴二：终端配色方案（ANSI）

每方案 = 16 ANSI 色 + foreground/background/cursor/selection + 是否粗体亮色，存为 JSON：

```json
{
  "id": "futureshell-dark", "name": "FutureShell Dark", "dark": true,
  "background": "#0f1115", "foreground": "#d7dbe2",
  "cursor": "#4f8cff", "selection": "#2b3a55",
  "ansi": ["#1f232b","#ff5f56","#3fd68c","#ffd166","#4f8cff","#c792ea","#56d4dd","#d7dbe2",
           "#57606f","#ff7b72","#56f2a5","#ffe08a","#79a8ff","#e2b3ff","#7ee7f0","#ffffff"],
  "boldBright": true
}
```

**自带方案（MVP ≥10 套）**：FutureShell Dark（默认，与 Obsidian 主题配套）、Xshell Standard（深，还原 Xshell 经典 ANSI）、Solarized Dark、Solarized Light、Monokai Pro、Dracula、Nord、Gruvbox Dark、One Dark、Tomorrow Night、Ubuntu、Windows Terminal Light。

- **作用域**：全局默认方案（设置）→ Profile 级覆盖（终端页签）→ 会话运行期临时覆盖（**标签右键菜单**「配色方案」子菜单，定义见 §1.3 标签右键菜单独立条目，不写库、关标签即释放（断线重连期间保留，§5 断线态标签存活）；终端区右键走 §2.8 上下文菜单规则，不承载此子菜单）。
- 切换只影响 xterm.js `theme` 选项与 `drawBoldTextInBrightColors` 选项，即时生效。方案 → 选项映射：`background/foreground/cursor/selection` → `theme.{background,foreground,cursor,selectionBackground}`；`ansi[16]`（我们自己的存储格式）→ xterm.js ITheme 命名字段 `black/red/green/yellow/blue/magenta/cyan/white/brightBlack…brightWhite`（已核实 xterm 6.0.0 的 ITheme **无** `ansi` 数组字段）；`boldBright` → `drawBoldTextInBrightColors`。背景透明度（0-100%）为**全局级**（settings 键 `term.opacity`，§2.12 权威，不入 term_blob）；字体为 Profile 级：构造期恒定 `allowTransparency: true`；透明度 = `theme.background` 的 alpha 改写（`rgba(bg, alpha)`），**即时生效**；字体族/字号经 xterm.js 动态 options（`term.options.fontSize = …`）写入，即时生效；16 色/fg/bg/cursor/selection 切换亦即时。仅当某渲染器降级路径实测不支持动态 options 时，才局部重建终端实例并以 serialize addon 保持 scrollback（实现注记，不改变上述接口承诺）。渲染器前提：`drawBoldTextInBrightColors` 在 xterm 6.0.0 的 DOM/WebGL 两种渲染器下均生效（源码核验：DomRendererRowFactory 的 P16/P256 前景色分支；6.0 已移除 canvas 渲染器 #5105），降级路径无需禁用该开关；6.0 不存在 canvas 回灌选项（`@xterm/addon-canvas` 停更于 0.7.0 且 peer 仅 `^5.0.0`），降级链恰为 webgl→DOM 两档（与计划本体 Task 18 `term.ts` 注释同口径）。
- 导入 `.xcs`（Xshell）/`.itermcolors` 在 Phase 4b（超越项之一：跨工具配色迁移）。

### §3.3 字体与排版

- UI 字体栈：`"Segoe UI Variable", "Segoe UI", "PingFang SC", "Microsoft YaHei UI", system-ui, sans-serif`。
- 终端字体：`"Cascadia Code", "JetBrains Mono", "Sarasa Mono SC", Consolas, "Courier New", monospace`（含 CJK 等宽回落，默认 13px，可调 8–32，抗锯齿子像素）。
- 字号缩放独立：UI（跟随系统）与终端（`Ctrl+滚轮`/`Ctrl±`，per-session）。

### §3.4 持久化

- 应用主题、密度、工具栏布局（键 `ui.toolbarLayout`，M4b，见 §2.2）、面板位置/宽度、全局终端方案/字体 → `settings` 表（connmgr 库，键值 JSON）。
- Profile 级方案/字体 → `profiles.term_blob`（背景透明度不入此列，为全局级 `term.opacity`，§2.12/§3.4 Q 波修订）（**需新增字段**：总设计 §3.1 TermSettings 原无此项，由 connmgr 迁移新增 JSON 列；路线图 M1 范围已回灌该列承诺，M1 建表时一并落地）。

---

## §4 交互与快捷键（Xshell 对齐 + 扩展）

| 操作 | 快捷键 | 备注 |
|---|---|---|
| 新建会话 | `Ctrl+N` | Xshell 同 |
| 会话属性 | `Alt+P` | Xshell 同 |
| 关闭标签 | `Ctrl+W` / 中键点击标签 | |
| 上一/下一标签 | `Ctrl+Tab` / `Ctrl+Shift+Tab` | MRU 序 |
| 查找 | `Ctrl+F` | 终端内浮层 |
| 清屏 | `Ctrl+L`（发送）/ 菜单清滚动 | 本地截获、不作按键原样透传（§2.7 白名单）：edit.clearScreen 经 term_input 下发 `\x0c` |
| 会话管理器 | `Ctrl+Shift+S` | 显隐 |
| 监控抽屉 | `Ctrl+Shift+M` | 显隐 |
| AI 助手 | `Ctrl+Shift+A` | Phase 2 |
| 全屏 | `F11` | |
| 字号 | `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | per-session |
| 复制 | `Ctrl+C`（有选区）/ `Ctrl+Shift+C`（恒复制） | 仅终端焦点，无选区 Ctrl+C 发 SIGINT，见 §2.8 |
| 粘贴 | `Ctrl+Shift+V`（恒粘贴）/ `Ctrl+V`（终端焦点下默认粘贴，§2.12 开关可关） | 含多行粘贴确认 |
| 广播开关 | `Ctrl+Shift+B` | M4a |
| 快速命令集 | `Ctrl+Shift+Q` | M4a |
| 键盘模式切换 | `Scroll Lock` / 状态栏键盘模式段点击 | 点击为三平台主入口；`Scroll Lock` 仅 Windows 肌肉记忆快捷（macOS 多数键盘含 Magic Keyboard 无此键，Linux 视键盘而定），见 §2.7 |

- 多行/含换行粘贴 → 确认气泡（「将向会话粘贴 12 行，可能立即执行」），可「记住本会话」。
- 所有快捷键可在设置中重绑（M4a 键盘配置文件；MVP 固定集）。

---

## §5 状态视觉语言

| 状态 | 标签图标 | 侧栏灯 | 状态栏 | 附加 |
|---|---|---|---|---|
| 连接中 | 蓝色旋转环 | 蓝闪 | 「连接中…尝试认证方法」 | — |
| 已连接 | 绿点 | 绿 | 🟢 已连接 | 活动输出时标签短暂高亮 |
| 已断开 | 灰色空心 | 灰 | 「已断开 [重连]」 | 顶部黄色 banner：重连倒计时 + 尝试次数 + 「立即重连」手动入口（逐字对齐总设计 §7.1；banner 与状态栏 [重连] 并存，任一入口触发即开始重连） |
| 错误 | 红色 ✕ | 红 | 错误摘要（悬停详情） | banner 红色 |
| 有输出提醒 | 橙色铃铛角标 | — | — | bell/关键词触发（M1 bell 基础；M4a 关键字规则 §2.11）；**清除手势**：单击角标即清除（悬停 tooltip「输出提醒，点击清除」），切回该标签亦清除（§2.8/§2.11 同规则） |

- AI 状态（Phase 2）：状态栏 `✨ 就绪/思考中/执行中`，思考时终端建议区微光边框。
- 动效克制：过渡 ≤150ms；`prefers-reduced-motion` 下全部关闭。

---

## §6 图标与密度

- 图标：单色轮廓（lucide 风格，1.5px 线宽），16px 网格；状态/品牌处用填充变体。打包内嵌 SVG sprite，无 CDN。
- 密度档：紧凑（默认，行高 1.2）/ 宽松（1.5，触屏友好），经 `--fs-density` 令牌整体缩放。
- 圆角：4px（控件）/ 8px（对话框）；阴影仅浮层使用（暗色主题以边框为主，避免灰雾）。

---

## §7 无障碍

- 文本对比 ≥ 4.5:1（High Contrast 主题 ≥ 7:1）；焦点环始终可见（`outline: 2px solid var(--fs-accent)`）。
- 全键盘可达：菜单、树、对话框、标签；屏幕阅读器标签（ARIA role=tree/tablist/status）。
- **对话框焦点契约**（全部 role=dialog 模态；载体 = `frontend/src/lib/focusTrap.ts`，计划 Task 17）：打开 → 聚焦指定默认元素（分层规则：① 逐条列明者从其列明：CloseConfirmDialog=「取消」（§2.9）；DeleteConfirmDialog=「取消」（§8）；VaultDialog=密码输入框（§2.5）；SessionRecoveryDialog=「全部跳过」+默认全选（§2.13）；AuthPromptDialog=首个输入框（§2.5）；HostKeyDialog（无输入控件，仅三按钮）：kind=changed=「拒绝」（§2.5），其余 kind=首个主按钮「接受并记录」；② 含输入的表单类（未列明者，如 ProfileDialog/SettingsDialog）=首个输入框；③ 纯确认类（未列明者）=非破坏性按钮）；Tab/Shift+Tab 循环圈闭于 dialog 内可聚焦元素；关闭后焦点归还触发元素；焦点环不变（本章首条）。
- 颜色不作为唯一信息通道（状态同时有图标/文字）。

---

## §8 到实现的映射（Phase 0+1 前端任务）

> 时效注记：本表落地位置为预告性映射，随路线图 §5 进度快照回灌进 Phase 0+1 计划；回灌完成后逐行复核（重点：web-links/opener 的 capability 注记、监控抽屉预留插槽在前端任务中有对应 Step；Round 6 复核清单：SearchOverlay→Task 18 Files/Step，Toast/toast.ts→Task 20 新 Step + App 挂载，VaultDialog→Task 20 setup/unlock 载体，SessionRecoveryDialog→Task 22 Files/Step 4，DeleteConfirmDialog→Task 20，focusTrap.ts→Task 17 Files/Step，shortcuts.ts→Task 17 + App.svelte 唯一窗口级 keydown，transfers.ts store→Task 20 Step 4 上提 + Task 21/Task 22 消费；Q 波纠正：SessionBanner→Task 22 TerminalPane 内联（不单设组件））。

| UI 规格章节 | 落地位置 |
|---|---|
| §3.1 令牌 + 4 主题 | `frontend/src/lib/theme/{tokens.css, themes.ts, store.ts}`（Svelte store + `documentElement.dataset.theme`；令牌全集以本规格 §3.1 表为权威，`docs/mockups/index.html` CSS 为视觉参考） |
| §3.2 终端方案 | `frontend/src/lib/term-schemes/*.json`（≥10 套）+ `term.ts` 应用逻辑（全局/Profile/临时三级，写入 xterm.js `theme` + `drawBoldTextInBrightColors` 选项，映射见 §3.2） |
| §3.4 settings 持久化 | connmgr 迁移 `0002_settings.sql`（`settings(key TEXT PRIMARY KEY, value TEXT NOT NULL)`）+ app 命令 `settings_get/settings_set` |
| §1.1 布局骨架 | `App.svelte`（grid 布局 + 面板停靠/宽度状态 store，默认侧栏左停靠）+ `MenuBar.svelte` + `ToolBar.svelte` |
| §2.3 会话管理器 | `Sidebar.svelte`（左停靠、拖拽排序、状态灯；拖右/浮动 M4b） |
| §2.6 组合命令栏 | `ComposeBar.svelte`（MVP：当前会话可用；广播目标项禁用并标注「即将推出」） |
| §2.7 状态栏 | `StatusBar.svelte` |
| §2.8 URL 识别 | `term.ts` 加载 `@xterm/addon-web-links`；外链一律 `invoke("open_external")` → Rust 侧 `OpenerExt` + 白名单 http/https/mailto（**R119**：capability **不授** `opener:*`——`opener:default` 的 `allow-default-urls` 作用域含 `tel:*` 且 `allow-reveal-item-in-dir` 无作用域约束，授出即等于在 Rust 白名单之外另开一条 webview 直通路；插件 scope 仅在 `commands.rs` 的 IPC 路径生效，Rust 侧 `open_url`/`reveal_item_in_dir` 本就不过 ACL，故 `open_external` 与 M4a `reveal_log_dir` 均不受影响，Rust 白名单成为唯一边界） |
| §5/§2.8 bell 角标 | `term.ts` 接 xterm.js `onBell` → per-session 角标态 store（切回激活标签或单击角标即清除），前端侧检测，不给 core 管道加事件；后台标签保留活实例仅停渲染（§2.8 同条），角标照常工作；`TabBar.svelte` 渲染橙色铃铛角标 |
| §2.8 搜索浮层 | `SearchOverlay.svelte`（计划 Task 18 承载；`Ctrl+F` 打开，绝对定位于终端区右上，接 `TermController.findNext/findPrevious/clearSearch`，`Esc` 关闭） |
| §2.4/§2.5 对话框 | `ProfileDialog / HostKeyDialog / AuthPromptDialog / VaultDialog / SettingsDialog`（SettingsDialog 页签结构见 §2.12；VaultDialog = 计划 Task 20 首启设置向导/日常解锁载体（mode=setup/unlock，§2.5），独立凭据浏览/管理归 M4b 密钥/代理管理器） |
| §2.9 关闭确认 | `CloseConfirmDialog.svelte`（计划 Task 22） |
| §2.3 删除确认 | `DeleteConfirmDialog.svelte`（计划 Task 20 承载；**MVP 仅 Sidebar 会话删除消费**——分组无右键菜单故无删除入口，见 §2.3；分组非空删除随 M4a 组右键菜单复用本对话框。破坏性样式，默认焦点「取消」） |
| §7 焦点契约 | `frontend/src/lib/focusTrap.ts`（计划 Task 17 承载；useFocusTrap = 初始焦点 + Tab 圈闭 + 焦点归还） |
| §4/§2.7 快捷键派发与按键仲裁 | `frontend/src/lib/shortcuts.ts`（计划 Task 17 承载；arbitrate + keyboardMode store + 键→动作表，App.svelte 挂唯一窗口级 keydown） |
| §2.13 会话恢复 | `SessionRecoveryDialog.svelte`（计划 Task 22 承载；启动时仅询问不自动连；默认焦点「全部跳过」+ 列表多选默认全选，§2.13） |
| §1.4 传输队列 | `TransferQueueDrawer.svelte`（全局抽屉、跨标签持久可见、进度/重试/取消；传后校验开关 §1.4）+ 传输态 store `frontend/src/lib/transfers.ts`（计划 Task 20 建立、Task 21 抽屉与 Task 22 关闭确认经 activeTransfers 共用计数） |
| §5 通知与状态提示 | `Toast.svelte`（分级错误通知，映射总设计 §7.1 错误分级；自动消失 3–8s、可堆叠；计划 Task 20 承载，store 于 `frontend/src/lib/toast.ts`）+ 断线重连 banner（内联于 `TerminalPane.svelte`，计划 Task 22 Step 3 承载，不单设组件；文案对齐总设计 §7.1：倒计时 + 尝试次数 + 「立即重连」；重连成功后「屏幕内容未恢复」一次性提示） |
| §1.4 SFTP 分屏 | `SftpPane.svelte` + `TerminalPane` 三态切换 |
| §1.3 监控抽屉 | M4a（MVP 预留折叠插槽，无数据采集） |

**验收基准**：默认 Obsidian 主题的截图与 Xshell 7 默认界面并排，布局结构一致（会话管理器左侧、组合栏底部、标签 MDI），视觉更现代。
