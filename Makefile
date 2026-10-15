# RustFRP 统一入口：Rust、共享 DOM UI 和系统 WebView 桌面。
export PATH := $(HOME)/.cargo/bin:$(PATH)
CARGO ?= cargo
NPM ?= npm
PYTHON ?= python3

ifeq ($(OS),Windows_NT)
TAURI_LOCAL_BUILD_ARGS := --no-bundle
else ifeq ($(shell uname -s),Darwin)
TAURI_LOCAL_BUILD_ARGS := --bundles app
else
TAURI_LOCAL_BUILD_ARGS := --no-bundle
endif

.PHONY: fmt lint test rust-lint rust-test core-test core-cov agent-cov server-cov assets-cov cov
fmt:
	$(CARGO) fmt --all
	$(CARGO) fmt --manifest-path rustfrp-desktop/Cargo.toml --all
lint: rust-lint desktop-lint ui-check
rust-lint:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings
test: rust-test desktop-test ui-test
rust-test:
	$(CARGO) test --workspace --all-features

# 单个 crate 覆盖率：行/函数/分支均需 ≥ 90%（分支覆盖用 -Z coverage-options=branch，通过 RUSTC_BOOTSTRAP=1 在 stable 上启用，仅用于覆盖率任务）
core-test:
	$(CARGO) test -p rustfrp-core
core-cov:
	mkdir -p target/cov
	CARGO_TARGET_DIR=$(PWD)/target/cov-core $(CARGO) llvm-cov clean --profraw-only
	CARGO_TARGET_DIR=$(PWD)/target/cov-core RUSTC_BOOTSTRAP=1 $(CARGO) llvm-cov --branch -p rustfrp-core --lcov --output-path target/cov/core.lcov
	$(PYTHON) tools/cov/check_lcov.py target/cov/core.lcov --name rustfrp-core --require-all --list-gaps

agent-cov:
	mkdir -p target/cov
	CARGO_TARGET_DIR=$(PWD)/target/cov-agent $(CARGO) llvm-cov clean --profraw-only
	CARGO_TARGET_DIR=$(PWD)/target/cov-agent RUSTC_BOOTSTRAP=1 $(CARGO) llvm-cov --branch -p rustfrp-agent --ignore-filename-regex 'rustfrp-agent/src/main.rs' --lcov --output-path target/cov/agent.lcov
	$(PYTHON) tools/cov/check_lcov.py target/cov/agent.lcov --name rustfrp-agent --require-all --list-gaps

server-cov: ui-build
	mkdir -p target/cov
	CARGO_TARGET_DIR=$(PWD)/target/cov-server $(CARGO) llvm-cov clean --profraw-only
	CARGO_TARGET_DIR=$(PWD)/target/cov-server RUSTC_BOOTSTRAP=1 $(CARGO) llvm-cov --branch -p rustfrp-server --all-features --ignore-filename-regex 'rustfrp-server/src/main.rs' --lcov --output-path target/cov/server.lcov
	$(PYTHON) tools/cov/check_lcov.py target/cov/server.lcov --name rustfrp-server --require-all --list-gaps

assets-cov: ui-build
	mkdir -p target/cov
	CARGO_TARGET_DIR=$(PWD)/target/cov-assets $(CARGO) llvm-cov clean --profraw-only
	CARGO_TARGET_DIR=$(PWD)/target/cov-assets RUSTC_BOOTSTRAP=1 $(CARGO) llvm-cov --branch -p rustfrp-ui-assets --all-features --lcov --output-path target/cov/assets.lcov
	$(PYTHON) tools/cov/check_lcov.py target/cov/assets.lcov --name rustfrp-ui-assets --require-all --list-gaps

# 全仓库
cov: core-cov agent-cov server-cov assets-cov desktop-cov ui-cov

.PHONY: cov-baseline bench-mem
# 新增模块导致比例变化时，在同一提交里重设基线（需在提交信息中说明原因）。先运行 make core-cov agent-cov server-cov 生成 lcov。
cov-baseline:
	$(PYTHON) tools/cov/check_lcov.py target/cov/core.lcov --name rustfrp-core --require-all --update-baseline --rebaseline --min 90
	$(PYTHON) tools/cov/check_lcov.py target/cov/agent.lcov --name rustfrp-agent --require-all --update-baseline --rebaseline --min 90
	$(PYTHON) tools/cov/check_lcov.py target/cov/server.lcov --name rustfrp-server --require-all --update-baseline --rebaseline --min 90
	$(PYTHON) tools/cov/check_lcov.py target/cov/assets.lcov --name rustfrp-ui-assets --require-all --update-baseline --rebaseline --min 90
	$(PYTHON) tools/cov/check_lcov.py target/cov/desktop.lcov --name rustfrp-desktop --require-all --update-baseline --rebaseline --min 90
	$(PYTHON) tools/cov/check_lcov.py target/cov/desktop-support.lcov --name rustfrp-desktop-support --require-all --update-baseline --rebaseline --min 90
	$(PYTHON) tools/cov/check_lcov.py target/cov/ui.lcov --name rustfrp-ui --require-all --update-baseline --rebaseline --min 90

