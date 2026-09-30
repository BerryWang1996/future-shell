# M1 验收条目与回归载体

> 本文保留 M1 验收条目及测试载体，供回归脚本检查。当前分类数量由
> `bash scripts/acceptance-triage.sh` 重新计算；1.0.0 实际验证见 [发版检查记录](release-readiness-1.0.0.md)。

未落勾项由脚本分为“有测试载体”“待人工验证”“尚未注明载体”三类。
分类只检查文档结构，不证明测试已经通过；具体运行证据与真机结论分别记入发版报告和人工清单。
脚本保留条目总数下限及人工分类上限，避免删空清单后检查仍然通过。

## 功能
- [ ] 对 OpenSSH 容器：密码/公钥/kbd-interactive/agent 四路认证、错误密码回退、ProxyJump 一跳、TOFU 首连/变更/严格/仅本次（AcceptOnce 内存态）四态，itest 全绿（载体 = crates/itest/tests/ssh_tofu.rs 五例（TOFU 四态 + 导入首连不弹一例，F41）+ ssh_connect.rs 基线四例，与路线图 §1 逐字对偶）
- [ ] keyboard-interactive 多 prompt（OTP 模拟）弹框往返（前端多提示往返载体 = `AuthPromptDialog.test.ts`「kbd-interactive 多提示」一节四条：两枚/三枚提示逐格归位且顺序不乱、echo 决定框型、只填第二格不串格，变异验证过。**残留**：后端 `kbd_interactive_loop` 对真服务器多轮（含 KBD_MAX_ROUNDS 封顶）的驱动仍无容器 itest，OTP 模拟亦无）
- [ ] **keyboard-interactive 单提示自动应答（R53 开关，审计2 #36 已接通）**：`AuthRef.kbd_auto_answer_single` 原于 M1 无消费方（彼时相位注口径），审计2 #36 起已入认证循环——`connect.rs` 在弹框前先过 `auth::kbd_auto_answer`：仅当「开关开 + 恰好一个提示 + 非回显 + 有口令」四者齐备才自动应答，其余一律照旧弹框（fail-safe）。触发时经 status 通道留痕「单提示自动应答（提示内容不回显）」。载体 = `connect.rs` 单测 `auto_answer_fires_on_single_hidden_prompt` / `auto_answer_off_by_flag` / `auto_answer_refuses_multiple_prompts` / `auto_answer_refuses_echo_prompts` / `auto_answer_needs_a_password` / `auto_answer_refuses_empty_prompt_set` + 接线守卫 `kbd_auto_answer_single_is_wired_into_auth_loop`。（本条 2026-08-25 改写：旧文「无消费方、不作验收」的前提已随审计2 #36 失效，三处旧相位注已更新/移除，故验收动作从「核对相位注」改为「核对行为与接线」）
- [ ] Windows：Pageant 可用；OpenSSH ssh-agent 可用（服务未启动时报错明确）（附录 A.2 spike 记录附后）（「服务未启动时报错明确」载体 = `crates/sshengine/tests/agent_logic.rs::unavailable_message_names_all_three_transports` + `agent.rs::a_deaf_agent_pipe_is_given_up_on_rather_than_waited_on_forever` + `crates/itest/tests/ssh_agent.rs` Err 分支：文案点名三条传输且归为可回退的本地故障。**残留**：Pageant / ssh-agent **成功路径**需真装 PuTTY、真起服务并 ssh-add，归人工）
- [ ] macOS/Linux：SSH_AUTH_SOCK agent 可用（Linux 成功路径载体 = `crates/itest/tests/ssh_agent.rs::a_live_agent_on_ssh_auth_sock_yields_its_keys`：测试内自起一个说 SSH agent 协议的 Unix socket，把 SSH_AUTH_SOCK 指过去，逼出「连接成功 ⇒ 真能问出钥匙」，变异验证过非空转；仅编译于 unix。**残留**：macOS 一路仍需真机；本条只证连接+枚举，不证真签名登录）
- [ ] 主机密钥：首次 TOFU 弹指纹确认（指纹对二进制 blob 求，与 `ssh-keygen -lf` 逐字一致）；密钥变更红色告警默认拒绝；strict 模式拒绝未知；AcceptAndRecord 经 check_server_key 入口真实落库（回归测试）；AcceptOnce「仅本次」接受放行但不写 host_keys（lookup 为空）、断开后新建连接对该主机重走 TOFU 再次询问（单测 connect_logic::accept_once_accepts_without_recording + itest ssh_tofu::tofu_accept_once_stays_in_memory_and_reprompts 双覆盖）
- [ ] host_keys 并发首触后单行不变式（R30）：同 host:port 两次 record 异 key_blob（多标签同连同一新主机，或 MITM 抢答与真机竞态）后信任库恰一行且为后记录新键，旧键再出示必判 Changed 硬失败（record 与 replace 同构单事务：DELETE 同 host:port 旧行 + INSERT；单测 hostkey::record_replaces_same_host_port_keeps_single_row + itest sftp::hostkey_record_keeps_single_row_per_host_port 双覆盖）
- [ ] 主机密钥默认策略设置消费（R31）：设置 `hostkey.defaultPolicy=strict`（SettingsDialog 或 `settings_set`）后新建档案默认策略为 strict（ProfileDialog 新建回填经 settingGet 消费；编辑既有档案不受影响、仍用档案自身值）（载体 = 写入侧 `commands/settings_cmd.rs` 白名单单测 + 消费侧 `ProfileDialog.test.ts`「R31 默认策略消费」一节：新建吃全局默认 / 无默认回落组件写死的 tofu / 编辑既有档案不被默认覆盖，三格各带反向变异验证——写死 tofu 与「默认覆盖档案自身值」两个变异均转红）
- [ ] known_hosts 导入（M1 出口项）：明文行解析入库（source=imported；逗号 host 列表逐一落库；`[host]:port` 括号语法；hashed hostname `|1|…` 整体跳过计数；同 host+port 异 key 冲突不覆盖、计 conflicted；重复导入幂等）；Task 20 导入按钮与汇总（imported/skipped/conflicted）；Task 9 容器 itest 对真实服务器宿主密钥验证指纹自洽；对已导入主机首连直接校验不弹 TOFU（TOFU 回调零调用，载体 = ssh_tofu.rs `imported_host_first_connect_verifies_without_tofu_prompt`）（FS_ITEST 门控）
- [ ] 真实交互：htop/vim 可用；多标签（10 个同开无明显卡顿）；隐藏标签切回保留 scrollback
- [ ] 断线自动重连（指数退避）+ banner + 立即重连；PTY 尺寸恢复、屏幕不恢复且有提示（退避载体 = `session_cmd.rs::reconnect_delay_doubles_then_caps_at_thirty_seconds` / `reconnect_delay_never_exceeds_the_cap_from_any_start`；banner 三态 + 立即重连/停止重连载体 = `TerminalPane.test.ts`「断线重连 banner」一节八条：attempt=0/≥1/gave_up 三态、退出码呈现、两个重连按钮各发对的 IPC、异会话不串弹，变异验证过。**残留**：「PTY 尺寸恢复」与「屏幕不恢复且有提示」两小句仍无载体）
- [ ] 状态栏 [重连] 与 banner [立即重连] 同门控（断线态状态栏 [重连] 点击触发重连、循环在跑时为 no-op；手工核验留痕）
- [ ] 连接态三处一致（UI 规格 §5）：双击连接 → 标签蓝环+侧栏蓝闪+状态栏「连接中…」同现；成功 → 三处转绿；拔网 → 三处转状态灯灰空心（离线）/灰（断开）/「已断开」+banner；密钥错误 → 三处红✕/红灯/错误摘要（E2E 或组件测试覆盖）
- [ ] SFTP 双栏：list/read/write/mkdir/rename/remove + 断点续传（中断网络后续传成功）+ 并发 + 进度事件；下载沙箱强制生效；core 层 SftpOps 另提供 stat_size（SSH_FXP_STAT·跟随软链取大小，对偶路线图出口『stat』）/lstat（SSH_FXP_LSTAT·不跟随）/read_link/symlink（Task 12 容器 itest 覆盖；面板软链 UI 归 M4a）
- [ ] 传后校验（路线图 M1 出口逐项对偶）：设置 `transfer.verifyAfterTransfer` 默认开（Task 21 `settings_bool` 回退 true）；Up = 传前本地哈希 vs 传后 exec 远端 `sha256sum` 比对；Down = 传前远端哈希 vs 传后本地哈希比对；远端无该命令（exit 126/127 或 not found 文案）→ 降级 size 比对并在 UI 显式标注「仅大小核对」；双路径失败 → 标注「未核对」；**禁静默跳过**——四态（sha256_match/size_only_match/mismatch/unverified）经 `transfer_verified { id, outcome }` 事件全量上报 TransferQueueDrawer 徽标（Task 21 Step 4）；执行体本体 = `fs_sshengine::verify`（`ExecChannel`/`run_verify`/`file_sha256`/`remote_sha256_via`/`parse_sha256sum`/`exec_unavailable`/`shell_quote`），Task 12 Step 2c/2d 九个单测 + sftp.rs 容器 itest `transfer_verify_roundtrip_against_real_sshd` 四态（FS_ITEST 门控）。**开关的可达性**（2026-08-11 补）：该键是持久的、后端在 `transfer_submit` **当刻**读它，而 §附表把控件载体钉在 TransferQueueDrawer——那个抽屉整体包在 `{#if $rows.size > 0}` 内，本会话有第一件传输之前不渲染，于是上次关掉它的用户在传第一个文件之前无处可开（等抽屉出现，作业已按「关」建好，再勾也不影响在飞的那件），而那恰是最该被校验的一件。现 SettingsDialog「安全与 Vault」页补第二处控件（`set-verify-after-transfer-global`，随时可达），抽屉侧订阅 `settingChanged` 跟随，两处不会各显各的值。载体：`settings-wiring.test.ts` 的「设置控件可达性」一节（钉死 Rust 侧动作当刻消费的四键 `security.clipboardClear`/`sftp.sandboxRoot`/`transfer.verifyAfterTransfer`/`vault.autoLockMinutes` 必须在选项页里都有控件——后端将来多读一个持久键会当场转红，逼着回答「用户改得到吗」）+ `SettingsDialog.test.ts` 的「关 → 重启 → 就地开回」整圈行为；三处默认值（控件初值 / 前端 fallback / 后端 `settings_bool(…, true)`）逐字一致亦有钉。V26 变异 14/14 全灭，目击者文件集逐条钉死
- [ ] Vault：OS keyring 路径 + 应用密码便携路径均可解锁（UI 层手动核验留痕：删除/屏蔽 keyring 条目后启动 → VaultDialog setup/unlock 以应用密码解锁成功）；篡改 → Integrity 错误；锁定后 secret 不可读
- [ ] Vault 自动锁定生效（审计 P1-16）：机制**在后端不在前端**——`spawn_auto_lock_watcher`（`app/src/commands/vault_cmd.rs`，随 setup 启动一次、随进程存活；`AUTO_LOCK_POLL` = 20s 巡检 + `LAST_ACTIVITY` 最后交互时间戳）闲置达 `vault.autoLockMinutes` → 置空 Store 并 emit `vault:locked{reason,idleMinutes}`；前端 `App.svelte::bridgeVaultEvents` 翻锁标 + Toast + 收掉 AuthPromptDialog/HostKeyDialog（人已离机，屏上不能留一个等着被随手点「接受」的主机密钥确认框）。设 '1' 后 1 分钟无交互出 Toast、错误抖动提示同路线图 L78 口径，手工核验留痕。阈值与设置解析另有自动化载体（`commands::vault_cmd` 单测 `auto_lock_*` / `should_auto_lock_*`）：前端 `settingSet` 走 `JSON.stringify` 且该项绑 `<select>`，库里落的是 `"5"` 这种**带引号的字符串**而非裸数字——只认 `Value::Number` 会让界面上选的每个时长都读成「未配置」、功能整体失效且无任何报错；0/缺省/解析失败一律收敛到「不锁」（替用户擅自选一个时长会在他没要求过的时刻截断正在进行的认证）；`minutes * 60` 溢出经 `saturating_mul` 收口（debug 下 panic 发生在 spawn 出去的任务里、主流程一无所知，release 下回绕成小阈值每轮必锁，两种失败都无声）
- [ ] 剪贴板定时清除（默认 30s、时长可配、整项可关，设置键 `security.clipboardClear`）：**M1 只验后端判定**，载体 = `commands::vault_cmd` 单测 `clipboard_clear_*`（前端真实写入形态、显式关闭、破损输入一律落到「开启」而非「关闭」、缺省 30s、`seconds: 0` 不当作「即写即清」）。端到端 UI 核验**曾顺延至 M4b**，现已解除顺延（2026-08-12，审计2 #27）：`vault_copy_to_clipboard` 当时在 M1 无任何 UI 调用点，`ipc-contract.test.ts` 的 `UNREACHED_BY_INVOKE` 就地登记了这件事，于是「验收要人工核验、入口却在下一个里程碑」成了一条自相矛盾的条目。审计2 #27 要求补齐 Vault 的删除/轮换/备份/恢复，这些动作本来就需要一个凭据列表来承载（`VaultManagerDialog.svelte`，菜单「工具 → 保险库管理…」），复制按钮挂在同一行上是零额外面积，顺带把这条悬空的验收项变成可执行：解锁 → 保险库管理 → 某条记录「复制」→ 按设置的秒数后剪贴板自动清空（手工核验留痕）。白名单条目已随之删除。M4b 仍要做的是完整的密钥/代理管理器（生成、导入、agent 交互），不是这一处列表。**未覆盖残留**：解析结果到 `vault_copy_to_clipboard` 内 `if !cfg.enabled { return }` 的接线本身仍无自动化测试（该命令需 Tauri `State` + 真库 + 真剪贴板，只有解析器是纯函数可测），故本条仍需一次人工核验留痕才算关闭
- [ ] 凭据 UI 端到端（手工核验留痕）：ProfileDialog 认证页签「存入 Vault…」新建 password 凭据 → 下拉自动回填 → 保存 Profile → 密码认证连接 OpenSSH 容器成功；锁定态报错明确；四路认证 itest 不替代此项
- [ ] 迁移：user_version 版本闸拒绝高版本库；迁移前自动备份（载体 = `crates/connmgr/tests/db.rs::refuses_newer_schema` 版本闸、`pre_migration_backup_is_pruned_to_five` 迁移前备份、`backup_failure_aborts_migration` 备份失败即中止迁移、`reopen_is_idempotent`）
- [ ] 重启恢复：强杀后启动列出未关闭会话，仅询问不自动重连（后端记账载体 = `app/src/journal.rs` 内联单测八条：开⇒未关闭 / 关⇒移出 / 关而复开⇒重算未关闭 / 同 key 幂等 / 丢弃只清未关闭 / 空库不报错 / profile Some&None 往返 / 关未开过的 key 为 no-op，变异验证过；前端询问载体 = `lib/session-restore.test.ts`。**残留**：「强杀后启动」的进程级 E2E 仍无，后端记账与前端询问两半已各有载体）
- [ ] 关闭确认：关闭含活动会话或排队/传输的标签/窗口弹模态（列会话数+传输数），确认后断开并取消传输；全空闲直接关闭；覆盖四入口（标签 × / 中键关闭 / Ctrl+W / 窗口关闭；右键菜单「关闭标签」与标签×同为 requestCloseTab 的 tab scope 控件面，不另计一路）——中键自动化（webdriver）困难时以组件/手工核验留痕（与路线图 M1 出口逐字对偶）
- [ ] audit 表迁移落地：字段与总设计 §6.2 对齐，append-only（触发器禁 UPDATE/DELETE）+ hash 链列就位（M1 只建表与约束）（载体 = `crates/connmgr/tests/db.rs::audit_table_is_append_only`（拒 UPDATE/DELETE + REPLACE 后门）+ `crates/connmgr/tests/audit_repo.rs::appended_rows_form_an_intact_chain` / `the_append_only_triggers_actually_fire` / `tampering_that_bypasses_sql_is_still_detected`）
- [ ] 输出提醒：会话收到 BEL/`\a` 后标签出现橙色铃铛角标，切回该标签或手动清除后消失（前端 onBell → tabs store，E2E 或单测覆盖）
- [ ] 终端内搜索：Ctrl+F 弹浮层、Enter/Shift+Enter 下/上一个、Esc 关闭还焦；菜单/工具栏『查找』经 onAction 达同一浮层（组件测试或 E2E 覆盖）
- [ ] 组合命令栏当前会话发送（§2.6）：Enter 发送附后缀（CR/LF/CRLF 选择器生效）达活动会话并回显执行、Ctrl+Enter 不附后缀、↑/↓ 走历史、无活动会话出 Toast 且不发 IPC（手工核验留痕，留痕方式同本清单中键条款）

