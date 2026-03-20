# sysid branch — Fork Notes

This branch tracks changes on top of `main` from
[anthropic-experimental/sandbox-runtime](https://github.com/anthropic-experimental/sandbox-runtime).

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

### 3. feat: add allowBrowserProcess config for macOS sandbox

Allows configuring browser process access for macOS sandbox profiles.

### 4. feat: add allowMachLookup config for custom Mach service access (macOS)

Allows configuring custom Mach service lookups in macOS sandbox profiles.

### 5. known limitation: Copilot bash session hangs for outputs > ~4 KB

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
