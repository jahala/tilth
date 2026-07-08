#!/usr/bin/env node

"use strict";

const https = require("https");
const fs = require("fs");
const path = require("path");
const { execSync } = require("child_process");

const PLATFORM_MAP = {
  "linux-x64": "x86_64-unknown-linux-musl",
  "linux-arm64": "aarch64-unknown-linux-musl",
  "darwin-x64": "x86_64-apple-darwin",
  "darwin-arm64": "aarch64-apple-darwin",
  "win32-x64": "x86_64-pc-windows-msvc",
};

const key = `${process.platform}-${process.arch}`;
const target = PLATFORM_MAP[key];

if (!target) {
  console.error(`tilth: unsupported platform ${key}`);
  console.error(`Supported: ${Object.keys(PLATFORM_MAP).join(", ")}`);
  process.exit(1);
}

const isWindows = process.platform === "win32";
const ext = isWindows ? "zip" : "tar.gz";
const binName = isWindows ? "tilth.exe" : "tilth";
// Fork rolling build: assets live on the fixed `nightly` prerelease, not a
// per-version tag, so this URL is version-independent.
const url = `https://github.com/paulnsorensen/tilth/releases/download/nightly/tilth-${target}.${ext}`;

const binDir = path.join(__dirname, "bin");
const binPath = path.join(binDir, binName);

// Always refresh: each npm version maps to a fresh nightly build, so never
// short-circuit on an existing (stale) binary.
fs.mkdirSync(binDir, { recursive: true });

console.log(`tilth: downloading ${target} nightly binary...`);

const MAX_REDIRECTS = 5;

// HTTPS-only, depth-capped: this binary is executed after download, so never
// let a redirect downgrade to plaintext or loop.
function follow(url, depth, callback) {
  if (!url.startsWith("https:")) {
    console.error(`tilth: refusing non-HTTPS download URL: ${url}`);
    process.exit(1);
  }
  if (depth > MAX_REDIRECTS) {
    console.error(`tilth: too many redirects (>${MAX_REDIRECTS})`);
    process.exit(1);
  }
  https.get(url, { headers: { "User-Agent": "tilth-npm" } }, (res) => {
    if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
      follow(res.headers.location, depth + 1, callback);
    } else if (res.statusCode !== 200) {
      console.error(`tilth: download failed (HTTP ${res.statusCode})`);
      console.error(`URL: ${url}`);
      console.error("Install manually: cargo install --git https://github.com/paulnsorensen/tilth");
      process.exit(1);
    } else {
      callback(res);
    }
  }).on("error", (err) => {
    console.error(`tilth: download failed: ${err.message}`);
    console.error("Install manually: cargo install --git https://github.com/paulnsorensen/tilth");
    process.exit(1);
  });
}

follow(url, 0, (res) => {
  if (isWindows) {
    const tmpZip = path.join(binDir, "tilth.zip");
    const out = fs.createWriteStream(tmpZip);
    res.pipe(out);
    out.on("finish", () => {
      out.close();
      try {
        execSync(`tar -xf "${tmpZip}" -C "${binDir}"`, { stdio: "ignore" });
        fs.unlinkSync(tmpZip);
      } catch {
        console.error("tilth: failed to extract.");
        process.exit(1);
      }
    });
  } else {
    const tar = require("child_process").spawn("tar", ["xz", "-C", binDir], {
      stdio: ["pipe", "inherit", "inherit"],
    });
    res.pipe(tar.stdin);
    tar.on("close", (code) => {
      if (code !== 0) {
        console.error("tilth: failed to extract.");
        process.exit(1);
      }
      fs.chmodSync(binPath, 0o755);
      console.log("tilth: installed successfully");
    });
  }
});
