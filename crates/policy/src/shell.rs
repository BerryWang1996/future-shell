//! shell 感知分解：把一段命令文本拆成「简单命令」的集合（总设计 §4.3 硬约束 1）。
//!
//! # 为什么不能对原始字符串做正则
//!
//! §4.3 明令禁止「对原始字符串直接做正则/关键词匹配」。理由是可绕过性——下面每一条
//! 在 shell 里都会真的删掉东西，而对字符串做匹配的引擎会把它们当成别的东西：
//!
//! | 输入 | 关键词匹配看到的 | 实际发生的 |
//! |---|---|---|
//! | `ls; rm -rf ~` | 首词是 `ls`，白名单命中 | 删掉家目录 |
//! | `ls $(rm -rf /tmp/x)` | 首词是 `ls` | 先跑替换里的 `rm` |
//! | `a=rm; $a -rf /` | 没有 `rm` 这个词 | 拼装出 `rm -rf /` |
//! | `r''m -rf /` | 没有 `rm` 这个词 | 引号被 shell 吃掉后就是 `rm` |
//! | `echo cm0gLXJmIC8K \| base64 -d \| sh` | 只有 `echo`/`base64` | 解码后执行 `rm -rf /` |
//!
//! 所以必须先按 shell 的词法把输入**拆开**（顶层列表 / 管道 / 命令替换 / 子 shell
//! 都要拆），再对拆出来的每个组分分别做语义判定，整体取最高级。
//!
//! # 失败闭合是设计目标，不是兜底
//!
//! 本解析器**不追求**解析全部 bash 语法——那既不可能，而且追求它会引向最坏的一种失败：
//! 「解析器以为自己看懂了，其实没有」。它的目标是二选一：要么给出一个可以信赖的分解，
//! 要么**明确报错**。报错由 [`crate::rules`] 翻译成 `Dangerous`
//! （M2 出口原文：「解析错误 → dangerous」）。
//!
//! 因此凡是本解析器没有把握的构造（未闭合引号、嵌套过深、超长输入、无法配对的括号）
//! 一律走 [`ParseError`]，绝不「尽力猜一下」。

use std::collections::BTreeMap;

/// 输入长度上限。
///
/// 不是性能考虑而是**语义**考虑：策略引擎面对的是「AI 生成的一条命令」或「用户敲的一行」，
/// 不是脚本文件。超过这个量级的输入要么是攻击面（拿超长输入撑爆解析器），
/// 要么是本引擎无权判定的东西（整个脚本）——两种都该拒绝而不是硬算。
///
/// 与 highlights.ts 那次灾难性回溯的教训同一条：**尺寸闸不能替代算法上界，
/// 但算法上界也不能省掉尺寸闸**。本解析器是单遍线性扫描（无回溯），
/// 尺寸闸在这里是第二道而不是唯一一道。
pub const MAX_INPUT_LEN: usize = 16 * 1024;

/// 嵌套深度上限（`$( $( $( … ) ) )`、`( ( ( … ) ) )`）。
///
/// 递归下降解析器的栈深由输入控制，没有上限就是一条 stack overflow ——
/// 而 stack overflow 在 Rust 里是 **abort**，不是 panic，捕获不了。
/// 8 层远超任何真实命令（人写的命令极少超过 2 层），够用且安全。
pub const MAX_DEPTH: usize = 8;

/// 单条输入允许拆出的简单命令数上限。
///
/// `a;a;a;a;…` 重复十万次是合法 shell，但对策略引擎没有意义，且会让规则层
/// 做十万次判定。到顶即报错（失败闭合），不静默截断——截断会让「后面那些命令」
/// 完全不受审查，那正是最坏的漏检形态。
pub const MAX_COMMANDS: usize = 512;

/// 解析失败的原因。
///
/// 每一种都必须导向 `Dangerous`。带上具体原因只为可诊断（审计与 UI 要能说清
/// 「为什么这条命令被拒」），分级判定本身不看是哪一种。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// 单引号没闭合。
    UnterminatedSingleQuote,
    /// 双引号没闭合。
    UnterminatedDoubleQuote,
    /// 反引号没闭合。
    UnterminatedBacktick,
    /// `$(` 没闭合。
    UnterminatedSubstitution,
    /// `${` 没闭合。
    UnterminatedParamExpansion,
    /// `(` / `)` 不配对。
    UnbalancedParen,
    /// `{` / `}` 不配对。
    UnbalancedBrace,
    /// 反斜杠出现在输入末尾（续行符，说明这不是完整的一条命令）。
    TrailingBackslash,
    /// 嵌套超过 [`MAX_DEPTH`]。
    TooDeep,
    /// 输入超过 [`MAX_INPUT_LEN`]。
    TooLong,
    /// 拆出的简单命令超过 [`MAX_COMMANDS`]。
    TooManyCommands,
    /// heredoc 的结束标记没出现。
    UnterminatedHeredoc,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::UnterminatedSingleQuote => "单引号未闭合",
            Self::UnterminatedDoubleQuote => "双引号未闭合",
            Self::UnterminatedBacktick => "反引号未闭合",
            Self::UnterminatedSubstitution => "`$(` 未闭合",
            Self::UnterminatedParamExpansion => "`${` 未闭合",
            Self::UnbalancedParen => "括号不配对",
            Self::UnbalancedBrace => "花括号不配对",
            Self::TrailingBackslash => "以反斜杠结尾（续行未完成）",
            Self::TooDeep => "嵌套过深",
            Self::TooLong => "输入过长",
            Self::TooManyCommands => "命令数过多",
            Self::UnterminatedHeredoc => "heredoc 结束标记未出现",
        };
        f.write_str(s)
    }
}

/// 一个词在**去引号之前**出现过哪些 shell 构造。
///
/// 这些标记是白名单判定的关键：§4.3 硬约束 2 说「任何包含 shell 元字符/控制操作符的
/// 命令一律 ≥ write，永不 read_only」。去引号之后 `r''m` 和 `rm` 长得一样，
/// 分不出来——所以必须在去引号的**同时**把「这里原本有引号」记下来。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WordTraits {
    /// 出现过单引号。
    pub quoted_single: bool,
    /// 出现过双引号。
    pub quoted_double: bool,
    /// 出现过反斜杠转义。
    pub escaped: bool,
    /// 出现过 `$VAR` / `${VAR}` 参数展开。
    pub param_expansion: bool,
    /// 出现过 `$(…)` 或 `` `…` `` 命令替换。
    pub command_substitution: bool,
    /// 出现过 `$((…))` 算术展开。
    pub arith_expansion: bool,
    /// 出现过 `*` / `?` / `[…]` 通配。
    pub glob: bool,
    /// 出现过 `~` 家目录展开（在词首）。
    pub tilde: bool,
    /// 出现过 `<(…)` / `>(…)` 进程替换。
    pub process_substitution: bool,
    /// 这个词里有**取值定不下来**的展开。
    ///
    /// 与 `param_expansion` 的区别是关键：`d=/tmp/build; rm -rf $d` 里的 `$d`
    /// 有 `param_expansion` 但**没有** `unresolved`——我们把它还原成了 `/tmp/build`，
    /// 完全知道它会删什么。而 `rm -rf $TARGET` 里的 `$TARGET` 定不了值，
    /// 那才是「不知道会删哪里」。
    ///
    /// 只看 `param_expansion` 会把前者也判成「目标未知」，于是每一条用变量存路径的
    /// 正常脚本都要强确认——那正是第一版的表现。
    pub unresolved: bool,
}