## UI
- [ ] 前端骨架：`cd frontend && npm ci && npm run build` 通过；CI frontend job 存在性门控解除后真实执行通过（本机构建载体 = `scripts/ci-local.sh` 的 `vite build` 步：注入一个不存在的 import 已验证能转红，且它是 app crate 的编译期前置，`generate_context!` 在 dist 缺失时直接 panic。**残留**：CI 上由 GitHub runner 真实执行仍需推送后才有，见路线图仅本地决定）
- [ ] 与 `docs/mockups/` 基线逐屏一致（会话管理器左停靠、组合栏底部、标签 MDI、状态栏分段）
- [ ] 键盘模式三平台可达（UI 规格 §2.7）：状态栏键盘模式段可点击切换且文字即时刷新；设置默认值对新建会话生效；Scroll Lock（Windows）与点击（三平台）行为等价；macOS 无 Scroll Lock 键盘下切换路径可达（组件测试 + 手工核验留痕）
- [ ] 终端焦点下 §4 固定快捷键集逐项手工核验（`lib/shortcuts.ts` 统一派发器下；MVP 固定集全部触发应用动作；Ctrl+Shift+A 于 MVP 禁用——本地截获、不触发动作，随 M2 AI 基础能力启用，与路线图 §1 M1 出口逐字对偶）；远程模式下 Ctrl+S/Ctrl+Q 原样达会话；Scroll Lock 切换键盘模式且状态栏段同步；白名单键终端焦点下从不原样透传至会话（应用动作可另行经 term_input 下发等效字节；由 Task 18 term.ts 键盘仲裁块就地保证，R58；webdriver 困难时以组件/手工核验留痕，同 Task 22 Step 4 (c) 中键口径：`onauxclick` 且 `e.button === 1`）
- [ ] 终端交互（§2.8/§4）：右键=粘贴默认生效、非空选区右键弹 §2.8 上下文菜单（可切恒弹菜单模式）；选择即复制开关即时生效；Ctrl+C 有选区复制并清除选区/无选区发 \x03，Ctrl+Shift+C/V 恒语义；`Ctrl+V` 粘贴开关（§2.12，默认开）开时终端焦点下 Ctrl+V 即粘贴（优先于远程原样发送、不发 \x16），关时该键原样发 \x16 且粘贴仅剩 Ctrl+Shift+V；多行粘贴确认气泡+记住本会话；四键（右键行为/选择即复制/多行粘贴确认/Ctrl+V 粘贴）改动无需重建终端（组件测试 term.test.ts + 手工核验留痕）
- [ ] 字体缩放（UI 规格 §3.3/§4）：终端焦点下 Ctrl+=/Ctrl+- per-session 缩放、Ctrl+0 复位基线、Ctrl+滚轮等价；菜单 view.fontGrow/view.fontShrink 为可用态（非「即将推出」禁用）；全局默认字号仅作新会话默认值（手工核验留痕 + 组件测试）
- [ ] 编辑菜单五项（复制/粘贴/复制为纯文本/全选/清除滚动缓冲）与工具栏复制/粘贴经 onAction 达活动终端选择面可用（手工核验留痕）
- [ ] 4 应用主题切换即时生效无刷新；终端配色三级作用域（全局/Profile/标签右键临时）生效（Profile 终端页签设置字号/配色 → 保存 → 新开会话命中覆盖、未设字段跟随全局，手工核验留痕），12 内置方案（≥10 下限）
- [ ] 标签右键临时配色（三级作用域第三级，R16）：标签右键「配色方案 ▾」子菜单列全 12 套 +「跟随默认」；选中 → 当前会话即时换色（不重建终端）、其他会话不受影响、关闭标签即释放（transientSchemes 键随 removeTab 移除，回落全局/Profile 合成值）（载体 = store 层 `lib/tabs.test.ts::setTransientScheme`（设置/清除/随标签释放/转正随迁）+ 菜单面 `TabBar.test.ts`「右键临时配色菜单」一节：子菜单列全清单每一套、点一套落进 store、跟随默认清除、勾选标记随覆盖，变异验证过；方案齐全另有 `term-schemes/schemes.test.ts`）
- [x] 对比度 ≥4.5:1 自动核验通过（载体：theme.test.ts 对比度核验组 + npm run verify:contrast；High Contrast ≥7:1）
  - 2026-08-11 核验：`npm run verify:contrast` 四套主题全过，High Contrast ≥7:1；theme.test.ts 绿。载体是自动的，无人工留痕成分。

