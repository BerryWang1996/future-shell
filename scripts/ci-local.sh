#!/usr/bin/env bash
# 本地 CI 镜像门禁（M4a.1 T89）。
#
# ## 为什么需要它
#
# `.github/workflows/ci.yml` 从未对这份代码跑过（origin 停在 Initial commit）。而我自己
# 的关卡序列一直是 clippy + test + vitest + svelte-check——**漏了 `cargo fmt --check` 与
# `cargo deny`**。规划期把 CI 的门禁集逐条跑了一遍，当场撞出两个会让 CI 第一次运行就
# 失败的问题：229 处 fmt 违规、RUSTSEC-2026-0258（h2 < 0.4.16）。
#
# 「照着 CI 的清单跑一遍」这件事必须有载体，否则下次照样漏。
#
# ## 与 ci.yml 的对应关系
#
# 步骤集合与顺序**逐条对齐** ci.yml 的 check job（见该文件 59–80 行）。
# `scripts/ci-parity.sh` 断言两边不漂——从本脚本里删掉一步，那个断言就红。
#
# 刻意**不**覆盖的两项，以及为什么：
#   · Linux/macOS 平台一路 —— 无环境。Linux 的部分由 scripts/linux-precheck.sh 覆盖
#     （容器内 check + clippy + 单元测试）；macOS 一路本仓无法验证，不假装。
#   · `--locked` —— CI 用它钉死 Cargo.lock；本地开发要允许 lock 更新（T90 的
#     `cargo update -p h2` 就是一次），故本脚本不带。发版前的 --locked 校验归 CI。
#
# 用法：bash scripts/ci-local.sh [--skip-slow]
#   --skip-slow：跳过 nextest/cargo test 两步（只做静态门禁，秒级返回）
#   --with-itest：额外带 FS_ITEST=1 跑容器集成测试（需 Docker，约 3–5 分钟）
#
# ## 为什么 --with-itest 必须存在（2026-08-22 教训）
#
# 容器 itest 走 FS_ITEST 门控（没 Docker 的机器要能跑测试，门控本身是对的），代价是
# 它们在日常关卡里**零执行**。全量跑一次后当场撞出两条：strict TOFU 的断言长期红
#（判据找英文 "strict"、实现给的是中文文案），以及 asciinema 安装缺 apt-get update 导致的
# 间歇红。ci.yml 的 Linux runner 会带上这个变量，但 CI 从未跑过——于是没有任何东西在看它们。
# 打 tag 前、以及改动 crates/itest 或任何 sshengine/terminal 行为之后，应当带上这个开关。
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

SKIP_SLOW=0
WITH_ITEST=0
for arg in "$@"; do
  case "$arg" in
    --skip-slow) SKIP_SLOW=1 ;;
    --with-itest) WITH_ITEST=1 ;;
    *) echo "未知参数：$arg（可用：--skip-slow / --with-itest）" >&2; exit 2 ;;
  esac
done

FAILED=()
PASSED=()

# 跑一步：回显命令、执行、记账。**不用 `set -e`**——一步失败要继续跑完其余步骤，
# 一次看到全部问题，而不是修一个跑一次。
run_step() {
  local name="$1"; shift
  echo ""
  echo "───── $name ─────"
  if "$@"; then
    PASSED+=("$name")
  else
    FAILED+=("$name")
    echo "!! $name 失败"
  fi
}

echo "== 本地 CI 门禁（镜像 ci.yml check job）=="

# 与 CI 相同：全新 checkout 先准备前端资源，再编译 app。
if [[ -f frontend/package.json ]]; then
  run_step "frontend deps" bash -c "cd frontend && npm ci"
  run_step "vite build" bash -c "cd frontend && npm run build"
fi
run_step "cargo fmt --check" cargo fmt --check
# 全新 checkout 没有 externalBin，必须先生成 helper，app 的 build.rs 才能运行。
run_step "RDP helper build" bash scripts/build-rdp-helper.sh --debug
run_step "cargo clippy -D warnings" cargo clippy --workspace --all-targets -- -D warnings
run_step "cargo deny (licenses/bans/advisories)" cargo deny check licenses bans advisories

