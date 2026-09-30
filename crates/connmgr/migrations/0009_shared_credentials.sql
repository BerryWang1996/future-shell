-- 共享凭据登记表（2026-08-28）。
--
-- # 这张表解决的问题
--
-- Vault 记录此前是**一个全局池**：连接属性里的凭据下拉框把所有记录摊开让
-- 用户挑。那个形状在诱导共享——凭据看起来是「先建池子再挑一条」，而不是
-- 「这个连接自己的密码」。用户要的是反过来：**默认每个连接一份自己的，
-- 想共享才主动抽出来**。
--
-- # 为什么不改 Vault 的记录结构
--
-- 「是否共享」是**应用层的组织方式**，不是秘密材料的属性：Vault 那侧的
-- 记录格式受 AAD 认证约束（改字段要动加密结构 + 迁移 + 备份兼容），
-- 而这里存的只是「哪些记录愿意被别的连接看见、显示成什么名字」——
-- 泄露它不泄露任何秘密，丢了它也只是回到「都不共享」的安全默认。
-- 放在应用库里，Vault 的格式一个字节都不用动。
--
-- # 与 Vault 的一致性
--
-- record_id 指向 vault 记录，但**不设外键**（跨存储，一个是加密文件一个是
-- SQLite）。列表接口一律拿本表与 `vault_list_secrets` 求交集：Vault 里已经
-- 删掉的记录不会因为本表还留着行而出现在选择器里。反向的孤儿行由
-- credential_unshare 与 profile_delete 清理，即便漏了也只是一行死数据。
CREATE TABLE shared_credentials (
  record_id INTEGER PRIMARY KEY,          -- vault 记录 id
  name TEXT NOT NULL,                     -- 用户起的名字（如「生产机 root」）
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