impl WordTraits {
    /// 这个词里有没有任何「不是普通字面量」的东西。
    ///
    /// 白名单要求「仅匹配单一程序名 + **普通参数**」——凡是带了下面任一构造的词，
    /// 它的最终值不由这段文本单独决定（引号会被吃掉、变量会被展开、通配会被扩张），
    /// 因此无法静态判定为只读。
    ///
    /// **`glob` 与 `tilde` 刻意不算在内。** 它们的展开结果仍是路径字面量，
    /// `ls *.txt` 与 `ls ~/logs` 是日常只读操作；把它们一律推到 write 会让白名单形同虚设，
    /// 用户对每一次 `ls *` 都要点确认，几天之后就会去关掉总开关——那比放行 `ls *` 危险得多。
    /// 通配展开到敏感路径的情形由 [`crate::rules`] 的路径谓词单独管（§4.3 硬约束 2 末句）。
    pub fn is_plain(self) -> bool {
        !(self.quoted_single
            || self.quoted_double
            || self.escaped
            || self.param_expansion
            || self.command_substitution
            || self.arith_expansion
            || self.process_substitution)
    }
}

/// 一个已去引号的词。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// 去引号、并对**可静态确定**的变量做过替换之后的文本。
    pub text: String,
    /// 原始片段（诊断用；也是「这里原本长什么样」的唯一记录）。
    pub raw: String,
    pub traits: WordTraits,
}

impl Word {
    fn plain(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            raw: text.clone(),
            text,
            traits: WordTraits::default(),
        }
    }
}

/// 重定向的方向。分方向是因为策略含义完全不同：
/// 读入无害，**写出**可能覆盖任意文件（`echo x > ~/.ssh/authorized_keys`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectKind {
    /// `<` / `<<<` / `<<`：读入。
    Input,
    /// `>` / `>|`：覆盖写。
    Output,
    /// `>>`：追加写。
    Append,
    /// `>&` / `<&` / `&>`：fd 复制或同时重定向。
    Duplicate,
}

impl RedirectKind {
    /// 这次重定向会不会**改写**目标。
    pub fn writes(self) -> bool {
        matches!(self, Self::Output | Self::Append)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub kind: RedirectKind,
    /// 目标（文件路径或 fd）。
    pub target: Word,
}

/// 一条简单命令：可选的赋值前缀 + argv + 自己的重定向。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SimpleCommand {
    /// 赋值前缀（`FOO=bar cmd`）与独立赋值语句（`FOO=bar`）。
    pub assignments: Vec<(String, Word)>,
    /// argv。**可能为空**——纯赋值语句就没有 argv。
    pub argv: Vec<Word>,
    pub redirects: Vec<Redirect>,
    /// 喂给这条命令的 heredoc 正文。
    ///
    /// 正文对**大多数**命令是数据（`cat <<EOF` 就是打印一段文本），但对**解释器**是代码：
    /// `bash <<EOF … EOF` 会把正文当脚本执行。规则层据此区别对待——
    /// 不带着正文走，`bash <<EOF\nrm -rf /\nEOF` 就只剩「有个 heredoc」这么一条理由。
    pub heredoc_bodies: Vec<String>,
    /// 这条命令是在哪一层拆出来的：0 = 顶层，≥1 = 命令替换/子 shell 内部。
    ///
    /// 规则层用它区分「用户打算跑的」与「藏在替换里顺手跑的」——后者本身就是可疑信号。
    pub depth: usize,
}

impl SimpleCommand {
    /// 程序名（argv[0] 去引号后的文本）。纯赋值语句返回 `None`。
    pub fn program(&self) -> Option<&str> {
        self.argv.first().map(|w| w.text.as_str())
    }

    /// 程序名的 basename（`/usr/bin/rm` → `rm`，`./rm` → `rm`）。
    ///
    /// 必须按 basename 判定：`/bin/rm -rf /` 与 `rm -rf /` 是同一件事，
    /// 只认 `rm` 的引擎会被一个绝对路径绕过。
    pub fn program_basename(&self) -> Option<&str> {
        self.program().map(basename)
    }

    /// argv[1..]。
    pub fn args(&self) -> &[Word] {
        self.argv.get(1..).unwrap_or(&[])
    }

    /// 这条命令里的每个词是否都是普通字面量。
    pub fn all_words_plain(&self) -> bool {
        self.argv.iter().all(|w| w.traits.is_plain())
            && self.redirects.iter().all(|r| r.target.traits.is_plain())
            && self.assignments.iter().all(|(_, w)| w.traits.is_plain())
    }
}

/// 取路径的最后一段。`/` 结尾时退回上一段（`/usr/bin/` → `bin`）。
pub fn basename(p: &str) -> &str {
    let p = p.trim_end_matches(['/', '\\']);
    match p.rsplit_once(['/', '\\']) {
        Some((_, last)) if !last.is_empty() => last,
        _ => p,
    }
}

/// 一条管道：`a | b | c` 的三个阶段。
///
/// 必须保留管道结构而不是打平成命令集合：`curl x | sh` 的危险性**不在** `curl`
/// 也不在 `sh`，而在「网络内容流进了解释器」这个连接关系。打平之后这个关系就丢了。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pipeline {
    pub stages: Vec<SimpleCommand>,
    /// 这条管道所在的嵌套层。
    pub depth: usize,
}

/// 整段输入的分解结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Script {
    /// 所有层级的管道，扁平存放（各自带 `depth`）。
    ///
    /// **顺序是执行顺序，不是书写顺序。** 命令替换里的命令排在包含它的命令**之前**——
    /// 因为它确实先跑：`ls $(rm -rf /tmp/x)` 里 shell 必须先求出替换的值才能调 `ls`。
    /// 这个顺序对分级判定没有影响（取最高级与顺序无关），但审计行按这个顺序落库时，
    /// 读的人看到的就是真实发生次序。
    pub pipelines: Vec<Pipeline>,
    /// 本段里**定义过**的函数名（按定义顺序）。
    ///
    /// 记它只为一件事：fork 炸弹的判据是「函数体里递归调用自己 + 后台执行」，
    /// 而`:(){ :|:& };:`里那个`:`到底是自我调用还是某个外部命令，只有知道
    /// 定义过哪些函数才分得出来。
    pub functions: Vec<String>,
    pub traits: ScriptTraits,
}

/// 整段输入里出现过的、与分级相关的构造。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptTraits {
    /// 出现过命令替换 `$(…)` / `` `…` ``。
    pub command_substitution: bool,
    /// 出现过 heredoc。
    pub heredoc: bool,
    /// 出现过子 shell `(…)` 或命令组 `{…}`。
    pub subshell: bool,
    /// 出现过后台执行 `&`。
    pub background: bool,
    /// 出现过函数定义 `f() { … }`。
    pub function_definition: bool,
    /// 出现过进程替换 `<(…)` / `>(…)`。
    pub process_substitution: bool,
    /// 有变量的取值**无法静态确定**（赋值来自外部或来自命令替换）。
    ///
    /// 这一条是变量拼装（`a=rm; $a -rf /`）的对偶信号：能定值的我们替换掉并照常判定，
    /// 定不了值的就必须失败闭合——`$CMD -rf /` 里的 `$CMD` 可能是任何东西。
    pub unresolved_expansion: bool,
}

impl Script {
    /// 遍历所有层级的所有简单命令。
    pub fn commands(&self) -> impl Iterator<Item = &SimpleCommand> {
        self.pipelines.iter().flat_map(|p| p.stages.iter())
    }
}

/// 把一段命令文本分解成 [`Script`]。
///
/// 失败即失败闭合，不做任何「尽力猜测」。
pub fn parse(input: &str) -> Result<Script, ParseError> {
    if input.len() > MAX_INPUT_LEN {
        return Err(ParseError::TooLong);
    }
    let mut p = Parser {
        script: Script::default(),
        vars: BTreeMap::new(),
        commands_emitted: 0,
    };
    p.parse_script(input, 0)?;
    Ok(p.script)
}

struct Parser {
    script: Script,
    /// 已知的、取值可静态确定的变量（用于变量拼装的还原）。
    vars: BTreeMap<String, String>,
    commands_emitted: usize,
}