# ── RDP helper：**另一个工作区**，上面每一条都够不到它 ──────────────────
#
# `rdp-helper/` 有自己的 Cargo.toml 与 Cargo.lock（IronRDP 与 russh 的依赖树
# 不能相遇，见 `rdp-helper/Cargo.toml` 头部）。`--workspace` 只覆盖主工作区，
# `cargo deny` 也只审主 lockfile——而 helper 是**随安装包一起发的出货二进制**，
# 它那 600 多棵依赖树同样要过许可证与安全公告。
#
# 这是两工作区架构引入的一个洞。不补的话，一个进产物的二进制在门禁上是隐形的。
if [[ -f rdp-helper/Cargo.toml ]]; then
  run_step "helper: cargo fmt --check" cargo fmt --manifest-path rdp-helper/Cargo.toml --check
  run_step "helper: cargo clippy -D warnings" cargo clippy --manifest-path rdp-helper/Cargo.toml --all-targets --locked -- -D warnings
  run_step "helper: cargo deny" cargo deny --manifest-path rdp-helper/Cargo.toml check licenses bans advisories
fi

if [[ "$SKIP_SLOW" == "0" ]]; then
  # 两条测试入口都要跑（ci.yml 的 S23 注记）：nextest 每测试一个独立进程，会把
  # 「进程全局状态」的竞态整个藏住；cargo test 是单进程多线程，那是另一张网。
  if command -v cargo-nextest >/dev/null 2>&1; then
    run_step "cargo nextest run" cargo nextest run --workspace --no-tests pass
  else
    echo "（跳过 nextest：未安装 cargo-nextest，CI 上由 taiki-e/install-action 装）"
  fi
  run_step "cargo test（单进程多线程）" cargo test --workspace
  if [[ -f rdp-helper/Cargo.toml ]]; then
    run_step "helper: cargo test" cargo test --manifest-path rdp-helper/Cargo.toml --locked
  fi
fi

# 容器集成测试：与 ci.yml 的 Linux 步骤同口径（那一步带 FS_ITEST=1）。
# 本机是 Windows，跑的是「Windows 宿主 + Linux 容器」这一路，不等同于 CI 的 Linux runner，
# 但它是本地唯一能真正执行这些测试的方式。
if [[ "$WITH_ITEST" == "1" ]]; then
  if docker info >/dev/null 2>&1; then
    run_step "容器 itest（FS_ITEST=1）" env FS_ITEST=1 cargo test -p fs_itest
  else
    echo ""
    echo "!! --with-itest 指定了，但 Docker 不可用——容器测试**未执行**（不是通过）"
    FAILED+=("容器 itest（Docker 不可用）")
  fi
fi

# 前端：ci.yml 的 frontend job
if [[ -f frontend/package.json ]]; then
  run_step "vitest" bash -c "cd frontend && npx vitest run"
  run_step "svelte-check" bash -c "cd frontend && npx svelte-check --threshold error"
  # 部分 npm 镜像未实现 audit endpoint；安全公告使用官方源，安装源配置不变。
  run_step "npm audit (production)" bash -c "cd frontend && npm audit --omit=dev --registry=https://registry.npmjs.org"
fi
run_step "version gate selftest" node .github/scripts/check-version-consistency.mjs --selftest
run_step "version consistency" node .github/scripts/check-version-consistency.mjs
run_step "documentation links" node scripts/check-doc-links.mjs

echo ""
echo "════════ 结果 ════════"
for p in "${PASSED[@]:-}"; do [[ -n "$p" ]] && echo "  绿  $p"; done
for f in "${FAILED[@]:-}"; do [[ -n "$f" ]] && echo "  红  $f"; done

if [[ ${#FAILED[@]} -gt 0 ]]; then
  echo ""
  echo "有 ${#FAILED[@]} 步失败——CI 上会是同样的结果。"
  exit 1
fi
echo ""
echo "全部通过。注意：这只覆盖 Windows 一路；Linux 见 scripts/linux-precheck.sh，macOS 无环境。"
