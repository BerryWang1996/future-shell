CREATE TABLE profiles (
  id            TEXT PRIMARY KEY,            -- uuid
  name          TEXT NOT NULL,
  group_path    TEXT,
  host          TEXT NOT NULL,
  port          INTEGER NOT NULL DEFAULT 22,
  username      TEXT NOT NULL,
  auth_blob     TEXT NOT NULL,               -- JSON: AuthRef（vault 记录 id / 方法开关）
  jump_blob     TEXT NOT NULL DEFAULT '[]',  -- JSON: Vec<JumpHop>
  host_key_policy TEXT NOT NULL DEFAULT '"tofu"',   -- serde_json 格式，与 HostKeyPolicy 序列化值对齐
  host_key_pins TEXT NOT NULL DEFAULT '[]',  -- JSON: Vec<HostKeyPin>
  env_blob      TEXT NOT NULL DEFAULT '{}',
  term_blob     TEXT NOT NULL DEFAULT '{}',
  sftp_blob     TEXT NOT NULL DEFAULT '{}',
  ai_policy_blob TEXT NOT NULL DEFAULT '{}',
  created_at    TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE host_keys (
  host TEXT NOT NULL,
  port INTEGER NOT NULL,
  key_type TEXT NOT NULL,
  key_blob TEXT NOT NULL,                    -- base64 公钥
  fingerprint_sha256 TEXT NOT NULL,
  first_seen TEXT NOT NULL DEFAULT (datetime('now')),
  last_seen  TEXT NOT NULL DEFAULT (datetime('now')),
  source TEXT NOT NULL DEFAULT 'tofu',       -- tofu | imported | pinned
  PRIMARY KEY (host, port, key_blob)
  -- 应用层不变式：每 (host, port) 至多一行，record()/replace() 均以单事务保证（DELETE 同 host:port 旧行 + INSERT）；
  -- PK 含 key_blob 仅为导入冲突检测便利，不放松该不变式（R30）
);

CREATE TABLE session_journal (
  session_key TEXT PRIMARY KEY,              -- profile_id 或 host:port
  profile_id TEXT,
  closed_cleanly INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
INSERT INTO meta (key, value) VALUES ('min_app_version', '0.1.0');  -- spec §3.4.2：库声明的最低兼容 app 版本
