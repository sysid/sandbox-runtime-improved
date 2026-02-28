# sysid branch — Fork Notes

This branch tracks changes on top of `main` from
[anthropic-experimental/sandbox-runtime](https://github.com/anthropic-experimental/sandbox-runtime).

## Changes vs. main

### 1. fix: allow access to `com.apple.SystemConfiguration.configd`

**Commit:** `2e97944`
**File:** `src/sandbox/macos-sandbox-utils.ts`

Adds `com.apple.SystemConfiguration.configd` to the allowed Mach service
lookups in the macOS sandbox profile. Tools like `uv` query `configd` to
discover network configuration (proxies, DNS, interfaces). Without this
allowance, network-dependent operations fail inside the sandbox.

The service is read-only and standard for any networked macOS application.

### 2. chore: include built `dist/` for git-based installs

**Commit:** `c9e564e`

Adds the compiled `dist/` directory to the repository despite it being
listed in `.gitignore`.

**Why:** When this package is installed directly from git
(`npm install github:user/repo`), npm does **not** run a build step.
Without `dist/` checked in, git-based installs would ship an empty
package with no compiled output. Registry installs (`npm install
@anthropic-ai/sandbox-runtime`) are unaffected because `dist/` is built
during `prepublish`.

> `.gitignore` only prevents *untracked* files from being staged.
> Once force-added (`git add -f dist/`), the files remain tracked
> regardless of `.gitignore`.
