//! 第三方格式导入的 IPC 出口（M4b 出口第 1 条的界面半边）。
//!
//! 解析引擎在 `fs_connmgr::import_foreign`，本文件只做三件事：**认格式、预览、落库**。
//!
//! # 为什么是两步（预览 → 确认），不是一键导入
//!
//! 这个功能最容易伤人的地方不是解析错，是**解析对了但用户不知道少了什么**。
//! 引擎按设计不导入任何凭据（`.xsh` 的 Password 是私有加密，FinalShell 的密码同理），
//! 所以导进来的每一条连接都缺认证材料。一键导入的话，用户看到「导入成功，共 12 条」，
//! 直到某天要连上去救火时才发现每一条都要重新填密码——而那正是最不该发现这件事的时刻。
//!
//! 于是 `preview` 把 `unresolved` 清单原样交给界面，用户确认后才 `commit`。
//! 这也让「哪些字段没导进来」成为**产出的数据**而不是文档里的一段话：
//! 文档会随格式演进过期，产出的清单永远对应这一次导入的这一个文件。

use crate::state::AppState;
use fs_connmgr::import_foreign::{self, ImportError, ImportedProfile, ImportedScheme, Unresolved};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;

/// 认出来的格式。给界面显示用，也让用户在文件被认成别的格式时能看出来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForeignKind {
    /// Xshell 会话文件 `.xsh`（INI）
    XshellSession,
    /// Xshell 配色文件 `.xcs`（INI，一个文件可含多套）
    XshellColors,
    /// FinalShell 连接配置（JSON）
    Finalshell,
    /// iTerm2 配色 `.itermcolors`（plist XML）
    ItermColors,
    /// **本程序自己导出的**配色 JSON（M4b「JSON 导出 round-trip」）。
    ///
    /// 自家格式出现在一个叫 `Foreign` 的枚举里看着别扭，但换个入口是更坏的选择：
    /// 用户不关心「这是不是外来格式」，他只想让「文件 → 导入」吃下上次导出的文件。
    /// 为自家格式单开一个菜单项，等于把我们的内部分类摆到界面上让他去分辨。
    NativeScheme,
}

/// 导出文件的格式标识与版本。
///
/// 带标识而不是靠「有没有 schemes 字段」来认，有两个用处：格式识别可靠（一个恰好也叫
/// schemes 的无关 JSON 不会被误吃），以及将来改结构时能按版本分支——没有版本号的格式
/// 只能靠猜，而猜错的表现是把用户的配色导成一堆错位的颜色。
pub const NATIVE_SCHEME_FORMAT: &str = "future-shell-color-schemes";
pub const NATIVE_SCHEME_VERSION: u32 = 1;

/// 本程序导出的配色文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSchemeFile {
    pub format: String,
    pub version: u32,
    pub schemes: Vec<ImportedScheme>,
}

/// 解析本程序自己导出的配色 JSON。
fn parse_native_schemes(content: &str) -> Result<Vec<ImportedScheme>, ImportError> {
    if content.len() > import_foreign::MAX_IMPORT_BYTES {
        return Err(ImportError::TooLarge);
    }
    let f: NativeSchemeFile =
        serde_json::from_str(content).map_err(|e| ImportError::NotThisFormat {
            detail: format!("不是本程序导出的配色 JSON：{e}"),
        })?;
    if f.format != NATIVE_SCHEME_FORMAT {
        return Err(ImportError::NotThisFormat {
            detail: format!("format 字段是 {}，不是配色文件", f.format),
        });
    }
    // 版本比我们新 ⇒ 明说，而不是尽力解析。字段含义可能已经变了，
    // 「尽力」的结果是一套颜色错位的配色，而那比拒绝更难被发现。
    if f.version > NATIVE_SCHEME_VERSION {
        return Err(ImportError::MissingRequired {
            field: "version（这个文件来自更新的版本，本程序看不懂）",
        });
    }
    if f.schemes.is_empty() {
        return Err(ImportError::MissingRequired { field: "schemes" });
    }
    Ok(f.schemes)
}

