-- 计划任务（M4a）：定时在远端执行命令 + 重启后补跑策略。
--
-- 两张表：任务定义 + 执行记录。执行记录不是「日志」而是**产品的一部分**——用户要能
-- 回答「昨晚那条备份到底跑了没有、为什么没跑」，而这个问题在没有记录时完全无法回答
-- （任务列表只能显示「上次触发时间」，跳过与失败都看不见）。
CREATE TABLE scheduled_tasks (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  name        TEXT    NOT NULL,
  -- 五段 cron（分 时 日 月 周），由 fs_connmgr::cron 解析。存原文而不存解析结果：
  -- 用户要能看回自己写的那一行，而解析结果（展开后的集合）人读不出原意。
  cron        TEXT    NOT NULL,
  command     TEXT    NOT NULL,
  -- 目标连接。存 profile_id 而不是 session_id：会话是临时的，任务是长期的。
  profile_id  TEXT    NOT NULL,
  enabled     INTEGER NOT NULL DEFAULT 1,
  -- 补跑策略：'skip' = 错过就算了（默认）；'once' = 错过多次也只补跑一次。
  -- 不提供「全部补跑」：应用关了三天再打开，一条每分钟的任务会瞬间排出 4320 次执行，
  -- 那不是补跑而是拒绝服务。
  catchup     TEXT    NOT NULL DEFAULT 'skip',
  -- 创建时定格的时区偏移（分钟，东为正）。
  --
  -- cron 的「03:30」必须是**人的墙上时钟**。偏移可以从系统按当前时区解析
  --（chrono 已随 russh-sftp 等进入依赖树，提成直接依赖是零新增 crate），也可以
  -- 固定存一个值（给另一时区的机器按它墙上时钟写 cron）。见下一列的开关。
  tz_offset_minutes INTEGER NOT NULL DEFAULT 0,
  -- 1 = 每一跳取**系统当前**本地偏移（跟随夏令时）；0 = 用上一列存的固定偏移。
  --
  -- 固定偏移不跟随 DST——DST 地区每年会差一小时，UI 在这个开关旁写明这一点。
  -- 默认 0（固定）：任务的判定在进程重启前后完全可复现，不受运行机器的时区设置
  -- 影响；跟随系统是一个显式选择。
  tz_follows_dst INTEGER NOT NULL DEFAULT 0,
  -- **评估水位**：到这一分钟为止的触发点都已处置完毕（跳过也算处置）。只升不降。
  --
  -- 与 last_fire_minute 分开存（初版曾混用一个量）：评估水位回答「还有没有没看过的
  -- 触发点」，上次触发回答「任务列表该显示什么」。合用时，Skip 策略要么不能推水位
  -- （留痕每 2 秒重复落一条），要么推了它（「上次触发」显示一个从未触发过的时刻）。
  checked_minute INTEGER NOT NULL DEFAULT 0,
  -- 最近一次**真实触发**的分钟序号（unix 秒 / 60）。纯展示用，不参与判定。
  last_fire_minute  INTEGER,
  created_at  INTEGER NOT NULL
);

CREATE INDEX scheduled_tasks_enabled ON scheduled_tasks (enabled);

-- 执行记录。删任务时一并删掉它的记录（ON DELETE CASCADE 需要 PRAGMA foreign_keys=ON，
-- 本仓未统一开启，故由仓库层显式删——见 ScheduleRepo::delete 的注释）。
CREATE TABLE scheduled_runs (
  id       INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id  INTEGER NOT NULL,
  -- 触发时刻（unix 秒）
  fired_at INTEGER NOT NULL,
  -- 'ok' 远端 exit 0 ｜ 'failed' 远端非零或通道错 ｜ 'skipped' 压根没跑
  -- 三态必须分开：'skipped' 与 'failed' 对用户是两件完全不同的事（前者是「没有会话」
  -- 这类环境问题，后者是命令本身的问题），合成一个「未成功」等于把唯一的线索抹掉。
  outcome  TEXT    NOT NULL,
  -- 是否是补跑（UI 上要能看出「这次是补的」）
  catchup  INTEGER NOT NULL DEFAULT 0,
  -- 远端退出码；没跑起来时为 NULL（**不是 0**——0 是「成功」这个有意义的观测值）
  exit_code INTEGER,
  -- 原因/输出摘要：跳过原因、stderr 首段。截断由仓库层做，见 RUN_DETAIL_CHARS_MAX。
  detail   TEXT    NOT NULL DEFAULT ''
);

CREATE INDEX scheduled_runs_task ON scheduled_runs (task_id, fired_at DESC);
