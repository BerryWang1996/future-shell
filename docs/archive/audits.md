# 历史审计原文汇编

> 以下材料只反映历史代码；原文中的缺陷判定、源码行号及工作树状态不代表当前版本。
> 当前结论见 [1.0.0 发版检查](../verification/release-readiness-1.0.0.md)。

## 2026-08-11 对抗裁决


本文件是**交叉对抗审计**的原始裁决产物，不是整改报告。每条缺陷由独立的提出方给出
`CLAIM` 与 `SCENARIO`，再由独立的裁判方逐行亲验后写下 `JUDGE`（试图推翻，失败才算成立）
与 `FIX`（对指控本身的修正——多条指控的危害描述被裁判方判定为「成立，但比描述的更宽/更窄」，
这些修正与结论同样重要）。`UPHELD=17` 是推翻失败、判定成立的条目数。

保留它的理由：整改代码只留下「修成了什么样」，而这里留下的是「为什么这是缺陷、
攻击者怎么走到那一步、哪些看似合理的反驳被验伪」。回归时判断一处改动是否重新打开旧洞，
读这份比读 diff 快得多。

各条目对应的整改状态见 git 历史与各文件内的整改注释；本文件**不随整改更新**，
它是那一时刻的裁决快照。

---

UPHELD=17

### [P1] upload-target-lock-ns-scoped
WHERE: crates/sshengine/src/transfer.rs:387
CLAIM: P0-3 的「同一远端目标串行写」在上传方向只在单条连接内成立：锁键含 `target_ns` = `{session_id}#{generation}`，而 session_id 是每次 session_open 新生成的 UUID，因此同一台主机的两条连接写同一个远端路径时互不排斥，两个作业会并发写同一个 `<remote>.fspart` 并先后 rename，产出交错/截断的远端文件。
SCENARIO: 剧本 A（双开同一 profile）：用户在侧栏对同一台主机点两次连接，得到 session_id=A、session_id=B（`session_cmd.rs:50` 每次 `Uuid::new_v4()`），generation 都是 1。在 A 里上传 build-old.jar 到 /opt/app/app.jar，几秒后在 B 里上传 build-new.jar 到同一路径。锁键分别是 `remote:A#1:/opt/app/app.jar` 与 `remote:B#1:/opt/app/app.jar`，`try_lock_target` 两次都返回 Some，没有任何一侧收到「目标忙」。两个作业都对 `/opt/app/app.jar.fspart` 以 WRITE|CREATE 打开并在各自的 offset 游标上 write_at：小文件的块区间重叠，大文件则一方写高位、一方写低位，最终 .fspart 是两个 jar 的字节拼接；两侧随后各自 commit（rename，必要时 remove-then-rename），后完成的一方把这坨混合物 rename 成 /opt/app/app.jar。UI 两条都显示 Done，用户拿到一个能通过大小检查却无法解压的 jar。剧本 B（重连超时）：连接抖动触发重连，`establish_session` 先 `shutdown_session_subsystems`，若 5s 宽限期到期仍有上传在跑（大文件正常触发），代码只 `warn!` 并继续，generation 自增为 2；用户重新提交同一路径的上传，键从 `remote:S#1:...` 变成 `remote:S#2:...`，与仍在跑的旧代次作业互不排斥，同样并发写同一个 .fspart。注意 `state.rs` 上 transfer_ns 的文档注释正是用「旧代次的任务此时已在 shutdown_session_subsystems 里被取消」来论证带 generation 是安全的——而那次 shutdown 是**可以超时返回 false 的**，注释的前提不成立。
JUDGE: 试图推翻失败，缺陷成立。逐条亲验：① crates/sshengine/src/transfer.rs:385-393 上传键确为 `remote:{target_ns}:{remote}`，下载键为 `local:{绝对路径}` —— 只有上传方向掺入了连接身份；TARGET_LOCKS 是进程级 static（transfer.rs:121），键是唯一判别依据。② app/src/state.rs:378-380 `transfer_ns` = `{session_id}#{generation}`；app/src/commands/sftp_cmd.rs:272 按此注入。③ app/src/commands/session_cmd.rs:50 每次 session_open 都 `Uuid::new_v4()`；frontend/src/App.svelte:361-375 与 components/Sidebar.svelte:172(`ondblclick={() => onOpen(p)}`)、:124 右键菜单 open 均无「该 profile 已连接」的去重门，双开同一主机可达。④ 物理层无兜底：crates/sshengine/src/transfer.rs:176-178 `remote_part` 是确定性的 `<remote>.fspart`，不含会话/代次；crates/sshengine/src/sftp.rs:136-155 `write_at` 以 WRITE|CREATE 打开后 seek+write，无 EXCL、无服务端排他。因此两条连接对同一远端路径的上传确实互不排斥且共写同一临时件。⑤ 回归缺口属实：crates/sshengine/tests/transfer.rs:678 两个 job 传的是同一个 "ns-busy"，跨 session/跨代次无用例。⑥ 剧本 B 的前提也属实：app/src/state.rs:42/208-220 超时仅 warn，shutdown_session_subsystems 返回 ()，session_cmd.rs:195 不检查即继续登记新代次；且 transfer.rs:61-66 的设计注释正是用「两个会话各自的 /tmp/a.bin 是两个文件」来论证按连接分 ns —— 该前提对不同主机成立、对同一主机的两条连接不成立，正是缺陷所在。
FIX: 主张与危害成立，三处细节需修正：（1）generation 不是「两条连接都为 1」——app/src/sessions.rs:41,62 的 NEXT_GENERATION 是进程级全局 AtomicU64（从 1 起、fetch_add 分配），同时开的两条连接拿到的是 1 和 2，键差异比指控描述的更大，结论不变反而更强。（2）损坏形态不是简单的「两个 jar 字节拼接」：后起作业的 prepare_part（transfer.rs:503-512）在非续传时对**共享**的 `<remote>.fspart` 调 `ops.truncate(part, 0)`，先抹掉先行者已写前缀；又因 write_at 带 CREATE，赢家 rename 走临时件后，输家的后续 write_at 会重新创建一个 .fspart 并在高位 offset 写入，产出前段全零的稀疏文件，随后其 commit 再把这份垃圾 rename 覆盖最终目标，两侧都可能报 Done。真实产物是「截断 + 零洞 + 两文件混合」，而非整齐拼接。（3）剧本 B 明显弱于剧本 A：5s 超时后 session_cmd.rs:207 的 `registry.remove` 会丢弃旧 LiveSession 及其 russh Handle，旧代次 SFTP 通道随之失效，残余并发写窗口很短；剧本 A（双开同一主机）无需任何故障即可稳定复现，应作为主证据。修复方向如指控所述：上传锁键应改用物理目标（host:port[:user]:remote，或直接用规范化的 profile 主机身份），而非连接身份。

### [P2] fspart-bypasses-sandbox-symlink-guard
WHERE: crates/sshengine/src/transfer.rs:538
CLAIM: P0-2 引入的 `.fspart` 临时件使下载写入落在 `<dest>.fspart` 上，而 S42 的末段软链防护只对 `<dest>` 本身做 `symlink_metadata` 检查，`<dest>.fspart` 从未被校验——沙箱内预先存在的 `<name>.fspart` 软链会被 `truncate(true)` 跟随，造成沙箱外文件被清零覆写。
SCENARIO: 沙箱根被配置为一个非独占目录（P1-15 允许用户通过 `Profile.sftp.download_sandbox` / `sftp.sandboxRoot` 指定，常见做法是指向 ~/Downloads 或团队共享盘）。攻击者/此前的恶意归档在该目录里留下一个名为 `notes.txt.fspart` 的软链，指向 `~/.bashrc`（或 `~/.ssh/authorized_keys`、CI 上的 `~/.gitconfig`）。目录里**没有** `notes.txt`，因此用户看不出异常。用户从远端下载 `/tmp/notes.txt`：`resolve_within` 对 `<root>/notes.txt` 做检查——父目录 canonicalize 通过、末段 `symlink_metadata` 返回 Err（文件不存在），按注释「不存在（Err）是正常的新建下载，放行」放行；随后 `local_part` 把路径变成 `<root>/notes.txt.fspart`，`OpenOptions::new().create(true).truncate(true).write(true).open(&part)` 打开软链时跟随到 `~/.bashrc` 并将其**截断为 0**，再写入远端内容；commit 阶段 rename 把这条软链本身移动成 `<root>/notes.txt`。净效果：一次看似普通的下载把沙箱外的用户配置文件覆写成了远端可控内容，且沙箱本身的防护提示（“destination is a symlink…”）一次都没触发。
JUDGE: 亲自核对后无法推翻，剧本走得通，且行号完全对得上。① crates/sshengine/src/sandbox.rs:100-117 的末段软链防护只作用于 `parent_c.join(dest.file_name())`，即 `<dest>` 本身，函数返回的也是这个路径；② crates/sshengine/src/transfer.rs:184-188 的 `local_part` 是在 sandbox 返回**之后**才 `push(".fspart")`，产生一个从未被任何校验触及的新末段；③ crates/sshengine/src/transfer.rs:538-543 精确就是 `OpenOptions::new().create(true).truncate(true).write(true).open(&part)`，tokio/std 的 open 跟随软链，无 O_NOFOLLOW、无 symlink_metadata 预检；④ `grep -n symlink crates/sshengine/src/transfer.rs` 零命中，整条传输路径对 part 无任何软链防护；⑤ transfer.rs:371 是 worker 里唯一的 sandbox 调用（对 job.local），part 路径不再复检；⑥ commit（transfer.rs:593）的 `tokio::fs::rename(&part, local)` 移动的是软链本身，最终 `<root>/notes.txt` 就是那条指向沙箱外的链接，与主张的净效果一致；⑦ 威胁模型在范围内——sandbox.rs:72-77 明确把「沙箱内预先存在的软链」列为 S42 必须防住的情形，app/src/state.rs:311-329 的 P1-15 三级回退确认沙箱根可由用户指定为任意非独占目录；⑧ 假绿判断成立——crates/sshengine/tests/sandbox.rs:126 的 final_component_symlink_rejected 只直接调 resolve_within 断言报错，从不经过 prepare_part/.fspart 的真实写入路径。
FIX: 主张成立，但攻击面比描述的更宽，两处需补正：(1) 不止「非续传 truncate」一条路径——crates/sshengine/src/transfer.rs:670-676 的 exec_once 同样用 `OpenOptions::new().create(true).truncate(false).write(true).open(local_part(local))` 打开 part，因此 `resume: true` 的下载虽不截断，却会把远端字节从 offset 处**追加**写进沙箱外的目标文件，同样是越权写；(2) 软链不必指向已存在的文件——`create(true)` 会跟随悬空软链在沙箱外**新建**该文件（prepare_part 第 524 行的 `tokio::fs::metadata(&part)` 跟随软链、对悬空链返回 NotFound，于是 existing=None 直接落入 truncate 分支），故「目标必须存在（如 ~/.bashrc）」不是前提条件。修复方向应是：对 `<dest>.fspart` 施加与 `<dest>` 同等的末段 symlink_metadata 校验（最干净的做法是让 resolve_within 或调用方一并校验带后缀的临时件路径，或在 unix 上对 part 用 O_NOFOLLOW 打开）。

### [P1] import-keeps-download-sandbox
WHERE: crates/connmgr/src/repo.rs:150
CLAIM: import_json 的剥离循环只清 vault 引用与主机密钥断言，漏掉了 Profile.sftp.download_sandbox；这是一个由外部 JSON 直接决定「本机往哪写文件」的字段，导入后即成为 SFTP 下载的沙箱根，可被指向开机自启目录。
SCENARIO: 攻击者发一份 profiles.json 给受害者：host 指向自己的服务器，同时带 "sftp": {"download_sandbox": "C:\\Users\\<user>\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Startup"}。受害者点「导入」——import_json 把 id 换新、清掉 auth.vault_record / passphrase_vault_record / host_key_policy / host_key_pins（逐跳亦清），但 sftp.download_sandbox 原样落库。受害者打开这条连接（TOFU 首见确认，正常点接受），进 SFTP 面板下载任意一个文件；远端文件名由攻击者的服务器控制（可命名为 evil.cmd / x.lnk）。download_sandbox_for 第①级读到攻击者给的路径，validate_sandbox_root 逐项通过（绝对路径 ✓、末段非符号链接 ✓、canonicalize 成功 ✓、是目录 ✓——它没有任何基目录白名单），返回 Startup 目录作为沙箱根；sftp_cmd.rs 的 Down 分支用 root.join(rel) 作为落盘目标，resolve_within 只保证「不越出 root」，而 root 本身就是攻击者选的。文件落进启动目录 → 受害者下次登录即执行。
JUDGE: 我逐段核对后无法推翻，剧本成立。

1) 剥离缺口属实：crates/connmgr/src/repo.rs:150-169 的循环只重铸 id、清 auth.vault_record / auth.passphrase_vault_record / host_key_policy / host_key_pins（Profile 级与逐跳），完全没有触碰 p.sftp。随后 repo.rs:203 `.bind(serde_json::to_string(&p.sftp)?)` 把整个 SftpDefaults 原样写库。上层 app/src/commands/conn_cmd.rs:157-166 profiles_import 只是薄封装，无额外清洗。全仓 grep download_sandbox 后，除 state.rs 的消费点与前端 ProfileDialog 外无任何导入面净化逻辑。

2) 外部 JSON 可携带：crates/connmgr/src/model.rs:83-88 SftpDefaults.download_sandbox: Option<String>，Profile.sftp 带 #[serde(default)]（model.rs:136-137），字段名即 serde 键，手写 JSON 可直接给值。

3) 消费链属实：app/src/state.rs:262-278 download_sandbox_for 第①级（最高优先级）经 profile_download_sandbox（state.rs:282-289，直接读 profile.sftp.download_sandbox）取值；validate_sandbox_root（state.rs:325-370）只校验 非空/绝对路径/末段非符号链接/可 canonicalize/是目录，确无任何基目录白名单或「须在应用数据目录下」的约束。Startup 目录这几条全过。落盘 app/src/commands/sftp_cmd.rs:190-205：root = download_sandbox_for(...)，dest_raw = root.join(rel)，sandbox::resolve_within(&root, &dest_raw) 只保证不越出 root，而 root 正是导入文件指定的。前端 SftpPane.svelte:134-142 下载时 local 传空串、remote = joinPath(remotePath, name)，确认落点完全由后端沙箱根决定。

