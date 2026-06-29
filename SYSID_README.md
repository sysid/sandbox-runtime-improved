# sysid branch — Fork Notes

Published as [`@sysid/sandbox-runtime-improved`](https://www.npmjs.com/package/@sysid/sandbox-runtime-improved) on npm.

```bash
npm install -g @sysid/sandbox-runtime-improved
```

This branch tracks changes on top of `main` from
[anthropic-experimental/sandbox-runtime](https://github.com/anthropic-experimental/sandbox-runtime).

## Versioning

`<upstream_version>-sysid.<fork_patch>` — e.g. `0.0.51-sysid.1` is the 1st fork release based on upstream `0.0.51`.

## Releasing

Two preconditions:

1. Docker Desktop must be running — build-seccomp uses QEMU containers for both architectures
2. npm login — check-npm-login fails fast if not

If you only want individual stages: make publish (build + publish, no install), or npm install -g
@sysid/sandbox-runtime-improved alone for the install. Don't confuse make install — that's just npm
install for dev dependencies, not the global CLI install.

```bash
git commit -am "chore: rebase onto upstream v0.0.54, bump fork version to 0.0.54-sysid.1"
git push origin main                  # fast-forwarded main
git push --force-with-lease           # sysid (diverged: 56 vs 19 commits — expected after rebase)

npm login

make all

make all
 └─ publish
     ├─ check            # lint-check + typecheck + test
     ├─ check-npm-login  # aborts if not logged into npm
     ├─ build-seccomp    # Docker cross-compiles apply-seccomp (x64 + arm64)
     ├─ clean            # rm -rf dist
     ├─ build            # tsc
     └─ npm publish --access public --tag latest
         └─ prepublishOnly  # verifies seccomp binaries present in vendor/, aborts if missing
 └─ npm install -g @sysid/sandbox-runtime-improved   # installs what was just published

```

### Rebase onto a new upstream version

```bash
# 1. Fast-forward local `main` to `upstream/main` (no checkout).
#    Refuses to run if `main` has diverged — fork work belongs on `sysid`.
make sync-main

# 2. Rebase `sysid` onto the new upstream tip.
#    Conflict policy: prefer upstream; drop fork code superseded by it.
git rebase upstream/main

# 3. Bump the fork version to match the new upstream base.
npm version <new_upstream>-sysid.1 --no-git-tag-version

# 4. Sync the upstream-version pointer in README.
make update-readme-version

# 5. Build first — integration tests spawn `dist/cli.js` and will fail otherwise.
npm run build

# 6. Lint + typecheck + test.
make check

# 7. Commit and push.
git commit -am "chore: rebase onto upstream v<new_upstream>, bump fork version to <new_upstream>-sysid.1"
git push origin main                  # the fast-forward from step 1
git push --force-with-lease            # sysid — only after local verification
```

The `/rebase-upstream` Claude Code skill drives steps 1–6 (conflict-resolution
rules, build-before-test ordering, verification). Step 7 is always manual —
the skill never commits or pushes.

### Re-publish without an upstream change

```bash
make bump-fork                     # 0.0.51-sysid.1 → 0.0.51-sysid.2
make publish                       # check + build-seccomp + clean + build + npm publish
```

### Make targets reference

| Target | Purpose |
|---|---|
| `make rebase-upstream` | Fetches upstream and prints the manual steps |
| `make bump-fork` | Bumps fork patch only (`-sysid.N` → `-sysid.N+1`) |
| `make update-readme-version` | Syncs README upstream-version pointer to `package.json` |
| `make publish` | Full release pipeline (depends on `build-seccomp`) |

## Changes vs. main

### 1. fix: ensure sandbox TMPDIR exists before first use

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

### 2. fix: make Node fetch() honour sandbox proxy env vars

**File:** `src/sandbox/sandbox-utils.ts`

Node's built-in `fetch()` (undici) ignores `HTTP_PROXY`/`HTTPS_PROXY` by
default — unlike `curl` and other CLI tools. On Node 22+, the
`--use-env-proxy` flag tells undici to read these variables.

`generateProxyEnvVars` now sets `NODE_OPTIONS=--use-env-proxy` (prepended to
any existing `NODE_OPTIONS`) when proxy ports are configured and Node >= 22.

### 3. feat: add allowBrowserProcess config for macOS sandbox

Adds an opt-in `allowBrowserProcess` config option (default: `false`) that
grants the Seatbelt permissions Chromium-based browsers need to launch.

**Warning — this significantly weakens the sandbox.** When enabled, the
following broad Seatbelt rules are added:

| Rule | What it allows |
|---|---|
| `(allow mach*)` | **All** Mach IPC — bootstrap registration, service lookups, task ports, cross-domain lookups. Needed for Crashpad, window server, CoreDisplay, GPU process, etc. |
| `(allow process-info*)` | Inspect **any** process on the system. Chrome manages renderer, GPU, utility, and crashpad children outside the sandbox boundary. |
| `(allow iokit-open)` | Broad IOKit device access for GPU and display management. |
| `(allow ipc-posix-shm*)` | Unrestricted POSIX shared memory (renderer ↔ GPU communication). |

Filesystem and network restrictions remain enforced. Only enable when
browser automation (e.g. `agent-browser`) is required.

**Configuration** (`~/.srt-settings.json`):

```json
{
  "allowBrowserProcess": true
}
```

### 4. fix: report correct version in `srti --version`

**Files:** `src/cli.ts`, `test/cli.test.ts`

`srti --version` previously reported `1.0.0` because `process.env.npm_package_version`
is only set when running via `npm run` — not when invoking the binary directly.
Now reads the version from `package.json` via `createRequire`.

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

### 6. feat: Docker-based seccomp binary cross-compilation for local publishing

**Files:** `Makefile`, `package.json`, `vendor/seccomp/Dockerfile.build`

Upstream PR [#199](https://github.com/anthropic-experimental/sandbox-runtime/pull/199) bakes BPF
bytecode into the `apply-seccomp` binary at compile time. The upstream CI builds these on native
Linux runners (x64 + arm64). Since we publish locally from macOS via `make publish`, the
`build:seccomp` script (which requires Linux + gcc + libseccomp-dev) cannot run natively.

A Docker-based build step cross-compiles static ELF binaries for both architectures using
platform-specific containers (QEMU emulation via Docker Desktop).

**Requires:** Docker Desktop running.

**Make targets:**

| Target | Description |
|---|---|
| `make build-seccomp` | Build `apply-seccomp` for x64 and arm64 via Docker |
| `make check-package` | Build binaries, pack tarball, verify both are included |
| `make publish` | Now depends on `build-seccomp` automatically |

**Safety net:** `prepublishOnly` in `package.json` aborts `npm publish` if either binary is
missing from the package.

```bash
  make publish
    → check (lint, typecheck, test)
    → check-npm-login
    → build-seccomp          ← Docker builds x64 + arm64 binaries into vendor/seccomp/
    → clean                  ← removes dist/
    → build                  ← tsc compiles TypeScript
    → npm publish
        → prepublishOnly     ← verifies vendor/seccomp binaries present, aborts if missing
```

The "files" field in package.json ships `vendor/seccomp`, so the Docker-built binaries end up
in the tarball at vendor/seccomp/{x64,arm64}/apply-seccomp directly (no copy into dist/ — upstream
dropped the dead `dist/vendor` copy in v0.0.61, since the seccomp resolver already finds binaries
at the package-root `vendor/` location). You can verify with make check-package anytime.

## Notable upstream features

Features that live in upstream but are worth calling out for fork users.

### In-process TLS termination + per-request filter (upstream v0.0.51)

**What it is.** SRT can now terminate TLS *in-process*, decrypt the HTTPS
traffic of the sandboxed child, and run a per-request JavaScript callback
that decides allow/deny. Before v0.0.51 the proxy only saw `CONNECT
host:port` for HTTPS and could enforce the domain allowlist but nothing
finer. Now you get the full method, URL, headers and (optionally) body of
every HTTPS request, in your own code, without running a separate MITM
proxy.

**What it is for.**

- Per-endpoint policy on HTTPS APIs ("allow `POST /v1/messages`, deny
  `POST /v1/admin/*`") — not just per-host allowlisting.
- Header- or body-level inspection (redact, log, rate-limit, block on
  prompt-injection patterns, etc.) where you previously had to stand up
  `mitmproxy` or similar.
- Replacing an external `mitmProxy` Unix-socket setup with a callback
  inside the same Node/Bun process.

**Two pieces that work together.**

| Knob | Purpose | Default |
|---|---|---|
| `network.tlsTerminate` | Turn on in-process TLS termination. Either supply a CA cert+key, or omit both and SRT generates an ephemeral RSA-2048 CA into a temp directory for the session. | off |
| `network.filterRequest` | `async (Request) => { action, reason? }` callback. Runs on plain HTTP through the proxy *and* on terminated HTTPS. Throw = deny (fails closed). | none |

Each is independently useful: `filterRequest` alone gates plain HTTP;
`tlsTerminate` alone gives you visibility (e.g. logging) without a
gating callback. Together they give you HTTPS-aware policy.

**Mutually exclusive with `mitmProxy`.** Both set ⇒ `initialize()`
throws. The external-MITM path and the in-process path can't coexist.

**Use as a library** (`@sysid/sandbox-runtime-improved`):

```ts
import { SandboxManager } from '@sysid/sandbox-runtime-improved'

await SandboxManager.initialize({
  network: {
    allowedDomains: ['api.anthropic.com'],

    // Omit caCertPath/caKeyPath and SRT mints an ephemeral CA for this
    // session, dropped in a temp dir, cleaned up by reset().
    tlsTerminate: {},

    // Runs on every parsed request — plain HTTP and terminated HTTPS.
    filterRequest: async req => {
      const url = new URL(req.url)
      if (url.pathname.startsWith('/v1/admin/')) {
        return { action: 'deny', reason: 'admin endpoints disabled' }
      }
      return { action: 'allow' }
    },
  },
  // ...rest of your config
})
```

**Or bring your own CA** (e.g. to share trust across multiple sandbox
sessions, or to pre-install the CA in a long-lived workspace):

```ts
tlsTerminate: {
  caCertPath: '/etc/srt/ca.crt',
  caKeyPath:  '/etc/srt/ca.key',  // RSA only — node-forge can't sign with EC
}
```

`caCertPath` and `caKeyPath` are paired: supplying only one is a config
error.

**Trust env vars injected into the sandboxed child.** SRT writes the CA
cert path into nine common per-tool trust-store variables so curl,
Node `fetch`, `pip`, `git`, `aws`, `cargo`, `deno`, etc. accept the
proxy-minted leaves without you doing anything:

```
NODE_EXTRA_CA_CERTS  SSL_CERT_FILE       CURL_CA_BUNDLE
REQUESTS_CA_BUNDLE   PIP_CERT            GIT_SSL_CAINFO
AWS_CA_BUNDLE        CARGO_HTTP_CAINFO   DENO_CERT
```

The child also gets read access to the CA cert path even if it falls
under your `denyRead` config — otherwise the child couldn't read its
own trust anchor.

**Failure mode is deny.** If your callback throws, rejects, returns
malformed data, or receives a malformed request, SRT responds `403`
with `X-Proxy-Error: blocked-by-sandbox-runtime` and the reason in the
body. A security boundary must fail closed.

**Caveats.**
- HTTP/1.1 only on the terminated leg — HTTP/2 negotiates down.
- No WebSocket / `Upgrade` support yet (the inner server refuses them).
- The terminating leg currently ignores `network.parentProxy`
  (acknowledged `TODO` upstream); if you need corporate-proxy chaining
  for the upstream side, stick with `mitmProxy` for now.

For the full design walk-through (data flow, why a per-connection
Unix-socket inner server, the AKI/SKI gotcha) see
`thoughts/comprehend/2026-05-12-upstream-v0.0.51-terminating-tls.md`.

## Acknowledgments

PRs on the original sandbox-runtime repo from:
- https://github.com/carderne
- https://github.com/MarlzRana
