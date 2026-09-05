// 审计2 #7：前端依赖的第三方许可清单生成器。
// 从 `npm sbom --sbom-format cyclonedx --json`（npm ≥10.9 自带，不引入新依赖）解析
// 组件清单，输出 name@version — license 行（purl 给出注册表坐标）。
// SBOM 里许可证声明缺失的组件（registry 元数据不带 license 字段）回落读本地
// node_modules/<name>/package.json 的 license 字段——生成在 `npm ci` 之后运行，
// 本地树在场；两头都没有才如实记「无许可证声明」。
// 用法：node scripts/gen-licenses.mjs > THIRD_PARTY_NOTICES.npm.md
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const frontend = join(here, "..");
const isWin = process.platform === "win32";

// 自检模式先行（函数声明提升，定义在后）：不依赖 npm sbom，无网络/无 lock 也能跑
if (process.argv.includes("--selftest")) {
  selftest();
  process.exit(0);
}

const SBOM_ARGS = ["sbom", "--sbom-format", "cyclonedx", "--json", "--omit", "dev"];
// Windows 直接 spawn npm.cmd 会 EINVAL；经 %ComSpec% /c 转发（参数为固定 flag、无空格，
// 不存在注入面），避免 shell:true 的参数拼接告警。
const raw = isWin
  ? execFileSync(process.env.ComSpec, ["/c", "npm", ...SBOM_ARGS], {
      cwd: frontend,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
    })
  : execFileSync("npm", SBOM_ARGS, {
      cwd: frontend,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
    });
const bom = JSON.parse(raw);
const components = Array.isArray(bom.components) ? bom.components : [];

/** purl pkg:npm/<pct-encoded-name>@<version> → 本地 node_modules 目录名。 */
function npmDirName(purl) {
  if (!purl) return null;
  const m = /^pkg:npm\/(.+?)@/.exec(purl);
  if (!m) return null;
  try {
    return decodeURIComponent(m[1]);
  } catch {
    return m[1];
  }
}

/** SBOM 缺许可证声明时回落本地 node_modules 的 package.json license 字段。 */
function localLicenseField(name) {
  if (!name) return null;
  const pkgPath = join(frontend, "node_modules", name, "package.json");
  if (!existsSync(pkgPath)) return null;
  try {
    const pkg = JSON.parse(readFileSync(pkgPath, "utf8"));
    return typeof pkg.license === "string" ? pkg.license : pkg.license?.type ?? null;
  } catch {
    return null;
  }
}

function licenseOf(c) {
  const lics = c.licenses ?? [];
  if (lics.length) {
    // CycloneDX 两种形状：{license:{id|name}} 与 {expression}（npm 对 SPDX 表达式组件
    // 走后者，如 tauri 的 "Apache-2.0 OR MIT"）
    const out = lics
      .map((l) => l.expression ?? l.license?.id ?? l.license?.name ?? null)
      .filter(Boolean)
      .join(" OR ");
    if (out) return out;
  }
  return localLicenseField(npmDirName(c.purl)) ?? "无许可证声明";
}

const rows = components
  .map((c) => {
    const name = c.name ?? "未知组件";
    const version = c.version ?? "?";
    const lic = licenseOf(c);
    const purl = c.purl ?? "";
    return `- ${name}@${version} — ${lic}${purl ? `（${purl}）` : ""}`;
  })
  .sort((a, b) => a.localeCompare(b, "en"));

process.stdout.write(
  [
    `前端组件 ${components.length} 个（npm 生产依赖树，由 npm sbom / CycloneDX 生成）`,
    "",
    ...rows,
    "",
  ].join("\n"),
);

// ── 自检（--selftest）───────────────────────────────────────────────────────
// 钉死 CycloneDX 三种许可证形状的解析（{license.id}/{license.name}/{expression}）
// 与本地回落：任一分支被改坏（如去掉 expression 支持，tauri 包会退回「未知」），
// 自检先红——脚本是发版材料的数据源，输出「未知」数量回归就是合规回归。
// 自检不依赖 npm sbom（无网络/无 lock 也能跑），故放在生成流程之前。
export function selftest() {
  const shapes = [
    { licenses: [{ license: { id: "MIT" } }], purl: "" },
    { licenses: [{ license: { name: "Boost Software License 1.0" } }], purl: "" },
    { licenses: [{ expression: "Apache-2.0 OR MIT" }], purl: "" },
    { licenses: [], purl: "pkg:npm/missing@1.0.0" },
  ];
  const got = shapes.map((s) => licenseOf(s));
  const want = [
    "MIT",
    "Boost Software License 1.0",
    "Apache-2.0 OR MIT",
    "无许可证声明",
  ];
  if (JSON.stringify(got) !== JSON.stringify(want)) {
    console.error(`selftest 失败：${JSON.stringify(got)} ≠ ${JSON.stringify(want)}`);
    process.exit(1);
  }
  console.log("selftest 通过（四种许可证形状解析正确）");
}