4) 测试确实同源遗漏，且比指控说的更强：crates/connmgr/tests/repo.rs:214-264 的 import_strips_per_hop_trust_and_passphrase_refs 只断言那四类字段归零；而 repo.rs:124-148 的 export_import_roundtrip 在第 145 行显式断言 `assert_eq!(got.sftp, p.sftp)`——sample() 的 fixture（tests/repo.rs:71）正带 download_sandbox: Some("D:/sandbox")。也就是说「sftp 原样穿过导入」不是无人覆盖的空白，而是被一条绿灯测试当作正确行为钉死的，改起来会撞这条断言。这反而加强了指控。
FIX: 剧本主体成立，两处细节需要修正/收紧：

(a) 落盘路径保留远端目录结构，不是直接落在沙箱根。sftp_cmd.rs:191 的 safe_relative_path（同文件 118-134）逐段清洗但保留层级，所以下载 /home/x/evil.cmd 会落到 <sandbox>\home\x\evil.cmd。Windows 启动目录只执行其直接子项，不递归子目录，因此「登录即执行」这一步要求被下载的远端文件位于 SFTP 会话根下（remote = "/evil.cmd" → rel = "evil.cmd" → 直接落进 Startup）。这对攻击者不构成障碍：他既控制服务器（可 chroot 使用户家目录即 "/"，或把诱饵放在 "/"），又能通过同一份 JSON 顺带指定 sftp.remote_dir="/"（该字段同样未被剥离），让 SFTP 面板一打开就停在根目录。

(b) 更准确的定性不必依赖 Startup 这一条利用链：本缺陷的本质是「一份外部 JSON 可以把本机下载沙箱根设为任意已存在的绝对路径目录」，即导入即获得受害者权限下的任意目录写入/覆盖点（DLL 侧载目录、便携式应用配置目录、自启目录等），Startup 只是其中最直接的一种。触发仍需受害者主动导入 + 打开该连接（TOFU 首见确认由 import 剥离后仍会弹）+ 下载一个文件，属「一次社工点击 + 一次正常操作」链，P1 定级合理。

### [P2] settings-autolock-not-reloaded
WHERE: frontend/src/components/SettingsDialog.svelte:44
CLAIM: 设置对话框从不回读 vault.autoLockMinutes，选择器每次打开都硬显 "0/从不"；因此把已生效的自动锁定改回「从不」时 select 的值没变、不触发 onchange，settingSet 永不调用——UI 显示的状态与 Vault 实际行为相反，且无法从界面关闭自动锁定。
SCENARIO: 用户在「选项 → 安全与 Vault」选 30 分钟，settingSet("vault.autoLockMinutes","30") 落库，后端 watcher 开始按 30 分钟闲置锁定。次日用户觉得太吵，重开对话框：autoLock 初始值是 $state("0")，onMount 里没有它的 settingGet，所以下拉框显示「从不」——用户看到的是「我本来就没开」。他仍点一下「从不」确认：select 的 value 未发生变化，DOM 不派发 change 事件，onchange 不执行，settingSet 不被调用，库里仍是 "30"。关闭对话框，Vault 继续每 30 分钟闲置自锁并弹 vault:locked，而界面上永远写着「从不」，没有任何路径能把它关掉（只能改库或换设备）。P1-16 修的正是「设置项承诺与真实行为不一致」，这条在同一集群里原样复现。
JUDGE: 亲自核对后无法推翻，核心机制成立。(1) frontend/src/components/SettingsDialog.svelte:44 确为 `let autoLock = $state("0")`；onMount(50-64) 逐条 settingGet 了 ui.density/term.fontSize/term.opacity/ui.rightClick/ui.copyOnSelect/ui.multilinePasteConfirm/ui.ctrlVPaste/keyboard.mode/hostkey.defaultPolicy/security.clipboardClear，确实没有 vault.autoLockMinutes。(2) 全文件唯一的 $effect 在 11-15 行，只做 useFocusTrap，没有任何补读逻辑；autoLock 在全文件只出现在 44 与 168 两处。(3) 唯一写入点 SettingsDialog.svelte:168 `<select bind:value={autoLock} onchange={() => void settingSet("vault.autoLockMinutes", autoLock)}>`，只有 change 事件触发；select 值未变时 DOM 不派发 change，这一步剧本成立。(4) 全仓 grep autoLockMinutes 只命中 vault_cmd.rs（读）、SettingsDialog.svelte（写）与 docs，无第二写入路径、无迁移/播种代码。(5) 后端确实在消费：app/src/commands/vault_cmd.rs:39-53 auto_lock_minutes 读该键（JSON 串与裸串都认），63-88 spawn_auto_lock_watcher 以 AUTO_LOCK_POLL=20s 轮询、超时 `*guard = None` 并 emit vault:locked，挂载于 app/src/lib.rs:73；前端 App.svelte:148 监听该事件置 vaultLocked=true 并收窗。(6) frontend/src/components/ 下无 SettingsDialog 测试文件，指控「无测试覆盖」属实。因此「库里已是 30、UI 却显示从不、且直接点从不不写库」这条链路真实可走通。
FIX: 缺陷成立，但两处表述需修正：(a)「每次打开对话框都硬显 0/从不」不准确——SettingsDialog 在 App.svelte:660 是无条件挂载的常驻组件（`{#if open}` 在组件内部包住模板，open 只是 prop），onMount 只在应用启动时跑一次，组件状态在同一次进程生命周期内跨开关保留。所以同一次会话里改成 30 后再打开，下拉框仍显示 30，是正确的；错位只在应用重启（或组件重新挂载）后出现——剧本写的「次日重开」隐含了重启，方向无误。(b)「没有任何路径能把它关掉（只能改库或换设备）」过强——重启后仍存在一条非直觉的 UI 逃生路径：先选「5 分钟」（值变化 → change 触发 → 写入 "5"），再选「从不」（值再变化 → change 触发 → 写入 "0"），两步即可真正关闭。准确表述应为：直接点选与显示值相同的「从不」是空操作，用户在 UI 上没有任何提示说明自己需要先绕一步，因而实际表现为「看上去已关闭、点了也没用、自动锁定继续生效」。此外 sandboxRoot（44 行群组的 48 行 + 195-196 行）同属只写不读，但它是 text input：库里有值而框内为空时用户若不输入就不触发 change，不会误覆写，危害仅限显示误导，弱于 autoLock。restoreUnclosed(32)、bellMode(42)、language(30) 同样未回读，属同一集群。

### [P2] vault-put-secret-plaintext-not-zeroized
WHERE: app/src/commands/vault_cmd.rs:146
CLAIM: vault_put_secret 把 base64 解码结果放在裸 Vec<u8> 里，直到第 159 行才包进 Zeroizing；中间第 157 行的 `ok_or("vault 未解锁")?` 早退会让这份私钥明文按普通 Vec 释放，堆上不清零——与同文件 229-231 处 R114 注释所确立的标准自相矛盾。
SCENARIO: 用户在「添加凭据」表单里粘贴私钥内容，被同事叫走。P1-16 的自动锁定（默认轮询 20s）在 5 分钟闲置后触发，state.vault 被置为 None。用户回来点「保存」：secret_b64 被 Zeroizing 包住（145），第 146-148 行 decode 出裸 `Vec<u8> bytes`（私钥完整明文），第 155 行 touch_vault_activity() 只更新计时器不会重新解锁，第 157 行 `vault.as_mut().ok_or("vault 未解锁")?` 立即返回 Err。bytes 走默认 Drop 被释放，内容原样留在已释放堆页上，直到被复用覆盖；此后的崩溃转储、休眠文件、页交换或同进程内存读取都可能取到它。UI 只提示「vault 未解锁」，用户解锁后重试一次，于是堆上留下两份残留。
JUDGE: 亲自读了 C:/Users/qq951/IdeaProjects/shell工具仿制/app/src/commands/vault_cmd.rs 全文与调用侧，未能推翻。核对点：(1) 145 行 `let secret_b64 = Zeroizing::new(secret_b64);` 只保护了编码母体；146-148 行 `let bytes = base64::engine::general_purpose::STANDARD.decode(&*secret_b64).map_err(...)?;` 产出的确实是裸 `Vec<u8>`，直到 159 行 `zeroize::Zeroizing::new(bytes)` 才受保护。(2) 中间确有早退：157 行 `let store = vault.as_mut().ok_or("vault 未解锁")?;` 此时 `bytes` 仍存活，`Vec<u8>` 的默认 Drop 只释放不清零。155 行 `touch_vault_activity()` 仅 store 时间戳，确实不会重新解锁（33-35 行）。(3) 「vault 未解锁」是常规可达路径而非罕见分支：`vault_lock`（197-200）直接置 None；`spawn_auto_lock_watcher`（63-88）在闲置超时后置 None；且前端 frontend/src/components/ProfileDialog.svelte:196-209 的 putSecret() 在调用前完全不检查 vault 状态，锁定态下点「保存」必然走到这个早退，UI 只回显「保存失败：vault 未解锁」，用户重试即再留一份残留——与指控剧本一致。(4) 同文件 222-224 的 R114 注释确实把「裸持有 + `?` 早退不清零」定性为已修审计项，且明确写了「是可触发的常规错误路径，非罕见分支」；142-144 的注释声称覆盖「编码母体 + 解码产物」两侧，实际只覆盖母体，属自相矛盾的遗漏。(5) 修复代价为零（把 Zeroizing::new 上提到 decode 处），无性能/所有权取舍可辩护——`store.put` 本就接收 `Zeroizing<Vec<u8>>`。
FIX: 两处需修正/补强：(a) 主张里写「同文件 229-231 处 R114 注释」有误——R114 注释实际在 222-224 行，229-231 行是 `String::from_utf8(secret.to_vec())` 及其块内 drop 说明；证据段引的 222-224 才是对的。(b) 触发面比剧本更宽：不必依赖自动锁定这一条路径。最简单的触发是「vault 从未解锁 / 用户手动点过锁定」——ProfileDialog.putSecret() 不做任何 vault_status 前置检查，任何锁定态下的保存都必然在 157 行早退。此外 156 行 `state.vault.lock().await` 是一个 await 点，命令 future 若在此被取消/丢弃，`bytes` 同样按裸 Vec 释放，这是剧本未提到的第二条泄漏路径。另需说明本项属纵深防御级别的卫生缺陷（进程内堆残留，非可远程利用），且入参 String 由 serde 反序列化产生的上游临时副本本就不受 Zeroizing 覆盖——但这不构成本行不修的理由，同文件已用 R114 确立了该标准。

### [P2] clipboard-copy-utf8-error-leaks-plaintext
WHERE: app/src/commands/vault_cmd.rs:229
CLAIM: vault_copy_to_clipboard 里 `String::from_utf8(secret.to_vec())` 在失败时把明文副本交给 FromUtf8Error，`map_err(...)?` 让这个持有裸 Vec 的错误对象无清零释放；to_vec() 产生的那份拷贝不在任何 Zeroizing 保护之下。
SCENARIO: 用户通过 vault_put_secret 存入一个二进制私钥（kind=private_key，内容为 DER 或任意非 UTF-8 字节——该命令接受任意 base64，不校验编码），随后在凭据列表点「复制到剪贴板」。第 228 行 store.get 解出 Zeroizing<Vec<u8>>，第 229 行 secret.to_vec() 复制出一份裸明文并交给 String::from_utf8；因含非 UTF-8 字节返回 Err(FromUtf8Error)，该错误内部持有那份裸 Vec。`.map_err(|e| e.to_string())?` 只取走错误描述，FromUtf8Error 连同私钥明文按默认 Drop 释放，堆上不清零。外层 `Zeroizing::new({...})` 因块以 `?` 提前退出而从未构造，保护不到这份拷贝。用户看到「invalid utf-8 sequence…」，重试几次就在堆上留下几份未擦的私钥。
JUDGE: 亲自核对后无法推翻，缺陷成立。① app/src/commands/vault_cmd.rs:225-231 与指控逐字一致：Zeroizing::new({...}) 包的是块的求值结果，块内 229 行的 `?` 直接从函数返回，Zeroizing::new 根本没被调用，保护不到任何东西。② crates/vault/src/store.rs:552 确认 Store::get 返回 Zeroizing<Vec<u8>>；Cargo.toml:23 锁定 zeroize "=1.9"，该版本 Zeroizing 只有 Deref/DerefMut、无 to_vec 内联方法，故 secret.to_vec() 经 Zeroizing<Vec<u8>> → Vec<u8> → [u8] 自动解引用落到 <[u8]>::to_vec()，确实新分配了一份不受任何保护的明文堆缓冲。③ std 的 FromUtf8Error 结构体持有 bytes: Vec<u8>（就是传入的那块缓冲），其 Display 只打印内部 Utf8Error 描述；`.map_err(|e| e.to_string())` 按值吃掉 e，闭包结束即 drop，那份明文 Vec 走默认 Drop 释放，不清零。成功路径无此问题——from_utf8 复用同一缓冲且由 Zeroizing<String> 接管。④ 可触发性成立：app/src/commands/vault_cmd.rs:136-161 的 vault_put_secret 只做 base64 解码，对字节内容零编码校验，kind 可取 private_key。⑤ 排除了「上游本来就漏、此处属噪声」的反驳：crates/vault/src/crypto.rs:96-112 中 pt 在存在明文后无任何早退分支，直接 Ok(Zeroizing::new(pt))，上游干净，229 行是这条路径上唯一的未擦拷贝点。⑥ 判定标准（同函数 222-224 行 R114 注释）确实只覆盖了③处剪贴板的两个 `?`，管不到 229 行自身。
FIX: 主张与技术机理准确，两处剧本细节需修正：

