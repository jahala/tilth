# Spec: Harden the npx shim (issue #124)

Bring tilth's npx install/run shim up to the hardening bar described in
[#124](https://github.com/paulnsorensen/tilth/issues/124): checksum verification,
HTTPS-only transport, redirect caps, timeouts, signal forwarding, and a nightly
CI regression smoke test.

## Context / ground truth (verified this session)

- **Two shim dirs, both download from the SAME URL.** `npm/` (canonical `tilth` /
  `@plotplot/tilth`) and `npm-nightly/` (`@paulnsorensen/tilth-nightly`) both
  download `https://github.com/paulnsorensen/tilth/releases/download/nightly/tilth-<target>.<ext>`.
  Fork law pins version to 0.8.4 and forbids tagging, so `release.yml` is dormant
  and the `nightly` rolling release is the only live asset source. **Therefore the
  `.sha256` sidecars both shims verify MUST be uploaded by `nightly.yml`.**
- **`npm-nightly/install.js` is already partially hardened** (HTTPS-only refuse,
  `MAX_REDIRECTS = 5`, dropped the `http` require). `npm/install.js` has NONE of
  that. Bring `npm/` to parity, then add the remaining items to both.
- **`npm/run.js` and `npm-nightly/run.js` are byte-identical** and both use
  `execFileSync` (no signal forwarding).
- **PLATFORM_MAP (5 entries) already matches the 5 build-matrix targets** in both
  `nightly.yml` and `release.yml`. Item #6 (platform map) is already satisfied —
  no change needed, but the new CI smoke test guards it going forward.
- The `.sha256` sidecar reference in the ticket (hallouminate `npm/` +
  `nightly-release-check.yml`) **no longer exists** — hallouminate removed its npm
  shim. Implement to this spec + standard Node practices, not by mirroring a file.

## Acceptance criteria

### 1. Workflows emit `.sha256` sidecars

In **`nightly.yml`** (critical — this is what the shims verify) and **`release.yml`**
(defensive parity for tag-release assets), after each asset is packaged, write a
sidecar `tilth-<target>.<ext>.sha256` next to it and upload it to the release.

- Sidecar format: standard `sha256sum` output — `<64-hex>␠␠<filename>` — one line.
- Portable generation across ubuntu / macOS / Windows-git-bash. macOS lacks
  `sha256sum`; use a fallback:
  ```bash
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$f" > "$f.sha256"
  else
    shasum -a 256 "$f" > "$f.sha256"
  fi
  ```
- `nightly.yml`: the "Upload asset to nightly release" loop must also upload the
  `.sha256` (extend the existing `for f in ...tar.gz ...zip` loop to also emit +
  upload the sidecar, `--clobber`).
- `release.yml`: add `tilth-*.sha256` to the `softprops/action-gh-release` `files:`
  list, and add the sidecar-generation step before the upload.
- Windows packaging uses `shell: bash` (Git Bash) which has `sha256sum` — fold the
  sidecar generation into each Package step, or a dedicated post-package step.

### 2. `install.js` hardening — BOTH `npm/install.js` and `npm-nightly/install.js`

Both files should end up functionally identical for the download path (keep their
existing per-package differences: `npm/` skips re-download if the binary already
exists; `npm-nightly/` always refreshes — preserve each). Required behavior:

- **HTTPS-only, including across redirects.** Refuse any non-`https:` URL before
  each request (port `npm-nightly`'s check into `npm/`; drop `npm/`'s `http`
  require). A `Location:` header that is non-https must abort with exit 1.
- **Cap redirects at 5.** Depth-counted `follow(url, depth, cb)`; exit 1 past 5.
  (Port into `npm/`.)
- **Drain redirect bodies.** Call `res.resume()` on every redirect response before
  recursing, so the socket frees.
- **Request timeout 30s.** On the request object: `req.setTimeout(30000, () => {
  req.destroy(new Error("timeout after 30s")); })`. A stalled connect/read must
  fail the install, not hang. Apply to BOTH the sidecar fetch and the archive
  fetch.
- **Checksum verification (the core item).**
  - Derive the sidecar URL as `${url}.sha256`. Fetch it first (same
    hardened `follow`), read the body, parse the leading 64-hex token as the
    expected digest. A missing/malformed sidecar → abort with a clear error +
    exit 1 (fail closed — never install an unverifiable binary).
  - Download the archive fully into a `Buffer` (accumulate chunks; still through
    the hardened `follow`).
  - Compute `crypto.createHash("sha256").update(buf).digest("hex")` and compare
    case-insensitively to expected. Mismatch → error naming both digests + exit 1,
    and do not extract.
  - On match: extract from the buffer.
    - Unix: `spawn("tar", ["xz", "-C", binDir])`, write the buffer to
      `tar.stdin`, `end()` it. Keep the existing close-code check + `chmod 0o755`.
    - Windows: write the buffer to a temp `tilth.zip`, `execSync("tar -xf ...")`,
      unlink. Keep existing behavior.
- Keep existing user-facing error messages / "Install manually" hints per file
  (they already differ: `npm/` says `cargo install tilth`, `npm-nightly/` says
  `cargo install --git ...`). Do not homogenize those.
- `require("crypto")` added; drop now-unused requires (`http`, `zlib` in `npm/`).

### 3. `run.js` signal forwarding — BOTH files

Replace `execFileSync` with `spawn` + signal forwarding so a signal sent to the
launcher reaches the binary and the launcher mirrors the child's terminating
signal:

```js
const { spawn } = require("child_process");
const child = spawn(bin, process.argv.slice(2), { stdio: "inherit" });
for (const sig of ["SIGTERM", "SIGINT", "SIGHUP"]) {
  process.on(sig, () => { if (!child.killed) child.kill(sig); });
}
child.on("error", (err) => {
  console.error(`tilth: failed to run binary at ${bin}`);
  console.error(err.message);
  process.exit(1);
});
child.on("exit", (code, signal) => {
  if (signal) { process.kill(process.pid, signal); }   // re-raise the child's signal
  else { process.exit(code ?? 0); }
});
```

Both `run.js` files must stay byte-identical to each other after the change.

### 4. Nightly CI regression smoke test

New job in `nightly.yml`, `needs: publish-npm`, matrix over
`ubuntu-latest` / `macos-latest` / `windows-latest`:

- `actions/setup-node@v6` (Node 24, matching the publish job).
- Fresh-install and run the just-published nightly package end to end:
  `npx --yes @paulnsorensen/tilth-nightly@latest --version` (or the binary's real
  version/help flag — confirm which flag the CLI supports; use `--version` if
  clap exposes it, else `--help`).
- Assert it exits 0 and prints a non-empty version/usage string. This exercises
  download → **checksum verify** → extract → exec on every platform, so shim /
  asset / platform-map breakage surfaces here.
- **Deviation from ticket wording, by design:** the ticket says "assert the
  release / npm / crates.io versions agree." The nightly channel is
  version-decoupled (`0.0.0-experimental.*`, npm-only, not on crates.io), so a
  cross-registry version-agreement check is not meaningful for nightly. The smoke
  test (does the published package install and run on each platform?) is the
  equivalent regression guard for this channel. Note this in the PR body.

## Out of scope

- No version bump / tagging (fork law).
- No change to PLATFORM_MAP (already correct).
- The nightly sigstore fix is a separate branch/PR (#132) — do not touch it.

## Verification

- `node --check` on all four JS files.
- YAML: `python3 -c "import yaml; yaml.safe_load(open(...))"` on both workflows.
- Local checksum-logic test: create a fixture archive + its `.sha256`, point the
  verify function at file:// or a local buffer, assert match passes and a tampered
  buffer fails closed. (Unit-style, no network.)
- `actionlint` if available.
