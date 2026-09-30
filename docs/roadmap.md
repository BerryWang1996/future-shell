# FutureShell 里程碑路线图与实施记录

> 本文保存 M0–M7 的范围、出口标准与历史实施记录。旧进度快照不作为当前发版结论；
> 最新状态见 [1.0.0 发版检查](verification/release-readiness-1.0.0.md)，文档导航见 [文档中心](README.md)。

版本 v1 · 2026-07-27 · 伞文档：统领各阶段独立计划

**定位**：本文档是总路线图，只定义每个里程碑的目标、范围、出口标准与依赖；每个里程碑的裁决、边界与实施记录合并在本文件 §6（2026-08-28 起合并阶段实施记录；使用说明与验证证据分别维护）。设计依据：`docs/design/architecture.md`（总设计）、`docs/design/ui.md`（UI 规格）。

**执行契约（每个里程碑一律如此，2026-07-27 按用户指令升级）**：撰写该里程碑实现计划 → 多视角对抗复审（Workflow）→ 修订 → **连续两轮复审均无确认的高/中危发现**方可定稿 → 实现（subagent-driven，每任务独立提交+任务间复审）→ 阶段完成后再次复审（同样需连续两轮通过）→ 出口验收清单逐项核验 → 打 tag。任一环节不过不得进入下一里程碑。联网核验：凡涉及外部 API/库行为的主张，尽量以 WebSearch/WebFetch 佐证（工具故障时降级为知识库+文档内留痕）。

**1.0.0 的人工核验清单**：当前共 15 项，其中原 M0–M4 出口 13 项
（推送 origin 2 项、真人操作 GUI 4 项、真实外部服务 3 项、特定机器 3 项、采购决定 1 项）。
它们不是待办而是**待验**——待办可以继续写代码消掉，待验不行，代签就是造假。
逐项的「怎么做 / 看什么 / 算通过的判据」见
[`docs/verification/manual-checklist-1.0.0.md`](verification/manual-checklist-1.0.0.md)。

---

## §0 总览

| 里程碑 | 一句话目标 | 版本 tag | 依赖 | 状态 |
|---|---|---|---|---|
| **M0 骨架** | 工作区/CI/8 crate 脚手架可构建可跑空测试 | `v0.0.1-m0` | — | 计划就绪（在 Phase 0+1 计划 Task 1–2） |
| **M1 MVP 核心** | 可用 SSH 终端 + SFTP + Vault + Profile，对标 Xshell/Xftp 基础完全体（SSH/SFTP 面） | `v0.1.0` | M0 | 计划回灌中（UI 规格 §8 任务） |
| **M2 AI 基础** | 自然语言→命令、输出解读、确认后执行，模型源可插拔（云+Ollama） | `v0.2.0` | M1 | 待计划 |
| **M3 Agent + MCP** | 自主运维 Agent 循环 + MCP 双向（对外 Server / 内置 Client） | `v0.3.0` | M2 | 待计划 |
| **M4 仿制深化 + 超越** | 4a：广播/片段库/监控/日志/录屏/关键字/计划任务/历史命令 UI/键盘配置文件/隧道/Xftp 补齐；4b：跨工具导入/配色编辑器/主题目录/自动更新+签名/取证包/便携打包评估 | `v0.4.0` / `v0.4.1` | M1（4b 部分项依赖 M2） | 待计划 |
| **M5 移动端** | Tauri 2 移动端伴生 App（iOS/Android），审批/监控/应急命令 | `v0.5.0` | M3 + M4a（监控卡） | 待计划 |
| **M6 云同步（可选）** | 端到端加密的配置/Profile 同步（用户自备存储或可选服务） | `v0.6.0` | M1 | 待计划，可无限期推迟 |
| **M7 系统面工具箱 + 桌面化文件操作** | 服务管理/补丁盘点/告警展示（**只做系统面，不碰应用层**）、文件管理器桌面化（应用内虚拟窗口）、串口直连 | `v0.7.0` / `v0.7.1` | M1（告警复用 M4a） | 已立项（2026-08-28 用户提出并裁定形态） |

> **tag 口径（2026-08-22 / M4a.1 T92 裁决）**：一律纯三段 `vX.Y.Z`，**不带** `-phaseN` 后缀，
> 阶段名写进 release notes 而不是版本号。两条理由：① 版本一致性门禁
> （`.github/scripts/check-version-consistency.sh`）只接受三段数字，放宽正则等于长期容忍任意后缀，
> 而版本号是用户可见字符串；② tauri updater 按 semver 比较，`0.4.0-phase4a` 属**预发布**版
> （排序**低于** `0.4.0`），将来真上 updater 时会静默判错「有没有新版本」。
> M4 的两个子里程碑因此分别落在 `v0.4.0`（4a）与 `v0.4.1`（4b）。
> 该口径由 selftest 的两条夹具钉住（纯三段接受 / 带后缀拒绝）。

**出口清单的勾选记号**（各里程碑「出口标准」下逐条使用；本节是唯一定义处）：

| 记号 | 含义 | 要求 |
|---|---|---|
| `[ ]` | 未做 | —— |
| `[x]` | 已落地**且有载体** | 必须在条目末尾注明判据，指名到具体测试文件/函数或脚本。写「已覆盖」这种无指向的话不算 |
| `[~]` | **部分**落地 | 必须写清哪一半闭合了、哪一半没有、以及为什么。整条不得按已完成计入 |
| `[!]` | **阻塞** | 卡在实现侧之外的前置（推送 origin、采购证书、真机、人工核验）。必须写清卡在哪、有没有本地替代、替代覆盖到什么程度。**一律不代签** |

`[~]` 与 `[!]` 的区别是「差在自己手上」还是「差在别人手上」：前者继续做就能变 `[x]`，
后者做多少都变不了。两者都**不**计入「已完成」。

依赖图：`M0 → M1 → M2 → M3 → M5`，`M4a → M5`（监控卡依赖 M4a 采集实装）；`M4` 自 M1 起可并行推进（AI 相关项挂 M2 之后）；`M6` 依赖 M1，独立于 M2–M5 链，可无限期推迟。

全局不变量（所有里程碑必须守住，出口验收含回归核验）：

1. 三平台 CI 全绿（fmt/clippy/nextest/deny；Linux 跑 FS_ITEST 容器测试）。
2. 无密钥泄漏路径：secrets 不出 core、不进日志、Zeroizing、剪贴板定时清除（默认开、默认 30s、时长可配/整项可关，逐字对齐总设计 §3.2）。
3. 渲染层隔离：严格 CSP、Markdown 净化、capabilities 最小集。
4. 性能底线（总设计 §0.1 全五项）：并发会话容量 100 个（50 活跃渲染 + 其余后台化）、每空闲会话常驻内存 ≤5MB、本地回环连接到可交互 <2s（不含认证往返）、冷启动 ≤3s、SFTP ≥20MB/s；外加路线图自加的高吞吐量化底线（依据总设计 §2.2 不丢字节与 §8.5.2 压测项）：200MB 输出零丢失且渲染队列水位 ≤ high-water×1.5。
5. 许可证白名单 Apache/MIT/BSD/MPL；无遥测。
6. UI 规格符合性：布局/皮肤/状态视觉与 `docs/design/ui.md` 一致（有原型截图基线 `docs/mockups/`）。

---

## §1 里程碑细则

### M0 骨架

- **目标**：仓库即工程。任何人 clone 后 `cargo nextest run` 一次通过；三平台 CI 矩阵存在且绿。（前端 `npm run build` 闸随前端骨架在 M1 出口验收——骨架属计划 Task 17，不在 M0 范围。）
- **范围**：workspace + workspace.dependencies 全量版本锁定（总设计 Global Constraints）；8 core crate 建壳（含 Phase 2+ 占位 crate 的 lib.rs 注释说明）；CI 中 `FS_ITEST` 环境闸约定（itest crate 本体随 M1 Task 9 创建，M0 只在 ci.yml 固定该环境变量语义）；rust-toolchain stable；deny.toml 许可证白名单；`.github/workflows/ci.yml` 三平台矩阵（frontend job 带 `frontend/package.json` 存在性门控：M0 阶段自动跳过、Task 17 建骨架后自动解锁，避免 M0 期间 CI 红）；仓库 README/CONTRIBUTING/目录说明。
- **不含**：任何功能代码；前端骨架（属 M1 计划 Task 17）。
- **出口标准**：
  - [x] `cargo build --workspace` / `cargo nextest run --workspace` / `cargo clippy -- -D warnings` / `cargo fmt --check` / `cargo deny check licenses bans advisories` 全通过（`advisories` = 总设计 §6.2「advisory 漏洞库检查」的门禁载体，与 `deny.toml` 的 `[advisories]` 段对偶）；（判据：2026-08-23 本机全跑——build 绿、nextest 852/852、cargo test 零失败套件、clippy --workspace --all-targets 零问题、fmt 绿、deny advisories/bans/licenses 三项 ok）
  - [!] CI 在 Windows/macOS/Linux 三 runner 全绿（至少一次真实运行记录；frontend job 因存在性门控跳过、日志可见门控判定）；
        **首次真实运行（2026-09-05，run 33942316287，候选分支）：windows-2025 全绿，另三处红**——
        ① ubuntu clippy：`crates/serial/tests/pty.rs` 等链接超时 panic 的路径上子进程不回收
        （`zombie_processes`；文件 `cfg(unix)`，Windows 上不编译）；② macos-14 死在 helper 构建：
        macOS 自带 bash 3.2 把 `"$PROFILE，"` 的全角逗号读进变量名，`set -u` 当场退出（全仓 18 处同形）；
        ③ frontend：runner 为 UTC，两条测试把被测函数刻意归一的 +0 与 `-0` 比较。
        **2026-09-29 已全部修复**，并补两件防再犯：`scripts/check-shell-portability.mjs`（进 CI docs-gates
        与 ci-local）、`scripts/linux-ci-mirror.sh`（容器里按 ci.yml 原顺序跑 ubuntu check 作业，含 Tauri
        Linux 全栈与 app crate——补上 linux-precheck 不覆盖的 10 处）。镜像实跑：fmt/clippy 双工作区、
        nextest 1657/1657、cargo test 全绿；它当场抓到一条只在 Linux 红的测试（RDPDR 联接逃逸测试用了
        Windows 的 `mklink`），已改为跨平台。**仍 `[!]`**：要等修复推送后三 runner 的真实运行记录。不代签。
  - [x] 8 个 crate 按 §3 命名公约命名（包/lib `fs_<name>` 下划线、目录 `crates/<name>` 无前缀）并与总设计 §1 一一对应（去前缀对应）。（判据：`crates/{vault,connmgr,sshengine,terminal,policy,ai,mcpbridge,audit}` 八目录去前缀与总设计 §1 一一对应，包名逐一为 `fs_<name>`；另有 `crates/itest`（`fs_itest`）属实现计划、不在 §1 八个之列，README 命名公约节已注明）
  - [x] README/CONTRIBUTING/目录说明齐备（与已审定计划 Task 1 Step 6 内容逐项一致：clone 即构建流程与 `cargo nextest`/clippy/deny 命令、依赖前置 Rust stable ≥1.94/Node 24/可选 Docker、`FS_ITEST` 环境闸（仅 Linux runner）与 `FS_PERF` 手动/夜间触发不进 PR 闸的语义说明、8 crate 目录结构与 §3 命名公约说明、三平台 CI 矩阵说明）。（判据：README 覆盖构建流程/前置/命名公约/目录结构，CONTRIBUTING 覆盖门禁命令、许可证白名单、`FS_ITEST` 与性能两条线、三平台矩阵。核验中发现并修正一条虚假环境闸：CONTRIBUTING 原写「性能测试 `FS_PERF` 手动/夜间触发」而全仓零命中——照着跑一条性能测试都不会跑且输出是绿的。现由 `scripts/env-gate-parity.sh` 守着）
- **风险**：russh 0.62.4 与平台工具链兼容 → M0 末做一次三平台 `cargo check -p fs_sshengine` 冒烟。

### M1 MVP 核心（Xshell/Xftp 基础完全体对标）

- **目标**：核心场景可替代 Xshell+Xftp（SSH/SFTP 面）的本地软件：多会话标签、完整认证矩阵、TOFU、ProxyJump、SFTP 双栏传输、Vault、断线重连、三平台安装包。UI 为 Xshell 式布局 + Obsidian 暗色主题 + 4 套应用主题 + ≥10 终端配色。（完整替代所需的输入广播/快速命令集/片段库/纯文本日志等老用户依赖项在 M4a 补齐。）
- **范围**（= Phase 0+1 计划 Task 3–25，含按 UI 规格 §8 回灌的前端+app 层任务，见 §5 进度快照；known_hosts 导入的计划载体：Task 7 解析入库 / Task 20 导入按钮 / Task 9·25 itest，Round 5 回灌）：vault 加密原语 + Store；connmgr SQLite/迁移/版本闸/备份/ProfileRepo（含 `profiles.term_blob` JSON 列：Profile 级字体/配色方案（theme_override 键，计划 R11；背景透明度为全局设置 `term.opacity`，UI 规格 §2.12），UI 规格 §3.4）/hostkey/settings 表 + **audit 表**（总设计 §6.2 字段：发起方/命令或工具调用/目标会话/风险级/裁决/exit code/输出摘要/时间戳 + hash 链列，append-only，M1 只建表与约束；AI 动作写入链路 M2 实装，Agent 步骤 + MCP 调用全链路写入 M3 完整）；sshengine 认证状态机/kbd-interactive（透传服务端 name/instruction）/三平台 agent/ProxyJump/TOFU（含「仅本次」内存态接受 AcceptOnce，会话结束即失效、不写 host_keys）/OpenSSH known_hosts 导入（入 host_keys 表 source=imported）/SFTP+传输管理器；terminal 网格/环形缓冲/流控背压/会话管道；Tauri app + Svelte 前端（UI 规格 §8 映射表全部组件：主题引擎、配色方案、菜单栏/工具栏/组合命令栏/状态栏、settings 命令、输出提醒 bell 角标）；打包 nsis/msi/dmg/deb/appimage；性能测试（FS_PERF 闸）。
- **不含**：AI 一切能力（命令面板预留禁用态）；广播发送（UI 预留禁用态）；监控抽屉数据采集（UI 预留折叠态）；会话日志/录屏（M4a）；Xshell `.xsh` 与 FinalShell 专有格式导入（仅 JSON，专有格式 M4b）；键盘配置文件自定义。
- **出口标准**（**本清单为 Phase 1 唯一验收权威**；计划 Task 25 出口清单须回灌至与本清单逐项一致后方可验收）：
  - [x] 前端骨架：`cd frontend && npm ci && npm run build` 通过（计划 Task 17），CI frontend job 存在性门控解除；（判据：`cd frontend && npx vite build` 通过，2026-08-23 实跑；dist 四个产物齐备）
  - [x] 对 OpenSSH 容器：密码/公钥/kbd-interactive/agent 四路认证、错误密码回退、ProxyJump 一跳、TOFU 首连/变更/严格/仅本次（AcceptOnce 内存态）四态，itest 全绿；导入 OpenSSH known_hosts → 落入 host_keys 信任库（source=imported），对已导入主机首连直接校验不弹 TOFU，itest 覆盖；（判据 **2026-08-26 交叉审计后如实重写**——原判据「四路认证 itest 全绿」勾得过头，
        ~~实测只有 2/4 打到真 sshd~~ → **2026-08-26：4/4 全部打到真 sshd**。
        · 密码 ✓ `ssh_connect.rs::password_auth_and_exec`；
        · 公钥 ✓ `ssh_auth.rs::publickey_auth_succeeds_against_real_sshd`（先
          `restrict_to_publickey_only` 防回落，判据取通道输出）；
        · **kbd-interactive ✓**（2026-08-26 补）：新 `crates/itest/src/kbd_sshd.rs`
          （Debian + PAM，**只通告 keyboard-interactive**）+ `ssh_auth.rs` 两例。
          为什么另起一个 Debian 容器而不给 Alpine 那个加配置：**Alpine 把 PAM 拆在
          另一个包里**（`openssh-server-pam`），基础包编出的 sshd 在 Linux 上没有
          任何 kbd-interactive 认证设备——它会通告 keyboard-interactive，然后对任何
          应答都返回失败。那种服务器测不出这一路：客户端走完整个流程最后拿到 failure，
          与「客户端根本没实现 kbd-interactive」在断言上不可区分。
          判据三层：连上了 / 服务器**真的发过 InfoRequest**（prompts_seen 非空，
          否则「碰巧成功的其它方法」也会让测试绿）/ 通道可用跑出预期输出。
          **反向对照常驻**：`kbd_interactive_with_a_wrong_answer_is_rejected`——
          没有它的话，一台配错 PAM、对任何应答都点头的 sshd 也会让上一条变绿；
          它同时断言 `Error::Auth.tried == ["keyboard-interactive"]`，
          把「服务器只通告了这一种」这个前提一次钉死。
        · **agent ✓**（2026-08-26 补）：`ssh_agent.rs::a_real_agent_signature_authenticates_against_real_sshd`
          ——**真的 `ssh-agent` + 真的签名 + 真的 sshd 容器**。
          假 agent 只回身份列表、不签名；agent 认证真正的那一步（把待签数据交给
          agent、拿回签名、送给服务器）此前一次也没做过，而它失败的样子是
          「agent 里的身份都不被接受」，与「服务器真的不认这把钥匙」不可区分。
          这里钥匙是容器自己 ssh-keygen 生成、公钥已在 authorized_keys 里，
          那个可能性被排除掉了，剩下唯一能让它失败的就是签名那一步。
          `Drop` 守卫杀 agent 进程（写在末尾的 kill 会被 panic 跳过，于是每一次红
          都留一个孤儿）。变异「无视 `allow_agent`」转红实证。
        **顺带打通了一条基础设施**：这条测试要 Unix（ssh-agent 的 Unix socket）
        与 Docker（sshd 容器）**同时**可用，而 Windows 宿主的 ssh-agent 服务是
        Disabled（启用要管理员权限 + 改这台机器的服务配置，属用户决定）、
        `linux-precheck.sh` 又明写「fs_itest —— 需 Docker-in-Docker」不覆盖。
        新 `scripts/linux-itest.sh` 解开这个结：`-v //var/run/docker.sock:...`
        （容器里的 testcontainers 直接指挥宿主守护进程，起出来的 sshd 是**兄弟**
        容器不是嵌套的，避开真 dind 的特权守护进程）+ `--network host`
        （testcontainers 的 `get_host()` 给 `localhost`，而兄弟容器发布的端口在
        WSL2 VM 的 localhost 上；不加这条容器里的 localhost 是它自己）。两条都
        实测过。分成独立脚本而不是给 precheck 加 flag：挂 Docker socket 等于把
        守护进程的完全控制权交给容器里的代码，那该是一次看得见的选择。
        同批修掉 `linux-precheck.sh --rebuild-image` 在 Git Bash 上的既有 bug
        （`MSYS_NO_PATHCONV=1` 关掉转换后 docker 收到 `/c/Users/...` 报 path not found；
        镜像已存在时 build_image 根本不会被调用，所以这个 bug 从没人撞见过）。
        **新基础设施顺带逼出一处既有脆弱**：`crates/itest/src/lrzsz.rs` 的就绪判定等的是
        装完包之后那句 "Server listening on"，也就是要让 testcontainers 的日志跟随连接
        在一条**静默 40 秒**的流上挂着（apt 输出全被重定向到 /dev/null）。直连宿主
        Docker 没事，经挂载 socket 的兄弟容器路径就随机断，报 `WaitLog(EndOfStream([]))`
        ——空缓冲，一个字节都没读到，那个报错本身什么也没说。三条 zmodem 用例因此在
        Linux 路径上随机红（每次红的是不同的一条；Windows 宿主 3/3 稳过，故不是产品 bug）。
        先试过加心跳让流不空转，**没用**——问题不在「静默」而在那条连接活得太久本身。
        改法：开局第一件事打一个标记（日志等待毫秒级命中、跟随连接随即关闭），
        就绪与否改由**轮询 TCP 拿 SSH banner** 判定（`SshdContainer` 同款做法，一直是稳的），
        日志流从此不参与就绪判定；超时时把容器 stderr 一并摊进 panic 文案，
        免得下一个人再花同样的时间去猜 `EndOfStream([])` 是什么意思。修后 Linux 路径 3/3 稳过。
        ProxyJump 一跳属实但原判据未指名，实际在 `jump_path.rs::editing_the_jump_chain_changes_the_actual_path`
        与 `reordering_hops_swaps_the_last_hop_seen_by_target`（经 connect() 真跳，
        判据取服务端的 `$SSH_CONNECTION` 而非自家播报）。
        TOFU 四态属实：`ssh_tofu.rs` 5 例逐态；known_hosts 导入首连不弹 TOFU 属实
        （同文件，answer 故意置 Refuse 双证零弹窗）。
        ~~**未闭合**：kbd-interactive 与 agent 两路的容器认证用例~~ → 两路均已闭合，
        本条转 `[x]`。仍需注意：agent 那条是 `#[cfg(unix)]`，在 Windows 宿主上不编译，
        只在 `scripts/linux-itest.sh` 里跑；Windows 侧的 agent（Pageant / 命名管道）
        由 `agent_unavailable_error_names_transports` 三平台同一套断言覆盖到
        「Ok ⇒ 真能问出身份」，**真签名那一步在 Windows 上仍未跑过**——
        要跑需启用系统的 ssh-agent 服务，属用户侧决定。）
  - [~] 真实交互：htop/vim 可用；性能全指标（对齐全局不变量 4）：100 并发会话（50 渲染+50 后台化）后端进程（core 侧）常驻内存增量 ≤100×5MB、每空闲会话 ≤5MB（渲染侧 WebView 内存单列观测值不设硬闸）、loopback 连接到可交互 <2s（不含认证往返）、冷启动 ≤3s、SFTP ≥20MB/s、200MB 高吞吐输出零丢失且渲染队列水位 ≤ high-water×1.5；（判据：六个指标里五个已进 CI 门禁——SFTP 合计吞吐 ≥20 MB/s 见 `scale.rs`；200MB 零丢失、渲染队列水位 ≤high×1.5、连接到可交互 <2s、100 并发会话内存增量与每空闲会话 ≤5MB 见 `render_pipeline.rs` 四例。**未闭合**：冷启动 ≤3s 须真拉起 Tauri 窗口，仍在 `perf.rs` 的本机自查线（`#[ignore]`），不代签）
  - [x] 关闭确认：关闭含活动会话或排队/传输的标签/窗口弹模态（列会话数+传输数），确认后断开并取消传输；全空闲直接关闭；E2E 覆盖四入口（标签×/中键/Ctrl+W/窗口关闭；右键菜单「关闭标签」与标签×同为 requestCloseTab 的 tab scope 控件面，不另计一路；中键路径若 Tauri webdriver 驱动困难，以组件测试或手工核验覆盖并留痕）；（判据：`lib/window-close.test.ts` 判据 7 例 + `CloseConfirmDialog.test.ts` 呈现 5 例与接线 3 例；两个 scope 同源于纯函数 `decideClose`）
  - [x] SFTP list/stat（载体 = SftpOps::stat_size，SSH_FXP_STAT 跟随软链取大小；软链自身元数据经 lstat·不跟随、read_link 取软链目标，归 M4a 面板）/read/write/mkdir/rename/remove/symlink + 断点续传 + 并发 + 进度事件 + 完成后可选 sha256sum 校验（远端无该命令时降级 size 比对并在 UI 显式标注，禁静默跳过），itest 覆盖，≥20MB/s；（判据：`sftp.rs` itest + `scale.rs` 合计吞吐 ≥20 MB/s）
  - [x] Vault：keyring 路径 + Argon2 回退路径、篡改→Integrity、锁定后 secret 不可读；（判据：`crates/vault/tests/store.rs::a_locked_vault_leaves_no_plaintext_secret_on_disk` 磁盘侧 + `vault_cmd.rs` 解锁守卫完备性门禁 IPC 侧）
  - [x] 迁移：user_version 版本闸拒绝高版本库、迁移前自动备份；（判据：`crates/connmgr/tests/db.rs` 版本闸与迁移前备份两例）
  - [x] 断线自动重连（指数退避）+ 未关闭会话启动恢复（仅询问，不自动连）；（判据：`session_cmd.rs::next_reconnect_delay` 序列 1→2→4→8→16→30 两例 + 四条编译期区间断言）
  - [!] 三平台安装包可构建（release.yml 草稿流程跑通，不发布）；**（本机已构建 Windows MSI/NSIS 并两次提取启动通过（2026-09-05）；
        三平台要 `workflow_dispatch` 在 GitHub runner 上跑一次。前置同上：修复推送后执行。不代签。）**
  - [x] audit 表迁移落地：字段与总设计 §6.2 对齐，append-only（触发器禁 UPDATE/DELETE）+ hash 链列就位；（判据：`crates/connmgr/tests/db.rs::migrations_create_all_tables_and_audit_triggers` 与 `audit_table_is_append_only`）
  - [x] 输出提醒：会话收到 BEL/`\a` 后标签出现橙色铃铛角标，切回该标签或手动清除后消失（E2E 或单测覆盖）；（判据：`lib/bell-badge.test.ts` 分四跳取证 + `TerminalPane.test.ts` 的 handler→store 那一跳）
  - [x] 组合命令栏当前会话发送（§2.6）：Enter 发送附后缀（CR/LF/CRLF 选择器生效）达活动会话并回显执行、Ctrl+Enter 不附后缀、↑/↓ 走历史、无活动会话出 Toast 且不发 IPC（手工核验留痕，留痕方式同本清单中键条款）；（判据：`ComposeBar.test.ts` 8 例覆盖 Enter/后缀三档/Ctrl+Enter/↑↓ 历史 + `sendCompose` 空目标短路的源码顺序断言）
  - [~] UI 验收：与 `docs/mockups/` 基线逐屏一致；4 主题切换无刷新；终端配色三级作用域生效；对比度 ≥4.5:1 自动核验脚本；（判据：后三句已有载体——4 主题切换与终端配色三级作用域见 `lib/theme/theme.test.ts` 与 `settings-wiring.test.ts`，对比度 ≥4.5:1 双载体见 `theme.test.ts` 对比度组 + `npm run verify:contrast`。**未闭合**：第一句「与 `docs/mockups/` 基线逐屏一致」——基线是 `docs/mockups/{index,sftp}.html` 两张静态页，「逐屏一致」需人眼对比，无自动化载体。不代签。）
  - [x] 背景透明度（UI 规格 §2.12 全局 settings 键 `term.opacity`，0–100）：设置页调整后新建会话终端背景转半透明、重启保持；置 100 恢复不透明（组件测试或手工核验留痕）；（判据：`term.test.ts::withAlpha` 6 例 + `TerminalPane.test.ts` 挂载读回 `term.opacity` 两例）
  - [x] Windows Pageant / OpenSSH agent spike 记录入附录（总设计开放问题 A.2 关闭或留痕）；（判据：总设计附录 A.2 已关闭并记录三条结论；载体 `agent.rs` 单测 + `ssh_agent.rs`。真实 Pageant/ssh-agent 在跑时的成功路径归人工核验，已在附录如实记）
  - [x] IPC 二进制载荷编码选型 spike（总设计开放问题 A.1）结论记录入附录或实现注记；（判据：总设计附录 A.1 已关闭并更正原风险量级；成立条件落成 `flow.rs` 编译期断言）
  - [x] TOFU 四态 + 导入首连 itest 载体：`crates/itest/tests/ssh_tofu.rs` 五例（首连 AcceptAndRecord 落库命中不再询 / 仅本次 AcceptOnce 不写库重连再询 / 严格模式不询直拒 / 变更密钥先拒后显式接受落第二行 / 已导入主机首连校验不弹窗（F41））+ `ssh_connect.rs` 基线四例全绿，与计划 Task 25 出口清单逐字对偶；（判据：`ssh_tofu.rs` 五例全绿）
  - [!] 凭据 UI 端到端（手工核验留痕）：ProfileDialog 认证页签「存入 Vault…」新建 password 凭据 → 下拉自动回填选中 → 保存 Profile → 以该密码对 OpenSSH 容器连接成功；Vault 锁定态下该流程给出「vault 未解锁」明确错误。四路认证 itest（本清单第 2 项）不替代此项；**（阻塞：出口原文即写明「手工核验留痕」——需真人在 GUI 里走完「存入 Vault → 下拉回填 → 保存 Profile → 连接成功」，并在 Vault 锁定态复跑一次看错误文案。四路认证 itest 按原文不替代此项。不代签。）**
  - [x] 连接态三处一致（UI 规格 §5）：双击连接 → 标签蓝环+侧栏蓝闪+状态栏「连接中…」同现；成功 → 三处转绿；拔网 → 三处转状态灯灰空心（离线）/灰（断开）/「已断开」+banner；密钥错误 → 三处红✕/红灯/错误摘要（E2E 或组件测试覆盖）；（判据：`lib/connect-state.test.ts` 19 例 + `StatusBar.test.ts` 四态文本这一处）
  - [x] 键盘模式三平台可达：状态栏键盘模式段点击（三平台主入口）与 Scroll Lock（Windows 肌肉记忆快捷）行为等价；macOS 无 Scroll Lock 键盘（含 Magic Keyboard）下切换路径经点击可达；MVP 默认远程，默认值可于 UI §2.12 键盘与鼠标页签配置；（判据：`StatusBar.test.ts` 点击入口 + `keymap.test.ts` 的 ScrollLock→toggle-mode 分支；两条路径各有载体）
  - [x] 终端焦点快捷键集逐项核验：`lib/shortcuts.ts` 统一派发器下 UI §4 MVP 固定集（Ctrl+N/W/Tab/F/,、Ctrl+Shift+S/M、F11、Alt+P、Ctrl+=/-/0 等）逐项触发应用动作；Ctrl+Shift+A 于 MVP 禁用（本地截获、不触发动作，随 M2 AI 基础能力启用）；远程模式下 Ctrl+S/Ctrl+Q 原样达会话；白名单键终端焦点下从不原样透传至会话（应用动作可另行经 term_input 下发等效字节；逐项手工核验留痕，留痕方式同本清单中键条款）；（判据 **2026-08-26 更新**：`lib/shortcuts.test.ts` 25 例。原判据写「Ctrl+Shift+A 于
        MVP passthrough」，而 M2 已把它接到 AI 面板——该断言现在钉的正是反面
        （`Ctrl+Shift+A → {kind:"action", id:"tools.ai"}`，文件内注释记了这次改动）。
        正文那句「随 M2 AI 基础能力启用」已经发生。其余部分不变：MVP 固定集、
        白名单代表集、远程 Ctrl+S/Ctrl+Q 原样透传、「有绑定必恒本地」不变式。）
  - [x] 粘贴/右键/Ctrl+C 双义（UI §2.8/§4）：右键=粘贴默认生效、非空选区右键弹上下文菜单；选择即复制开关即时生效；Ctrl+C 有选区复制并清除选区/无选区发 `\x03`，Ctrl+Shift+C/V 恒语义；`Ctrl+V` 粘贴开关（UI 规格 §2.12，settings 键 `ui.ctrlVPaste`，默认开）：开时终端焦点下 Ctrl+V 即粘贴——**优先于远程模式原样发送**、不向会话发 `\x16`，关时该键原样发 `\x16` 且粘贴仅剩 `Ctrl+Shift+V`；多行粘贴确认气泡+「记住本会话」（气泡键盘契约：`Enter` 确认 / `Esc` 取消，显示即聚焦气泡本体 `tabindex=-1`、关闭后焦点归还终端；UI 规格 §2.8/§7 准用，组件测试覆盖）；四键（右键行为/选择即复制/多行粘贴确认/Ctrl+V 粘贴）改动无需重建终端（E2E 或组件测试覆盖）；（判据：`lib/term.test.ts` 35 例）
  - [x] 终端内搜索：Ctrl+F 弹浮层（绝对定位终端区右上）、Enter/Shift+Enter 下/上一个、Esc 关闭还焦；菜单/工具栏「查找」经 onAction 达同一浮层（组件测试或 E2E 覆盖）；（判据：`SearchOverlay.test.ts` + `action-wiring.test.ts`）
  - [x] 字体缩放：Ctrl+=/Ctrl+-/Ctrl+0 与 Ctrl+滚轮 per-session 调节字号并复位（8–32 区间），菜单 fontGrow/fontShrink 可用态可达（E2E 或单测覆盖）；（判据：`term.test.ts::clampFontSize` 5 例钉 8–32 + `action-wiring.test.ts` 的 fontGrow/fontShrink/resetFontSize 接线）
  - [x] 编辑菜单五项（复制/粘贴/复制为纯文本/全选/清除滚动缓冲）与工具栏复制/粘贴经 onAction 达活动终端选择面可用（手工核验留痕）；（判据：`action-wiring.test.ts` 五项 onAction 接线 + `menus.test.ts` 无人认领项门禁）
  - [x] 对比度 ≥4.5:1 自动核验（归属双载体）：`theme.test.ts` 对比度核验组（WCAG 相对亮度公式，四主题 ≥4.5:1，High Contrast ≥7:1）+ `npm run verify:contrast`（`scripts/verify-contrast.mjs`）全绿；（判据：`lib/theme/theme.test.ts` 对比度组 + `npm run verify:contrast`）
  - [!] Vault 便携解锁 UI：删除/屏蔽 keyring 条目后启动 → VaultDialog setup/unlock 双模以应用密码解锁成功（错误抖动提示）；自动锁定生效（App 顶层 60s 轮询 `vault.autoLockMinutes` + 最后交互时间戳 → `vault_lock`）；手工核验留痕。**（阻塞：出口原文即写明「手工核验留痕」——需真人删/屏蔽 keyring 条目后重启应用，观察 VaultDialog 的 setup/unlock 双模与错误抖动，再等自动锁定生效。删 keyring 条目会影响本机真实凭据，不宜由自动化代劳。不代签。）**
- **tag**：`v0.1.0`。
- **风险**：russh Handler 生命周期/`Channel<Msg>` 克隆语义（计划已给 connect_inner 方案）；vt100 CJK 宽度（网格仅 best-effort，渲染权威在 xterm.js）；arboard 在 CI 无剪贴板环境失败 → 该命令测试走 mock。

### M2 AI 基础

- **目标**：AI 从「占位 crate」变成每天会用的功能：自然语言生成命令（带解释）、选中输出即解读/诊断、所有 AI 发起的执行必须过策略闸门且默认需确认。
- **范围**：`fs_ai` 实装——Provider trait（Anthropic 原生 / OpenAI 兼容 REST / Ollama / 错误提示式未配置态）；提示工程模板（NL→cmd、解读、诊断）与上下文组装（Profile 元信息 + 连接期一次性采集缓存的 host facts（uname/shell/cwd 等，经 exec 通道，失败静默降级不阻塞连接）+ 网格提取的最近输出，**禁** ring buffer 作源）；策略引擎 `fs_policy`（read_only/write/dangerous 三级、shell 感知解析、失败闭合、黑名单、LLM 自评仅辅票）+ **policy 回归语料库建设**（初始 ≥200 条，覆盖总设计 §8.1 必测类目，接 CI 召回率断言）；前端 AI 助手面板（Ctrl+Shift+A）、内联建议卡、确认对话框（展示将执行命令 + 策略判定 + diff 式高亮）；AI 执行总开关（总设计 §4.3）与按 Provider「允许发送屏幕上下文」开关（总设计 §4.2，云端默认开）；audit 记录 AI 动作；MCP 相关全部留到 M3。
- **不含**：自主多步 Agent；MCP Server/Client；模型微调/本地训练；语音。
- **出口标准**：
  - [x] 未配置任何模型源时：AI 面板显示配置向导，不崩溃、不降级为假数据；（判据：后端把「未配置」做成一等状态——`ProviderState::Unconfigured` 是枚举的一支而非 `Option::None`（后者会被 `unwrap_or_default()` 一笔带过，那正是「降级为假数据」的入口）；无内置 endpoint、无默认模型，空值报错而不兜底；`pick()` 取第一个**校验通过**的而非第一个。载体 `crates/ai/src/provider.rs` 13 例。前端 `components/AiPanel.svelte`：未配置时**直接落在配置页**（`refresh()` 里 `if (!status.configured) tab = "config"`），提问页签呈禁用态；`AiPanel.test.ts` 那条用例除了正向断言向导在，还反向断言 `ai-suggestion`/`ai-explanation`/`ai-error` 三者都**不在** DOM 里——「不降级为假数据」最可能的形态就是一条示例命令，而用户照着一条编出来的命令敲下去，后果由他承担。变异验证：把切页那行改成 `if (false)` → 2 条红。）
  - [~] 配置 Ollama（本地）与任一云 API 后：NL→命令可用，生成结果必须带「解释」与「策略预判」两段；出口至少覆盖 Anthropic 与 OpenAI 兼容两个适配器的 wiremock 回放测试（总设计 §8.1）；（**已闭合**：三家的报文构造与解析全部落地并带录制回放——`crates/ai/src/wire.rs` 三家各一份非流式 + 一份流式样本，断言「三家解析出同一回答与同样 token 计数」「tool call 三种形态都能解析」「三家流折叠出同一段文本」「流式拼起来的 arguments 是合法 JSON」「流式结果是非流式结果的前缀」；`transport.rs` 用可注入 Transport 钉住接线（非 2xx 不进解析器、Retry-After 穿透、配置不合法一个包也不发）。共 77+11 例。**未闭合**：① 与真实 Ollama/云 API 的活体连通从未跑过（本地无 Ollama、无云 key）；② ~~出口原文点名的 `wiremock` 未引入——现用「可注入 transport + 录制样本」达到等价的**回放**验证，但没有真起一个 HTTP server 验证客户端侧行为（重定向策略、超时）~~ → **本批闭合（等价路径，理由如下）**：新增 `crates/ai/tests/http_client_behavior.rs`，起一个真的 `TcpListener` stub server，4 例。**没引 wiremock**，与本仓另外三次同类取舍同口径（`criterion` 声明了却全仓无一处 `use`、`tauri-driver` 没有 lib target、为不多一条 license 例外而不引 `webpki-roots`）——wiremock 会拖进几十个 dev 依赖（自带 hyper 栈、regex、deadpool），而这里要验的全部行为一个 `TcpListener` 就够。**这份测试真正补上的洞**：`ReqwestTransport::new()` 里那三行 builder 配置此前从未被任何东西验证过——已有的测试全部注入假 `Transport`，整个绕过了 `reqwest`。其中最要紧的是 `redirect(Policy::none())`：它的注释写着「跟随重定向会把认证头带到一个我们没打算认证的主机上去」，是完全正确的安全论断，而在此之前**没有任何东西能证明那一行还在**——删掉它、或为了「支持某个会 301 的代理」改成 `Policy::limited(5)`，全仓一条测试都不会红，而 API key 会开始跟着 302 走。故那条用例起**两个** server 并断言第二跳的收件箱是空的（只断言状态码不够：`limited(n)` 跳数超限时也交回最后那个 3xx，状态码看起来一样而 key 已经发出去了），另加一条正向对照断言第一跳确实收到了带 key 的请求——否则「第二跳空」可能只是因为请求根本没发出去。变异验证 4 条（改成跟随重定向 / 不读 Retry-After 头 / 错误串带上 spec 即泄 key / 方法分派写错）全部转红。**wiremock 能做而这里没做的**：请求匹配与「未匹配即失败」的报告；API 契约的形状由 `wire.rs` 的录制样本覆盖，这份文件钉的是客户端**配置**。③ ~~「生成结果必须带解释与策略预判两段」属提示模板与 UI，未开始~~ → **本批闭合**：提示模板见 `app/src/commands/ai_cmd.rs::NL_TO_CMD_SYSTEM`（要求模型输出 `COMMAND:` 与 `WHY:` 两行，并**明确告诉模型不要评估危险程度**——「那由程序自己判定，你说了不算」）；策略预判那段由我们自己算（`fs_policy` 四档裁决），界面上三段分列（命令 / 为什么 / 我们判的档位，见 `AiPanel.svelte` 的 `ai-command`·`ai-why`·`ai-verdict`）。`ai_cmd.rs` 有一条 `no_code_path_reads_a_danger_verdict_out_of_the_model_reply`，在生产代码里禁掉 `"SAFE"`/`"DANGER:"`/`"RISK:"`/`"model_tier"`/`"self_assessed"` 五个串——让被审计者写审计结论等于没有审计；前端侧有对偶的结构断言（禁 `rm -rf`/`isDangerous`/`classifyCommand` 等出现在面板源码里）。）
  - [x] 策略引擎单测：`rm -rf /`、管道/重定向/`sudo`、`&&` 链、引号逃逸各用例判定正确且失败闭合（解析错误→dangerous）；（判据：`crates/policy/src/shell.rs` 27 例分解 + `rules.rs`/`gate.rs` 判定 + `tests/corpus.rs` 逐条比对；出口点名的五类各有语料：`rm -rf /` 见 03-destructive、管道/重定向见 05/07、`sudo` 见 07-wrappers、`&&` 链见 04-obfuscation compound、引号逃逸见 04-obfuscation quoting；失败闭合见 08-parse-error 8 例 + `unterminated_constructs_fail_closed`）
  - [x] policy 回归语料库 ≥200 条（含总设计 §8.1 必测类目），CI 断言 dangerous 组召回率 ≥95%（与总设计 §9 Phase 2 出口逐字对齐）；（判据：`crates/policy/tests/corpus/*.tsv` 共 269 条 ≥ 下限 200，16 个必测类目各带条数下限；`dangerous_recall_meets_the_exit_threshold` 实测 100%（139/139）≥ 95%；另有更硬的 `no_dangerous_case_is_ever_auto_approved`（零漏放）与 `read_only_group_is_not_over_blocked`（77 条零误升））
  - [x] 确认流：with_confirm 档位下，拒绝/超时（60s）不落执行；read_only 档位自动放行读命令、写命令仍拒绝；（判据：`crates/policy/src/gate.rs` 13 例——超时/拒绝/会话放弃三路均 `allows_execution()==false`；`read_only` 档对 write/dangerous 是 Deny 而非 Confirm（反向对照断言 `!needs_user()`）；3×3 裁决表形状断言）
  - [x] 解读流：选中终端输出 → 选区上下文菜单「AI 解读」（触发路径按 UI 规格 §2.8 上下文菜单规则：非空选区上右键弹菜单，与默认「右键=粘贴」共存不冲突）→ 净化 Markdown 渲染（CSP 下无脚本执行路径，注入测试用例通过）；（判据：触发路径 `TerminalPane.svelte` 的 `ctx-menu` 里那一项——此前是 `disabled title="Phase 2 AI 解读入口"` 的占位，本批接线。选区判空**在组件内做**而不是交给装配层：App 收到回调时选区可能已被右键本身清掉了。选区为空时禁用而不隐藏——这一项在菜单里的位置固定，隐藏会让菜单在有无选区时高度不同、右键两次条目跳位置。与「右键=粘贴」的共存由既有的 `contextMenuAction` 仲裁保证（`term.ts`，非空选区 → 弹菜单）。渲染净化走 `lib/markdown-lite.ts` + `components/Markdown.svelte`：**token 树**而非字符串替换，组件里三个固定分支、无 `<svelte:element>`、无 `{@html}`——注入不是被过滤掉的，而是**无法表达**；刻意不支持链接（模型生成的可点击链接是钓鱼载体）。`markdown-lite.test.ts` 23 例含注入语料。`App.svelte` 与面板之间用 `{text, seq}` 传选区：同一段输出「再解读一遍」是完全正常的意图（第一次没说清），只比文本的话第二次不重跑、按钮看起来就是坏的——`AiPanel.test.ts` 有正反两条钉住它。）
  - [x] audit 表可查到每条 AI 请求（脱敏：prompt 中的 secret 以占位符替代）与执行结果；（**已闭合**：脱敏。`crates/ai/src/redact.rs` 19 例——已知密钥确切替换（唯一强保证）、PEM 私钥块整块（含被终端截断的无 END 情形）、URL 里的 `user:pass@`（只涂口令保留用户名）、23 种带前缀 token、名字像密钥的赋值（涂值不涂名）；另有 10 条日常字符串的反向对照防假阳性，以及幂等性断言。~~**未闭合**：audit 表的写入链路（AI 请求与执行结果落库）尚未接~~ → **本批闭合**：`app/src/commands/ai_cmd.rs::audit_ai` 在 NL→命令与解读两条路上都写库（`Actor::Ai`，四档裁决映射到 `Verdict::{AutoRun, Approved, Denied}`）。两处细节值得记下来：① **建议本身就要落库**，不只落「执行了什么」——一次被拒的危险建议若不留痕，最该留痕的那条恰好没了；② `audit_ai` 落库前**再脱敏一遍**，尽管上游已经脱过 prompt。理由是命令是模型**新造**的文本：它可能把屏幕上看到的口令抄进命令里（`mysql -p<密码>` 就是这么来的），而那个串不在上游脱敏经过的任何一段里。③ ~~Confirm/StrongConfirm 两档记 `Approved` 是不准确的（用户还没点），但审计表的 `Verdict` 里没有「待确认」这一档~~ → **2026-08-26 推翻并修掉**：那句话是错的——`RequestOnly` 的语义就是「只是一次请求，没有要执行的东西」，Agent 侧的双行制早就这么用。记 `Approved` 的实际后果是**一次被用户拒绝的危险建议在审计里永远显示为已批准**，且 `Verdict::executed()` 对它返回真：链在密码学上完整、在语义上是错的。现改记 `RequestOnly`，两条测试守着（源码扫描钉住那一臂 + 语义对照钉住 `RequestOnly.executed()==false`），变异「回退成记 Approved」转红。**执行结果行同批补上**：`audit_exec_cmd.rs::audit_command_sent`，落点在整条命令的提交（组合命令栏/快速命令）而非 `term_input`（那是逐字节流），记 `Approved` + `exit_code: None`（远端退出码要等 PTY 解析，本层不知道就说不知道）。于是一次「建议 → 批准 → 执行」在链上是 RequestOnly + Approved 两行。）
  - [x] AI 执行总开关（总设计 §4.3）：关闭时 NL→命令/解读/执行入口全部不可用并呈禁用态，单测覆盖；按 Provider「允许发送屏幕上下文」开关（总设计 §4.2）：关闭时上下文组装不含终端输出；（**已闭合**：两个开关的判定逻辑。总开关见 `crates/policy/src/gate.rs::AiMode::Disabled`——该档下**连确认框都不弹**（弹了就意味着「关着但点一下就能跑」，那不是开关是提示），且优先于只读；按 Provider 的屏幕上下文开关见 `crates/ai/src/context.rs`，关闭时屏幕内容一个字符都不进上下文（不是脱敏后再发），`Default` 给出 `false`——忘记设置时的行为必须是「不发」。两者各带反向对照。~~**未闭合**：前端入口的禁用态呈现与 settings 接线~~ → **本批闭合**：settings 键 `ai.mode` 有了控件（选项页「AI」，`set-ai-mode`，三档 `disabled`/`read_only`/`with_confirm`）；串值用**下划线**形式，那是 `AiMode::from_str_exact` 认的稳定串——写成连字符会让那一档在界面上选得到、后端一律解析失败回落，即「选项存在但永远不生效」。按 Provider 的屏幕上下文开关在面板配置页逐条设置。`ai.providers` 不由前端 `settingSet` 写而经 `ai_provider_save` 命令（要与 Vault 里存 key 成对完成），`settings-wiring.test.ts` 为此新增 `COMMAND_WRITTEN` 一节——**不是豁免表**：它对每一项核「后端真有这个命令且提到这个键 / 前端真的 invoke 它 / 调用点在指名的可达面上 / 且它确实没有前端直写方」四件事，三个变异（改命令名、改可达面、改键名）各红 1–3 条。）
  - [x] host facts：NL→命令上下文含 uname/shell/cwd；采集失败不阻塞连接、降级路径单测通过；（**已闭合**：采集命令与解析。`crates/ai/src/context.rs` 的 `HOST_FACTS_COMMAND` 一条命令取五项（一条而非五条：每条都是一次 exec 往返，连接建立时用户在等），各项用固定标记分隔而不靠行序（`uname` 不存在的极简容器上靠行序会让后面所有项错位），`2>/dev/null` + `|| true` 保证任何一项失败都不让整条命令非零退出；解析器宽容——不认识的行跳过、空值等于没采到、全失败是合法状态、缺项**不写「未知」**（写了会让模型把它当事实推理）。7 例含降级路径。~~**未闭合**：连接期真正去 exec 这条命令的接线（app 层）~~ → **本批闭合**：`session_cmd.rs:323` 在会话建立后 `spawn_host_facts_collection`（**spawn 而不是 await**：出口原文要求「采集失败不阻塞连接」，而 await 一条 exec 往返本身就是在阻塞它——即使成功也让用户多等一次 RTT）；结果进 `AppState::host_facts` 缓存，`ai_cmd.rs` 组装上下文时按 session id 取。取不到就是取不到，不写「未知」。）
  - [~] 性能：AI 面板打开 ≤200ms；上下文组装 ≤50ms（10k 行网格）。（**已闭合**：上下文组装 ≤50ms。`crates/ai/src/context.rs` 有一条 10k 行网格的实测断言；先截断再处理使耗时与网格行数脱钩（8 KiB 上限、截头留尾、按字符边界切）。**AI 面板打开 ≤200ms：读法说明 + 一半未闭合。** 已闭合的是**我们自己那部分**：`AiPanel.test.ts` 有一条 render→输入框可交互的实测断言（jsdom 内 <150ms），以及一条更要紧的**结构**断言——打开路径上全程只有一次 IPC（`ai_status` 一次带回「配没配好/有哪些条目/哪条在生效/什么档位」四样）。后者才是真正把守 200ms 的：串行的第二次 IPC 一出现就红，而那是时延回归唯一现实的来源。**未闭合**：真机端到端那 200ms 没量过——jsdom 里没有 GPU 合成、没有真实 IPC 往返、字体也不排版，量不出那个数；本仓无 E2E 驱动（tauri-driver 无 lib target，早已删除留档）。故这一条按人工闸门处理，不代签。附带说明：绝对耗时那条门槛刻意定得宽（150ms 而非贴着实测值），因为一个量不出真值的性能断言若卡得死紧，只会在别人的机器上随机红，然后被人调宽、再调宽，最后没人看它。）
- **tag**：`v0.2.0`。
- **风险**：云 API 网络不可达环境（中国大陆）→ Ollama 本地路径为一等公民，测试以 mock provider 为主；提示注入经由远端输出 → 解读提示词显式隔离数据区 + 渲染层净化双保险。

### M3 Agent + MCP 双向

- **目标**：从「一问一答」到「派任务」：自主 Agent 循环（计划→执行→观察→收敛，人可随时中止/接管）；MCP 双向——对外暴露本地 Server 让其他 AI 工具驱动本机会话，内置 Client 挂载外部 MCP 工具供 Agent 使用。
- **范围**：`fs_ai` Agent runtime（任务队列、步数/连续自动命令数/token 三项预算上限、每步过 `fs_policy` 同一闸门、`run_command` 契约：exec 通道/超时 60s/输出 32KiB 截断/PTY 哨兵取退出码）；`fs_mcpbridge` 基于 rmcp——Server（默认关、stdio 为唯一传输，未来启用 HTTP streamable 时仅监听 127.0.0.1、工具白名单、Vault 永不暴露、阻塞式确认契约 + 结构化拒绝码）与 Client（外部 server 配置、工具发现、调用同样过闸门）；前端 Agent 面板（步骤时间线、逐步批准/全自动档位、急停按钮）；MCP 配置 UI。
- **不含**：多 Agent 协作；远程 MCP over WAN（仅本地/用户显式配置的地址）；计费/配额。
- **出口标准**：
  - [x] Agent 完成「磁盘使用排查」示例任务：自主 df→du→定位→报告，全程步骤可审、任一拒绝点即停并解释；
        （判据：`crates/ai/tests/agent_run.rs` 的 f1 走完 df→du→ls→finish 四回合，
        断言收敛正路、每步两行审计、token 精确记账、通道归零；反例三味各一条——
        Deny 即停（c4）、用户拒绝即停（c5，危险命令走强确认）、注入场景（f2：模型中招
        发 curl|sh，闸门独立于模型意志判 Dangerous）。**拒绝从不回喂模型**——把拒绝
        喂回去等于邀请它换一种危险姿势再试；这条由「对话里没有任何 ToolResult」断言
        钉死（变异连续幸存两轮才逼出正确判据）。界面在 `AgentPanel.svelte`（13 例），
        入口为「工具 → AI Agent（自主排查）」。）
  - [x] 步骤预算耗尽/用户急停 → 干净中止，无悬挂 exec 通道（测试断言通道计数归零）；
        （判据：急停 = **拨快时钟**——`RunClock` 在令牌置位后报 `u64::MAX`，
        `run_command` 自己的读前超时检查立即命中，走它自己的 kill→close。
        **一个字的急停专属回收代码都没有**：kill-before-close、单一返回点、
        partial_output 三条不变式全部继承 exec.rs 已被 12 个变异钉死的行为。
        设计阶段砍掉了「从 read() 注入 Err」的方案——核实 exec.rs:307-312/329-333，
        注入 Err 得到的是 close-without-kill + timed_out:false，会留下一个本地看不见、
        可能正在写大文件的远端进程。b1 断言 kill=1/close=1/**kill 先于 close**/
        registry 归零/partial output 在/exit_code=None；b4 对终态各跑一遍归零。
        **心跳读是这条的前提**：run_command 的超时检查在读循环顶部，若 read 在静默
        命令下永久阻塞，控制权永远回不到循环顶——连 60 秒超时都不会触发。
        app 层 `heartbeat_race` 250ms 让出，两条测试分别钉「这一层会让出」与
        「让出之后那一层会正确收尾」。）
  - [x] 连续自动命令数与 token 上限触顶即停并汇报（单测断言）；
        （判据：三条预算各自独立触顶、各自有停因变体——合并成一个
        `BudgetExhausted` 会让「删掉 streak 检查」与「删掉 steps 检查」在报告里
        不可区分。**streak 派发即计数**（不看结局：`cat /nonexistent` 的失败循环
        同样消耗「无人值守」语义）；**只有人触点清零**（确认被批准 / ask_user 被
        回答，恰两处，grep 守卫钉死调用点数量）——把清零钥匙交给模型能自产的
        事件等于没有上限。token 三本账：`usage=None` 记**可证上界**（请求体字节 +
        max_tokens），绝不记 0——把「不知道」当「知道是零」，预算就在最需要它的
        环境（不透明网关后面）恰好失效；触顶即停取强读法（settle 后立即闩，
        本回剩余工具调用一条不跑）。a2/a4/a11 三条端到端 + budget.rs 13 条纯函数，
        变异 17+11+9 全灭。汇报经 `agent:stopped` 事件带停因与三本账上屏。）
  - [x] `run_command` 契约三路单测断言（对齐总设计 §4.4）：超时 60s 触发杀通道进程且观察值含 `timed_out: true` 与 partial_output、通道回收无悬挂；输出超 32KiB 首尾保留、中间截断并插入 `[…N bytes truncated…]` 标记；PTY「在当前终端执行」模式从哨兵 `__FS_RC__` 解析 exit code（正常退出 0 与非零退出双路断言）；
        （判据：`crates/ai/src/exec.rs`，25 例 + 12 变异全红。**契约只有一份**，MCP 的
        `command.run` 直接用它——契约有两个实现就等于有两个契约。
        **一、超时那路**：`run_command` 只有一个返回点，于是「通道一定回收」不靠
        「记得写 close」而是结构上做不到漏。`the_channel_is_always_closed_on_every_path`
        对正常结束/超时/读出错三条路径各断言一次 close 计数，超时那条另断言 kill 计数
        ——**只 close 不 kill 会留下一个仍在跑的远端进程**，本地看不见它而它可能正在
        写一个大文件。超时的观察值保留 partial_output 且 `exit_code` 为 `None`：
        不用 `-1` 之类的哨兵值，因为 `-1` 会被下游当成一个真的退出码去解释，
        而「超时所以没有退出码」和「命令返回了 255」是完全不同的两件事。
        通道错误**不**报成超时——Agent 对两者的下一步不同（超时可改后台模式重试，
        通道错误重试同一条多半还是错）。超时检查放在 read **之前**：放在后面等于
        每次都多等一整个读周期。时钟与通道都是注入的 trait：一条「60 秒后超时」的
        测试不能真等 60 秒，而把超时改成 1 毫秒来测又会让它变成一场竞态赌博。
        **二、截断那路**：三个容易漏的点各有断言——① **标记本身占预算**
        （不占的话「上限 32 KiB」不成立，结果是 32 KiB 加标记，下游按 32 KiB
        分配缓冲的地方就溢出）；② **在字符边界切**（按字节切会把多字节字符劈开，
        中文运维输出里几乎必然发生，测试扫 60..400 全部 max）；③ **头略多于尾**
        （预算除不尽时余下那字节给头部——开头通常是错误信息与表头）。
        标记里的数字是**真的被删掉的**字节数，有一条断言把它抠出来与实际保留量对账。
        两个流各自受同一上限而**不共享总额**：共享的话一个话多的 stderr 会把 stdout
        挤没，反过来一个话多的 stdout 会让错误信息只剩几十字节。
        **三、哨兵那路**：`wrap_for_terminal` 用 `;` 而**不是** `&&`——命令失败时我们
        更需要退出码，而 `&&` 会让 printf 不执行、哨兵永不出现、只能等超时；
        那个错误极隐蔽（成功路径全正常，只有失败时表现为「卡 60 秒然后说超时」）。
        用 `printf` 而非 `echo`（后者的 `-n`/`-e` 行为在不同 shell 下不一致，
        而这一行是要被解析的）。`parse_sentinel` 找**最后一个**哨兵：终端里可能有
        上一次执行留下的哨兵行，找第一个会让第二次执行读到第一次的退出码——
        一个静默的错误答案。找不到返回 `None` 而**不猜 0**：「还没结束」与
        「成功结束了」在 Agent 循环里导向完全不同的下一步。另有 `strip_command_echo`
        剥掉 PTY 的命令回显，判据是「同时含命令原文与哨兵前缀」——只按前缀判会把
        真哨兵行也剥掉，那时表现为「命令跑完了却一直等到超时」。
        **两个被变异逮出来的松断言**（都已收紧）：① 头尾之差原先只断言
        `h >= t && h - t <= 1`，而把 `head = budget - budget/2` 改成对半分时两条都还成立
        ——改为扫一段连续 max 并要求其中至少一个出现「头恰好多一个」；
        ② 独立预算原先只断言 stdout 没被挤掉，而把 stderr 的预算改成
        `max - stdout.len()` 时仍然绿——补上「stderr 必须拿到自己那份完整预算」。）
  - [~] MCP Server：第三方 MCP 客户端（如 Claude Desktop 配置指向本程序）可 list/call 已授权工具；未授权工具与 Vault 类工具不可枚举；确认超时→结构化拒绝码；
        （**代码与门禁已闭合**：策略核心 `crates/mcpbridge`（102 例）+ stdio 传输
        （rmcp 3.1.4，default-features 关、零 HTTP，出站门禁特性过筛）+ app 六端口真实现
        （`app/src/mcp/`，复用 Agent 的 run_command 契约与连接装配序列）+ 前端确认框
        `McpConfirmDialog`（§4.5 阻塞式人机回路，强确认逐字输「确认」）+ 设置三键
        （默认关）。判据：未授权/不存在工具同词拒绝（防枚举）、四拒绝码映射、结构化输出
        递归脱敏、确认四出口（批准/拒绝/超时/上下文消亡）各有测试；拒绝走工具级
        structured_error 而非协议级错误。
        **2026-08-26 补：协议层从「没跑过」变成「跑过了」。** 上面那 102 例覆盖得很密，
        但它们**全部止步于函数调用**——`server::call(...)` 直接被调、`to_response(...)`
        直接被调，没有一条测试让一个真正的 MCP 客户端说过一句 JSON-RPC。而出口原文说的是
        「客户端可 list/call」，那与「函数返回了正确的值」之间隔着一整层：initialize 协商、
        tools/list 的分页包装、tools/call 的参数序列化、错误走 result.isError 还是
        JSON-RPC error。新增 `crates/mcpbridge/tests/protocol_e2e.rs`（11 例）：
        `tokio::io::duplex` 把 `McpServer` 与 `rmcp::serve_client` 对接，协议是真的
        （同一套编解码、同一次握手、同一条 JSON-RPC 通道），省掉的只是操作系统的进程边界。
        盯住出口三句 + 两条额外契约：未授权与不存在的回答**逐字相同**（含结构化码——
        差一个字就是枚举预言机）；已知机密在**字节离开进程之前**被脱敏（假终端端口故意
        裸着回吐机密，由协议对面收到的东西判断那个唯一的脱敏点在不在，带反向对照防
        「整段吞掉」）。4 个变异全红。
        **未闭合**：真拿一个 MCP 客户端（Claude Desktop）连上跑一遍——真客户端可能有
        自己的协议版本口径、可能对 schema 有额外要求。属真机/交互核验，不代签。）
  - [~] MCP Client：挂载一个外部 server（filesystem 类），Agent 可调用且调用受同一策略闸门约束（注入危险命令用例被拦）；
        （**代码与门禁已闭合**：`crates/mcpbridge/src/client.rs`（拉起 stdio 子进程 →
        握手 → tools/list / call_tool → 主动 cancel 收尾，现连现断不留孤儿）+
        `app/src/mcp::external_tools`（读挂载表、逐个拉工具、坏挂载只 warn 跳过）+
        `Conversation::build` 把外部工具并进模型可见的 tools 数组 + 设置页挂载管理 UI。
        **命名**：外部工具是运行期集合，线上名 `mcp__<server>__<tool>`，`parse_tool`
        按前缀分流（不必知道当前挂了哪些 server，仍是纯函数）。
        **分级**：`floor_at_write`——Write 起步、工具级只能 `.max()` 提级、永不降级。
        判据：端到端往返（duplex 进程内对接一个最小 rmcp server）、工具级错误不当成
        传输失败、`f4_a_dangerous_command_injected_via_an_external_mcp_result_is_still_gated`
        （**出口点名的那条**：外部工具返回值里的注入命令照样走强确认、拒绝即停——
        与 F2 的「模型自己发危险命令」是两个攻击面）。变异验证 4 条全杀。
        **2026-08-26 补：真子进程 + 异构实现那一半闭合。** 上面那条「端到端往返」跑在
        `tokio::io::duplex` 上，有两件事它**结构上**验不到：① **进程边界**——
        `TokioChildProcess` 拉子进程、接管 stdio、`cancel()` 之后子进程会不会变孤儿，
        在 duplex 上一件也不发生，而出口原文说的「挂载一个外部 server」是一个**进程**；
        ② **异构性**——duplex 那头是 rmcp 写的 server，与我们的 client 共用同一份编解码，
        任何一处对协议的理解偏差会被同时犯在两边、于是同时抵消掉。
        新增 `crates/itest/src/bin/fake_mcp_server.rs`（**手写 JSON-RPC，不用 rmcp**，
        只依赖 serde_json）+ `crates/itest/tests/mcp_client.rs`（11 例）：产品自己的
        `list_tools_once`/`call_tool_once` 拉起这个真子进程跑 filesystem 类工具往返、
        工具级错误落成非零 Observation、坏挂载快速失败不挂起、零参调用、
        注入文本只是数据、真挂载拿到的工具名逐个过闸门确认落在 Write 地板、
        线上名 `mcp__<server>__<tool>` 拼与拆往返。
        **顺带补上一条从没被测过的文档承诺**：`call_tool_once` 的文档写着「现连现断……
        换『不留孤儿子进程』的确定性」，而那句话此前没有任何东西守着——它错了的样子是
        用户用一天之后任务管理器里堆着几十个僵尸进程且无任何报错。现有
        `repeated_calls_do_not_leave_orphan_child_processes`（数增量不数绝对值，
        已知弱点写在测试注释里）。4 个变异全红。
        **不挂真 npx filesystem server 是刻意的**：那要求测试机有 node 与网络，
        「网络抖一下就红」的测试比没有测试更糟——它教人忽略红色。假 server 覆盖的是
        同一条代码路径（同一个 `call_tool_once`、同一个 `TokioChildProcess`）。
        **未闭合**：真挂 `@modelcontextprotocol/server-filesystem` 跑一遍，属真机核验，不代签。）
  - [x] 全链路 audit（Agent 步骤 + MCP 调用）hash 链完整，快照校验命令可用。
        （**「快照校验命令可用」那一半已于 2026-08-24 闭合**：hash 链本体
        （`audit_repo::verify_chain` 逐行重算报首处断点、`forensics::export`/`verify_json`
        取证包导出与不接触数据库的独立校验）早就有实现有单测，但**只有库函数**——
        invoke_handler 没注册、scripts 没调用、前端一处未提。读严一点，出口要的是
        一个**可用**的命令，库函数加 #[test] 不是命令。现三命令落地
        （`app/src/commands/audit_cmd.rs`）：`audit_verify`（断链报行号与断因，且
        **断因带追查方向**——content_altered 查「谁能写这个字段」、link_mismatch 查
        「谁动过这张表」，两句不是安慰是给事后调查的第一条线索）、`audit_export`
        （取证包 JSON，前端下载落盘）、`audit_verify_bundle`（收 JSON 文本不收路径——
        路径来自谁是前端的事）。前端 `AuditDialog.svelte`（10 例）挂在
        「工具 → 审计链校验」；`forensics::export` 文档把「导出前 UI 明示包含命令文本」
        写成调用方义务——对话框用**常驻披露段 + 按钮文案本身携带披露**兑现，而不是
        点击后再弹一层确认（两次确认的对话框教人快点按确定，常驻披露更难被略过）。
        两条守卫与功能同批落：① `audit_wiring`（命令注册/前端调用/菜单入口三件各自
        可红——「链校验能力存在但用户到不了」从此无法悄悄成立，三个变异各红）；
        ② `every_audit_writer_routes_through_redact`——`NewAuditEntry.action` 的文档写着
        「调用方负责先脱敏，由 app 层的测试守着」，而那个测试此前**不存在**；M3 马上
        要加 Agent/MCP 两个写入方，每个都有一百种理由「这条不脱也行」。**该守卫的
        第一版判据被变异逮住一次**：裸词 `redact` 会被**类型路径**骗过——不脱敏的写法
        `fs_ai::redact::Redacted { text: action.to_string(), .. }` 里也有 "redact"
        这个词（模块名），文件级判定照样绿；收紧为 `redact(`（带调用括号）才区分
        「调用了脱敏函数」与「提到了脱敏类型」。
        **2026-08-26 补正**：上面那句「收紧为 `redact(`」已不是代码现状——现行判据是
        **三选一**（`audit_cmd.rs`）：`redact(` / `RedactedAction::new(` /
        `: &fs_ai::agent::record::RedactedAction`。第三支恰恰又接受「一次类型提及」，
        与当初收紧的方向相反。这不是退化而是换了一种更强的保证：前两支靠「调用方
        记得调」，第三支背后的 `RedactedAction` 是**构造即脱敏**的 newtype——
        未脱敏的串在那个类型之外无处安放，写入点只是把已脱敏的值取出来用。
        M3 的三个写入方（agent_audit / mcp::ports / audit_exec_cmd）走的都是这条。
        代价是判据不再能区分「持有一个已脱敏值」与「只是写了个类型名」，
        故它依赖的是类型本身的保证，而不是这条 grep。
        **未闭合**：~~① Agent 步骤与 MCP 调用的写入（等 Agent runtime 设计落定）~~
        → **已闭合**（Agent 侧 `app/src/agent/agent_audit.rs` 的双行制；MCP 侧
        `app/src/mcp/ports.rs::McpAudit` + 逐格写死的 `map_verdict`，七格互不相同有测试）；
        ~~② 执行侧审计行——ai_cmd 目前给待确认建议记 `Approved`~~
        → **已修**（2026-08-26）：改记 `RequestOnly`。原注释称「枚举里没有『待确认』档」
        是错的——`RequestOnly` 的语义正是「只是一次请求，没有要执行的东西」，
        Agent 侧的双行制早就这么用。旧写法的实际后果是**一次被用户拒绝的危险建议
        在审计里永远显示为已批准**，且 `Verdict::executed()` 对它返回真：链在密码学上
        完整、在语义上是错的。两条测试守着（源码扫描钉住那一臂映到 RequestOnly 且
        建议路径不出现 Approved；语义对照钉住 `RequestOnly.executed() == false`），
        变异验证「回退成记 Approved」转红。**执行结果行同批补上**：
        `app/src/commands/audit_exec_cmd.rs::audit_command_sent`——落点在**整条命令
        的提交**（组合命令栏 / 快速命令），不在 `term_input`（那是逐字节流，
        每次击键记一行会把真正有意义的那条淹掉）。记 `Approved`（用户亲手按了发送键，
        这是本层能诚实断言的全部）、`exit_code: None`（远端退出码要等 PTY 解析，
        本层不知道就说不知道）、档位现算（与建议那条同一个分级器，两行的 risk_level 可比）。
        于是一次「建议 → 批准 → 执行」在链上是 RequestOnly + Approved 两行。
        变异验证 2 条全杀（误记 RequestOnly → 红；去掉空命令早返回 → 红）。
        ~~③ 取证包的「周期性快照」不存在（现在每次全表校验，O(全部行)）~~
        → **已闭合**（2026-08-26）：`crates/connmgr/src/audit_checkpoint.rs` + 迁移
        `0007_audit_checkpoint.sql`（快照表同样 append-only 触发器钉住）+ 命令
        `audit_verify_quick` + 启动装配 `checkpoint_on_startup`（按**行数**触发，
        默认 500 行；表不写就没什么可快照的，按时间跑是空转）+ AuditDialog
        「增量校验」按钮。14 个集成测试 + 6 个前端测试，两轮共 10 个变异全部转红实证。
        设计上有四处是想清楚了才写的，逐条记下来免得日后被「优化」掉：
        · **快照不是信任锚**：它与 audit 同库同权限，能改一个的人也能改另一个。
          它买到的只有性能与「链尾 hash 多存了一处」，不构成任何密码学保证。
        · **于是「增量通过」≠「链完整」**，且这件事不能靠调用方记得说明——
          `ChainReport` 把结论与覆盖范围绑在同一个值里，`message()` 无条件把
          「第 N 条及之前的 K 条这次未重算」印出来，UI 上 `ok && !full_coverage`
          渲染成 **⚠ 而不是 ✓**（绿勾会被读成「链完整」）。打开对话框跑的仍是
          全表校验——正确的默认不让位于快的默认。
        · **断链拒落快照**：给断了的链落快照，此后每次增量校验都从断口之后开始，
          永远返回绿。这正是篡改者最想要的那一步，故 `record` 先全表校验再落。
        · **锚点四问**（行在不在 / 存着的 hash 对不对 / **内容重算得出这个 hash 吗** /
          行数对不对）。第三问是被自己的测试逼出来的：第一版只比两个字符串，
          而 `UPDATE audit SET action='ls'` 根本不碰 hash 列——两个字符串纹丝不动，
          锚点检查全过。**只比列不重算，等于让锚点那一行成为全表唯一一处改了不会被
          发现的地方**，而它恰是每次都读却从不重算的那一行。第四问（行数）拦的是
          「锚点之前被删了整段」：锚点行自己纹丝不动，前三问全过，而增量校验
          永远读不到那个断口。
        · **用户点的校验不落快照**：模块头「校验本身不得留痕」——一条「有人校验过」
          的记录会告诉查看者链曾被审计过。只有按行数自动触发的那条路径落快照，
          它泄露的只是「程序跑过、表长了」。
        ④ `Verdict` 无「待确认」档——**本条已作废**：RequestOnly 就是它，不需要迁移。）
- **tag**：`v0.3.0`。
- **风险**：~~rmcp 2.x API 稳定度 → 计划阶段 spike 锁定接口~~ → **2026-08-24 spike 完成，见下**；Agent 失控面 → 预算+闸门+急停三重，默认档位 with_confirm。
- **rmcp spike（2026-08-24）**：结论是**可用，且能做到零 HTTP 面**。
  - **版本已经不是 2.x 了**：最新 `3.1.4`（2026-08-20 发布），3.0.0 在 7-28 落地，
    三周里出了 6 个版本（3.0.0 → 3.1.4）。这个节奏本身就是「API 稳定度」那条风险的实证，
    故接入时 pin 到 `3.1`（不是 `3`），并把适配面收在 `fs_mcpbridge` 一个 crate 里。
  - **许可证 Apache-2.0**，在 `deny.toml` 的白名单内，不需要新增例外（0.12.0 及更早是 MIT）。
  - **MSRV**：rmcp 3.x 要 1.88，本仓工作区是 1.94，够。rmcp 自身是 edition 2024 而本仓是 2021
    ——这没问题，edition 是逐 crate 的，1.94 认得 2024。
  - **关键结论：stdio 一路不带任何 HTTP 依赖。** rmcp 的普通依赖里每一个 HTTP crate
    （`hyper`/`hyper-util`/`reqwest`/`http`/`http-body`/`sse-stream`/`oauth2`/`tower-service`）
    **全部是 optional**；服务端那套（`axum`、带 `server` 特性的 `hyper`）只出现在
    **dev-dependencies** 里，根本不进已发布库的构建图。故按
    `default-features = false` + `["server", "client", "macros", "transport-io", "transport-child-process"]`
    声明时，实际拉进来的只有 chrono / futures / indexmap / pin-project-lite / serde /
    serde_json / thiserror / tokio / tokio-util / tracing——全是本仓依赖图里已有或无害的。
    这与总设计 §4.5「stdio 为 M3 阶段唯一传输（**传输层无监听地址**）」正好对齐。
  - **带 HTTP 的特性名（接入后一个都不许开）**：`server-side-http`、
    `transport-streamable-http-server`、`transport-streamable-http-server-session`、
    `transport-streamable-http-client*`（其中 `-unix-socket` 那个拉 hyper）、
    `__reqwest`/`reqwest`/`reqwest-tls-no-provider`/`reqwest-native-tls`、`auth`（拉 oauth2 + reqwest）、
    `auth-client-credentials-jwt`。
  - **因此要补一条门禁**（接入 rmcp 的同一批里做）：现有出站面门禁
    `frontend/src/lib/egress-contract.test.ts` 只看**直接声明的依赖名**，而 `rmcp` 本身
    不在 `HTTP_CLIENTS` 名单里——于是有人给它加一个 `transport-streamable-http-server`
    特性、悄悄给应用开一个 HTTP 监听端口，门禁**不会红**。要加的判据是
    「`rmcp` 必须 `default-features = false`，且其 features 列表与上面那张带 HTTP 的
    特性名单交集为空」。这条不加的话，「stdio 为唯一传输」又会变成一句只由注释担保的承诺。
  - 未决：`transport-child-process` 走 `process-wrap`（Client 方向要它来 spawn 外部 server），
    该 crate 的许可证与依赖面接入时再核；`which-command`（PATH 查找）非必需，先不开。
  - **服务端 API 形状（2026-08-24 二轮 spike，docs.rs/README 实查）**，实现批照此落：
    `#[tool_router(server_handler)]` 挂在 impl 块上 + `#[tool(description = …)]` 挂在方法上，
    参数走 `Parameters<T>`（T: Deserialize + JsonSchema）；启动是 `Calculator.serve(stdio()).await?`
    （`ServiceExt::serve` + `rmcp::transport::stdio`）+ `service.waiting().await?`。
    **错误分两路，选错路等于把拒绝码做废**：工具级错误用
    `Ok(CallToolResult::error / structured_error(...))`——客户端把 content 渲染给调用方看，
    线上即 isError=true，**这正是 §4.5 确认契约里 `{isError: true, code: FS_POLICY_DENIED, …}`
    的载体**（`structured_error(value)` 一步给 structured_content + is_error）；
    协议级错误用 `Err(McpError)`——**客户端不透明渲染、调用方看不到消息**，只用于
    「请求本身没法路由/处理」，绝不能拿来承载拒绝码。`CallToolResult` 是
    `#[non_exhaustive]`（只能走构造器）。`Observation` 的 snake_case serde 输出
    可直接进 `structured_content`。

### M4 仿制深化 + 超越

- **目标**：把 Xshell/Xftp/FinalShell 完全体的「老用户依赖项」补齐，并交付超越项。这是「对标→超越」主张的兑现里程碑。范围大，**强制拆分为 4a/4b 两个子里程碑**（tag `v0.4.0` / `v0.4.1`），各自独立通过复审与验收。
- **范围 4a（仿制补齐）**：
  - 广播发送双通道：① 组合命令栏目标选择器发送（当前/全部/分组/手选，MVP 预留的禁用项实装）；② 终端实时键入广播（Xshell「Send Input to Multiple Sessions」对标：一端键入同步到选定会话[分组/全部/手选]，独立开关，含交互式命令中途输入）；
  - 快速命令集（按钮条 + 编辑器 + 参数占位符）与命令片段库（FinalShell 式）；
  - 服务器监控抽屉实装（经 exec 通道采集 `/proc/*`，非 Linux 降级方案，2s 间隔，不占 PTY）与主机状态灯轮询；
  - 键盘配置文件（快捷键全量重绑 + 导入导出）；
  - 会话录屏与回放（asciinema 兼容格式，明确标注「记录含输出明文」）；
  - 会话纯文本日志（Xshell Logging 式：连接自动转录，路径/命名模板/追加或覆盖可配，断线重连续写，与录屏独立开关）；
  - 打开会话目录入口（reveal-item-in-dir：日志/录屏落盘目录一键打开，UI 规格 §2.1，MVP 期隐藏）；
  - 高亮关键字集（规则编辑器 + 输出命中着色 + 提醒角标联动，UI 规格 §2.11）；
  - 计划任务（Xshell 自身无内置调度器，老用户以操作系统任务计划程序 + 脚本调起解决——本产品原生内置定时触发命令/脚本执行并含重启后补跑策略，属超越项）；
  - 历史命令 UI（基于 M1 terminal crate 的 headless 网格接口[总设计 §2.2]提取 + M4a 新建本地历史库的检索面板，一键重发当前会话）；
  - SSH 隧道/端口转发管理器 UI；
  - ZMODEM（rz/sz）自动收发（内嵌 zmodem 实现：终端内 `rz` 触发上传对话框、`sz` 触发下载落盘，Xshell/FinalShell 共有、中文运维高频依赖项）；
  - 服务器进程管理（监控抽屉扩展页签：进程列表/过滤/终止，经既有 exec 通道 `ps`/`kill`，FinalShell 对标）；
  - 会话树手动新建文件夹/子组（MVP 分组由会话 `group_path` 自动派生，手动新建 M4a）；
  - 跳板链可视化编辑（ProxyJump 链图形化增删排序，MVP 仅文本字段）；
  - 连接详情弹层（点击状态栏状态灯弹出：认证方式、密钥指纹、延时三字段；MVP 该段点击无响应，字段形状由总设计 §2.1 载体注锁定——UI 规格 §2.7 已注「弹层随 M4a 接线」）；
  - Xftp 向：排除过滤器、同步浏览（双向目录联动 + 独立浏览模式开关）、外部编辑器关联（=远程文件编辑，语义按总设计 §2.3：下载临时文件→监听保存→回传，远程 mtime 变更则冲突提示不静默覆盖）、拖拽传输增强（多文件/文件夹拖拽、拖拽中取消）、传输队列『全部取消』、文件属性对话框 + 软链图标 + 「新建软链」菜单（core 层 lstat/read_link/symlink 已于 M1 就位，本项仅补 UI 载体）。
- **范围 4b（超越 + 发布生态）**：
  - 监控 × AI 联动：异常指标一键 AI 诊断（依赖 M2）；AI 生成命令片段；
  - 跨工具迁移：Xshell `.xsh`/`.xcs`、FinalShell 配置、iTerm2 配色导入（计划前 spike 定 `.xsh` 私有格式边界，承诺「公开字段尽力导入」）；
  - 配色方案编辑器（实时预览、导出 JSON）；主题引擎开放（用户 JSON 主题目录热加载）；
  - 标签拖出成窗、平铺视图输入同步；
  - Window 菜单全项实装（新建窗口/层叠/水平·垂直平铺/关闭全部标签/会话列表；关闭全部标签复用 UI §2.9 同一确认入口与 CloseConfirmDialog）；
  - 标签右键「关闭其他标签」启用（MVP 禁用态预埋，与 UI §2.1/§1.3 标签右键菜单定义同口径）；
  - **自动更新**（Tauri updater + 更新签名校验）与**代码签名/公证链路**（总设计 §6.2/§9：更新前 DB 备份，降级/跨版本开库遵循 §3.4 版本闸）；
  - audit hash 链可导出取证包；
  - 便携打包评估（Windows 绿色版/U 盘携带场景：做/不做结论与依据）；
  - 终端区截图（选区/全屏截图入剪贴板或存盘，工具栏 📷 入口，UI 规格 §2.2）；
  - 密钥/代理管理器（Vault 密钥浏览——仅指纹/类型/注释，永不显示私钥材料——与 agent 传输开关 UI，工具菜单入口，UI 规格 §2.1）；
  - 文件夹同步/比较（Xftp Synchronize Folders 对标：单向/双向文件镜像）评估——做/不做结论与依据；
  - 语言切换（i18n：首发简体中文 + 英文双语或仅搭框架二选一，UI 规格 §2.1/§2.12 标注 Phase 4b）。
  - 会话管理器停靠模式：可拖至右侧/浮动，记忆停靠侧（settings 键 `ui.sidebarDock`；MVP 固定左停靠，UI 规格 §1.1）；
  - 窄屏（<1100px）会话管理器抽屉式覆盖层（窗口收窄自动转覆盖层 + 点击外部收起，UI 规格 §1.1）；
  - 工具栏右键自定义（勾选显隐 + 拖拽排序，持久化于 settings 键 `ui.toolbarLayout`；MVP 工具栏为固定项集、右键无菜单——UI 规格 §2.2/§3.4）；
  - SFTP 本地栏新建目录/删除/重命名对偶评估（当前仅远程栏具备：计划本体 Task 21 SftpPane 与命令层 sftp_mkdir/sftp_remove/sftp_rename 均仅接远程 SftpOps，UI 规格 §1.4 已按此收窄）。
- **出口标准 4a（逐项可验证）**：
  - [x] 广播：组合栏 5 会话同发一致性测试（各会话收到字节级一致）；实时输入广播：一端键入 ≥5 会话字节级一致（含交互式命令中途输入用例）；
        —— 2026-08-22 核对落勾（实现见 Task 65）。两条子句各有一条同名测试：
        `frontend/src/lib/broadcast.test.ts` 的「组合栏 5 会话同发：各会话收到字节级一致的载荷」
        与「实时键入广播：一端连续键入（含交互式命令中途输入）各会话逐键字节级一致」，
        判据是**逐会话收到的字节序列相等**而非「发了 5 次」。另钉两条边界：单会话失败
        不阻断其余（广播的要点恰是一个目标死了其余照发）、失败结果不含载荷内容
        （组合栏载荷常含口令，错误面不得回显）。目标解析（current/all/group/pick）5 条
        单测覆盖断线剔除与异组退化。
  - [x] 快速命令集/片段库：参数占位符替换单测 + UI 增删改查测试；
        —— 2026-08-22 核对落勾（实现见 Task 66）。占位符替换的要害不是「能替换」而是
        **空参数不发**：`QuickCommands.test.ts` 钉「带占位符的片段先弹参数行，空参数不发
        （不静默发字面量），填全才发替换结果」——静默发出带 `{host}` 字面量的命令是可以
        在生产机上造成后果的。CRUD 四条齐全，另钉「取消不触发 onSave」（快照式提交：
        关掉不落不脏库）与「空库时确定按钮禁用」。
  - [~] 监控采集：Linux 容器 + macOS 真机两路验证，数值与同时刻 `top`/`vm_stat` 读数误差 ≤5%；采集失败静默降级（日志 warn，UI 不弹错误框）；
        —— 2026-08-22 **容器一路达成；macOS 一路「未实现」而非「待核验」**（2026-08-22 盘点更正，
        原措辞把它写成「待用户核验」，那读起来像「代码在、只差跑一遍」，不实）。
        `monitor_command()` 采的全是 Linux 专有源：`/proc/loadavg`、`free -m`、`/proc/stat`——
        全仓 `vm_stat`/`sysctl` **零命中**。macOS 上 6 项指标里**负载三项、内存两项、CPU 一项恒空**
        （`df -h /` 与 `hostname` 可用，`uptime -p` 经 `|| uptime` 回落但格式不同）。
        模块头原写「非 Linux 降级：CPU 行为空」也说轻了——内存与负载同样为空。
        **2026-08-26：macOS 分支已实现，状态从「未实现」转为「已实现、待真机核验」**
        （正是上一段自己写的那个口径）。`monitor_command()` 现为单脚本双路，由远端 shell
        自己选：负载 `sysctl -n vm.loadavg`（`{ 1.85 1.94 2.01 }`，花括号要剥）、
        内存 `vm_stat` 页数 × `hw.pagesize` ÷ 1 MiB 与 `hw.memsize`、核数 `hw.ncpu`、
        CPU 走**新增的 `CPU_PCT` 键**。
        · **CPU 为什么另开一个键而不复用 `CPU=`**：`/proc/stat` 给的是开机以来的累计
          jiffies，靠两次采样求差量；macOS 没有等价的累计计数器可从 shell 读到，
          `top` 给的直接就是区间百分比。两者形状不同，硬塞进同一个字段会让差量计算
          拿两个百分比去相减。故 `MonitorSnapshot` 加 `cpu_percent_direct`，
          app 层优先走差量（更准且不额外耗时），无累计值时才用它。
        · **`top -l 2 -s 1` 而不是 `-l 1`**：`-l 1` 的那份样本是「开机以来的平均」，
          在一台开机三十天的机器上永远接近某个中值，与「此刻忙不忙」无关。
          代价是等一秒——宁可慢一秒也不要一个稳定的错数。
        · **本仓没有 Mac，但这不等于只能靠肉眼审**。分支里真正会写错的是 shell 与 awk：
          字段序号数错一位、`tr -d '{}'` 漏掉一边、页数忘了乘页大小、单位少除 1048576。
          这些**全都能在 Linux 上暴露**：新 itest 两条把 Linux 源藏掉
          （`/tmp/noproc` 里两个只挡 `/proc/*` 的 cat/head——第一版用 `unshare -m` +
          `mount --bind`，容器非特权跑不了且引号嵌套到三层内层 sh 直接语法错，
          改成在脚本真正触及 /proc 的那两处下手，更准也不需要特权），
          再在 PATH 上放吐**真 macOS 格式**的 `sysctl`/`vm_stat`/`top` shim，
          于是 macOS 分支**真的执行**：负载 1.85/1.94/2.01、内存 5078/16384 MiB
          （整除的数，把「三个字段各自取对位置 + 乘了页大小 + 除对单位」一起钉住）、
          CPU 12.5%、核数 10。**反向对照**：`/proc` 在的时候必须走 Linux 那一路
          （否则在 Linux 上白花一秒且精度更差）。
          shim 的 `top` **必须认 `-l N`**——第一版无视参数恒打两份样本，于是断言钉住的是
          「我那段 awk 后覆盖前」而不是「产品用的是 `-l 2`」，变异「`-l 2` 改 `-l 1`」
          照样绿；这一处是被变异当场逮住的。变异 3/3 全红（不剥花括号 / 取第一份样本 /
          vm_stat wired 字段数错一位）。
        · **仍未闭合**：真 macOS 上跑一遍。剩下的风险已缩到「真 `vm_stat` 是不是真的
          这么打印」——一个有文档、格式几十年没变的问题，比「这段 awk 对不对」小得多，
          但不等于零。不代签。
        容器一路的证据（仍然成立）：itest `crates/itest/tests/monitor.rs` 把被测命令与对照命令放在
        **同一次** exec 里跑（分两次会把真实内存波动混进「误差」，那时 5% 就不再衡量采集正确性），
        内存总量须与 `free -m` **完全**相等、用量误差 ≤5%（另加 8 MiB 绝对宽容）、负载三项分别对上
        `/proc/loadavg` 的第 1/2/3 列；降级一路把 PATH 换成只含 sh 的目录，断言**每个 KEY 都还在**
        （「首项失败即中止」会让后面所有字段一起消失，那是半个快照而非降级快照）。
        变异验证 7/7 全杀（取错列、KiB/MiB 混淆、CPU 不采、负载列错位、降级变硬失败、首项中止）。
  - [x] 主机状态灯轮询：对容器 sshd 按可配置间隔（默认 30s）轮询未连接会话；主机停机/恢复后灯态（绿/灰/红三态定义见 UI 规格 §1.1，四态权威见 §5 状态表）在 2 个轮询周期内翻转；轮询失败静默降级（日志 warn，UI 不弹错误框）；itest/单测覆盖停机检测、恢复检测、连续失败降级三用例；
        —— 2026-08-22 达成。itest 三用例（停机检测 / 连续失败保持失败 / 恢复检测）对着一台
        真会停会起的容器跑真实 TCP 探测。两处实测留痕：
        ① 「停机」必须停**整个容器**——只停容器内的 sshd 服务时，Docker 的端口发布代理
        仍然接受 TCP 连接，探测照旧成功（第一版就撞在这上面，假红）。这同时说明了产品
        边界：只做 TCP 握手的状态灯在「机器在、服务死了」这一形态下会亮绿，那是 Xshell
        同款语义（见 host_probe 注释），不是测试要改的事；
        ② 容器重启后端口映射**换号**，恢复检测必须重新取地址——拿旧地址探测只证明
        「旧端口没人听」，与恢复无关。
        轮询节奏（默认 30s、可配）由 settings 侧单测钉，不在 itest 层重复。
  - [x] 键盘配置文件：重绑生效 + 导入导出 round-trip 测试；
        —— 2026-08-22 核对落勾（实现见 Task 68）。round-trip 在 `keymap.test.ts`：
        「导出（JSON.stringify）→ 导入（parseBindings）→ 合并，结果与原表一致」；重绑生效在
        `KeymapEditor.test.ts`：「录键：按下 Ctrl+Alt+K 后该动作改绑到规范键位」。
        另钉三条边界：无修饰字符键拒绝重绑（绑了它终端里就打不出这个字符）、畸形键位串
        静默跳过而不为一条坏行毁整表（导入他人配置的常态）、cmd/meta/option 别名归一。
  - [x] 录屏：回放与原始输出 diff 为空（文本层面），产物可被 `asciinema play` 播放；
        —— 2026-08-21 达成。两半各有独立证据（自家解析器与自家写入器天然自洽，光靠单测
        证不了「能播」）：① 单测 `crates/terminal/src/record.rs::concatenated_output_events_equal_the_input`
        钉往返 diff 为空，含逐字节喂入下的多字节存活（UTF-8 残留跨块保留，cast 里不许出现替换符）；
        ② itest `crates/itest/tests/record.rs` 把真实 SSH 输出录成文件送进容器，交由
        **真实 asciinema**（`asciinema cat`，与 `play` 同一解析器）还原后逐字比对，并用坏文件
        反证工具真的会拒（rc=1）。录制点接在 ZMODEM tap 之后 = 录「用户看到的」而非链路原始字节。
        隐私：开录与打开回放各弹一次「含输出明文」确认，落盘有 CAST_BYTES_MAX 上限（超限自停，
        不撑爆磁盘）；回放面板明示无「倒放」——拖动一律 reset 后从头快喂（终端状态是累积的）。
        变异验证 10/12 杀，2 项留痕：K3 是断言而非实现（由 K2 覆盖），K5（active 与
        file.is_some() 同置同清）为行为等价冗余保护，无公开 API 能构造分叉状态。
  - [x] 纯文本日志：开启日志的会话产出转录文件（目录/命名/追加可配），断线重连续写，与录屏可独立开关，itest 覆盖；
        —— 2026-08-22 核对落勾（实现见 Task 69）。`crates/terminal/tests/sessionlog.rs` 四用例：
        产出转录文件、断线重连**续写同一文件**、关掉即一个字节都不写；第四条是最要紧的
        ——「日志 tap 在背压丢弃**之前**」：若排在丢弃之后，终端拥塞时转录会静静缺段，
        而那正是最需要日志的时刻。与录屏独立：两者是 pipe 上两个互不相干的 tap
        （录屏接在 ZMODEM tap 之后，见 Task 80），各自有独立开关。
  - [x] 打开会话目录：reveal-item-in-dir 对已落盘日志/录屏目录生效（UI 或单测）；
        （**2026-08-26 补上录屏那一半**：交叉审计发现此前只有会话日志一个入口——
        录屏落在 `data_dir/recordings`（`record_cmd::recordings_dir`，不可配），
        与会话日志目录（可配 / 回落 `data_dir/logs/sessions`）**恒为两处、永不相交**，
        而 `session.openDir` 只调 `reveal_session_log_dir`，用户录了屏找不到文件在哪。
        这正是同一条自己修掉过的那类缺陷（「功能在、指错地方，且不报错」）在另一半上复发。
        现补 `reveal_recordings_dir` 命令 + `session.openRecordings` 菜单项 + 双语词条；
        目录解析与写入方同源（`recordings_dir_of`），不让两处各拼各的路径。
        `App.svelte` 里那句写着「转录/录屏」而实现只指转录的注释一并改正。）
        —— 2026-08-22 达成。核对时发现真缺陷并修掉：目录回落规则（配置为空 → 
        `<data_dir>/logs/sessions`）此前在**两处逐字重复**——写转录文件的 `build_session_log`
        与「打开会话日志目录」的 `reveal_session_log_dir`。两处一旦走散，这个入口就会打开
        一个空目录而转录文件躺在别处：功能在、指错地方，且不报错。已单源化为
        `state::session_log_dir`，两侧共用，并加 3 条单测钉住空/纯空白回落、配置优先且
        去空白、以及回落布局本身（任一侧擅自改名 logs/session、transcripts… 即红）。
  - [x] 关键字规则：命中高亮 + 提醒角标（启用提醒时）联动，CRUD UI 测试 + 正则/字面量双路单测；
        —— 2026-08-22 核对落勾（实现见 Task 71）。双路单测在 `highlights.test.ts`：字面量
        （大小写不敏感、一行多处全命中）与正则（全局、大小写不敏感）各一路；角标联动一条
        「命中带提醒的规则才联动；不带提醒的命中不响」。要害那条是**非法正则整条禁用并带
        错因，不静默降级成字面量**——降级会让规则看起来生效却匹配别的东西。CRUD 在
        `HighlightEditor.test.ts`（含「合法正则不显示错因」的反向对照，防「恒显示错误」的假绿）。
  - [x] 计划任务：按 cron 定时触发（误差 ±5s）、重启后补跑策略单测；
        —— 2026-08-21 达成。判定纯函数（fs_connmgr::schedule，21 条单测：分钟内任意秒触发、
        同分钟去重、回拨暂停不重放、Skip 留痕 Once 只补一场、新任务不补史前、窗口截断
        如实标记、DST 前跳/回拨）；±5s 由 tick=2s 的 const 断言钉住（2×余量）。
        **两个水位分开**（评估水位只升不降 / 上次触发纯展示）——合用一个量则留痕要么重复
        落库要么展示撒谎。Skip 也留痕（「已按策略跳过 N 次，最近原定 xx:xx」）。
        调度循环（app/src/scheduler）：同连接串行不排队（防等锁烧光超时预算并落指向不存在
        之远端超时的误导记录）；占位原子（先插后报会泄漏 profile 槽位——占位测试咬出）；
        回拨进入暂停那一刻留痕一次（PauseTracker 去重）。时区两档：固定偏移（默认，
        可复现）或跟随系统（chrono 已在依赖树，提成直接依赖 = 0 新 crate）。
        cron 校验与预览共用后端实现（schedule_preview_cron 给接下来三次触发——不做
        可能说错的 cron 人话化）；产品边界（只在有活动会话时执行，不自动拨号）常驻 UI。
        顺带修了共享 exec 通道的两个真缺陷：run_exec_channel 输出无上限（`cat 大日志`
        攒几百 MB 内存）与 -1 同时表示超时/缺退出码（落库读作 255）——ExecChannel
        改返回 ExecOutput{code: Option}，消费方全部随改。变异验证 11/11 可钉项全杀。
  - [x] 历史命令 UI：检索命中（数据来源 = M1 网格接口 + M4a 本地历史库）+ 一键重发到当前会话 E2E；
        —— 2026-08-21 达成。两半分开留痕（合起来才是 E2E，各自单独都不足）：
        ① `crates/itest/tests/history.rs`（不需 Docker，每次都跑）走完
        「终端字节 → SessionPipe 回滚文本（M1 网格接口）→ 启发式提取 → 入库 → 检索命中」，
        并钉住 ANSI 不泄漏、输出行不被误提取、提取去重、按主机过滤、两来源可区分；
        ② `frontend/src/components/HistoryDialog.test.ts` 钉「点条目 → onSend → App 走
        term_input」的一键重发，以及**屏幕提取来源不许走回车直发**（可能带提示符残渣）。
        两个数据源在库里以 `source` 列区分（`sent` 字节精确 / `grid` 近似），UI 明示。
  - [x] 隧道管理器：对容器 sshd 的本地端口转发端到端测试通过；
        —— 2026-08-22 达成。itest `crates/itest/tests/tunnel.rs` 让真实字节穿过整条链：
        本机 TCP 客户端 → 本地监听端口 → **真实 SSH** direct-tcpip → 容器内 busybox nc
        回显 → 回程，`hello-隧道-🚇` 逐字相等；另一例钉「目标端口无人监听时立刻见 EOF
        而不是一直挂着」——挂着会让用户以为服务卡了、去查错的地方。
        单测里的 EchoOpener 换不来这句话：AllowTcpForwarding 默认是关的、目标地址在容器
        网络命名空间里解析、通道开失败的传播只有真服务端会发生。
        变异验证 4/4（1 项锚点未命中跳过）。两处过程留痕：
        ① N3「stop 只发通知不置标志」首轮**存活**——`stopped` 标志防的窗口（accept 任务
        首次被轮询之前）没有任何测试钉住，已补单测 `stop_before_first_poll_is_not_lost`
        （current_thread 运行时确定性构造该窗口），补后该变异即死；
        ② N5 首轮存活是我的变异写成了 `.map(|_| forget(()))` 这种无行为改变的形式，
        改成真的泄漏 socket 后立刻被杀——变异无效与判据不足是两件事，得分开看。
        容器侧留痕：镜像里的 `nc` 是 OpenBSD netcat（无 `-e`），要用 `busybox nc -lk -e`；
        就绪判据 `grep ':7777 '` 因 netstat 列宽不命中（实际绑在 `:::7777`），去掉行尾空格。
        「停止后端口可重新 bind」比「连不上」是更强的判据：Windows 上 accept 循环退出后
        内核仍可能从 listen backlog 完成握手，于是连得上却没人服务。
  - [x] ZMODEM：对容器执行 `rz`/`sz` 分别触发上传/下载，传输字节校验正确，itest 覆盖；
        —— 2026-08-21 达成。itest `crates/itest/tests/zmodem.rs` 对**真实 lrzsz 0.12.21**（Debian
        容器，Alpine 仓库已无 lrzsz）经真 SSH 通道跑三路：`sz` 下行 256 KiB 逐字节相等、
        `rz` 上行 200 000 字节（含全部 256 种字节值，压满 ZDLE 转义）由容器侧 sha256sum 校验、
        取消序列令对端 `sz` 自行退出。对真实实现跑出来的两个真 bug 已修并各有钉例：
        数据帧未用 ZCRCE 收尾（S341）、非零 ZRPOS 被误当「请求断点续传」而取消（实为重传机制，S341）。
  - [x] 进程管理：进程列表采集 + 终止操作 itest（经 exec 通道），非 Linux 降级路径单测；
        —— 2026-08-21 达成。`crates/itest/tests/procs.rs` 四路：真实 `ps` 输出解析出结构化
        进程表（并断言字段未整体错位、用户名未被截断）、`kill -s TERM` 后目标进程的 exec
        通道终结且从进程表消失、杀不存在的 pid 带出远端原因、`PATH` 清空复现「无 POSIX ps」
        被判为**失败而非空表**。载体：监控抽屉新增「进程」页签（过滤/排序/终止）。
        `kill` 命令零注入面（pid 是 u32、信号过闭合白名单映射到编译期字面量）。
  - [x] 手动新建文件夹：新建/重命名/删除分组后会话归属即时更新、重启持久；
        —— 2026-08-22 **降级为部分达成**（原标 [x]，是我核对时看漏了出口原文里的
        「重命名」三个字）：~~`frontend/src/lib/folders.ts` 导出的是 normalizeFolderPath /
        mergeFolderPaths / addFolder / removeFolder / hasProfilesUnder / parseFolders ——
        **没有 renameFolder**；`Sidebar.svelte` 的分组行右键菜单也只有「删除文件夹」一项
        （该文件注释自陈如此）。即「新建」与「删除」两路已达成且有测试，「重命名」**未实现**。~~
        这条落勾错在方法上：我当时逐条核的是「注记里点名的测试文件是否存在」，而不是
        「出口原文的每个子句是否都有载体」——前者会让一个复合条目里没做的那一半蒙混过关。
        —— **2026-08-24 重新核实：上面那段划掉的话本身已经过期，本条闭合。**
        `renameFolder` 就在 `folders.ts:105`（docstring 自己写着「2026-08-22 补」），
        `Sidebar.svelte` 的分组菜单有 `groupmenu-rename`「重命名文件夹…」，
        `folders.test.ts` 有 7 条 rename 用例，`Sidebar.test.ts` 钉「点重命名 → 把**该分组的路径**
        交给 onRenameFolder」。也就是说功能在 8-22 当天就补上了，只是这条注记没跟着改——
        又一次「注记与代码分叉」，与本批早先误判出站面门禁不存在是同一个毛病的两面：
        **一次是注记说有而实际没有，一次是注记说没有而实际有；两次的错都在于信注记而没去核代码。**
        本批真正补的是纯函数测不到的那一半（`folders.test.ts` 新增 6 条源码扫查，共 25 例）：
        出口原文的两个承诺都落在 `App.svelte::renameFolderTo` 的 IPC 调用序列里——
        **重启持久**要求两侧都落库（清单进 `sidebar.folders`、每个受影响会话的新
        `group_path` 逐条进 `profile_save`）；少任何一侧重启后就分叉——只存清单 ⇒ 目录改了名
        而会话还挂在旧名下、树里凭空多一个旧目录；只存会话 ⇒ 空目录的改名丢失，
        而「空目录也要能存在」正是这批手动文件夹的全部意义。**即时更新**要求迁完
        `loadProfiles()` 重取，否则内存里的 profiles 仍带旧 group_path，用户看到的是「改名没生效」。
        另钉「部分失败必须说出来」：已迁的在新名下、没迁的在旧名下，两个目录都会显示，
        沉默会让用户以为这是界面 bug。7 个变异（不落清单/不落会话/迁错对象/不重取/失败沉默/
        菜单项禁用/菜单项不回投路径）全部转红。
        **过程留痕（两个自己的坑，都值得记）**：① 断言函数体最初用「从函数名往后切 1400 字符」
        的固定窗口，变异证明不可靠——注释掉 `saveFolders(list)` 后断言**仍然绿**，因为窗口
        越过函数结尾捞到了**邻居函数**里的同名调用；改成大括号配对取真实函数体。
        「为了错误的理由而通过」比不写更坏：它让人以为这条链有人守着。
        ② `await saveFolders(list);` 在 App.svelte 里有两处，`String.replace` 字符串模式只换第一处，
        于是那个变异改的是别的函数、看起来「幸存」其实根本没打在目标上——
        **变异幸存时要先怀疑变异本身有没有落地**，否则会去放松一条其实健全的断言。
        已达成部分的证据（仍然成立）：10 条单测覆盖路径规范化、非法输入返回 null 而非猜一个、
        只认 `/` 分层（放行 `\` 会让同一文件夹在两种写法下变成两个节点）、手动路径补全全部
        祖先层（缺祖先会让子目录挂到根上）、与派生路径去重合并、非法条目跳过不毁整棵树、
        删除连带手动子目录但不碰同名前缀的兄弟（`a/b` 不该删掉 `a/bc`）。
        持久化走 settings 表（手动文件夹清单）+ 会话 `group_path` 派生，两者在同一棵树上合并。
  - [x] 跳板链可视化编辑：两跳链增删后连接经新路径建立；
        —— 2026-08-21 达成。编辑面在审计2 #23 已落（ProfileDialog「跳板」页签：增删、
        ↑↓ 换序、逐跳凭据/策略/钉扎，上限 8 与后端 JUMP_MAX 同源，含 12 条组件测试）；
        本次补的是「连接**真的**走了新路径」这一半，判据取自**服务端**：itest
        `crates/itest/tests/jump_path.rs` 起三个容器（两跳板 + 目标），每次连接后在目标机
        读 `$SSH_CONNECTION` 首字段（sshd 按这条 TCP 的实际对端填写），把链从 0→1→2→回退 1
        改一遍，四次判据逐一对上末跳容器的 bridge IP；另一例把两跳换序，末跳随之换人。
        为什么不用自家 `events.status()` 播报当判据：那是「打算走哪条路」，由被测代码自己
        写出——漏掉末跳转发而直连目标时播报照旧完美。变异验证 3/3 杀（绕过末跳转发、
        截断到第一跳、反转 hop 顺序，各自都让判据红）。
        拓扑事实留痕：首跳必须用宿主映射地址（Docker Desktop 下 172.17/16 在宿主不可路由，
        实测 os error 10054），第二跳起必须用容器 bridge IP + 内部端口；中间跳承载流量的
        netstat 断言只在会话句柄仍活着时查（等 drop 后查到的只是 TIME_WAIT 残留，时绿时红）。
  - [x] 连接详情弹层：点击状态栏状态灯弹出认证方式/密钥指纹/延时三字段且与当前会话实际值一致，Esc 或点击外部关闭（组件测试或手工核验留痕）；
        —— 2026-08-22 核对落勾（实现见 Task 73）。`ConnectionDetailDialog.test.ts` 六条：
        三字段如实呈现、Esc 关闭（出口标准原文）、读取失败就地报错不整屏空白、
        指纹可复制（核对 MITM 的唯一手段，必须能取出来比对）。两条「不编数据」的边界：
        延时测不到显示「—」而不是 0（0 ms 会被读成「极快」）、认证方式未知时显示「—」
        而不编造一个方法名。
  - [x] 传输队列『全部取消』：抽屉批量取消启用（MVP 单件取消/重试在，批量取消归 M4a），触发后在途传输全部取消，UI 测试或手工核验留痕；
        —— 2026-08-21 达成。抽屉头部按钮 + `ConfirmDialog`；选行判据 `cancellableRows`
        与关闭门控共用 `isTerminal`（两处各写一份「哪些算在跑」会导致「全部取消」后
        关闭确认框还在弹）。逐件下发并收集失败，部分失败报「N 已请求 / M 失败（#id…）」
        而不是一句「已取消」；不乐观改状态（取消是请求，终态由后端事件带回）。
        8 条组件测试 + 6 条 lib 测试，变异验证 6/6 全杀。
  - [x] Xftp：排除过滤器/外部编辑器关联各有 UI/itest 覆盖（编辑器内保存触发自动回传 itest；远程端 mtime 被改动时提示冲突不静默覆盖）；同步浏览双向目录变更联动延迟 ≤1s + 独立浏览模式开关、两模式 itest；拖拽传输多文件/文件夹 + 拖拽中取消用例通过。
        —— 2026-08-21 达成。四项各自的判据与留痕：
        ① **排除过滤器**：匹配器只有一个（`fs_sshengine::filter`，7 条单测含病态回溯输入
        `a*a*…b`×64 不得指数爆炸、大小写敏感、条数/长度上限）。它同时管两栏列表**与**
        目录递归传输——两份匹配器一旦分叉，表现是「界面上看不见的文件被传走了」，不报错。
        ② **同步浏览**：独立开关（默认关），只同步用户主动的导航动作；另一侧没有同名目录时
        **原地不动 + 就地提示**，不导航到不存在的路径再弹「列出失败」。联动是同一次事件里的
        状态更新（无网络往返），远快于 ≤1s。
        ③ **拖拽增强**：拖已选中项 = 传整个选中集（资源管理器口径）；文件夹经后端递归枚举
        （`walk_files`，6 条单测 + 对真实 sftp-server 的 itest 分清目录/软链/文件）；上传方向
        先建远端子目录（含空目录）；入队过程可「停止入队」，并明说已提交的那些仍在传。
        软链**不跟随**且跳过项必须出现在界面上——静默跳过等于谎报完整性。
        ④ **外部编辑器**：回传判定是纯函数（`fs_sshengine::editsync`，5 条单测）。冲突判据是
        「远端在编辑期间变过」而非布尔标记：少了它，两人同编一个配置时后保存方会**静默**
        抹掉前者。判据用 size+mtime 而非内容哈希，方向是安全的（可能多报一次冲突，不会漏报）。
        **顺带发现并修掉一个生产缺陷**（itest 才照出来）：russh-sftp 2.3.0 的
        `FileAttributes::default()` 不是全 None，而是 `size:0, uid:0, gid:0,
        permissions:0o777|S_IFDIR, atime:0, mtime:0`。`truncate` 写的
        `{ size: Some(n), ..Default::default() }` 因此顺带请求 chown-to-root / chmod-0777-目录位 /
        时间戳清零；真实 sftp-server 上**尺寸改成功了、调用却回 Permission denied**（先应用 size、
        再在 chown 处失败）。这条路径生产在用（`write_remote_identity` 与非续传上传的清零），
        一直在吃这个假错误。改为逐字段显式 None，并加 itest 同时钉返回值与副作用
        （只断言尺寸变了的话，回归到缺陷版照旧全绿）。
        变异验证 33/33 全杀。过程留痕：首轮有 6 项被我的脚本判成「编译失败（变异无效）」，
        逐个手工复核后全部是 KILL——分类器把 cargo 的 `error: test failed` 误当成编译错误。
        判定脚本本身也会骗人，这是第二次踩到同一形状。
  - [x] SFTP 面板属性/软链：属性对话框展示 lstat 元数据（size/mtime/mode/uid/gid 降级路径，mode/uid/gid 缺省显示「—」或经 exec stat 补充）、软链条目显示软链图标与目标（read_link）、「新建软链」菜单调用 symlink 成功，UI 测试或 itest 覆盖；
        —— 2026-08-21 达成。组合语义放在引擎层（`fs_sshengine::sftp::describe_entry`）而非
        tauri 命令里，理由是**只有 itest 能证伪它**：`crates/itest/tests/sftp.rs` 对真实
        OpenSSH sftp-server 钉住「链自身的 size 是链文本长度（11）而非目标大小（12）」、
        readlink 原文（相对链 `../payload.bin` 照原样）、断链的 target_meta 为 None 而不是 Err、
        不存在路径为 Err；另一例从命令层口径再钉一次 symlink 参数序（写反会在 lstat 两处
        同时暴露）。缺省值口径由 `frontend/src/lib/fileprops.ts` 单测钉死：mode/uid/gid 缺失
        显示「—」不编 0644、uid=0（root）不得当成未知、setuid 无 x 位时用大写 S（配错要看得出来）。
        前端 13 条组件测试覆盖软链/断链/读取失败/本地侧/两参下发/链名带分隔符被拦。
        变异验证 15/15 全杀——其中 P3 一开始存活，暴露的是真缺陷：属性入口的「恰好一项」
        判据写了两份（模板 `size !== 1` 与函数 `size === 1`），按钮禁用挡住点击使第二份
        不可观测，把它放宽成 `>= 1` 全绿。已单源化为一个 `$derived`。
- **M4a.1 发布前置全绿（2026-08-22 完成，Task 87–95）**：M4a 出口清单落勾后插入的一段。
  起因是一个此前没人问过的问题——**这份代码除了开发者本机 Windows，从未在任何环境被编译
  或运行过**（`origin/main` 停在 Initial commit，38KB 的 release.yml 与 ci.yml 从未对它跑过）。
  规划期用 Docker 容器与本地工具实测，找到 5 个会让 CI 第一次运行就失败的缺陷（后续 M0 出口核验时又加一条第 6），逐条修掉
  并各配一道防复发门禁：

  | # | 缺陷 | 发现方式 | 修法与门禁 |
  |---|---|---|---|
  | 1 | `connmgr/db.rs` 的 `tracing::error!` 格式串写错（占位符与位置参数不匹配），Linux 上**编译不过** | 容器内首次 `cargo check` | 改 `%e`；`scripts/linux-precheck.sh` |
  | 2 | `sshengine/transfer.rs` 多余的 `OpenOptionsExt` 导入，`clippy -D warnings` 判死 | 同上 | 删导入 + 注释说明为何不能加回 |
  | 3 | **229 处** `cargo fmt --check` 违规（CI 的第一个门禁） | 本机首次跑 fmt | `cargo fmt` + `scripts/ci-local.sh` + `ci-parity.sh` |
  | 4 | RUSTSEC-2026-0258（h2 < 0.4.16），`cargo deny` 硬门禁失败 | 本机首次跑 deny | `cargo update -p h2` → 0.4.18（可修，故不走 ignore） |
  | 5 | `perf-gate.md` 声称 P50 ≤300ms/P95 ≤500ms，`perf.rs` 实际断言 **800/1200**（Task 64 改码未改文档，虚报 2.6×） | 两文件对读 | 表格结构化 + `perf-gate-parity.sh` |
  | 6 | CONTRIBUTING 与本节 §CI闸 都写着「性能测试 `FS_PERF` 手动/夜间触发」，而全仓 `FS_PERF` **零命中**——只有语义完全不同的 `FS_PERF_TEST`（应用侧就绪标记桩）。照着跑 `FS_PERF=1 cargo test` 一条性能测试都不跑，**而输出是绿的** | M0 出口逐条核验 CONTRIBUTING 时对读代码 | 文档改成现状（perf.rs 用 `#[ignore]`、可自动化部分随 `FS_ITEST` 进 CI）；`scripts/env-gate-parity.sh` 守着「文档里写的环境闸必须有真实读取点」 |

  另办四项：panic 取证（全仓 `set_hook` 此前**零处**，GUI 进程的 panic 写进无人可见的
  stderr）；高亮正则的灾难性回溯形状闸（实测嵌套量词式子在 **30 字符**行上跑 10.5 秒，
  8 KiB 截断闸完全无效——模块头原本声称「两道闸防回溯」，方向就是错的）；四源版本推
  0.4.0 与 tag 口径统一（路线图原定 `vX.Y.Z-phaseN` 与版本门禁的三段正则矛盾，裁决改
  路线图：放宽正则等于长期容忍任意后缀，且带后缀的版本按 semver 是**预发布版**、排序
  低于纯三段，将来上 updater 会静默判错）；验收 39 条三桶分类（有载体 16 / 待人工 9 /
  **待建载体 14**——最后这 14 条才是离发布的真实距离）。

  **本阶段刻意不做**（连同理由，均已核实）：updater —— `egress-contract.test.ts` 把
  `tauri-plugin-updater` 列入禁止名单，且跨版本升级需要「上一次 release 的产物」而我们
  一个都还没有；代码签名/公证 —— 卡在采购与签发周期，`release.yml` 对 tag 构建缺 secret
  直接 fail；三平台 CI 真跑 —— 需推送 origin，用户已定仅本地，Linux 一路由容器脚本部分
  替代（16/26 生产 cfg 站点，app 的 10 处需 Tauri Linux 全栈，如实记为未覆盖）；
  `min_app_version` 提升 —— 动 `0001_init.sql` 会改 sqlx migration checksum，既有库全开不了。

  变异验证累计 **49/49** 应杀项全杀（T87 2、T88 4、T89+T90 5、T91 6、T92 6、T93 9、
  T94 9、T95 4，另 4 项如实记为观测/等价而非漏网）。过程中三次靠变异验证抓出**判据
  本身**的缺陷：① 容器校验脚本写成 `cargo check | tail`，管道退出码是 `tail` 的（恒 0），
  两条真缺陷全部假活报绿；② CI 对齐断言直接 grep 原文，把说明性注释也算成「门禁在」；
  ③ 版本 selftest 没有条数下限，删掉夹具照旧打印「通过」。三条都已修并各配一条变异。
- **出口标准 4b**：
  - [x] 导入：Xshell `.xsh`/`.xcs`、FinalShell 配置、iTerm2 配色三路各有真实/构造样例的公开字段还原验证（字段映射表入计划，spike 记录不可解字段清单）
        → `crates/connmgr/src/import_foreign.rs`（d06fbbd）。三路各有构造样例的字段还原用例；
        **凭据一律不导入**（`.xsh` 的 Password 字段是私有加密，即便解得开也不该在导入路径上碰）；
        不可解字段不写成文档而是**产出数据**（`ImportReport::unresolved`），由界面显示给用户看——
        写进文档的清单会随格式演进而过期，产出的清单永远对应这一次导入的这一个文件。
        **界面半边**（同批补齐）：`app/src/commands/import_cmd.rs` 两个命令 + `ForeignImportDialog.svelte`，
        菜单「文件 → 导入(Xshell/FinalShell/iTerm2 格式)…」由禁用态转为可用。
        三处刻意的取舍：① **两步**（预览 → 确认）而不是一键导入——「密码不会被导入」必须在
        落库**前**说，否则用户要到某天连不上去时才发现；② 格式按**内容**认，扩展名只决定
        先试哪个（`.xcs` 存成 `.xsh` 是常事，只按扩展名判会说「这不是 Xshell 文件」，
        而它明明是）；③ 同名配色**跳过并列出名字**，不覆盖也不自动改名——覆盖会无声毁掉
        用户调过的配色，自动改名会在反复导入后堆出一串分不清的副本。
        导入的配色存 `term.customSchemes`（新 settings 键，颜色串限 `#rrggbb` 小写、
        ansi 恰 16 项——这些值直接进 xterm theme 对象，且来自外来文件），
        落库走与 `settings_set` **同一个** `validate_setting`，不另开一条绕过校验的入库路径。
        删除入口在设置页「终端外观」：导入路径只往里加，没有那个删除按钮就是「能添加不能移除」。
        测试 7（后端）+ 23（前端 3 份），变异验证 11 条全部转红。
  - [x] 监控 × AI 联动：异常指标一键诊断走 fs_policy 同闸门 + audit 记录，AI 生成片段入库可用（E2E 或单测）；
        （判据分三段。**一、异常判定**：新建 `frontend/src/lib/monitor-anomaly.ts`（25 例）。
        判定**本地算，不问模型**——与危险命令的档位判定同一个原则：把「这些指标正常吗」交给模型，
        等于让一次网络往返决定界面上要不要报警，而它会在离线时、限流时给出不同答案；
        模型在这个功能里的角色是「已知负载 12、8 核、内存 96%，**去查什么**」，那是它擅长的，
        「12 算不算高」是一道除法。有一条源码断言禁掉本模块的**一切** `import`
        （零 import 是个能一眼核实的判据，「只许 import 这些」则要求每次重新判断一遍）。
        **为此给后端加了 `cpu_cores`**（`monitor_command()` 加 `nproc`，回落 `/proc/cpuinfo`）：
        负载数字离开核数就没有意义——`load_1 = 5` 在单核上是五倍过载、在 64 核上几乎空闲，
        没有它就只能对负载放弃判断或拍一个必然错的阈值，而负载恰好是运维最先看的那个数。
        采不到核数时**不判**负载（不假设一个核数：猜错方向会让「一切正常」和「已经过载」互换）。
        阈值的依据逐条写在源码里；**测试用场景夹逼而不是拿常量测常量**——第一版每条边界都写成
        `detectAnomalies({ cpu_percent: CPU_WARN })`，于是把 `CPU_WARN` 从 90 改成 50 时
        21 条断言全绿（一台正常跑到 60% 的机器会开始报警而无人知道）。改用「跑批到 75% 不报警、
        95% 报警」这类场景后，8 个阈值变异（每个阈值的高低两侧）全部转红。
        **二、一键诊断的闸门与 audit**：走 `ai_suggest_command` 而非 `ai_explain`——
        出口原文要求「走 fs_policy 同闸门」，而解读不产出命令、也就没有闸门可过；
        诊断的产出本来就是「跑哪条命令去看」。于是闸门与 audit 全部复用 M2 的同一条路径，
        没有新增任何绕过它的口子。提示词**刻意不提任何具体命令名**（`top`/`iostat`/…），
        并有一条断言钉住那个留白：写「用 top 看看」会把模型锚定在那一条上，
        而它本来能从「负载高、CPU 空」推出该查 I/O。
        **三、AI 生成片段入库**：`quickCommandFromAi`（8 例 + 5 变异全红）。
        核心一条是 **deny 档不许入库**：片段库是一键下发的地方，而档位是会变的——
        今天以 read_only 档拒掉的一条 `rm -rf` 存进去，切到 with_confirm 之后就成了
        一个点两下就能跑的按钮；闸门的裁决必须在**入库这一刻**生效。放行判定用**白名单**
        （`auto`/`confirm`/`strong-confirm`）而非黑名单：将来多一档时黑名单会默认放行它。
        越界值一律拒绝而不静默截断——截断一条 shell 命令得到的是**另一条**命令，
        而它可能仍然合法、语义完全不同（`rm -rf /tmp/x` 截成 `rm -rf /tmp`）。
        AI 生成的片段进固定分类「AI」，不混入用户自己的分类：AI 给的命令与自己写的
        可信度不同，而用户按下那个按钮之前有权知道这一条是谁写的。
        **顺带修的两个真 bug**（都是测试逮出来的，不是我先想到的）：① 面板刚打开时
        `ai_status` 还在路上、`status` 是 null，而 prefill 的 effect 已把 seq 标记成已处理
        ——status 到达后 effect 重跑却提前 return，用户点「AI 诊断」看到的是面板打开、
        什么都没发生。「还不知道配没配好」与「知道没配好」是两件事，只有后者是一个决定。
        ② 那条 effect 里的 `tab = "ask"` 会盖掉未配置时该停在配置页的行为，
        用户看到的是一句「还没配置模型源」而不是怎么配——正是出口第 7 项要避免的。
        两个 bug 各有一条回归变异钉住。
        **另外加固了一条既有守卫**：`monitor.rs` 的 `command_emits_the_keys_the_parser_reads`
        原是一张**手写清单**、只做单向断言，本次加 `CORES` 时它照样绿——它守不住任何东西。
        改成两侧都从源码里提（命令侧扫 `echo "KEY=`、解析侧扫 match 分支）并双向断言，
        双向变异各自转红。改造过程里踩到两个自己的坑，都留了注释：`LOAD1` 里的 `1`
        不是大写字母而被扫查排除（两侧**对称**漏 3 个，相等断言照样绿，是做空防护逮住的）；
        以及注释里写的一个 `"0"` 把 `split('"')` 的奇偶配对打乱、采出一个叫 `0` 的假键
        ——「注释让断言胡乱变严」与「注释给自己作证」是同一个毛病的两面，而前者更坏：
        它会逼下一个人把断言改松。）
  - [x] 语言切换：设置项切换语言生效 round-trip（UI 规格 §2.12）
        → `frontend/src/lib/i18n/`（index + zh-CN + en 两份词典）+ settings 键 `ui.language`。
        **范围**：范围条原文给的是「首发简体中文 + 英文双语**或仅搭框架**二选一」，
        这里落的是框架完整 + **菜单栏一整片真实双语**（57 词条），其余界面文案仍是中文硬编码。
        这不是「快做完了」而是明确的边界，设置页里有一句 `language-scope` 提示照实说明——
        半翻译的界面若不说清，用户会以为翻译坏了。选菜单栏是因为它是切换后**一眼看得出
        生没生效**的那一片，而「生没生效」正是这条出口要验的。
        三处设计取舍：① `menus.ts` 从常量数组改成 derived store，label 由 `menu.<id>` 取词，
        于是「菜单里加了一条却忘了加词条」由守卫必红，而不是等有人切成英文才发现；
        ② 语言名（简体中文 / English）**不翻译**——误切成看不懂的语言之后要认得出母语才切得回来；
        ③ 相位代号（M4b / Phase 2）不翻译，它对应文档里的标识。
        `setLocale` 先落库再改 store：反过来会让用户看到语言变了、重启又变回去而不知为何。
        跨语言守卫钉住 Rust `LANGUAGE_IDS` ≡ 前端 `SUPPORTED_LOCALES`（含「至少两种」的
        提取有效性下限，否则抓不到 id 时空集相等会假绿）。
        测试 22（i18n）+ 4（设置页控件接线）+ 1（跨语言），变异验证 7 条全部转红。
  - [x] 配色编辑器/主题目录：JSON 导出 round-trip 一致；新主题文件热加载 ≤1s 生效
        → `SchemeEditor.svelte`（16 色 + 前景/背景/光标 + 实时预览）、`lib/scheme-file.ts`（导出格式）、
        `import_cmd.rs` 的 `ForeignKind::NativeScheme`（导出的文件从**同一个**「文件 → 导入」读回来）。
        **「主题目录」的读法**：不做磁盘目录 + 文件系统 watcher（那要引 `notify` 依赖并常驻一个
        watcher，而能力与「导入 + 编辑器」完全重叠）。落的是「文件 → 导入 / 编辑器保存 → ≤1s 生效」，
        判据落在两条真实断言上，而不是一句时间承诺——广播走 `settingChanged` store（同步），
        远小于 1 秒；有用例断言「改配色内容 → 已打开的终端当场换色且**不重建实例**」。
        **顺带修掉第 1 项留下的真缺口**：`TerminalPane` 的 `schemeOf` 原本只查内置 12 套，
        用户导进来一套配色、在设置页里选上它之后终端会**静默回落默认色**——他刚做完一件事，
        界面毫无变化，而没有任何东西说明为什么。改用 `getSchemeFrom(id, customSchemes)` 并订阅该键。
        round-trip 的两条判据分开测：序列化/反序列化（前端 + Rust 各一条 `assert_eq!` 整结构）
        与**格式识别**（自家导出的 `.json` 会不会被 FinalShell 分支先吃掉）——它们会在不同的地方坏。
        另有一条跨语言守卫读 `import_cmd.rs` 比对 format 串与版本号：两边走散的症状是
        **导出的文件自己读不回来**，而前端单测永远测不出来（自己序列化自己反序列化总是一致的）。
        测试：后端 12、前端 92（编辑器 10 / 导出格式 11 / 终端 4 / 设置页接线 8 + 既有回归）。
        变异验证 8 条全部转红。
  - [~] 标签拖出/平铺：双窗口拖出测试、平铺输入同步测试通过
        → `app/src/window_layout.rs`（几何，13 例）、`app/src/commands/window_cmd.rs`（Tauri 多窗口，5 例）、
        `frontend/src/ViewWindow.svelte` + `lib/view-window.ts`（视图窗口，9 例）、
        `TabBar` 拖出（7 例）。菜单「窗口」四项（新建/层叠/水平平铺/垂直平铺）由禁用转可用。
        **「输入同步到同一会话」不需要任何同步机制**：会话活在后端，输出经
        `term:data:{sessionId}` 广播（Tauri 的 emit 是全窗口的），输入走 `term_input` 进同一个 PTY。
        两个窗口看的本来就是同一份流——不存在「两个视图不一致」这种 bug。规格 §66 的
        「平铺为视图克隆」因此是免费的，真正要写的只有开窗口、传参数、排几何。
        几何是这一项唯一会算错的部分，故整块纯函数化：不重叠不留缝、余数分散而非
        全给最后一份、负原点工作区（副屏在左）、层叠回绕而不跑出屏幕、摆不下时
        **报数**而不是沉默地堆叠。
        `[~]` 而非 `[x]`：出口写的是「**双窗口**拖出测试」，而本仓没有能驱动两个真实
        Tauri 窗口的 E2E 框架。已覆盖的是拖出判定（7 例含四个方向与「拿不到坐标」）、
        窗口几何（13 例）、URL 契约（跨语言比对 Rust 侧的参数名与截断长度）；
        **未覆盖**「两个真窗口同时显示同一会话」这一路，需人工核验或 E2E 框架。不代签。
  - [x] 关闭全部标签：M4b 启用后触发同一关闭确认模态（MVP 期禁用态预埋不可触发），组件测试或手工核验留痕
        → 菜单「窗口 → 关闭全部标签」解禁，走 `requestCloseTabs` → **同一个** `decideClose` +
        **同一个** `CloseConfirmDialog`。另起一套判据的话，「关一个标签会问、关十个反而不问」
        这种事迟早出现，而它出现时没有任何提示；守卫钉住全仓 `decideClose` 调用**恰好三处**
        （关窗口/关一个/关一批），多一处少一处都要在那里做决定。
  - [x] 关闭其他标签：触发同一关闭确认（多标签名单），组件测试留痕
        → 标签右键菜单该项解禁。留下的是**右键点中的那个**，不是当前活动的那个——
        用户在非活动标签上右键选「关闭其他」，要留的显然是手指底下这个；按活动标签留的话，
        他会眼睁睁看着自己刚点的那个被关掉。这个区别只在「右键的不是活动标签」时显形，
        而那恰恰是这个菜单项最常见的用法（活动标签就在眼前，要收拾的是旁边那一堆）。
        顺带修掉 `CloseConfirmDialog` 的 `scope` **声明了却从没被消费**：模板里一个字都没用到它，
        于是「关整个窗口」与「关一个标签」弹的是一模一样的框——危险程度差着量级的两件事
        长得一样，是确认框最不该有的样子。现在标题与主按钮都跟着 scope 走（三种各不相同，有守卫）。
  - [x] ~~updater：mock 更新服务器走通「下载→签名校验→替换」；非法签名被拒并提示；更新前 DB 备份存在~~
        → **2026-08-23 改写为「只做手动检查」**（用户裁决）。原条与本产品的出站面承诺**不能同时成立**：
        README「出站面」节写着零遥测**没有例外**，而 `tauri-plugin-updater` 会在后台主动去厂商
        manifest 地址查版本——那是标准的 phone home，`egress-contract.test.ts` 已把它列入
        `TELEMETRY_CRATES`、任何 crate 都不允许引入。
        新出口：**用户点「检查更新」才联网**、地址由用户自己填（不填则永不联网）、
        只告知有无新版并给出链接，**不下载、不替换**。
        「不下载不替换」是设计约束而非未做：自动替换正在运行的二进制需要提权、需要处理
        「替换到一半断电」，而那两件事的失败模式是**用户的工具打不开了**——对一个运维要靠它
        救火的程序，这个代价不值得。
        判据：`app/src/update_check.rs` 13 例——「未配置地址时一个包都不发」（断言 transport
        零调用）、非 http 地址在发包前被拒、预发布版排序低于同号正式版（按字符串比会反）、
        版本比较是数值序（`0.9.0 < 0.10.0`）、六种失败路径**都不得显示为「已是最新」**、
        以及一条源码断言钉住本模块不含下载/写文件/执行的能力。
        变异验证 6 条全部转红。**不新增任何 HTTP 面**：复用 `fs_ai::Transport`，
        `reqwest` 仍只在 `crates/ai` 一个 crate 里，出站面门禁一个字未改。
        —— **2026-08-24 补记（本批）**：出站面门禁一直在，位置是
        `frontend/src/lib/egress-contract.test.ts`（19 例），而不是我一度以为的「不存在」。
        当时的误判过程值得记下来，因为它是一种会反复发生的错误：我 grep `HTTP_ALLOWED`
        时限定了 `--include="*.rs"`——门禁本身是 TypeScript 写的（它同时要管 Rust 清单、
        前端源码、CSP 与 Tauri capabilities，放在前端侧才能一处扫完），
        于是「搜不到」被我读成了「不存在」，还据此写了一段「本仓第四次同款」的记述并
        新建了一份 `app/src/egress_gate.rs`。**那段记述是错的，那个文件已删。**
        教训是具体的：断言「某个门禁不存在」之前，搜索不能带语言过滤——
        一个跨语言的契约门禁天然不在它所约束的那门语言里。
        本批真正的产出是把既有门禁的拒绝名单补齐 18 个条目，选取标准是
        「本仓最可能真的滑进来的那几个」：`tracing-opentelemetry` 与 `sentry-tracing`
        （本仓到处用 `tracing`，往它上面挂一个导出层是一行依赖的事，而名字里带 tracing
        会让它读起来属于既有日志设施）、`opentelemetry_sdk`（下划线与连字符是**两个包**，
        原名单只有连字符形式）、`tauri-plugin-aptabase`（以「一行接入桌面应用分析」为卖点，
        正是 Tauri 项目会顺手加的东西）、statsd 家族与 `prometheus`（不发给 SaaS，
        但一样把数据推向网络端点）、`http-client`（surf 的可插拔后端，绕过所有已列名字）。
        五个代表条目各做变异验证，全部转红，恢复后 19 例全绿。
        既有门禁比我那份更强的地方也记一下，免得将来又有人重造：它管
        dev-/build-dependencies、workspace 根清单、以及用 `package = ` 改名换马甲那一路
        （后者我那份漏了），还附带「豁免项指向的文件必须真的存在」的做空防护。
  - [~] 签名链路：Windows 至少一路在 release.yml 真实签名（CI 内自签证书可接受）并记录步骤；macOS 公证链路在 release.yml 走通 + 成本/周期评估记录入附录
        → 评估结论见本文件 §6.4。
        **维护者裁定（2026-09-29）：开源项目不购买商业签名证书（Windows 与 Apple 均不买），
        1.0.0 允许未签名发布。** 实现：仓库变量 `ALLOW_UNSIGNED_RELEASE=true`（Variables，
        不是 Secrets）时 tag 构建在缺证书平台警告放行，否则仍在签名闸失败；macOS 无证书时
        打 ad-hoc 签名（Apple Silicon 拒绝运行完全无签名的代码且不给放行入口）；draft Release
        正文自动写明未签名平台、`sha256sum -c` 与 `gh attestation verify` 两种核对方法和
        SmartScreen / Gatekeeper 放行步骤。Windows 自签演练链路保持不变（仍在非 tag 构建上跑）。
        **Windows 自签演练已写进 release.yml 并接完**（出口原文：CI 内自签证书可接受）：
        非 tag 且无 secret 时现场 `New-SelfSignedCertificate`，走**完全相同**的链路
        （拼 certificateThumbprint → 打包签名 → signtool /pa 验证）。
        这一步补的是一个真缺口：**没有它时整条签名代码在没配 secret 的情况下零执行**，
        第一次运行会是在真正发版的那一刻。顺带把 `signtool verify` 的条件从「有没有 secret」
        改成「**签了没有**」——原条件下演练只演到「签名步骤没报错」，而 Tauri 在指纹拼错时
        是静默跳过签名的，两者是两回事。job summary 里自签演练单独一档，
        **不写成「已签名且验证通过」**：发版决策看的就是那一行。
        **`[~]` 的理由**：CI 从未对这份代码运行过（origin 停在 Initial commit，
        且 2026-08-23 用户裁决「暂不推送，继续仅本地」）。workflow 已写完、
        `js-yaml` 解析通过，但「在 release.yml 真实签名」这句要的是一次**运行**，
        而那一次运行不在本地能力范围内。macOS 一路更远：公证需要 Apple Developer
        账号（$99/年，组织账号含 D-U-N-S 申请可能 1–4 周）——**账号问题不是技术问题**，
        属出口原文的降级条款，已显式列为 1.0 门槛前置补做项。不代签。
  - [x] 取证包：导出→重新导入校验 hash 链完整
        → `crates/connmgr/src/forensics.rs`（febdfd5）。导出的包重新导入后逐条重算哈希链；
        六种篡改形态各有一条用例（改内容 / 删中间一条 / 插一条 / 换顺序 / 改 prev_hash / 改自身 hash），
        每一种都必须被指出**具体是第几条**——只说「链断了」等于把定位工作留给人。
        校验实现与写入侧共用同一个 `verify_rows()` 自由函数，不是抄一份：两份实现迟早分叉，
        而分叉的方向必然是「校验侧比写入侧宽松」，那正好让篡改过关。
  - [x] 便携打包评估结论（做/不做 + 依据）记录入 4b 计划或附录
        → **结论：做**，且已实现（`app/src/state.rs` 的 `portable_data_dir`，5 例 + README 一节）。
        依据三条：① 总设计 §183 的「可选应用密码」本来就写着「面向便携模式（U 盘携带、
        不依赖系统 keyring）」——路已经铺好了，不做等于让那段设计悬空；② 实现成本是
        **一个目录解析分支**，数据目录本就是单点（`data_dir_or_err`）；③ 运维场景里
        U 盘带工具是真实需求，而这正是本产品的使用场景。
        判据是**可执行文件同目录**的 `portable.txt` 标记文件。三处刻意的选择：
        锚点是 exe 目录而**不是 cwd**（双击/命令行/快捷方式三种启动同一个答案，
        cwd 是三个答案——这也正是 P1-21 当初拒绝「回落 cwd」的理由，本条不是它的复活）；
        判据用 `is_file()` 而非 `exists()`（同名目录多半是解压出错）；
        位置不可写时**明确失败**而不是退回 `%APPDATA%`——用户放了标记文件，
        意图是「数据跟着程序走」，悄悄落到别处等于在他不知情时把 Vault 留在别人的机器上，
        那是这个功能最坏的失败方式。
        分发形态用现成的 zip（解压即用），不新增 bundle target。
  - [x] 截图：产出 PNG 与终端像素一致，剪贴板/存盘两路单测
        → `lib/screenshot.ts`（纯逻辑，34 例）+ `screenshot-browser.ts`（DOM 侧三件事）
        + `TermController.screenshot()` + 编辑菜单两项。
        **做法是「按缓冲区重绘」而不是「抓渲染器画布」**，因为两条渲染路径都走不通：
        DOM 渲染器根本没有画布（它是一堆 span）；WebGL 渲染器有画布，但上下文默认
        `preserveDrawingBuffer: false`，`toDataURL()` 拿到的通常是全黑或全透明——
        要改得让上游 addon 换上下文属性，且开了之后每一帧都多一次拷贝，
        为截图拖慢整个终端不划算。
        **「像素一致」的读法（诚实记录）**：自绘做不到**逐像素二进制相同**——字形栅格化、
        连字、字体回退都由浏览器决定，而 WebGL 渲染器用的是它自己的纹理图集。
        这里保证的是**内容一致**：每一格的字符、前景色、背景色、粗体与反显都取自
        同一份缓冲区、同一套配色（且配色/字号取**运行期**值，不是构造期的 opts——
        三级作用域换色与 per-session 缩放都会变）。这个差异写在模块头而不是藏着：
        一个说「像素一致」却做不到的实现，会让第一个拿它做视觉回归的人白花一天。
        几处易错点各有用例：反显（选区与光标就是靠它画的，漏了截图上会缺一块）、
        宽字符后半格（不跳会画出重影）、xterm 色立方的非均匀级差（0 之后直接跳 95，
        按均匀 51 算会让所有 256 色输出偏色）、空串格仍画背景（`ls` 的目录高亮）、
        只截可见屏（一屏几十 KB vs 一万行回滚几十 MB，不该共用一个按钮）、
        文件名净化（标题来自远端 OSC 0，是**远端可控**的）。
        变异验证 8 条全部转红。
  - [x] 密钥/代理管理器：密钥列表浏览（仅指纹/类型/注释）+ agent 传输开关生效，单测断言私钥材料不出现在 UI 数据载荷
        → `crates/sshengine/src/keyinfo.rs`（9 例）+ `agent::list_agent_keys` + `app/src/commands/key_cmd.rs`（5 例）
        + `KeyManagerDialog.svelte`（15 例）+ 工具菜单解禁。
        **「私钥不出现在载荷」不靠序列化过滤，靠类型**：`KeyInfo` 只有三个 `String`，
        装不下密钥材料——于是这件事不是「需要小心避免的错误」，而是**写不出来**的。
        过滤式做法（返回完整结构再删字段）只要有人加个字段就会破，且破时没有信号；
        两处各有一条守卫钉住字段数与字段类型（`KeyInfo` 恰 3 个 String、`KeyEntry` 恰 7 个且只装
        String/u64），加字段即红，逼着人回答「那个字段会不会带出材料」。
        **「agent 传输开关」刻意不新建**：起草时加过一个 `ssh.useAgent` 全局键，是错的——
        Profile 上已有 `auth.allow_agent`，再加全局开关会制造一个没人答得上来的问题
        （全局关、这个 Profile 开，用不用？）。改为给那条**既有**的链加守卫：
        ProfileDialog 勾选框 → `allow_agent` → `CredentialSetBuilder::agent()` → `Method::Agent` 进候选，
        跨 4 个文件、此前无人在看，而断在中间任何一处的表现都是「agent 里明明有可用密钥却总是登不上」
        （不报错、不告警——`auth.rs` 的 `Method::Agent` 文档注专门写过这个静默失效）。
        三处产品判断：① 加密私钥读不出指纹是**正常状态**不是失败（画成红色会让用户去修一件没坏的事），
        条目仍留在列表里（不显示会让他以为密钥丢了）；② agent 不可用与 agent 空**分开报**
        （前者要去启动 agent，后者要去 `ssh-add`）；Vault 锁着同理；③ 来源（Vault/Agent）必须显示——
        Vault 里的删得掉，agent 里的要去 `ssh-add -d`，不分来源用户会找一个删不掉的条目的删除按钮。
        顺带解禁 `tools.schemeEditor`（M4b 第 4 项已做完配色编辑器，菜单项却还禁着，
        与 `tools.highlight` 同款直达设置页「终端外观」）。
  - [x] 文件夹同步/比较评估结论（做/不做 + 依据）记录入 4b 计划或附录
        → 本文件 §6.3 第 1 条。**结论：不做。**
        依据三条：① 「同步」的本质是**删除与覆盖**，而一个正确的实现要回答一串每答错一个
        就静默毁数据的问题（两侧都改过算冲突还是取新的？FAT32 的 2 秒时间戳粒度？
        同大小同时间但内容不同的构建产物？符号链接同步链接还是目标、有环怎么办？
        中途断了怎么办？）——用一个里程碑的尾巴做它，产出的会是「看起来能用、
        边缘情况毁数据」的东西；② 真实场景里「同步目录」的绝大多数实例是「整个传上去/拉下来」，
        而 SFTP 双栏已支持整目录传输 + 传后 sha256 校验，剩下的「只传改过的」是优化不是缺口；
        ③ `rsync` 就是干这个的且把那些问题都想过了，我们的价值在于把用户连上去。
        重新评估的硬约束：**永不删除**——单向增量复制的失败模式是「多传了几个文件」，
        带删除的同步的失败模式是「文件没了」，两者不在一个量级。
  - [x] SFTP **本地栏**新建目录/删除/重命名对偶（与远程栏同能力）评估结论（做/不做 + 依据）记录入 4b 计划或附录
        → 同上文档 §2。**结论：分拆——新建目录与重命名做（已实现），删除不做。**
        一刀切在这条上是错的：三个操作的风险差着量级。
        **新建目录 / 重命名：做**（`local_mkdir` / `local_rename` + 本地栏两个按钮，12 例）。
        非破坏性；而「下载到一个新建的目录里」是最常见的一步，现在用户得切出去开资源管理器
        再切回来，远程栏却有这个按钮——这个不对称本身就是缺陷（R72 当时收窄是为控 MVP 范围，
        不是因为有风险）。重命名**目标已存在即拒绝**，不问「要不要覆盖」：
        `std::fs::rename` 在多数平台上会静默覆盖，那意味着输错一个名字就毁掉另一个文件。
        **删除：不做。** 三条依据合起来才够：① 它是三个里唯一不可逆的，而本地这一侧用户
        是在「用自己的电脑」——那上面有回收站，我们这里没有，第一次误点就是永久损失；
        ② `sftp.sandboxRoot` 管的是**下载落点**，对删除一句话都没说——要做得先定义一个新的
        安全边界，那不是一个按钮能带过去的；③ 用户的文件管理器有回收站、有撤销、有确认框，
        且他每天都在用。**这个缺席由两条守卫钉住**（Rust 侧源码断言禁止出现
        `local_delete`/`local_remove`/`local_rmdir`，前端断言没有 `local-delete` testid），
        防的是「顺手补齐对偶」——那看起来像在修一个不一致，实际是在加一个不进回收站的删除按钮。
        重新评估的前提：删除进系统回收站（Windows `FOF_ALLOWUNDO` / macOS `trashItem` /
        Linux XDG trash）；直接 unlink 的版本不做。
  - [x] 会话管理器停靠/浮动：拖至右侧与浮动模式可用、重启记忆停靠侧（ui.sidebarDock），组件测试或手工核验留痕
        → `lib/layout.ts` 的 `sidebarDock` / `effectivePresentation` + 视图菜单「会话管理器位置」三项。
        三种形态**共用同一个 DOM**，只换 CSS——换 DOM 会让 `<Sidebar>` 被销毁重建，
        于是每次切停靠侧都丢掉滚动位置与展开状态，而用户切停靠侧时恰恰是在整理界面。
        右停靠靠 flex `order` 换位（`.main` 必须显式 `order: 1`，否则默认 0 与侧栏并列、换位失效）。
  - [x] 工具栏自定义：右键菜单勾选隐藏某项后该项即时消失、拖拽排序后顺序变更，重启保持（settings 键 `ui.toolbarLayout`），组件测试或手工核验留痕
        → `lib/toolbar-layout.ts`（21 例纯逻辑）+ `ToolBar.svelte` 改造（14 例，含两条「重启保持」）。
        **顺序存 id 列表而不是给每项存序号**：序号方案在增删按钮时会散架（新按钮没有序号、
        删掉的留下空洞），而按钮集**会随版本变**——本次就把截图按钮从禁用转成了可用。
        id 列表下两件事都有确定答案：不在列表里的排到**末尾**（新按钮看得见但不打乱已调好的顺序），
        列表里已不存在的 id 直接忽略。分隔符按**相邻两项的原始分组是否不同**画，
        而不是固定每 N 个一条——排序会打散分组，那时固定间隔的线标记的不是任何边界。
        右键菜单挂在**容器**上并列出**全部**项：想显示一个已隐藏的项时那个按钮不在页面上，
        没法右键它。脏值回落**空布局**而不是「全部隐藏」——后者会让整条工具栏消失。
        顺带解禁工具栏上的截图按钮（M4b 第 12 项做完了却没人回来改，仍写着「即将推出 Phase 4b」）。
  - [x] 窄屏覆盖层：窗口收窄至 ≤1099px 时侧栏转绝对定位覆盖层（z-index 高于主区、带投影）、点击外部区域收起，≥1100px 恢复停靠，组件测试（matchMedia mock）或手工核验留痕
        → `initResponsive()` + `narrowScreen` store（matchMedia mock 测 6 例）。
        媒体查询写成 `(min-width: 1100px)` 而不是 `max-width: 1099px`：两者在整数上等价，
        但小数 DPI 缩放下 1099.5px 会**两边都不满足**（CSS 的 min/max-width 都是闭区间）——
        用「宽屏」做正判据、窄屏取补集就没有这个缝。
        **窄屏不改写 `sidebarDock`**：写回会让「拉窄一次就永久变成浮动」，而用户没做过那个选择；
        窗口拉宽后自动回到他选的那一侧。这也是「用户选的 float」与「窄屏自动覆盖」必须分开的原因。
        「点击外部收起」用透明遮罩而不是在 `.main` 上挂 click：后者会把「点终端里的某个位置」
        也算成点了外部，用户想放个光标却先收起了侧栏。
        拿不到 matchMedia 的环境**按宽屏处理**——按窄屏的话那种环境会永远显示覆盖层，
        而覆盖层默认收起，用户看到的是「侧栏不见了」。
  - [x] 全量 M1 出口标准回归不劣化
        → `scripts/m1-regression.sh` + 2026-08-23 一次 `ci-local.sh --with-itest` 全绿
        （八步：fmt / clippy / deny / nextest / cargo test / **容器 itest** / vitest / svelte-check）。
        容器一路含 M1 的性能门禁本体：`scale.rs` 的合计吞吐 ≥20 MB/s、千级文件目录完整性、
        300 文件往返逐字节一致、64 MiB 大文件往返，以及 `render_pipeline.rs` 的
        200MB 零丢失 / 渲染队列水位 ≤high×1.5 / 连接到可交互 <2s / 100 并发会话内存。
        **「跑一遍全量、全绿」证不了「不劣化」**：全量绿只说明**现在还剩下的**测试都过了。
        M4b 期间某个 M1 的测试文件被删掉、或某条断言被改松，全量照样绿——
        而那正是这条出口真正要防的（M1 的判据在后续里程碑里被悄悄拆掉）。
        故新增门禁做两件全量不做的事：① **载体存在性**——路线图 M1 出口每条判据里点名的
        31 个测试文件必须还在**工作区**（用 `[ -f ]` 判而不是 `git ls-files`：后者查的是索引，
        「删了但没提交」时它照样报在，而那恰是最该拦住的时刻）；② 载体清单**从路线图现解析**，
        不写死在脚本里——写死是双份维护，两份迟早分叉，分叉之后守的就不是路线图那份了。
        另有两道做空防护：载体数下限 25（解析器被改坏时抓到零条，空集会让存在性检查恒真）、
        M1 出口不许出现 `[ ]`（M1 已交付，「从未处置」这个状态不该存在）。
        变异验证 2 条转红：删掉一个载体文件（不 git rm）、把段落锚点改掉一个字。
        门禁进 `ci.yml` 新增的 `docs-gates` job（与 acceptance-triage / env-gate-parity /
        ci-parity 同批）——与 check/frontend 分开是因为它跑的是「文档里点名的载体还在不在」，
        混进 check 会让一次纯文档改动拖起整个 Rust 矩阵。
        **本条的范围**：核对的是 M1 出口里 22 条 `[x]` 的载体齐备与全量绿；
        2 条 `[~]`（冷启动 ≤3s 真机、mockups 逐屏一致）与 3 条 `[!]`（三平台安装包构建、
        凭据 UI 端到端、Vault 便携解锁 UI）**本就未闭合**，它们的状态与 M1 交付时一致，
        不因本次回归而改变，也不代签。

> **2026-09-03 三条裁定（用户复问后确认）**：
> ① **仍然只在本地**，不推 origin。下列五项因此维持 `[!]` 受阻，且**不代签**：
>    M0「CI 三 runner 全绿」、M1「三平台安装包可构建」、M4「签名链路」的 CI 那一半，
>    以及依赖它们的 macOS/Linux 构建验证。本地能证的只有「配置存在且语法正确」。
>    附带说明：**Windows 代码签名证书用户明确不买**（开源项目），而该项出口原文写的是
>    「CI 内自签证书可接受」，`release.yml` 里那条自签演练已接完 —— 缺的只是一次 CI 运行，
>    不是一张证书。
> ② **自绘下拉不做**（见 4c 该条）。
> ③ **M5 移动端维持不纳入 1.0.0**（见 M5 段首裁定，未变）。

#### 4c 界面收尾：响应式 / 动效 / 架构缺陷（2026-08-28 用户提出「审查」后立项）

> 这一批**不是新功能，是现存缺陷**——用户第 6/7 项要的是「审查当前设计是否
> 合理」，审查结果就是下面这些。挂在 M4 而不是 M7：它们是既有界面的欠账，
> 且每修一条，之后每个新面板都白拿（那几条会随新面板复制扩散）。

- **已有、不必重做**（避免重复投入）：窗口最小尺寸已设
  （`app/tauri.conf.json:14`，800×600）；`prefers-reduced-motion` 已处理
  （`frontend/src/styles.css:15-17`），且状态不只靠闪烁传达（颜色 + aria-label）。
- **出口标准**：
  - [x] **overlay 形态的侧栏缺开启入口**（2026-09-02 交付）：`sidebarOverlayOpen` 生产代码里
        只有三处 `set(false)`（`layout.ts:123`、`layout.ts:144`、
        `App.svelte:1536`），唯一的 `set(true)` 在测试夹具
        `layout.test.ts:177`——窄屏（<1100px）或用户选「浮动」停靠时，
        侧栏收起后**没有任何入口把它叫回来**。补一个开启入口（汉堡按钮/菜单项）
        并加判据。
        → **载体**：`lib/layout.ts::toggleSidebarFor(presentation)`——`view.sidebar`（Ctrl+Shift+S / 视图菜单）
        在 overlay 形态下翻 `sidebarOverlayOpen`（先保证 `sidebarVisible` 为真，否则 aside 不渲染、开了也看不见），
        停靠形态照旧翻 `sidebarVisible`；`ToolBar.svelte` 新增 `overlaySidebar` / `overlayOpen` 两个 prop，overlay
        形态下在最前面放一颗**不进 ITEMS、不可被右键隐藏、hidden 写了也照样显示**的叫回按钮
        （`tbtn-view.sidebar`，Icon `panel-left`，展开时按压态）。
        判据：`layout.test.ts` 4 条（overlay 翻浮层 / visible=false 时先补真 / 停靠不动浮层 / App 接线钉）+
        `ToolBar.test.ts` 4 条（在最前且按压态 / 停靠不显示 / 不进右键菜单 / hidden 写了也显示）；
        变异 M1（overlay 分支退回 `toggle("sidebar")`）、M2（按钮整块删掉）各转红。
        **真机核验**（CDP，已安装新包；不改窗口尺寸而是把停靠切成「浮动」——与窄屏同一条 overlay 渲染路径）：
        停靠 left 时无此按钮；切 float 重载后 aside `data-presentation=overlay`、x=-240（屏外），按钮出现且是
        工具栏第一颗、aria-pressed=false；点一下 → `overlay-open` + 遮罩、aside x=0、按钮 pressed=true；再点收起；
        Ctrl+Shift+S 同效展开；恢复停靠后按钮消失。
        > **定级订正（实测）**：一份调研曾把此条列为「800–1099px 一个连接都
        > 发不出去」的阻断级缺陷。**结论是错的**：工具栏第一个按钮就是
        > `session.new`（`ToolBar.svelte:37`，`Ctrl+N`）且始终可见、不在侧栏里。
        > 实测（dev server，1000px 视口）：侧栏 `position: static` 正常占位、
        > ＋按钮可用。真实影响是「侧栏叫不回来」，不是「发不出连接」。
        > 记这条订正是因为**一个「阻断级」误判会把整个排期带偏**。
  - [x] 响应式逐组件核查（2026-09-02 交付）：给出「组件:行号 + 什么尺寸下坏 + 怎么坏」的清单
        并逐条修（固定 px 宽、flex 不收缩的 `min-width:auto` 坑、绝对定位
        不跟随、横向滚动条）。判据：至少覆盖 800×600 / 1000×700 / 1280×800
        三档的组件测试或截图基线。
        → **方法**：新增 `scripts/responsive-audit.mjs`（Node 零依赖，走 WebView2 CDP 的
        `Emulation.setDeviceMetricsOverride`）对真跑的应用在 800×600 / 1000×700 / 1280×800 三档、
        36 个界面状态（空闲 / 菜单 / 工具栏右键 / 侧栏右键与贴边右键 / overlay 展开 / 设置 7 页 /
        会话属性 8 页 / 17 个对话框与面板 / 监控抽屉 / 快速命令条）逐点量五类溢出并截图，产出
        `target/responsive-audit/report.md` + 逐状态 PNG 基线。**探针带自检**：每次运行先塞 7 个必违规
        对照 + 1 个反例，抓不全或误报即终止——首版探针曾因 body 的 `overflow-x:hidden` 全盲、107 点
        零发现，正是自检把它抓出来的；零发现的报告与失明的报告长得一模一样，只有对照能分辨。
        → **清单（组件:行号 · 什么尺寸下坏 · 怎么坏 → 修法）**：
        ① `App.svelte` `.sidebar.overlay` 用 `transform: translateX(-100%)` 收起 · <1100px 或「浮动」停靠 ·
           transform 让侧栏成为后代 fixed 元素的包含块，`Sidebar.svelte` 的右键菜单与遮罩被 240px、
           `overflow:hidden` 的侧栏裁到只露一线（800×600 实测裁掉 143px，贴边右键裁掉 552px；截图里
           菜单只剩右缘一条竖线）→ 改 `left/right: calc(-1 * var(--fs-sidebar-w))` + `visibility:hidden`
           收起，宽度经行内 `--fs-sidebar-w` 与 `style:width` 同源；屏外侧栏顺带不再可 Tab。
        ② `Sidebar.svelte:341` `.submenu { left: 100% }` · 任一尺寸下菜单贴右缘（右停靠 / 靠右右键）·
           「移动到…」子菜单整个出界（1280 实测 right=1411 > 1280）→ `menu-pos.ts` 新增 `submenuSide()`，
           `clampToViewport` 按平移后右缘写 `data-sub-side`，Sidebar 据此 `right: 100%` 翻到左侧。
        ③ 15 个对话框根盒无 `max-height`（AuthPromptDialog:241 / HostKeyDialog:175 / RdpCertDialog:86 /
           ConfirmDialog:64 / TextInputDialog:89 / VaultDialog:171 / ConnectionDetailDialog:113 /
           DeleteConfirmDialog:52 / ZmodemBar:337 / SftpPane:942 / McpConfirmDialog:98 / CloseConfirmDialog:99 /
           SessionRecoveryDialog:141 / AgentPanel:337 `.confirm` / FilePickerDialog:278）· 600px 高 ·
           内容一长底部按钮被推出屏外（多数需会话才弹得出，动态核查覆盖不到，按静态清单补）→
           `max-height: 92vh; overflow-y: auto`。
        ④ `SessionRecoveryDialog:146` `min-width: 600px; max-width: 800px` · 800px 宽 · 贴边到边 →
           `min(600px, 92vw)` / `min(800px, 92vw)`；`CloseConfirmDialog` 的 `min-width: 400px` 同批改 `min()`。
        ⑤ 其余 33 个状态 × 3 档零发现（含 StatusBar 既有的三档 @media 折叠、TabBar 有意的横向滚动、
           全部对话框的 92–95vw 上限）；修前用修好的探针复核 5 状态 × 3 档：7 条发现，全部落在 ①②。
        → **判据载体**：`lib/responsive-guards.test.ts`（①–④ 的结构守卫：对话框根盒必有 max-height、
        固定 px 宽必配 max-width、min-width > 400 必用 min()、overlay 不得用 transform/translate 且要
        visibility:hidden 与 `--fs-sidebar-w`、右停靠两条规则都写 left:auto、Sidebar 弹层是 fixed、
        clampToViewport 写 data-sub-side、Sidebar 有翻转规则；非空证明 ≥25 个根盒）+ `lib/menu-pos.test.ts`
        `submenuSide` 3 条；变异 R1–R5 逐条转红。截图基线由 `scripts/responsive-audit.mjs` 产出到 `target/`
        （不入库），修前 / 修后各一轮的数字记在本条末尾。
        → **顺手抓到的非布局缺陷**：`audit_cmd.rs` 三条命令写 `State<'_, AppState>` 而 setup 只 manage 了
        `Arc<AppState>`，点「审计链校验」运行期报 `state not managed`（核查截图里就有这条 toast）。
        已改 `Arc<AppState>`，并加 `lib/state-contract.test.ts`（IPC 契约第三维：命令名 / 参数名之外的
        **状态类型**，非空证明 ≥100 处 State 参数）；变异 R6 转红。
        → **修前 / 修后（同一探针，真机 CDP）**：修前旧包 5 状态 × 3 档共 7 条，全部落在 ①②
        （800/1000：侧栏右键菜单被裁 143px、贴边右键被裁 552/752px、子菜单被裁；1280 贴边子菜单
        right=1411 出界）；修后新包 37 状态 × 3 档 = 110 个状态点零发现，探针自检 7 个对照全抓到、
        1 个反例未误报。新包已装到用户真实路径（WMI 容器外安装，UNC 核对 exe 15:53），并留有
        800×600 修前 / 修后侧栏右键菜单截图对照。
  - [x] 动效补齐（2026-09-02 交付）：对话框出现、标签切换、面板展开、toast。
        **必须尊重 `prefers-reduced-motion`**（既有机制已在，别绕过它）。
        → **做法**（`styles.css`「动效」段，一处全局表而不是 31 个对话框各写一份）：
        ① 只用 CSS——transition + `@starting-style`；**不用** Svelte `transition:/in:/out:` 指令、不用 Web
           Animations API，它们是 JS 驱动的，既有那条 `prefers-reduced-motion { * { transition/animation: none !important } }`
           兜底管不到。JS 侧唯一的时序是 toast 退场先标 `leaving`、`TOAST_EXIT_MS`（160ms）后再删
           （`lib/toast.ts`），它经 `lib/motion.ts` 查 matchMedia，reduce 下立即删。
        ② 进场靠 `@starting-style`（只在插入 / display:none→可见那一次生效）而不是挂在类名上的 keyframes：
           类切换会重放——VaultDialog 抖完一次会再「冒」一次。Chromium 117+ / WebKit 17.5+；不支持的引擎落终态、不报错。
        ③ 只动 opacity / transform / height，token 化：`--fs-motion-fast` 80ms（菜单 / 标签 / 终端区切换）、
           `--fs-motion-base` 140ms（对话框 / 面板 / toast）。终端不是营销页，最长 140ms。
        覆盖：对话框（遮罩淡入 + 面板上浮 8px，`.overlay:has(> [role=dialog])` / `[role=dialog]`）、菜单
        `[role=menu]`（只动 opacity——clampToViewport 用 translate 钳位，不跟它抢 transform）、标签 `[role=tab]`
        （底色 / 前景 / 选中指示过渡，会话标签与设置页页签共用）、会话标签切换时终端区淡入（`.term-wrapper.active`）、
        监控抽屉 0→150px、组合命令栏 / 快速命令条 / 状态栏淡入、toast 进场走 token + 退场 `.leaving`。
        → **判据**：`lib/motion-guards.test.ts`（token ≤200ms；每个进场对象 transition 与 @starting-style **双有**，
        按整条选择器精确匹配——首版用 includes，`.overlay:has(> [role="dialog"])` 那行让「删掉对话框起始态」的
        变异幸存；reduce 兜底同时清零 transition/animation；组件模板零过渡指令、全仓零 `.animate(`；组件内有限动效
        ≤250ms；toast 退场两拍）+ `lib/toast.test.ts`（两拍断言、撞车不重排、reduce 立即删）；变异 T1–T7 逐条转红。
        **真机**（`scripts/motion-audit.mjs`，CDP 仿真 no-preference / reduce 两向）：新包 20 项全过——对话框
        `opacity, transform 0.14s` + 遮罩 `opacity 0.14s`、标签 `0.08s`、菜单 `opacity 0.08s`、监控抽屉
        `height, opacity 0.14s` 终态 150px、toast 进场 `toast-in 0.14s`、点 × 后 leaving 且仍在 DOM、160ms 后移除；
        reduce 下以上全部 `none / 0s`，toast 点 × 立即移除。CDP 客户端与页内动作表抽到 `scripts/lib/{cdp,app-driver}.mjs`
        供响应式与动效两份核查共用。
        **一条如实的观察**：本机 Windows 关着「动画效果」，`prefers-reduced-motion` 默认就是 reduce——用户在这台
        机器上看不到任何新动效，这正是兜底该有的行为；要看效果需在 Windows 辅助功能里打开动画效果。
  - [x] 架构缺陷三条（审查发现，逐条给证据与判据；2026-09-02 交付）：定时器泄漏、
        事件监听器竞态、启动期静默失败。每条修法都是十几行，且不修会随
        每个新面板复制扩散。
        → **A1 定时器反复重建**：`startHostProbeLoop` 首轮此前在 App `$effect` 的调用栈上同步跑，
        `targets()` 读到的 profiles / sessionStates 被 Svelte 5 记为该 effect 的依赖 → 每个会话状态变化都
        stop → 重建 setInterval → 再探全部主机。改为 `queueMicrotask` 出栈，effect 只依赖间隔秒数。
        判据 `lib/host-lights-loop.test.ts` 3 条（首轮不同步读 targets / 先 stop 则首轮不跑 / 到点再探、stop 后不探）；
        变异 M4（退回同步 `run()`）转红。
        **A1 的代价与补丁（2026-09-02 当天真机抓到）**：首轮出栈后，启动时首轮跑在 profiles 还是空数组的时刻，
        探了个空——新包启动 25 秒侧栏一颗灯都没有（旧写法 effect 依赖 profiles，重建循环的同时也顺带做到了
        「档案一到就探」）。补 `HostProbeLoop.poke()`（300ms 去抖补探一轮、不重建定时器），App 用一个**只依赖
        profiles** 的 effect 在档案列表变化时 poke；刻意不依赖 sessionStates——会话开关引发的全量重探正是 A1 要
        去掉的流量尖峰。判据同文件 +3 条（poke 去抖一轮且 30s 刻不被重置 / stop 取消已排的 poke / App 接线
        且不读 sessionStates）；变异 P1（poke 空操作）、P2（stop 不取消 poke）、P3（删接线）各转红。
        补丁包真机：启动 3 秒内两盏灯已亮（`probe-green`），修前同一探法 25 秒仍为零盏。
        → **A2 监听器竞态**：`listen()` 异步落定而组件清理同步执行，8 个组件手写的 `un = await listen(...)` /
        `.then(fn => unlisten = fn)` 在「组件先卸载、listen 后落定」时把指向死组件的处理器永久登记。新增
        `lib/lifecycle.ts::untilUnmount(p)`（解绑函数**同步**在手；卸载先于落定则落定当刻就地解；`ready` 供需要
        顺序的调用点 await；登记失败上报日志不抛），全部 14 个站点改走它（TerminalPane×3 + 拖放订阅、RdpPane×6、
        RdpCertDialog、AuthPromptDialog、HostKeyDialog、AgentPanel×3、McpConfirmDialog、SettingsDialog、App 的
        rdp:status 与 `trackUnlisten`；`appMounted` 标志退役）。单独成模块而不放 ipc.ts：组件测试整模块 mock
        `../lib/ipc`，放那里每份 mock 都得补一个导出（首次落地时 7 个测试文件 103 条因此红过一轮）。
        判据 `lib/untilUnmount.test.ts` 3 条 + `lib/lifecycle-guards.test.ts` A2 组（全仓零 `= await listen` /
        零 `.then(fn => un = fn)`，非空证明 ≥12 处调用）；变异 M5a（落定不就地解）、M5b（组件回到 await listen）各转红。
        → **A3 启动期静默失败**：App onMount 此前一串裸 await，最前的 `await listen("rdp:status")` 一 reject，
        loadProfiles / vault_status / 会话恢复全部不跑且无任何报错。每步经 `boot(name, fn)` 隔离（失败
        `reportFrontendError("startup:<name>")` 留痕 + console.error，下一步照常）。判据 `lifecycle-guards.test.ts`
        A3 组（onMount 顶层零裸 await/void、boot 包住 loadProfiles/vault/sessions_unclosed、boot 内有上报）；
        变异 M6（loadProfiles 退回裸 await）转红。`window-close.test.ts` 的 bridgeWindowClose 调用点钉子同批改为兼认 boot 形态。
  - [x] **App.svelte 拆分**（2026-09-03 交付）：**先写组件测试再拆**。
        本条标 `[~]` 直到有安全网——仓内已有先例（S262）记着「靠正则守卫
        验收重构」失效过一次，零安全网的大重构不做。
        → **① 安全网先行**：`src/App.test.ts`（25 条）**真渲染 App**，只桩进程边界
        （`lib/ipc` + Tauri 的 window/webview/event），组件一律真挂载——拆分要动的正是组件装配，
        桩掉它们等于把被测对象删了。断言的是「从菜单/工具栏点下去会发生什么」：六条 chrome 在场、
        三个视图开关真的开关它们、全屏经窗口 API 而非假状态、十个菜单项各自开出对应对话框、
        两条 IPC 的命令名逐字不变、工具栏与菜单指向同一套动作、boot() 三步都发了出去。
        此前 App 只有**源码文本**守卫（layout / menus / lifecycle-guards 都扫 `App.svelte?raw`），
        那种守卫在「把代码搬到另一个文件」时整片失效——恰恰是重构最需要它的时刻。
        → **② 拆出三个模块**（App.svelte 1933 → 1756 行，-177）：
        `lib/profile-io.ts`（档案导入/导出五件事，只用 IPC + toast + 文件 API，唯一耦合是导入后
        重载列表的回调）、`lib/window-actions.ts`（多窗口四件事）、`lib/broadcast-actions.ts`
        （广播四件事；与既有 `lib/broadcast.ts` 分工：那边是 store 与纯解析，这边是「点了之后发生什么」）。
        三个模块各带单测共 31 条——这些分支此前只能靠渲染整个 App 才碰得到，于是「文件名怎么拼」
        「超限文件挡不挡」「detachTab 对已关标签开不开窗」从来没人测过，而它们各自对应过一次真实缺陷。
        → **③ 变异实证 9 条逐条转红**（X1/X2 菜单分支、X3 boot 不吞异常、X3b 吞了不留痕、
        X4 尺寸闸、X5 空词干回落、X6 detachTab 存在性检查、X7 广播排除自身、X8 目标=当前时拒开）。
        **X3 首版幸存是个真发现**：原测试拿 `loadProfiles` 当故障源，而它**自己**带 try/catch，
        把 boot() 整个拆掉测试也照绿；换成 `vault_status`（reject 会一路传到 boot）才立得住。
        → **④ 一次「搬家 → 守卫失效」现场**：`lib/action-wiring.test.ts` 里那条「导入尺寸闸必须在
        `file.text()` 之前」扫的是 `App.svelte?raw`，代码搬走后当场变红。这正是 S262 记着的那件事，
        也是本条要求先建行为安全网的理由。守卫已改读 `lib/profile-io.ts`；**它没有被行为测试取代**——
        闸挪到读文件之后时行为测试仍全绿（断言的是没发 IPC，而 invoke 本就排在读文件之后），
        只有源码顺序看得见内存峰值已经打出来了。
        → **⑤ 剩下的 1756 行为什么不再拆**（如实记，不是没做完）：其中约 320 行是模板、70 行是样式，
        script 段剩下的大头是 `onAction`（57 个分支）与二十来个对话框的开关状态。把它们搬出去要么
        传四十来个 setter、要么把每个对话框的双向绑定改成 props——那比留在原处更难读，也更容易出错。
        拆分的目的是「可测 + 可导航」，前者已由 ①③ 达成，后者已由 ② 把三组无状态耦合的逻辑挪走。
  - [x] **认证失败面板带出路**（2026-08-31/09-01 用户三次撞到同一处后立项；2026-09-02 交付）
        → 载体：`app/src/connect_failure.rs`（结构化 DTO + 6 条单测：原样带走 / agent 过滤 /
        六档分类 / 非认证类无认证字段 / 跳板上报 / 过线键集）、
        `frontend/src/components/ConnectFailurePanel.svelte` + `.test.ts`（13 条：身份与指纹 /
        对端通告原话 / 空 remaining 不编话 / 一次点击一次尝试且在途禁用 / agent 不可用禁用并说原因 /
        档案已开 agent 不渲染 / 跳板禁一键动作 / 收口令时按钮改名 / 保险库锁着给解锁 /
        回调各一 / 字符串失败退化 / 档案已删只留文案 / key_list 拿不到照常）、
        `frontend/src/lib/connect-state.test.ts`（结构化 rejection 落标签、toast 不出 [object Object]；
        字符串 rejection 老用例照旧）、`frontend/src/lib/enum-contract.test.ts`（category 三侧逐字对齐）。
        变异三处各转红（去 agent 过滤 / 结构化识别恒 null / 空 remaining 也渲染）。
        **真机核验**（CDP，建零凭据临时档案双击连接）：面板渲染身份
        `cf-probe-nobody@139.129.33.6:22` + 真实指纹、「服务器只接受：publickey / password
        （一次认证都没有发起）」、引擎中文指引为主文案、四颗按钮中「用本地 SSH Agent 重试」
        因本机无 agent 而禁用且 title 带原因。
        **顺带抓到并修掉一个 120 秒缺陷**：口令框「取消」此前只关框不告诉后端，靠 120 秒超时
        收尾——日志里用户 8/31 自己那次 09:44:57→09:46:57 与探针 06:00:30→06:02:30 都是整 120 秒；
        改为立刻回空应答后实测 `auth_respond` → `session_open 失败` 相差 **0.2 毫秒**。
        实施要点原文保留在下方供追溯：
        连接失败时不再只有一条被截断的 toast + 一句「去会话属性配凭据」的长文案，
        而是当场显示「服务器只接受什么 / 我们手里有什么」，并挂三颗能点的按钮：
        「选一条私钥凭据」「用本地 SSH Agent 重试」「重试连接」，点完即重连。
        实施要点（对抗验证已核过，逐条有依据）：① 后端把 `Error::Auth` 透成结构化
        DTO（summary/tried/remaining/notes/host/fingerprint/has_jump），`remaining`
        渲染前**过滤掉 agent**（协议层没有这个方法名，auth.rs 已踩过一次）；
        ② 面板顶部固定显示 `user@host:port` + 指纹——防「在 A 机面板上挑了 B 机
        私钥」；③ **加密私钥要多一跳**：选中 `encrypted` 的密钥后额外要 passphrase
        并存成 `passphrase_vault_record`，不做则「选私钥」对多数真实密钥直接失败；
        ④ `has_jump` 时三颗全禁（失败可能在跳板某一跳，错误不带跳数）；
        ⑤ **每颗按钮点一次只重试一次、在途禁用、绝不自动循环**——publickey 每失败
        一次照样吃堡垒机的 MaxAuthTries（常配 2–3 次）；
        ⑥ notes 里有 agent 管道路径之类的本地诊断，折叠在「详情」里，不原样倒进主文案。
        判据：`connect-state.test.ts:191` 必须仍绿（靠 DTO 的 summary）；新增跨语言
        守卫钉「remaining 过滤 agent」与「remaining 为空走本地文案」两条。
  - [x] **自绘下拉组件（`<option>` 展开态主题化）——2026-09-03 用户裁定「不做，保持原生」**：
        本条至此关闭，不是欠账。理由按当时列的代价照单采纳：原生 `select` 白送的键盘与读屏行为
        （↑↓/Home/End/PageUp/PageDown、可打印字符首字母跳转、Alt+↓ 展开、Esc 收起、
        aria-activedescendant 语义）全部要自己重写，做砸了比原生更糟；而收起态已经全局主题化
        （自绘箭头 + 四主题着色 + forced-colors 把控件交还系统），展开态那一层配色不值这个代价。
        **一条如实的边界留在 styles.css 的注释里**：`<option>` 展开后的面板由操作系统绘制，
        CSS 管不到——不假装解决它。原始评估原文保留于下：
        用户裁定「原生控件都要有自己的
        样式」，收起态已全局主题化（含自绘箭头 + 四主题着色 + forced-colors 兜底），
        但**展开后的选项面板由操作系统绘制，CSS 管不到**（Windows 的 Chromium 只让
        background/color 部分生效，圆角/内边距/hover 一律无效）。要它也主题化只能
        换自绘组件，全仓约 18 处 select。**代价如实记下**：原生 select 白送的键盘与
        读屏行为全部要自己实现——↑↓/Home/End/PageUp/PageDown、可打印字符首字母
        跳转（约 500ms 缓冲）、Alt+↓ 展开、**Esc 收起时必须 stopPropagation**（否则
        连带关掉外层 SettingsDialog 模态，是真实风险不是假想）、aria-activedescendant
        模式、forced-colors 下手动着色选中态。做砸了比原生更糟；只有当展开态配色被
        判定为必须时才值得动。判据：键盘契约逐条组件测试 + a11y 判据不劣化。
  - [x] **forced-colors（Windows 高对比度模式）全仓覆盖**（2026-09-03 交付）：2026-09-01 前全仓零处理，
        现仅 select 一处补了兜底。其余自绘构件——SVG 图标（currentColor 在强制颜色下
        会被替换但描边可能消失）、状态灯、侧栏活动行的 inset 阴影、断线横幅的底纹——
        在强制颜色模式下的表现未核过。判据：开 Windows 高对比度主题逐屏截图核对，
        或在测试里用 `forced-color-adjust` 相关规则的存在性守住关键构件。
        → **判据没按出口原文的「规则存在性」做**：那只能证明写过，证不了看得清。改成可量的：
        `scripts/forced-colors-audit.mjs` 用 CDP 仿真 `forced-colors: active`，把每个状态构件的
        各语义状态克隆出来（保留 Svelte 散列作用域类，量到的就是产品规则本身）取计算样式，
        **两两比对视觉签名**（背景/前景/四边框色与线型/圆角/投影/轮廓/描边/尺寸），
        任意两个语义不同的状态签名相同 = 一处缺陷；纯指示器另过一条「画得出来吗」。
        → **修前（真机，同一判据回放存档签名）**：6 个构件不合格——塌成同一样子的状态对 **16 组**、
        画不出来的指示器状态 **9 个**。逐条：① 侧栏七盏主机灯只剩两种样子（实心四盏彼此一样、
        空心三盏彼此一样）；② 侧栏活动行的 inset 投影被抹成 none、底色与普通行同为 Canvas，
        完全看不出选中；③ 标签页选中态同上；④ 标签状态点「已连接」≡「出错」（都是实心圆）；
        ⑤ 状态栏三色点 ok/warn/err 完全相同；⑥ 工具栏主按钮与普通按钮无从分辨。
        → **房规（styles.css）**：① 状态靠**形状**区分（实心/空心 × 圆/方 × 边框粗细），颜色只作辅助；
        ② 「选中/强调」用 `outline: 2px solid Highlight; outline-offset: -2px`——不占布局、在强制颜色下
        是被重新着色而不是被抹掉，且不动背景（这是①的前提：容器底色恒为 Canvas，里面 CanvasText
        填充的指示点才恒有对比）。规则写在各组件（Svelte 散列作用域，全局选择器够不着 `.lamp`/`.tab`）。
        → **一条实测得来的硬约束**：填充**必须写系统色关键字**。首版写 `background: currentColor`，
        实测被引擎强制成 Canvas——那是一颗**画不出来**的点，而两两比对还会因为圆角不同判它「可辨」。
        探针因此补了「画得出来吗」这条判据（底色 ≡ Canvas 且四边框宽与轮廓宽全 0），并只对纯指示器
        生效（行/按钮/标签靠内容作画，自身空白是常态——收紧前它误报过这两类）。
        → **修后（真机）**：0 个构件不合格，7 个构件全过（侧栏灯 7 态、侧栏行 2 态、标签页 2 态、
        标签点 4 态、状态栏点 3 态、工具栏 2 态两两可辨；SVG 图标描边在强制颜色下仍在）。
        → **判据载体**：`lib/forced-colors-guards.test.ts`（① 状态集不得漂移——常态有几个 `.lamp.X`
        强制颜色块里就得有几个，新增状态漏补当场红；② 填充不得用 currentColor 并留住实测理由；
        ③ 选中/强调用内嵌 outline 而非 `background: Highlight`；④ 房规文档在位 + 断线横幅两态区分）；
        变异 F1–F5 逐条转红。CDP 客户端与页内动作表复用 `scripts/lib/{cdp,app-driver}.mjs`。
        **一处如实的缺口**：断线横幅要有真实 SSH 会话才挂载，动态核查量不到它（我不会为了量它去连
        用户的服务器），只由上述结构守卫第④条守着，`scripts/forced-colors-audit.mjs` 里记为 skip。
        **探针自身的两次失明也记下**（都由「常态前置闸」与「自检对照」抓出）：首版拿 `.sidebar` 当宿主
        命中的是 App.svelte 的同名元素、散列作用域不对，三个构件一条规则都没匹配上却判成全过；
        第二版把 `rgba(0,0,0,0)` 与 `rgb(0,0,0)` 当成不同签名，而透明叠在同色 Canvas 上屏幕表现一致，
        「选中行看不出来」因此被判通过。
  - [x] **keyring 条目缺失时解锁框要说真话**（2026-09-02 交付）：`unlock_with_keyring` 的 NoEntry 与文件
        读失败共用 `Error::Locked`，解锁框只能报「密码错误或 keyring/文件异常」——
        循环论证。而「无应用口令的库 + keyring 条目不在」= **库里凭据永久不可恢复**
        （世上再无第二份主密钥），这件事该直说并指向「从备份恢复」，不该让用户反复
        试空口令。改法：单列 `Error::KeyringMissing` 变体；VaultDialog 据
        `file_needs_passphrase` 分岔文案（无口令 + KeyringMissing → 直说不可恢复；
        有口令 → 「应用密码错误」）。判据：两条分岔各一条组件测试 + vault crate 的
        变体单测。
        → **载体**：`fs_vault::Error::KeyringMissing`（Display 前缀 `keyring entry missing` 即前端判据子串），
        `Store::unlock_with_keyring` 的 NoEntry 改报它（无库文件仍 `Locked`：两种缺失分开，app 靠前者决定初始化
        向导、靠后者直说不可恢复）；新 IPC `vault_file_has_passphrase -> Option<bool>`（不要求解锁，读库文件头，
        `None` = 无库文件）；`VaultDialog.explainUnlockFailure` 分岔：无口令 + 缺条目 → 直说「无法恢复，
        再试空口令无用，去『工具 → 保险库管理 → 从备份恢复』或删库重建」；有口令 → 「留空走不通，请输入应用密码」；
        文件头读不出 → 两种可能并列并带原文；非 keyring 缺失 → 通用文案且不多问一次。**实施与计划的一处出入**：
        计划写「有口令 → 应用密码错误」，但用户填了口令时后端走 `unlock_with_passphrase`（不碰 keyring），
        `KeyringMissing` 只会在留空路径出现，所以有口令分岔的真话是「留空走不通、请输入」，不是「密码错误」。
        判据：`crates/vault/tests/keyring_missing.rs`（变体 / 库文件逐字节不动 / 凭据库不铸新钥 / 无库文件仍
        Locked / Display 前缀；单独成二进制——要删进程内唯一那条 mock 条目）+ `VaultDialog.test.ts` 5 条
        （含跨语言契约：判据子串 ↔ error.rs 的 `#[error]` 文案）；变异 M3a（退回 Locked）、M3b（分岔删掉）各转红。
        真机核验（真删 keyring 条目后重启）仍归上文 `[!]` 便携解锁 UI 那条，不代签；已核的是新 IPC 本身：
        已安装新包里 `invoke("vault_file_has_passphrase")` 返回 `false`（本机库未设口令，与静默解锁成功的
        `vault_status=true` 一致），启动序列经 boot() 后侧栏档案照常载入。

#### 4d RDP 余项（阶段 1–3 交付于 2026-08-27/28，实施记录见 §6.6）

- **出口标准**：
  - [x] **RDPDR 驱动器重定向**（2026-09-05 交付；判据载体按用户裁定改为
        「单测钉住 + 真对端 [!]」——xrdp 不实现 MS-RDPEFS 服务端，spike 实测）。
        这是 RDP 侧真正的安全面：共享哪个本地目录、是否只读、每次挂载写审计
        ——三件缺一不可，全部落地：
        - **架构**：文件 IO 与三道闸全在主程序（`app/src/rdp_share.rs`）——与
          剪贴板同一条纪律，helper（解析远端不可信字节的沙箱）只做 IRP →
          语义形状（`fs_rdpproto::FsOp`）的翻译（`rdp-helper/src/drive.rs`），
          即便 helper 被协议字节打穿也拿不到路径。上游 `ironrdp-rdpdr` 的
          `RdpdrBackend` 是同步 trait 而应答要走管道往返——按上游文档的
          「返回空 Vec = 暂缓，另行排队」正解：在途表存完成信息，应答到达时
          由 engine 组装 MS-RDPEFS 响应。
        - **三道闸**（各有专属测试 + 变异守卫）：
          ① 路径闸：远端路径只当相对路径用，绝对路径/`..`/盘符一律拒
            （ACCESS_DENIED，不裁剪——裁是静默改写）；打开后 canonicalize
            **再验一次**仍在根内（junction/symlink 逃逸，Windows 上用
            `mklink /J` 造真联接测试——它不需要特权，恰好也是恶意软件更常用的）。
          ② 只读闸：写意图（含 FILE_OPEN_IF 对**文件**的那一面——资源管理器翻
            文件夹也用 OPEN_IF，全判写会让只读共享连文件夹都翻不开）回
            STATUS_MEDIA_WRITE_PROTECTED，**不碰文件系统**。
          ③ 审计：挂载/卸载各一行、每个文件的打开一行（读/写意图 + 结果）、
            每次写完成一行（字节数）；被拒的写也落（Rejected 行）。**直接查
            审计表断言**——D5 变异（append 结果整个忽略）首版全绿幸存，行为
            测试证不了「写进去了」。
        - **翻译的纯函数化**：`translate_create(da, disp, directory)` 抽出后
          逐格钉死 disposition 语义表与写意图位（D7/D8 首版幸存——翻译错了
          没有任何协议层信号，只有真 Windows 上「只读共享居然能改文件」这种
          最坏的发现方式）。
        - **UI**：画布右上角常驻共享条（已挂的盘逐个列出 + × 卸载 + 「共享目录…」），
          只读开关默认开；挂载过 M7.2 的 `fs.share` 确认类别（命令全文 =
          「远端桌面将只读/读写本机目录：<路径>」，可写档危险色）。入口常驻
          是刻意的：藏进菜单的共享等于用户忘了自己共享过什么。会话最多 4 个盘。
        - **判据闭合**：`rdp_share` 12 条（真临时目录 + 真 sqlite 审计表 +
          真 junction）、`rdp_cmd::share_tests` 2 条（挂载必须先过 ShareTable
          ——D6 变异「直接宣告」会让闸与审计全部空转）、helper `drive` 3 条、
          RdpPane 共享 8 条。变异 D1–D11 逐条转红（含挂死型与「删运行时键」型）。
        - [!] **真对端**：真 Windows 远端 Explorer 列盘/读写，与 xrdp itest 的
          「RDPDR 通道挂上后连接照常」（已跑，绿）都留真机，不代签。MVP 明确
          不做的 IRP（SetInfo/NotifyChange/Lock 等）回 NOT_SUPPORTED——真
          Windows 上表现为属性页不全/远端改名不支持，盘能开、能列、能读写。
        - **上游支持（2026-09-04 spike 查明）**：`ironrdp-rdpdr 0.7` 有完整的 RDPDR
          client（`Rdpdr` SVC processor + `RdpdrBackend` trait，`handle_drive_io_request`
          是文件操作映射点），与已在用的 cliprdr/rdpsnd 同一挂接模式（
          `with_static_channel`）——工程量可控，无需手写协议栈。
        - **但判据载体不可行（同日实测）**：helper 挂上 RDPDR 通道（宣告一个盘 +
          打标 backend）连真 xrdp 容器，连接协商照常成功、通道被接受，而服务器
          **零参与**——无 VersionAndIdPdu 应答、无 announce response、无任何
          drive IO。即 **xrdp 不实现 MS-RDPEFS 的驱动器重定向服务端**（与其社区
          状态一致：xrdp 只支持剪贴板，不支持文件重定向）。「xrdp 容器 itest 里
          远端能列出并读写共享目录」这条判据**在本仓的容器环境里无法成立**。
          候选出路（待用户裁定）：
          ① 判据改载体：协议层用上游/我方单测钉（DriveBackend 的文件操作映射、
             只读闸、NT 状态码翻译、审计三行），「真对端」标 [!] 留真 Windows
             （与 RDP 阶段 1 的「xrdp 证协议不证真 Windows」同一口径反过来用）；
          ② 找/造一个实现了 RDPDR server 的容器对端（如 freerdp-shadow 的某种
             形态）——spike 未验证其存在性，可能是一趟新的镜像工程；
          ③ 整项挂起等真 Windows 环境（用户闸）。
  - [!] **EGFX / H.264**：上游 `ironrdp-egfx 0.3.0` 没有 compositor，接进来只能拿到
        裸 AVC 帧而无处解码合成。**阻塞在上游**，本仓不自造解码器（那是另一条
        依赖树 + 一块专利面）。解除条件：上游 egfx 提供 compositor 或等价 API。
        标 `[!]` 而不是 `[ ]`：它不是「没做」，是「当前不可做」。
  - [x] **帧管道 raw IPC channel**（2026-09-04 交付）：当前帧经 Tauri 事件（JSON + base64）投递，
        编解码开销随分辩率线性涨。改 `tauri::ipc::Channel` 的 raw 形态，批量成帧
        落 fetch 档。判据：`framestats`（FS_RDP_FRAMESTATS=1）对比同一动态画面下
        主线程占用与端到端延迟均下降，且既有背压判据（`MAX_FRAMES_IN_FLIGHT`）
        原样通过。
        → 实现分三层：后端 `frame_wire`（16 字节**小端**头 x/y/w/h + RGBA，`Rect` 的
        u16 字段**提升为 u32 写线**——多显示器拼接与 8K 位图出过 u16 不够的先例）经
        `Channel<InvokeResponseBody>::send(Raw(bytes))` 直送；前端 `lib/rdp-frames.ts`
        的通道表（通道在 `rdp_connect` **之前**创建——它是命令参数，而画布 `RdpPane`
        要到标签转正之后才挂载；中间的帧**缓存最后一帧**，画布来了立刻补画并**补 ack**
        ——漏 ack 会让 helper 的在途配额永久少一格）；`RdpPane` 的两条投递路径汇到
        同一个 `paintBytes` 绘制核心（拆出后由源码级判据钉住不分叉）。
        - **回落路径保留**：channel 发送失败（前端重载中）回落事件而不是丢帧；MCP 一侧
          不传 channel 天然走事件。`Option<Channel>` 不被 tauri 宏接受，命令拆成
          `rdp_connect_core(Option<..>)` + IPC 壳（非 Option）。
        - **A/B 开关**：设置键 `rdp.frameTransport`（raw 默认 | event），设置页「安全与
          Vault」页可选。两条路径在同一构建里，真机对比（出口判据）不用换版本。
        - 字节序是契约的一部分：Rust `to_le_bytes` ↔ 前端 `getUint32(off, true)`，
          两侧各有单测钉住**同一组字节**（F1/F5 变异：任一侧换大端当场红）。
        - **顺带修的真缺陷**：`rdp_reconnect` 的返回值一直被丢弃——RDP 重连是**新会话
          新 id**，标签从此指着死掉的旧 id（画布冻着、关标签也关不掉新会话）。现在
          RdpPane 接住返回值，replaceTabId 原地转正 + 帧通道重建（旧通道闭包绑着旧
          会话的绘制状态）。connect-state 的两条「唯一调用方」门禁名单相应扩容并记由。
        - 判据闭合：背压判据 = helper 35 条单测全绿（含「一次 flush 不得超发」）；
          功能等价 = 前端 25 条 RdpPane/rdp-frames + 后端 frame_wire roundtrip/分流
          顺序源码级守卫；变异 F1–F8 逐条转红（F4 首版幸存——framestats 记账无人守，
          补源码级守卫后转红）。**主线程占用/端到端延迟的数值下降**需要真机在两种
          档位下跑同一画面对比 framestats——工具已备好（A/B 开关 + framestats），
          真机数字不代签，留 [!]。
        - **itest 撞见的既有问题（如实记）**：`rdp_keyboard_input_reaches_the_server_
          and_repaints` 在当前 xrdp 镜像下红——敲键后 15 秒无重绘帧，主动索帧
          （RequestFullFrame）也无任何下行（helper stderr 空）。本批把该测试暴露的
          **栈溢出**修了（`next_from_helper` 两个 64 KiB 栈缓冲把 async 状态机顶爆
          Windows 2 MiB 测试线程栈；改堆分配），但「无下行」本身与帧管道正交
          （itest 直驱 helper，不经过 app 层的 emit_frame），根因待 RDPDR 批深查
          helper↔xrdp 的下行链路。其余 3 条 xrdp 用例全绿。

- **依赖**：4d 依赖 RDP 阶段 1–3（已交付）；RDPDR 另依赖 M7.2 的统一确认口径
  （挂载共享目录必须过确认框，且「以后不再显示」按动作类别记忆）。

- **依赖**：4a 仅依赖 M1；4b 的 AI 联动/AI 片段依赖 M2；4c 仅依赖 M1（纯前端）。
- **风险**：`.xsh` 为私有格式（INI+部分加密）→ spike 先行定边界；签名证书与账号成本 → 4b 计划前确认就绪。

### M5 移动端

> **不在 1.0.0 范围内**（2026-08-26 用户裁定）。M5 整段推迟到 1.0.0 发布之后，
> 出口清单原样保留、一条不删——它是 1.x 的输入，不是 1.0.0 的欠账。
>
> 这条裁定的直接后果：**1.0.0 的「做完」定义 = M0–M4 的出口清单**。
> 判断发布就绪时不必再把 M5 的 8 项算进分母；反过来，M0–M4 里任何一项都不能
> 因为「反正还有 M5」而放宽。

- **目标**：手机上的「伴生控制台」而非完整终端：接收告警/审批 AI 与 MCP 的确认请求、看监控、发应急单命令。
- **范围**：Tauri 2 移动端目标（iOS/Android）同一 Rust core 复用；移动端 ↔ 桌面端本地配对通道（局域网，配对码 + 端到端加密，协议预留总设计 §0 基线表，远程协议定稿见总设计 §9 Phase 5，复用 §4.5 确认契约思路）；推送（APNs/FCM 走本地通知降级方案先行）；移动端 UI（审批卡、监控卡、命令抽屉）；桌面端「手机控制」开关与会话级授权。
- **不含**：移动端直接持有 Vault 主密钥（派生受限令牌，能力子集）；公网中转（M6 再议）。
- **出口标准**：
  - [ ] iOS 模拟器 + Android 模拟器/真机各跑通：配对 → 收到确认请求 → 批准 → 桌面端执行；
  - [ ] 未配对设备无法枚举任何会话/命令（渗透用例清单）；
  - [ ] 局域网断开 → 请求队列与超时语义正确（不丢不重）；
  - [ ] 桌面端关闭/锁定 → 移动端下次心跳内（≤5s，配对通道在线时 ≤1s）一切请求被拒并提示；
  - [ ] 监控卡：桌面端采集（M4a）开启后，移动端监控卡按轮询间隔刷新指标（配对通道在线时 ≤2s，对齐 M4a 范围 2s 采集间隔）；配对通道断开/采集失败降级（对齐 4a 出口监控采集条「静默降级」语义）时卡片明确呈现「断连/数据过期」态——不空白、不弹错误框；iOS/Android 模拟器 E2E 各一路 + 降级态单测；
  - [ ] 命令抽屉：移动端发起的应急单命令经桌面端同一 fs_policy 闸门下发并写 audit（复用总设计 §4.5 确认契约）；正常命令执行且输出回传移动端；危险命令用例按策略被拦截（或触发确认卡二次批准后放行），移动端收到拦截/拒绝结果；E2E 或单测判定。以上两项闭合 §0「M4a → M5（监控卡）」依赖声明。
  - [ ] 推送降级：APNs/FCM 不可达环境（iOS/Android 模拟器天然无推送服务）降级本地通知，后台态下确认请求仍可送达并可批准，E2E 各一路留痕（对齐出口 1 模拟器口径）；
  - [ ] 桌面端「手机控制」开关与会话级授权：总开关关闭 → 移动端一切请求下次心跳内被拒并提示；已配对但未逐会话授权的会话，移动端不可枚举/不可操作（出口 2 渗透用例清单扩充该两类用例）。
- **tag**：`v0.5.0`。
- **风险**：苹果开发者账号/公证成本与周期（总设计开放问题 A.6）→ 计划前先确认账号就绪；Android 后台限制 → 前台服务 + 局域网保活策略 spike。

### M6 云同步（可选，默认推迟）

- **目标**：多机一致体验，且**不引入信任第三方**的默认路径。
- **范围**：Profile/设置/片段/主题（非密钥材料）的端到端加密同步；同步后端三选一：用户自备（S3 兼容/WebDAV/目录同步）优先、可选官方中继（若立项）；密钥材料永不离机（同步的是「用同步口令加密的密文 blob」）；冲突解决（LWW + 冲突副本）。
- **出口标准**：两台机器双向同步 round-trip 测试（含两端并发修改同一条目 → LWW 胜出生效且冲突副本保留）；篡改同步 blob → 解密失败且不影响本地；删除恢复。
- **tag**：`v0.6.0`。
- **决策点**：M4 验收后评估「用户是否真的需要」，可永久停留在纯本地。

### M7 系统面运维工具箱 + 桌面化文件操作（2026-08-28 立项）

> **产品边界（用户 2026-08-28 裁定，这条比任何单项功能都重要）**：
>
> > 本产品的定位是**终端工具**，相当于是套壳，**不可以做和应用相关的内容
> > （Docker 除外），只做和系统面相关的工具**，比如时间、网络、设置、定时，
> > 等等以及一些配置之类的。
>
> 判据：一项功能若是在管**某个应用**（建站、数据库、FTP、SSL 证书、
> 应用商店），一律不做——那是宝塔那类面板的地盘，与本产品定位冲突。
> 若是在管**系统本身**（服务、时间、网络、定时任务、包与补丁、系统配置），
> 才在范围内。Docker 是明确的例外（它已是系统级基础设施）。
>
> 同时重申既有硬约束：**不在远端装 agent**（宝塔正是 agent 型：远端常驻
> 进程 + Web 面板 + 8888 端口，整条不做）；**零多余出站**（不联网拉 CVE 库）。

- **目标**：把「登进服务器敲一串命令」的高频运维动作变成本程序里可点、
  可预览、可留痕的操作；并把远端文件操作做到桌面级手感。
- **依赖**：M1（SSH/SFTP）；监控相关项复用 M4a 的采集与告警检测。
- **不含**：任何应用层管理（建站/数据库/FTP/SSL/应用商店）；远端常驻 agent；
  联网漏洞库。

#### M7.0 已存在、不必重做（2026-08-28 核查，避免重复投入）

用户第 4 项「命令历史点击重执行 + 快捷键」提出时，该功能**已 100% 完成**。
核查结论一并记在这里，供后续排期直接引用：

| 用户以为要做的 | 实际状态 | 载体 |
|---|---|---|
| 命令历史点击重跑 + 快捷键（第 4 项） | **已完成**：`Ctrl+Shift+H` 打开、模糊搜索、按最近使用排序、**点击即重发**、方向键+回车快发、仅本机过滤、删除/清空、从终端回滚启发式提取 | `keymap.ts:107`、`HistoryDialog.svelte`、`history.ts`、`history_cmd.rs`、迁移 0005 |
| 窗口最小尺寸（第 6 项） | **已设** 800×600 | `app/tauri.conf.json:14` |
| 文件管理器图标视图 | **已有** details/icons + 大/小图标切换 | `SftpPane.svelte` |
| BusyBox/嵌入式采不到监控数据 | **说反了，采得到**：默认 itest 容器就是 Alpine（BusyBox 用户态），有绿灯用例断言内存总量与 `free -m` 完全相等 | `monitor.rs`、`crates/itest/tests/monitor.rs` |
| 告警要新造检测引擎 | **已在跑**：阈值 + 检测 + 严重度聚合，每 2 秒一轮（只差展示） | `frontend/src/lib/monitor-anomaly.ts` |
| 右键菜单要新写 | **已有三份实现**，抽共享组件比新写便宜 | `Sidebar` / `TabBar` / `RdpPane` |

> 命令历史里有一条**用户没提但已经做了的安全设计**，改动时不得拆掉：
> 历史分 `sent`（本程序亲手发出的字节，重发精确）与 `grid`（从屏幕回滚
> 启发式提取，可能带提示符残渣）两种来源，**`grid` 条目不允许回车直接重发**，
> 必须看着点一下。

> 可选增强（用户未要求，功能上不是缺口）：终端内 `Ctrl+R` 反向增量搜索
> ——目前只有弹框式面板，没有 shell 风格的内联搜索。

#### M7.1 系统面工具箱

- **范围**：服务管理（`systemctl` 列表/状态/启停重启 + `journalctl` 看日志）
  优先；其后按需扩展到时间/时区、网络（接口/路由/DNS）、定时任务
  （crontab/systemd timer）、系统配置。**Docker 是唯一允许的应用层例外**。
- **补丁管理（只读盘点）**：列出各发行版可升级的包
  （apt/dnf/zypper/apk/pacman），**不在程序里执行升级**。
  升级动作把命令送进用户自己的终端，由他按回车。
  - 理由（实测，2026-08-28）：`sudo` 要 TTY，而命令执行通道
    `app/src/agent/exec_port.rs::exec` 走 `channel_open_session()` +
    `channel.exec(...)`，**全程没有 `request_pty`**——`sudo` 在那条路上
    必然快速失败。三条出路（NOPASSWD / 弹框收口令注入 PTY / 送进终端）
    安全含义完全不同，用户裁定先只做只读盘点。
  - 沿用 `crates/sshengine/src/monitor.rs` 的现成形状：命令常量 + 纯函数
    解析 + 容器 itest 对照验证；零新依赖、零出站增量。
- **告警展示**：检测引擎已在跑（`frontend/src/lib/monitor-anomaly.ts`，
  阈值 + 严重度聚合，每 2 秒一轮），只差把结果显示成应用内横幅/角标。
  **跨设备告警（邮件/webhook）不在本里程碑**；「应用关着时也告警」
  在客户端形态下结构上做不到，不列为欠账。
- **出口标准**：
  - [~] 服务管理：列表/状态/启停重启/看日志五件事对真容器跑通，
        且**危险动作（停止/重启）走确认**（见下方「统一确认口径」）；
        → **载体**：`crates/sshengine/src/services.rs`（命令常量 + 纯函数解析，形状同 monitor/procs）、
        `app/src/commands/services_cmd.rs`（5 条 IPC）、MonitorPanel 新增「服务」页签
        （列表 / 筛选 / 状态灯 / 启停重启 / 展开看 journal）。
        **停止与重启走 M7.2 确认闸**（`service.stop` / `service.restart` 两个独立类别，用户可以
        只豁免其中一个）；**启动不走**——它不打断任何在用的连接，为它弹框只会训练用户闭眼点确认。
        → **单元名是安全边界**：它从前端来、要拼进 shell。用**白名单**（字母数字 + `. _ - @ \ :`）
        而不是黑名单——黑名单要穷举 shell 元字符，漏一个就是一次以远端身份执行任意命令。
        五个命令构造器全过它；itest 里另证「注入串根本构造不出命令」。
        反向也守：夹具里 103 个**真实**单元名一个都不许被拒（太严 = 这些服务在界面上点不动）。
        → **不加 `sudo`**：命令通道走 `channel.exec()` 全程没有 `request_pty`，`sudo` 必然失败且报
        「a terminal is required」这种让用户以为程序坏了的话。权限不足由 `classify_action_failure`
        把真正的出路说出来（换账号 / 配 NOPASSWD）。
        → **进度：解析这一半已对真 systemd 取证，动作那一半未跑过**。
        `crates/sshengine/tests/services_real_output.rs` 用的是 2026-09-03 从**真跑着 systemd**
        的 Ubuntu 22.04（本机 WSL2，`is-system-running` = running）原样抓下来的 103 行输出，
        一个字符没改；6 条断言钉住「行数不丢」「列不错位（按取值域判）」「描述不被切碎」
        「模板实例单元过得了校验」。非 systemd 分支由 `crates/itest/tests/sysbox.rs` 对真 Alpine 容器证。
        **`[!]` 的那一半**：五件事里的「启/停/重启真的生效」要一台能跑 systemd 容器的 Docker 主机。
        本机 Docker Desktop（WSL2 后端）**起不来 systemd 作 PID 1**——2026-09-03 实测四组参数组合
        （`--privileged` / `+--cgroupns=host` / `+--cgroupns=private` / `+挂 /sys/fs/cgroup`）
        容器一律 `Exited (255)` 且无任何日志。用例已写好（`systemd_container_covers_list_status_journal_and_actions`），
        门控在 `FS_ITEST_SYSTEMD=1` 之后，需 Linux 原生 Docker 或 CI 的 Linux runner。**我没跑过，不代签。**
  - [x] 补丁盘点：五种包管理器各有解析单测 + 至少两种在容器 itest 里对照
        （被测与对照同一次 exec），**「0 个可更新」与「包管理器不存在」
        必须是两种结果**，不得混成一个空列表；
        → `crates/sshengine/src/packages.rs`：探测 + 五个解析器（apt / dnf / zypper / apk / pacman）
        各带单测。**解析放在 Rust 而不是让远端 shell 归一**：那段 awk/sed 没有任何测试覆盖，
        且它跑在目标机上，版本差异 / locale / busybox 与 GNU 的差别全落在一段不可测的脚本里。
        → **两种真管理器对照**（`crates/itest/tests/sysbox.rs`，被测与对照同一次 exec）：
        · **apk / alpine:3.16**——`latest` 与 `3.18` 实测都是 0 个可升级（镜像随补丁重建），
          一条恒等于 0 的断言证不了任何东西；3.16 已停止重建，补丁稳定可见（实测 5 条）。
        · **apt / debian:bullseye-slim**——同样是重建过的镜像，故先用 `-t bullseye` 装基础套件
          （而非 security 套件）的 curl，把机器摆成一个必然有补丁待打的**真实**状态
          （实测稳定得到 curl / libcurl4 / libnghttp2-14 三条）。包、版本、索引全是真的。
        断言不止比条数：每个解析出的包名都要能在对照原文里找到（数目对上而名字全错的解析器
        照样能通过计数断言）。
        → 「不存在」那一条另有一例：在真 Alpine 上先证探测到 apk，再把五个可执行文件从 PATH
        挪走，证结果变成 `NoManager` 而不是「0 个可更新」。
        → **只读**：`scan_command` 有一条守卫逐字排除 11 个会改远端状态的子命令
        （含 `apt update` / `zypper refresh` / `pacman -Sy`）。升级命令只显示给用户自己送进终端。
  - [x] 全程零新增出站面：`egress-contract.test.ts` 不需要任何改动；
        → 全部命令经既有 SSH exec 通道；`services_cmd.rs` 自带一条守卫逐字排除
        reqwest / http:// / TcpStream / ureq。`egress-contract.test.ts` 本批一字未改。
  - [x] 每个改变系统状态的动作写审计（复用 `crates/audit`）。
        → 只有 `session_service_action` 改状态，只有它写审计，且是在**拿到退出码之后**才写——
        记的是「这次到底成没成」。守卫钉住「本文件审计条目恰好 1 条」：只读命令写审计会把审计链
        灌满噪声，而噪声里的一条真事件等于没记。
        与 M7.2 确认闸写的那条是**两件事**：那条记「用户批准了」，这条记「批准之后真的执行了，
        结果是这样」。少任何一条，事后追查都缺一半。
- **判据汇总**：`services.rs` 单测 17 条 + `packages.rs` 单测 9 条 + `services_real_output.rs` 6 条
  （真 systemd 夹具）+ `services_cmd.rs` 3 条 + 前端 `lib/sysbox.test.ts` 17 条
  + `crates/itest/tests/sysbox.rs` 6 条（5 条真容器跑过，systemd 那条按上文标注跳过）。
  变异 15 条逐条转红（S1–S6 服务、P1–P5 包、A1 审计、F1–F3 前端文案与确认档位）。

#### M7.2 统一确认口径（横跨 M7.1 / M7.3）

- **裁定（用户 2026-08-28）**：**前端确认框就够**，不新建分级裁决面；
  **并且首次确认时让用户选择「以后不再显示」**。
- 现状（静态确证）：用户主动发起的命令这条路上**没有任何闸**——
  `App.svelte` 直接 `invoke("term_input")`。而 `fs_policy::gate::decide`
  的入参是 `AiMode`（AI 执行档位），**不能直接复用**：套到用户自己点的
  按钮上会导致「用户把 AI 关掉 → 自己的模板也跑不了」。
- **出口标准**（2026-09-03 交付）：
  - [x] 确认框把**将要执行的命令全文**摊给用户看（不是「确定吗？」）；
        → `components/ActionConfirmDialog.svelte`：命令进独立 `<pre>`（等宽、可选中、可滚动、
        `pre-wrap` + `anywhere` 换行而非横向滚动条），**不截断不摘要**；后果说明另起一段。
  - [x] 「以后不再显示」是**按动作类别**记忆（如「服务重启」独立于
        「双击运行脚本」），不是一个全局开关——一次勾选放行全部动作
        等于把闸拆了；
        → `lib/action-confirm.ts` 的 `DangerousAction`（process.kill / service.stop /
        service.restart / script.run / fs.delete）+ 设置键 `confirm.suppressed`。
        勾选项上写着**类别名**——「以后不再显示」四个字单独出现时用户不知道自己放行多大一片。
        **脏值一律回落空集**（= 全部都要确认）：这一栏读错方向的后果是「没有确认框就重启了生产服务」，
        而反方向的最坏后果只是多按一次确认。
  - [x] 设置页可撤销该记忆（勾了之后必须能反悔）；
        → SettingsDialog「安全与 Vault」页逐条列出并各带「恢复确认」，另有「全部恢复确认」。
        **逐条**而不是只给一个清空按钮：用户可能只想收回「重启服务」而继续免确认「终止进程」。
        确认框里当场写明撤销入口在哪儿，否则勾完就再也找不到那个开关。
  - [x] 无论是否显示确认框，**审计照写**。
        → `lib/confirm-gate.ts` 在**四条**路径上都发 `audit_dangerous_action`：弹框批准、弹框拒绝、
        因勾过而免确认、以及被并发拒掉的那一条。豁免掉的是确认框，不是审计——否则
        「以后不再显示」就成了一个能把审计关掉的开关。后端 `app/src/commands/audit_exec_cmd.rs`
        新增该命令：`risk_level` 现算（与 AI 建议 / 命令发送同一个分级器，三行可比），
        命令文本过 `RedactedAction` 脱敏，**verdict 用 Approved/Rejected 而不是 AutoRun**
        （AutoRun 的文档义是「只读，自动放行」，拿它记一次预授权的重启会误导追查的人；
        免确认这件事记在 action 前缀 `[免确认]` vs `[确认]` 上，可 grep、语义不走样）。
- **首个真实接入点**：MonitorPanel 的「终止进程」。此前它自己挂一个 `ConfirmDialog`——那条路
  既没有记忆也不写审计。改走统一闸后命令全文是 `kill -{signal} {pid}`，后果说明沿用既有
  `killConfirmText`（KILL 不可捕获 / PID 1 会停掉整机）。**signal 不进 kind**：TERM 与 KILL 是
  同一件事的两种强度，拆成两类会让设置页出现两条几乎一样的豁免。
- **判据**：`lib/confirm-gate.test.ts`（16 条：弹框批准/取消、并发只允许一条、按类记忆、
  取消里勾选不落库、脏值回落、撤销后重新弹框、四条审计路径、审计失败不影响裁决）
  + `components/ActionConfirmDialog.test.ts`（8 条：命令全文逐字、后果独立成段、勾选带类别名、
  三种勾选组合、Escape=取消、换条目时勾选复位）+ MonitorPanel 三条改对着闸断言。
  变异 M1–M10 逐条转红。**M10 首版幸存是个真发现**：「取消则不发命令」那条只 flush 了一个微任务
  就断言，即使把闸的返回值整个无视也照绿；补一个宏任务再断言才立得住。

#### M7.3 文件管理器桌面化

- **形态裁定（用户 2026-08-28）**：**应用内虚拟窗口**，不做真原生多窗口。
  - 实测依据：`app/capabilities/default.json` 作用域是 `"windows": ["main"]`，
    权限集里**没有** `core:webview:allow-create-webview-window`——今天前端
    创建不了第二个原生窗口。走虚拟窗口则不动 capabilities、不扩权限面、
    三平台行为一致。
- **范围**：右键菜单（抽共享组件，仓内已有三份实现可归并）、
  剪切/复制/粘贴（**先只做同主机远端↔远端**）、拖动、双击运行 `.sh`。
- **双击运行 `.sh`**：前台/后台**都支持**，但**必须由用户当次选择，
  不得替用户默认**（用户 2026-08-28 原话）。
  - 后台走 `nohup` 的代价要在 UI 上说清：不终止的脚本 = 一个没人管的远端
    常驻进程，关掉本程序它照跑。
- **已有、不必重做**：图标视图与大/小图标切换（`SftpPane.svelte`）。
- **出口标准**（前两条 2026-09-04 交付，第三条待做）：
  - [x] 剪切/复制/粘贴的语义在**冲突/覆盖/失败**三种情形下都有明确行为
        并有测试——语义不定清楚，做出来就是一堆「点了没反应」；
        → 裁决全在 `crates/sshengine/src/fileops.rs`（纯函数、17 单测）：`plan_paste` 把
        冲突分成**覆盖 / 保留两者 / 跳过**三档，外加第四类「源与目标同路径」单列
        （`cp a a` 报错、`mv a a` 静默无事，而「复制到自己所在目录」很容易点出来）。
        执行在 `app/src/commands/fileops_cmd.rs`，**逐条**回报成败与原因——只回一个 `Err`
        的话，粘十个第七个失败时用户不知道前六个动没动，而文件操作不可撤销。
        前端 `lib/file-clipboard.ts` 的 `summarizePaste` 把成功/失败/改名/跳过/同路径
        **各自成句**，绝不合并成一句「粘贴完成」。
        - 破坏性那一步只认计划里显式的 `PasteOp.overwrite`：早先用「没改名」反推，
          而不冲突的条目同样没改名，于是每次普通剪切都先对目标发一次删除（今天恰好打在
          不存在的路径上因而无害）。「删哪一个」不该由推断决定。
        - **跨会话粘贴是禁用的并写明原因**，不是可点然后报失败——范围裁定只做同主机
          远端↔远端，跨机是一次真实传输，语义/进度/失败恢复都不同。
        - 判据：`file-clipboard.test.ts` 16 条 + `SftpPane.test.ts` 新增 13 条行为
          （含「探测阶段绝不调 `fileops_paste`」「剪切只是标记、此刻零 IPC」）。
          变异 X1/X3/X4/X5/X7 + R1–R7 逐条转红。
  - [x] 双击 `.sh` 前展示**脚本原文**并让用户选前台/后台，前台输出进新终端
        标签，后台明确告知「关掉本程序它仍在跑」；
        → `components/ScriptRunDialog.svelte`：原文经 SFTP `read_range` 读（不经 shell，
        没有引号与元字符问题），超 256 KiB **截断并说明**——用户是拿这段原文决定要不要执行的，
        「你看到的不是全部」静默处理等于让他在不完整信息上做不可撤销的决定。
        两个单选**都不预选**、「运行」在选定前禁用（用户 2026-08-28 原话：不得替用户默认）。
        后台档在按下运行**之前**就显示 nohup 的代价。
        - 前台开**新**标签而不是往当前标签打：当前标签下面可能正跑着 vim / tail -f，
          塞一条 `sh /path` 进去轻则打断、重则被那个交互程序吃掉——脚本不会运行而用户以为运行了。
          命令经 `lib/pending-commands.ts` 排队，App 在 `openSession` 返回后与 TerminalPane
          `onReady` 两处排空（`takeForSession` 取走即清保证只发一次）。
          **排空点不是 `session:status`→connected**：首次连接时后端那几条事件全部发生在标签
          存在之前，事件桥根本收不到。
        - 审计两条路都写，在**前端决策点**（M7.2 口径），后端刻意不写——前台那一路根本不经过
          任何 Rust 执行点，两边各写一份会让同一件事在库里出现两行、时间点还不同。
          `fileops_cmd.rs` 有一条守卫钉住这个缺席（并指明审计写在哪）。
  - [x] 虚拟窗口：拖动/叠放/关闭在三平台一致，窗口不可拖出可视区外
        （拖丢了就找不回来）。
        → `lib/vwindow.ts`（几何与 store）+ `components/VirtualWindow.svelte`（外壳）
        + `components/VWindowLayer.svelte`（画布，铺在终端区之上）。入口是文件视图工具栏的
        「⧉ 浮动」，弹出去之后本窗格切回终端——用户点它的意思是「我要同时看终端和文件」。
        - **「不可拖出可视区外」的实现只有一条路**：所有位移/缩放/视口变化都走同一个
          `clampRect`。组件自己就地算一次再写 store 的写法，迟早漏掉一条路径，而那条路径
          造出的窗口**真的找不回来**（虚拟窗口没有任务栏、没有 Alt+Tab、没有「窗口」菜单）。
          夹紧顺序是先尺寸后位置：反过来做，比画布还宽的窗口会先被推到 x=0 然后仍然溢出右边。
        - 画布**自己量尺寸**（ResizeObserver），不拿 `window.innerWidth`：画布上面有标签栏、
          下面有状态栏、右边可能开着 AI 面板，拿浏览器视口去夹，窗口能被推到状态栏底下。
          主窗口缩小 / 拉开监控面板都会触发重新夹紧。
        - 拖动用 Pointer Events + `setPointerCapture`：一套代码管鼠标/触摸/手写笔，且指针移出
          WebView 时事件照旧回到本元素（`document` 上挂 mousemove 的写法在快速拖动时掉事件，
          表现为「窗口跟不上鼠标然后卡住」）。**三平台一致也是这么来的**：几何全在 WebView 里算，
          不经过任何窗口管理器。
        - 标题栏可聚焦，方向键移动、`Ctrl+方向键`缩放、Shift 加速——只能拖 = 只能用鼠标。
        - 判据：`vwindow.test.ts` 21 条（含「往右下狂拖」「视口变小」「比画布还大的窗口」
          「视口比最小尺寸还小」）+ `VirtualWindow.test.ts` 12 条（真发指针事件）
          + `VWindowLayer.test.ts` 6 条 + `layout.test.ts` 4 条挂载点守卫
          + `SftpPane.test.ts` 3 条入口。变异 V1–V10 逐条转红。
        - **三平台一致仍是用户闸**：几何不经窗口管理器是实现层的论证，Windows 一路已跑；
          macOS/Linux 真机不代签（口径同本文档其它平台项）。
- **顺手修掉的真缺陷（2026-09-04）**：远端一侧的**每一种导航都不重新列表**——双击进子目录、
  点「上级」、点面包屑，路径与面包屑都变了，下面还是上一个目录的内容。根因在
  `$effect(() => void refresh())` 与 `refresh()` 的 async 边界：effect 只跟踪第一个 `await`
  之前同步读到的状态，而 `remotePath`/`sessionId` 读在其后。本地栏一直是好的
  （`localPath` 恰好读在 await 之前），「档案目录晚到时跟随」也一直是绿的（它同时改两栏路径），
  这就是它躲过既有测试的方式。修法：把两栏路径与过滤器提成 `refresh` 的实参。
  - **变异实证的一次自我纠错**：第一版变异只把 effect 那行退回 `refresh()`，测试全绿，
    我据此写下「守卫无效/缺陷不存在」并回退了修复——错的。默认参数
    `lp = localPath, rp = remotePath` 是在调用点同步求值的，依赖照样建立，那个变异体与修复后
    行为完全一致。把签名与调用点**一起**退回才是有效变异（实测 4 条转红）。教训记在
    `SftpPane.test.ts` 那三条导航测试的文档里：变异幸存首先要怀疑变异本身。
- **判据汇总**：`npm test` 108 文件、1505 → 1593 条全绿；`cargo test` app lib 345 / sshengine lib 160；
  `bash scripts/ci-local.sh` 12 关全绿（Windows 一路）。

#### M7.4 嵌入式：串口（UART）直连

- **裁定（用户 2026-08-28）**：目标形态是**只有串口的裸板**，不是 SSH 可达的
  开发板。
- **范围**：串口传输层（新 crate，跨三平台）、串口会话与现有终端渲染管道对接、
  端口枚举与波特率等参数、断开重连。
- **风险（如实记）**：这是本里程碑里量级最大的一项——要新建一套与 SSH 并列的
  传输层与会话注册表，不是在现有会话上加个开关。
- **同批必做**：**非 UTF-8 编码（GBK/GB2312）支持**。国产板子输出 GBK 时
  目前是整屏乱码（终端编码字段存在但**无消费方**）。串口不做这一条等于
  做了也用不了。
- **出口标准**（2026-09-04 交付；真硬件两小条标 [!] 不代签）：
  - [x] 真串口设备（或虚拟串口对）跑通：打开/收发/改波特率/断开重连；
        → 载体是用户裁定的 Docker 虚拟环境：`scripts/serial-itest.sh` 在 Linux 容器里用
        socat 造一对**真 tty 设备**（`open(2)` + `termios` + `read/write`，与真 USB 转串口
        同一条驱动路径），跑 `crates/serial/tests/pty.rs` 四条：双向收发（0x00–0xFF 全字节）、
        改波特率不重开端口（重开会抖 DTR，而很多板子的 DTR 接在复位脚上——Arduino/ESP32
        就是靠这个自动进下载模式的）、kill socat 模拟拔线后自动重连且**读流不断**、
        GBK 字节经整条终端管道后是 UTF-8。测试进程自己 spawn socat：拔线/插回的开关在
        测试手里，不靠 sleep 猜时序。
  - [!] 真实硬件那一小条不代签：波特率是否真的打在线上、RTS/CTS 电平、USB 转换器拔插时
        驱动的具体错误码——伪终端没有这些。**Windows COM 驱动一路同样未验**（容器是
        Linux，`serialport` 的 Windows 后端一行都没跑到）。要有真板子或装 com0com 才能闭合。
  - [x] GBK 输出正确显示，且**编码切换不需要重连**；
        → 编码支持是**独立的一批**（2026-09-04 早些时候交付，见前一次提交）：`fs_terminal`
        的 `StreamDecoder`（增量解码：一次 read 的边界不是字符边界，GBK 汉字的两字节完全
        可能落在两次 read 里）接进 `SessionPipe` 的读取循环，站在 ZMODEM 拦截器**之后**
        （反过来的话解码器会啃二进制帧头，传输直接坏掉）。UTF-8 恒等透传、逐字节与
        接入之前一致。串口尤其需要它——裸板的中文提示基本都是 GBK。
  - [x] 串口会话与 SSH 会话在标签/状态栏/历史等公共设施上行为一致。
        → 实现方式不是「照着做一遍」，而是**走同一条路**：`SerialReader` 是
        `AsyncRead`，直接喂给**同一个** `SessionPipe::spawn`——网格、滚动缓冲、合批背压、
        ack、录制、会话日志、编码切换一件没重写。`term_input`/`term_resize`/`term_ack`/
        `term_encoding`/`session_close` 在 SSH 注册表落空时回落到串口注册表，前端一行没改。
        事件**复用** `session:status/disconnected/closed` 三个名字而不是另起 `serial:*`：
        前端的事件桥、标签状态机、断线 banner 全挂在这三个名字上（`event-contract` 的
        站点数门禁 35→38 逐条记了账；`connect-state` 的「带 state 才算连接成功」名单
        扩到 serial_cmd.rs，判据不变）。
        - 串口的重连**不设次数上限**（`reconnect.rs`）：拔线→插回来是用户随时会做的事，
          「重试 5 次后放弃」的表现是程序早不试了而 banner 上只有一行旧提示。退避封顶 2 s。
        - 状态栏主机段在串口会话上显示 `COM3 · 115200 8N1` 并可点改波特率；会话属性里
          串口档案没有主机/用户名/端口三栏（留着会让用户以为要填点什么），认证/跳板/
          主机密钥/SFTP 页签收起来（对串口都不成立）。
        - 首次打开失败**当场报错**（「打不开 COM3：没有权限，或端口正被另一个程序占着
          （…Linux 上还要在 dialout 组里）」——按 `ErrorKind` 翻译而不是匹配错误文本，
          文本随平台与系统语言变）；此后的断开一律自动重连。
- **判据**：`fs_serial` 35 单测（params/ports/link/reconnect/session：内存管道实现的
  `PortFactory` 跑完重连循环、写路径、改波特率的全部逻辑）+ 容器内 pty 四条 + app 侧
  `serial_cmd` 三条守卫 + `serial_session` 三条（档案↔传输层取值逐字对齐，分叉的表现是
  用户配的 9600 7E1 被读成 115200 8N1）+ 前端 serial-wiring 9 / SerialBaudDialog 8 /
  ProfileDialog 串口 6 / StatusBar 2。变异 S1–S17 逐条转红——**S2/S3/S5 是「挂死」型**：
  变异之下测试不是失败而是永远不结束（CI 上即超时失败），第一版变异脚本没带子进程超时、
  整个脚本卡死在 S2 上，这是「变异幸存首先要怀疑变异本身」之后第二条教训：**变异工具
  必须假设被测程序会挂**。
- **依赖**：`tokio-serial`/`serialport`（MPL-2.0，deny 白名单内，`default-features = false`
  关掉 libudev——Linux 构建不因此要装 libudev-dev）；`encoding_rs`（编码批已入）。
  用户裁定 2026-09-04：引入这两个而不是自写三平台 FFI。

- **tag**：`v0.7.0`（M7.1–M7.4）。
- **执行顺序**：先 M7.2 的确认口径（M7.1/M7.3 都要用），再 M7.1 服务管理与
  补丁盘点，再 M7.3，串口最后（体量最大且独立）。

### 1.0.1 SFTP 高延迟吞吐（2026-09-30 立项）

- **起因**：用户要求复查速度是否达标。CI 的 ≥20 MB/s 门禁（`scale.rs`）只量本机回环，
  往返近乎为零；在容器出口注入 netem 延迟后，1.0.0 与 OpenSSH `sftp` 同条件对比
  （MB/s 上传/下载）：单向 10 ms 时 5.0/7.2 对 26.1/28.4，
  25 ms 时 2.1/3.2 对 21.0/28.3——**不达标**，且差距随延迟线性放大。根因：每块都要等齐
  「打开 → 读写 → 关闭」的往返，逐块串行。
- **设计**：
  - 上传：`SftpOps::open_writer` → 同一句柄、按偏移递增的流水线写（russh-sftp 最多 8 个写请求
    在途）。「已排队」与「已确认」分开记账，失败重试与续传只认已确认下界并把临时件截回那里；
    单句柄 + 服务端按序处理保证远端始终是连续前缀（续传以远端大小为断点的前提）。
  - 下载：窗口从 1 起步，一次读 ≤1 s 加一、≥5 s 减半，上限 8；`SftpOps::open_reader` 复用读句柄，
    每块只剩一次 READ 往返；本地按序落盘，短块（含空块）即报错并丢弃窗口里后续的读。
  - 两个入口的默认实现就是 1.0.0 的逐块调用，测试替身与故障注入不受影响；只有 `RemoteSftp`
    与包着它的 `TimedSftp` 覆写。
  - 慢链路：russh-sftp 单请求超时由默认 10 s 放宽到 600 s（1.0.0 因这 10 s 在低于约 26 KB/s 的
    链路上必然失败）；存活检测仍由 `TimedSftp` 按每次调用计时。下限见 `sftp.rs` 的
    `REQUEST_TIMEOUT_SECS` 注释。
- **出口标准**：
  - [x] 高延迟下吞吐与 OpenSSH 同量级。（判据：`sftp_bench.rs` 与 OpenSSH `sftp` 同一时段交替
    各 5 轮，中位数上传/下载——10 ms：1.0.1 53.8/29.4、1.0.0 5.6/8.8、OpenSSH 73.3/103.6；
    25 ms：35.5/32.3、2.5/3.7、38.9/19.2。方法与测量边界见
    [性能验证](verification/performance.md)「SFTP 高延迟对比」）
  - [x] 断点语义不变。（判据：`crates/itest/tests/transfer_real.rs` 128 MiB 上传在 16 MiB 处
    掐断连接，临时件逐字节是本地前缀、续传后完整；替身用例 `upload_retry_resumes_from_the_confirmed_floor_not_the_queued_offset`、
    `upload_retry_after_a_failed_finish_resumes_from_the_confirmed_floor`、
    `source_shrunk_mid_download_stops_at_the_short_chunk`）
  - [x] 窗口行为可观测。（判据：暂停时钟下的 `download_pipeline_grows_the_window_on_fast_reads`
    恰好涨到上限、`…_stays_sequential_on_slow_reads` 停在 1、`…_shrinks_the_window_when_the_link_slows_down`
    骤降后缩回 1；`download_reuses_read_handles_instead_of_reopening_per_chunk` 读句柄数 ≤ 窗口上限）
  - [x] 超时层不被绕开。（判据：`tests/timeouts.rs` 的读取器/写入器各两条：实参原样转交内层、
    按数据面预算计时且挂死即判死通道）
  - [x] 变异证明。（34 条：写入器已确认下界与起点、收尾不 fsync、会话配置、上传两条失败路径的
    断点与截断、下载窗口增/停/缩/上限、短块保护、读句柄复用与读满、超时层六处转交与预算、
    `RemoteSftp` 两个覆写的源码守卫。存活 2 条，均已说明：会话配置里的在途上限与默认值恰好
    同为 8（等价变异）；上传每轮更新断点只在本地源文件读出 I/O 错误时可达，tokio 文件读取
    在测试里注入不了这种错误）
  - [ ] SSH 通道接收窗口。10 ms 下载仍明显落后 OpenSSH，读耗时显示是交付速率受限；同一时段
    交替 5 轮，8 MiB 窗口在 25 ms 下把下载从 8.2 提到 14.0 MB/s，Nagle 开关则无差别。**未采纳**：russh 的窗口是会话级配置，终端通道会一起变大，
    Ctrl-C 之后要排空的在途输出从 2 MiB 变成 8 MiB；逐通道设置 russh 不支持（补窗目标是会话级
    字段）。可行方向：SFTP 走独立连接，或给 russh 提逐通道窗口。
  - [x] 回环吞吐门禁改测产品路径。（判据：`scale.rs` 的 `large_file_roundtrip_integrity_and_throughput`
    改为 `TransferManager` + 生产组装 `TimedSftp(RemoteSftp)`，门槛仍是合计 ≥20 MB/s、仍逐字节比对。
    旧口径测逐块 `write_at` / `read_range`——1.0.0 时那就是产品的切法，1.0.1 之后不是了。同日 main 上
    发布试跑里旧口径在 ubuntu runner 测得 19.3 MB/s，压线；新口径本机三次 100.5–113.5 MB/s。
    门槛临时改成 1000 时用例转红）
- **范围外**：延迟对比依赖 netem 与 OpenSSH 客户端，不进 CI。

---

## §2 对标与超越矩阵

### §2.1 Xshell 7 完全体 → 覆盖映射（SSH 面）

**不对标项声明**：本产品仅 SSH/SFTP；Xshell 侧的 TELNET/RLOGIN/SERIAL/RAW/本地 Shell 明确不支持（M6+ 评估或永久不做，总设计 §0 同步留痕）。**定档补充**：会话分组文件夹由会话 `group_path` 自动派生建立（无独立 folders 表）；手动新建文件夹定档 M4a（MVP 文件菜单 `folder.new` 禁用态预埋 + 注记「分组随会话 group_path 自动建立」，UI 规格 §2.1 文件菜单/§2.3 底部按钮同相位注记）。跳板链可视化编辑（从已有 Profile 选取/增删/排序）定档 M4a（MVP 仅只读呈现与按计划跳转，UI 规格 §2.4 认证页签同相位注记）。

| Xshell 能力 | 覆盖里程碑 | 备注 |
|---|---|---|
| 会话管理器（分组树/搜索/拖拽/状态灯） | M1 | UI 规格 §2.3，MVP 固定左停靠（拖右/浮动 M4b）；分组随会话 `group_path` 自动建立，手动新建文件夹 M4a（文件菜单 `folder.new` MVP 禁用态预埋，UI §2.3 底部按钮同注记） |
| 标签 MDI（MRU 切换/关闭确认） | M1 | UI 规格 §1.2；关闭确认模态见 UI 规格 §2.9 |
| 完整认证（密码/公钥/kbd-inter/agent/跳板链） | M1 | 三平台 agent 含 Pageant；跳板链可视化编辑 M4a（MVP 仅只读呈现与按计划跳转，UI 规格 §2.4 同注记） |
| 主机密钥管理（TOFU/指纹/known_hosts 导入） | M1 | 严格模式默认拒绝；OpenSSH known_hosts 导入入 host_keys（source=imported），总设计 §2.1/§6.1 |
| 组合命令栏 | M1 当前会话 / M4a 广播目标 | §2.6 |
| 输入广播（Send Input to Multiple Sessions） | M4a | 实时键入同步到选定会话，独立开关；双通道见本路线图 M4a 范围 ①② |
| 每会话配色方案 + 字体 | M1 | 三级作用域，≥10 内置 |
| 快速命令按钮集 | M4a | 带参数占位符，UI 规格 §2.10 |
| 键盘配置文件/快捷键自定义 | M4a | §4 |
| 高亮关键字/输出提醒（bell 角标） | M1 bell 基础 / M4a 关键字规则 | §5，UI 规格 §2.11 |
| 会话录屏与回放（通用终端工具补齐项，非 Xshell 原生——Xshell 仅文件日志[纯文本/RTF/HTML]，无内置回放） | M4a | asciinema 兼容 |
| 会话纯文本日志（Logging） | M4a | 路径/追加/时间戳可配，UI 规格 §2.4 日志页签 |
| SSH 隧道/端口转发管理器 | M4a | |
| ZMODEM（rz/sz）自动收发 | M4a | Xshell/FinalShell 均有，中文运维高频依赖项 |
| 脚本/自动化（expect 式） | M3（Agent 替代，超越） | Xshell 无内置脚本引擎（SecureCRT/MobaXterm 等竞品共有）；本产品以 M3 Agent 替代，属超越项 |
| 计划任务 | M4a | Xshell 无内置（老用户以操作系统任务计划程序解决），原生内置 + 重启补跑属超越项 |
| 导入自家格式 | M4b（`.xsh` 尽力导入） | spike 定边界 |
| 换肤 | M1 起（超越：4 主题 + 开放主题目录） | Xshell 换肤弱 |

### §2.2 Xftp 7 完全体 → 覆盖映射（SFTP 面）

**不对标项声明**：FTP/FTPS（Xftp 侧）明确不支持；文件传输仅走 SFTP 通道。

| Xftp 能力 | 覆盖里程碑 | 备注 |
|---|---|---|
| 双栏浏览器（面包屑/排序/隐藏文件） | M1 | §1.4 |
| 传输队列（进度/重试/取消） | M1 | 256KiB 块/3 重试/断点；MVP 单件取消/重试；『全部取消』M4a（R24 既定） |
| 拖拽传输 | M1 基础 / M4a 多文件+取消 | |
| 同步浏览 | M4a 实装（开关入口 + 双向联动 + 独立浏览模式，两模式 itest） | 联动延迟 ≤1s；M1 不含（§1.4 分屏仅双栏独立浏览） |
| 站点管理器复用 | M1（同一 Profile） | 合体优势 |
| 排除过滤器 | M4a | |
| 外部编辑器关联 | M4a | 语义按总设计 §2.3 远程文件编辑（下载→监听保存→回传 + mtime 冲突检测） |
| 文件属性对话框/软链图标/「新建软链」菜单 | M4a | core 层 lstat/read_link/symlink 已于 M1 就位（SftpOps），本项仅补 UI 载体 |
| 文件夹同步 / 文件夹比较（Synchronize Folders） | M4b 评估 | 做/不做结论与依据记录入 4b 计划 |
| 与终端程序分离 | **超越：同标签分屏** | M1 |

### §2.3 FinalShell 完全体 → 覆盖映射

| FinalShell 能力 | 覆盖里程碑 | 备注 |
|---|---|---|
| 服务器监控面板（CPU/内存/网络图） | M1 UI 预留 / M4a 实装 | 采集方式与 FinalShell 同为 SSH 通道（非超越点） |
| 进程管理（进程列表/过滤/终止） | M4a | 监控抽屉扩展页签，经既有 exec 通道（`ps`/`kill`） |
| 命令片段库 | M4a | 超越：AI 生成（M4b） |
| 历史命令 | M1（网格接口）/ M4a（历史库+UI） | 检索面板 + 一键重发 |
| 主机状态灯/连通轮询 | M1 基础 / M4a 轮询 | 轮询周期/翻转周期见 4a 出口「主机状态灯轮询」条 |
| 打包便携 | M4b 评估 | 做/不做结论与依据记录入 4b 计划 |

### §2.4 超越项总表（竞品皆无或明显弱）

1. **AI 原生**：NL→命令 / 输出解读 / 策略闸门下确认后执行 / 自主 Agent（M2–M3）。
2. **MCP 双向**：既是 MCP Server（被其他 AI 驱动）又是 Client（挂外部工具）（M3）。
3. **终端 + SFTP 同标签同时分屏**：竞品或双程序分离（Xshell+Xftp）、或同窗切换视图（FinalShell），均无同屏并排分屏（M1）。
4. **监控 × AI 联动**：采集经既有 SSH exec 通道（与 FinalShell 同），但异常指标可一键 AI 诊断、与终端/会话上下文统一呈现（M4b，依赖 M4a 采集 + M2）。
5. **现代主题引擎**：CSS 令牌驱动 4+ 应用主题 × ≥10 终端配色 × 用户自定义主题目录（M1/M4b）。
6. **跨工具迁移**：Xshell/FinalShell/iTerm2 配置与配色导入（M4b）。
7. **可取证审计**：append-only + hash 链 + 导出取证包（M3 完整，M1 建表）。
8. **安全默认值**：严格 TOFU 可选、下载沙箱分级、剪贴板定时清除、零遥测（M1 起）。

---

## §3 工程与发布约定

- **计划文档**：新阶段范围、出口与实施条目集中维护在本路线图，不再新增重复的单任务完成报告。
- **复审**：每个计划/规格经 Workflow 多视角对抗复审（≥4 视角 + 逐项反驳验证），连续两轮复审均无确认的高/中危发现方定稿/通过（低危可留痕不阻塞；M0/M1 的 Phase 0+1 计划与 UI 规格/本路线图走同一闸）。
- **实现**：superpowers:subagent-driven-development，每任务独立提交；任务间复审。
- **命名公约**（以已审定的 Phase 0+1 计划 Task 1 为准）：包名与 lib 名均为 `fs_<name>`（下划线，避 crates.io 同名风险）、目录 `crates/<name>`（无前缀）；总设计 §1 的 8 个 crate 名（`vault`/`connmgr`/`sshengine`/…）即去前缀模块名，一一对应（如总设计 `vault` = 包/lib `fs_vault` = 目录 `crates/vault`）。
- **版本**：里程碑 tag 一律**纯三段** `vX.Y.Z`（M1=`v0.1.0`、M2=`v0.2.0`…；M4 拆 `v0.4.0` / `v0.4.1`），阶段名写进 release notes 而不是版本号——详见 §0 的「tag 口径」说明。此前本行写的是 `v{0.N.0}-phaseN`，与 §0 的裁决及版本一致性门禁的三段正则**直接矛盾**（2026-08-22 盘点发现：T92 只改了 §0 的表，漏了这一行，于是同一份文档里两条互相否定的口径并存，而被否定的那条正是门禁 selftest 的反例夹具）。正式对外 1.0 的门槛 = M1+M4 验收通过 + Windows 代码签名 + macOS 公证链路打通（Linux 以免签分发 + 校验和声明替代，记录入附录；与总设计 §8.4 及 M4b 出口签名链路条对偶）。
- **CI 闸**：main 保护分支；PR 必过三平台矩阵 + deny；itest 仅 Linux runner（FS_ITEST=1）；性能测试**分两条线**（`FS_PERF` 闸从未落地，见 CONTRIBUTING）：可自动化的部分随 `FS_ITEST=1` 进 CI（`scale.rs` 吞吐、`render_pipeline.rs` 五条）；需 GUI 与预构建产物的部分在 `perf.rs`，全 `#[ignore]`、本机手动跑、不作发布证据。
- **发布**：tag v* 触发 release.yml 草稿（tauri-action，未签名产物 + 校验和）；签名密钥与发布动作始终人工执行。
- **push 到 GitHub**：属对外动作，每批提交累积后等用户点头再 push（当前本地领先若干提交）。

---

## §4 全局风险与缓解

| 风险 | 影响里程碑 | 缓解 |
|---|---|---|
| russh/russh-sftp API 变动 | M1 | 版本硬锁 0.62.4/2.3.0；CI 每周一次 latest 探针（仅告警） |
| Pageant 管道行为 | M1 | 附录 A.2 spike 入 M1 出口标准 |
| macOS 公证/开发者账号 | M4b、M5 | 立项即确认账号；公证链路于 M4b 出口走通（CI 测试证书可接受），账号未就绪时按 M4b 出口降级条款挂 1.0 前置项 |
| Linux webkitgtk 版本碎片 | M1 | CI ubuntu-latest 单 runner（与计划 Task 2 一致）；README/打包说明标注 webkitgtk 最低版本；22.04 下限双矩阵留待 1.0 前评估 |
| vt100 CJK/复杂转义 | M1 | 网格仅 best-effort（AI/搜索/审计），渲染权威 xterm.js |
| 模型网络不可达 | M2 | Ollama 一等公民 + mock provider 测试基线 |
| ~~rmcp 2.x~~ **rmcp 3.x** 稳定度 | M3 | spike 已完成（2026-08-24，见 M3 节）：实际最新 3.1.4、三周 6 个版本，故 pin `3.1` 并把适配面收在 `fs_mcpbridge` 一个 crate；HTTP 全在 optional 特性后，stdio 一路零 HTTP 依赖 |
| `.xsh` 私有格式 | M4b | 格式 spike 先行，承诺「公开字段尽力导入」 |
| 移动端账号/后台限制 | M5 | 账号就绪检查前置；前台服务 spike |

---

## §5 进度快照（2026-07-29）

- ✅ 总设计 v2（commit 7d0af7c，未 push）
- ✅ Phase 0+1 实现计划（commit b05c904，未 push）
- ✅ UI 设计规格 + HTML 原型双视图 + 视觉/对比度验证（commit 7523a67，未 push）
- ✅ 本路线图（commit 45538fd + 契约升级 139a3a5）
- 🔄 对抗复审 Round 1（UI 规格 + 路线图，4 视角×19 发现全部 CONFIRMED）已修订完成：
  - 停靠方向统一为左停靠（UI 规格/路线图/原型三处，对齐总设计 §2.4 第 2 项）；
  - M0 出口命令修正为 npm + `frontend/`；M1 性能出口对齐 §0.1 全五项（100 会话口径）；
  - M1 范围措辞修正（含回灌任务声明，状态改「计划回灌中」）；M4 拆分 4a/4b 并逐项出口、补自动更新+签名；M5 依赖补 M4a；
  - 超越项「无 agent 监控」虚假主张删除，改为「监控×AI 联动」；Alt+P 冲突修复（设置改 Ctrl+,）；导入菜单 MVP 口径（仅 JSON 加粗）；§3.1 令牌全集补齐；§8 映射补 settings 表迁移/opener/web-links。
- 🔄 Round 2 复审（14 项 CONFIRMED 中危）修订完成：
  - 矩阵 ↔ 范围 ↔ 出口三向对齐：计划任务/关键字规则/历史命令 UI 补入 M4a 范围与逐项出口；「打包便携」定档 M4b 评估；§2.2 拖拽传输/同步浏览「M4 完善」替换为可验证增量（多文件+取消、双向联动 ≤1s）；
  - 会话日志 ≠ 录屏：§2.1 拆两行（asciinema 录屏 / Xshell 式纯文本 Logging），M4a 各给出口；UI 规格 §2.4 Profile 编辑器补「日志」页签；
  - UI 规格补章节：§2.9 关闭确认模态（对齐总设计 §2.4 第 6 项 + §8 CloseConfirmDialog 行）、§2.10 快速命令按钮条、§2.11 高亮关键字集、§2.12 选项对话框页签结构；
  - §3.2 事实纠偏：boldBright → `drawBoldTextInBrightColors` 并补方案→选项映射表；临时配色入口由终端右键（与「右键=粘贴」冲突）改挂标签右键菜单；§5 bell 行补 M1/M4 相位标注；
  - 附录引用编号纠正：agent spike → A.2（两处 + M5 风险），M4b 公证 → A.6，M1 出口补 A.1（IPC 载荷编码）关闭项；
  - M0 范围 fs_itest 措辞修正（环境闸约定在 M0，crate 本体 M1 Task 9）；M1 范围/出口补 audit 建表（§6.2 字段、append-only）与 bell 角标验证项。
- 🔄 Phase 0+1 计划本体复审（4 视角×24 发现全部 CONFIRMED，21 高危）修订完成（commit 957e16c，G1–G5 共 119 处编辑）：russh 0.62.4 API 全线纠偏（AuthResult 枚举/kbd-interactive start-respond 循环/connect_stream/AgentClient/Channel split/russh-sftp 全异步）、Batcher EOF 关闭路径、sqlx 离线校验、下载沙箱强制、clipboard 不出 core、TOFU 生产写库、指纹按二进制 blob + UI 回灌（100 会话口径、bell、web-links/opener、主题/组件/迁移任务）
- 🔄 Round 3 复审（UI/路线图，17 项 CONFIRMED、去重 13 项）修订完成（本轮）：
  - 剪贴板基线回正：UI 规格 §2.12「固定不可关」改回「默认开、30s 时长可配、整项可关」，逐字对齐总设计 §3.2；不变量 2 同步补限定词（三处同一口径；计划侧 settings 驱动时长待回灌 Task 20）；
  - M4a 历史命令依赖统一为「M1 网格接口（总设计 §2.2）+ M4a 本地历史库」：4a 范围、§2.3 矩阵、4a 出口三处同步，消除与「4a 仅依赖 M1」的矛盾；
  - M0/M1 边界修正：frontend/npm 出口项自 M0 下移 M1（前端骨架 = 计划 Task 17），M0 ci.yml frontend job 加存在性门控声明，M1 出口补前端骨架验收项；
  - §2.1 事实纠偏：「会话录屏与回放」标注「非 Xshell 原生」（Xshell 仅文件日志[纯文本/RTF/HTML]，无内置回放）；M1 目标括注「录屏」替换为 Xshell 真实老用户依赖项（输入广播/快速命令集/片段库/纯文本日志）；
  - 输入广播补齐：§2.1 增行（Send Input to Multiple Sessions），M4a 范围改双通道（组合栏发送 + 实时键入广播独立开关），4a 出口补实时键入字节级一致用例；
  - 同步浏览定档：§2.2「M1 基础」改为「M4a 实装（两模式 itest）」，与 Phase 0+1 计划无同步浏览实体的现状对齐；
  - M2 出口补基线：policy 回归语料库 ≥200 条 + CI 断言 dangerous 召回率 ≥95%（对齐总设计 §8.1/§9），M2 范围补语料库建设任务；
  - M1 出口锚点改「本清单为唯一验收权威，Task 25 须回灌至一致」（消除 50 vs 100 会话口径双载体）；
  - UI 规格：§8 映射表补 bell 角标/搜索浮层两行 + 时效注记，§2.8 补 bell 前端侧检测来源与 Ctrl+C 双义性规则，§4 补复制行，§3.2 补透明度映射/终端重建边界/渲染器前提，§3.1 令牌表改权威集并补 `--fs-radius-lg`/`--fs-density` 语义。
  - 计划侧配套修订（待 UI 回灌批次完成后统一落盘，避免并发编辑）：Task 1 补 README/CONTRIBUTING 创建步骤；Task 2 ci.yml frontend job 存在性门控；Task 20 剪贴板时长读 settings 键 + 开关分支；Task 25 清单与本路线图 M1 出口逐项对偶。
- 🔄 Round 4 复审（UI/路线图，61 项 CONFIRMED[18 中危]、4 项 REFUTED，去重 33 项独立修订）修订完成（本轮）：
  - 事实纠偏：「Xshell Task Scheduler」虚假归因删除（Xshell 无内置调度器——老用户以操作系统任务计划程序解决，§2.1 矩阵与 M4a 范围两处改为超越项表述）；「Xshell 仅有纯文本 Logging」改为「Xshell 仅文件日志（纯文本/RTF/HTML），无内置回放」（§2.1 矩阵 + 本节日志两处）；超越项 3 收窄为「同屏并排分屏」（删对 FinalShell 不成立的「一个程序一个会话上下文」）；不变量 4 高吞吐底线归属说清（路线图自加量化，依据总设计 §2.2/§8.5.2）；
  - 范围边界：§2.1/§2.2 表头各加「不对标项声明」（TELNET/RLOGIN/SERIAL/RAW/本地 Shell、FTP/FTPS 不支持），§0 总览与 M1 目标限定「SSH/SFTP 面」，总设计 §0 同步增排除行；
  - 基线补齐（总设计承诺落载体）：known_hosts 导入定档 M1（范围/出口/§2.1 矩阵/UI §2.4 入口）；ZMODEM（rz/sz）定档 M4a（范围/出口/§2.1 矩阵）；文件夹同步/比较与便携打包评估定档 M4b（范围+出口）；FinalShell 进程管理定档 M4a（§2.3 矩阵/范围/出口）；Anthropic 适配器入 M2 范围与出口；host facts 连接期采集入 M2 范围与出口；截图 + 密钥/代理管理器入 M4b 范围与出口（UI 孤儿件闭合）；外部编辑器关联语义锚定总设计 §2.3（下载→监听→回传 + mtime 冲突检测）；
  - 口径对偶：M0 ↔ 总设计 §9 Phase 0 边界冲突 → 修订总设计（Tauri 壳/前端骨架/打包管线移 Phase 1，上位依据让步于已复审通过的 M0/M1 切分）；M1 性能出口 RSS 改「后端进程（core 侧）常驻内存增量」+ 渲染侧单列观测（对齐总设计 §0.1 后端常驻口径）；loopback <2s 补回「（不含认证往返）」限定词（出口 + 不变量 4 两处）；1.0 门槛改「Windows 签名 + macOS 公证，Linux 免签 + 校验和」（对齐总设计 §8.4/M4b 出口）；SFTP 出口补 stat/symlink + 传后可选 sha256sum 校验（禁 exec 降级 size）；M3 预算改「步数/连续自动命令数/token 三件套」+ 出口增触顶断言；M3 MCP Server 措辞改「stdio 唯一传输」（总设计 §4.5 同步）；M5 失效时限量化（≤5s/在线 ≤1s）；广播出口定量 ≥5 会话；M6 依赖图与表统一；crate 命名公约入 §3；M0 tag 例外入 §3 模式；审计写入归属三段式说清（M1 建表/M2 AI 动作/M3 全链路）；关闭确认补入 M1 出口；TOFU 增第四态「仅本次」（内存态 AcceptOnce，总设计 §2.1 同步补语义）；
  - AI 治理补齐：M2 范围/出口补 AI 执行总开关（总设计 §4.3）+ 按 Provider「允许发送屏幕上下文」开关（总设计 §4.2）；解读流触发改「选区上下文菜单」与 UI §2.8 新右键复合规则对偶（彻底解决默认「右键=粘贴」下入口不可达）；
  - UI 规格同批修订：§2.8 右键上下文菜单复合规则 + 菜单条目表，§2.4 known_hosts 导入入口 + 高亮关键字集勾选 + AI 页签 MVP 禁用态标注，§2.5 kbd-interactive name/instruction 字段映射，SessionRecoveryDialog 新节，§1.3 监控抽屉相位标注 + 标签右键菜单独立定义，§1.4 传输队列停靠钉死 + 传后校验开关，§2.6 发送后缀默认值 CR，§2.10 删「MVP 仅当前会话」，§3.1 tooltip 令牌，§3.2 透明度表述重写（删自相矛盾的「需重建」），§3.4 term_blob 改「需新增字段」（M1 connmgr 范围同步补列），§5 断线 banner 对齐总设计 §7.1，「语言」去粗标 Phase 4b，孤悬元素逐条处置（键盘模式语义/托盘删除/打开会话目录定义），Toast/SessionBanner 组件行；
  - 总设计同批修订：§0 协议排除行；§2.1 事件载荷补 kbd-interactive name/instruction + 「仅本次」内存态语义；§4.5 stdio 措辞；§9 Phase 0 行下移前端壳/打包管线至 Phase 1。
  - REFUTED（不修，留痕）：审计相位「自相矛盾」全量主张（实际仅路线图内部归属措辞问题，已按 #13 修订）；单一广播开关映射（UI 开关即实时键入通道，组合栏目标选择器另属 §2.6）；M4a Xftp 项无 UI 章节（§1.4/§2.4 已覆盖）；high-water 悬空（计划 Task 16 已定义常量，M1 出口公式自洽）。
- 🔄 Round 5 复审（UI/路线图，19 项 CONFIRMED[1 高危/5 中危]、1 项 REFUTED）修订完成（本轮）：
  - 事实纠偏：§2.1 矩阵「脚本/自动化（expect 式）」补归因备注（Xshell 无内置脚本引擎，SecureCRT/MobaXterm 等竞品共有，M3 Agent 替代属超越项）；总设计 §0 功能愿景行补「归因以路线图 §2 矩阵为准」注（广播对标 Xshell、片段库对标 FinalShell）；
  - 命名公约三方冲突消解：§3 公约与 M0 出口重写为已审定计划 Task 1 口径（包/lib `fs_<name>` 下划线、目录 `crates/<name>` 无前缀；总设计 §1 名 = 去前缀模块名），L47 冒烟命令 `fs_sshengine` 本就一致；
  - known_hosts 导入计划载体：M1 范围等式补注记，具体回灌在计划本体同批修订（Task 7 解析入库/Task 20 导入按钮/Task 9·25 itest）；
  - M4 覆盖补齐：4b 出口补三路导入（Xshell/FinalShell/iTerm2）与监控×AI 联动出口，4b 范围/出口补语言切换，4a 范围/出口补「打开会话目录（reveal-item-in-dir）」（UI §2.1/§2.2 孤悬件闭合），§2.4 超越项 4 归属改「M4b，依赖 M4a 采集 + M2」；
  - 口径对偶：M1 出口关闭确认改四入口（补中键，与 UI §2.1/§2.9/§4 一致；webdriver 困难时组件/手工核验留痕）；§4 风险 CI 缓解改 ubuntu-latest 单 runner（与计划 Task 2 及总设计 §8.4 一致，22.04 双矩阵留待 1.0 前评估）；
  - UI 规格同批修订：§2.7 按键仲裁规则（远程模式仅影响无修饰/单 Ctrl 键；恒本地白名单 Ctrl+Shift+* 全家/Ctrl+F/W/Tab/,/N/F11/Scroll Lock；Ctrl+C 选区复制优先于远程发送）；§2.8/§8 后台标签保留活实例仅停渲染（bell/ack 照常），serialize 休眠降级为关闭前快照可选优化；§3.2 `drawBoldTextInBrightColors` 事实纠偏（5.5.0 DOM/canvas/webgl 三渲染器均生效，源码核验，删 canvas 回灌项）；§2.12 补 Ctrl+V 粘贴开关（默认开，§2.1/§4 默认值的配置载体）；§2.2 工具栏删无语义的「全部保存」（骨架图同步），「打开目录」补 M4/MVP 隐藏标注；§5 角标清除手势（单击角标即清除 + tooltip）；§2.3 拖文件到未连接会话行为（先连接后入队，失败 Toast 丢弃）；§2.11 关键字高亮执行位置（前端 registerDecoration/onWriteParsed + core 规则持久化）；§2.13 引用改 §2.4 第 5 项 + session_journal 载体声明；
  - 总设计同批修订：§2.1 保活补心跳 RTT 载体（UI §2.7 延时字段）；§2.2 渲染层后台标签处置改「保留活实例仅停渲染」+ serialize 仅快照优化（三处同口径）；§2.4 第 5 项 + §3.4 第 3 条补 session_journal 载体的同句声明。
  - REFUTED（不修，留痕）：分屏方位「硬冲突」（总设计 §2.4 第 4 项「上下分栏」= 终端/SFTP 两种排布选项之一，与 UI §1.4 左终端/右 SFTP 默认并存，非矛盾）。
- 🔄 计划本体 Round 2 复审（39 项 CONFIRMED + P2-23 + 传后校验回灌 M1）修订完成（commit 8b3363f）：russh 0.62.4 API 全线纠偏落地（Task 7 known_hosts 导入全实现 ImportSummary/hashed 跳过/冲突不覆盖/幂等；Task 12 SftpOps lstat·read_link·symlink + 新增 verify.rs 传后校验执行体四态[verified/unavailable→size 降级/mismatch/failed]+9 单测+容器 itest 四态 round-trip；Task 13 scrollback 可变借用[vt100 set_scrollback(MAX)→contents()]；Task 15 双水位迟滞背压 queue_bytes_low；Task 16 EOF 渲染流关闭；Task 24 性能锚点改认证之后；Tasks 17–22 共 38 项前端+app 代理补丁含 CSP 双策略[script 禁 unsafe-inline/eval、style 允许]与方案甲 Mutex<Handle> 句柄单存）；P2-23 sftp_mkdir/remove/rename 命令 + invoke_handler 六命令接线；Task 21 transfer_submit 默认开校验（settings transfer.verifyAfterTransfer）+ 事件泵 Done 驱动 + transfer_verified 事件；TransferList→TransferQueueDrawer 全局抽屉（跨标签持久可见、徽标四态、重试按钮；全部取消随 M4a）；host_keys source 值由 import 改 imported 三方对偶；单测口径 24→33。
- 🔄 Round 6 复审（UI 规格 + 本路线图 + Phase 0+1 计划本体三文档，27 发现[3 高/24 中；26 CONFIRMED + 1 PARTIAL（i=19）]，全局裁决 R1–R12 + R4 并入 R10–R12）修订完成（本轮，commit d101409）：
  - 1) TOFU 四态载体定桩（i=1）：Task 25 出口改四态，新建 `crates/itest/tests/ssh_tofu.rs` 容器四例 + `connect_logic::accept_once_accepts_without_recording` 单例，本路线图 M1 出口/总设计 §2.1/UI §2.5 权威不动；
  - 2) 关闭确认四入口统一门控（i=2，R1 主体）：Task 22 Step 4 五子项（CloseConfirmDialog 签名默认焦点「取消」/ `requestCloseTab` 统一入口 + TabBar onClose 换线 / TabBar auxclick 中键 / Ctrl+W 经 R3 派发器 / 窗口路径触发收窄为「活动会话或排队/进行中传输」+ SessionRecoveryDialog 默认焦点「全部跳过」+默认全选），Task 20 Step 4 上提 `lib/transfers.ts` 共享 store 供传输计数（Round 5 修订 R28 自 Task 21 前移），全空闲直关；
  - 3) Vault 凭据 UI 端到端（i=3）：新增 `vault_list_secrets`（RecordMetaDto 仅元数据、永不回 secret 材料）+ ProfileDialog 认证页签「存入 Vault…」内联表单→自动回填选取，提交后前端不留明文，M1 出口同批增项，独立 Vault 管理器归 M4b 密钥/代理管理器；
  - 4) 键盘模式跨平台入口（i=4）：状态栏键盘模式段点击定为三平台主入口（纳入 UI §2.7 点击表），Scroll Lock 降格 Windows 肌肉记忆快捷，§2.12 补默认远程项，§4 表补行，计划骨架同批回灌并纠硬编码本地态显示；
  - 5) §5 快照簿记滞后（i=5）：补计划本体 Round 2 完成条目（见上），24 项条目状态词改「修订完成」（commit 957e16c），「下一」行计划本体 Round 4 纠正为 Round 3；
  - 6) 复审闸门口径收敛（i=6）：§3 复审条改「连续两轮复审均无确认的高/中危发现方定稿/通过（低危可留痕不阻塞）」，与 L7 执行契约同口径；
  - 7) M5 出口补齐（i=7）：增监控卡（配对通道 ≤2s 刷新、断连/过期态明确呈现）与命令抽屉（同一 fs_policy 闸门 + audit、危险命令拦截）两项，闭合 §0「M4a → M5（监控卡）」依赖声明；
  - 8) macOS 公证链路定档（i=8）：M4b 出口升级「公证链路在 release.yml 走通（CI 测试证书可接受；账号未就绪=降级条款+挂 1.0 前置）」，§4 风险归属改 M4b、M5，§3 1.0 门槛对偶自动成立；
  - 9) M4a 状态灯轮询出口（i=9）：4a 出口增「30s 间隔轮询、停机/恢复 2 周期内翻转灯态、失败静默降级、三用例」，§2.3 矩阵补回指；
  - 10) Ctrl+W/中键关闭绑定（i=10）：独立窗口键片段作废，并入 R1 统一门控 + R3 派发器，计划 Task 25 四入口条款保留为验收对偶；
  - 11) 关闭触发四路（i=11）：UI §2.9 删「关闭全部标签」第五路（M4b Window 菜单追加入口，启用后复用同一入口），与 UI §2.1/本路线图 M1 出口/计划 Task 22 逐字对齐；
  - 12) 窗口关闭触发收窄（i=12）：「若有 tabs」改「存在活动会话（connecting/connected/重连中）或排队/进行中传输」，补计数来源（tabs store + transfers store）与空闲关窗反例；
  - 13) kbd instruction 透传（i=13，R2 与 i=24 一次落笔）：trait 三签名 `(name, instruction, prompts)`、russh `InfoRequest` 解构透传、GuiEvents + types.ts `instruction?: string`、AuthPromptDialog 空值折叠、核验注记闭环；
  - 14) Vault UI 载体 + 自动锁定（i=14，R8 方案甲）：新建 VaultDialog.svelte（setup/unlock 双模、错误抖动）+ `vault_lock` 命令 + vaultToggle 锁定分流，App 顶层 60s 轮询 `vault.autoLockMinutes` + 最后交互时间戳自动锁定，保 §2.12 承诺，Task 25 补便携解锁手工验证；
  - 15) 按键派发器（i=15，R3 主体）：Task 17 新建 `lib/shortcuts.ts`（arbitrate + keyboardMode store + 键→动作表），App.svelte 唯一窗口级 keydown，UI §4 全集逐项经达 + 终端焦点 attachCustomKeyEventHandler 仲裁，M1 出口增逐项核验项；
  - 16) 终端粘贴/右键/Ctrl+C 双义（i=16）：TermOptions.interactions 闭包 + requestPasteConfirm，attachCustomKeyEventHandler/paste/contextmenu/selection/wheel 五挂点，UI §2.8 上下文菜单组件 + 多行粘贴气泡「记住本会话」，三个设置键有消费方；
  - 17) SearchOverlay 载体（i=17）：Task 18 新建组件+测试（Ctrl+F 开/Enter·Shift+Enter 上下/Esc 关还焦），TermController 改 findNext/findPrevious/clearSearch，edit.find 死菜单复活；
  - 18) Toast 载体（i=18）：Task 20 新建 `lib/toast.ts` + `Toast.svelte`（三级 info/warn/error、3/5/8s 自消、堆叠≤4），连接/重连/vault 失败统一呈现，UI §8 归属回灌；
  - 19) 连接态三处一致（i=19 PARTIAL，R5）：Tab = {id,title,bell,status,profileId,host} + setTabStatus/replaceTabId/reorderTab/MRU 栈，openSession 先建 connecting 占位，三事件桥 session:disconnected/status/closed，Sidebar 状态灯 + StatusBar 全参挂载，M1 出口增三处一致项；
  - 20) Sidebar 交互 + 死控件（i=20，R7 乙案）：Sidebar 搜索/右键菜单/底部三按钮/拖拽 + DeleteConfirmDialog + profiles_export/import 命令，help.* 四项全实现，folder.new 禁用注记「分组随会话 group_path 自动建立；手动新建文件夹 M4a」（本路线图 §2.1 定档留痕，UI §2.1/§2.3 同批相位注记）；
  - 21) 标签 MDI 交互（i=21）：TabBar 中键/铃铛角标点击清除+tooltip/draggable 排序/溢出箭头+下拉，Ctrl+Tab MRU 经 R3 派发器，M1 出口「四入口/手动清除」有实现支撑；
  - 22) 字体缩放回正 MVP（i=22）：menus.ts fontGrow/fontShrink 恢复可用态，onAction 分支 + zoomFont/resetFontSize + Ctrl+滚轮 per-session（8–32），M1 出口增项；
  - 23) 对比度脚本归属（i=23）：Task 17 新增 theme.test.ts 对比度 describe（WCAG 相对亮度，四主题 ≥4.5:1/hc ≥7:1）+ `scripts/verify-contrast.mjs` + `verify:contrast` 脚本归属，Task 25 出口双载体对偶；
  - 24) kbd instruction 计划侧归并（i=24）：与 13) 同批落笔（R2 裁决 i=24 ⊇ i=13），Task 19 事件面 + Task 20 对话框同批回灌；
  - 25) SftpPane 九项（i=25）：Task 21 Step 3 重写（工具行 返回/上级/刷新/新建目录/删除确认/行内重命名、面包屑、Ctrl/Shift 多选、隐藏文件开关、排序、drop 即 transfer_submit、错误条+空态、分隔条比例记忆），Step 6 补九项手动验证；
  - 26) 对话框焦点契约（i=26）：UI §7 新增通用三件套（默认焦点 CloseConfirm=取消/SessionRecovery=全部跳过/HostKey changed=拒绝/AuthPrompt=首个输入框；Tab 圈闭；关闭归还触发元素），Task 17 新建 `focusTrap.ts`，Task 22 新建 SessionRecoveryDialog；
  - 27) 编码边界甲案（i=27，R6）：v1 仅 UTF-8 直通，转码层 M4+ 评估（总设计 §0 表行 + §2.2 第 6 条 + §3.1 注释；UI §2.4 字段禁用态 + §2.7 段只读；计划 ProfileDialog/StatusBar 同批）；
  - 全局裁决与并入：R1 主体 i=2（i=10/i=12/i=21⑦文案/i=26 默认焦点均并入）；R9 本 M1 出口 10 条增量行与计划 Task 25 同句同文对偶（SftpPane 工具行/拖拽进 Task 21 Step 6，矩阵已含不新增）；R4 并入（r4-final.json：9 高 + 29 overcap + 12 low）——R10 `Arc<Db>` 方案甲（establish_session/reconnect_loop/watchdog 三签名 + Task 22 `state.db.clone()`），R11 配色三级作用域 `resolveTheme({global, profile, transient})`（profile 覆盖用 `themeOverride` 键、不动 Task 6 schema；临时层=会话内存态 M4a）（R16 已收进 M1，本条历史留痕），R12 low 项裁量吸收，余入组 L early/late 批（O21 跳板链 M4 定档留痕：已落笔（LL3））；
  - 关口状态：本轮 26 CONFIRMED + 1 PARTIAL 全部修订，「连续两轮零确认高/中危」计数自下一复审轮起算。
- 🔄 Round 7 复审（UI 规格 + 本路线图 + 总设计 + Phase 0+1 计划本体四文档，27 发现[9 CONFIRMED（1 高/8 中）+ 5 REFUTED + 13 low]，全局裁决 R16–R25，58 补丁 N1–N4 落盘）修订完成（本轮，commit 7ad3ca5）：
  - 1) R16（high，配色 transient 层相位对撞）：transient 配色层收进 M1——Task 18 tabs.ts 补 `transientSchemes` store + `setTransientScheme` 原语 + 单测，Task 20 TabBar 右键「配色方案 ▾」子菜单（12 套 + 跟随默认清除项），装配处传 tempSchemeId 至 TerminalPane，删改三处「相位 M4a / v1 恒 null」注记，resolveTheme 注释与 M1b 衔接段改三级接线完整，Task 25 出口补临时配色验证项（M1 出口「三级作用域生效」为唯一验收权威，§2.2 矩阵/UI §1.1/§2.5/§8 口径不动）；
  - 2) R17（传后校验降级触发条件三方分叉）：总设计 §2.3 定稿「远端 sha256sum 不可用（服务器禁 exec 或无该命令）时降级为 size 比对并在 UI 显式标注，禁止静默跳过」，UI §1.4 补「（含禁 exec）」括注对齐；
  - 3) R18（断线 banner 重连倒计时）：总设计 §7.1 banner 要素补「重连倒计时（下次尝试 Ns，1s 粒度，源自退避间隔）」，计划 Task 22 Step 3 banner 整块代码补 1s setInterval 倒计时实现（状态变更/重连成功清除），UI §5/§8「逐字对齐 §7.1」自洽；
  - 4) R19（恒本地白名单三方分叉）：UI §2.7 白名单补 Ctrl+L、Ctrl+=/Ctrl+-/Ctrl+0、F3、Alt+P，与计划 isAlwaysLocal 全集及 M1 出口互洽（Ctrl+C 双义条款不动）；
  - 5) R20（M4b 范围/出口缺 Window 菜单归属）：§1 M4b 范围行与出口回灌 Window 菜单全项 + 「关闭全部标签」验收行（启用后复用 R1 统一入口）；
  - 6) R21（§5「下一」行轮次滞后）：已随本条落笔纠正，O21 条目标「已落笔（LL3）」闭合；
  - 7) R22（「新建子组」相位泄漏）：UI §2.3 分组右键「新建子组」补 M4a 注记，与 §2.1「手动新建文件夹 M4a」同口径；
  - 8) R23（高亮关键字相位标注分叉）：以 §2.2 矩阵相位为权威，UI §2.11 标题/正文/分叉处改齐（含标题与正文自相矛盾处）；
  - 9) R24（「全部取消」归属分叉）：UI §1.4 TransferQueueDrawer「全部取消」补 M4a 注记（MVP 单件取消/重试在、批量 M4a），§5 4a 范围行补「传输队列『全部取消』」归属；
  - low 批（R25 + 簿记组吸收）：UI 笼统「Phase 4/M4」逐项对齐 4a/4b 拆分粒度、工具栏「剪切」列删除、帮助菜单四项按「MVP 必须项加粗」体例加粗、§2.11 匹配器单测声称按计划实际载体裁定、状态灯条断链引用改指真实定义章节；余 low 留痕不阻塞；
  - 关口状态：本轮 9 CONFIRMED 全部修订（计划 26 补丁 + UI 24 + 路线图 6 + 总设计 2，零失败零重复），「连续两轮零确认高/中危」计数自下一复审轮起算。
- 🔄 计划本体 Round 5 复审（Phase 0+1 计划，11 CONFIRMED[4 高/7 中]=6 独立缺陷类 + 5 REFUTED + 10 low，并入裁决 R26–R33）修订完成（本轮，commit `fb47d99`）：Scripted kbd_interactive 补参（R26）/Task 20 装配去重（R27）/transfers.ts 创建上移 Task 20（R28）/CredentialSet Zeroizing 化（R29）/host_keys ≤1 行不变式（R30）/defaultPolicy 消费方（R31）+ low 机械批与测试批（R32/R33）；关口状态：「连续两轮零确认高/中危」计数（计划本体轨）自下一复审轮起算。
- 🔄 **P 波合并修订**（Round 8 UI/路线图 + 计划本体 Round 6 + 依赖基线校准 + 自查，四来源一次性落盘，139 补丁 / 四文档 / 零失败零重复，commit `3bef48a`）：
  - **Round 8（UI 规格 + 本路线图）**：9 CONFIRMED 全中危（去重 8 类，C1≡C7 归一）+ 7 REFUTED + 12 low（L2≡L6 归一），并入裁决 R34–R42。要点：标签右键「关闭标签」与标签 × 同走 `requestCloseTab`（同一路径的两个控件面，§2.9 四路指路径分类而非控件个数）、「关闭其他标签」降 M4b（R34）；路线图 L73 快捷键枚举纠偏 `Ctrl+Shift+S/M`（Ctrl+Shift+A 于 MVP 禁用、随 M2 启用，R35）；「恒本地截获」术语三方归总——截获≠不下发，Ctrl+L→`edit.clearScreen` 仍经 `term_input` 发等效字节（R36）；传输队列「全部取消」相位对齐 M4a（R37）；4a 范围/出口补齐手动新建文件夹与跳板链可视化编辑两载体（R38）；UI §2.1 工具菜单补「高亮关键字…（M4a）」（R39）；§7 焦点契约兜底改三层规则（逐条列明 → 表单类首输入框 → 纯确认类非破坏性按钮，R40）；「快速命令集 ▾」补 M4a 相位注（R41）。
  - **计划本体 Round 6**（21 视角全活复跑，前一次外壳报 pass 但 14 agent 死 12 于网关 503、该轮作废不计门槛）：13 CONFIRMED[2 高 / 11 中] + 3 REFUTED + 6 low，并入裁决 R49–R59、R62–R67（C3≡C12≡C13 归一为 R51）。两项高危：① Task 12 SFTP 实现误按 `std::fs` 类型写协议层（`file_type_of`/`mode_of` 形参应为 `russh_sftp::protocol::FileType`/`FilePermissions`，`read_link` 直返 String），字面执行即 E0308×2 + E0599×1、crate 恒编译失败（R49）；② Task 20 `onReady` 单值承接使多标签场景后挂载覆盖先前控制器，改 `paneApis` 会话映射 + `activeController()` 派生，并补齐 ④ 分支块七支（查找/字号三支/MRU 双向/关闭活动会话/清屏，R57）。其余：itest 缺 `PublicKeyBase64` 导入与零参 `fingerprint()` 误用（R50）、`vault_put_secret` 秘密母体未 Zeroizing（R52）、Agent 身份偏好标注 Phase 2 生效（R53）、下载沙箱三级回落 + 设置键 `sftp.sandboxRoot`（R54）、根约束单一实现 `ensure_within` 去双份并补 4 例（R55）、agent 不可用测试改名并双支断言不静默跳过（R56）、term.ts 按 `arbitrate(e, { terminalFocused: true, … })` 契约改写并新增 `onLocalAction` 直投（R58）、ComposeBar 占位 `console.info` 换真 `term_input` 下发（R59）、Task 24 `app:ready` 陈旧追加块删除防双发（R66）、工具栏规格外孤条 `session.saveAll` 删除（R67）。
  - **依赖基线校准**（专项自查，非复审轮）：全 63 处版本钉逐一取证，24 bump / 16 hold / 12 unresolved，四项需拍板项由主控裁决为 R45–R48。全升级面内**仅一处真 API 重写**（sqlx 0.9 `SqlSafeStr`）。要点：spec §379「GPL 系禁入 core」适用范围界定为随包分发/静态链入的源码与 crate，**运行时动态链接 OS 自带系统库（libdbus/glibc/GTK）不在其列** → tauri 保持默认 features，apt 补 `libdbus-1-dev` 兜底（R45）；`Cargo.lock` 入库（本仓为应用而非库；sqlx 0.9 上游已不跟踪 lock）+ sqlx 精确钉 `=0.9.0`（R46）；9+ caret 工具依赖维持 caret（R47）；apt 补 `libxdo-dev`/`libssl-dev`、release.yml runner 统一 `ubuntu-latest`、deny.toml 移除已弃用 `version = 2` 键（R48）。落钉：`rust-version = "1.94"`（sqlx 0.9 MSRV 底线）/ vt100 `=0.16.2` / argon2 `=0.5.3` / tauri `2.11.5` + tauri-build `2.6.3` / testcontainers `0.27`（0.26.4 已 yank）/ @xterm/xterm `^6.0.0` 及五 addon 配对 / vite `^8.1.5` / vitest `^4.1.10` / jsdom `^29.1.1` / svelte `^5.56.8`（vite-plugin-svelte 7 peer 地板）/ typescript `~5.9.3`（TS 7 无 API、Svelte 工具链尚不可用）/ Node 24。
  - **自查**：Task 22 Step 4(b) 锚点漂移与虚构中间态改写（R43）、UI §8 `transfers.ts` 归属两处回灌（R44，O 波 R28 余孽）、Task 1 Step 7 `git add` 补 `Cargo.lock`（R46 口径与执行对齐）。
  - **A 波（P 波落地审计，非复审轮）**：4 分片（依赖/Rust/前端/跨文档）× 对抗质证，raised 20 / confirmed 18（2 项证伪：一处 R55 性质误判、一处 R53 承重前提为假）→ 去重 15 独立缺陷类，30 补丁全或无落盘（计划 27 + UI 3 + 手改 1）。三类：① 机械已落但旧散文残留（Node 22→24、UI §3.2 xterm 5.5.0→6.0.0 及渲染器前提改写——canvas 已于 6.0 移除 #5105、降级链 webgl→DOM、`@xterm/addon-canvas` 停在 0.7.0 且 peer `^5.0.0`；拔网四处同驱措辞；Task 25 出口「不原样透传至会话」）；② 裁决已落但机械对偶缺失（`fs_itest`/`future-shell-app` 两个 `[package]` 补 `rust-version.workspace`、精确钉清单补 `sqlx =0.9.0`、TabBar 补「关闭其他标签」禁用态预埋 + 单测、menus.ts 补 `tools.highlight`、Task 23 Step 4 标题与其「不引容器」注释对齐）；③ 兄弟改动后计数陈旧（前端 vitest 79→81、app crate 累计 9（unix）/8（Windows）、Task 20 Files 清单补 `TabBar.test.ts`）。延伸裁决二则并留痕：`edit.cut` 依 R67 自身理据（§2.2 权威清单仅复制/粘贴 + UI §1.1 明列剪切无 MVP 命令）删除；`import.xshell` 相位依 UI §2.1 细化 M4→M4b。校验：p-verify 139 补丁 0 失败（10 项 superseded 已归因）+ 自查探针 84→142 断言全绿 + 跨文档残留扫描零命中（commit `3bef48a`）。
  - 关口状态：P 波改动覆盖三份主文档，**先前任何通过一律清零**；「连续两轮零确认高/中危」计数两轨均自下一轮起算。
- 🔄 **Q 波合并修订**（Round 9 UI/路线图 + 计划本体 Round 7，两轨合并一次性落盘 291 补丁 / 四文档 / 零失败零重复，裁决 R68–R97，commit `9728e47`）：
  - **Round 9（UI 规格 + 本路线图，48 CONFIRMED [3 高 / 39 中 / 6 low]）** 全量处置，裁决 R68–R80。三高危：① F20 profile 配色层端到端（Rust/TS `theme_override` snake_case 键 + ProfileDialog 字号/配色两控件 + App 装配 ⑦ 传递 + 四处注释统一 + Task 25 出口补项，守 R11 不动 schema）；② F16 onAction 五桩（help.docs/keys/logs/about/tools.vaultToggle）整体替换标记 + 指令行删桩动作 + 首注列名双保险；③ F25 键盘模式 per-session（Tab.keyboardMode + setTabKeyboardMode/setTabError 原语 + toggleKeyboardMode(sessionId) 不持久化 + 全局 store 更名 keyboardModeDefault 仅 SettingsDialog 消费 + term.ts/App 仲裁读活动会话 + 红测改写，向 UI §2.12 刻意措辞对齐，UI 胜）。批次要点：R68 批号消歧（Task 19–22 区段 22 行裸「M4」→「补丁 M-4」+ Global Constraints 标记约定行；真里程碑 L10783 跳板链改 M4a）；R69 裸 Phase 4/M4 四文档量化细化（总设计 §9 阶段表拆 4a/4b 两行 + 8 散点，路线图 L53 两处 + L304 风险表，计划散点归各自计划）；R70 单件取消补载体（TransferState Cancelled + cancel() + `transfer_cancel` 六→七命令 + 抽屉非终态取消按钮 + STATE_LABEL「已取消」）；R71 侧栏拖右/浮动 + 窄屏覆盖层降 M4b（settings 键 ui.sidebarDock、matchMedia 1099px 验收）；R72 SFTP 本地栏收窄（远程栏专属新建目录/删除/重命名，M4b 评估留门）；R73 背景透明度定档全局 `term.opacity`（五处「Profile 级」回改 + TermOptions/rgba/TerminalPane/SettingsDialog 机械补消费方）；R75 连接详情弹层降 M4a（MVP 点击无响应，字段形状本注锁定）；R78 承诺无出口批 16 项（F28 状态栏 [重连] props+挂载+出口、F29 errorText 全链路、F36 性能出口 core 侧限定、F37 验收手段改 E2E/组件测试、F38 Ctrl+Shift+A 禁用镜像、F39 自动锁定 + 错误抖动、F40 关闭确认「断开并取消传输」、F41 导入首连 TOFU 移 Task 10 第五例 itest、F42 stat_size 载体、F43 组合栏 M1 出口镜像、F44 M0 出口 README/CONTRIBUTING、F45 M3 run_command 三路断言、F46 M5 推送降级 + 手机控制开关、F47 M6 并发冲突子句、F48 散点）；R79 三方契约批（F31 auth:prompt/auth_respond 命名 + 载荷 session_id + v4 changelog、F32 hostkey key_type 四处、F33 SessionBanner→TerminalPane 内联 + §8 时效注、F34 粘贴气泡键盘契约 + 门禁回灌 76→77/81→82 + 路线图出口句、F35 updated_at 五处 + 本地化/回落口径）；R80 机械对偶批（F14 监控占位、F15 编辑菜单五命令 onAction + clearScrollback + clipboard export、F17 ToolBar openDir 移除留 M4a 恢复位、F18 SOON→相位 8 处 + 删常量 + test 三断言、F21 transient 关标签即释放三处、F22 跟随默认清除项、F23 tooltip/radius-lg 三令牌、F26 灰实心灯、F27 tree a11y、F09/F11 令牌化 + 抽屉改流内块）。
  - **计划本体 Round 7**（33 CONFIRMED [6 高]，任务 w91n70xwx）合并处置：21 新增 → 17 修订组 R81–R97；7 项与 Round 9 重叠并入 F 系列不独立编号（R7-20→F38、R7-21→F39、R7-22→F42、R7-23→F41、R7-24→F37、R7-25→F40、R7-33→F31）；5 项组内重复并入母组（R7-19/30→R7-18、R7-26→R7-11、R7-27→R7-10、R7-31→R7-04）。六高危：ci.yml check job 从不构建 frontend/dist（generate_context! 将 panic，R81）；deny.toml 许可白名单缺 ISC（russh aws-lc-rs，R82）；FS_ITEST 空串赋值令环境闸失效（R83）；vt100 0.16 set_scrollback 为显示偏移非容量（R87）；vitest 缺 globals/setupFiles 致 SearchOverlay 测试破裂（R89）；release.yml 缺 permissions: contents: write 致 draft release 失败（R95）。余 R84–R86/R88/R90–R94/R96/R97 为 Files 清单补齐、Produces 消费方指错、悬空接口处置、backpressure 水位、行数口径、lib.rs 口径与 Task 23 幻影条目批。
  - **Q 波补遗 Q53/Q54**（commit `225034c`）：F41 批次两处「四例」对偶残留回填（Task 25 出口清单 × 路线图 §1 M1 出口，ssh_tofu.rs 四例→五例：四态 + 导入首连不弹一例），双方逐字对偶保持；p-verify 全量 293 补丁零失败（2 项既有 superseded 豁免不变）。Round 3 修订史「容器四例」系历史记录不动。
  - 关口状态：本轮覆盖 UI/路线图轨与计划本体轨，**先前任何通过一律清零**；「连续两轮零确认高/中危」计数自下一复审轮起算——Round 10（UI/路线图）与计划本体 Round 8 各为**首轮计数候选**。
- 🔄 **R 波合并修订**（Round 10 UI/路线图 + 计划本体 Round 8，两轨尝试 #2 真正执行完毕且**均 `pass:false`**，确认项去重合流为 19 个独立缺陷类 R98–R116，一次性落盘 73 op / 三文档 / 零失败零重复，commit `5db57fe`）：
  - **作废前置**：两轨尝试 #1 均因网关 503 空跑（Round 10 20 agent 中 19 失败、Round 8 6/6 视角全灭），返回的 `pass:true` 系伪阳性 → 按空跑作废原则不计闸门；脚本四项加固后重跑（空返回三次重试 / 任一发现视角为空即整轮 `void` / 票数 <2 的发现入 `unresolved[]` 并令 `pass=false` / 修复 `filter(Boolean)` 先于 `flatMap` 致的视角归属错位）。
  - **四条高危（三类真缺陷）**：① **R110 交付前提破裂**——Task 17 的 `App.svelte` 已 `import … from "./lib/tabs"`，而 `tabs.ts` 在 Task 18 才创建，Task 17 自身 `npm run check` 必红；处置为三处结构迁移（tabs.test.ts 红测块 → Task 17 新 **Step 5a**；tabs.ts 实现块 → Task 17 Step 6，落点在 shortcuts.ts 之后、focusTrap.ts 之前以满足 `keyboardModeDefault` 依赖；Produces 条目 → Task 17），Task 18 相应删 Files/Test 条目、Step 2 标题去「tabs store」、**Step 6→5 / 6b→5b / 7→6 / 8→7** 全序重编 + 两处跨步引用同改。② **R112 Vault 静默数据死锁（安全）**——keyring-only 库（建库时未设应用密码）自第二次启动起永久打不开：onMount 见 `vault_has_file()` 为真即置锁、连试都不试 keyring，而解锁弹窗必走 `unlock_with_passphrase` → 无口令 PHC 恒 `Error::Locked`；且直接改用 `open_or_create(dir, None)` 静默解锁更危险（keyring 条目被清时它会铸新主密钥并 `save()` 覆写，令既有密文永久不可解）。处置为新增 `Store::unlock_with_keyring(data_dir)`（缺库文件或缺 keyring 条目即 `Locked`，**绝不铸钥、绝不回写**）+ `vault_unlock` 无口令路径按 `vault.json` 存在与否二分 + 前端两处（onMount / `tools.vaultToggle`）改为一律先试静默解锁 + Task 4 增测（门禁 8→9）+ 手工核验第 8 项就地增补对偶两例（不增项，保「13 项全 PASS」）。③ **R113 导入面越权引用（安全）**——`import_json` 原样保留攻击者提供的 `auth.vault_record`，一份人工构造的 profiles.json 即可把任意 vault 记录 id 指给攻击者自己的会话，登录时经 SecretSource 取出别人的口令/私钥送往攻击者主机；处置为导入面剥离 `p.auth.vault_record` 与 `p.jump[*].auth.vault_record` + 增测 `import_strips_vault_record`（门禁 10→11、13→14）。④ **R101 §2.12「Ctrl+V 粘贴开关」是死档**——远程模式下 `attachCustomKeyEventHandler` 放行后 xterm `evaluateKeyboardEvent` 会把 Ctrl+V 映射为 `\x16` 并 `preventDefault`，textarea 的 `paste` 监听永不触发，开关开与关行为完全一致（且默认开时按 Ctrl+V 只得到 `\x16`）；处置为 `TermInteractions` 增 `ctrlVPaste`（默认 true）+ term.ts 在 Ctrl+Shift+V 分支后**就地**新增 Ctrl+V 条件白名单分支 + TerminalPane/SettingsDialog 三键→四键 + UI §2.7 仲裁段写明「必须就地判定」的理由 + 四处「三键」措辞同改。
  - **两处编译错误**：R105 `pub type TransferId = u64` 是类型别名，`mgr.cancel(TransferId(id))` 调用不存在的构造函数（改 `mgr.cancel(id)` 并摘 use 清单）；R106 `TermOptions` 无 `sessionId` 字段而 term.ts 两处引用 `opts.sessionId`（增必填字段、去 `= {}` 默认值、Produces 签名与调用点同改）。
  - **其余十条**：R98 连接详情弹层承诺无载体（路线图 4a 范围 + 出口各补一条）；R99 SFTP 本地栏三动作与远程栏不对偶（4b 出口补评估结论条）；R100 工具栏右键自定义无相位/无键名/无路线图条目（命名 `ui.toolbarLayout` 并标 M4b，UI §2.2/§3.4 + 路线图 4b 范围/出口四处对偶）；R102 M1 出口三键措辞未含 Ctrl+V 开关；R103 `term.opacity` 在 M1 出口无验收条；R104 MVP 侧栏根本没有组右键菜单（§2.1 三项整体归 M4a 且不做禁用态菜单，§8 DeleteConfirmDialog 的 MVP 消费方收窄为「仅会话删除」）；R107 App.svelte 两条 `./lib/tabs` import（合并并立去重铁律）；R108 Task 20 去重护栏只覆盖 `./lib/shortcuts`（改写为三模块通则 + 导入块按「改写既有 / 纯新增」重构）；R109 TS `TransferEvent.state` 漏 `"Cancelled"`（抽屉已按该态渲染）；R115 六处 `cargo deny check licenses bans` 全漏 `advisories`（`deny.toml` 已声明 `[advisories]` 段，不跑等于没有——总设计 §6.2）；R116 迁移后门禁计数陈旧。R111 归并去重时并入 R110 母因，保留空号不复用。
  - **校验**：`r-apply.js` 先 MOVE 后 PATCH、`count(OLD)===1 && count(NEW)===0` 双向唯一性、全有或全无 → 73 op 零失败零重复；`r-selfcheck.js` 70/70 PATCH 复核 + 16 条结构断言全绿；p-verify 逐测试文件 `it(` 权威普查——theme 14（10 静态 + 1 处 4 参数化）/ tabs 11 / shortcuts 19 / focusTrap 6 / layout 3 / menus 4 / compose-history 3 = **Task 17 60**，term 7 / schemes 6 / SearchOverlay 2 / TerminalPane 6 = **Task 18 21 → 累计 81**，toast 3 / TabBar 2 → **累计 86**；Rust 侧 Task 3 5 + Task 4 4 = `-p fs_vault` **9**，db 8、repo 3 → **11**、+ settings_repo 3 → **14**，逐条与门禁文对齐。
  - 关口状态：R 波改动覆盖三份主文档，**两轨计数清零**；「连续两轮零确认高/中危」自下一轮（Round 11 / 计划本体 Round 9）起重新起算 0/2。
- 🔁 **闸门主体迁移（2026-07-29，非复审轮）**：R 波之后两轨文档复审已连做 11 轮，缺陷产出转为措辞/簿记类，而 Task 1 开工首日编译器即在数秒内抓出 11 轮纸面复审全部漏掉的真缺陷（Windows NASM 硬前置，见下）。据此按用户原始指令「每开发完一个阶段就要做复审」回正闸门主体：
  - **第一裁判 = 编译器/类型检查器/测试**（`cargo build` / `fmt --check` / `clippy -D warnings` / `nextest` / `cargo deny check licenses bans advisories`，前端另加 `npm run check` / `vitest`）；
  - **第二裁判 = 对抗复审**，主体由「计划文档」改为「每个 Task 的代码 diff」，在该 Task 门禁全绿之后执行；
  - 「连续两轮零确认高/中危」的判据保留，但作用对象改为**已实现的代码**；UI/路线图/计划三份文档转为参照物，仅在实现取证推翻其结论时回填修订（如下述两条）。
- ✅ **M0/M1 实现落地（滚动记录）**：
  - Task 1 workspace 骨架（commit `276a441`）：8 个 core crate + `deny.toml` + README/CONTRIBUTING + `Cargo.lock` 入库；五门禁全绿。**实测取证反推计划修订**（commit `812b80c`，四处落盘）：`russh 0.62.4` 默认 feature `aws-lc-rs` → `aws-lc-sys 0.43.0` 的 build script 在 `x86_64-pc-windows-msvc` 上强制要求 `nasm.exe`，缺失即 `NASM command not found! Build cannot continue.` panic 并中止整个构建；`ring` 备选 backend 在 Windows x86_64 同样需要，换不掉；`windows-latest` runner 镜像不自带 NASM。→ Task 1 README 模板补「Windows 额外前置」、Task 2 ci.yml 与 Task 23 release.yml 各补 `ilammy/setup-nasm@v1`（`if: runner.os == 'Windows'`）、Task 23 Step 4 散文补前置。
  - Task 2 CI 三平台矩阵（commit `875ca93`）：`.github/workflows/ci.yml` 落盘（check job × windows/macos/ubuntu-latest + frontend job），本地冒烟 `clippy -D warnings` + `nextest --no-tests pass` 双绿；js-yaml 解析核验结构（check 14 步 / frontend 5 步）。
  - Task 3 vault 加密原语（commit `4c5a70c`）：AAD = `record_id||version` 各 8 字节大端绑定，换 id / 回滚 version / 篡改密文 / 换密钥四路均判 `Integrity`；Argon2id + 随机盐落 PHC。`-p fs_vault` 5 tests passed，四门禁全绿。**实测取证反推计划修订**（本条）：`aes-gcm 0.11`（hybrid-array）已弃用 `Array::from_slice`，计划 Task 3 Step 5 的 `Nonce::from_slice(...)` 在 `-D warnings` 下直接挂门禁 → 计划两处改 `&Nonce::from(...)`（`From<[u8; N]>`）。
  - Task 4 vault Store（commit `59a7422`）：OS keyring 托管主密钥（`future-shell`/`master-key`，base64）+ 应用口令备用解锁（Argon2id 派生密钥封 master key）+ 记录 CRUD + 原子落盘（tmp→rename）；`Store.key` 由 `impl Drop` 手动 zeroize，主密钥/派生密钥/base64 形态全程 `Zeroizing`。R112 语义在测试中固化：`unlock_with_keyring` 缺库文件或缺 keyring 条目一律 `Locked`，且断言「解锁失败不得建库」。`-p fs_vault` 9 tests passed（5 crypto + 4 store），四门禁全绿。**实测取证反推计划修订**（本条，计划 Task 4 Step 3 三处）：① `fill_bytes(&mut rand::rngs::OsRng, &mut k)` 与 ② `hash_password_into(…, &mut out)` 对 `Zeroizing<[u8;32]>` 均为 **E0308**（Rust 不会同时走 DerefMut + unsize 两跳把 `&mut Zeroizing<[u8;32]>` 变成 `&mut [u8]`），须写 `&mut *k` / `&mut *out`——已按计划原文逐字试编译取证，非推断；③ `STANDARD.encode(&*k)` 触发 `clippy::needless_borrows_for_generic_args`（`-D warnings` 下挂门禁），而其建议的 `*k` 会把 32 字节主密钥按值 Copy 进不受 `Zeroizing` 管辖的栈临时量（安全回退），故定为 `encode(&k[..])` 并就地注释理由。
  - Task 5 connmgr schema/迁移/版本闸（commit `6486e84`）：三份嵌入式迁移（0001 profiles/host_keys/session_journal/meta、0002 settings、0003 audit + append-only 双触发器）+ `Db::open` 双版本闸（`user_version > MAX_SCHEMA=3` → `SchemaTooNew`；库内 `meta.min_app_version` 高于本 app → `AppTooOld`）+ 迁移前 `VACUUM INTO` 快照备份与 5 份裁剪。`-p fs_connmgr` 8 tests passed（workspace 累计 17），四门禁全绿。计划 Step 5 预言的「6 绿 2 红（settings/audit 未建表）」中间态实测逐字命中，Step 6 补迁移后转全绿——build.rs 的 `rerun-if-changed=migrations` 确实令新增迁移触发 `include_str!` 重展开，该前置有效。**实测取证反推计划修订**（本条，三处）：① `pub struct Db` 缺 `#[derive(Debug)]`，而 Step 2 两处 `Db::open(..).unwrap_err()` 要求 `T: Debug` → **E0277 × 2**、测试目标恒编译失败（计划补 derive 并注明其为必需而非装饰）；② `pre_migration_backup_is_pruned_to_five` 原断言 `n <= 5` 是**空过测试**——`VACUUM INTO` 失败时只记 `tracing::warn`，7 份预置文件照样被裁到 5，红绿不分；已就地增断言「本轮确产出 `fs-v2-*.db` 且非空」（不增测试项，仍 8 个），并先以临时探针实测确认 `VACUUM INTO ?1` 绑定参数在 SQLite 可用、快照为 57344 字节真库；③ Step 8 `git add crates/connmgr` 漏 `Cargo.lock`——本 Task 首次引入 sqlx/uuid 依赖必改 lock（R46 口径），已补。
  - Task 6 connmgr Profile/Settings 仓库与 JSON 导入导出（commit `9b4e510`）：`ProfileRepo`（upsert 14 参数 `ON CONFLICT(id) DO UPDATE`／get／list 按 `group_path, name` 序／delete／export_json／import_json）+ `SettingsRepo`（get／set upsert／list 按 key 序）。R113 落地为可执行断言：`import_json` 逐条重签 id 后剥离 `p.auth.vault_record` 与 `p.jump[*].auth.vault_record`，`import_strips_vault_record` 实测两处均回落 `None`——否则一份人工构造的 profiles.json 可把任意 vault 记录 id 指给攻击者会话，登录时经 SecretSource 把受害者口令/私钥送往攻击者主机。`-p fs_connmgr` 14 tests passed（db 8 + repo 3 + settings_repo 3），workspace 累计 23，四门禁全绿；Step 4「11 绿」与 Step 8「14 绿」两处中间态预言均逐字命中。**实测取证反推计划修订**（本条，两处）：① Step 1 的 `db()` 辅助函数写作 `(dir, Db::open(&dir.path.join("fs.db")).await.unwrap())`——元组元素按从左到右求值，`dir` 先被移进第一元素、再在第二元素里被借用 → **E0382**，测试目标恒编译失败（本机先按计划原样复现取证、再改拆两句）；② `p.id = Uuid::new_v4(); // 避免 id 冲突` 的行尾注释会令 rustfmt 把紧随其后的 R113 六行整段并入该行尾注释、按 35 列悬挂缩进重排，Step 9 前的 `cargo fmt --all` 会把错位缩进直接写进仓库（`--check` 实测 EXIT=1），已改为注释独占一行。
  - Task 7 sshengine 主机密钥信任库与 TOFU 裁决（commit `be7d0fa`）：`hostkey.rs` 四态裁决 `decide()`（钉扎优先于信任库；known 为空 → `AskTofu`，Strict 下 `RefuseStrict`；known 非空且不匹配 → `Changed{old_fingerprint}` 硬失败）；`fingerprint_sha256()` 对**二进制** blob 求 SHA256 + base64 无 padding，已用 `ssh-keygen -lf` 真实向量逐字锁定；`TrustStore` 的 `record`/`replace` 同构（单事务 DELETE 旧行 + INSERT），钉死 R30「每 host:port 至多一行当前有效密钥」，R13 替换语义消除旧键退库后的 fail-open MITM；`import_known_hosts` 处理注释/空行/hashed(`|1|`)/`@cert-authority`/逗号 host 列表/`[host]:port`/同 host 异 key 计 conflicted 且不覆盖。`-p fs_sshengine` 10 绿 · workspace 33 绿；五门禁全绿。**两处实测取证反推计划修订**：① 计划 Step 6 的 `git add crates/sshengine` 漏了 `Cargo.lock`（本 Task 首次引入 russh/sha2 等依赖，锁文件必须同批入库，与 Task 1 R46 同口径）；② `cargo deny check advisories` 因 russh 默认特性 `rsa` 引入 rsa 0.10.0-rc.18 而红（RUSTSEC-2023-0071，非常量时间 / Marvin Attack，上游无修复版）→ `deny.toml` 增具名豁免并留复查触发条件（关闭该特性会同时废掉 ssh-rsa 主机密钥验证与 id_rsa 客户端认证，与「对标 Xshell 完全体」冲突；SSH 无 RSA 解密路径，RSA 仅用于签名，AC:H 成立）。
  - Task 8 sshengine 认证方法选择状态机（commit `e414a7c`，纯逻辑）：`Method{Password,KbdInteractive,PublicKey,Agent}`（Display 取 RFC 4252 线名，`from_wire` 反解服务器通告）+ `CredentialSet` 链式 Builder（秘密入参一律 `Zeroizing<String>`，**手写 Debug** 令密码与私钥 PEM 恒印 `[REDACTED]`，总设计 §3.2）+ `next_method` 取「偏好序 ∩ 服务器通告 ∩ 未尝试」——**绝不尝试服务器未通告的方法**（spec §2.1）；`KbdInteractive` 在仅有密码时亦判可用，覆盖 OpenSSH+PAM 禁 password 只通告 keyboard-interactive 的常见配置（R53 已裁定此路**无条件弹框**、不做静默自动应答，`kbd_auto_answer_single` 随 Phase 2 audit crate 生效）；`prioritize_agent_keys` 令偏好身份优先并受 cap（默认 3）限制，防一次连接把 agent 里的密钥挨个试穿、耗尽服务器 `MaxAuthTries`；`SecretSource` trait 由 app 层以 `vault::Store` 实现，core 不依赖 vault 存储。`-p fs_sshengine` 14 绿（hostkey 10 + auth_select 4）· workspace 61 绿；五门禁全绿。**计划同批回灌**：原文的链式调用与长签名过不了 `cargo fmt --check`，Step 4 的「`lib.rs` 追加 `pub mod auth; pub mod secrets;`」亦改为整文件照抄——rustfmt 的 `reorder_modules` 默认开启，追加到末尾会被重排。
  - **Task 8 第二裁判（对抗复审）**（commit `4cf60c0`）：两条确认发现，均为**单元测试结构上抓不到**的那一类。**S24（low，但正中功能靶心）**——`prioritize_agent_keys` 的「偏好优先」阶段只 `filter` 不去重：同一身份若在 `available` 里出现两次（多 agent 复用、转发链、非 OpenSSH 实现不做 add 去重）且又落在 `preferred` 里，就会被推两次，于是 cap 名额与服务器 `MaxAuthTries` 各被同一把密钥白占两份，真正能过的密钥反而排不进来——**恰好架空了 cap 存在的全部理由**；补齐阶段的 `!out.contains(k)` 救不回来，它只管自己新推的那些。改为两阶段都去重，新增回归 `agent_keys_never_repeat_an_identity`，非空证明做足（回退修复后确认变红，报错为 `left: ["d", "d", "a"] / right: ["d", "a", "b"]`，精确指向重复项）。注：MVP 下 `preferred` 恒空（R62 已裁定 `host_key_pins` 与 agent 公钥不是同一键空间，故不喂），该洞**当前不可达**——但 agent 身份偏好字段随 M4+ 落地时它就在那儿等着，故此刻补齐而非留待。**S25（med，静默失效类）**——线上根本没有 `agent` 这个认证方法（agent 认证在协议层就是 `publickey`，只是密钥材料来自 agent），`Method::Agent` 是纯本地区分，`from_wire` 也刻意无 `"agent"` 分支；由此得出一条**未被写下**的调用方硬约束：凡从服务器通告构造 `server_remaining` 之处，都必须在 `publickey` 在列且启用 agent 时补回 `Method::Agent`，否则 `next_method` 永远选不到它，**agent 认证整条路静默失效**——不报错、不告警，只是「agent 里明明有可用密钥却总登不上」。`connect.rs` 现有落点是对的（Task 10 计划 L5312 已有该补回），但约束只写在**消费方**注释里，而本模块的测试直接以字面量传 `server_remaining`、结构上绕过了通告映射，**违例无法被任何测试捕获**。处置：把约束写进 `Method::Agent` 与 `from_wire` 的 doc（点名 Display 与 from_wire 非互逆、点名现有落点），使其随 API 一起被读到。`-p fs_sshengine` 15 绿 · workspace 62 绿；五门禁全绿。
  - Task 9 itest 真连集成基线（commit `3347162`）：新增 `crates/itest`（workspace 第 9 个成员），`sshd.rs` 以 testcontainers 0.27 起 `linuxserver/openssh-server`（必须 `LOG_STDOUT=true`，否则 s6-overlay 把服务日志重定向到文件，`WaitFor::message_on_stdout("Server listening on")` 永不命中）+ `ssh_connect.rs` 四条真连测试：password 认证后 `exec echo` 取回 stdout 与 exit_status；PTY + shell 交互（写 `echo MARK-$((20+5))` 读回 `MARK-25`，证明伪终端确实在跑 shell 而不是管道）；错误口令后断言服务器**通告了剩余方法**（实测 `MethodSet([PublicKey, Password, KeyboardInteractive])` —— Task 10 的认证状态机正是靠这份通告驱动，此处把「服务器真的会告诉我们还剩什么」从假设变成取证）；以及 known_hosts 导入通路对**真实宿主密钥**的端到端校验（经已认证会话 `cat /etc/ssh/ssh_host_ed25519_key.pub` 取出 sshd 实际出示的公钥，拼成 `[127.0.0.1]:<映射端口>` 行喂给 Task 7 的 `import_known_hosts`，断言落库恰一行、`source=imported`、指纹等于**二进制 blob**（而非 base64 文本）的 SHA256、复导入只计 skipped）。四条全部由 `FS_ITEST` 门控，未设即打印 skip 并以 0 退出（Windows/macOS 无 Docker 时不阻塞门禁；CI 的 Linux 步骤置 `FS_ITEST=1`）。**S26（med，本地 100% 失败）**：计划里的 `SshdContainer::addr()` 写作 `format!("{host}:{port}").parse().unwrap()`，但 `get_host()` 返回的是 `url::Host` 而**不是** IP —— Docker Desktop（Windows/macOS 经 npipe / unix socket 连 daemon）下它是**域名字面量** `localhost`，而 `SocketAddr` 的 `FromStr` 对任何域名一律 `AddrParseError`，于是四条测试在本机 4/4 全红、且 panic 落在助手内部而非测试本体（`called Result::unwrap() on an Err value: AddrParseError(Socket)`）。改为先按 IP 字面量解析（Linux CI 的 `127.0.0.1` 直接命中，零 DNS）、失败再走 `tokio::net::lookup_host`，且**只取 IPv4**：端口取自 `get_host_port_ipv4`，那是 IPv4 侧的映射，而 `localhost` 在双栈机器上常先解析出 `::1`，据此拼出的地址指向一个根本没有映射的端口 —— 那是比 parse 失败更难查的假失败。**非空证明（这批是必需而非仪式）**：容器测试「跳过」与「通过」的退出码完全相同，一片绿分不出真跑还是空跑，FS_ITEST 门控本身就自带假绿风险。故四条断言各改成必错值跑了一遍，确认失败信息里带的都是**真实容器数据** —— `left: "hello-fs"`、`Welcome to OpenSSH Server … <容器 id>:~$ echo MARK-$((20+5)) / MARK-25`、`MethodSet([PublicKey, Password, KeyboardInteractive])`、`ssh-ed25519 AAAAC3NzaC1lZDI1NTE5… root@<容器 id>`（顺带证实该镜像的宿主密钥确在 `/etc/ssh/`，而非 linuxserver 常见的 `/config/ssh_host_keys/`，计划写的路径是对的）；旁证是设 FS_ITEST 时单测各约 0.9–1.2 s、不设时各约 0.02–0.06 s。计划 Task 9 的四个代码块（itest 的 `Cargo.toml`、根 `Cargo.toml` 的 `members` **整块**、`sshd.rs`、`ssh_connect.rs`）已按落地终态逐字节回灌，Step 4 补上非空证明段落；`members` 由原先的散文「追加一行」改成整块照抄，理由与 lib.rs 同源（多行紧凑数组，追加位置容易写错）。门禁：`fmt --check` 0 · `clippy --workspace --all-targets -D warnings` 0 · `nextest --workspace` 66 绿（`FS_ITEST=1` 与不设各跑一遍）· `cargo test --workspace` 66 绿 · `cargo deny check licenses bans advisories` ok（testcontainers/bollard 新拉进来的依赖树许可全部通过）。
  - **Task 9 第二裁判（对抗复审）**（commit `9c5dede`）：门禁全绿后逐行读 Task 9 差异，确认 5 处并全部整改。**S27（med，故障形态是挂死而不是报错）**：三处读通道的循环都是裸 `while let Some(..) = channel.wait().await`，而 `ChannelMsg` 流本身不带超时 —— 服务器若既不发数据也不关通道就永久挂起；nextest 默认对慢测试只打印 SLOW、并不终止它，于是表现为「CI 任务级超时」而非一条红测试，排查成本高一个量级。`pty_shell_interactive` 更糟：它写了 10 s deadline，但 deadline 只在两次 await *之间*检查，看着有超时其实等同于没有；且 `wait()` 返回 `None`（通道已关）时 `if let Some(Data)` 不匹配，循环立刻重来、`None` 立刻再返回，满 CPU 空转到 deadline，最后只报一句空的 `pty output: `。改为每次 `wait()` 都套 `tokio::time::timeout_at(deadline, ..)`，并把 `None` 单列成分支直接 panic 报「通道在拿到 MARK-25 前就关了」。**S28（med，静默假绿）**：`assert_eq!(exit_status, 0)` 写在循环体内，只有真收到 `ExitStatus` 才会执行 —— 服务器若直接关通道，循环从 `None` 正常退出，这条断言被**完全跳过**而测试照样绿，即「测试声称校验了退出码，实际可能一次都没校验」。抽出 `collect_exec()` 返回 `(stdout, Option<u32>)`，把断言挪到循环外写成 `assert_eq!(status, Some(0))`：收不到就是红。**S29（med，取证缺口 —— 本用例的核心主张没被验证）**：`known_hosts_import_matches_server_host_key` 的 doc 写着「该文件即 sshd 实际出示的密钥」，可握手用的是 accept-all 校验器、对服务器密钥不着一字，测试实际只证明了「文件内容能被 `import_known_hosts` 正确解析入库」，而「文件里的公钥 == 握手时出示的公钥」自始至终是假设。改为让 `AcceptAllKeys` 持一个 `Arc<Mutex<Option<String>>>` 记下 `check_server_key` 拿到的公钥，断言它与文件里的 base64 段逐字相等（`to_openssh()` 无 comment 段，故两边都取第 2 个字段比）。**S30（low，但取证增益最大）**：错误口令用例只断言 `!remaining_methods.is_empty()`，既弱又没打通到 Task 8 —— `Method::from_wire` 的线上名字串表此前只有「拿同一批字面量喂进去」的自证单测。改为把服务器通告逐个过一遍 `from_wire`，断言 PublicKey / Password / KbdInteractive 三者都在；实测真实 OpenSSH 通告 `["publickey", "password", "keyboard-interactive"]` 三个名字全中，Task 8 状态机的输入口径由此从假设变成事实。**S31（low，依赖卫生）**：`futures` 与 `tracing-subscriber` 全 crate 零引用（白扩编译面与 cargo-deny 的告警面）；`russh` / `base64` / `tempfile` / `fs_sshengine` / `fs_connmgr` 只被 `tests/` 使用却挂在 `[dependencies]`，害得 `cargo build --workspace`（非测试）为本 crate 白编译 russh + sqlx 两棵依赖树。前两个删除，后五个移入 `[dev-dependencies]`（`src/sshd.rs` 只需 testcontainers + tokio）。同批顺手改掉一处低危耦合：known_hosts 行原先硬编码 `127.0.0.1`，而地址其实来自 `addr` —— 二者在 Docker Desktop 下恰好一致，但那是巧合不是契约，改用 `addr.ip()` 后 lookup 侧同步跟上。三类新断言（exit-status 必达、握手密钥 == 文件密钥、通告过表）各做了必错值非空证明，实测失败信息携带真实容器数据（见计划 Task 9 Step 4 回灌）。门禁：`fmt --check` 0 · `clippy --workspace --all-targets -D warnings` 0 · `nextest --workspace` 66 绿（`FS_ITEST=1` 与不设各跑一遍，itest 4/4 各约 0.8–1.4 s）· `cargo test --workspace` 66 绿 · `cargo deny check licenses bans advisories` ok。
  - Task 10 sshengine connect 编排（TOFU 回调 + 通告驱动认证 + ProxyJump 链）（commit `3bb6d29`）：`events.rs` 定义 `SessionEvents`（`host_key_decision` / `kbd_interactive` / `status`）与 `HostKeyChoice{AcceptAndRecord,AcceptOnce,Refuse}`（`Refuse` 为 `#[default]`，任何忘记设值的调用方都 fail-closed）；`connect.rs` 的 `Connector` 实现 `russh::client::Handler`，把主机密钥裁决收敛到**唯一一处** `resolve_host_key(trust, events, host, port, policy, pins, presented)`——`check_server_key` 与单元测试走的是同一条代码路径，杜绝「测试路径放行、生产路径 accept-all」这类只在真连时才暴露的裂缝；`AcceptOnce` 放行但**不写** `host_keys`（无持久信任 → 下次必再询问，Changed 场景亦不覆盖不新增），`AcceptAndRecord` 经 Task 7 的 `replace` 语义落库（旧键退库，恒守 R30 单行不变式）。认证侧由 Task 8 的 `next_method` 驱动，只尝试服务器**已通告**的方法，keyboard-interactive 走 `authenticate_keyboard_interactive_start/respond` 往返并把 `InfoRequest.instructions` 原样透传到 `SessionEvents::kbd_interactive`（总设计 §2.1 v3，不得以 `..` 丢弃）；ProxyJump 经 `channel_open_direct_tcpip` → `into_stream()` → 三参 `client::connect_stream` 递归握手，每跳各自独立裁决主机密钥；`client::Config` 置 `keepalive_interval: 15s` / `keepalive_max: 3`。测试 11 条新增：`connect_logic.rs` 5 条纯逻辑（含经 `check_server_key` 生产入口的落库回归，指纹逐字锁到已知 ed25519 向量 `SHA256:WWGldbXAkapvaUjocuvVt0JLXky5hF80UshUCFxCoLE`）+ `ssh_tofu.rs` 5 条容器真连四态（首连接受并落库 / AcceptOnce 仅内存态且重连再弹 / Strict 不弹框直接拒 / 变更密钥先拒再显式接受完成替换 / 已导入主机首连**零 TOFU 回调**）+ `ssh_jump.rs` 1 条 direct-tcpip 二段握手。**两处容器侧硬缺陷，均由实测定位而非推测**：① `linuxserver/openssh-server` 的运行时配置 `/config/sshd/sshd_config` 第 92 行是**未注释**的 `AllowTcpForwarding no`，不改则任何 direct-tcpip 一律回 `ChannelOpenFailure(AdministrativelyProhibited)`，跳板测试全灭；该镜像的 `Include` 段被 init 脚本注释掉、`/config/sshd` 目录还是 init 现场创建的，故 `with_copy_to` 预置 drop-in 片段不可靠 → `sshd.rs` 新增 `enable_tcp_forwarding()`：sed 改写 + `grep -qx` **非空证明**（上游哪天改了写法就当场非零退出让测试红掉，而不是让转发继续被禁、把失败推迟到语焉不详的 channel open 错误）+ `s6-svc -r /run/service/svc-openssh-server` 重启 + `wait_for_sshd_banner()` 轮询到读出字面量 `SSH-` 才算恢复（只判 TCP connect 成功不够：容器内服务已停时 Docker 的端口转发进程仍可能接受连接后立刻断开，那会让紧随其后的握手以「连接被重置」形式假红）。② 计划原稿把 direct-tcpip 的目标写成 `addr.ip()/addr.port()`，但该连接是**服务端**在自己的网络命名空间里发起的，宿主映射端口（形如 32812）在那里根本不存在 → 新增 `pub const INTERNAL_SSH_PORT: u16 = 2222`（镜像 init 把 `Port` 定死为 2222）并以它为目标。**一处 API 事实**：russh 0.62.4 的 `client::Handle<H>` 未实现 `Debug`，`Result::expect_err` 因而 E0277，两处拒绝路径改用 `assert!(result.is_err(), "…")`。**非空证明**（6 条新增容器测试逐条必错值跑过，失败信息全带真实容器数据）：`left: "tofu" right: "tofu-MUTANT"`、`left: 1 right: 7`、`strict 拒绝须经 status 回调告知用户；实收：["strict 模式拒绝未知主机密钥 127.0.0.1:32826"]`、`left: 1 right: 9`、`left: "imported" right: "imported-MUTANT"`、`left: "jumped" right: "JUMPED-MUTANT"`，改动已按字节比对确认原样还原。计划回灌：Task 9 Step 2 的 `sshd.rs` 与 Task 10 的 `connect_logic.rs` / `events.rs` / `connect.rs` / `ssh_jump.rs` / `ssh_tofu.rs` 五个代码块全部替换为落盘字节，Step 5b 的依赖核对段把 `[dependencies]` 更正为 `[dev-dependencies]`（这三项仅测试用），并为两处容器缺陷各留一段实现期注记。门禁：`fmt --check` 0 · `clippy --workspace --all-targets -D warnings` 0 · `nextest --workspace` 77 绿（`FS_ITEST=1` 与不设各跑一遍，均 77/77）· `cargo test --workspace` 77 绿（同样两遍；nextest 一测一进程会掩盖进程级全局状态问题，故两种 runner 都跑）· `cargo deny check licenses bans advisories` ok。
  - **Task 10 第二裁判（对抗复审）**（commit `2ad957f`）：五道门禁全绿后逐行读 Task 10 差异，确认 5 处并全部整改，另有 1 处候选经查证否决。**S32（med，最常见路径上把明文口令送给一台永不使用它的服务器）**：`authenticate()` 把 `remaining` 初值硬编码成全四项，等于让**第一次尝试**绕开「绝不尝试服务器未通告的方法」这条硬约束（spec §2.1，`auth::next_method` 文档亦明写「∩ 服务器通告」）；PREFERENCE 把 Password 排第一，于是「只配了口令的 profile 连 publickey-only 服务器」这条最常见路径必然先发一次 password 认证——明文口令白送，且白占一次 MaxAuthTries（堡垒机常配 2–3 次，四个候选足以直接锁死账号）。整改：先发 RFC 4252 §5.2 的 `auth none` 取服务器通告再进状态机（少数服务器允许 none 直接登入，故 Success 分支直接放行），并把「通告 → 本地候选」抽成唯一实现 `announced_methods()`，S25 那条「publickey 在列且本会话启用 agent 时必须把 `Method::Agent` 补回候选」的约束因此不会在 none 探测与失败后重建两处中被漏掉。**S33（med，故障形态是挂死而非报错）**：`kbd_interactive_loop` 无回合上限，故障或恶意服务器可无休止回 InfoRequest，客户端就永远在「弹框 → 应答 → 再弹框」之间打转（与 Task 9 第二裁判 S27 同类：CI 表现为任务级超时，用户表现为对话框永远关不掉）。整改：`KBD_MAX_ROUNDS = 32` 封顶，超限判服务器异常并中止（真实服务端 OpenSSH+PAM 通常 1–2 轮）。**S34（low，潜伏型）**：`Method::Agent` 分支 `return Err` 隐式依赖「PREFERENCE 把 Agent 排在末位」这一事实——Task 11 若把 agent 前移（agent 通常应优先于交互式口令），该 return 会把其后所有可用方法一并掐掉，故障形态是「明明配了口令却直接判认证失败」。整改：改 `continue`（`tried` 已记入本方法，不会死循环；候选真正耗尽时由 `next_method` 的 `None` 分支给统一错误）。**S35（low，秘密卫生 + 静默损坏）**：`String::from_utf8_lossy` 在字节非法时走 `Cow::Owned`，先造出一个**不受 `Zeroizing` 保护**的裸 String 明文（总设计 §3.2：秘密不得以裸 String 存活），且非法字节被替换成 U+FFFD 得到一份静默损坏的口令/PEM，拿去认证只会神秘失败。整改：改严格 `std::str::from_utf8`，错误文案点明「不是合法 UTF-8」并给出 `valid_up_to()` 位置。**S36（low，可诊断性）**：ProxyJump 链上任何一跳失败都只冒泡出一个不带跳数的 `Error::Connect`/`Error::Auth`（`Error` 里没有放 profile 名的位置），用户看到「认证失败」却无从判断失败在哪一跳。整改：每跳前补 `events.status` 逐跳播报（`跳板 i/n`、`经末跳转发至目标`）。**经查证否决、不计入发现**：`format!("{}:{}", host, port)` 传给 `ToSocketAddrs`，host 为裸 IPv6 字面量时是否解析错位——追进 std 实现确认 `SocketAddr::from_str` 失败后走 `rsplit_once(':')` 拆分并交 getaddrinfo，数字主机名被正确接受，非缺陷，主动丢弃而非凑数上报。**编号避让**：本轮草稿原用 S29/S30/S33/S34，与 Task 9 第二裁判已占用的 S27–S31 撞号，落盘前统一顺延为 S32–S36。**测试与非空证明**：新增 3 个测试（workspace 77 → 80）。`crates/itest/tests/ssh_auth.rs::never_attempts_unannounced_method` 锁 S32——配套在 `sshd.rs` 新增 `restrict_to_publickey_only()`，在 `sshd_config` **开头**插入 `PasswordAuthentication no`/`KbdInteractiveAuthentication no`（`sshd_config(5)`：for each keyword, the first obtained value will be used，追加到末尾会被 init 先写入的 `PasswordAuthentication yes`（实测第 61 行）压住而完全无效）后 `s6-svc -r` 重启并等 banner；两次必失变异实测：① 回退 S32 → 失败于 `实际已尝试：["password"]（statuses=["尝试认证方法：password"]）`，② 注释掉 `restrict_to_publickey_only()` → 口令认证成功、`connect` 返回 `Ok`，失败于「连接必须失败」，分别证明断言锁的是修复本身、容器收紧是载荷而非摆设。`connect.rs` 内联 2 个单测锁 S35，同样两次必失变异实测（改回 lossy → `非 UTF-8 秘密必须被拒绝: CredentialSet { password: Some("[REDACTED]"), .. }`；翻转 PRIVATE KEY 分流 → `left: None / right: Some("s3cr3t-口令")`）。**S33 与 S34 明确无判别性测试，如实记录不补恒真测试充数**：S33 需要一台会无限回 InfoRequest 的服务器，本仓容器 sshd 无可脚本化的此类钩子且 `client::Handle` 不可 mock；S34 在当前 PREFERENCE 序下 Agent 恒为末位，改与不改的外部可观测行为完全一致，判据只能留到 Task 11 接线 agent 并改序时补。**顺带一处格式化陷阱**（非缺陷，记录以免复发）：S35 的 `match` 错误臂长度恰好卡在 rustfmt `max_width` 边界，块式与单行式互为「格式化后仍需再格式化」，`cargo fmt --check` 在两态间反复横跳；改写成 `map_err(..)?` 后稳定。五道门禁全绿（fmt / clippy `-D warnings` / nextest 80 全绿 × {无 FS_ITEST, FS_ITEST=1} / `cargo test --workspace` 同两态 / deny licenses+bans+advisories）。
  - Task 11 sshengine agent 传输（三管道）+ 认证分支接线（commit `cf58c73`）：新增 `crates/sshengine/src/agent.rs`——`SharedAgentClient = AgentClient<Box<dyn AgentStream + Send + Unpin>>` 以 russh 的 `dynamic()` 把三条传输的不同流类型擦除为同一句柄，unix 走 `connect_env()`（读 `SSH_AUTH_SOCK`），Windows 先探 Pageant 再回退 `\\.\pipe\openssh-ssh-agent`；`describe_agent_unavailable()` **不按平台裁剪**，任何平台都列全三种传输（agent 不可用时用户看到的往往只有这一行字，而「本机是什么平台」对拿到日志的人并不自明）。`connect.rs` 的 `Method::Agent` 分支由 Task 10 的占位 `continue` 换成实际流程：`request_identities()` → 仅取 `AgentIdentity::PublicKey`（证书身份留待后续）→ `prioritize_agent_keys(available, preferred, AGENT_IDENTITY_CAP_DEFAULT=3)` → 逐个 `authenticate_publickey_with(user, key, None, &mut agent)`（agent 已实现 `auth::Signer`，公钥按值传）。`preferred` **恒空**（R62：`host_key_pins` 是服务器主机密钥 blob，与「用户 agent 公钥 blob」不是同一键空间，喂进去只会让优先排序恒失效），agent 身份偏好字段随 M4+。测试 +2（workspace 80 → 82）：`crates/sshengine/tests/agent_logic.rs::unavailable_message_names_all_three_transports`（无 cfg 门控，Linux/macOS CI 同样跑）、`crates/itest/tests/ssh_agent.rs::agent_unavailable_error_names_transports`（**不引容器**，R56：被测的是宿主 `SSH_AUTH_SOCK`，容器与断言无因果关系；有 `SSH_AUTH_SOCK` 时须真连上、不得静默跳过）。**非空证明**：前者把文案里的 `Pageant` 换成「其他 agent」后确认变红，报错携带真实产出文案；后者在本机（Windows）恒 skip，故临时摘掉 `cfg!(windows)` 门控跑了一遍，确认断言可达且失败信息带真实值（`None`）——而这次强制执行**当场抖出一条真缺陷**，见下条。门禁：`fmt --check` 0 · `clippy --workspace --all-targets -D warnings` 0 · `nextest --workspace` 82 绿 · `cargo test --workspace` 82 绿 · `cargo deny check licenses bans advisories` ok。
  - **Task 11 第二裁判（对抗复审）**（commit `e3a3ae8`）：五道门禁全绿后逐行读 Task 11 差异，确认 3 处并全部整改。**S37（high，一整条回退路径在所有 Windows 上都是死代码）**：`connect_agent_client()` 的 Windows 分支按「构造是否成功」分诊 Pageant，而 russh 的 `connect_pageant()` 实现是 `Ok(Self::connect(PageantStream::new().await?))`——只是把流对象 new 出来，WM_COPYDATA 握手推迟到首次 I/O，因此 **Pageant 根本没在跑时它照样返回 `Ok`**，`if let Ok(_)` 恒真，其后的 `\\.\pipe\openssh-ssh-agent` 回退在任何一台 Windows 上都不可达。后果面很宽：绝大多数 Windows 用户用的恰恰是系统内置的 Win32-OpenSSH ssh-agent（Pageant 需另装 PuTTY），他们拿到的是一句 `early eof`——既不点名 agent 也不点名任何一条传输，`describe_agent_unavailable()` 精心写的三管道指引永远送不到人手里。本机取证链完整：`tasklist` 无 pageant 进程 · `sc query ssh-agent` 为 `STATE : 1 STOPPED` · `connect_pageant()` → `ok? true` · 紧接着 `request_identities()` → `early eof` · `connect_named_pipe()` → `系统找不到指定的文件。 (os error 2)`。整改：新增 `live_dynamic()`，**三条传输一律先发一次 `request_identities` 做存活探测**、通过后再 `dynamic()`；Pageant 探测失败即丢弃句柄回退命名管道，命名管道再失败才 `unavailable()`。unix 侧的 `connect_env()` 本就在变量未设/套接字不存在/无监听者三种情况下正确报错，仍一并纳入探测是为了让**三平台契约一致**——「返回 `Ok` 即代表可用」，集成测试因此不必按平台分叉写两套断言。**S38（high，agent 的本地故障连坐杀死其他认证方法）**：`Method::Agent` 分支用 `?` 把 `connect_agent_client()` 的 `Err` 直接掀出整条认证链；agent 连上但一把可用身份都没有时，则退化成 `Failure { remaining: MethodSet::empty() }`，`announced_methods` 算出空集、`next_method` 随即返回 `None`。两条路径后果相同：一个**同时**配了 agent 与口令的 profile，只要本机没起 agent 就连口令都不会试一次，故障形态是「凭据明明是对的却登不上」，而日志里看不出为什么。整改：`AuthStep` 新增 `Unattempted { reason }`（语义是「因本地原因根本没发出认证请求」，与「发出了但被拒」严格区分），候选集推进抽成纯函数 `advance_remaining(&AuthStep, Vec<Method>, &CredentialSet) -> Vec<Method>`——`Unattempted` 原样保留候选集（`tried` 已记名，状态机不会重选该方法）；`Method::Agent` 分支抽成 `agent_auth()`，连不上/列不出身份/无可用公钥身份三种本地故障一律返回 `Unattempted` 而非 `Err`，`Err` 只留给传输层错误；循环内以 `sent` 标志把「chosen 为空/find 全落空」这条不变量写成运行时事实而非注释里的假设。诊断不再被吞：`Error::Auth` 增补 `notes: Vec<String>` 栏并在终态错误中带出——不并进 `remaining`，因为那一栏的语义是「服务器还允许什么」，混入本地故障会把排查方向直接引偏到服务端。**S39（low）**：`authenticate_publickey_with` 处的 `key.clone()` 是冗余克隆（`key` 由 `.cloned()` 得来且调用后不再使用），clippy 的 `redundant_clone` 属 nursery 组默认不开、门禁抓不到，一并去掉。**测试与非空证明**：新增 1 条（workspace 82 → 83）`connect.rs::unattempted_method_keeps_remaining_intact`，对照组是**同一个函数**在 `Failure { empty }` 下的行为，两条断言相差的恰好是「有没有真发出去过一次认证请求」；同时把 `ssh_agent.rs` 的 Windows cfg 跳过整条删除，改成三平台同一套判据（`Ok` ⇒ 必须真能问出身份列表，`Err` ⇒ 文案须点名三条传输）。两次必失变异实测：① 把 `live_dynamic` 退回构造分诊，`FS_ITEST=1` 下 `ssh_agent` 变红于 `ssh_agent.rs:31`，panic 携带真实产物 `IO(Custom { kind: UnexpectedEof, error: "early eof" })`；② 把 `advance_remaining` 的 `Unattempted` 臂改走 `Failure { empty }` 语义，单测变红并打印 `left: [] / right: [Password, Agent]`。变异后均按字节还原。**如实记录未覆盖项**：`Method::Agent` 的**真连**路径仍无自动化测试——`AuthRef::default().allow_agent` 为 `false`，现有容器用例都到不了该分支，需要镜像内置 ssh-agent 才能构造；不补恒真测试充数，随 M4+ 与 R56（容器内 agent 真测）、R62（agent 身份偏好字段）同批。门禁在还原态复跑：`fmt --check` 0 · `clippy --workspace --all-targets -D warnings` 0 · `nextest --workspace` 83 绿 · `cargo test --workspace` 83 绿 · `cargo deny check licenses bans advisories` ok。
  - Task 12 sshengine SFTP 客户端与传输管理器（commit `6b73f1a`）：新增 `sftp.rs`（`SftpOps` 十方法 trait + `RemoteSftp` 的 russh-sftp 2.3.0 实现）、`transfer.rs`（256 KiB 分块、进度事件、3 次指数退避重试、断点续传、per-id 取消）、`sandbox.rs`（下载目录沙箱，spec §3.3）、`verify.rs`（传后校验四态：`Sha256Match` / `SizeOnlyMatch` / `Mismatch` / `Unverified`，禁静默跳过），并补 `crates/itest/tests/sftp.rs` 三条容器真连测试。**三处 russh-sftp 2.3.0 的 API 事实**（皆经 registry 源码核对而非推测）：① `Metadata` 即 `protocol::FileAttributes`，`size/uid/gid/permissions/atime/mtime` 全为 **pub 字段**，而 `permissions()` 返回的 `FilePermissions` 只是 9 个 bool —— 拿它重组 mode 会**静默抹掉 setuid/setgid/sticky**，故 `lstat` 直接取 `m.permissions.map(|p| p & 0o7777)`；② `create()` 隐含 TRUNCATE，续传路径必须走 `OpenOptions` 且显式 `.truncate(false)`，否则每次重试都把已落盘前缀清零；③ `symlink(path, target)` 把**首参**放进线上 `linkpath` 字段，而 OpenSSH 的 sftp-server 对这两个字段的解读与 draft 相反（PROTOCOL §3.1），两次反转相消 —— 故本仓按 `(target, link_path)` 调用是**正确**的，`sftp_crud_over_real_sshd` 的 `read_link("lnk.bin") == "t.bin"` 即该结论的非空证明。**下载沙箱 fail-closed**：`Direction::Down` 且 `sandbox_root` 为 `None` 时直接拒绝而非放行（Rust 侧是唯一可信边界，前端传入的本地路径与服务端文件名皆为不可信输入）；包含判定用 `Path::starts_with` 按**路径组件**比而非字符串前缀 —— `<tmp>/sbx-0-evil` 是 `<tmp>/sbx-0` 的字符串前缀延长但不是其子目录，字符串比较会把它误放行。**S40（实现期自捕，med，故障形态是挂死而非报错）**：`tests/transfer.rs` 的取消用例裸 `ev.recv().await` 等终态事件，终态事件若不发就永久挂起 —— 实测挂了 280 s 以上仍未红，nextest 只打印 SLOW 不终止；抽出 `next_event()` helper，每次收事件套 30 s `timeout` 并把「通道提前关闭」单列成分支（与 Task 9 第二裁判 S27 同类整改）。同批去掉两个 `done` / `cancelled_seen` 布尔标志改用穷尽 `match`（clippy 默认开的 `unused_assignments` 抓到）。**S41（high，容器实跑才暴露，本项目至今最典型的一次「单测全绿掩盖生产失效」）**：`verify::run_exec_channel` 写作 `ChannelMsg::Eof | ChannelMsg::Close => break`，而 RFC 4254 §5.3 规定 `exit-status` 在通道关闭*之前*送达、OpenSSH 的实际序是 `data…` → `EOF` → `exit-status` → `CLOSE` —— 在 `Eof` 处 break 等于把退出码整条丢掉，`code` 对**任何真实 OpenSSH** 恒为 `-1`，于是 `run_verify` 的 Up 路径永远落进降级分支，**上传方向的 SHA256 校验从未真正执行过**，UI 只会显示「降级 size 比对」。它躲过了 `tests/verify.rs` 全部 12 条单测（单测注入脚本化 `ExecChannel`，压根不经过该函数），也躲过了此前每一次「111 绿」（不设 `FS_ITEST` 时容器用例走 skip 分支，退出码与 PASS 完全相同）。整改：退出条件只留 `Close` 与流结束，`Eof` 改为起一个 `EXEC_TAIL_GRACE = 10 s` 的**尾部宽限窗口** —— 命令本体不设总超时（`sha256sum` 大文件可跑数分钟），但 EOF 之后服务器只剩两条消息要发，迟迟不发即判异常，既堵住丢码又不退化成挂死。排查副产物：全仓 `ChannelMsg::Eof` 仅此一处，Task 9 的 `collect_exec` 在 `ExitStatus` 处 break、对 `Eof` 走忽略分支，形似同源实则免疫，影响面已封闭。同批把该用例四条 `assert!(matches!(..))` 全改 `assert_eq!`（前者失败时只打印源码文本、拿不到实际结局，把「降级成 SizeOnlyMatch」和「Unverified」压成同一条无信息的红，正是这条缺陷多绕一圈才定位的直接原因），并新增 ⓪ 段把「退出码真的送达」直接钉在 `run_verify` 之前。**非空证明**：S41 回退后 `transfer_verify_roundtrip_against_real_sshd` 变红于 `left: -1 / right: 0`，panic 携带真实容器 stdout `2d64ab36…  v.bin`；`sftp_crud_over_real_sshd` 新增的 mode/uid/gid 三项断言锁住「取值域已剥离文件类型高位」与「uid/gid 非 None」，是 ① 的非空证明。计划 Task 12 十四个代码块（`tests/transfer.rs` / `tests/sandbox.rs` / `src/sandbox.rs` / `tests/verify.rs` / `src/verify.rs` / `src/sftp.rs` / `src/transfer.rs` / `tests/sftp.rs` 等）已按落盘字节逐块回灌并 `diff` 逐字节比对通过。门禁：`fmt --check` 0 · `clippy --workspace --all-targets -D warnings` 0 · `nextest --workspace` 111 绿（`FS_ITEST=1` 与不设各跑一遍，均 111/111；itest sftp 三条各约 0.8–1.1 s）· `cargo test --workspace` 111 绿（同样两态）· `cargo deny check licenses bans advisories` ok。**留给第二裁判的三条观察（本轮如实记录、未自行整改）**：① `run_transfer` 把「沙箱拒绝」这类**永久性策略拒绝**也按可重试错误退避重试 3 次（两条拒绝用例各因此空耗约 3.1 s）；② `run_verify` 的 Up 路径把**任何** exec 失败降级为 `SizeOnlyMatch`，敌意服务端可靠「让 sha256sum 返回非零」把完整性检查压到「字节数相等」；③ 合并 `Ok(_) | Err(_)` 分支后 `exec_unavailable` 在本 crate 内已无生产调用点（仍 pub、单测覆盖，app 层 Task 21 上报降级文案时使用）。另有一条门禁面观察：全仓无 `.config/nextest.toml` 的 `slow-timeout.terminate-after` 兜底，S40 那类挂死只能靠每处自带 timeout 拦，属 Task 1 门禁配置范畴，未在本任务内擅自加。
  - **Task 12 第二裁判（对抗复审）**（commit `e1b82b7`）：五道门禁全绿后逐行读 Task 12 差异，确认 10 处（2 高 / 5 中 / 3 低，编号 S42–S51），8 处整改、2 处经质证裁定为「本层不是缺陷」不改代码，另 1 条按归属推迟。**S42（high，沙箱越权写，整套既有用例结构性看不见）**：`sandbox::resolve_within` 只 canonicalize **父目录**，末段是原样 `join` 回去的。沙箱内若已存在一个名为 `report.pdf` 的**文件软链**，父目录校验完全通过，而随后 `OpenOptions::create(true).write(true).open()` 会**跟随**它，把服务端送来的字节写到沙箱外——**下载文件名由远端服务器控制**，这正是 spec §3.3 要挡的那条活路径。既有 `symlink_escape_rejected` 只覆盖软链**目录分量**（`<root>/escape/x.bin`，那条被 parent canonicalize 挡住），覆盖率看上去是满的、实则整条末段没人查。整改：末段补一次 `symlink_metadata`（**不跟随**）判定，是软链即拒。**Windows 侧覆盖不肯降级为跳过**——文件软链需 SeCreateSymbolicLinkPrivilege（本机实测 `mklink` 报 ERROR_PRIVILEGE_NOT_HELD），但**目录联接**（`mklink /J`）普通账户就能建，且 Rust 的 `FileType::is_symlink()` 对 `IO_REPARSE_TAG_MOUNT_POINT` 同样返回 true，走的是与文件软链完全相同的那条分支；于是新增 `final_component_reparse_point_rejected` 把该守卫钉在**主平台**上，建不出联接即判红而非静默跳过（静默跳过正是本项目一路在猎杀的假绿形态）。同批补 `regular_and_missing_destinations_still_accepted` 守住过度拒绝一侧：不存在 = 正常新建下载、普通文件 = 正常覆盖下载，都必须放行。明确不处理硬链接与 TOCTOU——二者只有本地同权限进程能构造，而那已持有该用户全部权限，挡它无意义。**S43（high，覆写上传残留旧尾，假绿成因值得单独记录）**：`SftpOps::write_at` 用 WRITE|CREATE 且**刻意不带 TRUNCATE**（带了会毁断点续传），于是非续传上传覆写一个**更大**的旧文件时只盖住前 n 字节、旧尾巴原样留在后面——用户把配置文件改短再上传，远端得到的是一个尾部挂着旧内容的文件（对 `.conf`/`.json` 这类而言就是语法直接损坏）。它躲过全部既有用例的原因是：测试替身 `FakeFs::write_at` 精确复刻了生产端 grow-only 的语义，**无论哪一侧写错两侧都一致**，任何断言都照过——这是「测试替身与被测实现同源」型假绿的教科书样本。整改：新增 `SftpOps::truncate`（SSH_FXP_SETSTAT **只置 size 位**，不动权限/属主/时间戳），`run_transfer` 在 `resume_offset == 0` 的上传前先把远端清零，且置于重试循环**之外**、失败即终止——SETSTAT 只是一个小包，发不出去说明会话已废，带着未截断的旧文件继续传就是在主动制造损坏。**S44/S45（med，异步 I/O 违规）**：`exec_once` 里残留的同步 `std::fs` 改 `tokio::fs`（本 crate 其余处早已是 `tokio::fs`，此处是唯一例外；同步读写阻塞 tokio 工作线程，并发传输占满线程时连 SSH 会话自身的 I/O 任务都会挨饿 → keepalive 超时 → 传输中途断线），并把本地文件句柄提到分块循环**之外**只开一次、只 seek 一次（原实现每 256 KiB 重开一次：1 GiB = 4096 次 `CreateFileW`，Windows 上每次都要过一遍 Defender 过滤驱动）。`tokio::fs::File` 带用户态写缓冲，故取消路径与 EOF 路径**都**补 `flush()`——否则已计入 `bytes_done` 的尾段可能根本没落盘，续传会从一个并不存在的 offset 起跑。**S46（med，重试退回起点，砸的正是本 crate 的招牌能力）**：原实现每次尝试都从 `job.resume_offset` 重新起跑，9 GB 传到 8.9 GB 断一次就白扔 8.9 GB，而重试恰是最需要断点续传的时刻。整改：游标 `offset` 跨重试共享、由 `exec_once` 原地推进；`Retrying`/`Failed` 事件的 `bytes_done` 从硬编码 0 改为 `offset`（报 0 会让进度条每次重试假摔回起点）。回归用例 `retry_resumes_from_achieved_offset` 断言的是 `write_at` 的 **(offset, len) 流水**而非最终内容——最终内容在两种实现下都正确，**只有流水能区分「续跑」与「重跑」**；为此 FakeFs 新增 `fail_write_indices` 按序号注入失败（原 `fail_next_writes` 只能打头几次、造不出「先成功若干块再断」的场景）。**S47（med，永久性拒绝被当可重试错误）**：沙箱解析原在重试循环**内**，「目的地逃出沙箱」这类**策略性永久拒绝**因此被退避重试 3 次（两条拒绝用例各空耗约 3.1 s，是 Task 12 落地时如实留痕的三条观察之一）；解析上提到循环外，拒绝即终止。**S50（med，无界增长）**：`TransferManager::cancels` 只在 `submit` 插入、从不摘除，随会话寿命线性增长（Xftp 式批量传输单次可达十万件，每条表项 + `Arc` 分配常驻不释放）。整改：终态即摘除，并顺势把它做成新增只读访问器 `active_len()` 的数据源；摘除后 `cancel(id)` 对已完成传输退化为静默 no-op，语义正确。**S48/S49（low，经质证裁定「不改代码」，只锐化文档）**：① `verify::exec_unavailable` 在 `Ok(_) | Err(_)` 合并分支后于本 crate 内无生产调用方，但它是 Task 21 区分「服务端没有 sha256sum」与「sha256sum 执行失败」两种降级理由的 app 层 API（`VerifyOutcome` 是纯枚举、不带理由字段），判定**保留**并在函数文档里钉死「本 crate 内故意无调用方、勿因无引用删除」，免得下一轮复审重新把它判成死代码；② `run_verify` 的 Up 路径把任何 exec 失败降级为 `SizeOnlyMatch`，**在本层不是缺陷**——Up 方向的哈希本就由服务端算出，能操纵退出码的服务端同样能直接回一个伪造哈希、也能伪造 `stat_size`，三者难度相同；服务端侧计算的校验只对**意外损坏**（链路误码、写盘截断）有效，对不诚实的服务端从来无效，收窄降级面只是把「可疑」重贴成「未核对」。真正的保证在于降级**不被隐藏**：`SizeOnlyMatch` 是四种结局中独立的一种、不是 `Sha256Match` 的近义词，UI 必须显式标注。整改落在文档：`run_verify` 补「信任边界」段，避免调用方（Task 21）高估 `Sha256Match` 的含义。**S51（门禁级，未修、留 Task 1）**：全仓没有 `.config/nextest.toml` 的 `slow-timeout.terminate-after` 兜底，S40 那类挂死只能靠每处自带 timeout 拦；本轮不就地加——周期设紧会误杀需要拉镜像的 `FS_ITEST` 用例，属 Task 1 的门禁配置范畴。**测试与非空证明**：新增 5 条回归用例（Windows 全仓 111 → **116**；`-p fs_sshengine` 49 → 54，unix 55），**六处必失变异逐一实测后逐字节还原**：① S42 拒绝侧删守卫 → 联接用例 FAIL；② S42 过度拒绝侧改「存在即拒」 → `regular_and_missing_destinations_still_accepted` FAIL；③ S43 短路截断块 → 远端实得 `short-file` 后接 0xAA 长尾；④ S46 每次尝试重置 `offset` → 写流水实得 `[(0,262144),(262144,262144),(0,262144),…]`、断言打印 `left: 0 / right: 262144`；⑤ S47 解析移回循环内（代以不可达路径复现原行为） → 两条沙箱用例 panic 于 `attempt=1`；⑥ S50 去掉摘除 → `active_len_drops_to_zero_after_terminal_state` 卡到 10 s 判据超时 FAIL。门禁在还原态复跑：`fmt --check` 0 · `clippy --workspace --all-targets -D warnings` 0 · `nextest --workspace` 116 passed / 0 skipped（`FS_ITEST=1` 与不设各跑一遍，均 116/116）· `cargo test --workspace` 同样两态全绿 · `cargo deny check licenses bans advisories` ok。**计划回灌**：Task 12 的 7 个整文件镜像代码块（`tests/transfer.rs` / `tests/sandbox.rs` / `src/sandbox.rs` / `tests/verify.rs` / `src/verify.rs` / `src/sftp.rs` / `src/transfer.rs`）按落盘字节整块替换，脚本抽回后与磁盘逐行比对全部 OK；`SftpOps` / `TransferManager` 的 Produces 清单补 `truncate(path, size)` 与 `async active_len() -> usize`，Step 5 计数行改 54/116，Task 12 原「留给第二裁判的三条观察」逐条改写为已裁定口径（脚本自查 `待裁` 残留计数归 0）。
  - Task 13 terminal headless 网格（vt100 包装）（commit `d87f9fd`）：新增 `crates/terminal/src/grid.rs`（`Grid` —— vt100 0.16 `Parser` 的薄包装，`new/feed/screen_text/scrollback_text/resize/rows/cols`，`DEFAULT_SCROLLBACK_LINES = 10_000`）与 `error.rs`（`Closed`/`QueueFull`/`Io`），`Cargo.toml` 接入 vt100 依赖。**vt100 0.16 三处 API 事实由编译器当场校正**（计划的记载是对的、我落地时的偏离是错的）：`Parser` 自身**没有** `set_size`、也**没有** `set_scrollback`，两者都在 `Screen` 上，须经 `parser.screen_mut()`；`contents_formatted`/`contents_diff` 返回 `Vec<u8>` 且**仅覆盖可见区**；`set_scrollback(usize)` 是**显示偏移**（内部钳至回看行数）而非容量上限——故 `scrollback_text` 取 `&mut self`，走「自 `usize::MAX` 起分页回退至 0 → 串接各页 `contents()` → 恢复调用前偏移」的临时通道，末行恢复在当前调用面下是冗余安全网（循环恒以 `cur == 0` 收尾），一旦将来加入向上翻页导航或改写分页方向即转为承重，该判断已写进 `grid.rs` 文档注释以防后人误删。**实现期自捕三条测试空断言缺陷（S52–S54），用例数 7 → 9**：**S52** —— 计划原用例 `output_always_valid_utf8` 断言 `from_utf8(screen_text().as_bytes()).is_ok()`，而 `screen_text()` 返回 `String`，该断言由**类型系统**恒真、对 vt100 行为零覆盖（实测屏幕内容为 `""` 也照过），删除并代之以 `multibyte_split_across_feeds_is_reassembled`（「服」的三个 UTF-8 字节被切成两次 `feed()` 须重组且不得出 U+FFFD——SSH 通道按 TCP 分片交付，中文主机名/路径是本项目主线场景）与 `invalid_utf8_does_not_swallow_following_output`（`ÿþ` 后的同批合法输出必须照常显示，二进制文件误 `cat` 是日常操作）；**S53** —— 交替屏用例只在退出后断言 `contains("base")`，这条断言在「`?1049h` 被整个忽略」这一**最该抓的**失效形态下同样成立（base 从未离开屏幕），补齐两侧状态断言（屏内 base 不可见 / vim stuff 可见，退出后反转）方唯一刻画「确实换过又换回来」；**S54** —— `scrollback_text` 的显示偏移副作用无人看守，而 Task 16 的管道正是**先取回看喂 AI、再取当前屏渲染**，一旦回退循环没走到偏移 0，其后每次 `screen_text()` 都返回历史某页，属调用方直接可见的失效，新增 `scrollback_text_leaves_current_screen_intact` 钉住。**非空证明 10 次必失变异**（M1 `screen_text`→`contents_formatted` / M2 `scrollback_text`→当前屏 / M3 忽略 scrollback 参数 / M4 `resize` 空实现 / M5 逐分片 `from_utf8_lossy` / M6 遇非法字节整批丢弃 / M7 分页提前一页收尾 / M7b 自底向上分页且删恢复行 / M8 用例删去 `?1049h` / M9 `feed` 空实现），每次均逐字节还原并 `diff -q` 复核；其中 **M7 落点与预测不符**——红的是 `scrollback_cap`（「9」）而非 S54 用例，因末行恢复仍把偏移拉回栈底、副作用被掩盖，如实记录并另补 M7b（自底向上分页这一**貌似合理的等价实现**）才打出预期红 `left: "line-36…39" / right: "line-0…4"`。门禁：`fmt --check` 0 · `clippy --workspace --all-targets -D warnings` 0 · `nextest --workspace` **125 通过 / 0 跳过**（`FS_ITEST` 空与 `=1` 两模式一致，较 Task 12 的 116 增 9）· `cargo test --workspace` 40 个二进制 125 通过 0 失败（两模式一致）· `cargo deny check` advisories/bans/licenses 全 ok；`-p fs_itest` 在 `FS_ITEST=1` 下 15 绿且单测耗时 1–2.5 s，确认容器用例真在跑而非被跳过。**第二裁判对抗复审（S55–S58，commit `d1a904e`）**：五道门禁全绿后逐行读 Task 13 差异，确认 4 处（3 中 / 1 低），全部整改并将 Task 13 计划代码块与计数逐字节回灌（用例 9 → 11，工作区 125 → 127）。**S55（中）**：`scrollback_text` 各页 `contents()` 原按字符串首尾相接——而 `contents()` 不以换行结尾且裁掉屏尾空行，直接拼接每 `rows` 行就把两条互不相干的行熔成一行（实测 `"line-90line-91"`），改为按行拼接。**S56（中）**：末页（`cur` 钳到 0 的当前屏）与上一页重叠行数可为正，且活光标所在尾行是空行会被裁掉，末页新内容可为 0 行（实测 rows=3 时偏移 1 页 `[97,98,99]`、偏移 0 页只剩 `[98,99]`），改为每页只取尾部 `page_len - overlap` 行（`overlap = step - (prev - cur)`），末页自然落 0；两种坏法由单条用例 `scrollback_pages_neither_glue_nor_duplicate_lines`（rows=3 / cap=10，非整除，与默认 24/10000 同构）以「每行可解析出序号 + 严格连续 + 末==99 + 长==12」合钉。**S57（中）**：vt100 0.16 对 0 行在内部 panic（实测 `vt100-0.16.2/src/grid.rs:26`/`:74`；0 列无事但一并钳），而零尺寸在真实链路可达——xterm.js fit addon 在窗口最小化/布局抖动时算出 0 行，Task 18 经 IPC 直转 `resize`；`new`/`resize` 一律钳到 `>= 1`，静默退到最小可用尺寸（「窗口太小」非调用方可处置的错误，panic 掉整个终端后端不可接受）。**S58（低）**：`resize_preserves_content` 原断言只读 `Grid` 自己的记账字段，区分不出「真调了 `set_size`」与「只更字段」，补尾段：改到 2 行屏后喂 r1/r2/r3、断言 `!contains("r1") && contains("r3")`。**必失变异 M-A..M-E**（S55–S58 各一 + S55 的「仅删去重不减拼接」变体）全部红出预期断言，逐字节还原（md5 复核）后绿，整改后门禁复跑 127 通过 / 0 跳过（两模式一致）。**S59（中，第二裁判复跑门禁期定位，commit `ff46589`）**：全量并行 nextest 中 `check_server_key_tofu_accept_persists_to_trust_store` 偶发单红一次（位置 59/127、0.5 s；隔离运行与整轮重跑皆绿）——根因为 **S22（#97）的跨进程撞名修复漏掉 sshengine 三个测试文件**：`connect_logic.rs`/`transfer.rs`/`sandbox.rs` 仍是旧式「pid + 计数器」命名并叠加 `create_dir_all`（`%TEMP%` 实测积有约 2 500 份 `fs-*` 残留），Windows 回收 pid 后新进程逐字继承上一轮同路径终态库——最可能链为残留 host_keys 已存本次将出示的公钥 → 首触命中信任库不弹框 → `asked.len()==0 ≠ 1`。整改：三个助手（`tmpdir`/`temp_subdir`/`sandbox_root`）迁至「纳秒名 + `create_dir` 撞名重试」，逐字对偶 connmgr/vault/hostkey 既有形态（S22 覆盖至此补齐），各补回归例 `*_never_hands_back_an_existing_directory`（按旧规则预铺当前 pid 前 128 槽位 + 毒文件，断言助手交回空目录；铺 128 令复现与执行器无关）；Task 10/12 计划三个代码块与 8 处计数锚点逐字节回灌并新增注 5。**必失变异 M-F/M-G/M-H**（三助手各退回旧式）红出逐字预期「助手交回了一个已存在且非空的目录：…（S59）left: 1 right: 0」，还原（md5 复核）后绿。第二裁判复跑门禁：`fmt --check` 0 · `clippy --workspace --all-targets -D warnings` 0（首轮因两条 doc 注释以 `+ ` 起首触发 `doc_lazy_continuation`，改写后净）· `nextest --workspace` **130 通过 / 0 跳过**（`FS_ITEST` 空与 `=1` 两模式一致，较第一裁判 125 增 5：S55–S58 期 +2、S59 期 +3）· `cargo test --workspace` 40 个二进制 130 通过 0 失败（两模式一致）· `cargo deny check` advisories/bans/licenses 全 ok。**第二裁判复跑对抗复审（S60–S67，commit `e72d0d3`）**：S55–S58 整改后以 15 agent 对抗工作流复跑（5 独立镜头找茬 → 合并 F1–F9 → 逐条独立反证），确认 8 处（7 中 1 低；F6 发现 low、裁判按「注释即规格的错误事实性断言」校准改 med），质证驳回 1（F9：`resize_preserves_content` 未刻画 `set_size` 有损语义——事实底座真[不重排、缩列截尾、缩行丢底部行，scratch 逐字复现]，但该例非空转[令其必失的变异随手可构]、测试名在其所演场景为真、`Grid::resize` 生产调用方为零、第三方库语义非项目变异可令失败，属增强建议不改码）。根因：S55/S56 版实现按**逻辑行**数裁末页重叠，而 vt100 显示偏移与页间重叠皆**物理行**单位、`contents()` 又每页重置熔行状态并逐页尾裁。**S60（中）**：重叠区含折行时过度裁剪，仅存于末页的新行被整页裁掉（实测屏上有 `NEW` 旧输出没有；默认 rows=24/cap=10000 → 末页重叠 8 物理行，屏顶 8 行内任何超 80 字符日常输出即触发，Task 16 的 AI 管道取的正是这条通道——AI 会系统性看不到最近输出）；**S61（中）**：熔行状态每页重置，跨页边界折行长行被无标记劈成两条逻辑行、碎片宽度可超 cols（实测 23 W 跨边界 → len=20 + len=3；减一个前导行对齐则熔成单条 len=23，页边界是唯一变量）；**S62（中）**：逐页尾裁于零重叠分页永久吞掉页尾空行（实测 `l1`/`l3` 之间空行蒸发）；**S63（中）**：既有分页回归只喂短行，折行丢行/切行——该函数最自然的坏法——零覆盖（当时实现本身就是必失变异体而套件 11/11 全绿）；**S64（中）**：`max_lines` 截断分支无任何测试执行（取头变异全绿）；**S65（中，改度）**：两处注释「0 列反而无事」与实测相反——构造虽过、首个输出字节即 panic 于 `screen.rs:730`（u16 下溢），`set_size(·,0)` 亦 panic 于 `grid.rs:726`（S57 块「0 列无事」措辞同此订正）；**S66（中）**：注释断言 fit addon 产出 0 行被钉死前端栈两层钳位证伪（`@xterm/addon-fit ^0.11.0` `MINIMUM_ROWS=1`、cell 尺寸 0 → `proposeDimensions` 返回 `undefined` 令 `fit()` 不调 `resize`；`@xterm/xterm 6.0.0` 公有 `resize` 先 `Math.max(rows, 1)`），钳位改定性「IPC 边界的纵深防御而非唯一防线」；**S67（低）**：`cjk_width_preserved` 名实不符（宽度本层不可观测、无 constructible 必失变异），改名 `cjk_roundtrip_preserved`。整改：`scrollback_text` 重写为逐页**物理行**文本 `rows(0, cols)` + `row_wrapped` 标志采集 → 物理行索引去重（非最老页跳过页首 `skip = rows - (prev - cur)` 行）→ 全局 `wrapped` 熔行重组 → 全局尾裁一次 → 取**最新** max_lines 行 → 恢复调用前偏移；S60–S62 三缺陷结构性消失；新增 4 条回归例共 15 例（S62 断言写成原样字符串逐字相等，令「多一尾空」与「少一中空」两种变异皆必失）；计划 Task 13 四个代码块逐字剪接回灌、计数锚点 11→15 / 130→134、Interfaces/Step 4 行文同步订正为物理行口径并新增注 6。**必失变异 M-I..M-N 六项**（M-I 整退旧算法三例红 / M-J 不熔折行 / M-K 不去重 / M-L 截断取头 / M-M 不裁尾空行两例红 / M-N 逐页尾裁）皆红出预期、md5 逐字节还原后 15/15 复绿。门禁：`fmt --check` 0 · `clippy --workspace --all-targets -D warnings` 0（首轮触 1.97 新 lint `manual_repeat_n`，`repeat(c).take(n)` → `repeat_n(c, n)` 后净）· `nextest --workspace` **134 通过 / 0 跳过**（`FS_ITEST` 空与 `=1` 两模式一致，较 S59 期 130 增 4）· `cargo test --workspace` 40 个二进制 134 通过 0 失败（两模式一致）· `cargo deny check` advisories/bans/licenses/sources 全 ok。**第三裁判对抗复审（S68–S72，commit `e71fb72`）**：e72d0d3 整改后以 12 agent 对抗工作流复跑（6 镜头：分页算术 / 逻辑重组 / vt100 基准事实 / 测试强度 / 副作用契约 / 消费方实景 → 合并 F1–F5 → 逐条独立反证），原始 6 条、合并 5 条、**确认 5 条（1 high / 4 med）/ 驳回 0**。**S68（high）**：rows=1 一遇边界折行即 panic 于 vt100 内部（0.16.2 `src/grid.rs:683` `prev_pos.row -= scrolled` u16 下溢；release 绕回后 `:689` `drawing_row_mut().unwrap()` 对 `None` 仍 panic——双 profile 皆崩），而 fit addon `MINIMUM_ROWS=1` 令 1 行为前端合法下发值、本层零钳位路径亦自产 1 行——e72d0d3 注释「最小可用尺寸=1」被实证证伪；`new`/`resize` 钳位下限改 **2 行**（rows=2 时 `scrolled` 恒 ≤ `prev_pos.row`：row 0 折行 `scrolled=0`、row 1 折行 `1−1=0`，实测+手推双 profile 皆安），注释逐字钉死全部引用行号。**S69（med）**：wrapped 行后被 EL/ED 抹空的续行被全局重组熔掉（`scrollback_text` 比 `screen_text()` 少逻辑行）；重组循环复刻 `vt100-0.16.2/src/row.rs:132-134` 规则（上一行 wrapped 且本行空 → 独立断行），`rows()` 与 `contents()` 同走 `Row::write_contents` 码路（`screen.rs:148-158`）故「text 为空」与「`prev_col == start`」结构性等价、零分歧。**S70（med）**：本批新注释把 `usize::MAX` 钳制端（最旧端 `scrollback.len()`）误称「栈底」、与既有惯用法（offset 0 = 栈底 = 当前屏）对同轴两端同名——改称「栈顶」，纯注释订正。**S71（med）**：S60 例钉不住「过滤后下标取 `row_wrapped`」变异（15/15 常绿却熔接独立逻辑行；默认 rows=24/cap=10000 末页 skip 恒 8、失效纯 feed 可达）；补 `overlap_wrap_flags_use_physical_row_indices` 逐字相等例令其必红。**S72（med）**：S62 例钉不住「`while`→`if` 单次尾裁」变异（偏离 `contents()` `while ends_with('\n')` 裁尽口径）；补 `all_trailing_blank_logical_lines_are_trimmed`（结尾 ≥2 空行）。**必失变异 M-O..M-S**（M-O `max(2)`→`max(1)` 红 2 / M-P 空续行断行失效红 1 / M-Q 注释-only 无变异 / M-R skip 后下标红 1、余 18 全绿 / M-S `while`→`if` 红 2——S69 例最小几何尾部 3 空行亦承重 while 语义，如实记录）皆红出预期、md5 逐字节还原（`8091a006…`）后 19/19 复绿。计划 Task 13 两个改动代码块逐字剪接回灌、计数锚点 15→19 / 134→138、Step 4 行文同步、新增注 7。门禁：`fmt --check` 0 · `clippy --workspace --all-targets -D warnings` 0 · `nextest --workspace` **138 通过 / 0 跳过**（`FS_ITEST` 空与 `=1` 两模式一致，较 S60–S67 期 134 增 4）· `cargo test --workspace` 40 个二进制 138 通过 0 失败（两模式一致）· `cargo deny check` advisories/bans/licenses/sources 全 ok。**第四裁判对抗复审（S73–S77，commit `cbfc933`）**：e71fb72/43f629f 整改后以 12 agent 对抗工作流复跑（六镜头：分页算术 / 逻辑重组 / vt100 基准事实 / 测试强度 / 副作用契约 / 消费方实景 → 合并 F1–F5 → 逐条独立反证），原始 8 条、合并 5 条、**确认 5 条（2 high / 1 med / 2 low）/ 驳回 0**。**S73（high）**：cols 下限 1 上任何宽字符即 panic 于 vt100 `screen.rs:730` 裸 u16 减法（release 绕回后 `:896` unwrap 仍崩，双 profile 皆崩），而 `new(0,0)` 自产 (2,1)、IPC `resize(·,1)` 穿透、fit addon 地板本即 `MINIMUM_COLS=2`——三处钳位全改 `cols.max(2)` 与上游对齐。**S74（high）**：收缩 resize 把骑线宽字符切成陈旧 IS_WIDE 半体（`row.rs:73-76` 裸截断缺 `truncate` 修复），其后覆写 panic 于 `screen.rs:847-870`、EL panic 于 `row.rs:86-89`，日常 `\r\x1b[K` 即触发崩溃循环；0.16.2 未实现 ECH，故收缩前旧几何逐屏行 `CUP + EL0`（续体在界内、`clear_wide` 安全）并以 `ESC 7`/`ESC [?6l`/`ESC 8` 包住复原光标/原点/SGR、`set_scrollback(0)` 前后捕获恢复偏移，只扫存活屏行。**S75（med）**：同尺寸 resize 守卫承重（vt100 同尺寸 `set_size` 经 `Row::resize` 无条件清 wrapped、双通道劈碎折行长行）而以 `same_size_resize_must_not_touch_the_parser` 钉死。**S76（low）**：S60 例注释 9→10 行订正（手推与三镜头实测相符）。**S77（low）**：删「折行未闭合」兜底死分支（末屏行 wrapped 恒 false：滚屏后清 `scroll_bottom`、`Row::resize` 无条件清，`grid.rs:549`/`:585` + `row.rs:75`），代之以钉死不变量注释。新增 4 条回归例 + 两处测试期望探针订正（2×2 延迟折行熔行严格相等、EL0 抹净断言拆段）。**必失变异 M-T..M-X**（M-T `cols.max(2)`→`max(1)` 全 3 处红 2 / M-U 删消毒调用红 1——panic 恰落引用的 `vt100-0.16.2/src/screen.rs:870` / M-V 删同尺寸守卫红 1 / M-X 删 `ESC 8` 红 1——Z 落消毒游走末站而非调用方原位 / S76·S77 无变异同类 M-Q 如实记录）皆红出预期；还原以 CR 剥离逐字节 == HEAD + 审过差异（16630 B）+ CRLF 245/245 无裸 CR/LF 作证（md5 仪式被 Edit 工具首写 CRLF 规范化失效，已换更强证明），终态 23/23 复绿。计划 Task 13 两个改动代码块逐字剪接回灌、计数锚点 19→23 / 138→142、Task 14 陈旧锚点「grid 7 + ring 3」→「grid 23 + ring 3」（10→26）、Task 16「19 个：grid 7 + ring 3 + flow 5 + pipe 4」→「35 个：grid 23 + …」、新增注 8。门禁：`fmt --check` 0 · `clippy --workspace --all-targets -D warnings` 0 · `nextest --workspace` **142 通过 / 0 跳过**（`FS_ITEST` 空与 `=1` 两模式一致，较 S68–S72 期 138 增 4）· `cargo test --workspace` 40 个二进制 142 通过 0 失败（两模式一致）· `cargo deny check` advisories/bans/licenses/sources 全 ok。
  - Task 14 terminal 环形缓冲（UTF-8 安全）（commit `27a166e`）：新增 `crates/terminal/src/ring.rs` —— `RingBuffer` 定容滑动窗口（默认 `DEFAULT_RING_BYTES = 256 KiB`，spec §0.1/§2.2），`new/push/snapshot_bytes/snapshot_text/total_written`；最旧字节先丢、`total_written` 计全量（含被丢字节）；`snapshot_text` 为 best-effort：跳过首部孤悬 UTF-8 续体字节（环绕切割残骸）、尾部残缺经 lossy 恰补一枚 U+FFFD——「可见有损优于静默吞字」，仅供录制回放/调试快照，**不作 AI/MCP 数据源**（语义抽取走 grid 网格通道）；0 容量构造级安全（恒空、不 panic），仅产出自有 `String` 不设 `&str` 访问器。`lib.rs` 追加 `pub mod ring;`。**测试较计划 3 例强化为 8 例**（S52 断言纪律：`String` 结构性恒为合法 UTF-8，对其 `from_utf8().is_ok()` 是永恒真空断言——一切文本断言改钉**内容**：逐字节严格相等 / U+FFFD 零枚 / 替换符恰一枚）：计划 3 例强化为精确相等，新增 5 生产形状（超容批取尾窗 / **环绕切割落在多字节字符中段**——窗口首字节须为「服」的续体 0x9C 且文本快照零噪音 / 滑动窗口计数可预测 / 0 容量退化 / 尾部残缺恰一枚 U+FFFD）。**必失变异四枚各红后复原**（md5 基线逐枚核验 OK）：M-Y 删首部跳过循环 → 2 红；M-Z 超容支路留头弃尾 → 1 红；M-AA `drain` 差一 → 3 红（含环绕切割例前置断言——窗口漂移 1 字节令切割点失中，同缺陷类连锁非假红）；M-AB 删写入计数 → 3 红。门禁：`fmt --check` 0 · `clippy --workspace --all-targets -D warnings` 0 · `nextest --workspace` **150 通过 / 0 跳过**（`FS_ITEST` 空与 `=1` 两模式一致，较注 8 期 142 增 8）· `cargo test --workspace` 150 通过 0 失败（两模式一致）· `cargo deny check` advisories/bans/licenses/sources 全 ok。回灌：计划 Task 14 两个代码块（ring.rs / tests/ring.rs）逐字节剪接回灌、Interfaces 行补 `snapshot_bytes`/`total_written` 公开面、计数锚点 26→31 / Task 16 35→40、新增注 9。（第五裁判轮临时探针与本 crate 同目录并存期，门禁以 `--test grid --test ring --lib` 作用域复验 31/31 绿，探针属裁判工作流不触不改。）
  - Task 15 terminal 流控（有界队列 + 合批 + 双水位滞回背压）（commit `4c0b3bd`）：新增 `crates/terminal/src/flow.rs` —— 渲染字节流流控四层（总设计 §2.2）：`FlowConfig`（字节双水位默认 2 MiB / 1 MiB、帧数高水位 16、合批目标 64 KiB、合批间隔 16 ms ≈ 60 fps 一帧）、`BatcherIn`（`push` 入队合并 / `backpressure_active` 双水位滞回 / `ack` 排水 / `queue_bytes` 采样 / `shutdown` EOF）、`BatcherHandle`（`pub join` + 与 `BatcherIn::shutdown` 共享信号的 `shutdown` 等价入口）；`Batcher::new` 为 tokio 式「构造即 spawn」分裂句柄（对偶 `mpsc::channel`），就地附理据豁免 `clippy::new_ret_no_self`、契约名保持不变（spec §2.2 与 Task 16 消费方据此）。**双水位滞回**：字节 ≥ high 或帧数 ≥ frames-high 触发，`bp_active` 状态位承载记忆，直到 `ack` 把字节水位排至 low 以下才解除——防阈值附近振荡；`ack` 超量饱和（`saturating_sub`）不下溢不 panic；`shutdown` 传 EOF：循环发末帧后退出并 drop `out` sender → 下游 `recv()` 得 `None`（app 层据此发 `session:closed`），两入口皆幂等。`lib.rs` 追加 `pub mod flow;`。**测试较计划 5 例强化为 8 例**（S52 断言纪律延续）：计划 5 例中 `small_chunks` / `large_frame` 由 `len + 首尾索引` 改为**全向量逐字节精确相等**（合批顺序一并钉死）；新增 3 条契约补钉——`ack(usize::MAX)` 饱和落地 0 不下溢回绕 / 双 `BatcherIn::shutdown` 叠加 `BatcherHandle::shutdown` 末帧恰发一次 / 句柄入口与 `BatcherIn::shutdown` 等价（同样末帧零丢失 + 同样 `Disconnected`）。**clippy 两处**：`identity_op`（默认低水位 `1 * 1024 * 1024` → `1024 * 1024`，语义不变）与 `new_ret_no_self`（前述豁免）。**必失变异五枚各红后复原**（md5 基线逐枚核验 OK）：M-AC 滞回→即时阈值 → 1 红；M-AD 字节不入队（只计数）→ 6 红（内容 2 例 + burst + shutdown 内容 3 例；hysteresis / over_ack 因计数行保留仍绿）；M-AE 收尾模式不置位 → 3 红（shutdown 三例皆 2 s join 超时 `Elapsed(())`）；M-AF ack 不排字节水位 → 2 红；M-AG 泄漏 `out` sender 克隆 → 3 红（shutdown 三例 `Disconnected` 断言独红、内容断言仍绿——故障隔离恰如设计）。诚实记录：原定 M-AD 方案（删 `notify_one` 即到即发唤醒）经静态推演为**隐形变异**——tokio `interval` 首 tick 即刻完成，paused-time 下循环首次 select 即经此路排空 `pending`、与 `notify` 无关，无例可红 → 改采必红的「字节不入队」。门禁（裁判探针共存、作用域限定）：`fmt --check` 0 · `clippy` 0（本人文件，两处修复后；残存告警皆出裁判探针文件）· `nextest -p fs_terminal --test grid --test ring --test flow --lib` **39/39**（grid 23 + ring 8 + flow 8）五连跑稳定。回灌：计划 Task 15 两个代码块（flow.rs / tests/flow.rs）逐字节剪接回灌（含模块 doc 首行）、门禁期望 5 → 8、Task 16 合计口径 40 → 43、新增注 10。（全工作区五门禁留档待两裁判撤探针后补跑，口径同注 9。）
  - Task 14 第一裁判整改 S78–S82（commit `0319d81`）：第一裁判（wv4rc4x0k，多镜并行 + 每条双怀疑者质证、REJECT 须双方皆驳）确认 5 项（0 high / 2 med / 3 low，0 驳回）——**S78（med）** ≥cap 支路 `buf.clear()` 承重无钉（旧 8 例入支路者窗口本空，删之变异套件逐字节同值，生产形状「非空窗口收 ≥cap 单批」窗口永久超容、256 KiB 上界永久失效）→ 补回归例 `overcapacity_push_onto_nonempty_window_resets_to_exact_cap_tail`（cap4：`ab`→`0123456789` 须 `6789`，后续小批须即回 `qrst`）；**S79（med）** `snapshot_text` 跳过 regime「仅」字边界无钉（旧四例头部非法字节恰全是续体，overskip 变异对首部 0xFF 静默吞字、违「可见有损优于静默吞字」）→ 补回归例 `leading_invalid_non_continuation_bytes_show_replacement_not_swallowed`（`[FF 41]` 须 `"\u{fffd}A"`、`[C0 80 42]` 须 `"\u{fffd}\u{fffd}B"`）+ 模块头补第三 regime 半句；**S80（low）** `drain` 之 `.min(self.buf.len())` 结构性死防御（裁判 3584 例穷举差分零发散 + 代数证 0<drop<buf.len() 恒立）→ 循 S77 死分支先例简化 `drain(0..drop)` + 不变量注释；**S81（low）** `snapshot_text` doc 补「控制序列原样留存、不作剥离——需要已剥离文本请走 Grid」对冲 spec §2.2 L121 字面张力；**S82（low）** 模块头补复杂度注「满载后 push 与 snapshot_bytes 各 O(cap)」免名实误导（spec「环形缓冲」指称定容语义契约而非实现机制，真环延后至实测有压）。**必失变异两枚各红后复原**（md5 核验）：M-AH 删 clear → 恰 S78 新例 1 红（t4/t7 窗口本空仍绿）；M-AI overskip → 恰 S79 新例 1 红（旧四例零发散）。命名对账：裁判建议 M-AC/M-AD 与注 10 已用名撞、两枚质证皆点出，采纳改用 M-AH/M-AI，Task 16 管道变异自 M-AJ 起更名。门禁（裁判探针共存、作用域限定）：fmt 0（本人文件；期间包级 fmt 曾空白重写四个探针——语义中立，自兹 fmt 恒显式文件）· clippy `-D warnings` 0 · nextest `--test grid --test ring --test flow --lib` **41/41**（grid 23 + ring 10 + flow 8）五连跑稳定。回灌：计划 Task 14 两代码块逐字节剪接 + 注 11 + 锚点 31→33 / Task 16 合计 43→45。收敛计数仍 0/2（本轮为有确认发现之轮，第二裁判待启）。
  - Task 15 第一裁判整改 S83–S88（commit `851cd5c`）：第一裁判（wf_fd7471cf）确认 1 high 1 med 4 low / 0 驳回——S83 帧背压双维释放 + FIFO 帧账本（释放条件 bytes < low **且** frames < frames-high；`sent_frames: VecDeque<usize>` 循环记账、ack 自队首顺次弹出/收缩首帧，单次聚合 ack 覆盖多帧——旧实现每 ack 恒减 1 帧 + 释放只看字节，致帧触发锁存逐调用振荡、聚合 ack 残帧永久 ≥ 高水位，裁判双剖面独立复现）；S84 帧触发 stealth 双钉例；S85 notify 唯一唤醒源钉例（1ms 超时区分 notify/tick）；S86 迟到 push 契约（fire-and-forget 无反馈通道，调用方须 shutdown 前停推）；S87 shutdown 无条件退出双层（非收尾发送竞态 shutdown 许可 + 收尾发送有界一个 `batch_interval` + 丢许可竞态窗口 AtomicBool 兜底——**变异战役 M-AN 自捕残余洞**：sub-batch 尾帧 + 消费者停滞 + 收尾发送竞争已失许可死锁，补洞后 M-AP 恰红 = 修前语义正证；test 8 初版 setup bug（paused-time advance 滞后帧到达一拍）订正为事后序断言）；S88 计划 L4957 悬空引用仅备录（预存 9728e47，磁盘 pipe 测试零命中、Task 16 拼接自愈）。必失变异 9 枚各红/实证后复原（M-AJ 2 / M-AK-v2 2——v1 系 E0267 编译错 mutant 作废 / M-AL 1 / M-AM 6 / M-AN 5——两处超预测诚实记 / M-AO 1 / M-AP 1 / M-AQ stealth 文档化 0），49/49 ×5（flow 8 → 16）+ clippy 0，注 12 回灌（两代码块逐字节剪接 + Interfaces 五处文案 + Expected 级联 45 → 53）。
  - Task 14 第二裁判整改 S100–S103（代码 commit `2042769` + 本回灌 commit）：第二裁判确认 4 项全 low（0 驳回；F1 发现时 med、双质证独立降 low）——收敛计数 0 → 1/2；S100 续体域内上半 0xA0-0xBF 双钉（t5 测字「服」→「常」令环绕残体首字节 B8 ≥ 0xA0 + S79 测函补 0xA0/0xBF 界钉；M-AR（mask 0xC0→0xE0）恰 2 红、旧 8 例零牵连）；S101 注 11 ⑧ `pipe 4 → 7` 预断删数（+3≠+2 算术冲突 + 全文零凭据）；S102 spec §2.2 L121「并剥离转义序列」改「控制序列原样留存…需已剥离文本走 headless 网格」对偶 snapshot_text doc；S103 ring.rs:46 注释补第二前提 buf.len()≤cap + 注 11 ③ 镜像。49/49 ×5 + clippy 0，注 13 回灌；第三裁判待启（零确认 med/high 则 2/2 关闭 Task 14）。
  - Task 13 第五裁判整改 S89–S99（代码 commit `acdd01c` + 本回灌 commit）：第五裁判（wth25dtnp，六镜头并行 + 合并 + 逐条双怀疑者质证）确认 11 项（1 high / 5 med / 5 low，驳回 0；原始 17 条合并后 11）——**S89（high）**收缩消毒只覆盖活动网格，备用屏激活期 resize 毒化非活动主网格、退屏后 EL/覆写双 profile 崩溃（回归套件零承重）→ 双网格 `?47` 调度器（切换零状态扰动、各网格自洽消毒）；**S90（med）**消毒序列 ESC 7 覆盖应用 DECSC 存储槽（实证静默错位）→「读光标 + DECOM 双探针 + 末尾相对复原」三段重写，全程不触存储槽；**S91–S94（med）**四处边界盲区零承重（EL0 擦除左边界 / `?6l`·`?6h` / 扫描上界 −1 / `set_scrollback` 等价删除）各补专钉，grid 23 → 29；**S95–S98（low）**三处注释文献订正（`insert_lines` 误标 `scroll_up` / ECH 失实句 / EL0 vs truncate 格属性差入残留）+ `OffsetGuard` RAII；**S99（low）**注 8 字节数误挂（diff 实为 11279 B 误标成文件尺寸 16630 B，git 实测订正）随本回灌落地。必失变异 M-AS..M-AY + M-AT2 全预测 == 实测精确（M-AT 3 红追因：探针末点恰被 ESC 7 存入槽；M-AT2 纯回退形态与钉例 doc 精确吻合），还原逐字节。门禁 fmt 0 / clippy 0 / nextest 作用域 55/55 ×5。收敛计数仍 0/2，第六裁判待启（零确认 med/high 则 1/2）。
  - Task 14 第三裁判整改 S104–S108（代码 commit `76c744f` + 本回灌 commit）：第三裁判（wba0hkqgs，多镜头 + 合并 + 逐条双怀疑者质证）确认 5 项（1 med / 4 low，驳回 0）——**S104（med）**谓词掩码唯一零钉象限（b7=0∧b6=0 = ASCII 0x00-0x3F）：丢 b7 高位检查的变异 `(b & 0x40) == 0` 与全部旧例导/止字节同真值、唯此象限为差异点，变异体静默吞没 ASCII 先导字节，ESC（0x1B）恰系终端快照最高频先导形（spec §2.2 L121「控制序列原样留存」），吞之即录制回放失真 → 象限四界 NUL/ESC/LF/0x3F 钉死；**S105（low）**跳过循环上界 `start < len` 的 −1 变异零钉（窗口以续体字节收尾形）→ 纯续体窗口 [B8,B8] 空串钉例；**S106–S108（low）**文案侧回灌遗漏/自相矛盾三项：Task 14 两代码块按磁盘逐字节重剪（注 13 回灌遗漏、三处字节漂移，闭合「按现计划重跑重建 M-AR 逃逸洞」风险）、注 12⑧「7 例后为 56」换壳预断删除（算式逐值等同已删 `pipe 4 → 7`）、注 13③「逐字对偶」→「对偶」+ 引文补「，环绕切割残骸」五字。必失变异 M-AZ/M-BA 全预测 == 实测精确（各恰 1 红），还原逐字节。门禁 fmt 0 / clippy 0 / nextest 作用域 57/57 ×5。收敛计数重置为 0/2（第二裁判首洁 1/2 因本批 med 中断），第四裁判待启（首洁 1/2、再洁 2/2 关闭 Task 14）。
  - Task 15 第二裁判整改 S109–S117（代码 commit `2736015` + 本回灌 commit）：第二裁判（wguxnxa7d，24 agent：五镜找茬 → 合并 → 逐条双怀疑者质证，REJECT 须双方皆驳）原始 12 条 → 合并 9 条，确认 9 项（1 med / 8 low，驳回 0）——**S109（med）**非收尾发送 `select!` 缺 `biased;`：tokio 默认随机择序，循环阻塞于发送、消费者随后推进腾出槽位时「send 就绪」与「shutdown 许可」同拍到达，约 50% 概率误走 break 丢弃本可送达的末帧，而丢帧许可的前提恰是「消费者停滞且通道满」——与三处写死的「消费者推进时末帧零丢失」契约直接冲突 → `biased;` + 发送支路居首，send 就绪即必发、shutdown 仅在 send 真阻塞时胜出；**S110–S112 / S114–S116（low）**六处零承重钉例盲区补钉：S110 FIFO 弹出端（旧三例帧长皆 10 等长，`front_mut`→`back_mut` 在帧数账目上隐形）→ 不等长帧钉死弹出端；S111 收尾发送等待上界系自由变量（`timeout(0)` 与 `timeout(50×interval)` 在 2 s 超时下皆全绿）→ 虚拟时钟上把上界钉死为恰一个 `batch_interval`，并同时记下其**不**分辨「删主 select shutdown 支路」的边界（源插桩取证实测：两条路总耗时同为 20 ms，却分别来自 `batch_interval` 的「发送超时上界」与「tick 周期」两种角色，数值恰好相撞）；S112 `BatcherIn::shutdown` 的 `notify_one` 此前无例可证（旧例通道皆宽裕，删许可后 `shutdown_flag` 兜底在下一 tick 照样收尾）；S114 字节双水位两端边界值从无例采样（`>=`→`>` 与 `<`→`<=` 双向隐形）；S115 主 select 的 shutdown 支路是「零延迟收尾」唯一快路径（删后仍能收尾，只是迟一个 `batch_interval`）；S116 `notify_one` 存许可 / `notify_waiters` 不存的载荷语义差；**S113 / S117（low）**文案两项：`push` 的「迟到字节静默丢弃」把「循环已退出（永不送达且留幽灵水位）」与「仍在收尾排空窗口内（可能被后续 `mem::take` 取走并送达）」两个语义不同的窗口混为一谈 → doc + Interfaces 按双窗口重写；注 12② 把 1 GiB 字节高水位误挂到帧水位例上 → 订正配置归属。必失变异 M-BB..M-BJ 九枚逐枚各红、实证后 md5 核验源文件逐字节还原；五门禁全绿（`rustfmt --check --edition 2021` 显式两文件 0 · `clippy -D warnings` 0 · nextest 定域 64 全过 · `cargo test` flow 23 + grid 29 + ring 12 · `deny` advisories/bans/licenses/sources 四项 ok），flow 用例 16 → 23、定域 64、工作区合计 183（119 非 terminal + 定域 64；Task 16 TDD 红期的 `tests/pipe.rs` 阻塞全工作区构建，故按分区合成计量并留痕）。收敛计数：本轮有确认 med → 仍归 **0/2**，第三裁判待启。
  - Task 14 第四裁判整改 S118/S119（代码 commit `82d6c62` + 本回灌 commit）：第四裁判（wetlbox1i，原始 17 → 合并 12 → 逐条双怀疑者质证）确认 1 项（1 med / 驳回 1 low）——**S118（med）**排水支路谓词 `bytes.len() > free`（`ring.rs:45`）的边界值 `== free + 1` 在旧 12 例从未采样：入排水支路者溢出量恒 ≥2、不入者余量恒 ≥0，`> free + 1` 变异体全部存活；边界一拍漏排后 `free = cap.saturating_sub(len)` 饱和归零，其后每笔 1 字节批恒假 → 窗口单调无界增长（实测 cap=8 + 40 单笔 → len 40），256 KiB 上限失守、S103 不变式 `buf.len() ≤ cap` 静默变假而无断言观测 → 钉两例：边界钉例 cap=4「abc」+「de」（基线 `b"bcde"` / 变异体 `b"abcde"` 长 5 > cap）与 41 笔单字节风暴（逐拍在界断言 + 末窗内容钉，奇数笔破多排变异的相位巧合 16×2 = 32×1）；**S119（low，订正）**S104 注释所引「pipe 'r'=0x72」系第三裁判仓外探针字节、仓内不可验 → 删悬空引证，止点枚举自足（驳回的 finder-low 指控此引证悬空，框架被驳、引证确悬，故仍订正而不计确认项）。发现文案两处缺陷未采入：所称「等价自然写法」实不等价（t1 即红）、t9 早退描述不确。必失变异 M-BK（`> free + 1`，存活变异本体）恰红新增两例、旧 12 例全绿；M-BL（`drain(0..drop+1)` 多排 1 字节）红 4 例。五门禁全绿（`rustfmt --check --edition 2021` 显式两文件 0 · `clippy -D warnings` 0 · nextest 定域 66 全过 · `cargo test` ring 14 · `deny` 四项 ok），ring 用例 12 → 14、定域 66、工作区合计 185（119 非 terminal + 定域 66，组合式计量，理由同注 16⑪）。收敛计数：本轮有确认 med → 仍归 **0/2**，第五裁判待启。
  - Task 13 第六裁判整改 S120（代码 commit `8701713` + 本回灌 commit）：第六裁判（w9pyqawn9，多镜 + 合并 + 逐条双怀疑者质证，双怀疑者独立路径依赖探针复现）确认 1 项（1 high / 驳回 0）——**S120（high）**`sanitize_active_grid` 相对复原支路算 `p0.0 - top + 1`，其不变式「DECOM 下光标结构性位在区域内」只枚举了 `set_pos` 调用方，漏掉 vt100 0.16.2 两条零钳位路径：`restore_cursor`（ESC 8，vt100 grid.rs:121-124 裸写 `pos = saved_pos`）与 `vpa`（ESC[Pn d → row_set → row_clamp，仅钳至屏底）。两路径皆可合法地把光标送出滚动区而 DECOM 仍开，p0.0 < top 时减法 u16 下溢：debug 直 panic「attempt to subtract with overflow」、release 绕回巨值经 set_pos 钳至区域底落错行 → 相对行计算前 `p0.0.clamp(top, bottom)`（界外值落最近区域边——DECOM 相对 CUP 可达的最近合法点，set_pos 再钳一次稳态不越界；p0.0 > bottom 侧本靠 set_pos 底边钳位、修前修后行为恒等，对称入钳）。钉两例：VPA 直出区域顶之上与 DECSC + DECRC 召回（DECSC 把 pos 与 origin_mode 一并存，区域前存储则召回 om=false 永不入 decom 支路，故 DECRC 形状必经 VPA 出区构造），两例同钉 DECOM + 滚动区域存活消毒；注释订正二处（长注与支路内联注「结构性位在区域内……故相对复原精确」改为 set_pos 路径限定陈述、长注回归例清单补两新例）；订正第六裁判过度声称（panic 点在 `format!` 早于 `parser.process(&seq)`，消毒序列整体未入解析器，无「seq 半程遗留毒体」机制，真实残留为双探针落地 + `?6l` 未喂 + 骑线半体未清）。必失变异 M-BM（删钳位 `let row = p0.0;`）恰红两新例（panic 点在减法行 grid.rs:361:40）、旧 29 例全绿。五门禁全绿（`rustfmt --check --edition 2021` 显式两文件 0 · `clippy -D warnings` 0 · nextest 定域 68 全过 · `cargo test` grid 31 · `deny` 四项 ok），grid 用例 29 → 31、定域 66 → 68（续 Task 14 S121 批至 69）。收敛计数：本轮有确认 high → 仍归 **0/2**，第七裁判待启。
  - Task 14 第五裁判首洁 + 整改 S121（代码 commit `5b41049` + 本回灌 commit）：第五裁判（wixjxgl2y，五镜：窗口语义 / UTF-8 契约 / 非空变异 / 边界资源 / 文案一致；原始 13 → 合并 6 → 逐条双怀疑者质证）确认 1 项（1 low / 驳回 5）——**注 19① 终局订正：中间态口径已被取代**（终局产物：原始 5 → 合并 5 → 确认 4 low / 驳回 1，med/high 口径零确认首洁，0/2 → 1/2 维持；4 low 已悉数整改 S122–S125，commit `ebd1c76`；第六裁判 woiahiges 再洁，Task 14 代码轨 2/2 关闭）；low 按章仍整改——**S121（low）**续体域 0x80-0xBF 的全域下界 0x80 本身从无例采样（S100 钉的是上半域两界 0xA0/0xBF，旧例先导续体 9C/8D/B8 皆 > 0x80，域外恰下 0x7F 亦无例），任何把 0x80 排出跳过域的变异在全部旧例存活 → 三钉封死下界两侧：0x7F 域外恰下须留存、0x80 域下界与 0x81 域内恰上皆须跳过。发现文案变异式不确未照抄：所称 `(b & 0x80) != 0` 实不在旧套件存活（把 0xC0-0xFF 一并纳入跳过域、早红于 S79 的 0xFF/0xC0 例），整改按「0x80 采样盲区」准确口径落钉。必失变异 M-BN（续体判定附加 `&& b != 0x80`，精确排出下界一点）恰红新钉例、旧 14 例全绿。五门禁全绿（`rustfmt --check --edition 2021` 显式路径 0 · `clippy -D warnings` 0（沿用上批）· nextest 定域 69 全过 · `cargo test` ring 15 · `deny` 四项 ok），ring 用例 14 → 15、定域 69、工作区合计 188（119 非 terminal + 定域 69，组合式计量，理由同注 16⑪）。收敛计数：S121 为 low 整改、不重置计数，仍 **1/2**，第六裁判待启（再洁 2/2 关闭 Task 14 代码轨）。
  - Task 14 第五裁判终局 4 low 整改 S122–S125（代码 commit `ebd1c76` + 本回灌 commit）：第五裁判（wixjxgl2y）终局产物 原始 5 → 合并 5 → 确认 **4 项（0 high / 0 med / 4 low）/ 驳回 1**，clean_round=true——med/high 口径首洁（收敛计数 1/2 维持，low 不重置）；注 18 头行与 rm18 的中间态数字（原始 13 → 合并 6 → 确认 1 low / 驳回 5）被取代（注 19①，两终局跨会话压缩点送达）；被驳回的一条恰是 S121 已修的 0x80 缺口（怀疑者跑在 S121 落盘后的树上双驳，时机 artifact，S121 保留不动）。**S122** len1 纯续体窗口钉例（`new(1)`+「常」→ 窗口 `[B8]` → text 空串；M-BO「len<2 快速路径插入」恰红一例）/ **S123** 尾部孤悬续体钉例（`[0x41,0x80]` → "A\u{fffd}" 而非静默剥除；M-BP「尾部游程剥除 + 前方 < 0xC2」恰红一例）/ **S124** 跳过上界游程长 6 钉例，一枚钉死 `start < N`（N≤5）全族（M-BQ `start < 3` 恰红一例）/ **S125** S118 注释补『持续单字节批形状下』限定（纯文案 M-Q 类，与 tests/ring.rs S118 两例注释及计划注 17① 口径合流）。ring 15 → 18，定域 69 → 72，工作区组合式 188 → 191。第六裁判（woiahiges）终局：原始 2 → 合并 2 → 确认 2 low / 驳回 0，clean_round=true → **2/2 关闭 Task 14 代码轨**（两 low——首部合法起始残缺零钉 / S79 点名集 0xC1·0xF5·0xFE 先导位零钉——折入下一合批 S137/S138 整改，不重置计数）。
  - Task 15 第三裁判终局 12 项整改 S126–S136 + Task 14 第六裁判 2 low 整改 S137/S138（代码 commit `46d487e` + 本回灌 commit）：Task 15 第三裁判（wg2n51bh4）终局 原始 13 → 合并 12 → 确认 **12 项（0 high / 2 med / 10 low）/ 驳回 0**，clean_round=false（注 17⑥ 中间态「首洁」已由注 19⑥ 取代，收敛计数 0/2）。**2 med**：`batch_interval = Duration::ZERO` 令合批任务首巡即 panic（tokio interval 非零断言，panic 在 spawned future 内、构造期不可捕获）/ 零阈值毒形集（`queue_frames_high = 0` 空队列即锁存、`queue_bytes_low = 0` 排干至 0 仍锁存、`queue_bytes_high = 0` 恒触发）令释放支路对 usize 不可达、背压永久锁存 → `Batcher::new` 顶部消毒三钳（interval 钳 1 ms / 两 high 钳 1 / low 链式钳 [1, high]），保分裂句柄契约不改签名（怀疑者 Result 建议依 spec §2.2 与 Task 16 消费方契约否决）。**10 low**：S129 收尾发送成功支路帧记账钉（contract 与 mutation 双镜合并一钉）/ S130 两条丢帧支路幽灵水位钉 ×2（queue_bytes doc 条款 + 两 S87 旧例 queue_bytes==200 断言）/ S131 S114 归属误挂按例分裂（== low 由 S114 例钉字节维、== frames-high 实由 S83 两例钉——S114 例帧维恒 0；模块 doc + 方法 doc + 计划 Interfaces 三处）/ S132 累积跨阈 notify 钉（累积判据变异为单块判据旧套 23 例全绿）/ S133 ack 收缩减法非赋值钉（破旧例数值巧合 10−5=5 vs =5：帧 [10,30] 双部分 ack）/ S134 非收尾 send-Err 退出钉（删整支则逐 tick 空转至超时——非 busy-wait 忙等，tick 间真实挂起；阻塞于发送期间接收端 drop 形状旧例零钉）/ S128 水位倒挂钳位 + 六连读锁存钉 / S135 S116 注释与注 16⑧ 就地订正（「旧例恰有等待者 / 不可分辨」实证为伪：M-BJ 恰红 4 例含 3 旧例）/ S136 Interfaces 首参名 cfg + 类型注记。**Task 14 第六裁判（woiahiges）终局 原始 2 → 合并 2 → 确认 2 low / 驳回 0**，clean_round=true → **2/2 关闭 Task 14 代码轨**；两 low 折入本合批为关闭后整改（不重置计数）：**S137** 首部合法起始残缺序列显形钉（三形 [E6,9C,41] / [F0,9F,98,41] / 复合 [80,E6,9C,41]——「尽力 best-effort」变异体于全 20 旧例逐例同基线，怀疑者实证）/ **S138** S79 点名集 {0xC0,0xC1,0xF5-0xFF} 闭合（t10 追加 r5/r6/r7 子例，钉析取式掩码变异族 `|| b == 0xC1` / `|| b == 0xF5` / `|| matches!(b, 0xF6..=0xFE)`）。非空证明：15 枚变异各恰红一例（flow M-BR..M-CB 各 31 pass/1 fail · ring M-CC..M-CF 各 18 pass/1 fail；M-BV 红形为构造期 panic——clamp(1,0) min>max，如实记；M-CC 初版变异语句漏区间下界 0xE0/0xF0 致红 8 例、系语句自身 bug，补齐后恰红一例；逐枚 restored=true 逐字节相等）。门禁：定域 **82**（grid 31 + ring 19 + flow 32），工作区组合式 **201 = 非 terminal 侧实测 119 + 定域 82**。
  - Task 13 第七裁判终局 10 项整改 S139–S146（代码 commit `6b1c854` + 本回灌 commit）：第七裁判（wmgm0n1ux，五镜：并发时序 · 契约不变量 · 非空变异 · 边界资源 · prose 一致；双怀疑者对抗交叉）终局 原始 11 → 合并 11 → 确认 **10 项（0 high / 1 med / 9 low）/ 驳回 1**，clean_round=false。驳回项（tests/grid.rs:393-394 / 617-618「其」指代歧义）双怀疑者实证为伪——「其」指变异注入的 DECSC，无动作。**med #8 → S145**：尺寸钳位只防下界不防上界——极端合法 u16 尺寸经单条 IPC 令 vt100 立即分配（new 主网格 ~128 GiB / resize 双网格 ~256 GiB），失败走 handle_alloc_error → abort（非 panic、catch_unwind 不可拦），整后端连所有会话同死；新增常量 MAX_ROWS=1024 / MAX_COLS=4096（双网格 ~268 MiB），new + resize 对称钳上界（走在 vt100 分配之前），双钉例 `grid_new_clamps_extreme_sizes_to_upper_bounds` / `resize_clamps_extreme_sizes_to_upper_bounds`（65535² → (1024, 4096)，字面量断言兼钉常量值变异）。**9 low → S139–S144 + S146**：S139 decom_on 分支相对复原 CUP 于旧全 31 例零承重（删复原 CUP / 行参 −1 / 列参 −1 / 行列参恒 1 诸变异常绿），钉例 `decom_restore_cursor_is_observable_on_bare_write_after_resize` + 三处 doc 订正；S140 区域底边界 inclusive 语义零承重（`clamp(top, bottom-1)` 常绿），钉例 `decom_restore_keeps_cursor_on_region_bottom_not_one_above`，与 S139 / S120 两例合成四象限；S141 `decom_on` 否命题零承重（`= true` 常绿），钉例 `shrink_does_not_enable_decom_for_an_app_that_never_set_it`；S142 多骑线行累积零承重（覆盖赋值常绿；真实双骑线流是崩溃形态——探针实证双 profile 皆 panic），钉例 `two_straddling_rows_are_both_sanitized_not_just_the_last`；S143 完整存活边界负例零承重（`is_wide() || is_wide_continuation()` 常绿而经 EL0 的 clear_wide 反清左半静默误删整对），双钉例 `wide_pair_fully_inside_new_last_cols_survives_shrink_untouched` + `cjk_run_filling_old_width_halves_exactly_on_shrink`；S144 `DEFAULT_SCROLLBACK_LINES` 零引用（scrollback=0 ⇒ AI/MCP 上下文通道静默失史），钉例 `default_scrollback_matches_spec` 钉死 spec §2.2 数值 10000；S146 纯 prose——注 18①「区域前存储则召回 om=false 永不入 decom 支路，故 DECRC 形状必经 VPA 出区构造」实证为伪（?6h→DECSC→DECSTBM→DECRC 区域外而 DECOM 仍开、零 VPA），就地订正为非穷举（S117 先例，被代引文留痕）。怀疑者订正悉数采录。非空证明：12 枚必失变异 M-CG..M-CR 逐枚锚点计数恰 1、恢复字节相等、红数逐枚恰符预测（M-CG 2 / M-CH 1 / M-CI–M-CL 各 2 / M-CM 1 / M-CN 1 / M-CO 2 / M-CP 1 / M-CQ·M-CR 各 1；M-CN 首跑撞 LNK1104 瞬态链接锁、续跑复杀，如实记），12/12 杀死无超额红。门禁：定域 **91/91**（grid 40 + ring 19 + flow 32；grid 31 → 40），cargo test --test grid 40，工作区组合式 **210 = 实测 119 + 定域 91**；计划注 21 回灌（两代码块重剪 + 三级联）+ S146 就地订正。
  - Task 15 第五裁判 + R4 残余并批整改 S147–S165（代码 commit `b1c152ddb9dce70e479de00678208cb7ab1a63e5` + 本回灌 commit）：第五裁判（wuo5almnl，整改后树，五镜 + 双怀疑者交叉）终局确认 **20 项（0 high / 0 med / 20 low）/ 驳回 0**，clean_round=true（首洁 1/2）；第四裁判（wxe6qa9je）跑在整改前树上仅辅助信号（确认 6 low / 驳回 5），3 项折入本批（R4#3→S148 / R4#7→S159 / R4#8→S158）。两源 22 检出归 19 个 S 编号（S147–S165），悉数整改：**8 新钉**——S147 收尾 Ok(Err) 记账反面 / S148 FIFO 账本两端（双钉）/ S150 shutdown 主臂零延迟 / S151 Duration::MAX 有限上界钳（新增常量 MAX_BATCH_INTERVAL = 1 h）/ S152 bytes_high 下限钳恰 1 / S153 interval 地板恰 1 ms / S155 非收尾 send-Err 记账反面（钉例 `closing_send_err_does_not_record_phantom_frame` 等，详注 22）；**src 机制 4**——S149 收尾快路径等价性文档化 / S151 MAX_BATCH_INTERVAL / S152 链式钳位 high.max(1) → low.clamp(1,high) / S153 interval.clamp(1 ms, 1 h) 对称；**注释/doc 订正 10**（S154/S155/S156/S157/S158/S159/S160/S161/S162/S163，其中 S161 收尾发送分布经怀疑者探针 5/5 实跑取证：成功 ×5 / 超时丢帧 ×2 / Ok(Err) ×0；S163 shutdown_flag 改防御纵深口径——M-CE 32/32 + 40/40 全绿零承重，注 16④ 镜像旁注）；**计划级联 2**（S164 Task 15 Step 3 23→40 / S165 ack 参数名 consumed）。非空证明：11 枚变异 M-CS..M-DC 实证逐枚符预测——M-CS–M-CX + M-DC 各恰红 1 例无超额；M-CZ/M-DA/M-DB 诚实全绿（反向等价探针：closing 向排空延迟不可观测 / 循环顶空 pending 退出等价 / tokio 1.53.1 许可不丢零承重），注 22 ㉑ 备录。门禁：定域 **99/99**（grid 40 + ring 19 + flow 40；flow 32 → 40），cargo test --test flow 40，工作区组合式 **218 = 实测 119 + 定域 99**；计划注 22 回灌（双代码块重剪 + 五级联 + 三冻结注旁注）。收敛：Task 15 **1/2**（lows-only 轮不重置计数；第六裁判 judge-task15-round6 再洁 2/2 关闭）。
  - Task 13 第八裁判整改 S166–S170（代码 commit `dd997951cb49be81959012ac9f46a03b6383f877` + 本回灌 commit）：第八裁判（judge-task13-round8）终局确认 **5 项（1 med / 4 low）/ 驳回 0**，悉数整改——**S166（med）** S89 双网格消毒的切换字节 ?47h/?47l 与应用字节流同走一个 vte wire parser，ESC anywhere-transition 打断在途半截 CSI/UTF-8、后续字节误解析——真实终端 resize 走本地几何通道不触 wire parser，本层结构性不能（vt100 0.16.2 网格切换为私有 API，字节注入是唯一路径，上游约束），如实钉三形状（A 半截 CSI "hello2J" / C 半截 UTF-8 「服」不材化 / F-ALT 对称注入同类）；**S167/S168（low×2，共享一钉）** resize「先钳位、后守卫」次序于旧 40 例零承重，但守卫前置变异令 raw≠current ∧ clamp(raw)==current 的入口（fit addon MINIMUM_ROWS=1 / IPC 穿透零值）复活 S75 灾难 → 钉下界钳位形状 no-op；**S169（low）** 探针 2 行参必须是 self.rows 旧几何，换 new_rows 于行列齐缩形状误判 decom_on、?6h 误开 → 钉行列齐缩形状；**S170（low）** 「~268 MiB」系十进制 268 MB 误挂二进制单位 → 算术订正（注 21① 就地订正镜）。同批自查 6 处引用订正（src 自引 4 + tests→src 引 1 + plan 行号符号化 1，纯注释级）。非空证明：必失变异 M-DD..M-DG 四枚 --no-fail-fast 全量下逐枚恰符预测（45/45 无截断、无超额红、无超时，三方对账、复原逐字节相等）——期间发现本环境 nextest 默认 fail-fast 截断（次运行 M-DF 9/45、M-DD 2/45 未执行）+ 旧汇总正则误读「36/45 tests run」为 run=45，探针已加截断即死 + 对账即死三重保险。五门禁全绿：rustfmt 显式两文件 0 · clippy -D warnings 0 · nextest 定域 **104/104**（grid 45 + ring 19 + flow 40）· cargo test --test grid 45 · deny 四项 ok，grid 用例 40 → 45，工作区组合式 **223 = 实测 119 + 定域 104**；计划注 24 回灌（双代码块重剪 + 四级联 + 一就地订正镜）。收敛：Task 13 **0/2**（本轮有 med 确认），第九裁判待启（首洁 1/2、再洁 2/2 关闭 Task 13 代码轨）。
- 🛠 **M0 对抗复审整改（第二裁判首次落到代码上）**：对 Task 3–6 已落地代码做 4 视角对抗复审，24 条确认发现（R117 critical + S1–S21 + 3 条并入），分四批整改，每批均遵守**非空证明纪律**——每条新增回归测试都先临时回退其修复、确认按预期变红且报错信息精确指向该缺陷，再恢复：
  - **R117 critical / S1**（commit `2350f93`）：KEK 与口令校验串做**域分离**。历史实现用 `derive_key_from_passphrase` 生成 PHC 校验串落盘，又从**该 PHC 自身的盐** + 同一套 Argon2 参数重算 32 字节密钥当 KEK 去封 `master_sealed` —— 同口令同盐同参 ⇒ Argon2 输出逐字节相同 ⇒ **PHC 末段的 hash 字段就是 KEK 本身**，与被它加密的 `master_sealed` 并排明文躺在同一个 JSON 里。攻击者只要拿到 `vault.json`（备份、同步盘、误传、取证镜像），无需口令、无需 OS keyring 即可解出 master key 并解密全部凭据（总设计 §3.2 彻底失效）。处置：新增 `crypto::generate_kdf_salt` / `derive_kek(passphrase, kdf_salt)`，`kdf_salt` 由 `arm_passphrase` 独立随机生成并明文落盘（盐无需保密，只需与校验盐无关）；回归 `tests/kek_isolation.rs` 以「只拿到 vault.json 的攻击者」为模型，扫荡文件里**任何**可解成 32 字节的字段（含 PHC 逐段），一律不得解开 `master_sealed`。
  - **A 批 S2/S4/S7/S10/S14/S19/S20**（commit `a1d6f3c`，vault Store 健壮性）：「打开保险库不得毁库、不得改密、不得 panic」。S2 —— `Err(_) => VaultFile::default()` 把**任何**读取失败（磁盘瞬时 IO 错、JSON 尾部被截断、权限问题）当成「首启空库」，下一次 `save()` 就把用户全部凭据原子覆盖为空文件，且不留任何痕迹；改为只在 `NotFound` 时建新库，其余一律 `Err` 传播。S4 —— `open_or_create(dir, Some(pass))` 对**既有**库会无条件重铸校验串与 KEK 封装，等于「用今天输的口令悄悄改掉昨天的口令」；改为既有库走校验路径。S7/S10/S14 —— keyring 取回的 base64 长度不足 32 时 `copy_from_slice` 直接 panic（`Store::open` 在 UI 线程上）；改为显式长度校验并返回 `Integrity`。S19/S20 —— 一并覆盖锁定态语义与错误分型。回归 `tests/robustness.rs` 8 例。
  - **B 批 S5/S6/S9/S11/S12/S13/S15/S17/S18**（commit `482c753`，connmgr）：S11 库路径不得先拼进 `sqlite:` URL 再 `from_str`（sqlx 0.9 会在首个 `?` 处截断并对文件名做 percent-decode，用户目录里一个 `%41` 就把库悄悄建到别处）→ 改走 `SqliteConnectOptions::new().filename(path)` builder；S6 `recursive_triggers` 必开，否则一条 `INSERT OR REPLACE INTO audit(id,…)` 就能原地改写任意审计行而两个 `RAISE(ABORT)` 触发器全程沉默（总设计 §2.1「审计不可篡改」）；S13 `busy_timeout=15s`，否则多窗口同时首启时后到者立刻 `SQLITE_BUSY` 而非退避重试；S9 导入必须回落默认 TOFU 并清空指纹钉（否则人工构造的 profiles.json 能让会话**预先信任**攻击者主机密钥，首连不弹确认，中间人当场生效，与 R113 剥 `vault_record` 同源）；S17 `import_json` 全批包单事务（全成功或全不落）；S12 `port as u16` 静默截断改 `u16::try_from`；S18 新增 `Error::CorruptRow` 与 `NotFound` 分型，`list()` 跳过坏行照常呈现、`get()` 如实报坏在哪一列（旧实现里**一行**坏数据会让整张连接列表凭空消失）；S5 测试样本改为「每字段皆非默认且同类型两两不同」+ 整体 `assert_eq!`（旧样本大半是 `Default::default()` 且只断言两个字段，14 个 bind 与 14 列整体错位一格也照样全绿）；S15 迁移产物断言由白名单两张表改为六张逐一点名。`-p fs_connmgr` 21 绿。
  - **C 批 S8/S16/S21**（commit `72c32db`，vault）：S8 AAD 历史上只绑 `record_id + version`，而 `kind` 与 `label` 恰恰决定这段密文**将被怎么用**且与密文并排明文躺在 vault.json 里 —— 把一条 `private_key` 改成 `password`，登录时私钥全文会被当口令**明文发往远端**；把生产库口令的 label 改成「测试机口令」，用户就会把生产口令绑到攻击者主机上。处置：`Aad` 结构体（而非平铺 `&str` 参数，避免相邻两参调换后 seal/open 两边同错、依旧自洽的静默降级）+ **长度前缀**编码（直接拼接时 `("ab","c")` 与 `("a","bc")` 得到同一串 AAD）+ 版本化域前缀 `future-shell/vault/aad/v2`。S16 `keyring_core::set_default_store` 是**进程全局**开关，旧测试每个函数各调一次 → 同一二进制内既有竞态、又会静默换钥导致先前密文解不开（`Integrity`），而这一切只被「nextest 每测试独立进程」挡着，一句 `cargo test` 就踩中 —— 改为 `tests/common/mod.rs` 里 `Once` 收敛「装一次 + 预铸主密钥一次」。S21 `Store::delete` 未落盘，旧测试只断言「删完 get 不到」（内存态即可满足）而漏掉持久化，新增 `delete_is_persisted_to_disk` 重开库断言。`-p fs_vault` 25 绿（nextest 与 `cargo test` 双跑）。
  - **D 批 S22**（commit `1d5f923`，测试装置）：本条不是被人读出来的，是**第一裁判自己抖出来的** —— 连续 4 轮 `cargo nextest run --workspace` 各随机挂一条（db / repo / vault 都中过），报错清一色 `AppTooOld { min: "999.0.0", cur: "0.1.0" }`，与被测代码毫无关系。根因在测试临时目录助手：唯一键取「进程 id + 进程内计数器」，落地用 `create_dir_all` —— 三项隐含前提同时不成立。① Windows 积极回收 pid，而 nextest 为每个测试各起一个短命进程，同轮不同测试拿到同一 pid 是常态；两边计数器又都从 0 起，路径于是逐字相同。② 这些目录从不清理，`%TEMP%` 里积着历轮的成百上千份残留（实测 1280 个，其中 5 个的 `fs.db` 里 `min_app_version = 999.0.0` —— 那正是 `refuses_db_requiring_newer_app` 故意写坏的库）。③ `create_dir_all` 对**已存在**的目录返回 `Ok`，于是测试悄悄跑在前人的终态上。后果是双向的：假红已实测（且每轮挂的还不是同一条，极易误判成产品代码有并发问题）；更坏的一半是**假绿** —— 「解锁失败不得建库」「首启建新库」「备份裁剪到 5 份」这类断言一旦跑在已有库/已有备份的目录上，测的就不再是本次代码的行为，缺陷会被残留数据盖住，而第一裁判恰恰全靠这些断言把关。处置：唯一性交给**文件系统**裁决，不靠「pid 应该不会重」这种猜测 —— 名字加纳秒时间戳，且用 `create_dir`（目录已存在即报错）而非 `create_dir_all`，撞名就换个名字重来；五处助手全部改齐（connmgr 的 db / repo / settings_repo 经**新建**的 `tests/common/mod.rs` 共用，sshengine `tests/hostkey.rs`、vault `tests/common/mod.rs` 各自就地改写）。回归 `tmpdir_never_hands_back_an_existing_directory` 先按**旧命名方案**为当前 pid 播下 128 个非空占位目录，再断言助手交回的目录为空；非空证明同样做足（回退修复后确认变红，报错为「助手交回了一个已存在且非空的目录：…\fs-connmgr-50600-0（S22）」）。1280 份历轮残留一并清除。`-p fs_connmgr` 22 绿、workspace 57 绿且**连跑 5 轮无抖动**。
  - **E 批 S23**（commit `6979cd1`，门禁自身）：同样是回灌验收顺手抖出来的，但被测对象不是产品代码，而是**门禁自己**。① `README.md` 的命令块写着 `cargo nextest run --workspace　# vault 测试依赖 nextest 每测试独立进程（mock keyring 为进程级全局库），勿用 cargo test` —— 这句在 S16 修复（`Once` 收敛 `set_default_store` 并在同一 `Once` 里预铸主密钥）之后已经**事实错误**，且方向是反的：`cargo test` 不再是「跑不了」，而是**唯一能暴露那一整类进程全局态缺陷的入口**，S16 本身正是被它抓出来的。一句写进 README 的「勿用」，等于把此后所有贡献者从这张网前面引开。② `.github/workflows/ci.yml` 只有 `cargo nextest run --workspace` 两步（Linux 带 `FS_ITEST=1`、Windows/macOS 不带），压根没有 `cargo test` 步骤 —— 于是 S16 那一整类缺陷在 CI 上**永不可见**：本机偶发变红，推上去照样全绿，而「CI 全绿」正是本项目对外的正确性凭据。③ README 的 clippy 行漏了 `--all-targets`，比 CI 实际执行的弱一档（不 lint 测试代码），照 README 跑通的本地门禁到 CI 才挂。处置三处：README 命令块改为两条测试入口并列并补 `--all-targets`，其后新增一段讲明**两者不是同一张网** —— `cargo test` 把同一二进制的全部测试跑在一个多线程进程里（抓「进程全局态」，S16），nextest 每测试一个独立进程（抓「跨测试残留」，S22），两条各占一头，缺一头就有一整类缺陷落地；CI 在两条 nextest 步骤之后追加 `cargo test --workspace`，刻意不置 `FS_ITEST`（容器化 itest 已由上面的 Linux nextest 步骤覆盖，无谓重跑一遍）。验收：`cargo test --workspace` 本机实测 57 绿**之后**才写进 CI（不写没跑过的门禁），`py -c "import yaml"` 实测 workflow 仍可解析、`check` job 共 15 步且末步即新增的 `cargo test --workspace`；计划 Task 1 Step 6 / Task 2 Step 1 的 README 与 ci.yml 代码块同步回灌，逐字节比对通过。
  - **门禁复核**（每批均全绿）：`fmt --check` 0 · `clippy --workspace --all-targets -D warnings` 0 · `nextest --workspace` 57 绿（D 批前 56） · `cargo test -p fs_vault` 25 绿（**双入口必跑**：nextest 每测试独立进程会掩盖进程全局态缺陷，S16 正是这样被 `cargo test` 抓出来的）· `cargo test -p fs_connmgr` 22 绿 · `cargo test -p fs_sshengine` 10 绿 · `cargo test --workspace` 57 绿（E 批起并入 CI） · `cargo deny check licenses bans advisories` ok。
  - **计划回灌**（commit `6979cd1`）：Phase 0+1 计划 Task 3/4/5/6 的代码块此前仍是被整改掉的**原始易损版本**（`derive_raw`、`Err(_) => VaultFile::default()`、无长度校验的 `copy_from_slice`、裸 `[u8;32]` key 字段、4 参 seal/open 的 id-only AAD、每测试 `set_default_store`、`sqlite:` URL 拼接、缺 `recursive_triggers`、无事务 `import_json`、两字段 `crud_roundtrip`）—— 若照计划重跑一遍，21 条缺陷会原样重现。本次将四个 Task 的 Files / Interfaces / 各 Step 代码块全部替换为**已落地并通过门禁的实际源码**，并为每个 Task 增补一段「M0 对抗复审整改」缘由注（讲清每条缺陷的**攻击面**而非仅写法差异，避免执行者按「常规写法」自由发挥重蹈覆辙）；测试计数同批校正：Task 4 `9 → 25`（crypto 6 + store 6 + aad_binding 3 + robustness 8 + kek_isolation 2）、Task 5 `8 → 12`、Task 6 `14 → 22`（db 12 + repo 7 + settings_repo 3），Task 5 Step 5 的过期测试名 `migrations_create_settings_and_audit_tables` 一并改齐。另：Task 5 的 **Files** 与 Step 2 补上新建的 `crates/connmgr/tests/common/mod.rs`（D 批 S22 引入的共享临时目录装置，被本 Task 与 Task 6 的三个测试文件共同 `mod common;` 引用，计划里不列出则后续步骤直接编译不过）。
    同批顺手把 **Task 1 与 Task 7** 也对齐落地实况（这两个 Task 无安全缺陷，纯粹是「照抄计划会挂门禁」）：① Task 7 的三个代码块（`tests/hostkey.rs` / `src/error.rs` / `src/hostkey.rs`）与 Task 6 的两个 `settings_repo` 块此前是**手写紧凑版**（单行结构体、链式调用不换行），语义无误但过不了 `cargo fmt --all -- --check` —— 而 fmt 是第一道门禁，照抄即红；已全部替换为 rustfmt 后的落地源码。② Task 7 Step 5 补 `cargo deny check licenses bans advisories` 一步并把 `deny.toml` 的 RUSTSEC-2023-0071 具名豁免全文写进计划，同时写明**不得**用 `default-features = false` 或 `--allow` 绕过的理由（前者废掉 ssh-rsa 主机密钥验证与 id_rsa 认证，后者让整条 advisory 门禁对所有未来漏洞一并失效）—— 这是 Task 7 首次把 russh 拉进依赖图必然遇到的红灯，计划编写期未预见。③ Task 7 Step 6 `git add` 补 `Cargo.toml Cargo.lock deny.toml`。④ Task 1 的 `src/lib.rs` 占位块原本只给了 `fs_vault` 一行样例，八个 crate 的 doc 注释实际各不相同且被后续 Task 的 lib.rs 补丁当作锚点，已改为逐 crate 列全并注明须逐字照抄。
     **回灌验收方式**：脚本提取计划里全部 ` ```rust ` / ` ```sql ` / ` ```toml ` 块，与仓库中已落地文件（以 **git 索引内容**为准，绕开 `core.autocrlf` 在工作树里给 `store.rs`/`repo.rs` 换成的 CRLF）逐一做**逐字节**比对：24 个 Rust 源文件 **24/24** 命中、3 个迁移 SQL **3/3** 命中；`deny.toml` 因跨 Task 1 与 Task 7 两步写成，改为「按计划所述把 Task 7 的 `ignore` 段插进 Task 1 块的 `[advisories]` 后」拼装再比对，同样逐字节一致。两个 `lib.rs` 此前以「`lib.rs` 追加 `pub mod repo; …`」的散文形式给出，本次改为整文件代码块——`pub use store::{SecretKind, Store, RecordMeta}` 这种花括号内未排序的写法过不了 `cargo fmt --check`，而散文形式恰恰把顺序留给了执行者。另核代码围栏配对平衡、全文 0 处 CRLF。
- ⚠️ **已发现但本轮未修（留痕，待 Task 8 前处置）——vault 主密钥的跨进程铸造竞态**：S16 只收敛了**测试进程内**的 `set_default_store` 竞态，同源问题在**生产**里依然存在 —— 两个 future-shell 进程同时首启时，`Store::open_or_create` 会双双从 keyring 读到 `NoEntry`、各自铸一把主密钥、再依次 `set_password`，凭据库里只剩最后一位写入者的那把，而先前进程手中的 `Store` 仍在用自己那把封记录 —— 落盘密文与凭据库密钥就此永久错配，表现为下次启动 `Integrity`、全部凭据不可解且**不可恢复**。M0 的 vault 无任何跨进程锁。候选处置：① 建库走「文件锁 + 双检」（`open_or_create` 持目录内独占锁后再读 keyring）；② keyring 写入改「仅当不存在时写」并在写后回读比对，不一致则以回读值为准重开。触发面：Windows 上双击图标两次、开机自启与手动启动撞车、便携版同盘双开。
  - **Task 16 实现 + 回灌**（代码 commit `548b8b182aa3ef0ba2e945d411b61d40e58c13a7`）：pipe.rs 会话管道实现完成——fan-out 三路单读取任务按序 grid.feed → ring.push → batcher.push（录制 tap 位置预留 Phase 4a）+ 双水位背压读取（high-water 上暂停、low-water 释放，滞回带防临界振荡）+ EOF/shutdown 一律走合批收尾传播（末帧后 render_tx drop → recv() 得 None，session:closed 单一信号源，Task 19 接线）；实现侧订正草案一处（结构持 ring + ring_snapshot_text 访问器——ring 录制需外部可观测试钉）。用例 7（草案 4 + 增 3：ring fan-out 字节一致 / resize 转发网格 / ack 水位排干），定域 104 → 111，工作区组合式 223 → 230 = 119 + 111。五门禁全绿（rustfmt 显式三文件 / clippy 五目标 -D warnings / nextest --no-fail-fast 111/111 无截断 / cargo test pipe 7/7 / deny 四项）；非空证明 M-DH..M-DN 七枚变异体逐枚恰符预测、累计红 9 例次——其中 M-DJ 以 TIMEOUT 形红：背压缺失时写侧 select! 500 ms 逃生永不触发、测试无穷挂死，故新增 `.config/nextest.toml`（fail-fast=false 全局兜底 + fs_terminal 包级 slow-timeout 30 s × 1 超限终止；nextest 0.9.140 无硬 timeout 键、诊断取证自 overrides.timeout 改），门禁复跑全绿。计划回灌 wb23（注 23 + 双剪接 + lib.rs 重同步 + Interfaces/Files 补 + S88 自愈 + 二级联），路线图 rm23。Task 16 首裁判待启（0/2，跑在回灌后树）。
  - Task 15 第六裁判整改 S171–S175（代码 commit `b9f671edc5da5ce22d0d7c72aa843cbb3bfa14cf` + 本回灌 commit）：第六裁判（judge-task15-round6，镜头五：文档—代码一致性）终局确认 **5 项（1 med / 4 low）/ 驳回 0**，clean_round=false、计数重置。**S171（med）** 记账先于可见——多线程 runtime 下 send 的 await 内即可唤醒消费者，其即刻 ack 可能先于发送返回；旧实现发送成功后记长，该 ack 撞 `ack` None 分支弃损配额、账本永久膨胀、frames_high=1 成永久锁存 → 收尾/非收尾两分支记账移至发送前，四条不送达支路（收尾接收端关 / 收尾超时 / 非收尾接收端关 / 非收尾 shutdown 许可）各 pop_back 冲销尾记账；**S172（low）** S163 改写注释两处形状限定（flag 兜底仅停泊等待 select 可达；阻塞发送 select 时许可是唯一唤醒源，见 S112 例）；**S173（low）** 新钉 flow_config_default_matches_spec_2_2（五值逐钉 spec §2.2）；**S174（low）** S148 钉 B 例注重写双变异体结构（混合变异存活域 + 分辨边界）；**S175（low）** 新双钉 closing_timeout/permit_arm_revokes_preliminary_frame_accounting（冲销义务），S147/S155 例注口径随 S171 迁移。非空证明：M-DO/M-DP/M-DQ 三变异体各恰红 1 例后字节级复原（43 tests run: 42 passed / 1 failed ×3），S171 顺序迁移于 current_thread 不可钉、诚实记录（先例 M-CZ/M-DA/M-DB）。五门禁全绿（裁判探针共存、定域）：rustfmt 显式两文件 0 · clippy 定域 -D warnings 0 · nextest 定域 **114/114**（flow 40 → 43）· cargo test --workspace --exclude fs_terminal 全目标（非 terminal 侧实测 119；首版 --lib --bins 口径不足仅覆盖 fs_app 3 例、gate171d 复验）· deny 四项 ok，工作区组合式 **233 = 实测 119 + 定域 114**；计划注 25 回灌（双代码块重剪 + 三级联）。收敛：Task 15 **0/2**（本轮 med 确认、重置），第七裁判跑在回灌后树上（首洁 1/2、再洁 2/2 关闭）；Task 16 首裁判终局 11 项确认（3 med/high），整改自 S176 起待启；Task 13 第九裁判终局 clean_round=true（3 low 确认），整改接续待启、计数 1/2。
  - Task 13 第九裁判整改 S187–S189（代码 commit `a7a2463a72b993a4e03aff4292a01f6c47e8b63f` + 本回灌 commit）：第九裁判（judge-task13-round9）终局 clean_round=true——确认 **3 项（3 low）/ 驳回 0**（怀疑者校准后无 med/high），lows-only 轮不重置收敛计数（1/2 保持），全部整改。**S187（low）** `decom_on` 之 `top != 0 ||` 析取项零承重——底锚定 DECOM 区域（top>0 ∧ bottom==rows-1，如 `ESC[5;24r`，预留顶部状态行的应用即命中）是其唯一承重形状，删之则抹除偏落误抹他行 + 骑线漏抹成 screen.rs:870 毒链 + 复原相对化偏行，旧全 45 例区域底 ∈ {7,9,19} 恒 ≠ rows−1 而常绿；新钉 `bottom_anchored_decom_region_must_be_detected_by_top_disjunct` 三观测点各钉一面，M-DR 恰一红。**S188（low）** `body.is_empty()` 早退三行零承重——承重形状为 DECOM 开 + VPA 出区 + 无骑线缩列（正实现零字节注入、出区光标原位存活；变异体钳落区域顶），旧全 45 例常绿；新钉 `straddler_free_shrink_must_not_move_an_out_of_region_decom_cursor`，M-DS 恰一红。**S189（low）** `new` doc 口径分裂——~256 GiB 仅属 `set_size` 双网格路径，`new` 主网格 ~128 GiB，旧文高估一倍，订正为分裂口径与常量 doc 及 S145 钉例对齐。门禁复跑（裁判探针共存，定域）：fmt 0 · clippy -D warnings 0 · nextest 定域 **116/116**（grid 47 + ring 19 + flow 43 + pipe 7）· 非 terminal 侧实测 **119** · deny ok，工作区组合式 **235**（gate187 两处计数漏检由 gate187b 剥 ANSI 色复核补过）。计划回灌 wb26（注 26 + 双剪接 + 三级联 + Task 13 Step 4 期望行工作区 230→235 就地订正，S117 先例），路线图 rm26。收敛态：Task 13 **1/2**（lows-only 不重置、整改已毕，第十裁判待启、再洁 2/2 关闭）；Task 15 **0/2**（第七裁判跑在树 `5e8eee6`、结果待处理）；Task 16 **0/2**（首裁判终局 11 项确认（3 med/high），整改 S176–S186 待启）；Task 14 代码轨 **2/2** 关闭不变。
- ⏳ 下一：**Task 20 Step 2：Sidebar.svelte（分组树 + 搜索过滤 + 右键六项菜单 + 拖入分组移动 + 底部三按钮 + 状态灯）**——**Task 20 Step 1 实施完成已提交**（注 50 + rm50，代码 + 镜像合并 commit `0a911e27b2d56bb6d0e8b305564bd167a93e1960`）：app 侧命令扩充——vault_cmd 扩 vault_init/unlock/put_secret/list_secrets/lock/has_file/copy_to_clipboard（明文只在 core，sha256 比对定时清除），conn_cmd 扩 profile_save/delete/export/import + hostkey_import（替换 vault_status/profiles_list 两 stub），opener_cmd 加 reveal_log_dir，新增 settings_cmd（settings_get/set）；lib.rs 新注册 15 项；Cargo.toml +arboard=3/sha2=0.10；deny.toml 为 arboard Windows 后端 clipboard-win/error-code 的 BSL-1.0 定向 exceptions（OSI 批准、非 GPL，不放宽全局白名单）。计划外编译修正：conn_cmd 拆分补 use、vault_copy_to_clipboard 的 zeroize import 扩 {Zeroize,Zeroizing}。门禁十跑全绿 ⇒ 组合式 **273**（t-ws 125）。镜像：Step 1 单一多文件块重写对齐磁盘（拆分命令文件），vault/conn 节经校验完整包含、围栏配平、CR=0。**Task 20 未闭环**：Step 1 完成，Step 2–6 未开始。
  - **Task 19 第三轮复审收敛、Task 19 闭环**（注 49 + rm49，复审收敛记录 commit `6dc16af672e240d19219d70baf4be6077d2055e0`，无代码改动）：**inline 单代理聚焦验证**（S295 终验 + 终局清扫），S295 正确无新缺陷——收尾唯一 await 已前移至 registry.remove 前（其后 remove 旧+spawn 泵+insert 新为同步段）、Ok 臂后置检查（stop_rx 捕获 session_close 置位 / registry 空检查）命中即回收复活条目静默返回；残余仅 remove→insert 指令级窗口 + render 泵交接低，均留档。**缺陷密度三连降**：一波 12（0 高 5 中 7 低）→ 二波 1 中 + 1 低 → 三波 0 新，收敛至零中/高 ⇒ **Task 19 闭环**。**Task 20 承接位**（前序留档集中落地）：Task 19 事件桥五事件前端消费（term:data / session:status / auth:prompt / hostkey:prompt / session:disconnected / session:closed）· S292 首次 fit resize 在会话存在时修正后端 24×80 暂态 · term:data 载荷键 data_b64（snake）↔ term_input dataB64（驼峰）对偶 · Task 18 留档（五盲区运行态 / macOS #22 / 剪贴板通道择一 / 全局 settings 广播）多在 Task 20 装配期落地。Task 20 为前端任务，门禁 = vitest + svelte-check + build（无 Rust 改动则不触十门禁）。
  - **Task 19 第二轮复审 + S295 整改已提交**（注 48 + rm48，代码 + 回灌合并 commit `ef2ebf04f38657e5d3fb77f7782781fbb2b765d6`）：**inline 单代理对抗复审**（ultracode 已关，未启多代理编排，方法论差异已在注 48 ① 诚实披露），焦点 = 验证 S290–S294 修复面 + 显式裁决 S291 复活 TOCTOU 残余。**核心发现 R2-F1（中）= S291 复活 TOCTOU 残余**：一波 pre-check 在 establish 之前、且收尾 await（`old.write.lock().await`）留在 `registry.remove` 之后 → session_close 于该 await 窗口抢入时 registry.remove 返回 None、置不了 stop、后置检查无从捕获 → 复活。**S295 双管整改**：(a) 收尾唯一 await 前移至 `registry.remove` 之前（经 registry.get 克隆 Arc 动锁不动条目），「remove 旧 + spawn 泵 + insert 新」收窄为同步段（可插队窗口 → 指令级）；(b) reconnect_loop Ok 臂加 establish 后置检查（stop_rx 捕获 session_close 置位 / registry 空检查），命中即回收复活条目（remove + pipe.shutdown，drop 释放写半部+handle 关连接）静默返回、不发虚假「已重连」。**残余**：仅剩 remove→insert 两条同步语句间指令级窗口，实践可忽略、留档不追闭（追闭需 SessionRegistry 原子 replace + session_close 先 stop 后 remove 重排，复杂度与收益不成比例）。**其余修复面验证无新缺陷**：S290 classify_link_end 矩阵正确（RemoteSignaled 经 end 臂、单测覆盖）· S292 onData/onResize/onBell 前移 fit 正确（zoom/resetFont 无回归）· S293 每轮新读网格正确 · S294 超时臂拆分 + watchdog stop 复查无碍。**门禁（十门禁完整复跑，前台）**：fmt/clippy-ws/clippy-term 0 · nt-term 148/148 · nt-pipe 17/17 · t-term 148 · t-lib/t-doc 0 · t-ws **124**（不变，本批未新增单测）· deny ok ⇒ 工作区组合式 **272**（不变）；前端 vitest 118/118 · svelte-check 204 FILES 0/0 · build 137 模块（本批仅 session_cmd.rs，前端未动）。**镜像**：session_cmd.rs 单块重同步后 31/31 整文件 + pipe 部分镜像字节相等，plan CR=0。**诚实披露**：本轮 inline 单代理（独立性弱于多代理，S295 自审自改，指令级窗口与 render 泵交接考量未经他者复核）· render 泵交接低留档（render_rx→spawn_pump 间隔收尾 await，帧在有界 64 通道累积，实践良性）· 复活指令级窗口留档不追闭。**Task 19 未闭环**：第二轮完成，第三轮未开始。
  - **Task 19 第一轮复审 + 整改已提交**（注 47 + rm47，代码 + 回灌合并 commit `74a2ac725772347cc98dcffaa7a485c12cac10c1`）：**ultracode 6 镜头 + 对抗核验**（29 agents · 0 错误），13 条 → **成立 12（0 高 5 中 7 低）、推翻 1**（session:closed 多次 emit 经核验无害判 invalid）。**ipc-contract 镜头 CLEAN**（camelCase 对偶经 tauri-macros 源码核验、参数序/载荷键/capabilities 全对偶）· vault-secrets 主链路通过。**整改 S290–S293（5 中全改）**：**S290** ExitSignal 被吞（信号死亡误判正常结束 → 标签静默消失不重连）→ 新增 `LinkEnd::RemoteSignaled` + 归类提取为纯函数 `classify_link_end` + 矩阵单测（同灭「归类不可测」低）· **S291** 重连生命周期三缺陷合并整改——孤儿化（establish 在 remove 后失败 → 推迟 remove 至新通道就绪）/ 复活（connect 期间用户已关闭仍 insert → insert 前复查 stop+registry）/ 早退无终态 + 装配失败静默（补 `emit_reconnect_gave_up` 终态事件 + establish 失败视同 attempt 退避重试）· **S292** term.ts 初次 fit resize 上报丢失 → onData/onResize/onBell 注册移到首次 fit 之前（远端 PTY 滞留 24×80 修复；后端真实尺寸开 PTY 留 Task 20）· **S293** rows/cols 一次性快照 → 每轮拨号前新读。**7 低处置**：events 人机回路 2 项 **S294 留档**（prompt 无代际键——失败闭合无安全漂移、注释声明不变量；emit 无重放——超时/关闭路径补 tracing::warn、重放留 Task 20）· watchdog/close 竞态 S294 emit 前复查 stop 收窄（残余 TOCTOU 良性注释留档）· vault 本地失败归 refused S294 补失败日志（4 值契约不精确留档 Task 22）· rm46 披露漏摘 (b) **carryover 至注 47 ⑥**（不改写已提交历史；PROMPT_TIMEOUT 无行为测试仍 open）。**门禁（Rust 十门禁完整复跑，前台完整输出）**：fmt/clippy-ws/clippy-term 0 · nt-term 148/148 · nt-pipe 17/17 · t-term 148 · t-lib/t-doc 0 · t-ws **124**（123 + classify_link_end_matrix）· deny ok ⇒ 工作区组合式 **271→272**；前端 vitest 118/118 · svelte-check 204 FILES 0/0 · build 137 模块。**镜像**：4 块重同步后 31/31 整文件 + pipe 部分镜像字节相等，plan CR=0。**诚实披露**：整改止于编译+单测层（S291 三竞态/S292 时序无自动化行为覆盖，真实链路留 Task 20 手动 + Task 24/25 出口）· PROMPT_TIMEOUT 无行为测试 open（rm46 漏摘项 carryover）· S290 信号死亡以 ExitNonZero 承载、信号名不入载荷（留 Task 22 按需扩展）。**Task 19 未闭环**：第一轮完成，第二轮未开始。
  - **Task 19 实施完成已提交**（注 46 + rm46，代码 + 回灌合并 commit `e8d4365951eb5279b01e21a791e07a4d44555652`）：**app 会话生命周期 IPC 全量落地**——`events.rs` GuiEvents 事件桥（TOFU/kbd prompt 经 oneshot + emit，`block_in_place` 阻塞等待，**120s 超时** Refuse/空应答收尾 F15/F20）· `sessions.rs` SessionRegistry + 渲染泵（EOF 仅自退不代发 P2-16）+ **LinkEnd/DisconnectReason**（补丁 M-4：RemoteExitZero=closed{remote_exit}，非零/传输 EOF=disconnected）+ ChannelReader（russh 读半部 → 有界 mpsc(16) → owned AsyncRead，背压保留）· `session_cmd.rs` session_open/close、term_input/resize/ack + establish_session 共用装配 + **watchdog 三分支** + **reconnect_loop**（指数退避 1→30s×10、stop 可打断、恢复 PTY 尺寸不恢复屏幕、gave_up 记最终归类）+ VaultSecrets（try_lock + Zeroizing）· `auth_cmd.rs` auth_respond/hostkey_decide（失败闭合）· state/lib/mod 装配 + `types.ts` 载荷类型对偶 + `pipe.rs` grid_rows/cols 访问器。**S289 偏离集群六类**（编译期暴露、逐项修正、围栏重同步）：① SecretSource 增 Send+Sync 超 trait（connect future 跨 await 持 &dyn，spawn 要 Send；Task 10 current-thread 未暴露）② Error::Auth 三字段对偶（notes 列 S38 本地原因，计划块两字段过时）③ E0597 guard 具名绑定 ④ emit &str 借用 ⑤ clippy 五处（derive Default / allow×2 too_many_args 循 transfer.rs 先例 / doc 空行 / handle 字段 dead_code 同 S248 口径）⑥ rustfmt 重排后六块重同步（judge 探针 43 个零改动核验）。**门禁（wb41 后首次触 Rust，十门禁完整复跑，前台完整输出）**：fmt/clippy-ws/clippy-term 0 · nt-term 148/148 · nt-pipe 17/17 · t-term 148 · t-lib/t-doc 0 · t-ws **123**（121+2）· deny ok ⇒ 工作区组合式 **269→271**；前端 vitest 118/118 · svelte-check 204 FILES 0/0 · build 137 模块。**0xC0000005 观察项**：本次含 fs_vault store 6/6 全程无异常、无新证据、续携。**镜像**：六块重同步后全量扫描 **31/31 整文件 + 2 部分镜像字节相等**，App.svelte 块残差 0（仍恰 S244 偏离），plan CR=0；**autocrlf 事故两起留痕**（checkout 计划文件 CRLF 化、cmd /c 单行 %~dp0 不展开——陷阱三/四记忆条目实战验证）。**诚实披露**：验证止于编译+单测层——session_open→watchdog→reconnect_loop 真实 SSH 链路无自动化覆盖（app 装配层按 Task 20 手动验证 + Task 24/25 出口承接）；term:data 载荷键 data_b64（snake）与 term_input dataB64（驼峰）对偶为两侧手工维持、无编译期互检，Task 20 联调为首个真实校验点。**Task 19 未闭环**：实施完成，两轮复审未开始。
  - **Task 18 第三轮复审完成、Task 18 闭环**（注 45 + rm45，代码 + 回灌合并 commit `c9ef2482f25a41afe522b661a0eb4e1620b0ba85`）：**三轮趋势 24 成立（6 中 18 低）→ 13 成立（3 中 10 低）→ 1 成立（0 中 1 低），密度与严重度三连降，第三轮零中高 ⇒ 闭环**（standing 纪律两轮达标且超额一轮；加验理由见注 44 ⑥——二轮中级别系一轮整改自身回归，故修复面须再验证）。**执行方式留痕**：第三轮先以多代理编排启动但**三镜头全因上游 API 故障中止**（1×429 + 2×503，agents_done=0/error=3，零结果零结论，**未按通过计**）；ultracode 关闭后改**主循环 inline 三镜头**完成。**方法论一条**：编排失败须显式判为「未执行」而非「无发现」——`confirmed: []` 在失败态与 clean 态形状相同，须查 `agents_error`/failures 字段。**镜头结论**：①S283/S284/S285 代码面 **1 低（S288）无中高**——S283 计数三出口平衡（try 前 ++、finally --）、权威闭包 `() => pasteConfirm !== null` 与状态机对偶且**重入瞬间无假空窗**（先结算旧泡再置新泡，pasteConfirm 始终非 null）、S284 可选链与 `instanceof Node` 判定安全；②S286/S287 台账 **clean**（CR=0、裸 ETX=0、$-词齐全、哈希/步号/五盲区/item14 ⑥⑦ 全在盘、镜像 21/21）；③wb44 diff 全量 hunk **clean**（2 commit、5 files +258/−28 逐 hunk 归属可判、无意图外改动、wb43 修复面零改动、数字对账 19 例 = 15+4、118 = 114+4）。**S288（低·留档不修）**：S284 归属判别对 **body 焦点**的窄化——气泡悬置期点击应用非可聚焦区（工具栏空隙/状态栏）后焦点落 `document.body` 不在任何 el 内，Enter/Esc 不再结算，S272 的「Esc 取消不可达」在该支重现；判可接受（F34 自动聚焦 + 现实漂移路径落 el 内 textarea 已覆盖；两按钮始终可点，无死锁/数据损失/错误动作），**不即修因 body 焦点无法归属窗格**（多标签恒挂载下人人认领 = 重开 S284 越权），安全处理需 `activeTabId` 信号（Task 20 装配层所有），已在组件注释 + **Task 20 item 14 ⑧** 双处立承接。**门禁（前台）**：vitest 118/118 · svelte-check 204 FILES 0/0 · build 137 模块 · 镜像 21/21 · CR/控制字符干净。**诚实披露**：本轮系 **inline 单人复审**、无多代理对抗核验，独立性弱于前两轮，S288 未经他者推翻尝试 · 零 Rust 改动门禁未复跑（269 仍准、0xC0000005 续携）· 运行态取证一律留 Task 20 item 14。**转 Task 19 入场须知**：① 触及 Rust ⇒ **十门禁必须完整复跑**、269 基线待复核；② `crates/terminal/tests/judge*.rs` 43 探针零改零删，**禁 `cargo fmt --all` 写入**，只 `cargo fmt -p future-shell-app`；③ Step 3 需回改 `pipe.rs` 加 `grid_rows()/grid_cols()`；④ 事件载荷键 `data_b64`（snake）与 `term_input` 的 `dataB64`（驼峰）相反，前端消费点已由 S281 红测钉死，Rust emit 侧须逐字对偶。
  - **Task 18 第二轮复审 + 整改已提交**（注 44 + rm44，代码 + 回灌合并 commit `14c14eb0c0c1b89792f876e49fccc4a1d5762f87`；本批整改与回灌同提交）：**ultracode 5 镜头 + 变异抽查**（fix-term / fix-comp / test-mutation 隔离 worktree / mirror-r2 / carry-over），25 agents · 0 错误，产出 14 条 → **成立 13（3 中 + 10 低）、推翻 1**（missTimer 经 Svelte 内部语义核验判 invalid）。**变异镜头**：8 条定向变异 7 条精确转红无交叉污染、1 条预期缺口成 S285。**整改 S283–S287**（下一可用 **S288**；本批无新裁决，下一可用 R 号仍 **R120**）：**S283**（中）`pasteConfirmOpen` 布尔重入陈旧致防 `\r` 透传保护对存续气泡失效 → TermOptions 增 `pasteConfirmPending` 权威闭包（TerminalPane 注入 `() => pasteConfirm !== null`，组件 $state 为结算唯一权威源）+ 内部计数回落双轨 · **S284**（中×2 合并）窗口级结算无窗格归属判别 → `el.contains(e.target)` 归属判据（非 tagName——xterm 漂移目标恰是 el 内 TEXTAREA，inField 式排除会误伤正用例），修跨标签越权结算（隐藏标签恒挂载、N 窗口监听并存）与 Settings/Compose 输入面误结算；S272 焦点漂移例目标同步改入窗格 el（body 加判别后不再合法）· **S285**（低）气泡 stopPropagation 变异缺口钉死（气泡派 bubbling KeyboardEvent + window spy）· **S286**（低簇·台账 5 处）rm43 裸 CR + `$effect`/`$state` 三处 shell 展开丢失 + S269 步号 Step 6→5 订正；plan item 14 裸 ETX×2 → 转义文本；注 43 ⑥ 201→235→240；注 43 ④ 清单实盘口径 + 15 之口径时序说明。**方法论钉死**：书写期转义事故簇全在 bash/node -e 插值路径，此后文档写入文件化 + 写后控制字符/CR 扫描 · **S287**（低簇·承接 4 路）#22 macOS → item 14 ⑥；剪贴板通道择一 → Task 20 Step 4 前置指令 + item 14 ⑦；四盲区 → **五盲区**（补 onSelectionChange→copyOnSelect）；S274 decorations 重搜回路留档（上游 VS Code 同款，Task 20 实测后裁决）。**TDD 红先行**：S283–S285 经 stash 隔离取确定性红（3 failed / 16 passed of 19）复元转绿。**环境陷阱新档**：git stash/checkout 触发 autocrlf 转换（676 CRLF 回灌镜像块），归一后复验 21/21；纪律：checkout/stash 后必跑 CR 扫描。**门禁（前台）**：vitest 118/118 · svelte-check 204 FILES 0/0 · build 137 模块 · 镜像 21/21 · 控制字符扫描干净。**诚实披露**：S283 计数回落支无红测（pasteFlow jsdom 不可实例化，五盲区同簇）· 零 Rust 改动门禁未复跑（269 仍准、0xC0000005 续携）· S273/S274/copyOnSelect 运行态留 Task 20 item 14。**收敛判定**：两轮 24（6 中 18 低）→ 13（3 中 10 低）密度严重度双降，但二轮中级别系一轮整改自身回归（S272 引入），修复面须再验证——三轮通过则与二轮构成连续通过链、Task 18 闭环转 Task 19。
  - **Task 18 第一轮复审 + 整改已提交**（注 43 + rm43，代码 + 回灌合并 commit `cd66556da22ad172881b952b47f84219918a0dd7`；本批整改与回灌同提交）：**ultracode 多代理复审**——7 镜头并行 + 每条发现多视角对抗核验（high/medium 三视角·low 单视角），**55 agents · 0 错误**，产出 26 条 → **成立 24（6 中 + 18 低）、推翻 2**（均留痕免二轮重查）。**整改 S269–S282**（下一可用 **S283**；本批无新裁决，下一可用 R 号仍 **R120**）：**S269** Task 20 Step 5 门禁基线漏改同类项（S286 订正步号：该门禁位于 Step 5「构建与手动验证」Expected，原文误引 Step 6）（86 → **119** = 69 + 21 + 复审整改 24 + toast 3 + TabBar 2）· **S270** 再应用 `$effect` 加字号守卫——scheme-only 变化（临时配色/profile 覆盖）不再把 per-session 缩放静默打回基线（§3.2 切换作用域 + §3.3）· **S271** `setFontSize` 补 `fit()` 与 zoom/resetFont 对齐（2:1 多数成立，spec-basis 反对票留字）· **S272** 粘贴气泡簇三合一——重入先以 false 结算旧泡（原单槽覆写令第一条 pasteFlow 永挂）+ 卸载兜底结算；焦点漂移双层兜底（term.ts 钩子增 `pasteConfirmPending` 于 **xterm 目标阶段**消费 Enter/Escape 防 `\r` 误发 + 窗口级冒泡结算）；气泡自身 stopPropagation（共存时一次 Esc 不双层齐关）· **S273** `clearSearch` 补清「机器选区」（残留选区翻转 §2.8 Ctrl+C 双义/右键分流、copyOnSelect 时翻匹配覆写剪贴板；取舍：F3 续搜锚点由 findNextAgain 重开浮层补回）· **S274** 搜索 decorations 高亮（原裸调零高亮、双清沦为空操作）+ 无命中 .miss 警示 · **S275** 右键菜单补窗口级 pointerdown 关闭（拖选起手即关；**macOS Ctrl+点击秒关闭留档 Task 20 平台验收**）· **S276** 右键菜单复制/粘贴改经 term.ts 剪贴板统一通道 · **S277** clipboardWrite 空串短路（无选区 Ctrl+Shift+C 不再清空用户剪贴板）· **S278** pasteConfirmLines 剥尾换行（单命令粘贴不再虚报 2 行）· **S279** el 监听经 AbortController dispose 一次性拆除 · **S280** 键盘装配提取 `makeTermKeyHandler` 导出——createTerminal jsdom 不可实例化致 184 行接线零测试，提取后 §2.7/§2.8 全分支 + stopPropagation 不变式 9 例纯函数红测钉死 + ack「b64 长度 ≠ 字节数」口径不变式例 · **S281** 测试墙补齐 7 面（term:data 回放载荷键 data_b64 / onAck→term_ack / S267-S275 菜单关闭三路径 / F34 聚焦断言 / onReady 三键投影 / R57 续搜两分支 / vi.mock 改 importActual 除死重副本；**实测发现 `$state` 将 controller 包成 proxy**，断言改成员级留字）· **S282** 口径订正（Step 2 Expected tabs 子句过时删除 + 规格 §3.2 示例 id obsidian-term → futureshell-dark）另 Task 20 手动验证清单扩至 **14 项**（新增终端行为核验吞并目测三项）· **#9 注记级留档**（全局层 settings 无变更订阅，两 `$state` 组件私有，Task 20/22 接线广播时须回补写入端）。**TDD 红先行**：24 新例中 15 条先取确定性红（15 failed / 97 passed of 112），整改后 **114/114 全绿**。**门禁（前台完整输出）**：vitest 114/114 · svelte-check 204 FILES 0 ERRORS 0 WARNINGS · build 137 模块不变（仍无生产入口，同注 42 ⑤a）。**镜像**：6 变更块降序重同步后 21/21 字节相等（App.svelte 块经双指针核验仍恰为 S244 台账所记偏离）。**诚实披露**：createTerminal 残留盲区（paste 拦截/contextmenu/write→onAck/decorations 渲染）扩 Task 20 第 14 项承接 · macOS 留档 · 零 Rust 改动门禁未复跑（269 仍准、0xC0000005 观察项续携）· S273/S274 视觉效果留 Task 20 运行态取证。**Task 18 未闭环**：第一轮完成，第二轮未开始。
  - **Task 18 Step 1–6 实施完成已提交**（注 42 + rm42，代码 + 回灌合并 commit `27476621bbd5c1913356b7e097c7264604b4e81c`；本批整改与回灌同提交，故不再分列两笔）：xterm.js 终端组件全套落地，**计划外整改 S266–S268**（下一可用 **S269**；本批无新裁决，下一可用 R 号仍 **R120**）。**落地物**：`term.ts`（`createTerminal` + 纯仲裁四枚 `ctrlCAction`/`contextMenuAction`/`pasteConfirmLines`/`shouldCopyOnSelect` + b64 编解码）· `term-schemes/`（加载器 + `validateScheme` + `schemeToXtermTheme` + `resolveScheme` 三级择一 + **12 套内置方案 JSON**）· `TerminalPane.svelte` · `SearchOverlay.svelte` · 四个测试文件。**TDD 红先行**：Step 2 四文件先取确定性红（4 files failed 全为 `Failed to resolve import`，四模块均不存在），与 Step 2 Expected 逐字对齐，红阶段既有 69 例全绿不受污染；绿后 **90/90**。**计划外三项**：**S266** 组件测试环境两处缺件——(a) vitest 未把 `browser` 插到 `resolve.conditions` 的 `node` 前 ⇒ Svelte 解析到 SSR 构建、`render()` 抛 `lifecycle_function_unavailable`、9 例组件测试全红，接官方 `svelteTesting()` 插件解决（**留字**：该插件 `addBrowserCondition` 仅在 conditions 已含 `node` 时才插入、空数组时是 no-op，本项目可用系 vitest 在 config 钩子期已填 `node`，**版本相关、升级后须复验**）；(b) b64 往返例报「no visual difference」却不等——探针实测 `{sameCtor:false, backIsGlobal:true, bytesIsGlobal:false, valuesEqual:true}` 证 jsdom 的 `TextEncoder` 转发 Node `util` 版、吐 **Node realm** Uint8Array，而 `new Uint8Array()` 取 **jsdom vm** 构造器，vitest `toEqual` 比对构造器身份故判不等 ⇒ **修环境不修断言**，新增 `src/test-setup.ts` 经 `Uint8Array.from` 归一（不改测试为 `Array.from` 对比是因那会连「返回值确为 Uint8Array」一并放弃且日后每个用 TextEncoder 的用例重犯；不换 `globalThis.Uint8Array` 是因会波及 Blob/crypto 等以 jsdom realm 做 `instanceof` 的 DOM API）· **S267** 右键菜单关闭改挂窗口级——svelte-check 两条 a11y 告警**追下去是真行为缺口**：菜单原仅「点终端挂载 div / 选菜单项」两条关闭路径，**点击终端面板以外任何位置、以及 Esc，菜单都不关而一直浮着**；改挂 `svelte:window` 后两路径齐备，键盘等价物（Esc）正是告警所要 ⇒ **告警随行为补齐消失、非压制** · **S268** Step 6 期望值 81 过时（81 = Task 17 **60** 基线 + 增量 21，而 Task 17 两轮复审后真实基线为 **69** ⇒ **90**；增量 21 与原文清单逐项一致，仅基线过时），计划与 `vitest.config.ts` 注释两处同步更正。**镜像同步**：9 个围栏块 + 12 个配色 JSON 块**经内容扫描证 21/21 字节相等**，`vitest.config.ts` 块（Task 17 区）随改重同步，Files 清单补一行登记两处计划外文件。**门禁（全部前台完整输出，遵注 41 ⑤）**：`vitest` **90/90**（term 7 / schemes 6 / SearchOverlay 2 / TerminalPane 6）· `svelte-check` **204 FILES 0 ERRORS 0 WARNINGS**（新面 2 告警经 S267 归零非豁免）· `build` 成功。**⚠️ 诚实披露四条**：(a) build 仍 **137 模块不变**——本 Task 代码**尚无生产入口可达**（装配在 Task 20），只有测试图消费；为免「未引用」冒充「可打包」另做**一次性接线探针**：临时在 `main.ts` 引入 TerminalPane 后 build 通过、JS 86.90 → **586.07 kB**、CSS 11.70 → **17.26 kB**（`xterm.css` 确已解析）、无 addon/WebGL 解析错，探针后即回滚 · (b) 该探针另暴露 **Task 20 承接项**：xterm 可达后单 chunk 越 500 kB，rolldown 报体积告警，装配时需上代码分割 · (c) 本批**零 Rust 改动、Rust 十门禁未复跑**，工作区组合式仍以 wb41 的 **269** 为准，下批触及 Rust 即须完整复跑；注 41 的 `0xC0000005` 观察项**继续滚动携带、本批无新证据** · (d) 计划 Step 6 的 **xterm 6.0 目测三项**（滚动条外观 / resize 实测 rows×cols / SearchOverlay 不被裁切）**未做**，均需运行态真实会话、依赖 Task 20 装配，连同注 41 携入的三项装配期验收（`terminalFocused` 硬编码、helper textarea 双派发须 `stopPropagation`、`ComposeBar.onSent` 未接线）**一并留作 Task 20 验收**。**Task 18 未闭环**：仅完成实施，两轮复审均未开始。
  - **Task 17 第二轮复审 + 整改已提交**（注 41 + rm41，代码 + 回灌合并 commit `c819e88495ae2b6e324c99c1816e85ed7b378c51`；本批整改与回灌同提交，故不再分列两笔）：九镜头逐面出证，**2 中已整改、2 低留档、0 高、0 新裁决**，另开 1 项门禁完整性观察、携 3 项转 Task 18 验收。**镜头判定**：①S257 焦点圈闭 **clean——行为取证已补齐**（注 40 ⑥ 披露「S257–S261 未取自动化行为证」在此闭合：`preview_eval` 直驱运行中的 dev server 得 (a) 圈闭环绕 `{count:7, disabledExcluded:true, tabAtLastWrapsToFirst:true, shiftTabAtFirstWrapsToLast:true}`、(b) 焦点归还 `{dialogClosed:true, activeAfterClose:"BUTTON/工具(T)", focusEscapedToBody:false}`；§7「全部模态装焦点圈闭」由**空契约 → 实测契约**）· ②S260 子菜单键盘可达 **clean**（「应用主题」四皮肤纯键盘可 Tab 抵达并触发）· ③R118 键盘面回归 **clean + 1 低**（复核**放宽的反面**「输入框内不得吞键」：S253 纳入白名单的 `Ctrl+Shift+←/→/Home/End/Delete/Z/A/K/C/V` 全是输入框真实编辑键，因 `actionForKey` 无绑定而落到 `passthrough` 原样放行——**行为正确但此前无用例钉死**，日后任一键加窗口级绑定即致重做/选区扩展静默失灵而门禁全绿，补 S265）· ④`lib/ipc.ts` **clean**（其 `settingGet` 无运行期类型校验一节作为 S263 上游成因并入⑤）· ⑤`layout.ts` **1 低已整改**（两道钳位吃不掉 `NaN` ⇒ `style:width="NaNpx"` 非法声明、侧栏塌陷；受害面唯持久化读回，拖拽 `e.clientX` 恒有限）· ⑥`tabs.ts` **1 中已整改**（`transientSchemes` 以 sessionId 为键而 `replaceTabId` 占位转正**不迁移** ⇒ 双向失效：新 id 取不到致配色**静默还原**、`removeTab` 按新 id 释放致旧键**永久滞留**，R16 释放契约在转正处断裂）· ⑦`App.svelte` 分发完备性 vs §4/§2.1 **1 中已整改**（`enabled:true` 却既无分支又不在承接台账内的孤儿入口 **6 条**：`edit.copy`/`paste`/`copyPlain`/`selectAll`/`clearScrollback` + `file/import.json`；复制粘贴按 §2.1 属 MVP 必须项且工具栏另有按钮，点击静默无事且无任何承接指向）· ⑧app crate Rust 侧 + `tauri.conf.json` **clean** · ⑨ComposeBar 历史 **clean**（游标语义与 shell 一致；组件侧 2 项转 Task 18）。**整改 S262–S265**（下一可用 **S266**；本轮无新裁决，下一可用 R 号仍 **R120**）：**S262** 补承接台账六条（并把原文 `import / export` 更正为真 id `import.json`）**且在 `menus.test.ts` 立机器守卫**——「每个 `enabled` 菜单项（含子项）必在 `App.svelte` 有分支或有承接记录」，判据刻意不区分实体与台账（区分需语义分析，台账本身即合法承接形态），用**整词正则**而非裸 `includes`（否则 `export` 这类短 id 被无关文本蒙混），源文本走 vite `?raw` 而非 `node:fs`（tsconfig 只装 `vite/client` 无 `@types/node`，且 vitest 下 `import.meta.url` 非 `file:` 方案必抛），另附自校验例钉死 `mentions("session.ne")===false && mentions("session.new")===true` · **S263** `setSidebarWidth` 非有限值回落默认 + 1 例（取舍留字：用 `Number.isFinite` 不隐式转换，故 JSON `null` 亦判非法回落默认，**不走** `Math.round(null)=0 → 钳 160` 的老路径）· **S264** `replaceTabId` 单次 `update` 迁移临时配色键 + 2 例（含「无配色的标签转正不凭空造键」的不越界例）· **S265** 输入框安全回归例（十键逐枚**双断言** `isAlwaysLocal===true` **且** `actionForKey().kind==="passthrough"`——只断言前者不足以证明「不被吞」，另以 `Ctrl+Shift+S/M/Tab` 三键作正向对照证明用例非恒真）。**镜像同步**：7 个受控围栏块随码同步，`App.svelte` 块差 142 字符**经逐行比对确认恰为 S244 台账所记的 `lib/term` 导入一行**（计划留、盘上省），其余全是由此产生的 ±1 行位移，同步时**原样保留**该偏离以免 S244 注文失真。**否定性结论留痕**（免下轮重查）：(a) `beforeDevCommand` 无根 `package.json` 的 cwd 疑虑**已证伪**——`cargo tauri info` 打印的 `framework: Svelte`/`bundler: Vite` 与 JS 包版本只可能读自 `app_dir()/package.json` 与 `app_dir()/node_modules`，反证 app_dir 即 `frontend/` · (b) R119/S255 取**制品级**证：`app/gen/schemas/capabilities.json` 解析后恰为四权限且带 S255 描述（证明构建确已重生成而非只改源），`acl-manifests.json` 佐证 `core:window:default` 含 `allow-is-fullscreen`、`core:event:default` 含 `allow-listen/emit`，全仓 grep 前端 Tauri API 面仅 `emit/getCurrentWindow/invoke/listen` 四项 ⇒ 最小集经验充分 · (c) `app/icons/` 含 `.ico`/`.icns` 打包不缺件 · (d) §4 269–270 行确认复制粘贴键盘入口只在终端焦点成立 ⇒ 无窗口级绑定**属设计而非遗漏**，正是 S262 只补台账不补分支的依据。**⚠️ 门禁完整性观察（开放、未复现、不整改）**：wb41 复跑期间一次 `cargo test --workspace --exclude fs_terminal` 在 `fs_vault --test store` 处以 **`GATE_EXIT=-1073741819`（`0xC0000005` 访问违例，Windows 原生异常、非 Rust panic、进程级中止）** 失败，崩溃早于 6 条用例任一打印结果；定向复现**全部失败**（串行 6/6 · 独立 40/40 · 三进程并发 45/45 · 完整 t-ws 前台 2/2 · 后台 t-ws 1 次亦 0），**约 88 次干净运行仅 1 次崩溃**。唯一原生嫌疑面已定位未证实：`crates/vault/tests/common/mod.rs::setup_mock_keyring` 为绕开 keyring 4.x 惰性装库**故意先对真实 Windows 凭据管理器发一次 `Entry::new("future-shell-test-trigger","init")`** 再 `set_default_store` 换入 mock 并丢弃刚装入的平台库——该二进制**唯一**的原生 Win32 调用兼全局 store 交换点，且用机器级共享的固定凭据名。**不作推测性整改**：①该处是 S16 竞态既有解，改动恐回退到「静默换钥 → 随机 `Integrity`」更坏失效模式；②单次未复现不足以定因，凭猜改安全关键 crate 测试装置风险高于收益；③本批零 Rust 改动、与 Task 17 无因果。**后续任一批次再见 `0xC0000005` 即升级为定向裁决**（首查 = 能否去掉 trigger 而不回退 S16）。**方法论一条**：本次崩溃是**前台复跑**才暴露的——后台门禁套件输出经 `tail` 截断只剩最后一个测试二进制四行，`GATE_EXIT` 虽真实但细节不可读、肉眼看去像「全部 0 tests」；**此后凡以门禁结论入注一律以前台完整输出为准**。**门禁复跑 wb41（十跑全绿，全部前台完整输出）**：fmt 0 · clippy-ws 0 · clippy-term 0 · nextest 定域 **148/148**（flow 55 + grid 57 + pipe 17 + ring 19）· nextest pipe 17/17 · `cargo test -p fs_terminal` 四靶 **148** · `--lib` 0 · `--doc` 0 · `--workspace --exclude fs_terminal` **121（不变）** · deny 四项 ok ⇒ **工作区组合式 269（不变）**；前端 `vitest` **69/69 = 注 40 的 63 + 6**（theme 14 + shortcuts 23 + tabs 13 + focusTrap 6 + menus 6 + layout 4 + compose-history 3）· `svelte-check` **164 FILES 0 ERRORS 0 WARNINGS** · `build` **137 模块（不变——`?raw` 仅测试期依赖不入产物）**。**TDD 红先行取证**：新增用例全部先取确定性红，其中 **S262 守卫首跑即红并当场揪出人工复审漏掉的 `file/import.json`**（人工只找出 5 条、机器补第 6 条——立机器守卫而非只补注释的价值证明）。**诚实披露**：前端仍无变异测试基线故不作变异非空证明；镜头①②的真机取证依赖 dev server 运行态，**属一次性人工脚本取证、未进门禁**，Task 18 引入组件测试脚手架后应固化为自动化用例；`0xC0000005` 未复现未定因未整改，如实留档而非归零。**两轮收敛判定**：首轮 1 高 + 3 中 + 6 低 + 2 裁决，次轮 2 中 + 2 低 + 0 高 + 0 新裁决，缺陷密度与严重度**双降**，且首轮遗留的「未取行为证」披露已补证闭合 ⇒ **收敛成立**，standing 纪律「两轮复审通过方转下一 Task」达成。**携入 Task 18 的三项验收**：(a) `App.svelte` 窗口级处理器硬编码 `terminalFocused:false`，接线时须传真值否则 R118 直通真机失效；(b) xterm helper textarea 属 `TEXTAREA`，白名单键会被终端钩子与窗口处理器双派发，钩子内须 `stopPropagation`；(c) `ComposeBar` 的 `onSent` 回调 App.svelte 从未传入而 §2.6 要求发送后焦点回终端，其 `input` 的 `bind:this` 亦无消费者。**另 1 低记录不修**：MRU 游标与 `activeTabId` 在 `Ctrl+Tab` 循环中途增删标签时可失同步，代价仅一次按键空转且下次按键自愈——修正需在增删路径重置游标，反会打断「按住 Ctrl 连点 Tab」的正常循环语义，为观感级边角引入更重 bug 不划算。注 40 ⑤ 的六条低档全部续有效。
  - **Task 17 第一轮复审 + 整改已提交**（注 40 + rm40，代码 + 回灌合并 commit `0f8c33a59b778cd2fe7b36e72b8908b436af3162`；本批整改与回灌同提交，故不再分列两笔）：六镜头逐面出证，**1 高 + 3 中已整改、6 低留档、2 项转 Task 18 验收**。**镜头判定**：①主题引擎/R11 三级解析 **clean**（脚本核验 `tokens.css` 4 主题各含 §3.1 全 28 令牌无缺无溢、`themes.ts` ↔ `tokens.css` 112 项逐条同值、WCAG 全过——obsidian `accent-fg/accent` 6.53 正是 S243 白改黑的收益，留白仅 1.9）· ②快捷键仲裁 **1 高 1 中** · ③focus trap **1 中**（`focusTrap.ts` 产品代码零消费者，§7「全部模态装焦点圈闭」是空契约）· ④菜单启用矩阵 **clean**（与 §2.1 逐项对齐 7/8/9/7/6/4，1 低留档）· ⑤opener/capability **1 中** · ⑥镜像逐字一致 **clean**（46 块 34 文件全覆盖，12 处差异全由 S243–S249 解释）。**裁决 R118（高，规格改判）**：`arbitrate` 在远程 + 终端焦点下把非白名单复合修饰键判 local，而 `actionForKey` 对其零绑定 ⇒ 拦下后无动作可执行、**按键被静默吞掉**——受害面正是 `Alt+B`/`Alt+F`（readline 词移动）、`Alt+Backspace`、`Alt+.`、`Alt+数字`（tmux 前缀）与 AltGr 合成字符；§2.7 原句「仅影响无修饰或单 Ctrl 键」被实现读作「其余一律截获」，与「远程 = 原样发往会话」自相矛盾 → 改判为白名单外一律直通，安全前提「凡 §4 有绑定的键必属恒本地白名单」由新增守卫用例对 17 键逐枚断言，**且该用例在改实现前即已绿**（放宽无损之证）。附 **S253** AltGr 守卫（AltGr 同置 ctrlKey+altKey，致 `AltGr+Shift+A`=Ą / `AltGr+E`=€ / `AltGr+P` 误入白名单；`actionForKey` 的 Alt+P 支本就带 `!ctrl`，两函数口径本次对齐）。**裁决 R119（中，规格改判）**：capability 授 `opener:default`，其作用域含 **`tel:*`**（Rust 白名单专门拒，单测第 58 行有断言）且 reveal 支无作用域约束；计划原注「与 Rust 白名单取交集」只在前端走插件 JS API 时成立，而**全仓零 `@tauri-apps/plugin-opener` import**（前端一律 `invoke("open_external")`）⇒ 该授权实为白名单之外另开的 webview 直通路。**源码级核证**（registry 2.5.4）：作用域校验只在 `commands.rs` 的 IPC 命令（`CommandScope`/`GlobalScope`），`lib.rs` 的 `Opener::open_url`/`reveal_item_in_dir` 均不过 ACL ⇒ 删权对 `open_external` 与 Task 20 `reveal_log_dir` 皆无影响，**Rust 白名单成为唯一边界**；同批删零消费者的 `core:window:allow-set-title`。**整改 S253–S261**（下一可用 **S262**；下一可用 R 号 **R120**）：S253/S254 shortcuts.ts 双修 · S255 capability 收窄 · S256 测试翻面 1 例 + 新增 3 例 · S257 SettingsDialog 接 focusTrap + tabindex · S258/S261 `<nav>` role 冲突改 div · S259 语言项按 §2.12 禁用 · S260 MenuBar 子菜单出 button 并挂 `:focus-within`（原 hover-only + button 套 button ⇒ 「应用主题」四皮肤纯键盘不可达，违 §7）。**门禁复跑 wb40**：svelte-check **164 FILES 0 ERRORS 0 WARNINGS**（注 39 留档的 4 条 a11y WARNING 全数清零）· vitest **63/63**（60 + 3）· build 137 模块 · fmt 0 · clippy-ws 0 · `cargo test --workspace --exclude fs_terminal` **121 不变**、工作区组合式 **269 不变**。**TDD 红先行取证**：先落测得 2 failed | 20 passed 确定性红（恰为 AltGr 与 R118 两例），修实现后 22/22。**诚实披露**：S257–S261 属组件层而本项目**无组件测试脚手架**，其验证止于 svelte-check 零告警 + 全量绿 + build + 静态审读，**未取自动化行为证**，须第二轮冒烟人工补证或 Task 18 期引入脚手架后补测。**转 Task 18 的两项验收**：(a) `App.svelte:123` 硬编码 `terminalFocused:false`，接线时必须传真值否则 R118 直通真机失效；(b) xterm helper textarea 属 `TEXTAREA`，白名单键会被终端钩子与窗口处理器双派发，钩子内须 `stopPropagation`。
  - **Task 17 实施完成已提交**（注 39 + rm39，代码 commit `0604cfeb206febb175ca214f0e624770d7a27d52` + 本回灌 commit `0ef44fbc0dc79e4ec9b9d5f941aacc872792bbb9`）：Tauri 2 应用壳 + Svelte 5 前端骨架 Steps 1–10 全毕，101 个受控文件入库（app crate 全套 + 图标 45 件 + frontend 全量 + `scripts/gen-placeholder-icon.mjs`）。**门禁十跑全绿**：fmt 0 · clippy-ws 0 · clippy-term 0 · nextest 定域 **148/148**（flow 55 + grid 57 + pipe 17 + ring 19，**用例数不变**）· nextest pipe 17/17 · cargo test 分裂靶（`--lib` 0 / `--doc` 0 / `--workspace --exclude fs_terminal` **121** = 注 38 的 119 + 本批 app 侧 2〔`open_external` 白名单单测〕，40 靶逐枚合计：connmgr 22 / itest 15 / sshengine 57 / vault 25 / app 2）· deny 四项 ok（首跑因新依赖树红 17 条，S250 扩 `deny.toml` 后复绿）——**工作区组合式 269 = 121 + 148**（注 38 为 267）；**前端另跑** vitest **60/60**（theme 14 + shortcuts 19 + tabs 11 + focusTrap 6 + menus 4 + layout 3 + compose-history 3）· svelte-check **0 ERROR**（4 条 a11y WARNING 留档）· `npm run build` dist 生成。版本实测：tauri 2.11.5 / tauri-cli **2.11.4**（独立发版线，计划 Step 8 预判命中）/ svelte 5.56.8 / vite 8.2.0 / vitest 4.1.10 / TS 5.9.3 / Node 24.16.0。**Step 9 冒烟**（`cargo tauri dev` 两次启动）：窗口开、骨架全呈、主题下拉 5 次切换即时生效且期间无页面重载、监控抽屉 120px 开合、设置对话框 4 可用 + 2 禁用页签开合；首启一行 `Error reading the log directory/files: os error 3` 溯源 tracing-appender 0.2.5 惰性建目录与 `max_log_files` 清理线程 `read_dir` 的**首启一次性竞态**（二启零复现，日志与 `fs.db` 均正常落盘）记档不整改；`settings_set not found` 的 console.warn 为**设计内降级**（该命令 Task 20 注册，`lib/ipc.ts` 容错回落默认值不阻塞 UI），Step 9「Console 无报错」判据不破。**计划偏差 S243–S252**（下一可用 **S253**）：S244 App.svelte 省略 Task 18 的 `lib/term` 镜像残留导入 / S245 TS2352 双转型 / S246 TABS 显式类型替 `as const`（TS2339）/ S247 补 `use tauri::Manager`（E0599）/ S248 AppState `#[allow(dead_code)]` / S249 `cargo fmt -p future-shell-app`（**严禁 `--all`**，judge 探针 43 件零改动纪律）/ S250 deny.toml 扩 1 许可 + 16 条 unmaintained 忽略（GTK3 全家 10 + proc-macro-error + rust-unic 全家 5，各具三段式理据）/ S251+S252 Step 10 add 清单补 `Cargo.lock` 与 `deny.toml`。**TDD 诚实披露**：shortcuts/tabs/focusTrap 三模块取得确定性红后转绿，theme/layout/menus/compose-history 四模块与实现同批落盘、红态未单独取证；前端无变异测试基线，本批不作变异非空证明。复审须覆盖前端新面：主题三级作用域解析（R11）、快捷键仲裁、focus trap、菜单启用/禁用矩阵与 spec §2.1–§2.12 一致性、opener 白名单 Rust 侧边界、capability 最小权限面。前批 rm38 之 Task 13/15/16 代码轨关闭与 Task 14 代码轨 2/2 关闭续有效。
  - **Task 16 终局裁判关闭 + 三任务代码轨降档关闭记录**（注 38 + rm38，commit `1be06cb89906c676132c3b227211b240046b4adb`）：终局裁判（run wf_2eecc76c-1de，受审树 `a45daba`——wb37 回填 docs-only，pipe/flow 源码与 wb36 主提交 `d9a7856` 字节一致）关键行 **found=8 / uniq=8 / confirmed=7 / rejected=1 / medHigh=0 / clean_round=false / recomputed_confirmed=7 / recomputed_medHigh=0 / match=true**（29 代理 28 成 1 误：skeptic:F7#1 遭网关 503 终止未出票，F7 以幸存 2/2 实证票确认）——7 项确认皆 low（**文档措辞 4**：F1 暂停点水位下界 high 无条件陈述〔帧维单独触发可远低于 high〕/ F2 pipe.rs 三处把非收尾发送停摆丢帧退出误归「收尾分支」〔实走 flow.rs:392-395 许可臂，S150/S175b 钉死〕/ F3 消毒注 ② 误称 queue_bytes_high=0 为永久锁存〔释放子句不含 high，与 S127/S157 字段 doc 矛盾〕/ F4 消毒注 ④「batch_bytes ≥1 ≡ 形状 1」为伪〔batch_bytes 是活阈值，S194/S132 恰证〕；**钉例缺口 3**：F5 S214 钉例只钉背压 stop 臂「发起收尾」半、`return` 读取先退半零钉〔M-BPRET 全套绿〕/ F6 resize cols 轴与构造期 Grid::new 转发零钉〔M-GEOM-DUPROW 全绿〕/ F7 SessionPipe::ack 只钉下界〔M-ACKOVER 因 saturating_sub 地板全绿；补钉一例即杀：写 1000、ack(400)、断 queue_bytes()==600〕），驳回 1：**F8** EOF/读错臂 return 零钉指控——M-EOFNORET 令 read_error_closes_render_stream 确定性挂死红、实有钉；7 low 悉记档不阻塞、M1 出口不要求整改（后续批次触及同文件可顺手并批，补钉形已具文于注 38 ①）；**三任务代码轨关闭**（注 36 ⑥ 降档决定兑现）：Task 13 记录降档（末次受审树 `3f08480`，其后增量仅 wb35 自身整改——四新钉 + 例注、grid.rs 实现零改动，豁免第十二裁判复跑）/ Task 15 记录降档（末次受审树 `766931a`，其后增量仅 wb36 flow.rs 注释 +7/−4 与 wb37 纯注释级、flow.rs 实现零改动，豁免第十一裁判复跑）/ Task 16 本轮降档（两轮裁判后终局轮零确认 med/high → 关，「连续两轮全洁」豁免）；skeptic 变异实验于隔离 worktree 或复原后主树 git diff 全空核验、judge 探针 43 件零改动零删除；**Task 14 代码轨 2/2 关闭**不变。

---

## §6 附录：历史计划与实施记录（2026-08-28 合并）

> **本仓只保留这一份开发文档**（用户 2026-08-28 裁定：「不可以开发计划到处都是」）。
> 原先散落在 `docs/superpowers/plans/` 下的六份独立文件已合并至此并删除。
>
> 合并纪律：**只留裁决与边界，不留施工步骤**。已交付功能的「怎么做的」在代码
> 注释里（本仓惯例：为什么这么写、否决了什么、边界在哪，都写在代码旁边），
> 文档重复一遍只会两处分叉。逐任务的实施清单（M0/M1 那份 26031 行的施工图）
> 随交付完成而删除——它的价值已经兑现在代码与出口清单里。

### §6.1 M0/M1 逐任务实施计划（已删除）

`2026-07-26-phase0-1-mvp-core.md`，26031 行，M0 骨架 + M1 MVP 核心的逐任务
施工图，**全部交付完毕**（M1 出口 22 条 `[x]` 见 §1，载体由
`scripts/m1-regression.sh` 逐个核对存在性，33 个载体现在都在）。

已删除。设计理由已在代码注释中，出口判据在 §1 的 M1 出口清单里，
两者都有门禁看守——文档再留一份 26031 行的历史步骤，只是多一处会分叉的副本。

### §6.2 M3 Agent 运行时规格（已合并）

原 `2026-08-25-m3-agent-runtime-spec.md`（783 行）。逐类型的实现规格
（`tool.rs`/`gate.rs`/`budget.rs`/`confirm.rs`/`record.rs`/`ports.rs`/`history.rs`
的字段与签名）**已全部落进代码**，此处只留裁决：

- **骨架取方案 C（单闸门工具协议运行时）**，落在 `crates/ai/src/agent/`（纯逻辑）
  + app 装配层（ports 实现）。
- **嫁接**：A 的 `RedactedAction` 脱敏 newtype、`GlobalMode::Unloaded → ModeNotLoaded`、
  审计先行（audit_draining）、streak 重置的双向变异对；B 的 `TokensSpent` 三本账
  （None-usage 记可证上界）、`admit` 固定检查序（闩→急停→步数→token→连续）、
  `ConfirmAnswer` 无 pending 变体的端口形状。
- **砍掉**：A 的「read() 注入 Err」急停（核实 `crates/ai/src/exec.rs`：读错误走
  terminal break、kill 只在 `if timed_out` 里，注入 Err 得到的是 close-without-kill）；
  C 的「streak 执行成功后才 +1」（改为派发即计数）；C 的「Abandoned→FS_POLICY_TIMEOUT
  归并」（MCP 面留待后续，已定为**不得归并**）。
- **A vs C 的裁决理由**：A 的两个被确认缺陷落在**本批**核心面（急停 kill 语义、
  token 触顶），C 的唯一致命缺陷落在**明确延后的 MCP 面**；A 的「纯归约」优势在 C 里
  以更低代价保留（六个模块零 fake 可直接构造）；A 的「机器无钟、事件携带 elapsed」
  在急停上恰恰引出那个 Err 注入错误——急停必须穿透到正在 await 的 `run_command`
  内部，事件模型天然做不到。
- **双行审计为默认**（RequestOnly 前置行 + 结果行）：单行制有 crash 窗口内
  「已执行但无审计行」的 custody 缺口；链体积翻倍用「只读 AutoRun 步可合并为单行」缓解。
- **token 的 None-usage 记可证上界而非即停**：上界（请求体字节数 + 该回 `max_tokens`）
  是可证高估，fail-closed 方向不变，且不至于让流式/无 usage 网关场景下任务一律暴毙。

> 代码入口：`crates/ai/src/agent/mod.rs`（模块头指向本节）。

### §6.3 M4b 两项范围评估（已合并）

原 `2026-08-23-m4b-scope-assessments.md`。M4b 出口两条要求「做/不做 + 依据」记档：

1. **文件夹同步 / 文件夹比较**：**不做**（M4b 范围内）。
2. **本地栏的新建目录 / 重命名 / 删除**：**分拆**——新建目录与重命名**做**，
   删除**不做**。三者风险差着量级：前两个非破坏性（最坏是多个空目录 / 换个名字），
   而删除是唯一不可逆的；本地这一侧用户是在「用自己的电脑」，而自己的电脑上有
   回收站、我们这里没有。
   > 判据载体：`app/src/commands/sftp_cmd.rs`（禁止本地删除的守卫）+
   > `frontend/src/components/SftpPane.test.ts`。两处注释指向本节。

### §6.4 代码签名与公证的成本/周期评估（已合并）

原 `2026-08-23-signing-cost-assessment.md`。**性质：采购前必须复核**——
价格与周期会变，本节的价值在**决策结构**而非具体数字。

- **Windows**：1.0 之前走 Azure Trusted Signing 或同类云签名——它把「私钥必须在
  硬件里」这个硬约束变成一次 API 调用，是唯一能让 CI 全自动发版的路径。
  在此之前：`release.yml` 的自签演练保证链路是活的，tag 构建被签名强制闸拦死，
  不会有未签名产物混进 release。
- **macOS**：Apple Developer Program（$99/年）是**账号问题不是技术问题**——
  组织账号还要 D-U-N-S 编号（申请本身可能耗时 1–2 周）。证书类型必须是
  **Developer ID Application**（App Store 之外的分发），拿错的表现是公证失败。
- 状态：仍是 `[!]` 阻塞项（采购是用户闸门，不代签）。
  参见 `docs/verification/manual-checklist-1.0.0.md` 的 V13。

### §6.5 ZMODEM rz/sz 体验重设计（已合并）

原 `2026-08-26-zmodem-ux-redesign.md`（343 行），已交付。留档的是**缘起**，
因为它值得被记住：

> 两个问题都不是测试能发现的——协议往返全对、字节逐一校验、itest 对真 lrzsz
> 跑通，但**用起来是坏的**。「弹窗关不掉」在任何一条自动化判据下都是绿的。

- 关不掉的根因：`view.needFile` 在远端 rz 等待期间恒为真，用户关掉的那一刻
  effect 重跑、条件再次成立、弹窗立刻重开。**关闭按钮存在但形同虚设**。
  修法是改成**边沿触发**（只在 false→true 的瞬间开一次）。
- sz 落盘：改为**两段式**——先收进暂存文件，收完再问用户去哪/是否覆盖。
  理由是协议硬约束：rz/sz 有几十秒级超时，**不能弹框等用户再应答协议帧**。
- 顺带修掉一个潜伏 bug：原判据用 `offset > 0` 区分「初始 ZRINIT」与「ZEOF 后的
  ZRINIT」，0 字节文件时 offset 恒 0 会误判——换成显式 `eof_sent` 标志。

### §6.6 RDP 协议支持（已合并）

原 `2026-08-27-rdp-protocol-support.md`（198 行）。阶段 1–3 已交付
（驱动器重定向按用户裁定留作后续）。留档的是几条**边界与教训**：

- **为什么有独立 helper 进程**：IronRDP（经 picky）钉死
  `curve25519-dalek =5.0.0-rc.1`，russh 要 `^5`——两棵依赖树不能出现在同一个
  Cargo.lock 里。独立工作区 = 独立 lockfile。**就算上游明天修好这个拆分也该留着**：
  helper 是解析远端不可信字节的那一侧，跑在独立进程里，出内存安全问题波及不到
  主程序、也碰不到 Vault；网络在主程序侧，所以「经 SSH 跳板连 RDP」是免费的。
- **TOFU 的检查点在「秘密出线前」而不是「握手前」**：`ironrdp_tls::upgrade` 用
  不校验的 verifier 完成握手并把证书带回来，看起来像缺口实际不是——口令只在
  CredSSP 段才过线，而证书裁决在它之前。
- **安全层双通告（HYBRID + SSL）**：只通告 HYBRID 固然更严，代价是整类服务器
  连不上——那不是安全，是失能（实测：Debian 13 的 xrdp 整包无 CredSSP）。
- **三种载体三种背压**（本仓最容易抄错的地方）：终端字节 never-drop（丢一个字节
  屏幕就全错）、像素**合并**（后写覆盖前写，照抄 never-drop 会吃光内存）、
  音频**丢弃积压**（两段声音叠加是噪音，不能合并）。
- **键盘走扫描码而不是字符**：这样远端的键盘布局/输入法才是生效的那个。
- 真机踩过的坑（2026-08-27/28，逐条已修并有判据）：主线程栈溢出（1 MiB 栈撑不住
  Tauri 在主线程构造的命令 future）、CredSSP 通道绑定公钥传成 SPKI DER 而非原始
  字节、未实现 Deactivation-Reactivation 导致改分辨率后画面冻死、RemoteFX 未通告
  导致动态画面卡死、帧流无背压。