# 内存基准：先 cargo build --release；超预算则失败
bench-mem:
	cargo build --release -p rustfrp-agent
	$(PYTHON) tools/bench/mem.py --tunnels 10 --conns 100 --max-edge-mb 10 --max-client-mb 10

.PHONY: web-build web-run e2e ui-shots
web-build: ui-build
web-run: web-build
	RUSTFRP_PANEL_DIR=$(PWD)/rustfrp-ui/dist $(CARGO) run --release -p rustfrp-server --features embedded-panel -- run
# 全栈冒烟：真实 server + agent + REST
e2e:
	$(CARGO) build --release
	$(PYTHON) tools/e2e/full_stack.py
# 评审用截图（真实字体）：make ui-shots DIR=/tmp/shots
ui-shots:
	$(PYTHON) tools/e2e/run_ui.py -- node rustfrp-ui/node_modules/@playwright/test/cli.js test --config rustfrp-ui/playwright.config.ts --grep 'system fonts'

.PHONY: installer-test
installer-test:
	$(PYTHON) tools/installer/test_install.py

# ───────── 原生共享管理 UI / 系统 WebView ─────────
.PHONY: ui-deps ui-check ui-test ui-cov ui-build ui-e2e desktop-test desktop-lint desktop-build desktop-e2e desktop-cov
ui-deps:
	cd rustfrp-ui && $(NPM) ci
ui-check:
	cd rustfrp-ui && $(NPM) run check
ui-test: ui-check
	cd rustfrp-ui && $(NPM) test && $(NPM) run build-test
ui-cov:
	cd rustfrp-ui && $(NPM) run coverage
	mkdir -p target/cov
	cp rustfrp-ui/coverage/lcov.info target/cov/ui.lcov
	$(PYTHON) tools/cov/check_lcov.py target/cov/ui.lcov --name rustfrp-ui --require-all --list-gaps
ui-build:
	cd rustfrp-ui && $(NPM) run check && $(NPM) run build && $(NPM) run bench
# 自动启动独立 Controller；固定 9674。端口占用时拒绝接管，可显式指定使用现有隔离实例。
ui-e2e: ui-build
	$(CARGO) build --release -p rustfrp-agent -p rustfrp-server --features rustfrp-server/embedded-panel
	$(PYTHON) tools/e2e/run_ui.py -- node rustfrp-ui/node_modules/@playwright/test/cli.js test --config rustfrp-ui/playwright.config.ts
desktop-test:
	$(CARGO) test --manifest-path rustfrp-desktop/Cargo.toml --workspace --all-features
desktop-lint:
	$(CARGO) fmt --manifest-path rustfrp-desktop/Cargo.toml --all -- --check
	$(CARGO) clippy --manifest-path rustfrp-desktop/Cargo.toml --workspace --all-targets --all-features -- -D warnings
desktop-build: ui-build
	cd rustfrp-desktop && ../rustfrp-ui/node_modules/.bin/tauri build $(TAURI_LOCAL_BUILD_ARGS)
ifneq ($(OS),Windows_NT)
ifeq ($(shell uname -s),Darwin)
# 本地开发包补齐资源签名；正式 Release 使用独立签名流程。
	codesign --force --deep --sign - --identifier com.rustfrp.desktop rustfrp-desktop/target/release/bundle/macos/RustFRP.app
	codesign --verify --deep --strict rustfrp-desktop/target/release/bundle/macos/RustFRP.app
endif
endif
desktop-e2e:
	$(CARGO) build --release -p rustfrp-agent -p rustfrp-server --features rustfrp-server/embedded-panel
	$(CARGO) build --manifest-path rustfrp-desktop/Cargo.toml
	$(CARGO) build --manifest-path rustfrp-desktop/Cargo.toml -p rustfrp-desktop-support --example local_probe
	$(PYTHON) tools/e2e/run_ui.py -- $(PYTHON) tools/e2e/native_local.py
desktop-cov:
	$(PYTHON) tools/cov/desktop.py
