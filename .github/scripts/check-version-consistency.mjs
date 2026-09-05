import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
const semver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const section = (text, name) => text.split(`[${name}]`)[1]?.split(/^\[/m)[0] ?? "";
const version = (text) => /^version\s*=\s*"([^"]+)"/m.exec(text)?.[1];

export function checkVersions(read, ref = "") {
  const cargo = version(section(read("Cargo.toml"), "workspace.package"));
  assert.match(cargo ?? "", semver, "workspace version must be X.Y.Z");
  const pkg = JSON.parse(read("frontend/package.json"));
  const lock = JSON.parse(read("frontend/package-lock.json"));
  const sources = {
    tauri: JSON.parse(read("app/tauri.conf.json")).version,
    frontend: pkg.version,
    "frontend lock": lock.version,
    "frontend lock root": lock.packages?.[""]?.version,
    helper: version(section(read("rdp-helper/Cargo.toml"), "package")),
  };
  const dbVersion = /pub const APP_VERSION: &'static str = ([^;]+);/.exec(read("crates/connmgr/src/db.rs"))?.[1];
  assert.equal(dbVersion, 'env!("CARGO_PKG_VERSION")', "APP_VERSION must follow its Cargo package");
  assert.match(section(read("crates/connmgr/Cargo.toml"), "package"), /^version\.workspace\s*=\s*true\s*$/m);
  for (const [file, required] of [
    ["Cargo.lock", ["future-shell-app", "fs_connmgr", "fs_rdpproto"]],
    ["rdp-helper/Cargo.lock", ["fs-rdp-helper", "fs_rdpproto"]],
  ]) {
    const local = read(file).split("[[package]]").slice(1).filter((p) => !/^source\s*=/m.test(p));
    const names = local.map((p) => /^name\s*=\s*"([^"]+)"/m.exec(p)?.[1]);
    for (const name of required) assert.ok(names.includes(name), `${file} missing ${name}`);
    for (const entry of local) {
      const name = /^name\s*=\s*"([^"]+)"/m.exec(entry)?.[1];
      sources[`${file}:${name}`] = version(entry);
    }
  }
  if (ref.startsWith("refs/tags/")) {
    assert.ok(ref.startsWith("refs/tags/v"), "release tag must start with v");
    sources.tag = ref.slice("refs/tags/v".length);
  }
  for (const [name, value] of Object.entries(sources)) assert.equal(value, cargo, `${name}: expected ${cargo}, got ${value}`);
  return cargo;
}

function selftest() {
  const fixture = {
    "Cargo.toml": '[package]\nversion = "9.9.9"\n[workspace.package]\nversion = "1.0.0"\n[workspace.dependencies]\nx = "2"',
    "app/tauri.conf.json": '{"version":"1.0.0"}',
    "frontend/package.json": '{"version":"1.0.0"}',
    "frontend/package-lock.json": '{"version":"1.0.0","packages":{"":{"version":"1.0.0"}}}',
    "rdp-helper/Cargo.toml": '[workspace]\n[package]\nversion = "1.0.0"',
    "crates/connmgr/Cargo.toml": '[package]\nversion.workspace = true',
    "crates/connmgr/src/db.rs": 'pub const APP_VERSION: &\'static str = env!("CARGO_PKG_VERSION");',
    "Cargo.lock": ["future-shell-app", "fs_connmgr", "fs_rdpproto"].map((n) => `[[package]]\nname = "${n}"\nversion = "1.0.0"\n`).join(""),
    "rdp-helper/Cargo.lock": ["fs-rdp-helper", "fs_rdpproto"].map((n) => `[[package]]\nname = "${n}"\nversion = "1.0.0"\n`).join(""),
  };
  const read = (p) => fixture[p];
  assert.equal(checkVersions(read), "1.0.0");
  assert.equal(checkVersions(read, "refs/tags/v1.0.0"), "1.0.0");
  for (const tag of ["v9.9.9", "v1.0.0-phase4a", "1.0.0", "v01.0.0"]) assert.throws(() => checkVersions(read, `refs/tags/${tag}`));
  let negative = 4;
  for (const file of Object.keys(fixture)) {
    const saved = fixture[file];
    for (const mutated of ["", saved.replaceAll("1.0.0", "1.0.1").replace("version.workspace = true", "version = \"1.0.1\"").replace('env!("CARGO_PKG_VERSION")', '"1.0.1"')]) {
      fixture[file] = mutated;
      assert.throws(() => checkVersions(read), `must reject changed/missing ${file}`);
      negative++;
    }
    fixture[file] = saved;
  }
  fixture["frontend/package-lock.json"] = '{"version":"1.0.0","packages":{"":{"version":"1.0.1"}}}';
  assert.throws(() => checkVersions(read));
  console.log(`selftest passed: 2 positive, ${negative + 1} negative`);
}

try {
  if (process.argv.includes("--selftest")) selftest();
  else console.log(`版本一致：${checkVersions((p) => fs.readFileSync(path.join(root, p), "utf8"), process.env.GITHUB_REF)}`);
} catch (error) {
  console.error(`版本一致性失败：${error.message}`);
  process.exitCode = 1;
}
