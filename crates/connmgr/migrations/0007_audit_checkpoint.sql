-- 审计链的周期性快照（M3 出口 7 的 ③）。
--
-- **这张表不是信任锚。** 它与 audit 同库同权限、同一把写锁——能改 audit 的人
-- 也能改它。把它当成「审计更难篡改了」是错的，那种错会让人少查一遍。
-- 它买到的只有两件事：
--   ① 性能：校验从最后一个快照往后跑，而不是每次 O(全表)；
--   ② 一份异地副本：链尾 hash 多存了一处，篡改者要把两处改得互相自洽才不露馅
--      （抬高了成本，不构成证明）。
--
-- append-only 与 audit 同样用触发器钉住：快照的语义是「我在时刻 T 查过，到这一行
-- 为止是好的」。允许 UPDATE 就等于允许把一次失败的校验事后改成成功的。
CREATE TABLE audit_checkpoint (
  id         INTEGER PRIMARY KEY,
  row_id     INTEGER NOT NULL, -- 覆盖到 audit 的这一行为止（含）
  row_hash   TEXT    NOT NULL, -- 该行当时的 hash——锚点，增量校验必须先对上它
  row_count  INTEGER NOT NULL, -- 到该行为止的总行数（抓「前段被整体删除」）
  created_at TEXT    NOT NULL
);

CREATE TRIGGER audit_checkpoint_no_update BEFORE UPDATE ON audit_checkpoint BEGIN SELECT RAISE(ABORT,'audit_checkpoint append-only'); END;
CREATE TRIGGER audit_checkpoint_no_delete BEFORE DELETE ON audit_checkpoint BEGIN SELECT RAISE(ABORT,'audit_checkpoint append-only'); END;
