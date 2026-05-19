#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const tsgoDir = path.dirname(fileURLToPath(import.meta.url));
const manifest = path.join(tsgoDir, "../node_modules_files.rs");
const source = fs.readFileSync(manifest, "utf8");
const includePaths = [...source.matchAll(/include_str!\("([^"]+)"\)/g)].map((m) => m[1]);

function formatBytes(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value.toFixed(value >= 10 ? 1 : 2)} ${units[unit]}`;
}

let totalBytes = 0;
const missing = [];

for (const rel of includePaths) {
  const abs = path.join(tsgoDir, "../", rel);
  try {
    totalBytes += fs.statSync(abs).size;
  } catch {
    missing.push(rel);
  }
}

if (missing.length > 0) {
  console.error("Missing files:");
  for (const p of missing) console.error(`  ${p}`);
  process.exit(1);
}

console.log(`files: ${includePaths.length}`);
console.log(`total: ${formatBytes(totalBytes)} (${totalBytes.toLocaleString()} bytes)`);