/// 一次预览的结果。连接与配色两类都可能为空——一个 `.xcs` 里没有连接，一个 `.xsh` 里没有配色。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignPreview {
    pub kind: ForeignKind,
    /// 源文件名，原样回显。用户一次导入多个文件时，报告里要认得出是哪一个。
    pub filename: String,
    pub profiles: Vec<ImportedProfile>,
    pub schemes: Vec<ImportedScheme>,
    /// 认得出但没导入的字段。**空表示这个文件里的东西全都处理过了。**
    pub unresolved: Vec<Unresolved>,
}

/// 落库的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForeignImportOutcome {
    pub profiles_added: usize,
    pub schemes_added: usize,
    /// 因为同名已存在而跳过的配色。不覆盖是刻意的——见 `commit` 的文档注。
    pub schemes_skipped: Vec<String>,
}

/// 自定义配色存放的 settings 键。
///
/// 与内置的 12 套（前端 `lib/term-schemes/*.json`）分开存：内置的随程序版本走，
/// 导入的属于用户数据。混在一起的话，升级时要么覆盖用户导入的，要么不敢更新内置的。
pub const CUSTOM_SCHEMES_KEY: &str = "term.customSchemes";

/// 一次能导入多少套配色。`.xcs` 一个文件里放几十套是常见的，但几千套只能是文件出了问题。
const MAX_SCHEMES_PER_IMPORT: usize = 200;

/// 自定义配色的总量上限。这个键的值要整份读进内存、整份推过 IPC，
/// 而 settings 值有 8 KiB 的硬闸（`settings_cmd::SETTING_VALUE_BYTES_MAX`）——
/// 一套配色序列化后约 300 字节，故实际能存的远少于这个数；这里的上限只是让
/// 「为什么存不进去」有一句能读懂的话，而不是撞上那个字节闸报一句 JSON 太大。
const MAX_CUSTOM_SCHEMES: usize = 24;

/// 从内容认格式。**不只看扩展名**——用户改过名的文件、从聊天软件里存下来的文件，
/// 扩展名经常是错的或没有。扩展名只用来决定**先试哪个**，最终由解析成功与否说了算。
fn detect_and_parse(filename: &str, content: &str) -> Result<ForeignPreview, ImportError> {
    let stem = std::path::Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("导入的配色");
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    // 扩展名只决定**先试哪个**，不决定试哪些：首选排在前面，其余四种全部跟在后面。
    //
    // 「只试扩展名对应的那个」是个很容易写出来的版本，但它会在最刺眼的场合失手——
    // 用户把一个 `.xcs` 存成了 `.xsh`（两者都出自 Xshell，混淆再正常不过），
    // 那个版本会告诉他「这不是 Xshell 会话文件」，而它明明是一个 Xshell 文件。
    // 全试一遍的代价是几次字符串扫描，换来的是「文件名错了也没关系」。
    const ALL: [ForeignKind; 5] = [
        // 自家格式排第一：它有 format 字段，识别最确定，且不会误吃别的东西。
        // 排在后面的话，一个自家导出的文件会先被 FinalShell 分支试着解析
        // （两者都是 JSON），而那条分支的「不是这个格式」判据不如 format 字段硬。
        ForeignKind::NativeScheme,
        ForeignKind::XshellSession,
        ForeignKind::XshellColors,
        ForeignKind::Finalshell,
        ForeignKind::ItermColors,
    ];
    let preferred: &[ForeignKind] = match ext.as_str() {
        "xsh" => &[ForeignKind::XshellSession],
        "xcs" => &[ForeignKind::XshellColors],
        "itermcolors" => &[ForeignKind::ItermColors],
        // .json 两种都可能：自家导出的与 FinalShell 的。自家的靠 format 字段先认。
        "json" => &[ForeignKind::NativeScheme, ForeignKind::Finalshell],
        _ => &[],
    };
    let order: Vec<ForeignKind> = preferred
        .iter()
        .copied()
        .chain(ALL.into_iter().filter(|k| !preferred.contains(k)))
        .collect();

    // 记住第一个「不是格式不对，而是格式对了但内容有问题」的错误：
    // 那种错误比最后一个 NotThisFormat 有用得多——它说明我们认出了文件，只是缺字段。
    let mut substantive: Option<ImportError> = None;
    for kind in &order {
        let attempt = match kind {
            ForeignKind::XshellSession => {
                import_foreign::parse_xshell_session(content).map(|r| ForeignPreview {
                    kind: *kind,
                    filename: filename.to_string(),
                    profiles: vec![r.value],
                    schemes: vec![],
                    unresolved: r.unresolved,
                })
            }
            ForeignKind::XshellColors => {
                import_foreign::parse_xshell_schemes(content).map(|r| ForeignPreview {
                    kind: *kind,
                    filename: filename.to_string(),
                    profiles: vec![],
                    schemes: r.value,
                    unresolved: r.unresolved,
                })
            }
            ForeignKind::Finalshell => {
                import_foreign::parse_finalshell(content).map(|r| ForeignPreview {
                    kind: *kind,
                    filename: filename.to_string(),
                    profiles: vec![r.value],
                    schemes: vec![],
                    unresolved: r.unresolved,
                })
            }
            ForeignKind::NativeScheme => parse_native_schemes(content).map(|v| ForeignPreview {
                kind: *kind,
                filename: filename.to_string(),
                profiles: vec![],
                schemes: v,
                // 自家格式没有「认得出但导不进来」的字段——它就是我们自己写出去的。
                unresolved: vec![],
            }),
            ForeignKind::ItermColors => {
                import_foreign::parse_iterm_colors(content, stem).map(|r| ForeignPreview {
                    kind: *kind,
                    filename: filename.to_string(),
                    profiles: vec![],
                    schemes: vec![r.value],
                    unresolved: r.unresolved,
                })
            }
        };
        match attempt {
            Ok(p) => return Ok(p),
            Err(ImportError::NotThisFormat { .. }) => continue,
            Err(e @ ImportError::TooLarge) => return Err(e), // 文件太大与格式无关，立刻停
            Err(e) => substantive.get_or_insert(e),
        };
    }
    Err(substantive.unwrap_or(ImportError::NotThisFormat {
        detail:
            "认不出这是哪种配置文件（支持本程序导出的配色 JSON、Xshell .xsh/.xcs、FinalShell JSON、iTerm2 .itermcolors）"
                .into(),
    }))
}

