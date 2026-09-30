#!/usr/bin/env node
// Check repository Markdown links and in-app help destinations without network access.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function withoutFences(text) {
  let fence = null;
  return text.split(/\r?\n/).map((line) => {
    const match = line.match(/^\s*(`{3,}|~{3,})/);
    if (match && !fence) { fence = match[1]; return ""; }
    if (fence) {
      if (match && match[1][0] === fence[0] && match[1].length >= fence.length) fence = null;
      return "";
    }
    return line;
  }).join("\n");
}

function headingIds(text) {
  const seen = new Map();
  const ids = new Set();
  for (const match of withoutFences(text).matchAll(/^#{1,6}\s+(.+?)\s*#*$/gm)) {
    const base = match[1].toLowerCase().replace(/<[^>]*>/g, "")
      .replace(/[^\p{L}\p{N}\p{M}\s_-]/gu, "").replace(/ /g, "-");
    const count = seen.get(base) ?? 0;
    seen.set(base, count + 1);
    ids.add(base + (count ? "-" + count : ""));
  }
  for (const match of text.matchAll(/\b(?:id|name)=["']([^"']+)["']/g)) ids.add(match[1]);
  return ids;
}

export function validateLink(repoRoot, file, destination) {
  if (/^(?:https?:|mailto:|data:|app:)/i.test(destination)) return false;
  const [targetWithQuery, fragment] = destination.split("#", 2);
  const relative = decodeURIComponent(targetWithQuery.split("?", 1)[0]);
  if (/^(?:[A-Za-z]:|\/|\\)/.test(relative)) throw new Error("absolute filesystem path: " + destination);
  const target = relative ? path.resolve(path.dirname(file), relative) : file;
  const boundary = path.relative(repoRoot, target);
  if (boundary === ".." || boundary.startsWith(".." + path.sep) || path.isAbsolute(boundary)) {
    throw new Error("link escapes repository: " + destination);
  }
  if (!fs.existsSync(target)) throw new Error("missing target: " + destination);
  if (fragment && target.endsWith(".md") && !headingIds(fs.readFileSync(target, "utf8")).has(decodeURIComponent(fragment))) {
    throw new Error("missing heading: " + destination);
  }
  return true;
}

function markdownFiles(directory) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const file = path.join(directory, entry.name);
    if (entry.isDirectory()) return markdownFiles(file);
    return entry.isFile() && file.endsWith(".md") ? [file] : [];
  });
}

function main() {
  const documents = ["README.md", "CONTRIBUTING.md", "CHANGELOG.md"].map((file) => path.join(root, file));
  documents.push(...markdownFiles(path.join(root, "docs")));
  if (documents.length < 8) throw new Error("documentation set unexpectedly small");
  const failures = [];
  let checked = 0;
  for (const file of documents) {
    const text = withoutFences(fs.readFileSync(file, "utf8")).replace(/`[^`\n]*`/g, "");
    const links = [...text.matchAll(/!?\[[^\]\n]*\]\(\s*(?:<([^>]+)>|([^\s)]+))(?:\s+["'][^\n]*?["'])?\s*\)/g)];
    const references = [...text.matchAll(/^\s*\[[^\]]+\]:\s*(?:<([^>]+)>|(\S+))/gm)];
    for (const match of [...links, ...references]) {
      try { if (validateLink(root, file, match[1] ?? match[2])) checked++; }
      catch (error) { failures.push(path.relative(root, file) + ": " + error.message); }
    }
  }
  // These paths are assembled with the repository URL in App.svelte.
  const appFile = path.join(root, "frontend/src/App.svelte");
  const helpPaths = [...fs.readFileSync(appFile, "utf8").matchAll(/\/blob\/main\/(docs\/[^"`\s]+)/g)];
  if (!helpPaths.length) failures.push("no in-app documentation destination found");
  for (const match of helpPaths) {
    try { validateLink(root, path.join(root, "README.md"), match[1]); checked++; }
    catch (error) { failures.push("frontend/src/App.svelte: " + error.message); }
  }
  if (checked < 10) failures.push("too few local links checked");
  if (failures.length) throw new Error(failures.join("\n"));
  console.log("Documentation links: " + documents.length + " Markdown files, " + checked + " local destinations passed.");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { main(); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