1）触发入口写错了。前端目前没有「凭据列表 → 复制到剪贴板」按钮——全仓 grep 显示 frontend/ 下无任何 vault_copy_to_clipboard 的 invoke 调用（Task 20 Step 2 的 Sidebar/ProfileDialog 尚未开始）。真实可达路径是：该命令已在 app/src/lib.rs:97 注册进 invoke_handler，故可由 webview 侧任意 IPC 调用触达。即「已上线可达的 IPC 命令」，而非「用户点击的既有 UI 动作」。

2）危害面要收窄。返回给调用方的错误串只有 std 的「invalid utf-8 sequence of N bytes from index M」，不含任何明文字节——不存在明文外泄到 IPC/日志。真实后果只是：每次失败调用在堆上留下一份已释放但未清零的明文副本（多次重试则多份），暴露面限于 core dump、崩溃转储、页面换出（swap/hiberfil）以及后续堆分配复用前的窗口。据此严重度按 P3 更贴切（违反本项目自定的 §3.2「用毕即擦」不变式与 R114 同类约束，属一致性/纵深防御缺陷，而非可直接利用的泄露）。

准确表述：vault_copy_to_clipboard 在 app/src/commands/vault_cmd.rs:229 用 secret.to_vec() 制造了一份不受 Zeroizing 保护的明文拷贝并交给 String::from_utf8；当凭据内容非 UTF-8（vault_put_secret 接受任意 base64 且不校验编码）时返回的 FromUtf8Error 持有该拷贝，`.map_err(|e| e.to_string())?` 使其按默认 Drop 释放而不清零，且外层 Zeroizing::new 因块内早退从未构造。修法可为改用 std::str::from_utf8(&secret) 借用校验后再 to_owned，或先构造 Zeroizing 再校验，使早退路径也在保护之下。

### [P0] hostkey-accept-record-choice-mismatch
WHERE: frontend/src/components/HostKeyDialog.svelte:118
CLAIM: 主机密钥对话框的「接受并记录」按钮发送的 choice 字符串是 "accept_persist"，而后端 auth_cmd.rs 只识别 "accept_record"，落入 catch-all 分支 `_ => HostKeyChoice::Refuse`，于是「接受」被静默翻译成「拒绝」。
SCENARIO: 用户新建一个从未连过的主机的 profile（policy 默认 Tofu，pins 为空）→ 点击连接 → connect.rs 的 resolve_host_key 得到 Decision::AskTofu → 前端弹出 HostKeyDialog（kind 非 "changed"，焦点落在主按钮上）→ 用户点击「接受并记录」（或直接敲回车，因为 line 57 让该按钮成为初始焦点）→ 前端 invoke("hostkey_decide", { sessionId, promptId, choice: "accept_persist" }) → auth_cmd.rs:33-37 的 match 无 "accept_persist" 臂，走 `_ => HostKeyChoice::Refuse` → 引擎按拒绝处理，连接被中止，且密钥从未写入信任库。结果：任何首次 TOFU 连接都无法建立，且用户重试多少次都一样（每次都重新弹框、每次都被翻译成拒绝），UI 上表现为「点了接受却连不上、也没有任何报错解释」。
JUDGE: 亲自逐处核对，剧本走得通，无法推翻。(1) frontend/src/components/HostKeyDialog.svelte:60 联合类型为 "accept_persist" | "accept_once" | "reject"；:118 主按钮 onclick 传 decide("accept_persist")；:63 const args = { sessionId, promptId, choice } 原样传入，:66 invoke("hostkey_decide", args)。(2) frontend/src/lib/ipc.ts:1 与 :5 —— invoke 直接从 @tauri-apps/api/core re-export，全文件无任何 choice 归一化包装。(3) app/src/commands/auth_cmd.rs:33-37 match 只有 "accept_record" 与 "accept_once" 两臂，_ => HostKeyChoice::Refuse，故 "accept_persist" 确实落入 Refuse。(4) crates/sshengine/src/connect.rs:63-82 —— AskTofu/Changed 走 events.host_key_decision，Refuse => Ok(false)（不落库、不接受），只有 AcceptAndRecord 才 trust.record/replace。(5) crates/sshengine/src/hostkey.rs:59-64 —— known 为空 + 非 Strict 策略 => Decision::AskTofu，剧本前置条件成立。(6) app/src/events.rs:116-152 确实 emit "hostkey:prompt"（含 promptId），App.svelte:691 挂载 HostKeyDialog，事件确实到达该对话框；HostKeyDialog.svelte:57 非 changed 时初始焦点为 acceptBtn，即回车默认答案就是这个坏按钮。(7) 跨边界测试确不存在：frontend/src 下 grep hostkey 的 *.test.ts 零命中；crates/sshengine/tests/connect_logic.rs:168 直接构造枚举、绕开 IPC 字符串。指控的"假绿"判断成立。
FIX: 主体成立，两处表述需收紧：(a) 并非"任何首次 TOFU 连接都无法建立"——中间按钮「仅本次接受」发送的 "accept_once" 与后端拼写一致（auth_cmd.rs:35），该路径可用；真正损坏的是主按钮「接受并记录」（HostKeyDialog.svelte:118）以及以它为初始焦点的回车默认路径，后果是密钥永不落库、每次连接都重新弹框，且用户点主按钮/敲回车必失败。kind=changed 的「接受并记录」同样损坏。(b) 并非"没有任何报错"——Refuse 令 connect.rs:65 返回 Ok(false)，russh 据此判密钥被拒并使建连失败，用户会看到一个连接错误，只是错误语义具误导性（表现为"密钥被拒"，而非"你的选择在 IPC 边界被错译"）；HostKeyDialog.svelte:67-70 的 catch/toast 不会触发，因为 hostkey_decide 本身返回 Ok。另注：前端第三个字符串 "reject" 同样不在后端 match 中，靠 catch-all 巧合得到正确的 Refuse；契约（docs 记载的 accept_record|accept_once|refuse）实为三个字符串全部不符，只有 accept_persist 一处造成语义错误。

### [P1] pinned-empty-pins-clickthrough
WHERE: crates/sshengine/src/hostkey.rs:47
CLAIM: policy = FingerprintPinned 且 pins 为空时，decide() 直接短路、跳过全局信任库比对，返回 `Decision::Changed { old_fingerprint: "" }` —— 即降级为一个可以点「接受」放行的对话框，安全性反而弱于 Strict 的硬拒绝；而 UI 允许选中该策略却没有任何添加 pin 的入口，所以「空 pins」是可达且必然的状态。
SCENARIO: 用户在 ProfileDialog 的主机密钥策略下拉里选「指纹钉扎」（ProfileDialog.svelte:291 `<option value="fingerprint_pinned">`）→ 保存。该对话框只有 removePin（:211），没有 addPin，空状态文案就是「无钉扎指纹（TOFU/严格无需）。」，「导入 known_hosts」按钮调的是 hostkey_import（写全局信任库，不写 profile pins），而 repo.rs 的 import_json 还会显式 `p.host_key_pins.clear()`。于是保存后 pins 恒为 []。→ 连接该主机：decide(FingerprintPinned, pins=[], known=[该主机已在信任库中的正确密钥], presented) → 命中 line 47 的 `policy == HostKeyPolicy::FingerprintPinned` 短路 → 不查 known → 返回 Changed{ old_fingerprint: "" } → 前端弹出红色「主机密钥已变更」对话框，old 指纹为空字符串。用户在 MITM 场景下看到的是一个每次连接都出现、且上一次也出现过的「变更」告警，训练出无脑点接受的肌肉记忆；而只要点了接受（AcceptAndRecord），replace 还会把攻击者的密钥写进信任库覆盖原记录。同一用户若当初选的是「严格」，得到的是 RefuseStrict 硬拒绝、不可点穿。即：选择了名义上最强的策略，实际得到的保护弱于次强策略。
JUDGE: 试图推翻失败，逐环节均亲自核对通过。(1) hostkey.rs:47 的 `if !pins.is_empty() || policy == HostKeyPolicy::FingerprintPinned` 与引用逐字一致；pins 为空时 `.any()` 恒 false、`pins.first()` 为 None，必然产出 `Changed{old_fingerprint:""}`，而 :59-68 的 known 查找与 `Strict => RefuseStrict` 整段不可达。(2) 空 pins 可达：ProfileDialog.svelte:291 提供 `fingerprint_pinned` 选项；:88 `pins = p?.host_key_pins ?? []` 是唯一初始化，:211 removePin 是全文件唯一 pin 变更函数，确无 addPin；:305 的「导入 known_hosts…」走 :160-173 `invoke("hostkey_import")`，只写全局信任库；crates/connmgr/src/repo.rs:159 `p.host_key_pins.clear()`（另 :167 清跳板 pins）。app/src/commands/ 下无任何写 pin 的命令。(3) 无兜底校验：对 `FingerprintPinned` 在 crates/connmgr/src、app/src、crates/sshengine/src 全量 grep，仅命中 model.rs:10 枚举定义、connect.rs:883 测试、hostkey.rs:47 本身，不存在「策略=钉扎但 pins 为空」的保存期或连接期拒绝。(4) 后果链闭合：connect.rs:57 `hostkey::decide(...)` 是唯一生产调用点，Changed 落入 :63-84 交互分支；app/src/events.rs:138 `"kind": Changed => "changed"`、:139 `old_fingerprint: Some(old_fingerprint.clone())`（即 `Some("")`，非 None）；HostKeyDialog.svelte:97-107 渲染红色「主机密钥已变更」告警与空 `<code>`，:118 「接受并记录」按钮存在，选中后 connect.rs:74-77 走 `trust.replace` 覆盖信任库原记录。(5) 测试假绿属实：crates/sshengine/tests/hostkey.rs:65-84 的 pin_takes_precedence 两处调用均用非空 pins，全文件无一处 `decide(HostKeyPolicy::FingerprintPinned, &[], ...)`。
FIX: 指控主体准确，补三处更精确的表述（均使结论更强或更严谨，不削弱）：

1) 危害被低估：由于 :47 在查 `known` 之前就短路，用户点「接受并记录」触发的 `trust.replace`（connect.rs:76）写入的记录**在该策略下永远不会被读取** —— 下次连接仍从 :47 短路，仍返回 `Changed{""}`。因此该红框是**每次连接无条件、永久复现**的，「仅本次接受」与「接受并记录」对抑制后续告警**完全等效（都无效）**。这比原文「训练出无脑点接受的肌肉记忆」更强：不是概率性脱敏，而是确定性地把安全告警降级为必经的点击噪音。写入信任库的副作用仅在用户日后把策略改回 tofu/strict 时才显形（届时攻击者密钥已被静默固化为可信）。

2) 前端有一处部分缓解，不构成防御：HostKeyDialog.svelte:52-57 对 `kind === "changed"` 把初始焦点放在「拒绝」按钮上（注释「默认焦点即默认答案，高危分支不能默认接受」）。但「接受并记录」按钮仍在（:118），一次点击即可放行，不改变可点穿结论。另 `old_fingerprint` 是空字符串而非 null，故 dialog 走 changed 分支并渲染一个**空白**的「旧密钥指纹」行，而非省略该行 —— 这个空白本身就是可见的异常信号，却未被任何代码识别为「配置错误」。

3) 波及面不止 Profile 级：connect.rs:250-266 `hop_profile()` 用 `h.host_key_policy.unwrap_or_default()` + `h.host_key_pins.clone()` 构造跳板 Profile，跳板若配 `fingerprint_pinned` 且 hop pins 为空，逐字复现同一缺陷。值得注意的是 connect.rs:238-239 的文档注释**恰好逐字描述了本缺陷的危害**（「用户为了连上去在那个「密钥已变更」的红框上点接受——跳板机就此从未被真正校验，而 UI 上显示的却是「已按钉扎策略校验」」）—— 作者在「跳板继承父 pins」这一情形下识别并修掉了该病理，却遗漏了 hostkey.rs:47 这个根因，Profile 级与 hop 级的空 pins 路径至今仍然中招。

4) 原文一处措辞需澄清（不影响结论）：当前 connect.rs 的 `hop_profile` **不再**继承父 Profile 的 pins（签名已刻意不收 parent），所以不存在「父 Profile 空 pins 传染给跳板」的路径；跳板的空 pins 是其自身配置独立触发的。

### [P1] jump-hop-policy-unreachable-tofu
WHERE: crates/sshengine/src/connect.rs:260
CLAIM: 跳板链每一跳的 host key 策略走 `h.host_key_policy.unwrap_or_default()`，默认值是 Tofu；而代码注释指定的补救手段（逐跳配置 JumpHop::host_key_policy / host_key_pins）在当前产品里根本不可达，因此 profile 上设置的 Strict / FingerprintPinned 对堡垒机一跳静默失效。
SCENARIO: 用户为目标主机 profile 设置 policy = Strict（意图：任何未知/变更的密钥一律硬拒），并配置了一条经堡垒机 bastion.example.com 的 jump（jump 数据只能来自导入或历史数据，见下）。→ 连接时 connect.rs 先对每一跳调 hop_profile 构造该跳的临时 profile，line 260 把 `h.host_key_policy`（None）解成 `HostKeyPolicy::default()` = Tofu → 第一跳 bastion 的密钥若是首次见到，decide 返回 AskTofu，弹框可点接受；若攻击者在到 bastion 的路径上做 MITM 且本地无该 bastion 记录，用户点一次「接受并记录」即被中间人接管整条链（此后所有到目标主机的流量都经攻击者），而目标主机那一跳的 Strict 从未有机会生效——攻击者只需转发目标主机的真实密钥即可。用户完全没有可用手段把这一跳提升为 Strict：ProfileDialog.svelte 的 save() 里写着 `jump: initial?.jump ?? []` 并注明「本对话框暂无跳板编辑 UI，故原样透传既有值」，而 jump 链最现实的来源 import_json 在 crates/connmgr/src/repo.rs 中显式执行 `h.host_key_policy = None; h.host_key_pins.clear();`，把每一跳的身份断言全部抹平回 None。
JUDGE: 逐条核对，结构性主张全部属实，无法推翻：

