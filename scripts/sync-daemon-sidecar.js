#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { execSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..");

function getTargetTriple() {
  try {
    const output = execSync("rustc -vV", { encoding: "utf8" });
    const match = output.match(/host:\s*([^\r\n]+)/);
    if (match && match[1]) {
      return match[1].trim();
    }
  } catch (err) {
    console.warn("Could not determine target triple from rustc:", err.message);
  }
  if (process.platform === "darwin") {
    return process.arch === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin";
  } else if (process.platform === "win32") {
    return "x86_64-pc-windows-msvc";
  } else {
    return process.arch === "arm64" ? "aarch64-unknown-linux-gnu" : "x86_64-unknown-linux-gnu";
  }
}

const targetTriple = process.env.TAURI_ENV_TARGET_TRIPLE || getTargetTriple();
const isRelease = process.argv.includes("--release") || process.env.NODE_ENV === "production";
const profile = isRelease ? "release" : "debug";
const exeExt = targetTriple.includes("windows") ? ".exe" : "";

const sidecarDir = path.join(repoRoot, "src-tauri", "binaries");
const sidecarFile = path.join(sidecarDir, `terminator-daemon-${targetTriple}${exeExt}`);

if (!fs.existsSync(sidecarDir)) {
  fs.mkdirSync(sidecarDir, { recursive: true });
}

console.log(`Building terminator-daemon (${profile}) for ${targetTriple}...`);
const cargoCmd = `cargo build -p terminator-daemon ${isRelease ? "--release" : ""}`;
execSync(cargoCmd, { cwd: repoRoot, stdio: "inherit" });

const sourceBin = path.join(repoRoot, "target", profile, `terminator-daemon${exeExt}`);
if (!fs.existsSync(sourceBin)) {
  // Fallback to debug if release not built, or vice versa
  const altBin = path.join(repoRoot, "target", isRelease ? "debug" : "release", `terminator-daemon${exeExt}`);
  if (fs.existsSync(altBin)) {
    fs.copyFileSync(altBin, sidecarFile);
    console.log(`Copied ${altBin} -> ${sidecarFile}`);
  } else {
    console.error(`Error: Expected ${sourceBin} to exist after cargo build.`);
    process.exit(1);
  }
} else {
  fs.copyFileSync(sourceBin, sidecarFile);
  fs.chmodSync(sidecarFile, 0o755);
  console.log(`Copied ${sourceBin} -> ${sidecarFile}`);
}
