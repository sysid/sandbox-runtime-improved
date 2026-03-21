# sysid branch — Fork Notes

Published as [`@sysid/sandbox-runtime-improved`](https://www.npmjs.com/package/@sysid/sandbox-runtime-improved) on npm.

```bash
npm install -g @sysid/sandbox-runtime-improved
```

This branch tracks changes on top of `main` from
[anthropic-experimental/sandbox-runtime](https://github.com/anthropic-experimental/sandbox-runtime).

## Versioning

`<upstream_version>-sysid.<fork_patch>` — e.g. `0.0.43-sysid.1` is the 1st fork release based on upstream `0.0.43`.

## Changes vs. main

### 1. fix: allow access to `com.apple.SystemConfiguration.configd`

**File:** `src/sandbox/macos-sandbox-utils.ts`

Adds `com.apple.SystemConfiguration.configd` to the allowed Mach service
lookups in the macOS sandbox profile. Tools like `uv` query `configd` to
discover network configuration (proxies, DNS, interfaces). Without this
allowance, network-dependent operations fail inside the sandbox.

The service is read-only and standard for any networked macOS application.

### 2. fix: ensure sandbox TMPDIR exists before first use

**Files:** `src/sandbox/sandbox-utils.ts`, `src/sandbox/sandbox-manager.ts`

`generateProxyEnvVars` always sets `TMPDIR=/tmp/claude` (or `$CLAUDE_TMPDIR`)
for sandboxed processes, but the directory was never created. When `TMPDIR`
points to a non-existent path, `mktemp` fails silently and returns an empty
string. Shell sessions that redirect to that empty string (e.g. `cat $tmp`)
then block on stdin — causing the Copilot shell tool to appear to hang after
printing just 1–2 lines of output.

`ensureSandboxTmpdir()` is now called from `initialize()` so the directory
always exists before any sandboxed command runs. `CLAUDE_TMPDIR` can override
the default `/tmp/claude`.

### 3. fix: make Node fetch() honour sandbox proxy env vars

**File:** `src/sandbox/sandbox-utils.ts`

Node's built-in `fetch()` (undici) ignores `HTTP_PROXY`/`HTTPS_PROXY` by
default — unlike `curl` and other CLI tools. On Node 22+, the
`--use-env-proxy` flag tells undici to read these variables.

`generateProxyEnvVars` now sets `NODE_OPTIONS=--use-env-proxy` (prepended to
any existing `NODE_OPTIONS`) when proxy ports are configured and Node >= 22.

### 4. feat: add allowBrowserProcess config for macOS sandbox

Adds an opt-in `allowBrowserProcess` config option (default: `false`) that
grants the Seatbelt permissions Chromium-based browsers need to launch.

### 5. fix: report correct version in `srti --version`

**Files:** `src/cli.ts`, `test/cli.test.ts`

`srti --version` previously reported `1.0.0` because `process.env.npm_package_version`
is only set when running via `npm run` — not when invoking the binary directly.
Now reads the version from `package.json` via `createRequire`.

### 6. feat: add allowMachLookup config for custom Mach service access (macOS)

Allows configuring custom Mach service lookups in macOS sandbox profiles.

### 7. known limitation: Copilot bash session hangs for outputs > ~4 KB

**Not a sandbox-runtime bug.** Reproducible in vanilla Copilot (no sandbox wrapper).

Copilot's internal bash session uses a PTY for command I/O. The macOS kernel PTY
buffer is ~4 KB. For commands that produce more than ~4 KB of output (e.g. a
large `git diff`, or `seq 1 5000`), the writer process blocks when the buffer
fills. Copilot's Node.js event loop drains the PTY too slowly to prevent the
deadlock — the writer never unblocks, the command appears to hang indefinitely.

**Workaround** — redirect output to a file, then operate on the file:

```bash
run 'git --no-pager diff main..HEAD -- path/ > /tmp/claude/diff.txt 2>&1 && wc -l /tmp/claude/diff.txt'
! cat /tmp/claude/diff.txt     # read directly in terminal, bypassing bash session
```

The file-redirect path bypasses the PTY entirely; the writer finishes instantly.
Using `! cmd` for subsequent reads avoids the PTY for large outputs.

## Acknowledgments

PRs on the original sandbox-runtime repo from:
- https://github.com/carderne
- https://github.com/MarlzRana