1) crates/sshengine/src/connect.rs:260 确为 `host_key_policy: h.host_key_policy.unwrap_or_default()`，:261 `host_key_pins: h.host_key_pins.clone()`；crates/connmgr/src/model.rs:6-11 的 HostKeyPolicy 带 `#[default] Tofu`，model.rs:31-35 的 `JumpHop::host_key_policy` 是 `#[serde(default)] Option<HostKeyPolicy>`。hop 未配置时确实回落 Tofu，且 hop_profile 刻意不收 parent，profile 级 Strict 对跳板一跳完全不参与。

2) connect.rs:246-247 的文档注释确实把「在这一跳上配 JumpHop::host_key_policy / host_key_pins」写成用户可用的补救路径，但该路径没有写入端：frontend 全仓仅 3 处提到 jump（ProfileDialog.svelte:139 `jump: initial?.jump ?? []` 并自注「本对话框暂无跳板编辑 UI，故原样透传既有值」、types.ts:48-58 类型声明、types.ts:118），无任何跳板编辑组件；Rust 侧除 model/connect/内联测试外无 JumpHop 构造点。

3) 唯一现实的 jump 来源 profiles_import（frontend/src/App.svelte:584 → app/src/commands/conn_cmd.rs:158 → crates/connmgr/src/repo.rs:122 import_json）在 repo.rs:166-167 逐跳执行 `h.host_key_policy = None; h.host_key_pins.clear();`。于是「可创建的跳板链」与「可达的补救手段」互斥：hop 恒为 Tofu，用户无产品内手段提升。

4) 语义确认：crates/sshengine/src/hostkey.rs:59-64 的 decide 在 known 为空时 Strict → RefuseStrict、其余 → AskTofu；connect.rs:304-311 的 Connector 用的是 hop_profile 产出的 host/port/policy/pins，resolve_host_key 按该跳自己的 host:port 查信任库并弹框。首见 bastion 即一次可点接受的 TOFU 框，与指控一致。

5) connect.rs:876-925 的 hop_never_inherits_target_host_pins 确实把「hop 空钉 + Tofu」断言为期望行为（分支②虽演练了逐跳 Strict/pins，但该输入在产品中无写入端），指控对该测试性质的描述成立。

未能推翻的原因：所有「代码里其实已有防护」的候选（导入面兜底、UI 兜底、其他 jump 写入端、decide 的额外分支）我都查过，均不存在。
FIX: 危害面被夸大，需修正为：

(a)「目标主机那一跳的 Strict 从未有机会生效——攻击者只需转发目标主机的真实密钥即可」不成立。connect.rs 末尾是 `connect_inner(profile, secrets, pool, events, Some(stream))`，目标主机那一跳仍以完整 profile（Strict + pins）在转发流之上做端到端 SSH 握手与 check_server_key。中间人若只转发 TCP 流给真实目标，内层 KEX 签名由目标主机私钥产生，攻击者只见密文、无法解密或篡改内层会话；若想真正 MITM 内层，就必须出示自己的主机密钥，此时目标那一跳的 Strict/known_hosts 会判 RefuseStrict/Changed 挡下。故「此后所有到目标主机的流量都经攻击者（可读）」是错的，只是路由经过攻击者。

(b) 真实危害应限定在「堡垒机这一跳」：攻击者可完整 MITM 到 bastion 的 SSH 会话——窃取 bastion 的口令 / keyboard-interactive 应答（password 认证下即凭据泄露）、观察连接元数据、任意拒绝服务或悬挂会话，并在用户点过一次「接受并记录」后把伪造密钥写进 host_keys 表长期驻留（resolve_host_key 的 AcceptAndRecord 分支）。仍是 P1 量级的信任降级，但不是「整条链被接管」。

(c) 两个细节补正：① import_json 同时把 Profile 级 policy/pins 也剥回默认（repo.rs:159-160），所以导入后目标主机的 Strict 也需用户在 ProfileDialog 重新设置——目标可设、跳板不可设，这正是缺陷落点；② TOFU 弹框传的是该跳自己的 host:port（connect.rs:304-311 + resolve_host_key），界面不会把 bastion 冒充成目标主机，问题不在提示误导，而在「用户没有任何途径要求这一跳必须 Strict/钉扎」。

(d) 修复方向的准确表述：pins 绑定具体主机身份、不得跨主机继承（现状正确），但 policy 是与主机无关的「严格度意图」，把它与 pins 一并丢弃才是本缺陷成因；要么让 hop 继承父 Profile 的 policy（仅 policy，配合该跳自己的 known_hosts），要么在 ProfileDialog 补上跳板编辑 UI，使 connect.rs:246-247 承诺的路径真正可达。

### [P2] truststore-single-row-false-changed
WHERE: crates/sshengine/src/hostkey.rs:208
CLAIM: import_known_hosts 对同一 (host, port) 的第二条密钥直接计入 conflicted 并丢弃，配合 record/replace 维持的「每个 host:port 至多一行」不变式，使多算法主机在 russh 协商到未被导入的那种算法时产生假的「主机密钥已变更」告警。
SCENARIO: 典型服务器的 known_hosts 里同一主机同时有 ssh-ed25519 与 ssh-rsa 两行（OpenSSH 客户端默认会各记一行）。用户点「导入 known_hosts」→ import_known_hosts 先写入先出现的那条（比如 ssh-rsa），遇到第二条 ed25519 时命中 line 208-209 的 `if !known.is_empty() { s.conflicted += 1; continue; }` 被丢弃。→ 随后用本客户端连接该主机，russh 按其默认算法偏好协商出 ssh-ed25519，presented.key_blob 与库里唯一那条 rsa 记录不等 → decide 走到末尾 `None => Decision::Changed { old_fingerprint: known[0].fingerprint_sha256 }` → 弹出红色「主机密钥已变更，可能存在中间人攻击」告警，展示的 old 指纹是 rsa 的、new 是 ed25519 的。这是一次纯假阳性 MITM 告警；用户为了能连上只能点接受，replace 会把 ed25519 覆盖写入、rsa 记录消失——而真正的 MITM 告警与这次长得一模一样，告警的信噪比被这条不变式直接破坏。
JUDGE: 我逐条核对后无法推翻。(1) hostkey.rs:203-211 确认 import 对同一 (host,port) 的第二条异 blob 记录只做 `s.conflicted += 1; continue;`，永不落库；lookup(hostkey.rs:87-94) 的 SQL 只按 host/port 过滤，不含 key_type。(2) decide(hostkey.rs:59-68) 的 `None =>` 分支只要 known 非空且 blob 不等就返回 Changed{old_fingerprint: known[0]…}，全程无算法维度。(3) record/replace(hostkey.rs:111-166) 与 migrations/0001_init.sql:20-33 的注释共同钉死「每 host:port 至多一行」，schema 注释明写该不变式。(4) connect.rs:57-86 的 AcceptAndRecord 对 Changed 走 replace，会删掉旧算法那行。(5) connect.rs:298-302 的 client::Config 只改 keepalive、其余 Default，未限制 preferred.key；russh 0.62.4 negotiation.rs:155-172 的 Preferred::DEFAULT.key 顺序为 Ed25519 → ECDSA(P256/384/521) → RSA-SHA2-512/256 → ssh-rsa，多算法服务器必定协商出 Ed25519。(6) 链路对用户可达：ProfileDialog.svelte:160-173 调 hostkey_import → conn_cmd.rs:168-180（lib.rs:103 注册），告警文案 HostKeyDialog.svelte:97-100 为「警告：主机密钥已变更 … 也可能是中间人攻击」，与真实 MITM 完全同形。因此「已导入 rsa 行的多算法主机首连即弹假变更告警」的剧本走得通，缺陷成立（属有意设计的权衡，但后果真实）。
FIX: 准确表述应为：① 假阳性并非无条件触发，而是取决于 known_hosts 里该主机**第一条**出现的行是什么算法——import 只保留首条，若首条恰是 Ed25519（russh 首选），则协商结果与库内一致、不会误报；只有当幸存那行是 rsa/ecdsa 时才误报。② 同一根因还有一个不依赖「第二条被丢弃」的变体：known_hosts 里该主机**只有**一条 ssh-rsa 旧行（服务器同时支持 ed25519）时，同样会误报。③ 「在两种算法之间来回连接时反复触发同一告警」不成立：client::Config 用 Default（connect.rs:298-302），russh 的 host key 偏好顺序是固定常量（negotiation.rs:155-172），一次 AcceptAndRecord→replace 写入 ed25519 后，后续连接仍协商 ed25519，告警是每主机一次性的，不会反复 flapping（重复导入 known_hosts 也只会 conflicted+1、不覆盖）。④ 该不变式是刻意设计（R13/R30，hostkey.rs:106-110 与 0001_init.sql:31-33 均有说明），指控应表述为「安全设计权衡引入的误报」而非实现疏漏；唯一的可见提示是导入汇总里的「冲突 N」计数（ProfileDialog.svelte:169），既无解释也不能阻止后续误报。

### [P1] aad-binds-self-declared-id
WHERE: crates/vault/src/store.rs:557
CLAIM: AEAD 的 AAD 绑的是记录**自报**的 `r.id`（与密文同在一个可篡改的 JSON 对象里），而不是查找用的 map 键 `id`；因此把整条记录原样搬到另一个 id 槽位不会触发 Integrity，S8「任何一处元数据改动都让 get 返回 Integrity」的承诺在 id 维度上不成立。
SCENARIO: 威胁模型与 crates/vault/tests/aad_binding.rs 头注声明的完全一致（vault.json 位于备份/同步盘/同机他人账户可写处）。用户有两个 profile：A = 生产库主机，`auth.vault_record = 3`（记录 3 = 生产 root 口令，label "prod-db"）；B = 攻击者可观测的测试主机，`auth.vault_record = 7`。攻击者只改 vault.json 一处：把 `records` 里键 `"3"` 的整个对象**原封不动**复制/改挂到键 `"7"` 下（对象内的 id/kind/label/version/sealed 一个字节都不动）。用户下次连测试主机 B → app/src/commands/session_cmd.rs:728 的 `store.get(7)` → store.rs:553 用 map 键 7 取出该对象，store.rs:557 却用对象自带的 `r.id = 3` 组 AAD → AAD 与封装时逐字节相同 → 解密成功，返回生产 root 口令。crates/sshengine/src/connect.rs:761 的 `build_credentials` 只按内容里有没有 "PRIVATE KEY" 二分，完全不查 kind，于是生产口令被明文发往攻击者主机。全程无任何 Integrity 报错，UI 里 vault 列表只会多出一条同名 "prod-db"（list() 同样用 r.id，见 store.rs:583），用户几乎不可能察觉。
JUDGE: 亲自核对后无法推翻，剧本走得通。① crates/vault/src/store.rs:552-564 —— `let r = self.file.records.get(&id)…` 用 map 键查找，随后 `crypto::Aad { record_id: r.id, version: r.version, kind: r.kind.aad_tag(), label: &r.label }` 全部取自记录对象自身，AAD 的每一个分量都与密文同处一个可篡改 JSON 对象内，没有任何一项锚在查找键上。② `struct Record`（store.rs 约 62-68）把 `id` 作为真实序列化字段，与 BTreeMap 的键彼此独立、可分别伪造。③ `parse_vault`（store.rs:100-118）只做 format 上下界闸门，全文 grep `r.id` 仅命中 557（AAD）与 583（list），确无 `map_key == r.id` 一致性校验；robustness.rs 只覆盖 next_id 回滚。④ `put`（store.rs:509-548）写入时 `record_id: id` 与 map 键同源，故把 get 改成 `record_id: id` 对诚实库完全等价 —— 正说明绑定锚错了一侧。⑤ 消费路径成立：app/src/commands/session_cmd.rs 的 `VaultSecrets::secret(id)` 直接 `store.get(id)`（约 726 行），由 crates/sshengine/src/connect.rs 的 `build_credentials` 按 `profile.auth.vault_record` 调用，而该函数只按 `text.contains("PRIVATE KEY")` 二分，全程不查 `kind`，秘密会照 profile 指向的主机发出。⑥ 测试确实无覆盖：crates/vault/tests/aad_binding.rs 仅有 tampered_kind_is_detected（72-95）与 swapped_labels_are_detected（98-122）两种就地改字段，无一条搬动 map 键，故该洞不会让任何测试变红，而文件头注（第 15-17 行）恰恰声明「任何一处元数据改动都让 get 返回 Integrity」。结论：S8 承诺在 id 维度上确实不成立，攻击者只需把整条记录对象原样挂到另一个键下即可让任意记录在任意槽位被解出。
FIX: 主张与剧本成立，仅需三处细化：① 行号口径 —— `put` 实际为 store.rs:509-548（非 516-538），session_cmd.rs 的调用点是 `store.get(id)` 在约 726 行（非 728），connect.rs 的 `"PRIVATE KEY"` 二分在 `build_credentials` 体内约 781 行（函数自约 755 行起）；这些偏移不影响结论。② 缺陷面比剧本更宽：不只是「换 id 槽位」，而是「AAD 的全部四个分量（record_id/version/kind/label）都取自记录自身，没有任何一项与查找键或文件级上下文绑定」，因此任意记录都可被整体搬到任意槽位、被任意 profile 取用；同理两条记录整体对调键也不会触发 Integrity（区别于已被测试覆盖的「只对调 label 字段」）。③ 可察觉性细节：若攻击者是把键 "3" 的对象复制覆盖到键 "7"，`list()`（store.rs:578-588）因用 `r.id` 会返回两条 id 均为 3、label 均为 "prod-db" 的条目，同时原测试机那条记录从列表中消失——即「多出一条同名 prod-db」且「少了一条 test-box」，且前端若以 id 作列表 key 会出现重复 key；这仍是极弱的信号，不构成防护。修复方向应是 get/list 一律使用 map 键（`record_id: id`），并在 `parse_vault` 中增加 `map_key == r.id` 一致性校验，二者对诚实库完全无损。