impl Parser {
    /// 解析一段脚本文本（顶层或某个替换/子 shell 的内部）。
    fn parse_script(&mut self, s: &str, depth: usize) -> Result<(), ParseError> {
        if depth > MAX_DEPTH {
            return Err(ParseError::TooDeep);
        }
        for segment in split_lists(s)? {
            if segment.background {
                self.script.traits.background = true;
            }
            self.parse_and_or(&segment.text, depth)?;
        }
        Ok(())
    }

    fn parse_and_or(&mut self, s: &str, depth: usize) -> Result<(), ParseError> {
        let stages = split_pipes(s)?;
        if stages.iter().all(|st| st.trim().is_empty()) {
            return Ok(());
        }
        let mut pipeline = Pipeline {
            stages: Vec::new(),
            depth,
        };
        for stage in stages {
            let stage = stage.trim();
            if stage.is_empty() {
                continue;
            }
            // 子 shell / 命令组：整段递归，本身不产生简单命令。
            if let Some(inner) = strip_group(stage)? {
                self.script.traits.subshell = true;
                self.parse_script(inner, depth + 1)?;
                continue;
            }
            // 函数定义 `f() { … }`：函数体递归；定义本身不执行。
            if let Some((name, body)) = split_function_def(stage)? {
                self.script.traits.function_definition = true;
                self.script.functions.push(name);
                self.parse_script(body, depth + 1)?;
                continue;
            }
            let cmd = self.parse_simple(stage, depth)?;
            self.commands_emitted += 1;
            if self.commands_emitted > MAX_COMMANDS {
                return Err(ParseError::TooManyCommands);
            }
            pipeline.stages.push(cmd);
        }
        if !pipeline.stages.is_empty() {
            self.script.pipelines.push(pipeline);
        }
        Ok(())
    }

    /// 解析一条简单命令：赋值前缀 + argv + 重定向。
    fn parse_simple(&mut self, s: &str, depth: usize) -> Result<SimpleCommand, ParseError> {
        let mut cmd = SimpleCommand {
            depth,
            ..Default::default()
        };
        let items = self.scan_items(s, depth)?;

        let mut seen_argv = false;
        let mut it = items.into_iter().peekable();
        while let Some(item) = it.next() {
            match item {
                Item::Word(w) => {
                    // 赋值前缀只在 argv 之前有效：`FOO=1 cmd` 是赋值，
                    // `cmd FOO=1` 里的 `FOO=1` 只是个普通参数（`make CC=gcc` 就是这样）。
                    if !seen_argv {
                        if let Some((name, value)) = split_assignment(&w) {
                            // 只有**纯字面量**的赋值才进符号表：`a=rm` 可以还原，
                            // `a=$(curl x)` 不行——后者必须标记为不可解析，
                            // 否则 `$a` 会被当成空串而整条命令看起来无害。
                            if value.traits.is_plain() {
                                self.vars.insert(name.clone(), value.text.clone());
                            } else {
                                self.script.traits.unresolved_expansion = true;
                            }
                            cmd.assignments.push((name, value));
                            continue;
                        }
                    }
                    seen_argv = true;
                    cmd.argv.push(w);
                }
                Item::Redirect(kind) => {
                    // 重定向目标是紧随其后的那个词。缺了目标是语法错误，
                    // 但 shell 会报错而不执行——按失败闭合处理更安全，不猜。
                    match it.next() {
                        Some(Item::Word(target)) => cmd.redirects.push(Redirect { kind, target }),
                        _ => return Err(ParseError::UnbalancedParen),
                    }
                }
                Item::Heredoc(body) => {
                    self.script.traits.heredoc = true;
                    // 正文在**这一层**按数据处理，不无条件当命令解析——
                    // `cat <<EOF\nrm -rf /\nEOF` 里那行 rm 是文本不是命令，
                    // 把它当命令会造成大量假阳性（而假阳性会把用户训练成闭眼点确认）。
                    // 「喂给解释器的正文是代码」这一判断需要知道程序名，属规则层的事，
                    // 所以正文原样带上去（见 `heredoc_bodies`）。
                    // 但正文里的命令替换在**任何**命令下都会执行，须就地递归。
                    for sub in collect_substitutions(&body)? {
                        self.script.traits.command_substitution = true;
                        self.parse_script(&sub, depth + 1)?;
                    }
                    cmd.heredoc_bodies.push(body);
                }
            }
        }
        Ok(cmd)
    }

    /// 扫描一条简单命令的内容，产出词 / 重定向符 / heredoc 正文。
    fn scan_items(&mut self, s: &str, depth: usize) -> Result<Vec<Item>, ParseError> {
        let mut out = Vec::new();
        let cs: Vec<char> = s.chars().collect();
        let mut i = 0usize;
        while i < cs.len() {
            let c = cs[i];
            if c.is_whitespace() {
                i += 1;
                continue;
            }
            // 注释：`#` 只在词首才是注释（`a#b` 里的 `#` 是普通字符）
            if c == '#' {
                break;
            }
            // 进程替换 `<(…)` / `>(…)`：整段是一个词，但**内容会执行**，须递归。
            //
            // 必须在这里处理而不是留给 read_word：`<` 是词终结符，read_word 碰到它会
            // 立即返回 consumed = 0，于是零进展守卫把整条命令判成解析失败。
            // 第一版把这段只写在 read_word 里（带 `i > start` 条件），因此只覆盖了
            // `cmd>(…)` 这种紧贴的写法，而真实写法 `diff <(a) <(b)` 里 `<` 恰在词首。
            if matches!(c, '<' | '>') && matches!(cs.get(i + 1), Some('(')) {
                let (inner, consumed) = read_balanced(&cs, i + 1, '(', ')')?;
                self.script.traits.process_substitution = true;
                self.parse_script(&inner, depth + 1)?;
                let mut w = Word::plain(format!("{c}({inner})"));
                w.traits.process_substitution = true;
                out.push(Item::Word(w));
                i += 1 + consumed;
                continue;
            }
            // 重定向操作符（含 fd 前缀 `2>`、`&>`）
            if let Some((kind, consumed, heredoc_tag)) = read_redirect(&cs, i) {
                i += consumed;
                if let Some(tag) = heredoc_tag {
                    // `<<TAG`：正文在后续行里，但**本行剩下的部分仍是这条命令的一部分**。
                    // `cat <<EOF > /etc/motd` 的 `> /etc/motd` 就在那儿；
                    // 第一版把它连同正文一起吞掉，于是那个写重定向凭空消失了。
                    let tail = rest_of_line(&cs, i);
                    let (body, consumed_body) = read_heredoc_body(&cs, i, &tag)?;
                    i += consumed_body;
                    out.push(Item::Heredoc(body));
                    // 把本行剩余部分接回来继续扫（可能含重定向、也可能含更多参数）
                    if !tail.trim().is_empty() {
                        out.extend(self.scan_items(&tail, depth)?);
                    }
                } else {
                    out.push(Item::Redirect(kind));
                }
                continue;
            }
            let (word, consumed, subs) = self.read_word(&cs, i)?;
            // **零进展守卫。** read_word 碰到词终结符会立即返回 consumed = 0；
            // 若上面没有别的分支消化掉那个字符，这个循环就原地空转。
            // 第一版漏了这条守卫，一个未处理的裸 `(` 让循环无限 push 空词直到分配失败
            // ——在 Windows 上报的是 STATUS_STACK_BUFFER_OVERRUN，看着像栈溢出，其实是 abort。
            // 守卫把「解析器有个洞」变成一条明确的失败闭合，而不是进程崩溃。
            if consumed == 0 {
                return Err(ParseError::UnbalancedParen);
            }
            i += consumed;
            for sub in subs {
                self.script.traits.command_substitution = true;
                self.parse_script(&sub, depth + 1)?;
            }
            out.push(Item::Word(word));
        }
        Ok(out)
    }