/// 预览一个第三方配置文件。**不写任何东西。**
#[tauri::command]
pub async fn foreign_import_preview(
    filename: String,
    content: String,
) -> Result<ForeignPreview, String> {
    let mut p = detect_and_parse(&filename, &content).map_err(|e| e.message())?;
    // 预览解析出了、但落库时**无处安放**的字段，在这里如实并入 unresolved
    // （2026-08-31）。此前它们只出现在解析结果里、落库时被静默丢掉——
    // 而 private_key_path 的文档还写着「能省用户一次翻找」，等于承诺了没做的事。
    // 归到 unresolved 而不是硬塞进 Profile：私钥在本程序里走 Vault（存的是密钥
    // 材料的记录 id，不是磁盘路径），模型里根本没有「私钥路径」这一栏；
    // 硬加一栏会让「私钥从哪来」出现第二个真相来源。
    for prof in &p.profiles {
        // 用户名为空：`Profile::validate()`（model.rs:218 的 nonempty）要求它非空，
        // 而两个解析器对 UserName / user_name 都是 `unwrap_or_default()`——源文件
        // 没这个字段时就是空串。不在这里说，用户会在点「导入」之后收到一句
        // 「第 1 条：用户名不能为空」——那时他已经不知道是哪个文件、哪一条了。
        if prof.username.trim().is_empty() {
            p.unresolved.push(fs_connmgr::import_foreign::Unresolved {
                key: "username".into(),
                why: format!(
                    "{} 没有用户名 —— 导入前请先补上，否则这一条落不了库",
                    prof.host
                ),
            });
        }
        if let Some(path) = prof.private_key_path.as_deref() {
            p.unresolved.push(fs_connmgr::import_foreign::Unresolved {
                key: "private_key_path".into(),
                why: format!(
                    "源文件指向私钥 {path} —— 本程序的私钥走 Vault（导入后请在连接属性里选私钥）"
                ),
            });
        }
        if let Some(hint) = prof.auth_hint.as_deref() {
            p.unresolved.push(fs_connmgr::import_foreign::Unresolved {
                key: "authentication_type".into(),
                why: format!(
                    "源文件声明认证方式为 {hint} —— 凭据没有导入，请在连接属性里补认证方式"
                ),
            });
        }
    }
    if p.schemes.len() > MAX_SCHEMES_PER_IMPORT {
        return Err(format!(
            "这个文件里有 {} 套配色，超过单次导入上限 {}",
            p.schemes.len(),
            MAX_SCHEMES_PER_IMPORT
        ));
    }
    // 配色名去空白：`.xcs` 的 section 名两侧常带空格，落库后会变成两个看起来一样的名字
    for s in &mut p.schemes {
        s.name = s.name.trim().to_string();
    }
    Ok(p)
}

