# Developer entry points for cometty. Run `make help` for the target index.
#
# The recipes wrap the commands documented in AGENTS.md and docs/development.md;
# keep those references authoritative when commands change. Stay compatible with
# GNU Make 3.81 (the macOS system make): no .SHELLFLAGS or newer features.
SHELL := /bin/bash
.DEFAULT_GOAL := help
.SUFFIXES:

CARGO ?= cargo
CARGO_FLAGS ?= --locked

RUN_ARGS ?=
FILTER ?=
TEST_ARGS ?=

BENCH_OUT ?=
PROFILE_TEMPLATE ?= Allocations
PROFILE_TIME ?= 60s
PROFILE_OUT ?= target/profile/allocations.trace
PROFILE_ARGS ?=

BUNDLE_ARCH ?= universal
BUNDLE_IDENTITY ?=
BUNDLE_NOTARY ?=

RELEASE_BUMP ?= patch
RELEASE_VERSION ?=
RELEASE_PUSH ?=
RELEASE_DRY_RUN ?=
RELEASE_SKIP_VERIFY ?=
RELEASE_REMOTE ?= origin
RELEASE_FLAGS = $(if $(strip $(RELEASE_VERSION)),--version '$(RELEASE_VERSION)',--bump '$(RELEASE_BUMP)') --remote '$(RELEASE_REMOTE)' $(if $(strip $(RELEASE_PUSH)),--push) $(if $(strip $(RELEASE_DRY_RUN)),--dry-run) $(if $(strip $(RELEASE_SKIP_VERIFY)),--no-verify)

# Every target is a command rather than an artifact name.
.PHONY: help build release run run-release test test-release check clippy fmt \
	fmt-check diff-check verify bench probe-scheduling probe-retained-glyphs \
	probe-uploads profile bundle deps clean

help: ## Show this target index
	@printf 'Usage: make <target> [VAR=value]\n\nTargets:\n'
	@awk 'BEGIN { FS = ":.*## " } /^[a-zA-Z0-9_-]+:.*## / { printf "  %-24s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@printf '\nExamples:\n'
	@printf '  make test FILTER=grid::history::\n'
	@printf '  make release RELEASE_DRY_RUN=1\n'
	@printf '  make release RELEASE_BUMP=minor RELEASE_PUSH=1\n'
	@printf '  make bench BENCH_OUT=target/perf/run1\n'
	@printf '  make profile PROFILE_TEMPLATE="Time Profiler" PROFILE_TIME=60s PROFILE_OUT=target/profile/cpu.trace\n'
	@printf '  make bundle BUNDLE_IDENTITY="Developer ID Application: ..." BUNDLE_NOTARY=profile-name\n'

build: ## Build the debug binary
	$(CARGO) build $(CARGO_FLAGS)

run: ## Launch the GUI (needs a display and GPU)
	$(CARGO) run $(CARGO_FLAGS) -- $(RUN_ARGS)

run-release: ## Launch the optimized GUI (needs a display and GPU)
	$(CARGO) run --release $(CARGO_FLAGS) -- $(RUN_ARGS)

test: ## Run tests (FILTER=..., TEST_ARGS=...)
	$(CARGO) test $(CARGO_FLAGS) $(FILTER) $(if $(strip $(TEST_ARGS)),-- $(TEST_ARGS))

test-release: ## Run tests in release mode (FILTER=..., TEST_ARGS=...)
	$(CARGO) test --release $(CARGO_FLAGS) $(FILTER) $(if $(strip $(TEST_ARGS)),-- $(TEST_ARGS))

check: ## Type-check the crate
	$(CARGO) check $(CARGO_FLAGS)

clippy: ## Lint every target with warnings denied
	$(CARGO) clippy --all-targets $(CARGO_FLAGS) -- -D warnings

fmt: ## Rewrite formatting (not used by verify)
	$(CARGO) fmt

fmt-check: ## Check formatting without rewriting
	$(CARGO) fmt --check

diff-check: ## Reject whitespace errors and conflict markers in the diff
	git diff --check

verify: test check clippy fmt-check diff-check ## Local gate: the AGENTS.md routine

bench: ## Headless release benchmark; BENCH_OUT=dir (see docs/performance.md)
	bash scripts/bench-headless.sh $(BENCH_OUT)

probe-scheduling: ## Opt-in probe: fair scheduling under load
	COMETTY_PERF=0 $(CARGO) test --release $(CARGO_FLAGS) pty::tests::fair_drain_under_load -- --ignored --exact --nocapture --test-threads=1

probe-retained-glyphs: ## Opt-in GPU probe: retained glyph preparation
	$(CARGO) test --release $(CARGO_FLAGS) renderer::gpu_tests::retained_glyphs_match_fresh_preparation_after_atlas_churn -- --ignored --exact --nocapture --test-threads=1

probe-uploads: ## Opt-in GPU probe: failed-upload cleanup and recovery
	$(CARGO) test --release $(CARGO_FLAGS) renderer::gpu_tests::failed_frame_uploads_are_reclaimed_and_recover -- --ignored --exact --nocapture --test-threads=1

profile: ## macOS Instruments trace; PROFILE_TEMPLATE=, PROFILE_TIME=, PROFILE_OUT=, PROFILE_ARGS=
	CARGO_PROFILE_RELEASE_DEBUG=1 COMETTY_PERF=1 bash scripts/profile-macos.sh --template '$(PROFILE_TEMPLATE)' --time-limit $(PROFILE_TIME) --output $(PROFILE_OUT) -- $(PROFILE_ARGS)

release: ## Bump version, verify, commit, tag (RELEASE_BUMP=, RELEASE_VERSION=, RELEASE_PUSH=1)
	bash scripts/release.sh $(RELEASE_FLAGS)

bundle: ## Build macOS .app + zip; BUNDLE_ARCH=, BUNDLE_IDENTITY=, BUNDLE_NOTARY=
	bash scripts/bundle-macos.sh --arch $(BUNDLE_ARCH) $(if $(BUNDLE_IDENTITY),--identity '$(BUNDLE_IDENTITY)') $(if $(BUNDLE_NOTARY),--notary-profile '$(BUNDLE_NOTARY)')

deps: ## Inspect locked feature resolution for parser/GPU dependencies
	$(CARGO) tree $(CARGO_FLAGS) -e features -i vte
	$(CARGO) tree $(CARGO_FLAGS) -e features -i cosmic-text

clean: ## Remove Cargo build artifacts (including target/perf and traces)
	$(CARGO) clean