    /// 读一个词，处理引号 / 转义 / 展开，并把命令替换的正文收集出来。
    fn read_word(
        &mut self,
        cs: &[char],
        start: usize,
    ) -> Result<(Word, usize, Vec<String>), ParseError> {
        let mut text = String::new();
        let mut raw = String::new();
        let mut traits = WordTraits::default();
        let mut subs = Vec::new();
        let mut i = start;

        while i < cs.len() {
            let c = cs[i];
            // 词边界：空白与操作符
            if c.is_whitespace() || is_word_terminator(cs, i) {
                break;
            }
            match c {
                '\\' => {
                    raw.push(c);
                    i += 1;
                    let Some(&next) = cs.get(i) else {
                        return Err(ParseError::TrailingBackslash);
                    };
                    traits.escaped = true;
                    raw.push(next);
                    // 转义换行 = 续行，两个字符都消失
                    if next != '\n' {
                        text.push(next);
                    }
                    i += 1;
                }
                '\'' => {
                    traits.quoted_single = true;
                    raw.push(c);
                    i += 1;
                    loop {
                        let Some(&ch) = cs.get(i) else {
                            return Err(ParseError::UnterminatedSingleQuote);
                        };
                        raw.push(ch);
                        i += 1;
                        if ch == '\'' {
                            break;
                        }
                        // 单引号内一切都是字面量，连反斜杠也是
                        text.push(ch);
                    }
                }
                '"' => {
                    traits.quoted_double = true;
                    raw.push(c);
                    i += 1;
                    loop {
                        let Some(&ch) = cs.get(i) else {
                            return Err(ParseError::UnterminatedDoubleQuote);
                        };
                        if ch == '"' {
                            raw.push(ch);
                            i += 1;
                            break;
                        }
                        if ch == '\\' {
                            raw.push(ch);
                            i += 1;
                            let Some(&esc) = cs.get(i) else {
                                return Err(ParseError::UnterminatedDoubleQuote);
                            };
                            raw.push(esc);
                            traits.escaped = true;
                            // 双引号内反斜杠只对这几个字符有转义作用，其余原样保留两个字符
                            if matches!(esc, '$' | '`' | '"' | '\\' | '\n') {
                                if esc != '\n' {
                                    text.push(esc);
                                }
                            } else {
                                text.push('\\');
                                text.push(esc);
                            }
                            i += 1;
                            continue;
                        }
                        // 双引号内 `$` 与反引号仍然活跃
                        if ch == '$' || ch == '`' {
                            let (frag, consumed, sub, t) = self.read_dollar(cs, i)?;
                            raw.extend(cs[i..i + consumed].iter());
                            text.push_str(&frag);
                            traits = traits.merge(t);
                            if let Some(s) = sub {
                                subs.push(s);
                            }
                            i += consumed;
                            continue;
                        }
                        raw.push(ch);
                        text.push(ch);
                        i += 1;
                    }
                }
                '$' | '`' => {
                    let (frag, consumed, sub, t) = self.read_dollar(cs, i)?;
                    raw.extend(cs[i..i + consumed].iter());
                    text.push_str(&frag);
                    traits = traits.merge(t);
                    if let Some(s) = sub {
                        subs.push(s);
                    }
                    i += consumed;
                }
                '<' | '>' if i > start && matches!(cs.get(i + 1), Some('(')) => {
                    // 进程替换 `<(…)` / `>(…)`：内容会执行
                    let (inner, consumed) = read_balanced(cs, i + 1, '(', ')')?;
                    traits.process_substitution = true;
                    self.script.traits.process_substitution = true;
                    raw.extend(cs[i..i + 1 + consumed].iter());
                    subs.push(inner);
                    i += 1 + consumed;
                }
                '*' | '?' => {
                    traits.glob = true;
                    raw.push(c);
                    text.push(c);
                    i += 1;
                }
                '[' => {
                    traits.glob = true;
                    raw.push(c);
                    text.push(c);
                    i += 1;
                }
                '~' if i == start => {
                    traits.tilde = true;
                    raw.push(c);
                    text.push(c);
                    i += 1;
                }
                _ => {
                    raw.push(c);
                    text.push(c);
                    i += 1;
                }
            }
        }

        // 变量拼装的还原：整词恰好是一个已知变量的引用时，替换成它的字面值。
        //
        // 只在**整词**是引用时替换（`$a` 而非 `x$a`）。部分替换要处理拼接语义，
        // 而拼接出来的东西再拿去做白名单匹配已经不可靠——那种情形交给
        // `unresolved_expansion` 失败闭合，比猜一个值安全。
        Ok((Word { text, raw, traits }, i - start, subs))
    }

    /// 处理 `$` 开头的构造与反引号。返回（展开后的文本片段，消耗字符数，命令替换正文，附加 traits）。
    fn read_dollar(
        &mut self,
        cs: &[char],
        i: usize,
    ) -> Result<(String, usize, Option<String>, WordTraits), ParseError> {
        let mut t = WordTraits::default();
        if cs[i] == '`' {
            let (inner, consumed) = read_balanced_backtick(cs, i)?;
            t.command_substitution = true;
            // 命令替换的值**按定义**就是静态不可知的——它是另一条命令的输出。
            // 所以它同样是「目标未知」：`rm -rf $(cat targets.txt)` 删什么完全取决于
            // 那个文件的内容，与 `rm -rf $TARGET` 是同一类不确定性。
            t.unresolved = true;
            return Ok((String::new(), consumed, Some(inner), t));
        }
        match cs.get(i + 1) {
            // `$((…))` 算术展开：不是命令替换，值是数字
            Some('(') if matches!(cs.get(i + 2), Some('(')) => {
                let (_, consumed) = read_balanced(cs, i + 1, '(', ')')?;
                t.arith_expansion = true;
                Ok((String::new(), 1 + consumed, None, t))
            }
            // `$(…)` 命令替换。值静态不可知，故与查不到的变量同样标 unresolved。
            Some('(') => {
                let (inner, consumed) = read_balanced(cs, i + 1, '(', ')')?;
                t.command_substitution = true;
                t.unresolved = true;
                Ok((String::new(), 1 + consumed, Some(inner), t))
            }
            // `${…}` 参数展开。可能内嵌命令替换（`${x:-$(cmd)}`），须递归收集。
            Some('{') => {
                let (inner, consumed) = read_balanced(cs, i + 1, '{', '}')?;
                t.param_expansion = true;
                let name: String = inner
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                // 定不了值时**保留字面量**而不是变成空串。空串会让参数整个消失——
                // `rm -rf $HOME` 的目标就没了，路径规则再也看不到它，于是一条删家目录的
                // 命令只剩「有个定不了值的展开」这么一条 Write 级理由。
                // 保留字面量则下游的路径判定还认得 `$HOME`/`%USERPROFILE%` 这类熟脸。
                let resolved = self.lookup(&name);
                if resolved.is_none() {
                    t.unresolved = true;
                }
                let value = resolved.unwrap_or_else(|| dollar_literal(&name));
                // `${x:-$(cmd)}` 里的 cmd 会执行，交回上层递归
                let nested = collect_substitutions(&inner)?;
                let sub = nested.into_iter().next();
                if sub.is_some() {
                    t.command_substitution = true;
                }
                Ok((value, 1 + consumed, sub, t))
            }
            // `$NAME`
            Some(c) if c.is_alphanumeric() || *c == '_' => {
                let name: String = cs[i + 1..]
                    .iter()
                    .take_while(|c| c.is_alphanumeric() || **c == '_')
                    .collect();
                t.param_expansion = true;
                // 定不了值时**保留字面量**而不是变成空串。空串会让参数整个消失——
                // `rm -rf $HOME` 的目标就没了，路径规则再也看不到它，于是一条删家目录的
                // 命令只剩「有个定不了值的展开」这么一条 Write 级理由。
                // 保留字面量则下游的路径判定还认得 `$HOME`/`%USERPROFILE%` 这类熟脸。
                let resolved = self.lookup(&name);
                if resolved.is_none() {
                    t.unresolved = true;
                }
                let value = resolved.unwrap_or_else(|| dollar_literal(&name));
                Ok((value, 1 + name.chars().count(), None, t))
            }
            // `$?` `$$` `$!` `$#` `$0`… 特殊参数，值不是命令名
            Some(_) => {
                t.param_expansion = true;
                Ok((String::new(), 2, None, t))
            }
            // 末尾裸 `$`：字面量
            None => Ok(("$".into(), 1, None, t)),
        }
    }

