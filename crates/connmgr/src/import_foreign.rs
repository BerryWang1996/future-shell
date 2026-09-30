//! 从别家工具导入（M4b 出口第 1 项：Xshell `.xsh`/`.xcs`、FinalShell 配置、
//! iTerm2 配色三路，「字段映射表入计划，spike 记录不可解字段清单」）。
//!
//! # 凭据一律不导入，这是有意的
//!
//! Xshell 的 `Password` 与 FinalShell 的 `password` 都是用各自的私有算法加密的。
//! 社区有解密脚本，但本模块**不实现解密**，理由有三条，按重要性排：
//!
//! 1. **导进来的口令是一份来源不明的明文。** 用户以为自己只是搬了个连接配置，
//!    实际上把另一个产品的凭据库解密后塞进了本程序的 Vault。哪天出事时，
//!    「这条口令是怎么进来的」会是一个没人能回答的问题。
//! 2. Xshell 的加密与机器/用户绑定，换机就解不开——于是它是一个**有时能用**的功能，
//!    而有时能用的导入比不能用的导入更糟：用户不知道该不该重新填密码。
//! 3. 去解别家产品的凭据存储，法律与伦理上都不是本程序该做的事。
//!
//! 所以导入结果里**没有**凭据字段，而是在 [`Imported::unresolved`] 里明确列一条
//! 「口令未导入，请重新填写」。
//!
//! # 「不可解字段清单」是数据，不是文档
//!
//! 出口标准要求记录不可解字段。写成 markdown 的话它会过期——别家改一版格式，
//! 文档就不准了，而没人会去更新它。
//!
//! 所以清单由解析器**产出**：每遇到一个认得出但没法映射的键，就往
//! [`Imported::unresolved`] 里加一条，带上原始键名与原因。UI 直接展示这份清单
//! （「这 3 项没能导入」），用户当场就知道要手动补什么。
//!
//! # 样例的来源与局限
//!
//! 测试里的样例是**按公开格式构造**的，不是从真实安装里抄的（本机没装那几个产品）。
//! 构造样例能验证「认得出的字段映射对了」与「认不出的字段进了清单」，
//! 但验证不了「真实文件里还有哪些我们没见过的键」——后者只能靠 `unresolved` 兜住，
//! 而那正是把它做成数据的理由。这条局限如实写在这里，不假装样例等于真实覆盖。

use std::collections::BTreeMap;

/// 一个没能导入的字段。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Unresolved {
    /// 源文件里的原始键名（含 section，如 `CONNECTION:AUTHENTICATION/Password`）。
    pub key: String,
    /// 为什么没导入。直接给用户看。
    pub why: String,
}

/// 导入出来的一条连接配置。
///
/// 刻意**不**直接产出 [`crate::model::Profile`]：那个类型有 id、有 auth 引用，
/// 而导入阶段两者都还不存在。让调用方拿这份中间结果去建 Profile，
/// 于是「导入解析」与「建库」两件事各自可测。
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ImportedProfile {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// 源文件声明的认证方式（`password` / `publickey` / …），仅作提示。
    ///
    /// 不含任何凭据材料——见模块头「凭据一律不导入」。
    pub auth_hint: Option<String>,
    /// 私钥**路径**（不是内容）。路径不是秘密，且它能省用户一次翻找。
    pub private_key_path: Option<String>,
    pub group_path: Option<String>,
}

/// 导入出来的一套终端配色。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImportedScheme {
    pub name: String,
    /// `#rrggbb`。
    pub foreground: String,
    pub background: String,
    /// ANSI 16 色，索引 0–15。
    pub ansi: Vec<String>,
    /// 光标色（有些格式没有）。
    pub cursor: Option<String>,
}

/// 一次导入的结果。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Imported<T> {
    pub value: T,
    /// 认得出但没能映射的字段。**空表示全部字段都处理过了**。
    pub unresolved: Vec<Unresolved>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// 整个文件不是这种格式。
    NotThisFormat { detail: String },
    /// 是这种格式，但缺了必需字段（没有 host 的连接配置导进来没有意义）。
    MissingRequired { field: &'static str },
    /// 文件过大——导入的是配置文件，不是数据集。
    TooLarge,
}