### [P1] save-gate-does-not-stop-lost-update
WHERE: crates/vault/src/store.rs:596
CLAIM: `save_gate` 只把两次整文件重写**串行化**，并未解决它自己注释里点名的「后者会把前者整份内容覆盖掉，且没有任何报错」——每个 Store 拿的是打开时的内存快照，后落盘者会把另一实例期间写入的记录连同 next_id 一起抹掉；而唯一覆盖该场景的测试只断言两次调用都 Ok，从不检查结果文件。
SCENARIO: 可复现剧本（已实测，见 evidence）：Vault 已解锁、state 里是 Store A（记录 {1}，next_id=1）。用户在凭据管理界面新增一条 → `vault_put_secret` 走 Store A？不，取相反次序更贴近真机：用户点了「解锁/重新解锁」（app/src/commands/vault_cmd.rs:112-129），`Store::unlock_with_passphrase` 先读盘拿到快照 {1}，随后跑 Argon2id（Params::default = 19 MiB / t=2，实测数十~上百毫秒）；在这段窗口里另一条命令 `vault_put_secret`（vault_cmd.rs:156-160）在**旧** Store A 上落盘写入记录 2（磁盘 {1,2}, next_id=2）。窗口结束，vault_cmd.rs:129 把带着陈旧快照 {1}/next_id=1 的 Store B 装进 state。此后用户再存任意一条凭据 → Store B 取 id = 2（与刚才那条撞号），整文件重写 → 磁盘上原来的记录 2 被**静默替换**。后果有两层：① 记录 2 里的凭据永久丢失，界面上曾显示「已保存」；② 引用 `vault_record = 2` 的 profile 现在解析到的是一条完全不同的秘密 —— 连生产主机时发过去的是另一台机器的口令（或反之）。全程零报错。
JUDGE: 试图推翻失败，核心主张逐条核实成立。

1) 代码事实（亲读）：
- crates/vault/src/store.rs:590-604 `save()`：只 `self.lock.save_gate.lock()` 后 `serde_json::to_vec_pretty(&self.file)` + `write_atomic`。全程不重读磁盘、无 mtime/版本/CAS 比对 —— gate 确实只做「串行化」，不做「合并/冲突检测」，与其上方注释所声称要解决的「后者整份覆盖前者且无报错」并不是同一件事（串行化只解决半成品交错，解决不了整份覆盖）。
- store.rs:508-546 `put()`：`self.file.next_id += 1; let id = self.file.next_id;` 完全基于内存快照取号，不校验磁盘上的最大 id。
- store.rs:265-280 `vault_guard()`：注册表按目录返回同一个 `Arc<VaultGuard>`，同进程多个 Store 可并存（跨进程锁不挡自己），故「两个活的 Store 指向同一文件」是被设计允许的常态。