/// 把一条预览结果拼成 `ProfileRepo::import_json` 吃的载荷。
///
/// # 为什么走 import_json 而不是自己拼 SQL
///
/// id 冲突重生成、`host_key_pins` 剥离这些不变量都在那条路径上（见
/// connmgr/repo.rs 的「导入剥离不变量」），绕过去等于把它们重做一遍（并且做错）。
///
/// # 为什么这里要**显式**生成 id（2026-08-31 修）
///
/// `import_json` 解析成 `Vec<fs_connmgr::Profile>`，而 `Profile.id` 是**必填无
/// default** 的 Uuid。此前这里拼的载荷没有 id，于是整条落库路径 100% 失败——
/// 用户选完文件、预览一切正常、点「导入」，收到一句
/// `serde: missing field \`id\``（真机实测报出）。
///
/// id 由**本机生成**而不是从源文件带过来，是刻意的：第三方文件里的标识符是别人
/// 库里的主键，带过来只会和本机既有记录撞；`import_json` 本身也会为撞了的 id
/// 重新生成——但它得先能解析。
///
/// 抽成独立函数（而不是留在命令体内）是为了**可测**：`foreign_import_commit`
/// 要 `State<AppState>` + 数据库，而失败发生在拼载荷这一步，纯函数一条断言就钉住。
fn commit_payload(ip: &ImportedProfile) -> String {
    serde_json::json!([{
        // 本机生成：不带源文件的标识符（见上）
        "id": uuid::Uuid::new_v4().to_string(),
        "name": ip.name,
        "host": ip.host,
        "port": ip.port,
        "username": ip.username,
        "group_path": ip.group_path,
        // 第三方格式里只有 SSH 会话；显式写出而不是靠 serde default 蒙对
        "protocol": "ssh",
    }])
    .to_string()
}

/// 把用户确认过的预览结果落库。
///
/// # 同名配色不覆盖
///
/// 重名时跳过并在结果里列出来，而不是覆盖或改名。覆盖会**无声地毁掉**用户调了很久的一套配色；
/// 自动改名（`Dracula (2)`）则会在反复导入同一个文件后堆出一串副本，而用户分不清哪个是哪个。
/// 跳过 + 明确告知，把决定权留给人——他可以先删掉旧的再导一次。
#[tauri::command]
pub async fn foreign_import_commit(
    preview: ForeignPreview,
    state: State<'_, Arc<AppState>>,
) -> Result<ForeignImportOutcome, String> {
    let pool = state.db.pool();
    let mut profiles_added = 0usize;
    for ip in &preview.profiles {
        let n = fs_connmgr::ProfileRepo::new(pool)
            .import_json(&commit_payload(ip))
            .await
            .map_err(|e| e.to_string())?;
        profiles_added += n;
    }

    let mut schemes_skipped = Vec::new();
    let mut schemes_added = 0usize;
    if !preview.schemes.is_empty() {
        let repo = fs_connmgr::SettingsRepo::new(pool);
        let existing_raw = repo
            .get(CUSTOM_SCHEMES_KEY)
            .await
            .map_err(|e| e.to_string())?;
        let mut list: Vec<ImportedScheme> = existing_raw
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        for s in &preview.schemes {
            if s.name.is_empty() {
                schemes_skipped.push("（无名）".to_string());
                continue;
            }
            if list.iter().any(|e| e.name == s.name) {
                schemes_skipped.push(s.name.clone());
                continue;
            }
            if list.len() >= MAX_CUSTOM_SCHEMES {
                schemes_skipped.push(s.name.clone());
                continue;
            }
            list.push(s.clone());
            schemes_added += 1;
        }
        if schemes_added > 0 {
            let json = serde_json::to_string(&list).map_err(|e| e.to_string())?;
            // 走与 `settings_set` 同一个校验，而不是直接 repo.set。
            // 这条路径处理的是**外来文件**里的颜色串，它们最终会进 CSS 变量与终端主题对象；
            // 如果只有前端写入那条路径过校验，绕过校验的恰好是风险最高的这一条。
            crate::commands::settings_cmd::validate_setting(CUSTOM_SCHEMES_KEY, &json)?;
            repo.set(CUSTOM_SCHEMES_KEY, &json)
                .await
                .map_err(|e| e.to_string())?;
        }
    }

    Ok(ForeignImportOutcome {
        profiles_added,
        schemes_added,
        schemes_skipped,
    })
}