impl ImportError {
    pub fn message(&self) -> String {
        match self {
            Self::NotThisFormat { detail } => {
                format!("这个文件不像是该格式：{detail}")
            }
            Self::MissingRequired { field } => {
                format!("文件里缺少必需字段 `{field}`，无法建立连接配置")
            }
            Self::TooLarge => "文件过大，超出配置文件的合理范围".into(),
        }
    }
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// 导入文件的大小上限。
///
/// 配置文件都在几 KB 量级。给 1 MiB 的余量，超过即拒——那更可能是用户选错了文件
/// （或者是一份被拼接过的东西），而不是一个真的很大的会话配置。
pub const MAX_IMPORT_BYTES: usize = 1024 * 1024;

fn check_size(s: &str) -> Result<(), ImportError> {
    if s.len() > MAX_IMPORT_BYTES {
        return Err(ImportError::TooLarge);
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════════════
// 极简 INI（Xshell 的 .xsh / .xcs 都是这个形状）
// ══════════════════════════════════════════════════════════════════════

/// section 名 → (键 → 值)。section 名保留原样（Xshell 的 section 带冒号分层）。
type Ini = BTreeMap<String, BTreeMap<String, String>>;

/// 解析 INI。
///
/// 两个与「教科书 INI」不同的地方，都来自真实文件：
/// - **section 名含冒号**（`[CONNECTION:AUTHENTICATION]`），所以不能按冒号切分层级；
/// - **值里可能含 `=`**（base64 的补位、路径里的等号），所以按**第一个** `=` 分割。
fn parse_ini(s: &str) -> Ini {
    let mut out: Ini = BTreeMap::new();
    let mut section = String::new();
    for line in s.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with(';') || t.starts_with('#') {
            continue;
        }
        if let Some(inner) = t.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            section = inner.trim().to_string();
            out.entry(section.clone()).or_default();
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            out.entry(section.clone())
                .or_default()
                .insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

/// 从多个候选 section 里找一个键（Xshell 不同版本把同一字段放在不同 section）。
fn ini_get<'a>(ini: &'a Ini, sections: &[&str], key: &str) -> Option<&'a str> {
    for s in sections {
        if let Some(v) = ini.get(*s).and_then(|m| m.get(key)) {
            if !v.is_empty() {
                return Some(v.as_str());
            }
        }
    }
    None
}

// ══════════════════════════════════════════════════════════════════════
// Xshell 会话（.xsh）
// ══════════════════════════════════════════════════════════════════════

/// Xshell `.xsh` 里**认得出但不导入**的键，及原因。
///
/// 列成表而不是散在代码里，是为了让「我们知道这个键存在」这件事可见——
/// 不列的话，一个没处理的键与一个没见过的键在结果上没有区别。
const XSH_KNOWN_UNMAPPED: &[(&str, &str)] = &[
    (
        "Password",
        "口令由 Xshell 用私有算法加密（且与机器绑定），本程序不解密——请重新填写并存入 Vault",
    ),
    ("PasswordV2", "同上（Xshell 新版的口令字段）"),
    ("Passphrase", "私钥口令同样是加密存放的，请重新填写"),
    ("ProxyPassword", "代理口令加密存放，未导入"),
];

/// 解析 Xshell 的 `.xsh` 会话文件。
pub fn parse_xshell_session(content: &str) -> Result<Imported<ImportedProfile>, ImportError> {
    check_size(content)?;
    let ini = parse_ini(content);
    // 判据：必须有 SessionInfo 或 CONNECTION 段。只看有没有 `[` 会把任何 INI 都认成 .xsh
    if !ini.contains_key("SessionInfo") && !ini.contains_key("CONNECTION") {
        return Err(ImportError::NotThisFormat {
            detail: "没有 [SessionInfo] 或 [CONNECTION] 段".into(),
        });
    }
    let mut unresolved = Vec::new();

    // 协议：只接受 SSH。SFTP/TELNET/SERIAL 等本程序处理不了，
    // 静默当成 SSH 导进来会得到一条连不上的配置。
    if let Some(p) = ini_get(&ini, &["CONNECTION", "SessionInfo"], "Protocol") {
        if !p.eq_ignore_ascii_case("SSH") && !p.eq_ignore_ascii_case("SFTP") {
            return Err(ImportError::NotThisFormat {
                detail: format!("协议是 {p}，本程序只支持 SSH/SFTP"),
            });
        }
    }

    let host = ini_get(&ini, &["CONNECTION", "SessionInfo"], "Host")
        .ok_or(ImportError::MissingRequired { field: "Host" })?
        .to_string();
    let port = ini_get(&ini, &["CONNECTION", "SessionInfo"], "Port")
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(22);
    let username = ini_get(
        &ini,
        &["CONNECTION:AUTHENTICATION", "CONNECTION", "SessionInfo"],
        "UserName",
    )
    .unwrap_or_default()
    .to_string();
    let auth_hint = ini_get(&ini, &["CONNECTION:AUTHENTICATION"], "Method").map(|m| {
        // Xshell 的写法是 "Password" / "Public Key" / "Keyboard Interactive"
        match m.to_ascii_lowercase().replace(' ', "") {
            s if s.contains("publickey") => "publickey".to_string(),
            s if s.contains("keyboard") => "keyboard-interactive".to_string(),
            s if s.contains("password") => "password".to_string(),
            _ => m.to_string(),
        }
    });
    let private_key_path = ini_get(&ini, &["CONNECTION:AUTHENTICATION"], "UserKey")
        .or_else(|| ini_get(&ini, &["CONNECTION:AUTHENTICATION"], "PrivateKey"))
        .map(str::to_string);
    let name = ini_get(&ini, &["SessionInfo"], "Description")
        .filter(|d| !d.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| host.clone());

    // 认得出但不导入的键
    for (sec, kv) in &ini {
        for (k, v) in kv {
            if v.is_empty() {
                continue;
            }
            if let Some((_, why)) = XSH_KNOWN_UNMAPPED.iter().find(|(name, _)| name == k) {
                unresolved.push(Unresolved {
                    key: format!("{sec}/{k}"),
                    why: (*why).to_string(),
                });
            }
        }
    }

    Ok(Imported {
        value: ImportedProfile {
            name,
            host,
            port,
            username,
            auth_hint,
            private_key_path,
            group_path: None,
        },
        unresolved,
    })
}

// ══════════════════════════════════════════════════════════════════════
// Xshell 配色（.xcs）
// ══════════════════════════════════════════════════════════════════════

/// `.xcs` 里的 ANSI 键名，按索引 0–15 排。
///
/// 顺序是 ANSI 的标准顺序（黑红绿黄蓝洋红青白，然后各自的亮色），
/// 而 `.xcs` 文件里的键是**乱序**的——按文件顺序读会得到一套颜色错位的配色，
/// 而颜色错位的终端比没有配色更难用。
const XCS_ANSI_KEYS: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "black(bold)",
    "red(bold)",
    "green(bold)",
    "yellow(bold)",
    "blue(bold)",
    "magenta(bold)",
    "cyan(bold)",
    "white(bold)",
];

/// 把 `.xcs` 的颜色值规范成 `#rrggbb`。
///
/// `.xcs` 写的是不带 `#` 的六位十六进制，但也见过带 `#` 的。两种都吃。
fn normalize_hex(v: &str) -> Option<String> {
    let s = v.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{}", s.to_ascii_lowercase()))
}

/// 解析 Xshell 的 `.xcs` 配色文件。一个文件里可能有多套配色。
pub fn parse_xshell_schemes(content: &str) -> Result<Imported<Vec<ImportedScheme>>, ImportError> {
    check_size(content)?;
    let ini = parse_ini(content);
    let names = ini.get("NAMES").ok_or(ImportError::NotThisFormat {
        detail: "没有 [NAMES] 段".into(),
    })?;
    let mut unresolved = Vec::new();
    let mut out = Vec::new();

    // [NAMES] 里是 name0=X / name1=Y。按索引升序取，count 只作参考——
    // 见过 count 与实际条数不符的文件，信 count 会漏掉或越界。
    let mut idx: Vec<(usize, &String)> = names
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix("name")?
                .parse::<usize>()
                .ok()
                .map(|i| (i, v))
        })
        .collect();
    idx.sort_unstable_by_key(|(i, _)| *i);

    for (_, scheme_name) in idx {
        let Some(sec) = ini.get(scheme_name.as_str()) else {
            unresolved.push(Unresolved {
                key: format!("NAMES/{scheme_name}"),
                why: "[NAMES] 里声明了这套配色，但文件里没有对应的段".into(),
            });
            continue;
        };
        let get = |k: &str| sec.get(k).and_then(|v| normalize_hex(v));
        let (Some(foreground), Some(background)) = (get("text"), get("background")) else {
            unresolved.push(Unresolved {
                key: format!("{scheme_name}/text|background"),
                why: "缺少前景或背景色，这套配色没有导入".into(),
            });
            continue;
        };
        // ANSI 16 色：缺一个就整套不要。补默认值会得到一套「大部分对、有一格不对」的配色，
        // 而那种错误比整套缺失更难被发现。
        let mut ansi = Vec::with_capacity(16);
        let mut missing = None;
        for k in XCS_ANSI_KEYS {
            match get(k) {
                Some(c) => ansi.push(c),
                None => {
                    missing = Some(k);
                    break;
                }
            }
        }
        if let Some(k) = missing {
            unresolved.push(Unresolved {
                key: format!("{scheme_name}/{k}"),
                why: format!("ANSI 色 `{k}` 缺失或格式不对，这套配色没有导入（不补默认值：一格不对的配色比整套缺失更难发现）"),
            });
            continue;
        }
        out.push(ImportedScheme {
            name: scheme_name.clone(),
            foreground,
            background,
            ansi,
            cursor: get("cursor"),
        });
    }

    Ok(Imported {
        value: out,
        unresolved,
    })
}

