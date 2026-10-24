# AgentDocker — everyday commands for people building from source.
#
# `make install` is the one most people want: build the release binaries,
# package the native app for this machine, and install it for this user so
# the next launch of AgentDocker (Dock, Spotlight, `agentdocker`, `agentd`)
# runs what you just built. Installation asks the daemon to reload; the
# experimental gate permits replacement while preserving agents. Otherwise
# the daemon keeps serving; `make restart-daemon` is the explicit fallback.
#
# Every target is a thin wrapper over the scripts the release pipeline uses,
# so a local install is the same payload shape as a published one.

SHELL := /bin/bash
.SHELLFLAGS := -eu -o pipefail -c
# Apple's make 3.81 lacks .ONESHELL, so multi-step recipes are single commands.
.DEFAULT_GOAL := help

# Cargo decides where artifacts go (CARGO_TARGET_DIR, .cargo/config.toml or
# target/); nothing here overrides that, so no extra build cache appears.
# docs/TESTING-AND-BENCHMARKS.md asks for one campaign per checkout.
PREFLIGHT := python3 scripts/build_storage.py
# Where packaged artifacts and smoke reports go. Packaging refuses to reuse a
# directory, so each run gets its own timestamped folder underneath. Spotlight
# skips a `.noindex` folder, and each packaged app is unregistered from Launch
# Services once packaging or installing has run (an assessment registers it),
# so `open -a AgentDocker` never opens a copy in the checkout.
ARTIFACTS ?= artifacts/local.noindex
STAMP := $(shell date +%Y%m%d-%H%M%S)
PACKAGE_DIR := $(ARTIFACTS)/desktop-$(STAMP)
VERSION := $(shell python3 -c 'import tomllib;print(tomllib.load(open("Cargo.toml","rb"))["workspace"]["package"]["version"])')
SOURCE := $(shell git rev-parse HEAD)
# Installation prefix; empty means this user's home (~/Applications, ~/.local/bin).
PREFIX ?=
PREFIX_FLAG := $(if $(PREFIX),--prefix "$(PREFIX)",)
ifeq ($(shell uname -s),Darwin)
PAYLOAD := AgentDocker.app
LOCAL_PREVIEW := --local-preview
else
PAYLOAD := agentdocker-desktop
LOCAL_PREVIEW :=
endif
LSREGISTER := /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
# Forget packaged apps in Launch Services; nothing where there is none.
FORGET = if [ -x "$(LSREGISTER)" ]; then for app in $(1); do "$(LSREGISTER)" -u "$$app" >/dev/null 2>&1 || true; done; fi

.PHONY: help build check test clippy fmt app install install-preview status rollback run smoke restart-daemon clean-artifacts

help: ## Show this help
	@awk 'BEGIN{FS=":.*## "} /^[a-zA-Z_-]+:.*## /{printf "  \033[1m%-16s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@echo
	@echo "Typical: make install   (build → package → install for this user; then quit and reopen AgentDocker)"

build: ## Debug build of every crate
	$(PREFLIGHT)
	cargo build --workspace

test: ## Unit and integration tests (nextest when installed)
	$(PREFLIGHT)
	if command -v cargo-nextest >/dev/null; then cargo nextest run --workspace; else cargo test --workspace; fi

clippy: ## Strict lint
	$(PREFLIGHT)
	cargo clippy --workspace --all-targets -- -D warnings

fmt: ## Format everything
	cargo fmt --all

check: ## The standard verification gate used before a PR
	bash scripts/verify.sh check

app: ## Release build + native package for this machine (no signing)
	$(PREFLIGHT)
	mkdir -p "$(ARTIFACTS)"
	native="$$(python3 scripts/build_native.py)" && \
	  bindir="$$(printf '%s' "$$native" | python3 -c 'import json,sys;print(json.load(sys.stdin)["binary_directory"])')" && \
	  target="$$(printf '%s' "$$native" | python3 -c 'import json,sys;print(json.load(sys.stdin)["target"])')" && \
	  python3 packaging/desktop/package.py --binary-dir "$$bindir" --output "$(PACKAGE_DIR)" \
	    --version "$(VERSION)" --source "$(SOURCE)" --target "$$target" && \
	  ln -sfn "desktop-$(STAMP)" "$(ARTIFACTS)/latest" && \
	  echo "Packaged $(PACKAGE_DIR)/$(PAYLOAD)"
	$(call FORGET,"$(PACKAGE_DIR)/$(PAYLOAD)")

install-preview: app ## Build, package, and show what `make install` would change
	"$(PACKAGED_CLI)" desktop $(PREFIX_FLAG) install --from "$(PACKAGE_DIR)/$(PAYLOAD)" $(LOCAL_PREVIEW) --preview
	$(call FORGET,"$(PACKAGE_DIR)/$(PAYLOAD)")

PACKAGED_CLI := $(PACKAGE_DIR)/$(PAYLOAD)/$(if $(filter Darwin,$(shell uname -s)),Contents/MacOS,bin)/agentdocker

install: app ## Build, package, and install for this user (takes effect on next launch)
	"$(PACKAGED_CLI)" desktop $(PREFIX_FLAG) install --from "$(PACKAGE_DIR)/$(PAYLOAD)" $(LOCAL_PREVIEW)
	$(call FORGET,"$(PACKAGE_DIR)/$(PAYLOAD)")
	"$(PACKAGED_CLI)" desktop $(PREFIX_FLAG) status
	@echo; echo "Installed. Quit and reopen AgentDocker to run this build."; \
	  echo "The report's 'daemon' entry says whether a running daemon reloaded to it; if it did not, make restart-daemon switches it once agent work is done"

status: ## Show the active and previous installed versions
	agentdocker desktop $(PREFIX_FLAG) status

rollback: ## Activate the previous retained version
	agentdocker desktop $(PREFIX_FLAG) rollback $(LOCAL_PREVIEW)

restart-daemon: ## Stop the running daemon so the next client starts the installed one (end agent work first)
	agentdocker daemon stop && echo "Daemon stopped; the next agentdocker command or app launch starts the installed build."

run: ## Run the desktop app from this checkout against your real daemon (debug build)
	$(PREFLIGHT)
	cargo run -p agentdocker-ui

smoke: ## Native workflow acceptance against the release binaries build_native.py reports
	$(PREFLIGHT)
	mkdir -p "$(ARTIFACTS)"
	native="$$(python3 scripts/build_native.py)" && \
	  bindir="$$(printf '%s' "$$native" | python3 -c 'import json,sys;print(json.load(sys.stdin)["binary_directory"])')" && \
	  python3 scripts/iced_workflow_smoke.py --binary-dir "$$bindir" --output "$(ARTIFACTS)/smoke-$(STAMP)"

clean-artifacts: ## Remove local package and smoke output (keeps the Cargo cache)
	$(call FORGET,"$(ARTIFACTS)"/desktop-*/$(PAYLOAD) artifacts/local/desktop-*/$(PAYLOAD))
	rm -rf "$(ARTIFACTS)" artifacts/local
