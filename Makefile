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
# Misc \
MISC:  ## ############################################################
.PHONY: install-srt
install-srt:  ## install Anthropic Sandbox Runtime globally via npm
	npm install -g @anthropic-ai/sandbox-runtime


.PHONY: run
run:  ## run sandbox with echo test command
	node dist/cli.js -c "echo hello from sandbox"

.PHONY: install
install:  ## install dependencies
	npm install

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