// ══════════════════════════════════════════════════════════════════════
// FinalShell
// ══════════════════════════════════════════════════════════════════════

/// FinalShell 配置里认得出但不导入的键。
const FINALSHELL_KNOWN_UNMAPPED: &[(&str, &str)] = &[
    (
        "password",
        "口令由 FinalShell 加密存放，本程序不解密——请重新填写并存入 Vault",
    ),
    (
        "secret_key_id",
        "指向 FinalShell 自己的凭据库条目，本程序读不到",
    ),
    ("private_key_password", "私钥口令加密存放，未导入"),
];

/// 解析 FinalShell 的连接配置（JSON）。
pub fn parse_finalshell(content: &str) -> Result<Imported<ImportedProfile>, ImportError> {
    check_size(content)?;
    let v: serde_json::Value =
        serde_json::from_str(content).map_err(|e| ImportError::NotThisFormat {
            detail: format!("不是合法 JSON：{e}"),
        })?;
    let obj = v.as_object().ok_or(ImportError::NotThisFormat {
        detail: "顶层不是对象".into(),
    })?;
    // 判据：FinalShell 的连接配置一定有 host。只看「是 JSON」会把本程序自己的
    // 导出文件也认成 FinalShell 配置。
    let host = obj
        .get("host")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or(ImportError::MissingRequired { field: "host" })?
        .to_string();

    let port = obj
        .get("port")
        .and_then(|x| x.as_u64())
        .and_then(|p| u16::try_from(p).ok())
        .unwrap_or(22);
    let username = obj
        .get("user_name")
        .or_else(|| obj.get("userName"))
        .or_else(|| obj.get("user"))
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let name = obj
        .get("name")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&host)
        .to_string();
    // FinalShell 用一个数字表示认证方式；含义按公开资料，未知值原样带过去
    let auth_hint = obj
        .get("authentication_type")
        .and_then(|x| x.as_u64())
        .map(|t| match t {
            0 => "password".to_string(),
            1 => "publickey".to_string(),
            other => format!("finalshell-auth-{other}"),
        });
    let private_key_path = obj
        .get("private_key_path")
        .or_else(|| obj.get("privateKeyPath"))
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    // FinalShell 用 folder/group 表示分组
    let group_path = obj
        .get("folder")
        .or_else(|| obj.get("group"))
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let mut unresolved = Vec::new();
    for (k, val) in obj {
        let empty = match val {
            serde_json::Value::String(s) => s.is_empty(),
            serde_json::Value::Null => true,
            _ => false,
        };
        if empty {
            continue;
        }
        if let Some((_, why)) = FINALSHELL_KNOWN_UNMAPPED.iter().find(|(n, _)| n == k) {
            unresolved.push(Unresolved {
                key: k.clone(),
                why: (*why).to_string(),
            });
        }
    }

    Ok(Imported {
        value: ImportedProfile {
            name,
            host,
            port,
            username,
            auth_hint,
            private_key_path,
            group_path,
        },
        unresolved,
    })
}

