#!/usr/bin/env bash
# CI 对齐断言（M4a.1 T89）。
#
# `scripts/ci-local.sh` 的价值全在「它跑的确实是 CI 会跑的那些门禁」。若有人往 ci.yml
# 加了一步而本地脚本没跟上（或反过来），本地全绿就不再意味着 CI 会绿——门禁悄悄变空。
#
# 本脚本双向断言：ci.yml 里的每条门禁命令都要在 ci-local.sh 里出现，反之亦然。
#
# ## 做空防护
#
# 只断言「双向包含」是不够的：把两边的门禁**同时**删光，双向包含依然成立而检查恒绿。
# 故另加一条**条数下限**——CI 的门禁不得少于 MIN_GATES 条。这一条是从 M4a 的教训里
# 抄来的形状（perf/acceptance 门禁都栽在「解析器匹配零条即恒绿」上）。
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

CI=".github/workflows/ci.yml"
LOCAL="scripts/ci-local.sh"

# CI 与本地都必须有的门禁，用「特征串」表示（避免逐字比对被参数差异干扰：
# 本地刻意不带 --locked，见 ci-local.sh 的说明）。
GATES=(
  "cargo fmt --check"
  "cargo clippy"
  "cargo deny check licenses bans advisories"
  "cargo nextest run"
  "cargo test"
  "cargo fmt --manifest-path rdp-helper/Cargo.toml --check"
  "cargo clippy --manifest-path rdp-helper/Cargo.toml"
  "cargo deny --manifest-path rdp-helper/Cargo.toml"
  "cargo test --manifest-path rdp-helper/Cargo.toml"
  "node .github/scripts/check-version-consistency.mjs"
  "node scripts/check-doc-links.mjs"
  "node scripts/check-shell-portability.mjs"
)
# 做空防护：门禁条数不得低于此。加门禁时同步上调，删门禁时会在这里绊一下。
MIN_GATES=12

fail() { echo "CI 对齐失败：$*" >&2; exit 1; }

[[ -f "$CI" ]] || fail "找不到 $CI"
[[ -f "$LOCAL" ]] || fail "找不到 $LOCAL"

[[ ${#GATES[@]} -ge $MIN_GATES ]] ||
  fail "门禁清单只剩 ${#GATES[@]} 条（下限 ${MIN_GATES}）——这个检查被做空了"

missing_ci=()
missing_local=()
# **必须先剥注释再匹配**：本脚本第一版直接 grep 原文，于是把 ci-local.sh 里那句
# 「漏了 `cargo fmt --check` 与 …」的说明文字也算成「门禁在」——删掉真正的 run_step
# 那一行，检查照旧全绿（变异验证 C1 当场抓到）。散文不是门禁。
#
# CI 侧同理：ci.yml 的注释里也提到过这些命令。
ci_code="$(grep -vE '^\s*#' "$CI")"
local_code="$(grep -vE '^\s*#' "$LOCAL")"

for g in "${GATES[@]}"; do
  grep -qF -- "$g" <<<"$ci_code" || missing_ci+=("$g")
  grep -qF -- "$g" <<<"$local_code" || missing_local+=("$g")
done

if [[ ${#missing_ci[@]} -gt 0 ]]; then
  fail "这些门禁在 $CI 里找不到（清单写错了，或 CI 真的删了门禁）：${missing_ci[*]}"
fi
if [[ ${#missing_local[@]} -gt 0 ]]; then
  fail "这些门禁 CI 会跑但 $LOCAL 不跑（本地全绿不再意味着 CI 会绿）：${missing_local[*]}"
fi

echo "CI 对齐：${#GATES[@]} 条门禁在 $CI 与 $LOCAL 两侧都在（下限 ${MIN_GATES}）"
