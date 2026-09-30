use crate::model::*;
use crate::Error;
use sqlx::SqlitePool;
use uuid::Uuid;

pub struct ProfileRepo<'a> {
    pool: &'a SqlitePool,
}

/// P2：`list()` 跳过的那一行到底是哪一行、为什么坏——必须能被调用方拿到。
///
/// 只把坏行写进日志等于没写：桌面端用户不看 stdout，他看到的只有「我那条生产机配置
/// 凭空消失了」。侧栏因此可以在列表末尾挂一条「N 条配置无法读取」的提示，点开给出
/// id 与原因，用户至少知道该去恢复备份、还是该手工修一列 blob。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadRow {
    /// 库里那一列 `id` 的**原始文本**。坏行的成因之一就是 id 本身不是合法 uuid，
    /// 所以这里不能是 `Uuid`——一旦解析失败就再也指不出「是哪一行」了。
    pub id: String,
    /// 人可读的失败原因（哪一列、坏在哪）。
    pub error: String,
}

impl<'a> ProfileRepo<'a> {
    pub fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn upsert(&self, p: &Profile) -> Result<(), Error> {
        upsert_on(self.pool, p).await
    }

    /// S17（med）：导入必须**全成功或全不落**。历史实现逐行 upsert 无事务包裹——
    /// 中途任何一行失败（磁盘满、约束冲突、进程被杀）都会留下一份「导入了一半」的连接列表，
    /// 用户看到的是残缺配置，且无从判断缺了哪些。
    async fn upsert_all(&self, items: &[Profile]) -> Result<(), Error> {
        let mut tx = self.pool.begin().await?;
        for p in items {
            upsert_on(&mut *tx, p).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn get(&self, id: Uuid) -> Result<Profile, Error> {
        let row = sqlx::query_as::<_, Row>(
            r#"SELECT id, name, group_path, host, port, username, protocol AS protocol_blob, auth_blob, jump_blob,
                      host_key_policy, host_key_pins, env_blob, term_blob, sftp_blob, ai_policy_blob, serial_blob
               FROM profiles WHERE id = ?1"#,
        )
        .bind(id.to_string())
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| Error::NotFound(id.to_string()))?;
        row.into_profile()
    }

    /// 好行列表。坏行被跳过——需要知道「跳了哪些」时用 [`Self::list_with_diagnostics`]。
    ///
    /// 签名保持不变是有意为之：侧栏、导出等既有调用方全都只关心好行，
    /// 没必要为了诊断面而被迫改写（也避免这轮整改扩散到别人的文件里）。
    pub async fn list(&self) -> Result<Vec<Profile>, Error> {
        Ok(self.list_with_diagnostics().await?.0)
    }

    /// P2：好行 + **坏行诊断**。
    ///
    /// S18 定下的「一行坏数据不得让整张连接列表消失」仍然成立（列表页的可用性优先于
    /// 完整性），但历史实现把坏行降级成一条 `warn` 就扔了——对着 GUI 的用户而言，
    /// 那条配置就是**凭空消失**，既没有提示，也无从判断是自己删过还是库坏了。
    /// 所以坏行不再只进日志，而是作为返回值的一等公民交给调用方去呈现。
    ///
    /// `get()` 对同一条仍旧如实报 `CorruptRow`：从诊断里点进去要看到真正的原因，
    /// 而不是「查无此条」这种误导。
    pub async fn list_with_diagnostics(&self) -> Result<(Vec<Profile>, Vec<BadRow>), Error> {
        let rows = sqlx::query_as::<_, Row>(
            r#"SELECT id, name, group_path, host, port, username, protocol AS protocol_blob, auth_blob, jump_blob,
                      host_key_policy, host_key_pins, env_blob, term_blob, sftp_blob, ai_policy_blob, serial_blob
               FROM profiles ORDER BY group_path, name"#,
        )
        .fetch_all(self.pool)
        .await?;
        let mut good = Vec::with_capacity(rows.len());
        let mut bad = Vec::new();
        for r in rows {
            // `into_profile` 按值消费，而坏行的 id 恰恰是诊断里最要紧的一格，先留一份。
            let raw_id = r.id.clone();
            match r.into_profile() {
                Ok(p) => good.push(p),
                Err(e) => {
                    // `CorruptRow` 的 Display 已含 id，单独取 reason 免得诊断里 id 出现两遍；
                    // 其它错误（理论上到不了这里）则原样带上，宁可啰嗦也不要丢信息。
                    let reason = match &e {
                        Error::CorruptRow { reason, .. } => reason.clone(),
                        other => other.to_string(),
                    };
                    // 用户数据读不出来是 error 不是 warn：warn 在默认过滤级别下常被吞掉，
                    // 而这条恰恰是「配置消失」类工单唯一的线索。
                    tracing::error!(row_id = %raw_id, "profiles 行损坏，已从列表跳过：{reason}");
                    bad.push(BadRow {
                        id: raw_id,
                        error: reason,
                    });
                }
            }
        }
        Ok((good, bad))
    }

    pub async fn delete(&self, id: Uuid) -> Result<(), Error> {
        sqlx::query("DELETE FROM profiles WHERE id = ?1")
            .bind(id.to_string())
            .execute(self.pool)
            .await?;
        Ok(())
    }

    pub async fn export_json(&self) -> Result<String, Error> {
        Ok(serde_json::to_string_pretty(&self.list().await?)?)
    }

    pub async fn import_json(&self, s: &str) -> Result<usize, Error> {
        // 审计2 #37：内容经 IPC 全量到达（前端 file.text() 读进来的），先量尺寸再解析。
        // 上限不是为了防恶意攻击者——他自己选的文件爱多大都行——是不让一次异常导入
        // 把进程内存打出一个峰值。解析前的 `s.len()` 是零成本的闸。
        if s.len() > limits::IMPORT_MAX_BYTES {
            return Err(Error::Validation(format!(
                "导入文件超过 {} 字节上限",
                limits::IMPORT_MAX_BYTES
            )));
        }
        let mut items: Vec<Profile> = serde_json::from_str(s)?;
        if items.len() > limits::IMPORT_MAX_RECORDS {
            return Err(Error::Validation(format!(
                "导入记录数超过 {} 条上限",
                limits::IMPORT_MAX_RECORDS
            )));
        }
        // ── 导入剥离不变量（R113 / S9 / R2-1）─────────────────────────────────────
        // profiles.json 是一份**完全由外部提供**的文件（邮件附件、同事分享、恶意页面下载）。
        // 它可以合法承载「连去哪、用哪个用户名、终端怎么配」这类**偏好**，但绝不能承载
        // 下面两类东西——两类都不是偏好，而是本机才有资格作出的判断：
        //
        //   ① **指向本机秘密的引用**（`vault_record`、`passphrase_vault_record`）。
        //      它们是整数记录 id，导出面恒不含所指明文，于是「导出不得含明文秘密」那条
        //      断言完全挡不住它们；照单全收就等于让文件指定「登录时去 vault 里取哪一条」，
        //      一份构造过的 JSON 即可把受害者自己的口令/私钥经 SecretSource 取出、
        //      送往攻击者的主机（总设计 §3.2：秘密只经 vault 引用 → 引用归属校验必须落在导入面）。
        //   ② **身份断言**（`host_key_policy`、`host_key_pins`，Profile 级与**每一跳**）。
        //      指纹钉的全部意义在于「本机亲眼见过并确认过这台机器的公钥」。由文件预置的钉
        //      是攻击者的自证：导入后首连不再弹 TOFU 确认，中间人当场生效，而界面上显示的
        //      恰恰是「已按钉扎策略校验」——比不校验更糟（总设计 §2.1 主机密钥验证）。
        //   ③ **本地写边界授权**（`sftp.download_sandbox`）。它是 spec §3.3 下载沙箱的
        //      **最高优先级**输入（`app/state.rs::download_sandbox_for` 第 ① 级，压过全局设置
        //      与默认目录），也就是「远端服务器送来的字节允许落在本机哪棵目录树下」这个
        //      判断的唯一来源。一份 JSON 里写上 `"download_sandbox": "C:/Users/x/.ssh"`，
        //      导入后从那台（攻击者控制的）主机下载一个名为 `authorized_keys` 的文件即可落地——
        //      全程不触发任何越权告警，因为在引擎看来这就是用户配的沙箱，一切合规。
        //      与 ①② 同源：写边界是本机才有资格作的授权，不是可随文件旅行的偏好。
        //
        // 故一律归零：凭据回落「未绑定」，由用户在 ProfileDialog 内重新选；信任回落 TOFU，
        // 下次连接照常弹首见确认，信任在本机重新建立一次；沙箱回落全局设置 / 默认下载目录，
        // 需要专设的用户在 ProfileDialog 里自己填一次。代价只是几次点击，收益是
        // 「外部文件永远无法替本机作信任与授权决定」。
        //
        // **不剥 `sftp.local_dir` / `sftp.remote_dir`**：这条边界划在「是否构成本机授权」上，
        // 不划在「是否属于 sftp 段」上。这两项目前在 Rust 侧无任何消费者（只被 ProfileDialog
        // 读写显示，作为下次打开面板时的起始目录），既不授权写入、也不指向秘密、更不断言身份，
        // 与 `host`/`username`/`term` 同属可随文件旅行的偏好。顺手一并剥了看着「更安全」，
        // 实则把不变量从一条可判定的规则退化成一句「sftp 段大概都不太可信」——
        // 下一个往 SftpDefaults 加字段的人就再也读不出该不该剥了。
        //
        // **维护铁律**：今后往 `Profile` / `JumpHop` / `AuthRef` / `SftpDefaults` 新增**任何**
        // 指向本机秘密的引用字段（xxx_vault_record、keyring 句柄、凭据别名……）、身份断言字段
        //（policy、pin、fingerprint、已信任标记……）或本机资源授权字段（沙箱根、可写目录白名单、
        // 允许执行的命令集……），都必须同步加进下面这个循环，并在 tests/repo.rs 里补一条对应的
        // 归零断言。历史上这条已被漏过三次：第一次漏了跳板链（只剥了 Profile 级），第二次漏了
        // `passphrase_vault_record` 与逐跳 policy/pins，第三次漏了 `sftp.download_sandbox`
        // ——三次都是「新字段绕开了同源防护」，而不是防护本身写错了。加字段时不改这里，就是第四次。
        for p in items.iter_mut() {
            // 避免 id 冲突
            p.id = Uuid::new_v4();

            // ① 凭据引用：Profile 级
            p.auth.vault_record = None;
            p.auth.passphrase_vault_record = None;
            // ② 身份断言：Profile 级
            p.host_key_policy = HostKeyPolicy::default();
            p.host_key_pins.clear();
            // ③ 本地写边界授权（local_dir / remote_dir 是纯偏好，见上，故意保留）
            p.sftp.download_sandbox = None;

            // 跳板链每一跳同剥。跳板不是「次要的一跳」——它是整条链上**第一个**拿到你流量的
            // 节点，逐跳字段被绕过与 Profile 级被绕过等价危险，只是换了个位置。
            for h in p.jump.iter_mut() {
                h.auth.vault_record = None;
                h.auth.passphrase_vault_record = None;
                h.host_key_policy = None; // None = 未配置 → 连接层回落 TOFU（见 model.rs）
                h.host_key_pins.clear();
            }
        }
        // 审计2 #37：剥离后逐条过统一校验（与 profile_save 同一套规则，两条入口不可能漂移）。
        // 整份拒绝而非跳行：导入是「全成功或全不落」（S17），而「跳过坏行照导其余」
        // 会让用户对着「导入 N 条」以为 N 条都可用。文案带上行号，改哪条一目了然。
        for (i, p) in items.iter().enumerate() {
            p.validate()
                .map_err(|why| Error::Validation(format!("第 {} 条：{why}", i + 1)))?;
        }
        self.upsert_all(&items).await?;
        Ok(items.len())
    }
}

/// upsert 的执行器泛型版：`&SqlitePool` 与事务 `&mut SqliteConnection` 共用同一条 SQL，
/// 避免「单条写」与「导入批写」两处语句各写一遍后悄悄漂移（列序错位正是 S5 漏检的那类缺陷）。
async fn upsert_on<'e, E>(ex: E, p: &Profile) -> Result<(), Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query(
        r#"INSERT INTO profiles
           (id, name, group_path, host, port, username, protocol, auth_blob, jump_blob,
            host_key_policy, host_key_pins, env_blob, term_blob, sftp_blob, ai_policy_blob,
            serial_blob, updated_at)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16, datetime('now'))
           ON CONFLICT(id) DO UPDATE SET
             name=?2, group_path=?3, host=?4, port=?5, username=?6, protocol=?7,
             auth_blob=?8, jump_blob=?9, host_key_policy=?10, host_key_pins=?11,
             env_blob=?12, term_blob=?13, sftp_blob=?14, ai_policy_blob=?15, serial_blob=?16,
             updated_at=datetime('now')"#,
    )
    .bind(p.id.to_string())
    .bind(&p.name)
    .bind(&p.group_path)
    .bind(&p.host)
    .bind(p.port as i64)
    .bind(&p.username)
    .bind(serde_json::to_string(&p.protocol)?)
    .bind(serde_json::to_string(&p.auth)?)
    .bind(serde_json::to_string(&p.jump)?)
    .bind(serde_json::to_string(&p.host_key_policy)?)
    .bind(serde_json::to_string(&p.host_key_pins)?)
    .bind(serde_json::to_string(&p.env)?)
    .bind(serde_json::to_string(&p.term)?)
    .bind(serde_json::to_string(&p.sftp)?)
    .bind(serde_json::to_string(&p.ai_policy)?)
    .bind(serde_json::to_string(&p.serial)?)
    .execute(ex)
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    name: String,
    group_path: Option<String>,
    host: String,
    port: i64,
    username: String,
    protocol_blob: String,
    auth_blob: String,
    jump_blob: String,
    host_key_policy: String,
    host_key_pins: String,
    env_blob: String,
    term_blob: String,
    sftp_blob: String,
    serial_blob: String,
    ai_policy_blob: String,
}

