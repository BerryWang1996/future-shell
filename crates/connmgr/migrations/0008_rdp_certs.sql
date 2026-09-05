-- RDP 服务器证书的 TOFU 信任表（阶段 1）。
--
-- 与 host_keys（SSH 主机密钥）同款语义、不同载体：RDP 的信任对象是
-- X.509 证书，锚定物是 **SHA-256 指纹**（证书会轮换、链会换，指纹是
-- 唯一稳定的「这台服务器此刻是谁」的证据）。
--
-- · (host, port) 至多一行的口径与 host_keys 相同：TOFU 的语义就是
--   「每台服务器记一把」，换证书 = 变更告警，不是追加第二把。
-- · 指纹由 helper 上报（TLS 握手后、认证前——裁决在秘密出线之前）。
CREATE TABLE rdp_certs (
  host TEXT NOT NULL,
  port INTEGER NOT NULL,
  fingerprint_sha256 TEXT NOT NULL,        -- 小写十六进制冒号分隔（与 SSH 指纹同款排版）
  subject TEXT NOT NULL DEFAULT '',
  issuer TEXT NOT NULL DEFAULT '',
  first_seen TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (host, port)
);

-- Profile 的协议字段（同批）：加列而不是并进 auth_blob——协议是**顶层路由**
-- 属性（决定走哪条会话栈），藏在认证子结构里会让每处「该建什么会话」的判定
-- 都多剥一层。默认 'ssh'：存量行零迁移平滑。
ALTER TABLE profiles ADD COLUMN protocol TEXT NOT NULL DEFAULT '"ssh"';
