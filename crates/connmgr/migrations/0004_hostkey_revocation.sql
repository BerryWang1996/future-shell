-- 审计2 #22：OpenSSH known_hosts 的 `@revoked` 标记行此前被整行丢弃（只计一个笼统的
-- skipped）。它的语义是「这把主机密钥永不得被接受」——通常意味着密钥已泄露或已被替换。
-- 丢掉它的后果不是少一条信息，而是**安全控制反转**：用户明确标记为吊销的密钥，在本客户端
-- 这里退化成一个友好的「首次连接，是否信任？」确认框。
--
-- 吊销记录必须与信任记录分表：`host_keys` 上钉着「每 (host, port) 至多一行」的不变式
-- （R13/R30），一条吊销记录若占住那唯一的槽位，就会把这台主机**真正的**密钥挤出去。
-- 吊销是黑名单，信任是白名单，两者数量关系完全不同（一台主机可以吊销任意多把密钥）。
CREATE TABLE revoked_host_keys (
  host TEXT NOT NULL,                        -- fs_connmgr::canonical_host 的输出（审计2 #21）
  port INTEGER NOT NULL,
  key_type TEXT NOT NULL,
  key_blob TEXT NOT NULL,                    -- base64 公钥
  fingerprint_sha256 TEXT NOT NULL,
  added TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (host, port, key_blob)
  -- 与 host_keys 相反：这里**不**限制每 (host, port) 一行。吊销只增不减，
  -- 同一台主机换过几把密钥就该有几条。`INSERT OR IGNORE` 借该 PK 实现幂等重导入。
);