impl Row {
    fn into_profile(self) -> Result<Profile, Error> {
        // S18（med）：uuid 解析失败是「这一行坏了」，不是「查无此条」。映射成 NotFound 会让
        // 排障者去找一条其实躺在库里的记录，也让 list() 的失败原因彻底失真。
        let id = Uuid::parse_str(&self.id).map_err(|e| Error::CorruptRow {
            id: self.id.clone(),
            reason: format!("id 非合法 uuid：{e}"),
        })?;
        // S12（med）：`as u16` 会静默截断——库里 65537 会变成 1，程序照样连上去，只是连错端口。
        // 写路径本就是 u16 不会越界，但库文件可被外部工具改写，读路径必须自己把关。
        let port = u16::try_from(self.port).map_err(|_| Error::CorruptRow {
            id: self.id.clone(),
            reason: format!("port {} 越出 1..=65535", self.port),
        })?;
        let corrupt = |field: &str, e: serde_json::Error| Error::CorruptRow {
            id: self.id.clone(),
            reason: format!("{field} 非合法 JSON：{e}"),
        };
        Ok(Profile {
            id,
            name: self.name,
            group_path: self.group_path,
            host: self.host,
            port,
            username: self.username,
            protocol: serde_json::from_str(&self.protocol_blob)
                .map_err(|e| corrupt("protocol_blob", e))?,
            auth: serde_json::from_str(&self.auth_blob).map_err(|e| corrupt("auth_blob", e))?,
            jump: serde_json::from_str(&self.jump_blob).map_err(|e| corrupt("jump_blob", e))?,
            host_key_policy: serde_json::from_str(&self.host_key_policy)
                .map_err(|e| corrupt("host_key_policy", e))?,
            host_key_pins: serde_json::from_str(&self.host_key_pins)
                .map_err(|e| corrupt("host_key_pins", e))?,
            env: serde_json::from_str(&self.env_blob).map_err(|e| corrupt("env_blob", e))?,
            term: serde_json::from_str(&self.term_blob).map_err(|e| corrupt("term_blob", e))?,
            sftp: serde_json::from_str(&self.sftp_blob).map_err(|e| corrupt("sftp_blob", e))?,
            serial: serde_json::from_str(&self.serial_blob)
                .map_err(|e| corrupt("serial_blob", e))?,
            ai_policy: serde_json::from_str(&self.ai_policy_blob)
                .map_err(|e| corrupt("ai_policy_blob", e))?,
        })
    }
}