#[cfg(test)]
mod tests {
    /// 落库载荷必须能被 `ProfileRepo::import_json` 解析（2026-08-31 用户实测报出）。
    ///
    /// # 这条判据存在的理由
    ///
    /// `foreign_import_commit` 手拼一份 JSON 交给 `import_json`，而后者解析成
    /// `Vec<fs_connmgr::Profile>`——`Profile.id` 是**必填无 default** 的 Uuid。
    /// 手拼的载荷里没有 id，于是这条路径**100% 失败**：用户选完文件、预览一切正常、
    /// 点「导入」，收到一句 `serde: missing field \`id\``。
    ///
    /// 它能带着这个 bug 出厂，是因为 `foreign_import_commit` 此前**一条测试都没有**：
    /// 预览侧（`detect_and_parse`）测得很密，落库侧一条没测，而两者之间正是这个接缝。
    ///
    /// 本测试直接把拼载荷那段的形状钉住：**不经数据库**（那要 sqlite 夹具），
    /// 只断言「这份 JSON 能反序列化成 Profile」——那正是 `import_json` 的第一步，
    /// 也正是失败发生的那一步。
    #[test]
    fn commit_payload_must_deserialize_into_a_profile() {
        // 与 foreign_import_commit 里的 json! 逐字同构（改那边不改这里 → 本测试转红）
        let ip = ImportedProfile {
            name: "ai 生产环境 国外".to_string(),
            host: "54.254.142.143".to_string(),
            port: 22,
            username: "root".to_string(),
            group_path: None,
            ..Default::default()
        };
        let one = commit_payload(&ip);
        let parsed: Result<Vec<fs_connmgr::Profile>, _> = serde_json::from_str(&one);
        let profiles = parsed.unwrap_or_else(|e| {
            panic!(
                "落库载荷无法解析成 Profile —— 用户点「导入」时收到的就是这个错误：serde: {e}\n载荷：{one}"
            )
        });
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].host, "54.254.142.143");
        assert_eq!(profiles[0].username, "root");
        assert_eq!(profiles[0].port, 22);
        // id 由本程序生成（不是外来的）：import_json 还会为冲突的 id 重新生成，
        // 但它得先能**解析**——这里只要求非 nil。
        assert_ne!(
            profiles[0].id,
            uuid::Uuid::nil(),
            "落库载荷该带一个本机生成的 id"
        );
    }

    /// 空用户名：载荷本身**解析得开**（缺 id 那个 bug 修完后），但会在
    /// `Profile::validate()`（model.rs:218）被拒。
    ///
    /// 这条判据钉的是「知情」而不是「能过」：源文件没有 UserName 时，
    /// 两个解析器都给出空串（`unwrap_or_default()`），落库必然被拒——
    /// 所以 `foreign_import_preview` 要**在导入之前**把它列进 unresolved。
    /// 若哪天有人给 username 加了个默认值（比如填 "root"），本测试转红：
    /// 替用户猜用户名比让他自己填危险得多（连错机器上的 root 是另一种事故）。
    #[test]
    fn an_empty_username_still_parses_but_is_refused_by_validate() {
        let ip = ImportedProfile {
            name: "无名".into(),
            host: "10.0.0.9".into(),
            port: 22,
            username: String::new(), // 源文件没有 UserName
            ..Default::default()
        };
        let profiles: Vec<fs_connmgr::Profile> =
            serde_json::from_str(&commit_payload(&ip)).expect("载荷该解析得开（id 已补）");
        assert_eq!(profiles[0].username, "", "不得替用户猜用户名");
        assert!(
            profiles[0].validate().is_err(),
            "空用户名该被 validate 拒——预览阶段必须已经把它列进 unresolved"
        );
    }

    /// 协议字段：FinalShell/Xshell 导进来的都是 SSH，落库后必须是 SSH 而不是靠
    /// serde default 蒙对。RDP 档案不从这条路径来（第三方格式里没有 RDP 会话）。
    #[test]
    fn commit_payload_lands_as_ssh() {
        let ip = ImportedProfile {
            name: "x".into(),
            host: "h".into(),
            port: 22,
            username: "u".into(),
            ..Default::default()
        };
        let profiles: Vec<fs_connmgr::Profile> =
            serde_json::from_str(&commit_payload(&ip)).expect("载荷该可解析");
        assert_eq!(profiles[0].protocol, fs_connmgr::Protocol::Ssh);
    }

    use super::*;

    const XSH: &str = "[SessionInfo]\nHost=10.0.0.5\nPort=2222\nUserName=ops\n[CONNECTION:AUTHENTICATION]\nMethod=password\nPassword=AAECAwQ=\n";
    // `.xcs` 的形状：`[NAMES]` 里 count + nameN 列出这个文件含哪几套，各套自己一个 section。
    // 引擎按 XCS_ANSI_KEYS 的固定顺序取色（文件里的键是乱序的，按文件顺序读会得到一套错位的配色）。
    const XCS: &str = "[NAMES]\ncount=1\nname0=Dracula\n\n[Dracula]\ntext=f8f8f2\nbackground=282a36\nblack=000000\nred=ff5555\ngreen=50fa7b\nyellow=f1fa8c\nblue=bd93f9\nmagenta=ff79c6\ncyan=8be9fd\nwhite=bbbbbb\nblack(bold)=555555\nred(bold)=ff6e6e\ngreen(bold)=69ff94\nyellow(bold)=ffffa5\nblue(bold)=d6acff\nmagenta(bold)=ff92df\ncyan(bold)=a4ffff\nwhite(bold)=ffffff\n";
    const FS_JSON: &str = r#"{"name":"生产 web","host":"1.2.3.4","port":22,"user_name":"root","password":"secret","authentication_type":1}"#;
    /// iTerm2 的 `.itermcolors`：16 个 `Ansi N Color` 加前景背景，各是一个三分量 dict。
    /// 引擎要求前景与背景都在——缺一个就不是一套能用的配色。
    fn iterm_fixture() -> String {
        let mut s = String::from(r#"<?xml version="1.0"?><plist version="1.0"><dict>"#);
        for i in 0..16 {
            let v = i as f64 / 16.0;
            s.push_str(&format!(
                "<key>Ansi {i} Color</key><dict><key>Red Component</key><real>{v}</real><key>Green Component</key><real>{v}</real><key>Blue Component</key><real>{v}</real></dict>"
            ));
        }
        s.push_str("<key>Foreground Color</key><dict><key>Red Component</key><real>1.0</real><key>Green Component</key><real>1.0</real><key>Blue Component</key><real>1.0</real></dict>");
        s.push_str("<key>Background Color</key><dict><key>Red Component</key><real>0.0</real><key>Green Component</key><real>0.0</real><key>Blue Component</key><real>0.0</real></dict>");
        s.push_str("</dict></plist>");
        s
    }

    #[test]
    fn each_format_is_recognized_by_its_extension() {
        assert_eq!(
            detect_and_parse("a.xsh", XSH).unwrap().kind,
            ForeignKind::XshellSession
        );
        assert_eq!(
            detect_and_parse("a.xcs", XCS).unwrap().kind,
            ForeignKind::XshellColors
        );
        assert_eq!(
            detect_and_parse("a.json", FS_JSON).unwrap().kind,
            ForeignKind::Finalshell
        );
        assert_eq!(
            detect_and_parse("a.itermcolors", &iterm_fixture())
                .unwrap()
                .kind,
            ForeignKind::ItermColors
        );
    }

    #[test]
    fn a_wrong_extension_does_not_stop_us() {
        // 用户从聊天软件里存下来的文件经常叫 `session.txt`，或者根本没有扩展名。
        // 扩展名只决定先试哪个，认不认得出由内容说了算。
        for (name, body, want) in [
            ("会话.txt", XSH, ForeignKind::XshellSession),
            ("配色", XCS, ForeignKind::XshellColors),
            ("导出", FS_JSON, ForeignKind::Finalshell),
            (
                "colors.txt",
                iterm_fixture().as_str(),
                ForeignKind::ItermColors,
            ),
        ] {
            assert_eq!(
                detect_and_parse(name, body).map(|p| p.kind),
                Ok(want),
                "{name} 该被认成 {want:?}"
            );
        }
        // 连扩展名都是**误导性**的也一样：.xcs 的内容配 .xsh 的名字
        assert_eq!(
            detect_and_parse("骗人的.xsh", XCS).map(|p| p.kind),
            Ok(ForeignKind::XshellColors)
        );
    }

    #[test]
    fn something_that_is_not_a_config_file_says_so_plainly() {
        let e = detect_and_parse("x.bin", "这是一段普通文本，什么格式都不是").unwrap_err();
        let m = e.message();
        // 错误里要列出支持哪几种——「认不出」而不说支持什么，用户无从判断是自己选错了文件
        // 还是这个功能不支持他的软件。
        assert!(m.contains("Xshell"), "{m}");
        assert!(m.contains("FinalShell"), "{m}");
        assert!(m.contains("iTerm2"), "{m}");
    }

    #[tokio::test]
    async fn no_credential_material_reaches_the_preview() {
        // 这是整个导入路径上最重要的一条：源文件里明明白白写着 Password/secret，
        // 而给到界面的结构里**任何字段都不得含有它**。
        // 断言整份序列化后的 JSON，而不是逐字段检查——逐字段检查会漏掉将来新加的字段。
        for (name, body, secret) in [("a.xsh", XSH, "AAECAwQ="), ("a.json", FS_JSON, "secret")] {
            let p = foreign_import_preview(name.into(), body.into())
                .await
                .unwrap_or_else(|e| panic!("{name} 预览失败：{e}"));
            let json = serde_json::to_string(&p).unwrap();
            assert!(
                !json.contains(secret),
                "{name} 的预览结果里出现了凭据材料：{json}"
            );
            // 反向对照：确实解析出了东西，不是因为整个结果是空的才「没有凭据」
            assert_eq!(p.profiles.len(), 1);
            assert!(!p.profiles[0].host.is_empty());
        }
    }

    #[tokio::test]
    async fn the_unresolved_list_names_the_credential_we_refused() {
        // 「没导入密码」必须是**说出来的**，不是默默的。用户看不到这一条就会以为连得上。
        let p = foreign_import_preview("a.xsh".into(), XSH.into())
            .await
            .unwrap();
        assert!(
            p.unresolved.iter().any(|u| u.key.contains("Password")),
            "unresolved 里没提到 Password：{:?}",
            p.unresolved
        );
        // 而且要说清为什么，不能只给一个键名
        let why = &p
            .unresolved
            .iter()
            .find(|u| u.key.contains("Password"))
            .unwrap()
            .why;
        assert!(why.chars().count() >= 8, "理由太短，等于没说：{why}");
    }

    #[tokio::test]
    async fn scheme_names_are_trimmed_before_they_can_collide() {
        // `.xcs` 的 section 名两侧常带空格。不去空白的话，`Dracula` 与 `Dracula ` 会
        // 各占一格，界面上看起来是两个一模一样的条目。
        let padded = XCS.replace("name0=Dracula", "name0=  Dracula  ");
        let p = foreign_import_preview("a.xcs".into(), padded)
            .await
            .unwrap();
        assert_eq!(p.schemes[0].name, "Dracula");
    }

    #[test]
    fn the_scheme_limit_is_below_what_the_settings_byte_gate_allows() {
        // 这两个数字若走散，用户会撞上「设置值超过 8192 字节上限」——一句与配色毫无关系的话。
        // 一套配色序列化后约 300 字节（16 个 #rrggbb + 名字 + 前景背景光标）。
        let one = ImportedScheme {
            name: "一个名字还算长的配色方案".into(),
            foreground: "#f8f8f2".into(),
            background: "#282a36".into(),
            ansi: (0..16).map(|_| "#ff5555".to_string()).collect(),
            cursor: Some("#f8f8f0".into()),
        };
        let per = serde_json::to_string(&one).unwrap().len();
        assert!(
            per * MAX_CUSTOM_SCHEMES < 8192,
            "{MAX_CUSTOM_SCHEMES} 套 × {per} 字节会撞上 settings 的 8 KiB 闸"
        );
    }

    // ── 自家格式的 JSON 导出 round-trip（M4b 出口「JSON 导出 round-trip 一致」） ──

    fn scheme(name: &str) -> ImportedScheme {
        ImportedScheme {
            name: name.into(),
            foreground: "#cccccc".into(),
            background: "#1e1e2e".into(),
            ansi: (0..16)
                .map(|i| format!("#{:02x}{:02x}{:02x}", i * 16, i, 255 - i * 16))
                .collect(),
            cursor: Some("#f5e0dc".into()),
        }
    }

    /// 导出的 JSON 再读回来，必须**逐字段**等于导出前。
    ///
    /// 这条是出口的字面判据。它容易被写成一个假的版本：只比名字、或者只比数量。
    /// 这里比的是整个结构相等，且用了一套 16 色各不相同的配色——
    /// 若某处把 ansi 顺序弄反（`.xcs` 那条路径上真出过这种事），相等就不成立。
    #[test]
    fn exported_json_round_trips_field_for_field() {
        let original = vec![scheme("MyDark"), scheme("另一套")];
        let file = NativeSchemeFile {
            format: NATIVE_SCHEME_FORMAT.into(),
            version: NATIVE_SCHEME_VERSION,
            schemes: original.clone(),
        };
        let json = serde_json::to_string_pretty(&file).unwrap();
        let back = parse_native_schemes(&json).expect("自己导出的文件必须导得回来");
        assert_eq!(back, original);
        // ansi 顺序也要原样——逐项比对，而不是只看长度
        assert_eq!(back[0].ansi, original[0].ansi);
        assert_ne!(back[0].ansi[0], back[0].ansi[15], "夹具本身要能区分顺序");
    }

    /// 走完整的一圈：导出 → 从**导入入口**读回来（而不是直接调 parse）。
    ///
    /// 分开写是因为两者会在不同的地方坏：上一条测的是序列化/反序列化，
    /// 这一条测的是**格式识别**——一个自家导出的 .json 会不会被 FinalShell 分支先吃掉。
    #[tokio::test]
    async fn the_import_entry_point_recognizes_our_own_export() {
        let file = NativeSchemeFile {
            format: NATIVE_SCHEME_FORMAT.into(),
            version: NATIVE_SCHEME_VERSION,
            schemes: vec![scheme("MyDark")],
        };
        let json = serde_json::to_string(&file).unwrap();
        for name in ["配色.json", "配色", "配色.txt"] {
            let p = foreign_import_preview(name.into(), json.clone())
                .await
                .unwrap_or_else(|e| panic!("{name} 认不出：{e}"));
            assert_eq!(p.kind, ForeignKind::NativeScheme, "{name}");
            assert_eq!(p.schemes.len(), 1);
            assert_eq!(p.schemes[0], scheme("MyDark"));
            // 自家格式没有导不进来的字段
            assert!(p.unresolved.is_empty());
        }
    }

    #[test]
    fn a_json_that_is_not_ours_is_not_eaten_by_the_native_branch() {
        // FinalShell 的配置也是 JSON。没有 format 字段这道闸的话，
        // 「认自家格式」会退化成「认任何 JSON」，而排在第一位的它会把别人的文件全吃掉。
        assert!(matches!(
            parse_native_schemes(FS_JSON),
            Err(ImportError::NotThisFormat { .. })
        ));
        // 反向对照：FinalShell 的文件仍然被认成 FinalShell
        assert_eq!(
            detect_and_parse("x.json", FS_JSON).map(|p| p.kind),
            Ok(ForeignKind::Finalshell)
        );
    }

    #[test]
    fn a_newer_format_version_is_refused_rather_than_guessed() {
        // 「尽力解析」一个看不懂的版本，结果是一套颜色错位的配色——
        // 而那比一句「看不懂」更难被发现。
        let json = format!(
            r#"{{"format":"{NATIVE_SCHEME_FORMAT}","version":{},"schemes":[]}}"#,
            NATIVE_SCHEME_VERSION + 1
        );
        let e = parse_native_schemes(&json).unwrap_err();
        assert!(e.message().contains("version"), "{}", e.message());
    }

    #[test]
    fn an_empty_scheme_list_says_so_instead_of_importing_nothing() {
        // 「导入成功，共 0 套」是最没用的一句话：用户不知道是文件空的、
        // 还是我们没读懂。明确报缺 schemes。
        let json = format!(r#"{{"format":"{NATIVE_SCHEME_FORMAT}","version":1,"schemes":[]}}"#);
        assert!(matches!(
            parse_native_schemes(&json),
            Err(ImportError::MissingRequired { field: "schemes" })
        ));
    }
}