2) 实测取证（自己跑的，不是照抄）：`cargo test -p fs_vault --test robustness in_process_reopen_shares_the_same_lock` 通过；其临时目录 C:\Users\qq951\AppData\Local\Temp\fs-vault-robust-17456-1786412504813495900-1\vault.json 实际内容为 `"next_id": 1` 且 `"records": {}` —— 测试第 529-536 行 `second.put(ApiKey,"q",…)` 写入的记录被 `first.delete(id)` 的整文件重写静默抹掉，next_id 还倒退回 1。测试只 `.unwrap()` 两次调用的 Ok，对结果文件零断言（我 grep 过 crates/vault/tests/*.rs，没有任何测试断言跨实例写入的留存），假绿成立。

3) 应用层窗口成立：app/src/commands/vault_cmd.rs:112-129 `vault_unlock` 先 `Store::unlock_with_passphrase(...)`（store.rs:427-435 先 `fs::read` 拿快照，随后才跑 verify_passphrase + derive_kek 两次 Argon2id），**之后**才 `*state.vault.lock().await = Some(store)`；state.vault 是 `Arc<Mutex<Option<Store>>>`（app/src/state.rs:51），锁并不覆盖 Store 的构造期。因此在 Argon2 期间，vault_cmd.rs:156-160 `vault_put_secret` 完全可以拿到 state 锁、用旧 Store 落盘，随后陈旧快照的新 Store 被装入 state —— 之后任何一次 save 都会整份覆盖这条记录，且 put 会重发同一个 id。vault_init（open_or_create，同样先构造后装载）有同一窗口。无任何报错路径。
FIX: 两处需要收紧表述（不改变缺陷成立与根因）：

① 触发面比剧本描述的窄一档，但缺陷本身与触发面无关。后端确实没有任何防护（两条命令可并发在飞行中），但当前前端把窗口压得很小：VaultDialog 只能从「工具」菜单打开（frontend/src/App.svelte:279-300），而 ProfileDialog 的模态遮罩盖住菜单栏，shortcuts.ts 里也没有 vault 快捷键 —— 纯鼠标点击路径下让 vault_unlock 与 vault_put_secret 同时在飞行中并不容易。因此这是「后端契约缺失 + 前端偶然掩护」，而不是「日常必现」。判定仍成立的理由是：窗口宽度由 Argon2id（19 MiB/t=2，两次）决定即数百毫秒量级，IPC 层无任何互斥，且 store 层语义本身（实测已复现）就是静默丢数据。

② 后果的两层要分清前提。「记录被抹掉」不需要 id 撞号：陈旧 Store B 之后的**任何一次** save（put/delete/change_passphrase）都会把窗口期内写入的记录整份抹掉 —— 这正是实测那条 `first.delete()` 的形态。而「profile 的 vault_record=2 解析到另一条秘密」这层，才额外需要剧本描述的「B 再 put 一次、取到与被抹记录相同的 id」这一步。指控把两层并列陈述没错，但第二层是条件更强的推论。

③ 一处措辞：save 的注释并非「没解决自己点名的问题」那么绝对 —— gate 确实解决了它同段落里的另一半（临时文件交错 / 半成品 rename），未解决的是「陈旧快照整份覆盖」。准确说法是注释的承诺范围大于实现能力，而非注释所述全然落空。

### [P0] unix-stale-lock-permanent-lockout
WHERE: crates/vault/src/store.rs:253
CLAIM: Unix 锁文件的清理路径（`Drop for VaultGuard` 里的 `remove_file`）**永远不会执行** —— 守卫的 Arc 存活在一个进程等长的 `static` 注册表里，而 Rust 从不为 static 跑析构；于是在 macOS 上（`pid_is_alive` 对非 Linux 一律返回 true）第二次启动必然被自己上一次的锁文件永久挡在门外。
SCENARIO: macOS 用户（release.yml:83-91 明确构建 universal-apple-darwin 的 dmg）：① 首次启动，`vault_init`/`vault_unlock` → `vault_guard` → `open_exclusive` 在 `~/Library/Application Support/<id>/` 建出 `vault.lock`，内容写入本进程 pid（如 4821）。② 用户正常退出应用。守卫的 `Arc<VaultGuard>` 被 store.rs:263-277 的 `static REGISTRY: OnceLock<Mutex<HashMap<..>>>` 强引用持有且「只增不减」，strong_count 永不归零，`Drop for VaultGuard`（store.rs:152-160，全仓唯一删除 vault.lock 的地方）从不运行；即便运行，Rust 也不为 static 跑析构。vault.lock 带着 pid 4821 留在盘上。③ 用户再次启动。`open_exclusive`（store.rs:202-243）的 `create_new` 返回 AlreadyExists → 读出 "4821" → `pid_is_alive(Some(4821))`：store.rs:252 的 guard `cfg!(target_os = "linux")` 在 macOS 为假 → 落到 store.rs:253 的 `Some(_) => true` → 返回 `Error::Storage("保险库已被 pid 4821 锁定…")`。④ 此后**每一次**启动都以同样方式失败（与 pid 4821 是否真的存在无关）。用户全部凭据不可用，除非他自己去隐藏的 Application Support 目录里手工删掉 vault.lock。
JUDGE: 我试着从四个方向推翻，全部失败：

① 「Drop 会跑吗」——不会。`vault_guard`（crates/vault/src/store.rs:263-277）把 `Arc<VaultGuard>` 插进 `static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<VaultGuard>>>>`，整个文件里只有 get/insert 两处操作，没有任何 remove/clear/take。故 strong_count 至少恒为 1；即便归零，Rust 也不为 static 跑析构。store.rs:152-160 的 `impl Drop for VaultGuard` 里那句 `#[cfg(unix)] remove_file(&self.lock_path)` 是事实上的死代码。

② 「有没有第二处清理点」——没有。全仓 grep `vault.lock|open_exclusive|pid_is_alive|VaultGuard|vault_guard` 只命中 crates/vault/src/store.rs、crates/vault/tests/robustness.rs:516（一句 exists 断言）与 app/src/commands/vault_cmd.rs 的 `state.vault.lock()`（那是 tokio Mutex，同名不同物）。app 侧 `vault_lock`（vault_cmd.rs:198）只是 `*state.vault.lock().await = None`，丢掉 `Store` 而已——`Arc<VaultGuard>` 仍被注册表握着，锁文件纹丝不动。

③ 「pid 判活会不会救回来」——不会。store.rs:248-255：`Some(p) if cfg!(target_os = "linux") => /proc/<p> 存在性`，macOS 上该 guard 为假，直落 store.rs:253 `Some(_) => true`。于是 store.rs:221 的 `if pid_is_alive(pid)` 恒真 → store.rs:222-227 直接返回 `Error::Storage("保险库已被 pid … 锁定")`，store.rs:229 的 `remove_file` 回收分支在 macOS 上不可达。attempt 两轮循环也帮不上——第一轮就 return 了。

④ 「会不会只堵住一条打开路径」——三条全堵。`open_or_create` 在读盘前取闸门（store.rs:306），`unlock_with_passphrase`（store.rs:462）与 `unlock_with_keyring`（store.rs:500）在读盘后取闸门；三者都是 `?` 直接上抛。app/src/commands/vault_cmd.rs:100-131 的 `vault_init`/`vault_unlock` 覆盖了全部入口，无一幸免。

⑤ macOS 确为发布目标：.github/workflows/release.yml 的 bundle 矩阵有 `os: macos-latest / tauri_target: universal-apple-darwin / bundles: dmg`。

⑥ 测试确实盖不住：robustness.rs:504-537 的 `in_process_reopen_shares_the_same_lock` 全程同进程复用注册表条目，且每个用例 `tempdir()` 独立目录，无「同一目录跨进程/跨注册表重开」的用例。

结论：剧本能走通，缺陷成立。
FIX: 主张与剧本的主干全部属实，三处表述需要收紧：

1) 「静态注册表持强引用」与「static 不跑析构」是两条各自独立、任一成立即可的原因，不是串联条件。指控把它们并列陈述是对的，但更准确的说法是：`remove_file` 那行在任何平台、任何运行路径下都不可达（连测试进程里也不跑），属于死代码，而非「理论上会跑但被注册表挡住了」。

2) 「用户全部凭据不可用，除非他自己去隐藏的 Application Support 目录里手工删掉 vault.lock」——恢复门槛没那么高，也没有数据损失。错误串（store.rs:222-227）里带了锁文件的完整路径与「若确认该进程已不存在，删除该锁文件后重试」的指引，且 vault.json 与 keyring 条目完好无损。真实症状是：每一次启动都必须先手工删一次锁文件，删完即可正常使用（因为本次启动写进去的新 pid 在下次启动时同样被判为存活）。即「每启动一次要人工干预一次」，而不是「永久失去凭据」。

3) 这个指引还被前端稀释了：frontend/src/components/VaultDialog.svelte:53 把后端错误包在 `解锁失败（密码错误或 keyring/文件异常）：${e}` 里，用户第一反应会去怀疑自己记错了应用密码，而不是去读后半截那条删锁文件的指引。

补充一条指控没提但同源的次生问题：Linux 上该路径也不是完全干净的——`Drop` 同样不跑，锁文件同样残留，只是靠 store.rs:252 的 `/proc/<pid>` 判活兜住；一旦 pid 被系统回收给了别的进程（长时间运行 / pid 空间绕回），Linux 用户会撞上同一条报错。macOS 是必然复现，Linux 是概率复现。

### [P0] hostkey-choice-string-mismatch
WHERE: frontend/src/components/HostKeyDialog.svelte:118
CLAIM: 主机密钥对话框的主按钮「接受并记录」向 `hostkey_decide` 发送 `choice: "accept_persist"`，而后端只认 `"accept_record"`，其余一律落入 `_ => HostKeyChoice::Refuse`——TOFU 首次信任这条主路径实际被翻译成「拒绝」。
SCENARIO: 全新安装（trust store 为空）→ 连接一台未知主机 → 弹出 TOFU 确认框 → 用户点击带 `class="primary"`、默认获得焦点的主按钮「接受并记录」（回车也命中它，见同文件第 57 行 `initial: kind === "changed" ? rejectBtn : acceptBtn`）→ 前端 invoke `hostkey_decide {choice:"accept_persist"}` → `auth_cmd.rs:36` 的 catch-all 把它映射成 `HostKeyChoice::Refuse` → oneshot 回送 Refuse → `check_server_key` 判定拒绝 → 握手失败、密钥从未写入信任库。用户每次点主按钮都连不上，只有点次要按钮「仅本次接受」（`accept_once`，是后端认识的字面量）才能连上，且因为没落库，下次连接照样重新弹框。密钥变更（kind==="changed"）场景下，用户主动确认接受新密钥同样被静默改判为拒绝。
JUDGE: 无法推翻，缺陷成立，链路每一环均亲自核对：(1) frontend/src/components/HostKeyDialog.svelte:60/63/66/118 — 主按钮「接受并记录」调用 decide("accept_persist")，args 原样带 choice 传入 invoke("hostkey_decide")。(2) frontend/src/lib/ipc.ts — 仅 `export { invoke, listen }` 直转 @tauri-apps/api/core，不存在任何字面量翻译层（这是我最期望找到的翻案证据，确认不存在）。(3) frontend/dist/assets/index-Dule2mgN.js — 已构建产物同样是 `U('click', y, () => void h('accept_persist'))`，证明并非仅存于源码的死代码，发货物带着该字面量。(4) frontend/src/App.svelte:19/62/690 — <HostKeyDialog bind:this={hostKeyDlg} /> 确实挂载，组件非死代码。(5) app/src/commands/auth_cmd.rs:33-37 — `match choice.as_str() { "accept_record" => AcceptAndRecord, "accept_once" => AcceptOnce, _ => Refuse }`；参数为裸 `choice: String`，无 serde rename / 自定义反序列化；app/src/lib.rs:117 是唯一注册点，无第二处命令覆盖。(6) crates/sshengine/src/connect.rs:65 — `HostKeyChoice::Refuse => Ok(false)`，经 check_server_key(:92-114) 返回 false，russh 判定服务器密钥被拒、握手失败；:67-82 的 trust.record / trust.replace 落库分支永不执行。关于「无测试跨越 IPC 字面量边界」：grep -rn "hostkey_decide" app/src crates frontend/src 仅命中命令定义、lib.rs 注册、前端 invoke 与三处注释，无任何测试；app/tests 目录不存在；crates/itest/tests/ssh_tofu.rs 的 8 处与 crates/sshengine/tests/connect_logic.rs:168 全部直接构造 HostKeyChoice 枚举，绕开字符串映射；grep -rln "hk-persist" 在 node_modules/dist 之外零命中，无 e2e。「假绿」判断成立。
FIX: 主结论与 P0 定级完全成立，仅一处细节需修正：指控称主按钮「默认获得焦点（回车也命中它）」，这只对 kind === "tofu" 成立。HostKeyDialog.svelte:57 `initial: kind === "changed" ? rejectBtn : acceptBtn` 意味着密钥变更场景默认焦点在「拒绝」按钮上，此时回车命中的是拒绝，其结果与后端实际行为恰好一致（都是拒绝），因此 changed 场景下受影响的只是「用户主动点击主按钮」这条路径，而非回车路径。准确表述：TOFU 首触场景下，默认焦点按钮 + 回车 + 鼠标点击三条路径全部被错译为 Refuse（主路径彻底不可用）；密钥变更场景下，回车落在「拒绝」上属预期，但用户主动点击「接受并记录」同样被静默改判为拒绝，且新密钥永不落库。两个场景的点击主按钮路径均破损，缺陷性质与严重度不变。

### [P1] prompt-dialog-single-slot-overwrite
WHERE: frontend/src/components/HostKeyDialog.svelte:38
CLAIM: 后端已改造为按 `(session_id, prompt_id)` 支持多个并发待决 prompt，但前端每类对话框只有全局一个实例、且把 prompt 状态存在标量 `$state` 里，后到的事件直接覆盖先到的；被覆盖的那个 prompt 在 UI 上不可达，只能等 120s 超时被拒绝。
SCENARIO: 用户连续点开两个指向不同未知主机的连接（或一次「打开全部」恢复两个会话）。后端为会话 A、B 各注册一条待决项并先后 emit 两次 `hostkey:prompt`。前端唯一的 HostKeyDialog 实例先被 A 填充，随即被 B 的载荷整体覆盖（sessionId/promptId/host/fingerprint/kind/oldFingerprint 全换成 B 的），`open` 仍为 true——用户看到的是 B 的指纹，A 的确认框从未存在过（也没有任何『还有 1 个待处理』的提示）。用户点「仅本次接受」→ 请求携带 B 的 (sessionId, promptId)，只解决 B。A 的待决项无人认领，直到 app/src/events.rs 的 `PROMPT_TIMEOUT`（120s）到期被判 Refuse，会话 A 静默连接失败。同样的覆盖发生在键盘交互认证上：两个会话同时进入 keyboard-interactive 时，AuthPromptDialog 的 `responses` 会被第二次事件重置（AuthPromptDialog.svelte:42 `responses = prompts.map(() => "")`）——若用户已在输入第一个会话的口令，输入内容会在毫无提示的情况下被清空并改问另一台主机的问题，用户很可能把 A 的口令输进 B 的会话。
JUDGE: 我逐一核对了指控中的每一处引用，并额外去验证「两个 prompt 真的能并发到达前端」这一前提（这是唯一可能推翻它的地方），结果指控成立。

1) 前端单槽覆盖 —— 属实。
- frontend/src/components/HostKeyDialog.svelte:15-28 全部是标量 `$state`（open/sessionId/promptId/host/port/keyType/fingerprint/kind/oldFingerprint），:37-50 的 `listen("hostkey:prompt", ...)` 回调无条件整体赋值并 `open = true`，没有任何「已 open 就入队/丢弃」的判断，也没有按 promptId 的集合。:60-71 的 `decide()` 用 `const args = { sessionId, promptId, choice }` 读当前标量，:73-76 `close()` 把 promptId 置 null —— 一次裁决只解决当前槽里那一枚，被覆盖的那枚在 UI 上再无入口。
- AuthPromptDialog.svelte:15-27 同构，:35-46 回调里 `prompts = e.payload.prompts; responses = prompts.map(() => "")` 确实会把用户已输入的明文应答清空并换成另一台主机的问题。加重情节：该对话框整个模板（:94-130）只渲染 name/instruction/prompts，**完全不显示 host/session**，用户在视觉上无法区分「问题换了一台主机」，把 A 的口令输进 B 的会话是现实的。
- frontend/src/App.svelte:689-690 确认各只挂载一个实例，无 `{#each}`；grep 全前端只有这两处监听 `auth:prompt` / `hostkey:prompt`，不存在别处的队列/计数兜底。

2) 后端并发度确实存在且大于 1 —— 属实。
- app/src/events.rs:18 `PromptKey = (String, String)`，:124-125 与 :157-158 每次发问现取 `new_prompt_id()` 后 insert，载荷带 `"promptId": key.1`（:131、:163）；:73-103 `take_pending` 在 promptId 为 None 且同会话待决 >1 时直接报错。:113 `PROMPT_TIMEOUT = 120s`，:206-212 超时 → `HostKeyChoice::Refuse`，与「A 静默失败」一致。
- 关键前提我单独查证：不存在任何序列化拨号的全局锁。app/src/commands/session_cmd.rs:45-102 `session_open` 是普通 async tauri command，每次 invoke 各自 `spawn_blocking` 拨号；更强的是 session_cmd.rs:607-615 —— **每个会话各有一条独立的自动重连循环**，各自 new 一个 `GuiEvents` 调 `connect()`，无跨会话互斥。两个会话同时掉线重连（kbd-interactive 配置）即可在零用户操作下产生两条并发 `auth:prompt`。
- 触发面 2：Sidebar.svelte:172 `ondblclick={() => onOpen(p)}` 是 fire-and-forget，App.svelte:359-373 `openSession` 无「连接中禁止再开」门控，用户在第一枚 prompt 弹出前连开两个配置即并发。

3) 测试覆盖 —— 属实。events.rs:241-326 的三个单测只验后端表结构（共存 / 单条回退 / clear_session 隔离），无任何测试覆盖「两个事件到达同一个前端对话框」。

未能推翻。唯一说错的是剧本里的一条触发路径（见 correction）。
FIX: 剧本中「一次『打开全部』恢复两个会话」这条触发路径不成立，应删除。App.svelte:440-461 的 `onRestoreSessions` 是 `for (const key of sessionKeys) { ... if (await openSession(profile)) ... }` 串行 await，而 `session_open`（session_cmd.rs:45-100）要等 `connect()` 整体完成才 resolve，`connect()` 内部的 hostkey/kbd 回调是同步阻塞等待前端裁决的（events.rs:195-238 block_in_place + 120s）。因此恢复路径是「答完 A 才拨 B」，不会产生并发 prompt。

准确的触发路径是以下两条（第二条更强，无需用户抢点）：
(a) 用户在第一枚 prompt 弹出前（TCP/banner 等待窗口内）连开两个配置 —— Sidebar.svelte:172 `ondblclick={() => onOpen(p)}` 不 await、App.svelte 也无「连接中」门控；注意一旦对话框已弹出，其 `.overlay { position: fixed; inset: 0; z-index: 60 }` + focusTrap 会挡住再次点击，所以该路径必须在弹窗出现前完成两次点击。
(b) 两个会话同时断线，各自的 per-session 自动重连循环（session_cmd.rs:607-615，每个 watchdog 独立 spawn，无跨会话互斥）并发调用 `connect()` —— kbd-interactive 配置下直接产生两条并发 `auth:prompt`，全程无需用户操作。

另需澄清一处措辞：由于 promptId 被原样回传（HostKeyDialog.svelte:63 / AuthPromptDialog.svelte:68），用户的应答**不会**被错投到另一回合（后端 events.rs:80-83 精确定位）。本缺陷的实际后果是「先到的 prompt 被静默丢弃 → 120s 后按 Refuse/空应答收尾，会话静默失败」，以及 kbd 场景下「已输入的明文口令被无提示清空、且因对话框不显示主机名而诱导用户把 A 的口令输入 B 的会话」——后者是凭据泄露给错误主机，属真实的安全后果。

### [P1] p0-lifecycle-zero-coverage
WHERE: app/src/state.rs:195
CLAIM: 审计 P0-4（代次化子系统缓存键）与 P0-5（先 shutdown 再 remove）的全部实现集中在 app/src/state.rs，而该文件在整个仓库里没有任何测试引用——全绿的 CI 对这两条 P0 修复不构成任何约束。
SCENARIO: 把 state.rs:208 的 `if !mgr.shutdown(SUBSYSTEM_SHUTDOWN_GRACE).await { ... }` 整段删掉（只保留 226/231 的两次 retain），或把 state.rs:17 的 `pub type SubsystemKey = (String, u64)` 改回 `String` 并删掉 sftp_ops_for 里 `gen_now != generation` 的复核（state.rs:135-137）——这正是审计原文描述的 P0-5/P0-4 缺陷形态。改完后跑 `cargo clippy --workspace --all-targets -D warnings`、`cargo nextest run --workspace`、`cargo test --workspace`、`npm test`：四道门全部通过，无一条断言变红。结果是「关掉标签后后台 worker 继续改用户本地文件」「重连后 SFTP/传输继续跑在已断开的旧连接上」这两个 P0 可以被任何一次重构悄悄改回去而 CI 不出声。
JUDGE: 我逐条核对，未能推翻。

1) 生命周期实现确实只在 app/src/state.rs：`shutdown_session_subsystems`（state.rs:195-239，先 shutdown 后两次 retain）、`SubsystemKey = (String, u64)`（state.rs:17）、`sftp_ops_for` 里的代次复核（state.rs:125-130，`if gen_now != generation` 在 128 行）、`transfer_manager_for` 里的第二次复核（state.rs:165）、`transfer_ns`（state.rs:378）。

2) 覆盖确实为零。`grep -rn --include=*.rs -E "SessionRegistry|SubsystemKey|shutdown_session_subsystems|generation_of|get_with_generation|sftp_ops_for|transfer_manager_for|transfer_ns"` 排除 target 后命中仅 6 个文件：state.rs(13)、commands/sftp_cmd.rs(10)、commands/session_cmd.rs(5)、sessions.rs(5)、lib.rs(1)、journal.rs(1)，全部是生产代码路径。`grep -n "cfg(test)" app/src/state.rs` 无输出，state.rs 无测试模块。`app/tests` 不存在（ls 报 No such file）。crates/itest 的 [dev-dependencies] 只有 fs_sshengine / fs_connmgr，不依赖 app crate。

3) app crate 的 6 个测试模块我逐个读过，无一触及生命周期：sftp_cmd.rs:425 的 12 例只测 `submitted_payload` / `safe_relative_path` / `ensure_within`；sessions.rs:282 的 3 例只测 `LinkEnd`/`DisconnectReason` 归类，完全不碰同文件里的 `SessionRegistry::generation_of`(sessions.rs:89) 与 `get_with_generation`(sessions.rs:81)；session_cmd.rs:741 的 2 例测 `is_valid_env_key` 与 `cancel_reconnect_round`，而同文件里 4 处 `shutdown_session_subsystems` 调用点（session_cmd.rs:195/295/390/446）无一被断言。

4) 我特意找过反证：`TransferManager::shutdown` 本身在 crates/sshengine/tests/transfer.rs:764 `shutdown_rejects_new_jobs_and_drains_in_flight` 里有覆盖。但那测的是引擎原语，与「app 是否调用它、调用顺序、是否遍历所有代次」这三件 P0-5/P0-4 的实质完全正交——删掉 state.rs:208 那段，sshengine 那条测试照样绿。

5) 无任何文本级门禁盯着 state.rs（全仓仅 lib.rs:27 一条注释提到 state.rs）。npm test 是前端，物理上够不到 Rust 后端。

结论：这两条 P0 的修复在 CI 上确实没有任何约束。
FIX: 剧本大方向成立，三处细节需修正：

(a) 行号：`gen_now != generation` 的代次复核在 state.rs:128-130（不是 135-137）；135-139 是 `channel_open_session` / `request_subsystem`。

(b) 「删掉 state.rs:208 整段」若只删 `if !mgr.shutdown(...) { ... }` 这个 if 块，`cargo clippy -D warnings` 会红：`SUBSYSTEM_SHUTDOWN_GRACE`(state.rs:42) 变成 dead_code，且 205-207 循环里 `let Some(mgr) = cell.get() else { continue }` 绑定的 `mgr` 变成 unused。准确表述是：必须把 198-221 的整段快照+循环连同该 const 一起删掉，只保留 223-232 的两次 retain（外加 235-238 的 verify_plans 清理）——这样退化成纯 remove 语义，clippy/nextest/cargo test/npm test 四道门全绿。同理把 `SubsystemKey` 改回 `String` 时，state.rs:226 与 232 的 `|(sid, _), _|` 模式必须一并改成 `|sid, _|` 才编得过。也就是说：门禁能挡住「删一半」，挡不住「删干净」——而重构者删的恰恰是干净的那种。

(c) 证据里的计数口误：app crate 是 6 个 `#[cfg(test)]` 模块（不是「4 个」），实际 `#[test]` 数为 conn_cmd 2 / opener_cmd 4 / session_cmd 2 / sftp_cmd 12（不是 11，其中 `ensure_within_rejects_symlink_pointing_outside` 带 `#[cfg(unix)]`，Windows 上不跑）/ events 3 / sessions 3。这些口误不影响结论。

另补一条指控没提但同样零覆盖的点：`transfer_manager_for` 装配后的第二次代次复核（state.rs:165）与 `transfer_ns` 的 `<sid>#<gen>` 格式（state.rs:378-380，P0-3 的锁命名空间粒度）同样无任何断言，改动后 CI 一样不出声。

### [P2] verify-plans-settled-orphan
WHERE: app/src/commands/sftp_cmd.rs:410
CLAIM: P1-2 的 verify_plans 握手在「登记方从不回来取」的两条真实路径上会永久留下 `VerifySlot::Settled` 孤儿条目，而 state.rs:237 的清理逻辑明确把 Settled 排除在回收之外——注释里「它由 transfer_submit 侧在同一轮内取走」的前提不成立。
SCENARIO: 路径一：用户把 `transfer.verifyAfterTransfer` 置 false（settings_cmd.rs 的 settings_set 无 key 白名单，任意键可写）。此时 sftp_cmd.rs:248 走 `(None, None)`，pending 为 None，握手块（sftp_cmd.rs:285-297）在 plans 为空时**什么都不插**；随后事件泵在终态查表落空，走 sftp_cmd.rs:409-411 插入 `Settled{done}`，此后再无任何代码路径会 remove 它。每完成一次传输泄漏一条，进程生命周期内单调增长。路径二（无需改设置）：传输在途时用户关闭标签 → shutdown_session_subsystems 先在 state.rs:208 await `mgr.shutdown(grace)`，worker 在这段时间内发出 Cancelled/Failed 终态 → 事件泵此时若已被 state.rs:233-238 的 retain 清掉 Planned（或本就晚一步），落到 None 分支插入 `Settled{done:false}`；而 retain 的匹配臂 `VerifySlot::Settled { .. } => true`（state.rs:237）明文保留它。两条路径都无任何测试覆盖。
JUDGE: 亲自读代码后无法推翻，核心机制完全成立。(1) sftp_cmd.rs:409-411 的 None 分支确实无条件 `plans.insert(id, VerifySlot::Settled { done })`；(2) sftp_cmd.rs:284-298 的握手仅在 pending 为 Some 时插 Planned，pending 为 None 时不留痕迹且永不回来；(3) state.rs:235-238 的 retain 明文 `VerifySlot::Settled { .. } => true` 保留，注释 233-234 断言的「由 transfer_submit 侧在同一轮内取走」在登记方已跑过的情形下不成立；(4) 决定性一环：transfer.rs:34-37 的 `next_transfer_id` 是进程级单调 AtomicU64，id 永不复用，故孤儿 Settled 永远等不到「同 id 的 transfer_submit」来取——这排除了「后续提交会顺手清掉」的唯一辩护；(5) `grep -rn verify_plans` 全仓仅 sftp_cmd.rs:285/401、state.rs:235 三处改动 + lib.rs:69 初始化，无 reaper/TTL/容量上限；(6) 覆盖率主张属实：sftp_cmd.rs:427 只 use 了 ensure_within/safe_relative_path/submitted_payload，`grep -rln "Settled|verify_plans"` 在 crates/**/tests 下零命中。路径一（settings_cmd.rs 无 key 白名单，settings_bool 见 state.rs:292-299，前端 ipc.ts:65 的 JSON.stringify(false) 恰好产出可匹配的裸 "false"）与路径二均可走通。
FIX: 两处需要修正/加强：① 路径一并非「每完成一次传输必泄漏一条」——只有在「submit 握手先跑、终态事件后到」这一常规顺序下才泄漏；若终态抢在握手之前，sftp_cmd.rs:286-287 会把 Settled 取走，此轮不泄漏。另外 `transfer.verifyAfterTransfer` 目前无任何前端代码写入（全仓只有 sftp_cmd.rs:211 读它），故路径一需要手工 invoke settings_set 才能触发，属潜伏缺陷而非默认配置下即发。② 路径二被低估了：它不只是「shutdown 的 await 期间的窄竞态」。真正稳定的触发是 shutdown 超时分支——TransferManager::shutdown（transfer.rs:314-329）在 SUBSYSTEM_SHUTDOWN_GRACE=5s（state.rs:42）内未收尾即返回 false，而 state.rs:208-221 明确把它当作预期结果（warn 后继续），随后 state.rs:235 的 retain 在 worker 仍在跑时就清掉 Planned；这些 worker 之后才发出终态（每个 worker 持有自己的 ev sender 克隆，管理器被 drop 后仍能投递），泵必然落到 None 分支插入 Settled 孤儿。这一分支不依赖调度巧合。③ 量级上应说明单条孤儿仅 u64 键 + Settled{bool}（约十几字节），且每作业至多一条（第二次终态走 `Some(other)` 放回分支），故是「无回收的单调增长」而非快速膨胀——P2 定级合理。