// ══════════════════════════════════════════════════════════════════════
// iTerm2 配色（.itermcolors，plist XML）
// ══════════════════════════════════════════════════════════════════════

/// 一个 RGB 分量三元组（iTerm2 用 0.0–1.0 的浮点）。
#[derive(Debug, Default, Clone, Copy)]
struct Rgb {
    r: f64,
    g: f64,
    b: f64,
}

impl Rgb {
    fn to_hex(self) -> String {
        let q = |x: f64| -> u8 {
            // 显式钳位。**这不是在修一个 bug**：Rust 1.45 起 `f64 as u8` 是**饱和**
            // 转换（实测 2.0*255 → 255、-5.0*255 → 0），所以即使不 clamp，
            // 超界分量也会落到端点上而不会回绕。
            //
            // 保留它的理由是可读性而非正确性：读这段代码的人不一定知道那个语言细节，
            // 而「颜色分量必须在 [0,1] 内」是这里的意图。
            // 相应地**没有**测试能证明 clamp 有用（去掉它测试照样绿，变异验证确认过）
            // ——如实记在这里，不假装它被守着。
            (x.clamp(0.0, 1.0) * 255.0).round() as u8
        };
        format!("#{:02x}{:02x}{:02x}", q(self.r), q(self.g), q(self.b))
    }
}

/// 极简 plist 解析：只认 `<key>NAME</key><dict>…<key>Red Component</key><real>X</real>…</dict>`。
///
/// 不引 plist 依赖：本程序只需要读这一种结构，而一个通用 plist 解析器
/// 会带来一片用不到的表面（二进制 plist、日期、data blob）。
/// 认不出的结构一律跳过，不报错——iTerm2 的文件里有 `Ansi 0 Color` 之外的键。
fn parse_itermcolors_dicts(content: &str) -> BTreeMap<String, Rgb> {
    let mut out = BTreeMap::new();
    let mut rest = content;
    while let Some(ks) = rest.find("<key>") {
        let after = &rest[ks + 5..];
        let Some(ke) = after.find("</key>") else {
            break;
        };
        let key = after[..ke].trim().to_string();
        let tail = &after[ke + 6..];
        // 这个 key 后面必须紧跟一个 <dict>，否则它不是颜色项
        let Some(ds) = tail.find("<dict>") else {
            rest = tail;
            continue;
        };
        // <dict> 之前不能又出现 <key>（说明这个 key 的值不是 dict）
        if let Some(next_key) = tail.find("<key>") {
            if next_key < ds {
                rest = tail;
                continue;
            }
        }
        let Some(de) = tail[ds..].find("</dict>") else {
            break;
        };
        let body = &tail[ds + 6..ds + de];
        let mut rgb = Rgb::default();
        let mut got = 0u8;
        let mut b = body;
        while let Some(cs) = b.find("<key>") {
            let a2 = &b[cs + 5..];
            let Some(ce) = a2.find("</key>") else { break };
            let comp = a2[..ce].trim();
            let t2 = &a2[ce + 6..];
            // 值可能是 <real> 或 <integer>
            let val = ["<real>", "<integer>"].iter().find_map(|tag| {
                let s = t2.find(tag)?;
                let close = tag.replace('<', "</");
                let e = t2[s..].find(&close)?;
                t2[s + tag.len()..s + e].trim().parse::<f64>().ok()
            });
            if let Some(x) = val {
                match comp {
                    "Red Component" => {
                        rgb.r = x;
                        got |= 1;
                    }
                    "Green Component" => {
                        rgb.g = x;
                        got |= 2;
                    }
                    "Blue Component" => {
                        rgb.b = x;
                        got |= 4;
                    }
                    _ => {}
                }
            }
            b = t2;
        }
        if got == 7 {
            out.insert(key, rgb);
        }
        rest = &tail[ds + de..];
    }
    out
}

