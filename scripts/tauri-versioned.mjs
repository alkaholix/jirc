import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const { version } = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const targetDir = join(root, "src-tauri", `target-${version}`);
const executable = join(
  root,
  "node_modules",
  ".bin",
  process.platform === "win32" ? "tauri.cmd" : "tauri"
);

console.log(`Tauri/Cargo output: ${targetDir}`);
const result = spawnSync(executable, process.argv.slice(2), {
  cwd: root,
  env: { ...process.env, CARGO_TARGET_DIR: targetDir },
  stdio: "inherit",
  shell: process.platform === "win32",
});

if (result.error) throw result.error;
process.exit(result.status ?? 1);