--- REJECTED ---
[{'id': 'transfer-submitted-queued-dropped', 'where': 'frontend/src/lib/transfers.ts:79', 'claim': '`transfer:submitted` 的处理函数把行状态硬编码为 `"Running"`，完全不读后端刚刚为此专门补上的 `p.state`（`"Queued"`）；后端侧的修复与单测都落空，「排队 N」永远为 0。', 'why': '指控与仓库实际代码不符，逐条核对：\n\n1) 核心主张「第 79 行写死 state: "Running"，p.state 从未被引用」——不成立。C:/Users/qq951/IdeaProjects/shell工具仿制/frontend/src/lib/transfers.ts 全文 141 行，transfer:submitted 回调在第 74-89 行，第 84 行实际写的是：`sessionId: p.sessionId, done: 0, total: 0, state: p.state ?? "Queued", startedAt: Date.now() });`。p.state 被直接透传，仅在字段缺失时兜底为 "Queued"（而非 "Running"）。指控所指的第 79 行是注释文本（"原先硬编码 \\"Running\\" 是在替后端撒谎…"），指控者很可能把这段追述历史缺陷的注释当成了代码。\n\n2) 旁证「grep -rn "Queued" frontend/src 无任何命中」——直接为假。实测命中 6 处：transfers.ts:78/81/84，TransferQueueDrawer.svelte:21/22/23。\n\n3) 旁证「TransferQueueDrawer.svelte 的 STATE_LABEL 没有 Queued 条目，会渲染裸字符串」——为假。TransferQueueDrawer.svelte:23 为 `const STATE_LABEL: Record<string, string> = { Queued: "排队中", Running: "传输中", Done: "完成", Failed: "失败", Cancelled: "已取消" };`，第 21-22 行注释还专门说明了补这一项的原因。\n\n4) 端到端语义实测走通：后端 app/src/commands/sftp_cmd.rs 的 submitted_payload() 发 "state": "Queued" → 前端 transfers.ts:84 存 state="Queued" → activeTransfers()（transfers.ts:132-141）中 "Queued" 既不等于 Running/Retrying，也不在 TERMINAL_STATES={Done,Failed,Cancelled}（第 15 行）内，故走第 138 行 queued += 1。剧本中「5 件提交、并发上限小于 5」的场景下返回 {queued:5, running:0}，随后按 progress 事件逐件转成 Running，CloseConfirmDialog.svelte:58 因此显示正确的排队/进行中拆分，不存在「排队 N 恒为 0」。\n\n5) 「唯一的消费者把该字段丢弃、sftp_cmd.rs:433 的 submitted_payload_marks_queued() 是假绿」——前提既已不成立，该结论随之失效；生产者与消费者对 "Queued" 的约定在前端确有对应实现。\n\n注：transfers.ts 目前是未跟踪新文件（git status 显示 ??），`git show HEAD:frontend/src/lib/transfers.ts` 为空，仓库内也只有这一份 transfers.ts（find 确认无副本），因此不存在「指控读的是另一版本」的余地——指控描述的那个版本在当前树中不存在。'}]


## 42 项生产发布审计原文

来源：历史临时文件 audit2-full.md，2026-09-05 整理入库。原始绝对路径已改为仓库路径文本；历史行号仅用于追溯。

生产发布审计结论
发布判定：No-Go，当前版本不应作为正式生产产品发布。
按独立根因合并后，共确认 42 项生产问题。其中最需要阻断发布的是：

* 正式安装包允许未签名发布，也不验证真实签名、公证状态。
* 上传提交失败时可能先删除线上原文件，随后重命名仍失败，造成不可恢复的数据丢失。
* 断点续传不验证临时文件身份，可能把两个版本拼成一个损坏文件。
* 传后校验发生在最终文件已经替换之后，校验失败只能报警，无法保护旧文件。
* 大量 SFTP/校验操作没有超时，取消和退出不能保证停止后台 I/O。
* 多进程和路径别名可绕过传输目标锁。
* keyboard-interactive 禁用开关可被已配置密码绕过。
* Vault 缺少用户可达的删除、轮换、备份和恢复能力。
* README 的产品能力描述与实际 Phase 1/MVP 实现明显不一致。

这里的 P0/P1/P2 是发布决策严重度，不是 CVSS 等漏洞评分。
一、发布与供应链

1. [P0] 未签名产物仍能进入正式 Release。
`.github/workflows/release.yml:158`、`.github/workflows/release.yml:206`、`.github/workflows/release.yml:276`
Windows/macOS 签名 secret 缺失时只写 job summary，不会使构建或 draft release 失败。人工仍可直接发布这些产物。工作流也没有运行 `signtool verify`、`codesign --verify`、notarization/stapling 验证；所谓“已签名”只是根据 secret 是否存在推断。生产影响是用户无法确认发布者身份，更新链路和安装信誉不可审计。
2. [P0] 版本一致性检查明确没有实现。
`.github/workflows/release.yml:33`、`.github/workflows/release.yml:303`
tag、Cargo workspace、Tauri、前端包以及数据库 `APP_VERSION` 依赖人工核对。当前虽然都是 `0.1.0`，但以后错误 tag 可以静默产出内部版本不一致的安装包，影响升级、回滚、问题定位和安全响应。
3. [P1] 缺少 SBOM、provenance 和前端漏洞门禁。
`.github/workflows/release.yml:22`、`.github/workflows/ci.yml:79`
CI 有 Cargo 侧检查，但没有生成 SBOM、构建来源证明，也没有 `npm audit`、OSV 或同等前端依赖门禁。本次只读执行 `npm audit --json` 和 `npm audit --omit=dev --json`，在 2026-08-12 查询时均为零漏洞；这只是时点结果，不会阻止未来新增已知漏洞的依赖进入正式产物。
4. [P1] 构建工具链和托管 runner 会随时间漂移。
`rust-toolchain.toml:1`、`.github/workflows/ci.yml:28`、`.github/workflows/release.yml:74`、`.github/workflows/release.yml:154`
Rust 使用 `stable`，Windows/macOS 使用 `*-latest`，Tauri CLI 安装约束为 `^2.11`。相同源码在不同日期可能得到不同编译器、SDK、系统库和打包工具，难以复现、回滚和验证历史产物。
5. [P0] 没有最终安装包 smoke test。
`.github/workflows/release.yml:158`、`.github/workflows/release.yml:276`
工作流只构建和上传，没有对最终 MSI/NSIS、DMG、AppImage/deb 等真实安装、启动、基础连接、卸载和覆盖升级进行验证。编译成功不能证明安装包可安装、依赖齐全或升级不会破坏用户数据。
6. [P1] 多项已知 Rust 风险被显式忽略。
`deny.toml:20`
包括 `RUSTSEC-2023-0071`、GTK3 unmaintained 链及其他维护状态风险。忽略并非一定代表当前路径可利用，但正式发布前需要逐项证明不可达、平台限定或形成有期限的风险接受；目前 Linux 仍公开分发相关 GTK3 链。
7. [P1] 缺少第三方许可交付材料。
`LICENSE:1`、`package-lock.json:1`
仓库只有项目许可证，没有发现安装包随附的 `THIRD_PARTY_NOTICES`、第三方许可证清单或自动生成流程。前端依赖涉及 MIT、Apache-2.0、MPL-2.0、BlueOak、CC0、ISC、BSD 等；Cargo 侧白名单检查也不会自动生成交付材料。是否构成具体法律违规取决于最终打包内容和各依赖条款，但生产发布的合规门禁目前缺失。

二、传输与数据完整性

