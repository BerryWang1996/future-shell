#!/usr/bin/env bash
# 文档里写的环境闸必须真实存在（M0 出口「README/CONTRIBUTING 齐备」的防漂门禁）。
#
# ## 为什么需要它
#
# CONTRIBUTING.md 与路线图 §CI闸 都写着「性能测试 `FS_PERF` 手动/夜间触发，不进 PR 闸」。
# 那是 Phase 0+1 计划里的设计，**从未落地**：`perf.rs` 最终选的是 `#[ignore]`（理由写在
# 该文件头部——那些测试要 GUI 与预构建产物，env 闸挡不住这些前置），全仓 `FS_PERF` 零命中，
# 只有一个名字相近但语义完全不同的 `FS_PERF_TEST`（应用侧的就绪标记桩）。
#
# 后果不是「文档过时」这么轻：新人照着 CONTRIBUTING 跑 `FS_PERF=1 cargo test`，
# 一条性能测试都不会跑，而**输出是绿的**——他会得出「性能测试通过」的结论。
# 这与 perf.rs 头部那句「跑绿了不代表性能达标」是同一类危险，只是方向相反。
#
# 本脚本断言：文档里以「环境闸」口吻提到的每个 `FS_*` 变量，都能在代码或 workflow 里
# 找到真实的读取点。找不到 = 文档在描述一个不存在的开关。
#
# ## 做空防护（三条互相独立）
#
# ① 变量数下限：扫出的变量数 < MIN_VARS 即红（防正则被改坏后一个都扫不到而恒绿）；
# ② 文档集合非空且每个文件都存在（防文件改名后静默跳过）；
# ③ 反向自检：一个刻意伪造的变量名必须被判为「不存在」——证明查找逻辑真的会说不。
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# 面向使用者的文档。计划/路线图不算——那里写的是**打算做什么**，允许与现状不一致；
# 而 README/CONTRIBUTING 是「照着做」的说明书，写错就是把人带沟里。
DOCS=(README.md CONTRIBUTING.md)
# 当前应有 2 个：FS_ITEST、FS_PERF_TEST。加环境闸时同步上调；这条防的是正则失效。
MIN_VARS=2

fail() { echo "环境闸对齐失败：$*" >&2; exit 1; }

for d in "${DOCS[@]}"; do
  [[ -f "$d" ]] || fail "找不到文档 $d（改名了？门禁会因此静默跳过，故直接判红）"
done

# 代码侧的真实读取点：Rust 源码、workflow、脚本。
#
# **必须排除本脚本自身**：下面做空防护③用的那个伪造变量名就写在这个文件里，
# 不排除的话它会把自己扫出来，反向自检恒红（第一版正是如此）。同一个自指陷阱在
# app/src/commands/vault_cmd.rs 的守卫门禁里也踩过——那里的解法是「源码只取
# #[cfg(test)] 之前的生产段」。
# **必须按词边界匹配，不能用 -F 子串**：本仓恰好有一对 `FS_PERF` 与 `FS_PERF_TEST`，
# 子串匹配会让不存在的 `FS_PERF` 借着 `FS_PERF_TEST` 报「ok」——那正是这个门禁要抓的
# 那一条，第一版就这样把自己骗过去了。`\b` 在 `_` 前不成立（`_` 是词字符），
# 所以 `\bFS_PERF\b` 不会命中 `FS_PERF_TEST`，正是想要的语义。
code_has() {
  local var="$1"
  grep -rqE "\\b${var}\\b" \
    --include='*.rs' --include='*.yml' --include='*.yaml' --include='*.sh' --include='*.ps1' \
    --exclude="$(basename "${BASH_SOURCE[0]}")" \
    app crates frontend .github scripts 2>/dev/null
}

mapfile -t VARS < <(grep -rhoE '\bFS_[A-Z0-9_]+\b' "${DOCS[@]}" | sort -u)

if (( ${#VARS[@]} < MIN_VARS )); then
  fail "只从文档里扫出 ${#VARS[@]} 个 FS_* 变量（下限 ${MIN_VARS}）：正则已失效，此门禁不再检查任何东西"
fi

# 做空防护③：查找逻辑必须真的会说「不存在」
if code_has "FS_THIS_MUST_NEVER_EXIST_XYZ"; then
  fail "反向自检失败：伪造的变量名居然被判为存在，查找逻辑恒真"
fi

# 明知**不存在**、文档里作为反面教材提到的变量。写进来必须附理由。
#
# 需要这张表是因为「把假开关记下来警示后人」和「文档里写着一个假开关」在纯文本扫描下
# 长得一模一样——CONTRIBUTING 现在正是在讲 FS_PERF 不存在这件事，不豁免就会把自己判红。
declare -A KNOWN_ABSENT=(
  [FS_PERF]="Phase 0+1 计划设计过的性能闸，最终未落地（perf.rs 改用 #[ignore]）；CONTRIBUTING 与本脚本头部作为反面教材记录"
)

bad=0
for v in "${VARS[@]}"; do
  if code_has "$v"; then
    if [[ -n "${KNOWN_ABSENT[$v]:-}" ]]; then
      # 豁免表过期了：说好不存在的东西现在存在了。这不是好事也不是坏事，
      # 但豁免必须撤掉，否则它会永久遮住这个变量的真实状态。
      echo "  STALE $v —— 已在豁免表里标为「不存在」，但代码里现在找得到了；请删掉豁免条目" >&2
      bad=1
    else
      echo "  ok    $v"
    fi
  elif [[ -n "${KNOWN_ABSENT[$v]:-}" ]]; then
    echo "  known-absent $v —— ${KNOWN_ABSENT[$v]}"
  else
    echo "  MISS  $v —— 文档里写着这个环境闸，但代码/workflow 里没有任何读取点" >&2
    bad=1
  fi
done

if (( bad )); then
  fail "上述变量只存在于文档里。要么实现它，要么把文档改成现状——\
不要留一个「设了也没用」的开关，那会让人以为跑过了。"
fi

echo "环境闸对齐：${#VARS[@]} 个变量全部有真实读取点"
