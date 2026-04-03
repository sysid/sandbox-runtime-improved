.DEFAULT_GOAL := help
MAKEFLAGS += --no-print-directory

SHELL	= bash
.ONESHELL:

################################################################################
# Building \
BUILD:  ## ############################################################

.PHONY: build
build:  ## compile TypeScript
	npm run build

SECCOMP_IMAGE := sysid/seccomp-builder

.PHONY: build-seccomp
build-seccomp:  ## cross-compile seccomp binaries via Docker (x64+arm64)
	docker build --platform linux/amd64 -t $(SECCOMP_IMAGE):amd64 -f vendor/seccomp/Dockerfile.build vendor/seccomp/
	docker run --rm --platform linux/amd64 \
		-v "$(CURDIR)/vendor:/build/vendor" \
		$(SECCOMP_IMAGE):amd64 bun vendor/seccomp/build.ts
	docker build --platform linux/arm64 -t $(SECCOMP_IMAGE):arm64 -f vendor/seccomp/Dockerfile.build vendor/seccomp/
	docker run --rm --platform linux/arm64 \
		-v "$(CURDIR)/vendor:/build/vendor" \
		$(SECCOMP_IMAGE):arm64 bun vendor/seccomp/build.ts
	@file vendor/seccomp/x64/apply-seccomp vendor/seccomp/arm64/apply-seccomp

.PHONY: clean
clean:  ## remove build artifacts
	npm run clean

.PHONY: typecheck
typecheck:  ## run type checking without emitting
	npm run typecheck

################################################################################
# Testing \
TESTING:  ## ############################################################

.PHONY: test
test:  ## run all tests
	npm run test

.PHONY: test-unit
test-unit:  ## run unit tests only
	npm run test:unit

.PHONY: test-integration
test-integration:  ## run integration tests only
	npm run test:integration

.PHONY: test-watch
test-watch:  ## run tests in watch mode
	bun test --watch

################################################################################
# Code Quality \
QUALITY:  ## ############################################################

.PHONY: lint
lint:  ## lint and autofix
	npm run lint

.PHONY: lint-check
lint-check:  ## lint without fixing
	npm run lint:check

.PHONY: format
format:  ## format source files with prettier
	npm run format

.PHONY: check
check: lint-check typecheck test  ## run all checks (lint + typecheck + test)

################################################################################
# Versioning \
VERSIONING:  ## ############################################################

.PHONY: bump-fork
bump-fork:  ## bump fork patch: 0.0.42-sysid.1 → 0.0.42-sysid.2
	@current=$$(node -p "require('./package.json').version"); \
	base=$$(echo "$$current" | sed 's/-sysid\.[0-9]*//' ); \
	patch=$$(echo "$$current" | grep -o '[0-9]*$$'); \
	next=$$((patch + 1)); \
	npm version "$$base-sysid.$$next" --no-git-tag-version; \
	echo "Bumped to $$base-sysid.$$next"

.PHONY: update-readme-version
update-readme-version:  ## update README with current upstream base version
	@upstream_ver=$$(node -p "require('./package.json').version.replace(/-sysid.*/, '')"); \
	sed -i.bak "s/(currently based on upstream \*\*v[0-9][^*]*\*\*)/(currently based on upstream **v$$upstream_ver**)/" README.md && rm -f README.md.bak; \
	echo "README updated to upstream v$$upstream_ver"

.PHONY: rebase-upstream
rebase-upstream:  ## fetch upstream and rebase, set new base version
	git fetch upstream
	@echo "Run: git rebase upstream/main"
	@echo "Then: npm version <new_upstream_ver>-sysid.1 --no-git-tag-version"
	@echo "Then: make update-readme-version"

.PHONY: check-npm-login
check-npm-login:  ## check if logged into npm
	@if ! npm whoami &>/dev/null; then \
		echo "Not logged into npm. Run 'npm login' first."; \
		exit 1; \
	fi
	@echo "npm: logged in as $$(npm whoami)"

.PHONY: check-github-token
check-github-token:  ## check if GITHUB_TOKEN is set
	@if [ -z "$$GITHUB_TOKEN" ]; then \
		echo "GITHUB_TOKEN is not set. Please export your GitHub token before running this command."; \
		exit 1; \
	fi
	@echo "GITHUB_TOKEN is set"

.PHONY: publish
publish: check check-npm-login build-seccomp clean build  ## run checks, build, compile seccomp, publish
	npm publish --access public --tag latest

################################################################################
# Setup \
SETUP:  ## ############################################################

.PHONY: all
all: publish  ## publishes and then installs locally from npm
	npm install -g @sysid/sandbox-runtime-improved



.PHONY: install
install:  ## install dependencies
	npm install

.PHONY: check-package
check-package: build-seccomp  ## verify seccomp binaries are included in npm package
	@tgz=$$(npm pack 2>/dev/null | tail -1); \
	echo "Checking $$tgz for seccomp binaries..."; \
	missing=0; \
	for arch in x64 arm64; do \
		if tar tf "$$tgz" | grep -q "vendor/seccomp/$$arch/apply-seccomp"; then \
			echo "  ✓ $$arch binary found"; \
		else \
			echo "  ✗ $$arch binary MISSING"; \
			missing=1; \
		fi; \
	done; \
	rm -f "$$tgz"; \
	[ $$missing -eq 0 ] || (echo "FAIL: seccomp binaries missing from package"; exit 1)

.PHONY: run
run:  ## run sandbox with echo test command
	node dist/cli.js -c "echo hello from sandbox"

################################################################################
# Misc \
MISC:  ## ############################################################

define PRINT_HELP_PYSCRIPT
import re, sys

for line in sys.stdin:
	match = re.match(r'^([a-zA-Z0-9_-]+):.*?## (.*)$$', line)
	if match:
		target, help = match.groups()
		print("\033[36m%-20s\033[0m %s" % (target, help))
endef
export PRINT_HELP_PYSCRIPT

.PHONY: help
help:
	@python -c "$$PRINT_HELP_PYSCRIPT" < $(MAKEFILE_LIST)