## 非功能
- [ ] §0.1 性能目标（逐字对齐全局不变量 4）：100 并发会话（50 渲染 + 50 后台化）后端进程（core 侧）常驻内存增量 ≤100×5MB、每空闲会话 ≤5MB（渲染侧 WebView 内存单列观测值不设硬闸）；loopback 连接到可交互 <2s（锚点在认证之后，不含认证往返）；冷启动 ≤3s；SFTP ≥20MB/s（五个指标里四个有真载体且非 `#[ignore]`：内存两句 = `render_pipeline.rs::a_hundred_idle_sessions_stay_within_the_core_side_memory_budget`、loopback <2s = `loopback_reaches_interactive_within_two_seconds_after_auth`、SFTP 吞吐 = `scale.rs::large_file_roundtrip_integrity_and_throughput`，FS_ITEST=1 真容器实测通过。**残留**：冷启动 ≤3s 须真拉起 Tauri 窗口，仍在 `perf.rs` 的本机自查线 `#[ignore]`，不代签）
- [ ] §2.2/§8.5.2 高吞吐底线：200MB 输出零丢失且渲染队列水位 ≤ high-water×1.5（经 `queue_bytes()` 观测）（载体 = `crates/itest/tests/render_pipeline.rs::two_hundred_megabytes_arrive_intact_and_the_queue_stays_under_the_watermark` + `a_slow_consumer_cannot_push_the_render_queue_past_the_watermark`；FS_ITEST=1 下真容器实测通过）
- [ ] 三平台安装包可构建（release.yml 草稿流程跑通，不发布）；`cargo clippy -D warnings`、`cargo fmt --check` 通过；CI 三平台绿；`cargo deny check licenses bans advisories` 通过（advisories = 总设计 §6.2 advisory 漏洞库检查）
- [x] 无遥测代码路径（~~grep 审查~~ → 载体：frontend/src/lib/egress-contract.test.ts，19 例；变异校验 V25 26/26 + 2026-08-23 新增 4 条）
      **2026-08-23 口径收窄**：本条原文是「无外发 HTTP」，而 Phase 2 的 AI provider
      （云 API 与本地 Ollama 都算）必然走 HTTP，两者不能同时成立。故拆成两条判据：
      ① **遥测/上报型依赖在任何 crate 里都不允许**（这一条无例外，就是「无遥测」的字面含义）；
      ② **HTTP 客户端只允许出现在 `crates/ai`，且只允许 `reqwest`**（逐 crate 逐 crate 名豁免）。
      出货清单也由写死改为从 `app/Cargo.toml` 的 path 依赖推导——写死的那份漏掉了
      M2 才加进来的 `fs_policy`/`fs_ai`，于是这条门禁在新代码上曾悄悄停止生效。
      承诺措辞的对应订正见 README「出站面」节。
  - **原方法已被证伪，故换载体。** `app/src` 与各 crate 的 src 里 grep `reqwest|fetch|hyper` 确实一无所获，
    但 Cargo.lock 里躺着 reqwest / hyper / ureq / tonic 四套 HTTP 栈——grep 看不见依赖图，
    "grep 干净"与"不外发"是两件事。
  - **2026-08-11 逐目标解图（点时验证，非门禁）**：`cargo tree -p future-shell-app -e normal` 共 1221 行，
    `reqwest|ureq|hyper|tonic|bollard|testcontainers|fs_itest` **零命中**。四者的来路：
    reqwest ← tauri 2.11.5 的 `[target.'cfg(android/ios)'.dependencies]`（桌面三平台不入图，
    需 `cargo tree --target all` 才看得见）；hyper/ureq/tonic ← bollard ← testcontainers ← `fs_itest`（测试专用工作区成员）。
  - **门禁覆盖（每次 CI 复算）**：出货清单五份的任何依赖表（含 target 门控表、build-dependencies、
    `[dependencies.foo]` 子表）+ 根 `[workspace.dependencies]` + `package = ` 改名别名，均不得出现出站型 crate；
    出货清单不得按名或按 path 依赖 `fs_itest`；前端生产源码（40 个文件，含 components/ 与 App.svelte）
    零 `fetch(`/`XMLHttpRequest`/`WebSocket`/`EventSource`/`sendBeacon`；前端依赖无网络型 Tauri 插件；
    CSP 的 default-src/connect-src/script-src/form-action/base-uri 逐条钉住；`withGlobalTauri` 未开；
    能力清单不授 `opener:`/`http:`/`shell:`，且 `open_external` 中白名单校验在开链之前。
  - **本次顺带修掉的一处真缺口**：CSP 原先没有 `form-action` 与 `base-uri`，而这两条**不回落到 default-src**——
    `default-src 'self'` 拦不住 `<form action="https://…" method=post>`。本仓唯一的 `<form>` 正是
    AuthPromptDialog 里收口令/私钥密码的那个。已补 `form-action 'none'; base-uri 'none'`。
  - **门禁看不见的**：传递引入（某个新增 crate 自己带 reqwest）——本文件只查直接声明。
    那一格仍归 `cargo tree` 与人工评审，上面的 1221 行结果是**点时**结论，不随提交自动复算。
- [ ] 生产运行期无外发的**运行时**证据（当前无载体：上述全部为静态判定，没有任何测试观测过真实进程的 socket 行为）

## Spike 记录
- 附录 A.2：Pageant 管道路径验证结论：____
- 附录 A.2：\\.\pipe\openssh-ssh-agent 验证结论：____
- 附录 A.1：Tauri IPC 二进制载荷编码选择（base64 vs 分片）及吞吐数据：____
