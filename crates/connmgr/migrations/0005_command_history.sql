-- 本地命令历史库（M4a：历史命令 UI 的持久层）。
--
-- 为什么要一张表而不是复用 settings：历史是**增长的、要检索的**数据，塞进
-- settings 的单值 JSON 里既撑爆 8 KiB 的值上限，也没法按主机/时间/次数排序。
--
-- `host` 用 '' 而不是 NULL 表示未知：SQLite 的唯一索引把 NULL 当彼此不同，
-- 用 NULL 会让「同一条命令在未知主机上跑过 100 次」变成 100 行，去重直接失效。
CREATE TABLE command_history (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  command    TEXT    NOT NULL,
  -- 冗余存主机名而不是只存 profile_id：档案会被删改，而「这条命令在哪台机器上
  -- 跑过」是历史本身的事实，不该随档案消失。profile_id 另存供跳回连接用。
  host       TEXT    NOT NULL DEFAULT '',
  profile_id TEXT    NOT NULL DEFAULT '',
  -- 'sent' = 本程序亲手发出（字节精确）；'grid' = 从终端回滚里启发式提取（近似）。
  -- 两者必须可区分：前者可以直接重发，后者可能带上提示符残渣，UI 要如实标注来源。
  source     TEXT    NOT NULL DEFAULT 'sent',
  used_at    INTEGER NOT NULL,
  use_count  INTEGER NOT NULL DEFAULT 1
);

-- 去重键：同一命令 + 同一主机 + 同一来源只留一行，重复执行只更新时间与次数。
-- 不去重的历史库里 90% 是重复项，检索结果全被同一条 `ls` 占满。
CREATE UNIQUE INDEX command_history_uniq
  ON command_history (command, host, source);

-- 检索按「最近用过」排序，这是最常用的一路
CREATE INDEX command_history_used_at ON command_history (used_at DESC);