    /// 查一个变量的静态值。查不到即标记「有展开定不了值」（失败闭合的输入信号）。
    fn lookup(&mut self, name: &str) -> Option<String> {
        match self.vars.get(name) {
            Some(v) => Some(v.clone()),
            None => {
                self.script.traits.unresolved_expansion = true;
                None
            }
        }
    }
}

impl WordTraits {
    fn merge(mut self, o: Self) -> Self {
        self.quoted_single |= o.quoted_single;
        self.quoted_double |= o.quoted_double;
        self.escaped |= o.escaped;
        self.param_expansion |= o.param_expansion;
        self.command_substitution |= o.command_substitution;
        self.arith_expansion |= o.arith_expansion;
        self.glob |= o.glob;
        self.tilde |= o.tilde;
        self.process_substitution |= o.process_substitution;
        self.unresolved |= o.unresolved;
        self
    }
}

enum Item {
    Word(Word),
    Redirect(RedirectKind),
    Heredoc(String),
}

/// 定不了值的变量在词里留下的字面量。
fn dollar_literal(name: &str) -> String {
    let mut s = String::with_capacity(name.len() + 1);
    s.push('$');
    s.push_str(name);
    s
}

/// 顶层列表的一段。
struct Segment {
    text: String,
    /// 这一段以 `&` 结尾（后台执行）。
    background: bool,
}