8. [P0] 上传提交失败时可能删除原目标文件。
`crates/sshengine/src/transfer.rs:638`
第一次 `rename` 失败后，只要临时件还能 `stat`，代码就删除最终目标再重试。由于错误已经折叠成字符串，无法确认失败是否真的因为“目标已存在”；权限变化、连接异常和服务端错误也会进入删除路径。第二次重命名失败时，线上旧文件已经不可恢复。
9. [P0] 断点续传只比较临时文件长度，不验证内容身份。
上传见 `crates/sshengine/src/transfer.rs:553`，下载见 `crates/sshengine/src/transfer.rs:588`。
同路径 `.fspart` 可能来自旧任务、旧版本文件或另一进程。只要长度不超过当前源文件，就会从该偏移继续写入，形成“旧前缀 + 新后缀”。校验关闭或降级为 size-only 时，损坏文件可以静默提交。
10. [P1] 校验发生在最终文件已经替换之后。
提交见 `crates/sshengine/src/transfer.rs:497`，校验调度见 `app/src/commands/sftp_cmd.rs:390`。
`Done` 后才执行校验。即使随后得到 `VerifyOutcome::Mismatch`，旧最终文件也已经被覆盖。因此当前“传后校验”只能发现错误，不能作为阻止错误内容上线的提交闸门。
11. [P1] 大量 SFTP I/O 和校验 exec 没有总体超时。
`crates/sshengine/src/sftp.rs:69`、`app/src/state.rs:315`、`app/src/state.rs:589`、`crates/sshengine/src/transfer.rs:471`、`crates/sshengine/src/transfer.rs:737`
list/stat/read/write/truncate/rename、SFTP 子系统建立以及校验 exec 创建都可能无限等待。底层请求不返回时，三次重试、任务取消和退出时的五秒宽限均无法保证生效；后台 worker 可长期持有本地文件句柄及连接对象。
12. [P1] 传输目标锁仅在单进程内有效。
`crates/sshengine/src/transfer.rs:128`、`app/src/lib.rs:46`
`TARGET_LOCKS` 是进程内状态，而应用没有单实例约束或跨进程文件锁。启动两个 FutureShell 进程后，可以同时写同一下载目标或远端 `.fspart`，导致交错写入、互相截断或双重提交。
13. [P1] 主机和路径别名能够绕过同目标锁。
`app/src/state.rs:532`、`crates/sshengine/src/transfer.rs:195`
DNS 名、IP、CNAME，远端相对/绝对路径及软链接不能统一到同一物理对象。用户通过不同别名连接到同一个文件时，可绕过锁并发写入。
14. [P1] 下载路径清洗存在确定性的文件名碰撞。
`app/src/commands/sftp_cmd.rs:106`、`crates/sshengine/src/sandbox.rs:11`
例如远端同目录的 `a:b` 和 `a?b` 都会映射为本地 `a_b`。Windows/macOS 还存在大小写等价碰撞。并发下载时一条任务可能报“目标忙”，顺序下载时后一个文件会覆盖前一个文件。
15. [P1] 传输期间源文件变化检测不完整。
`crates/sshengine/src/transfer.rs:821`
当前主要检测上传源变短和下载提前 EOF。同长度原地改写、文件增长、已经读取的前缀被修改均可能生成混合时间点快照；校验关闭或降级时无法可靠发现。
16. [P1] 下载临时文件没有显式设置 Unix `0600`。
`crates/sshengine/src/transfer.rs:626`
`.fspart` 的权限取决于进程 umask，常见配置下可能是 `0644`。下载内容可能包括私钥、数据库、配置或其他敏感文件，传输期间便可能被同机其他用户读取。
17. [P2] UI 允许选择目录删除，但后端只调用文件删除。
`frontend/src/components/SftpPane.svelte:157`、`frontend/src/components/SftpPane.svelte:317`、`crates/sshengine/src/sftp.rs:180`
用户选择目录后执行删除必然失败，空目录也没有删除路径。属于正式 SFTP 产品的功能完整性缺陷。
18. [P2] Profile 中的 SFTP 默认目录是死配置。
`frontend/src/components/ProfileDialog.svelte:94`、`crates/connmgr/src/repo.rs:151`、`frontend/src/components/SftpPane.svelte:4`
`local_dir`、`remote_dir` 可以编辑和保存，但 Rust 侧明确没有消费者；面板仍固定从本地 home 和远端 `.` 起步。用户配置不会生效。

三、SSH 身份、认证与主机信任

19. [P1] “关闭 keyboard-interactive”可被密码配置绕过。
`crates/sshengine/src/auth.rs:103`
`KbdInteractive` 的可用条件是 `kbd_interactive || password.is_some()`。因此即使用户将 `allow_kbd_interactive=false`，只要 Profile 绑定了密码，服务端通告 keyboard-interactive 后仍会进入该方法并弹出提示。这是明确的安全控制失效。
20. [P1] Vault 密钥用途靠明文内容猜测，而不是 `SecretKind`。
`app/src/commands/vault_cmd.rs:179`、`crates/sshengine/src/connect.rs:806`
未知 kind 会静默映射为 `ApiKey`；认证层不读取存储记录的类型，只检查明文是否包含 `"PRIVATE KEY"`。合法密码/API Key 包含该字符串会被误当私钥，反过来 API Key 也可能被作为 SSH 密码发送。IPC 和凭据用途边界没有 fail-closed。
21. [P1] 主机密钥信任按原始 host 字符串精确匹配。
`crates/sshengine/src/connect.rs:47`、`crates/sshengine/src/connect.rs:335`、`crates/sshengine/src/hostkey.rs:238`
DNS 大小写、尾点和等价 IPv6 文本没有规范化。`Example.COM`、`example.com.` 等会被视为不同主机；Strict 模式会误拒绝，TOFU 模式则会重复建立信任，增加用户接受错误密钥的机会。
22. [P1] `known_hosts` 导入不兼容多项 OpenSSH 语义。
`crates/sshengine/src/hostkey.rs:319`
hashed hostname、`@revoked`、`@cert-authority` 会被跳过；通配符、否定模式等 host pattern 没有实现，可能按字面字符串落库。导入没有对整个文件使用单一事务，中途数据库错误会留下部分导入；文件大小和行数也没有限制。
23. [P2] 跳板链缺少普通用户可达的完整编辑入口。
后端实现见 `crates/sshengine/src/connect.rs:241`，UI 见 `frontend/src/components/ProfileDialog.svelte:137`。
UI 不能创建或编辑 jump，只会透传已有值；导入还会剥离逐跳凭据和信任断言。后端能力存在，但普通用户无法构造完整、安全、可维护的跳板配置。
24. [P2] SSH Agent UI 文案与实际能力不一致。
`frontend/src/components/ProfileDialog.svelte:324`
UI 写的是“允许 Agent 转发”，实际只是使用本地 agent 执行客户端公钥认证，并未实现 SSH agent forwarding。用户可能错误认为远端能够使用本地 agent，从而形成错误的运维和安全预期。
25. [P1] Agent 存活探测自身没有超时。
`crates/sshengine/src/agent.rs:38`、`crates/sshengine/src/connect.rs:613`
`request_identities().await` 无超时。后续真正认证的签名请求虽然有超时，但半失效的 agent 可以在准备阶段无限挂起，导致连接流程无法按预期取消或失败返回。
26. [P2] 密码认证优先于 private key/agent。
`crates/sshengine/src/auth.rs:103`
Profile 同时配置密码和密钥/agent、服务器同时通告多种方法时，客户端先发送密码。它不是协议漏洞，但会扩大口令暴露面，并可能提前消耗服务端 `MaxAuthTries`，与企业常见的“公钥优先、密码兜底”策略不一致。

四、Vault 与本地敏感数据

27. [P1] Vault 没有用户可达的记录删除和改密/轮换入口。
Core 已实现 `crates/vault/src/store.rs:536` 和 `crates/vault/src/store.rs:755`，但 Tauri handler 仅暴露 `app/src/lib.rs:95` 所列操作。
用户不能撤销单条凭据、清理废弃密钥或轮换应用密码。泄露凭据或人员离职场景下缺少基本的秘密生命周期管理。
28. [P1] “应用密码可选”可能导致 Vault 永久不可恢复。
`frontend/src/components/VaultDialog.svelte:68`、`app/src/commands/vault_cmd.rs:148`、`crates/vault/src/store.rs:651`
未设置 passphrase 时，主密钥只依赖 OS keyring。换机、系统重装或凭据管理器被清理后，现有 `vault.json` 没有恢复路径；产品也没有 Vault 导出、备份和恢复机制。UI 没有充分提示这一不可逆风险。
29. [P1] Profile 新录入的秘密在取消或关闭后不会立即清理。
`frontend/src/components/ProfileDialog.svelte:184`、`frontend/src/App.svelte:831`
只有保存成功才清空 `newSecret`。点击取消、X、Escape、背景关闭、收起表单或保存失败时，明文仍保存在常驻组件的 WebView 状态中，可能持续到下一次覆盖或进程退出。
30. [P2] VaultDialog 关闭后密码仍保留在组件状态。
`frontend/src/components/VaultDialog.svelte:12`
`pass1/pass2` 只在下一次 `open=true` 时重置。取消或 Escape 后，密码仍留在 WebView 内存中直到再次打开或程序退出，不符合最短明文驻留原则。
31. [P1] SQLite、备份和日志没有显式收紧 Unix 权限。
`crates/connmgr/src/db.rs:19`、`crates/connmgr/src/db.rs:69`、`app/src/lib.rs:26`
Vault 文件自身已使用 `0600`，但 `fs.db`、WAL、备份、日志目录及文件未显式设置 `0600/0700`。在常见 umask 下可能出现目录 `0755`、文件 `0644`。数据库包含主机、用户名、环境变量、配置和 Vault record ID；日志包含连接目标与错误元数据。
32. [P1] 日志文件可能无限增长。
`app/src/lib.rs:30`
`tracing_appender::rolling::Builder::new()` 默认使用 `Rotation::NEVER`。代码虽然设置 `max_log_files(7)`，但没有配置 rotation，实际形成单个永不轮转的 `futureshell.log`。长时间运行或开启详细 `RUST_LOG` 后可能耗尽磁盘。
33. [P2] 启动错误文件使用共享临时目录中的固定文件名。
`app/src/lib.rs:162`
路径固定为 `temp_dir()/future-shell-startup-error.txt`，使用普通 `std::fs::write`，Unix 上未显式设置 `0600`。内容通常是路径和错误元数据而非 Vault 明文，但在共享临时目录中存在信息泄露、符号链接和覆盖风险。
34. [P2] SQLite 使用 `synchronous=Normal`，耐久性决策未被明确接受。
`crates/connmgr/src/db.rs:30`
该设置通常能保持数据库结构安全，但突然掉电时可能丢失最近已提交的数据。对于连接配置、设置和安全状态是否允许这种耐久等级，目前没有发布文档、测试或风险接受说明。

五、输入边界、资源控制和死配置

35. [P2] Profile 的终端滚动行数设置完全不生效。
保存端见 `frontend/src/components/ProfileDialog.svelte:91`，终端硬编码见 `frontend/src/lib/term.ts:292`、`crates/terminal/src/pipe.rs:64`。
xterm 始终使用 `scrollback: 10000`，headless grid 也使用固定默认值。用户看到配置被保存，但实际运行行为不变。
36. [P2] `kbd_auto_answer_single` 是死配置。
`crates/connmgr/src/model.rs:55`、`frontend/src/components/ProfileDialog.svelte:326`
UI 可勾选并持久化，但当前认证流程没有消费方。安全相关开关显示可用却不起作用，会造成错误配置预期。
37. [P2] Profile 和导入入口缺少有效校验及资源上限。
`frontend/src/components/ProfileDialog.svelte:104`、`app/src/commands/conn_cmd.rs:59`、`frontend/src/App.svelte:640`、`app/src/commands/conn_cmd.rs:185`
后端未统一校验 name/host/username 非空、端口非零、字段长度、jump/env/pin 数量、环境变量长度、字号和滚动行数范围。JSON 与 `known_hosts` 导入使用 `file.text()` 全量载入，缺少文件大小和记录数量限制。异常输入可造成不可用 Profile、内存峰值或数据库膨胀。
38. [P2] SFTP 目录列表没有分页或条目上限。
`crates/sshengine/src/sftp.rs:80`、`app/src/commands/sftp_cmd.rs:81`、`frontend/src/components/SftpPane.svelte:43`
后端全量收集，IPC 全量序列化，前端全量排序和渲染。遇到包含数十万条目的目录时可能产生明显 CPU、内存占用和 UI 卡死。
39. [P2] terminal input IPC 没有请求大小和等待超时边界。
`app/src/commands/session_cmd.rs:235`
输入字符串会全量 base64 解码，缺少最大长度；获取写锁及 `data_bytes().await` 也无超时。普通键盘输入很小，但大型粘贴或异常 WebView 调用可以制造大分配并长时间占用会话写路径。
40. [P2] 通用设置接口接受任意 key/value。
`app/src/commands/settings_cmd.rs:19`、`crates/connmgr/src/settings_repo.rs:27`、`frontend/src/lib/ipc.ts:36`
没有 key 白名单、运行期 schema、长度或总量限制。前端泛型只做 TypeScript 断言，不验证持久化 JSON 的实际结构；损坏或越界值进入数据库后，各消费者会产生不一致的回退行为。

六、产品范围、宣传与验收证据

41. [P0/范围阻断] README 宣称“完全体并超越”，实际实现是 Phase 1 MVP。
`README.md:3`；多个 crate 仍只有占位结构：`crates/ai/src/lib.rs:1`、`crates/policy/src/lib.rs:1`、`crates/mcpbridge/src/lib.rs:1`、`crates/audit/src/lib.rs:1`。AI 菜单和监控仍禁用或占位：`frontend/src/lib/menus.ts:70`、`frontend/src/App.svelte:782`。
如果按 README 的“完整替代并超越”宣传验收，这是直接的发布阻断；如果只发布 SSH/SFTP MVP，必须重新界定产品名称、能力矩阵和发布说明。
42. [P1] 性能和资源目标没有形成发布门禁。
`README.md:103`、`crates/itest/tests/perf.rs:1`
性能测试均为 `#[ignore]`，依赖 GUI 和预构建产物，测试文件本身也声明不能作为发布证据。当前没有长连接、高吞吐、大目录、海量小文件、内存占用、冷启动、断网恢复和安装包兼容性的稳定基线。

建议的发布门槛
在不讨论具体改法的前提下，至少应满足以下条件后再重新评估生产发布：

1. 解决第 8、9、10 项传输提交与续传数据完整性问题。
2. 为所有远程 I/O、校验、agent 探测建立可验证的超时和退出语义。
3. 防止多进程以及端点/路径别名并发写同一目标。
4. 修复 keyboard-interactive 开关和 Vault `SecretKind` 边界。
5. 补齐 Vault 删除、轮换、备份、恢复和明文生命周期。
6. 正式产物强制签名，并验证真实签名、公证和 stapling。
7. 实现版本一致性检查、SBOM、provenance、依赖漏洞门禁与许可证材料。
8. 对最终安装包执行安装、启动、升级、卸载和基础 SSH/SFTP smoke test。
9. 明确产品是受限 SSH/SFTP MVP，或完成 README 宣称的功能。
10. 将性能、资源和超大输入测试纳入不可绕过的发布门禁。

审计限制与工作树状态

* 本次是静态、只读审计。
* 为遵守“不改代码只读”，没有运行会生成或改写 `target/`、数据库、lockfile 或安装产物的构建与集成测试。
* 没有连接真实 SSH/SFTP 服务，没有做动态攻击、断网/掉电故障注入，也没有验证真实安装包。
* 唯一联网依赖查询是只读的 `npm audit`；当前结果为零漏洞，但不能替代持续 CI 门禁。
* 已排除若干旧问题：Vault 自动锁当前有实际消费方；认证提示在正常提交/取消和自动锁时会清除；Vault 原子临时文件在 Unix 已设置 `0600`；下载沙箱三级设置已有消费方。
* 最终 `git status --short --branch` 仅显示 `## fix/audit-remediation`，工作树干净。

本次审计未修改、格式化、创建或删除任何文件。


根据审计结果 逐条修复