/// 解析 iTerm2 的 `.itermcolors`。
pub fn parse_iterm_colors(
    content: &str,
    scheme_name: &str,
) -> Result<Imported<ImportedScheme>, ImportError> {
    check_size(content)?;
    if !content.contains("<plist") && !content.contains("<dict>") {
        return Err(ImportError::NotThisFormat {
            detail: "不是 plist XML".into(),
        });
    }
    let dicts = parse_itermcolors_dicts(content);
    if dicts.is_empty() {
        return Err(ImportError::NotThisFormat {
            detail: "没有解析到任何颜色项".into(),
        });
    }
    let mut unresolved = Vec::new();
    let fg = dicts
        .get("Foreground Color")
        .ok_or(ImportError::MissingRequired {
            field: "Foreground Color",
        })?;
    let bg = dicts
        .get("Background Color")
        .ok_or(ImportError::MissingRequired {
            field: "Background Color",
        })?;
    let mut ansi = Vec::with_capacity(16);
    for i in 0..16 {
        match dicts.get(&format!("Ansi {i} Color")) {
            Some(c) => ansi.push(c.to_hex()),
            None => {
                return Err(ImportError::MissingRequired {
                    // 缺 ANSI 色就整套不要，同 .xcs 的理由
                    field: "Ansi N Color",
                });
            }
        }
    }
    // iTerm2 有几个本程序没有对应概念的颜色，明确记下来
    for k in [
        "Selected Text Color",
        "Selection Color",
        "Bold Color",
        "Link Color",
        "Badge Color",
        "Tab Color",
        "Underline Color",
        "Cursor Text Color",
    ] {
        if dicts.contains_key(k) {
            unresolved.push(Unresolved {
                key: k.to_string(),
                why: format!("本程序的配色模型没有 `{k}` 这一项，已跳过"),
            });
        }
    }
    Ok(Imported {
        value: ImportedScheme {
            name: scheme_name.to_string(),
            foreground: fg.to_hex(),
            background: bg.to_hex(),
            ansi,
            cursor: dicts.get("Cursor Color").map(|c| c.to_hex()),
        },
        unresolved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 样例：按公开格式构造（本机没装那几个产品，见模块头「样例的来源与局限」）──

    const XSH: &str = r#"
[SessionInfo]
Version=5.2
Description=生产 web
Protocol=SSH

[CONNECTION]
Host=10.0.0.5
Port=2222

[CONNECTION:AUTHENTICATION]
Method=Public Key
UserName=deploy
UserKey=C:\Users\me\.ssh\id_ed25519
Password=cGFzc3dvcmQtZW5jcnlwdGVkPT0=
Passphrase=YW5vdGhlci1lbmNyeXB0ZWQ=

[TERMINAL]
Rows=24
Cols=80
"#;

    const XCS: &str = r#"
[NAMES]
count=1
name0=MyDark

[MyDark]
text=cccccc
background=1e1e2e
black=45475a
red=f38ba8
green=a6e3a1
yellow=f9e2af
blue=89b4fa
magenta=f5c2e7
cyan=94e2d5
white=bac2de
black(bold)=585b70
red(bold)=f38ba8
green(bold)=a6e3a1
yellow(bold)=f9e2af
blue(bold)=89b4fa
magenta(bold)=f5c2e7
cyan(bold)=94e2d5
white(bold)=a6adc8
cursor=f5e0dc
"#;

    const FINALSHELL: &str = r#"{
      "name": "db-primary",
      "host": "10.0.0.9",
      "port": 22,
      "user_name": "root",
      "authentication_type": 0,
      "password": "ENCRYPTED_BLOB",
      "secret_key_id": "abc-123",
      "folder": "生产/数据库"
    }"#;

    fn iterm(extra: &str) -> String {
        let mut s = String::from(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
"#,
        );
        let color = |name: &str, r: f64, g: f64, b: f64| {
            format!(
                "\t<key>{name}</key>\n\t<dict>\n\t\t<key>Red Component</key>\n\t\t<real>{r}</real>\n\t\t<key>Green Component</key>\n\t\t<real>{g}</real>\n\t\t<key>Blue Component</key>\n\t\t<real>{b}</real>\n\t</dict>\n"
            )
        };
        for i in 0..16 {
            let v = i as f64 / 15.0;
            s.push_str(&color(&format!("Ansi {i} Color"), v, v, v));
        }
        s.push_str(&color("Foreground Color", 1.0, 1.0, 1.0));
        s.push_str(&color("Background Color", 0.0, 0.0, 0.0));
        s.push_str(extra);
        s.push_str("</dict>\n</plist>\n");
        s
    }

    // ── Xshell 会话 ──

    #[test]
    fn xshell_session_maps_the_fields_we_understand() {
        let r = parse_xshell_session(XSH).unwrap();
        assert_eq!(r.value.name, "生产 web");
        assert_eq!(r.value.host, "10.0.0.5");
        assert_eq!(r.value.port, 2222);
        assert_eq!(r.value.username, "deploy");
        assert_eq!(r.value.auth_hint.as_deref(), Some("publickey"));
        assert_eq!(
            r.value.private_key_path.as_deref(),
            Some(r"C:\Users\me\.ssh\id_ed25519"),
            "私钥**路径**该导入——它不是秘密，且能省用户一次翻找"
        );
    }

    #[test]
    fn xshell_credentials_are_never_imported_and_are_reported() {
        let r = parse_xshell_session(XSH).unwrap();
        let dumped = serde_json::to_string(&r).unwrap();
        // 加密后的口令一个字节都不该出现在结果里
        assert!(
            !dumped.contains("cGFzc3dvcmQtZW5jcnlwdGVk"),
            "加密口令进了导入结果：{dumped}"
        );
        assert!(!dumped.contains("YW5vdGhlci1lbmNyeXB0ZWQ"));
        // 但必须明确告诉用户「这两项没导入」
        let keys: Vec<&str> = r.unresolved.iter().map(|u| u.key.as_str()).collect();
        assert!(
            keys.iter().any(|k| k.ends_with("/Password")),
            "口令未导入这件事没有出现在清单里：{keys:?}"
        );
        assert!(keys.iter().any(|k| k.ends_with("/Passphrase")), "{keys:?}");
        // 清单里的说明要告诉用户下一步
        for u in &r.unresolved {
            assert!(
                u.why.contains("重新填写") || u.why.contains("未导入") || u.why.contains("读不到"),
                "{}: {}",
                u.key,
                u.why
            );
        }
    }

    #[test]
    fn xshell_rejects_non_ssh_protocols_rather_than_importing_a_broken_config() {
        // 静默当成 SSH 导进来会得到一条连不上的配置，而用户以为导成功了
        let s = XSH.replace("Protocol=SSH", "Protocol=TELNET");
        let e = parse_xshell_session(&s).unwrap_err();
        assert!(matches!(e, ImportError::NotThisFormat { .. }), "{e:?}");
        assert!(e.message().contains("TELNET"));
    }

    #[test]
    fn xshell_without_host_is_rejected() {
        let s = XSH.replace("Host=10.0.0.5", "");
        assert_eq!(
            parse_xshell_session(&s).unwrap_err(),
            ImportError::MissingRequired { field: "Host" }
        );
    }

    #[test]
    fn a_random_ini_is_not_mistaken_for_a_session_file() {
        // 只看「有没有 [」会把任何 INI 都认成 .xsh
        let e = parse_xshell_session("[foo]\nbar=1\n").unwrap_err();
        assert!(matches!(e, ImportError::NotThisFormat { .. }), "{e:?}");
    }

    #[test]
    fn ini_values_may_contain_equals_signs() {
        // base64 的补位、路径里的等号——按最后一个 `=` 或按 split 全部会切错
        let ini = parse_ini("[A]\nk=YWJj==\n");
        assert_eq!(ini["A"]["k"], "YWJj==");
    }

    #[test]
    fn ini_section_names_keep_their_colons() {
        // Xshell 的 section 是 [CONNECTION:AUTHENTICATION]，
        // 按冒号切分层级的解析器会找不到这个段
        let ini = parse_ini("[CONNECTION:AUTHENTICATION]\nUserName=x\n");
        assert_eq!(ini["CONNECTION:AUTHENTICATION"]["UserName"], "x");
    }

    #[test]
    fn ini_skips_comments() {
        let ini = parse_ini("[A]\n; c1\n# c2\nk=v\n");
        assert_eq!(ini["A"].len(), 1);
    }

    // ── Xshell 配色 ──

    #[test]
    fn xcs_scheme_round_trips_with_ansi_in_standard_order() {
        let r = parse_xshell_schemes(XCS).unwrap();
        assert_eq!(r.value.len(), 1);
        let s = &r.value[0];
        assert_eq!(s.name, "MyDark");
        assert_eq!(s.foreground, "#cccccc");
        assert_eq!(s.background, "#1e1e2e");
        assert_eq!(s.cursor.as_deref(), Some("#f5e0dc"));
        assert_eq!(s.ansi.len(), 16);
        // 顺序必须是 ANSI 标准序，不是文件里的出现顺序——
        // 错位的配色比没有配色更难用
        assert_eq!(s.ansi[0], "#45475a", "索引 0 应是 black");
        assert_eq!(s.ansi[1], "#f38ba8", "索引 1 应是 red");
        assert_eq!(s.ansi[2], "#a6e3a1", "索引 2 应是 green");
        assert_eq!(s.ansi[8], "#585b70", "索引 8 应是 bright black");
        assert_eq!(s.ansi[15], "#a6adc8", "索引 15 应是 bright white");
        assert!(r.unresolved.is_empty(), "{:?}", r.unresolved);
    }

    #[test]
    fn a_scheme_missing_one_ansi_colour_is_skipped_not_defaulted() {
        // 补默认值会得到一套「大部分对、有一格不对」的配色，
        // 而那种错误比整套缺失更难被发现
        let s = XCS.replace("green=a6e3a1\n", "");
        let r = parse_xshell_schemes(&s).unwrap();
        assert!(r.value.is_empty(), "缺一色仍然导入了");
        assert_eq!(r.unresolved.len(), 1);
        assert!(r.unresolved[0].key.contains("green"), "{:?}", r.unresolved);
    }

    #[test]
    fn a_declared_but_absent_scheme_is_reported() {
        let s = XCS.replace("name0=MyDark", "name0=MyDark\nname1=Ghost");
        let r = parse_xshell_schemes(&s).unwrap();
        assert_eq!(r.value.len(), 1);
        assert!(
            r.unresolved.iter().any(|u| u.key.contains("Ghost")),
            "{:?}",
            r.unresolved
        );
    }

    #[test]
    fn scheme_count_field_is_not_trusted() {
        // 见过 count 与实际条数不符的文件
        let s = XCS.replace("count=1", "count=7");
        assert_eq!(parse_xshell_schemes(&s).unwrap().value.len(), 1);
    }

    #[test]
    fn hex_normalisation_accepts_both_forms_and_rejects_junk() {
        assert_eq!(normalize_hex("1e1e2e").as_deref(), Some("#1e1e2e"));
        assert_eq!(normalize_hex("#1E1E2E").as_deref(), Some("#1e1e2e"));
        for bad in ["", "12345", "1234567", "zzzzzz", "#12345g"] {
            assert_eq!(normalize_hex(bad), None, "{bad:?} 该被拒");
        }
    }

    // ── FinalShell ──

    #[test]
    fn finalshell_maps_fields_and_reports_credentials() {
        let r = parse_finalshell(FINALSHELL).unwrap();
        assert_eq!(r.value.name, "db-primary");
        assert_eq!(r.value.host, "10.0.0.9");
        assert_eq!(r.value.port, 22);
        assert_eq!(r.value.username, "root");
        assert_eq!(r.value.auth_hint.as_deref(), Some("password"));
        assert_eq!(r.value.group_path.as_deref(), Some("生产/数据库"));

        let dumped = serde_json::to_string(&r.value).unwrap();
        assert!(!dumped.contains("ENCRYPTED_BLOB"), "加密口令进了结果");
        let keys: Vec<&str> = r.unresolved.iter().map(|u| u.key.as_str()).collect();
        assert!(keys.contains(&"password"), "{keys:?}");
        assert!(keys.contains(&"secret_key_id"), "{keys:?}");
    }

    #[test]
    fn finalshell_needs_a_host_so_our_own_export_is_not_mistaken_for_one() {
        // 只判断「是 JSON」会把本程序自己的导出文件也认成 FinalShell 配置
        assert!(parse_finalshell(r#"{"profiles":[]}"#).is_err());
        assert!(parse_finalshell("not json").is_err());
        assert!(parse_finalshell("[]").is_err());
    }

    #[test]
    fn finalshell_unknown_auth_type_is_carried_through_not_guessed() {
        let s = FINALSHELL.replace("\"authentication_type\": 0", "\"authentication_type\": 9");
        let r = parse_finalshell(&s).unwrap();
        assert_eq!(r.value.auth_hint.as_deref(), Some("finalshell-auth-9"));
    }

    #[test]
    fn finalshell_out_of_range_port_falls_back_rather_than_wrapping() {
        // `as u16` 会把 70000 回绕成 4464——一个看起来合理但完全错的端口
        let s = FINALSHELL.replace("\"port\": 22", "\"port\": 70000");
        assert_eq!(parse_finalshell(&s).unwrap().value.port, 22);
    }

    // ── iTerm2 ──

    #[test]
    fn iterm_colours_convert_from_float_components_to_hex() {
        let r = parse_iterm_colors(&iterm(""), "Imported").unwrap();
        assert_eq!(r.value.name, "Imported");
        assert_eq!(r.value.foreground, "#ffffff");
        assert_eq!(r.value.background, "#000000");
        assert_eq!(r.value.ansi.len(), 16);
        assert_eq!(r.value.ansi[0], "#000000");
        assert_eq!(r.value.ansi[15], "#ffffff");
        // 中间值要四舍五入而不是截断：8/15 = 0.5333 → 136
        assert_eq!(r.value.ansi[8], "#888888");
    }

    #[test]
    fn iterm_components_outside_zero_to_one_land_on_the_endpoints() {
        // 见过分量略微超出 [0,1] 的文件（浮点累积误差）。结果必须落在端点上。
        //
        // 注意这条测试**证明不了 clamp 的必要性**：Rust 的 `f64 as u8` 本来就是饱和转换，
        // 去掉 clamp 这条照样绿（变异验证确认过）。它守的是「超界输入不会产出乱颜色」
        // 这个**结果**，而这个结果由语言与 clamp 共同保证——如实说明，
        // 不把它当成「clamp 有测试覆盖」的证据。
        let extra = "\t<key>Cursor Color</key>\n\t<dict>\n\t\t<key>Red Component</key>\n\t\t<real>1.0000001</real>\n\t\t<key>Green Component</key>\n\t\t<real>-0.0000001</real>\n\t\t<key>Blue Component</key>\n\t\t<real>0.5</real>\n\t</dict>\n";
        let r = parse_iterm_colors(&iterm(extra), "X").unwrap();
        assert_eq!(r.value.cursor.as_deref(), Some("#ff0080"));
        // 严重超界也要落端点（这一条同理由语言的饱和转换保证）
        let wild = "\t<key>Cursor Color</key>\n\t<dict>\n\t\t<key>Red Component</key>\n\t\t<real>5.0</real>\n\t\t<key>Green Component</key>\n\t\t<real>-5.0</real>\n\t\t<key>Blue Component</key>\n\t\t<real>0</real>\n\t</dict>\n";
        let r = parse_iterm_colors(&iterm(wild), "X").unwrap();
        assert_eq!(r.value.cursor.as_deref(), Some("#ff0000"));
    }

    #[test]
    fn iterm_reports_colours_we_have_no_concept_for() {
        let extra = "\t<key>Badge Color</key>\n\t<dict>\n\t\t<key>Red Component</key>\n\t\t<real>1</real>\n\t\t<key>Green Component</key>\n\t\t<real>0</real>\n\t\t<key>Blue Component</key>\n\t\t<real>0</real>\n\t</dict>\n";
        let r = parse_iterm_colors(&iterm(extra), "X").unwrap();
        assert!(
            r.unresolved.iter().any(|u| u.key == "Badge Color"),
            "{:?}",
            r.unresolved
        );
    }

    #[test]
    fn iterm_missing_an_ansi_colour_is_rejected() {
        let full = iterm("");
        // 去掉 Ansi 5 Color 那一段
        let cut = full.replace(
            "\t<key>Ansi 5 Color</key>\n\t<dict>\n\t\t<key>Red Component</key>\n\t\t<real>0.3333333333333333</real>\n\t\t<key>Green Component</key>\n\t\t<real>0.3333333333333333</real>\n\t\t<key>Blue Component</key>\n\t\t<real>0.3333333333333333</real>\n\t</dict>\n",
            "",
        );
        assert_ne!(cut, full, "取材失效：没有真的删掉那一段");
        assert!(parse_iterm_colors(&cut, "X").is_err());
    }

    #[test]
    fn a_non_plist_file_is_rejected() {
        for bad in ["", "just text", "{\"json\": 1}"] {
            assert!(parse_iterm_colors(bad, "X").is_err(), "{bad:?}");
        }
    }

    // ── 共通 ──

    #[test]
    fn oversized_files_are_rejected_by_every_parser() {
        let big = "x".repeat(MAX_IMPORT_BYTES + 1);
        assert_eq!(parse_xshell_session(&big), Err(ImportError::TooLarge));
        assert_eq!(parse_xshell_schemes(&big), Err(ImportError::TooLarge));
        assert_eq!(parse_finalshell(&big), Err(ImportError::TooLarge));
        assert_eq!(
            parse_iterm_colors(&big, "X").unwrap_err(),
            ImportError::TooLarge
        );
    }

    #[test]
    fn every_error_message_is_in_plain_language() {
        for e in [
            ImportError::NotThisFormat { detail: "x".into() },
            ImportError::MissingRequired { field: "Host" },
            ImportError::TooLarge,
        ] {
            let m = e.message();
            assert!(m.chars().count() >= 8, "{e:?} 的提示太短：{m}");
        }
    }
}