/// 拆分器专用的跳过器：[`skip_opaque`] 的一切，**再加上**成组的 `(…)` / `{ … }`
/// 与 heredoc 正文。
///
/// 为什么拆分器需要比 `skip_opaque` 更强的版本——三条都是第一版实测踩到的：
/// - `{ rm -rf /; }` 里的 `;` 不是顶层分隔符。不跳组就会拆成 `{ rm -rf /` 与 ` }`
///   两段，前一段的花括号再也配不上对。
/// - `:(){ :|:& };:`（fork 炸弹）里的 `&` 同理——它在函数体内。
/// - `cat <<EOF\nrm -rf /\nEOF` 里的换行不是顶层分隔符。不跳 heredoc 正文就会把正文
///   当命令，于是一段**纯文本**里的 `rm` 被判成要执行的命令：假阳性。而假阳性会把用户
///   训练成闭眼点确认，那比漏一条更难挽回。
///
/// `skip_opaque` 本身**不能**加这些：它被 [`read_balanced`] 复用，而那里正在自己数括号，
/// 让它顺手跳掉整个 `(…)` 会把括号计数搅乱。
fn skip_for_split(cs: &[char], i: usize) -> Result<Option<usize>, ParseError> {
    if let Some(n) = skip_opaque(cs, i)? {
        return Ok(Some(n));
    }
    match cs.get(i) {
        Some('(') => {
            let (_, consumed) = read_balanced(cs, i, '(', ')')?;
            Ok(Some(consumed))
        }
        // `{` 只有后跟空白时才是命令组；`{a,b}` 是花括号展开，属于词的一部分
        Some('{') if matches!(cs.get(i + 1), Some(c) if c.is_whitespace()) => {
            let (_, consumed) = read_balanced(cs, i, '{', '}')?;
            Ok(Some(consumed))
        }
        // heredoc：`<<TAG` / `<<-TAG`（`<<<` 是 here-string，不带正文）
        Some('<') if matches!(cs.get(i + 1), Some('<')) && cs.get(i + 2) != Some(&'<') => {
            match heredoc_span(cs, i) {
                Some(r) => r.map(Some),
                None => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

/// 一段 heredoc（从 `<<` 到结束标记行末）的总长度。
fn heredoc_span(cs: &[char], i: usize) -> Option<Result<usize, ParseError>> {
    let (_, consumed, tag) = read_redirect(cs, i)?;
    let tag = tag?;
    match read_heredoc_body(cs, i + consumed, &tag) {
        Ok((_, body_len)) => Some(Ok(consumed + body_len)),
        Err(e) => Some(Err(e)),
    }
}

/// 按 `;` `&&` `||` `&` 换行拆分顶层列表，引号与嵌套感知。
fn split_lists(s: &str) -> Result<Vec<Segment>, ParseError> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0usize;
    while i < cs.len() {
        // 跳过引号、组、heredoc 正文（内部的分隔符不算顶层分隔符）
        if let Some(consumed) = skip_for_split(&cs, i)? {
            cur.extend(cs[i..i + consumed].iter());
            i += consumed;
            continue;
        }
        let c = cs[i];
        match c {
            ';' | '\n' => {
                out.push(Segment {
                    text: std::mem::take(&mut cur),
                    background: false,
                });
                i += 1;
            }
            '&' if matches!(cs.get(i + 1), Some('&')) => {
                out.push(Segment {
                    text: std::mem::take(&mut cur),
                    background: false,
                });
                i += 2;
            }
            // `&>file` 是重定向不是后台，交给词法层
            '&' if matches!(cs.get(i + 1), Some('>')) => {
                cur.push(c);
                i += 1;
            }
            '&' => {
                out.push(Segment {
                    text: std::mem::take(&mut cur),
                    background: true,
                });
                i += 1;
            }
            '|' if matches!(cs.get(i + 1), Some('|')) => {
                out.push(Segment {
                    text: std::mem::take(&mut cur),
                    background: false,
                });
                i += 2;
            }
            _ => {
                cur.push(c);
                i += 1;
            }
        }
    }
    if !cur.trim().is_empty() {
        out.push(Segment {
            text: cur,
            background: false,
        });
    }
    Ok(out)
}

/// 按单个 `|` 拆管道（`||` 已在上一层处理掉）。
fn split_pipes(s: &str) -> Result<Vec<String>, ParseError> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut i = 0usize;
    while i < cs.len() {
        if let Some(consumed) = skip_for_split(&cs, i)? {
            cur.extend(cs[i..i + consumed].iter());
            i += consumed;
            continue;
        }
        // `|&` = `2>&1 |`（bash），仍是管道
        if cs[i] == '|' {
            out.push(std::mem::take(&mut cur));
            i += if matches!(cs.get(i + 1), Some('&')) {
                2
            } else {
                1
            };
            continue;
        }
        cur.push(cs[i]);
        i += 1;
    }
    out.push(cur);
    Ok(out)
}

/// 若位置 `i` 是一段「不透明区」（引号、`$(…)`、`` `…` ``、`${…}`、`(…)`）的开头，
/// 返回它的总长度。用于让上层的分隔符扫描跳过整段。
///
/// 这个函数是整个拆分逻辑的正确性关键：少跳一种构造，`echo "a;b"` 里的分号
/// 就会被当成命令分隔符，从而把 `b` 当成一条命令——那既是假阳性，
/// 也说明分解器没有真的理解输入。
fn skip_opaque(cs: &[char], i: usize) -> Result<Option<usize>, ParseError> {
    match cs.get(i) {
        Some('\\') => match cs.get(i + 1) {
            Some(_) => Ok(Some(2)),
            None => Err(ParseError::TrailingBackslash),
        },
        Some('\'') => {
            let mut j = i + 1;
            while let Some(&c) = cs.get(j) {
                j += 1;
                if c == '\'' {
                    return Ok(Some(j - i));
                }
            }
            Err(ParseError::UnterminatedSingleQuote)
        }
        Some('"') => {
            let mut j = i + 1;
            while let Some(&c) = cs.get(j) {
                if c == '\\' {
                    if cs.get(j + 1).is_none() {
                        return Err(ParseError::UnterminatedDoubleQuote);
                    }
                    j += 2;
                    continue;
                }
                j += 1;
                if c == '"' {
                    return Ok(Some(j - i));
                }
            }
            Err(ParseError::UnterminatedDoubleQuote)
        }
        Some('`') => {
            let (_, consumed) = read_balanced_backtick(cs, i)?;
            Ok(Some(consumed))
        }
        Some('$') => match cs.get(i + 1) {
            Some('(') => {
                let (_, consumed) = read_balanced(cs, i + 1, '(', ')')?;
                Ok(Some(1 + consumed))
            }
            Some('{') => {
                let (_, consumed) = read_balanced(cs, i + 1, '{', '}')?;
                Ok(Some(1 + consumed))
            }
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

/// 从 `cs[at]`（必须是 `open`）读到配对的 `close`，返回（内部文本，含两端的总长度）。
fn read_balanced(
    cs: &[char],
    at: usize,
    open: char,
    close: char,
) -> Result<(String, usize), ParseError> {
    debug_assert_eq!(cs.get(at), Some(&open));
    let mut depth = 0usize;
    let mut inner = String::new();
    let mut j = at;
    while let Some(&c) = cs.get(j) {
        // 嵌套里的引号要整体跳过，否则 `$(echo ")")` 会在那个右括号上提前收尾
        if depth > 0 {
            if let Some(consumed) = skip_opaque(cs, j)? {
                // 引号内容属于 inner
                inner.extend(cs[j..j + consumed].iter());
                j += consumed;
                continue;
            }
        }
        if c == open {
            depth += 1;
            if depth > MAX_DEPTH {
                return Err(ParseError::TooDeep);
            }
            if depth > 1 {
                inner.push(c);
            }
            j += 1;
            continue;
        }
        if c == close {
            depth -= 1;
            if depth == 0 {
                return Ok((inner, j + 1 - at));
            }
            inner.push(c);
            j += 1;
            continue;
        }
        inner.push(c);
        j += 1;
    }
    Err(match (open, close) {
        ('(', ')') => ParseError::UnterminatedSubstitution,
        ('{', '}') => ParseError::UnterminatedParamExpansion,
        _ => ParseError::UnbalancedParen,
    })
}

/// 反引号命令替换。反引号不能嵌套（要嵌套得用 `\``），所以扫到下一个未转义的反引号即止。
fn read_balanced_backtick(cs: &[char], at: usize) -> Result<(String, usize), ParseError> {
    debug_assert_eq!(cs.get(at), Some(&'`'));
    let mut inner = String::new();
    let mut j = at + 1;
    while let Some(&c) = cs.get(j) {
        if c == '\\' {
            if let Some(&n) = cs.get(j + 1) {
                inner.push(n);
                j += 2;
                continue;
            }
            return Err(ParseError::TrailingBackslash);
        }
        if c == '`' {
            return Ok((inner, j + 1 - at));
        }
        inner.push(c);
        j += 1;
    }
    Err(ParseError::UnterminatedBacktick)
}

/// 从一段文本里收集所有命令替换的正文（用于 heredoc 正文与 `${…}` 内部）。
fn collect_substitutions(s: &str) -> Result<Vec<String>, ParseError> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < cs.len() {
        match cs[i] {
            '\'' => {
                // 单引号内不展开
                let consumed = skip_opaque(&cs, i)?.unwrap_or(1);
                i += consumed;
            }
            '`' => {
                let (inner, consumed) = read_balanced_backtick(&cs, i)?;
                out.push(inner);
                i += consumed;
            }
            '$' if matches!(cs.get(i + 1), Some('(')) && !matches!(cs.get(i + 2), Some('(')) => {
                let (inner, consumed) = read_balanced(&cs, i + 1, '(', ')')?;
                out.push(inner);
                i += 1 + consumed;
            }
            _ => i += 1,
        }
    }
    Ok(out)
}

/// 词的终结符：这些字符出现时当前词结束。
fn is_word_terminator(cs: &[char], i: usize) -> bool {
    match cs[i] {
        ';' | '|' | '&' | '(' | ')' | '<' | '>' | '\n' => true,
        // `{`/`}` 只在独立成词时是操作符（`{a,b}` 是花括号展开，属于词的一部分）
        _ => false,
    }
}

/// 识别重定向操作符。返回（方向，消耗字符数，heredoc 结束标记）。
fn read_redirect(cs: &[char], i: usize) -> Option<(RedirectKind, usize, Option<String>)> {
    // `<(` / `>(` 是**进程替换**不是重定向，由 read_word 处理。
    // 不先挡掉这一条，`diff <(cmd) b` 会被当成「重定向到文件 `(cmd)`」，随后
    // scan_items 在裸 `(` 上零进展地空转——第一版正是这样把测试进程跑成 abort 的。
    if matches!(cs.get(i), Some('<') | Some('>')) && matches!(cs.get(i + 1), Some('(')) {
        return None;
    }
    // fd 前缀：`2>`、`1>&2`
    let mut j = i;
    while matches!(cs.get(j), Some(c) if c.is_ascii_digit()) {
        j += 1;
    }
    // `&>` / `&>>`
    if cs.get(j) == Some(&'&') && matches!(cs.get(j + 1), Some('>')) {
        let extra = usize::from(cs.get(j + 2) == Some(&'>'));
        return Some((RedirectKind::Output, j + 2 + extra - i, None));
    }
    match (cs.get(j), cs.get(j + 1), cs.get(j + 2)) {
        // heredoc：`<<-TAG` / `<<TAG`；`<<<` 是 here-string（数据，按 Input）
        (Some('<'), Some('<'), Some('<')) => Some((RedirectKind::Input, j + 3 - i, None)),
        (Some('<'), Some('<'), _) => {
            let dash = usize::from(cs.get(j + 2) == Some(&'-'));
            let mut k = j + 2 + dash;
            while matches!(cs.get(k), Some(c) if c.is_whitespace() && *c != '\n') {
                k += 1;
            }
            let tag: String = cs[k..]
                .iter()
                .take_while(|c| !c.is_whitespace())
                .filter(|c| !matches!(c, '\'' | '"' | '\\'))
                .collect();
            if tag.is_empty() {
                return None;
            }
            let tag_len = cs[k..].iter().take_while(|c| !c.is_whitespace()).count();
            Some((RedirectKind::Input, k + tag_len - i, Some(tag)))
        }
        (Some('>'), Some('>'), _) => Some((RedirectKind::Append, j + 2 - i, None)),
        (Some('>'), Some('&'), _) => Some((RedirectKind::Duplicate, j + 2 - i, None)),
        (Some('>'), Some('|'), _) => Some((RedirectKind::Output, j + 2 - i, None)),
        (Some('<'), Some('&'), _) => Some((RedirectKind::Duplicate, j + 2 - i, None)),
        (Some('>'), _, _) => Some((RedirectKind::Output, j + 1 - i, None)),
        (Some('<'), _, _) => Some((RedirectKind::Input, j + 1 - i, None)),
        _ => None,
    }
}

/// 读 heredoc 正文，直到出现单独一行的结束标记。
///
/// 返回 `(正文, 从 from 起消耗的字符数)`。注意消耗**包含**了 `<<TAG` 与正文之间那段
/// 「本行剩余内容」——`cat <<EOF > /etc/motd` 里的 `> /etc/motd` 就在那一段里。
/// 调用方必须把那一段拿回去继续扫描（见 [`rest_of_line`]），否则那个重定向会凭空消失：
/// 一条往 `/etc/motd` 写东西的命令会变成「只是有个 heredoc」。
fn read_heredoc_body(cs: &[char], from: usize, tag: &str) -> Result<(String, usize), ParseError> {
    // 跳到行尾
    let mut i = from;
    while matches!(cs.get(i), Some(c) if *c != '\n') {
        i += 1;
    }
    if cs.get(i).is_none() {
        // 没有换行 = 正文压根没开始。这是不完整的输入，失败闭合。
        return Err(ParseError::UnterminatedHeredoc);
    }
    i += 1; // 吃掉换行
    let body_start = i;
    let mut line_start = i;
    while i <= cs.len() {
        let at_end = i == cs.len();
        if at_end || cs[i] == '\n' {
            let line: String = cs[line_start..i].iter().collect();
            if line.trim() == tag {
                let body: String = cs[body_start..line_start].iter().collect();
                return Ok((body, i.min(cs.len()) - from));
            }
            if at_end {
                break;
            }
            line_start = i + 1;
        }
        i += 1;
    }
    Err(ParseError::UnterminatedHeredoc)
}

/// 从 `from` 到本行行尾的文本（不含换行）。heredoc 用它把「本行剩余部分」取回来。
fn rest_of_line(cs: &[char], from: usize) -> String {
    cs[from..].iter().take_while(|c| **c != '\n').collect()
}

/// 若整段是 `( … )` 或 `{ … ; }`，返回内部文本。
fn strip_group(s: &str) -> Result<Option<&str>, ParseError> {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix('(') {
        let Some(inner) = rest.strip_suffix(')') else {
            return Err(ParseError::UnbalancedParen);
        };
        return Ok(Some(inner));
    }
    if let Some(rest) = t.strip_prefix('{') {
        let Some(inner) = rest.strip_suffix('}') else {
            return Err(ParseError::UnbalancedBrace);
        };
        // `{a,b}` 是花括号展开而非命令组：命令组要求 `{` 后有空白
        if !rest.starts_with(char::is_whitespace) {
            return Ok(None);
        }
        return Ok(Some(inner));
    }
    Ok(None)
}

/// 若整段是函数定义 `name() { … }`，返回（函数名，函数体）。
///
/// 需要它是为了 fork 炸弹：`:(){ :|:& };:` 的危险不在定义而在最后那个 `:` 调用，
/// 但不解析定义体就看不出这个函数在递归调用自己。
fn split_function_def(s: &str) -> Result<Option<(String, &str)>, ParseError> {
    let t = s.trim();
    let Some(paren) = t.find("()") else {
        return Ok(None);
    };
    let name = t[..paren].trim();
    if name.is_empty() || name.contains(char::is_whitespace) {
        return Ok(None);
    }
    let rest = t[paren + 2..].trim_start();
    let Some(body) = rest.strip_prefix('{') else {
        return Ok(None);
    };
    let Some(body) = body.strip_suffix('}') else {
        return Err(ParseError::UnbalancedBrace);
    };
    Ok(Some((name.to_string(), body)))
}

/// 若一个词形如 `NAME=value`，拆成（名，值）。
///
/// `NAME` 必须是合法标识符——`--opt=v`、`a=b=c` 的左半、`http://x` 都不是赋值。
fn split_assignment(w: &Word) -> Option<(String, Word)> {
    // 用 raw 判定「等号在原文里的位置」：去引号后的 text 可能把等号挪了位
    let eq = w.text.find('=')?;
    let (name, value) = (&w.text[..eq], &w.text[eq + 1..]);
    if name.is_empty() || !is_identifier(name) {
        return None;
    }
    Some((
        name.to_string(),
        Word {
            text: value.to_string(),
            raw: w.raw.clone(),
            traits: w.traits,
        },
    ))
}

fn is_identifier(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progs(input: &str) -> Vec<String> {
        parse(input)
            .expect("应能解析")
            .commands()
            .filter_map(|c| c.program_basename().map(str::to_string))
            .collect()
    }

    #[test]
    fn top_level_list_separators_all_split() {
        // 出口标准点名的「`&&` 链」与「复合命令」：首词是 ls 不代表整条只读
        assert_eq!(progs("ls; rm -rf ~"), vec!["ls", "rm"]);
        assert_eq!(progs("ls && rm -rf ~"), vec!["ls", "rm"]);
        assert_eq!(progs("ls || rm -rf ~"), vec!["ls", "rm"]);
        assert_eq!(progs("ls\nrm -rf ~"), vec!["ls", "rm"]);
        assert_eq!(progs("ls & rm -rf ~"), vec!["ls", "rm"]);
    }

    #[test]
    fn pipelines_keep_their_stage_order() {
        let s = parse("curl http://x | sh").unwrap();
        assert_eq!(s.pipelines.len(), 1, "一条管道不该被拆成两条");
        let names: Vec<_> = s.pipelines[0]
            .stages
            .iter()
            .map(|c| c.program_basename().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["curl", "sh"], "阶段顺序是危险判定的输入");
    }

    #[test]
    fn command_substitution_is_decomposed_not_ignored() {
        // 出口标准点名的「命令替换」。
        // 顺序是**执行**顺序：shell 必须先求出替换的值才能调 ls，所以 rm 在前。
        assert_eq!(progs("ls $(rm -rf /tmp/x)"), vec!["rm", "ls"]);
        assert_eq!(progs("ls `rm -rf /tmp/x`"), vec!["rm", "ls"]);
        let s = parse("ls $(rm -rf /tmp/x)").unwrap();
        assert!(s.traits.command_substitution);
        // 替换里的命令层级更深——规则层据此区分「顺手跑的」
        let rm = s
            .commands()
            .find(|c| c.program_basename() == Some("rm"))
            .unwrap();
        assert!(rm.depth > 0, "替换内的命令必须标出更深的层级");
    }

    #[test]
    fn separators_inside_quotes_are_not_separators() {
        // 少跳一种不透明区就会在这里假阳性
        assert_eq!(progs("echo 'a; rm -rf /'"), vec!["echo"]);
        assert_eq!(progs("echo \"a && rm -rf /\""), vec!["echo"]);
        assert_eq!(progs("echo 'a | sh'"), vec!["echo"]);
        // 替换内部的右括号不能提前收尾
        assert_eq!(progs("echo $(echo \")\")"), vec!["echo", "echo"]);
    }

    #[test]
    fn quote_stripping_reveals_the_real_program_name() {
        // `r''m` 与 `"rm"` 在 shell 里都是 rm；只认字面 `rm` 的引擎会被绕过
        assert_eq!(progs("r''m -rf /"), vec!["rm"]);
        assert_eq!(progs("\"rm\" -rf /"), vec!["rm"]);
        assert_eq!(progs("r\\m -rf /"), vec!["rm"]);
        // 但「原本有引号」这件事必须留痕，否则白名单会把它当普通词
        let s = parse("r''m -rf /").unwrap();
        let c = s.commands().next().unwrap();
        assert!(c.argv[0].traits.quoted_single);
        assert!(!c.argv[0].traits.is_plain(), "带引号的词不是普通字面量");
    }

    #[test]
    fn variable_assembly_is_resolved_when_statically_knowable() {
        // 出口标准点名的「元字符拼装」`a=rm; $a -rf /`
        assert_eq!(progs("a=rm; $a -rf /"), vec!["rm"]);
        assert_eq!(progs("a=rm; ${a} -rf /"), vec!["rm"]);
        // 前缀赋值同样进符号表
        assert_eq!(progs("a=rm true; $a -rf /"), vec!["true", "rm"]);
    }

    #[test]
    fn unresolvable_expansion_is_flagged_not_guessed() {
        // 定不了值就必须留下失败闭合的信号，而不是当成空串让命令看起来无害
        let s = parse("$CMD -rf /").unwrap();
        assert!(s.traits.unresolved_expansion);
        // 来自命令替换的赋值也定不了值
        let s = parse("a=$(curl http://x); $a").unwrap();
        assert!(s.traits.unresolved_expansion);
    }

    #[test]
    fn absolute_and_relative_program_paths_normalize_to_basename() {
        assert_eq!(progs("/bin/rm -rf /"), vec!["rm"]);
        assert_eq!(progs("./rm -rf /"), vec!["rm"]);
        assert_eq!(progs("/usr/bin/env rm -rf /"), vec!["env"]);
    }

    #[test]
    fn redirect_targets_are_captured_with_direction() {
        let s = parse("echo x > /etc/passwd").unwrap();
        let c = s.commands().next().unwrap();
        assert_eq!(c.redirects.len(), 1);
        assert_eq!(c.redirects[0].kind, RedirectKind::Output);
        assert_eq!(c.redirects[0].target.text, "/etc/passwd");
        assert!(c.redirects[0].kind.writes());

        // 读入方向不算写
        let s = parse("wc -l < /etc/passwd").unwrap();
        assert!(!s.commands().next().unwrap().redirects[0].kind.writes());

        // 追加也是写
        let s = parse("echo k >> ~/.ssh/authorized_keys").unwrap();
        let r = &s.commands().next().unwrap().redirects[0];
        assert_eq!(r.kind, RedirectKind::Append);
        assert!(r.kind.writes());

        // fd 前缀不该被当成程序名的一部分
        let s = parse("cmd 2> /tmp/err").unwrap();
        assert_eq!(s.commands().next().unwrap().program_basename(), Some("cmd"));
    }

    #[test]
    fn subshell_and_group_contents_are_decomposed() {
        assert_eq!(progs("(rm -rf /)"), vec!["rm"]);
        assert_eq!(progs("{ rm -rf /; }"), vec!["rm"]);
        let s = parse("(rm -rf /)").unwrap();
        assert!(s.traits.subshell);
        // `{a,b}` 是花括号展开，不是命令组
        assert_eq!(progs("echo {a,b}"), vec!["echo"]);
    }

    #[test]
    fn function_body_is_decomposed_for_fork_bomb_detection() {
        let s = parse(":(){ :|:& };:").unwrap();
        assert!(s.traits.function_definition);
        // 函数体里的自我调用必须被看见
        let names: Vec<_> = s.commands().filter_map(|c| c.program_basename()).collect();
        assert!(
            names.contains(&":"),
            "函数体内的递归调用必须被拆出来：{names:?}"
        );
    }

    #[test]
    fn heredoc_body_is_data_not_commands() {
        // heredoc 正文里的 rm 是文本。当成命令会造成大量假阳性，
        // 而假阳性会把用户训练成闭眼点确认——那比漏一条更难挽回。
        let s = parse("cat <<EOF\nrm -rf /\nEOF").unwrap();
        assert!(s.traits.heredoc);
        let names: Vec<_> = s.commands().filter_map(|c| c.program_basename()).collect();
        assert_eq!(names, vec!["cat"], "heredoc 正文不该产生命令");
    }

    #[test]
    fn heredoc_body_substitutions_do_execute() {
        // 但正文里的命令替换是会执行的，必须拆出来
        let s = parse("cat <<EOF\n$(rm -rf /tmp/x)\nEOF").unwrap();
        let names: Vec<_> = s.commands().filter_map(|c| c.program_basename()).collect();
        assert!(
            names.contains(&"rm"),
            "heredoc 里的命令替换会执行：{names:?}"
        );
    }

    #[test]
    fn process_substitution_contents_are_decomposed() {
        let s = parse("diff <(rm -rf /tmp/a) b").unwrap();
        assert!(s.traits.process_substitution);
        let names: Vec<_> = s.commands().filter_map(|c| c.program_basename()).collect();
        assert!(names.contains(&"rm"), "{names:?}");
    }

    #[test]
    fn nested_param_expansion_default_can_hide_a_command() {
        // `${x:-$(cmd)}` 里的 cmd 在 x 未设时会执行
        let s = parse("echo ${x:-$(rm -rf /tmp/y)}").unwrap();
        let names: Vec<_> = s.commands().filter_map(|c| c.program_basename()).collect();
        assert!(names.contains(&"rm"), "{names:?}");
    }

    #[test]
    fn arithmetic_expansion_is_not_command_substitution() {
        let s = parse("echo $((1+2))").unwrap();
        assert!(!s.traits.command_substitution, "算术展开不执行命令");
        let c = s.commands().next().unwrap();
        assert!(c.argv[1].traits.arith_expansion);
    }

    #[test]
    fn unterminated_constructs_fail_closed() {
        // 每一种都必须报错而不是「尽力猜」——上层会把错误翻成 dangerous
        assert_eq!(parse("echo 'x"), Err(ParseError::UnterminatedSingleQuote));
        assert_eq!(parse("echo \"x"), Err(ParseError::UnterminatedDoubleQuote));
        assert_eq!(parse("echo `x"), Err(ParseError::UnterminatedBacktick));
        assert_eq!(parse("echo $(x"), Err(ParseError::UnterminatedSubstitution));
        assert_eq!(
            parse("echo ${x"),
            Err(ParseError::UnterminatedParamExpansion)
        );
        assert_eq!(parse("echo x\\"), Err(ParseError::TrailingBackslash));
        assert_eq!(
            parse("cat <<EOF\nrm\n"),
            Err(ParseError::UnterminatedHeredoc)
        );
    }

    #[test]
    fn resource_limits_fail_closed_rather_than_blow_the_stack() {
        // 长度闸
        let long = "a".repeat(MAX_INPUT_LEN + 1);
        assert_eq!(parse(&long), Err(ParseError::TooLong));
        // 深度闸：stack overflow 在 Rust 里是 abort，捕获不了，所以必须先挡住
        let deep = format!("{}echo{}", "$(".repeat(40), ")".repeat(40));
        assert_eq!(parse(&deep), Err(ParseError::TooDeep));
        // 命令数闸：到顶报错而不是静默截断（截断会让后面的命令完全不受审查）
        let many = "true;".repeat(MAX_COMMANDS + 10);
        assert_eq!(parse(&many), Err(ParseError::TooManyCommands));
    }

    #[test]
    fn assignment_prefix_only_counts_before_argv() {
        // `make CC=gcc` 里的 CC=gcc 是参数不是赋值
        let s = parse("make CC=gcc").unwrap();
        let c = s.commands().next().unwrap();
        assert!(c.assignments.is_empty(), "argv 之后的 x=y 是普通参数");
        assert_eq!(c.argv.len(), 2);
        // 而 `CC=gcc make` 里的是赋值前缀
        let s = parse("CC=gcc make").unwrap();
        let c = s.commands().next().unwrap();
        assert_eq!(c.assignments.len(), 1);
        assert_eq!(c.program_basename(), Some("make"));
    }

    #[test]
    fn non_identifier_equals_is_not_an_assignment() {
        for input in ["curl --url=http://x", "echo a=b=c", "git config x.y=z"] {
            let s = parse(input).unwrap();
            let c = s.commands().next().unwrap();
            assert!(
                c.program_basename().is_some(),
                "{input} 的程序名不该被吃掉：{c:?}"
            );
        }
    }

    #[test]
    fn basename_handles_both_separators_and_trailing_slash() {
        assert_eq!(basename("/usr/bin/rm"), "rm");
        assert_eq!(basename("rm"), "rm");
        assert_eq!(basename("/usr/bin/"), "bin");
        assert_eq!(basename("C:\\Windows\\System32\\cmd.exe"), "cmd.exe");
        assert_eq!(basename(""), "");
    }

    #[test]
    fn glob_and_tilde_stay_plain_so_the_whitelist_remains_usable() {
        // 刻意的取舍：`ls *.txt` / `ls ~/logs` 必须还能走白名单，
        // 否则用户对每次 ls 都要点确认，几天后就会去关总开关。
        let s = parse("ls *.txt").unwrap();
        let c = s.commands().next().unwrap();
        assert!(c.argv[1].traits.glob);
        assert!(c.all_words_plain(), "通配不该把词判为非普通");
        let s = parse("ls ~/logs").unwrap();
        assert!(s.commands().next().unwrap().all_words_plain());
    }

    #[test]
    fn empty_and_whitespace_only_input_parses_to_nothing() {
        for input in ["", "   ", "\n\n", "  \t \n "] {
            let s = parse(input).unwrap();
            assert_eq!(s.commands().count(), 0, "{input:?} 不该产生命令");
        }
    }

    #[test]
    fn comment_ends_the_command() {
        assert_eq!(progs("ls # rm -rf /"), vec!["ls"]);
        // 但井号在词中间是普通字符
        let s = parse("echo a#b").unwrap();
        assert_eq!(s.commands().next().unwrap().argv[1].text, "a#b");
    }
}
