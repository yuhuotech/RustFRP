# RustFRP 统一入口。覆盖率门槛见 docs/DEVELOPMENT_STANDARDS.md（行/函数/分支 ≥ 90%）。
export PATH := $(HOME)/.cargo/bin:$(PATH)
CARGO ?= cargo

.PHONY: fmt lint test core-test core-cov cov
fmt:
	$(CARGO) fmt --all
lint:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets -- -D warnings
test:
	$(CARGO) test --workspace

# 单个 crate 覆盖率：行/函数/分支均需 ≥ 90%（分支覆盖用 -Z coverage-options=branch，通过 RUSTC_BOOTSTRAP=1 在 stable 上启用，仅用于覆盖率任务）
core-test:
	$(CARGO) test -p rustfrp-core
core-cov:
	mkdir -p target/cov
	$(CARGO) llvm-cov clean --workspace
	RUSTC_BOOTSTRAP=1 $(CARGO) llvm-cov --branch -p rustfrp-core --lcov --output-path target/cov/core.lcov
	python3 tools/cov/check_lcov.py target/cov/core.lcov --name rustfrp-core --list-gaps

agent-cov:
	mkdir -p target/cov
	$(CARGO) llvm-cov clean --workspace
	RUSTC_BOOTSTRAP=1 $(CARGO) llvm-cov --branch -p rustfrp-agent --ignore-filename-regex 'rustfrp-agent/src/main.rs' --lcov --output-path target/cov/agent.lcov
	python3 tools/cov/check_lcov.py target/cov/agent.lcov --name rustfrp-agent --list-gaps

server-cov:
	mkdir -p target/cov
	$(CARGO) llvm-cov clean --workspace
	RUSTC_BOOTSTRAP=1 $(CARGO) llvm-cov --branch -p rustfrp-server --ignore-filename-regex 'rustfrp-server/src/main.rs' --lcov --output-path target/cov/server.lcov
	python3 tools/cov/check_lcov.py target/cov/server.lcov --name rustfrp-server --list-gaps

# 全仓库
cov: core-cov agent-cov server-cov app-cov

.PHONY: cov-baseline bench-mem
# 新增模块导致比例变化时，在同一提交里重设基线（需在提交信息中说明原因）。先运行 make core-cov agent-cov server-cov 生成 lcov。
cov-baseline:
	python3 tools/cov/check_lcov.py target/cov/core.lcov --name rustfrp-core --update-baseline --rebaseline --min 90
	python3 tools/cov/check_lcov.py target/cov/agent.lcov --name rustfrp-agent --update-baseline --rebaseline --min 90
	python3 tools/cov/check_lcov.py target/cov/server.lcov --name rustfrp-server --update-baseline --rebaseline --min 90
	python3 tools/cov/check_lcov.py target/cov/app.lcov --name rustfrp-app --update-baseline --rebaseline --min 90

# 内存基准：先 cargo build --release；超预算则失败
bench-mem:
	cargo build --release -p rustfrp-agent
	python3 tools/bench/mem.py --tunnels 10 --conns 100 --max-edge-mb 10 --max-client-mb 10

# ───────── Flutter（rustfrp-app）─────────
FLUTTER ?= flutter
.PHONY: app-test app-cov web-build web-run e2e app-shots
app-test:
	cd rustfrp-app && $(FLUTTER) analyze && $(FLUTTER) test
# 行覆盖 ≥ 90%（Dart 没有可用的分支覆盖工具，见 docs/DEVELOPMENT_STANDARDS.md §1.3）
app-cov:
	cd rustfrp-app && $(FLUTTER) test --coverage
	mkdir -p target/cov && cp rustfrp-app/coverage/lcov.info target/cov/app.lcov
	python3 tools/cov/check_lcov.py target/cov/app.lcov --name rustfrp-app --list-gaps
# 面板 Web 构建：必须 --no-web-resources-cdn（CSP 只允许同源脚本，国内也访问不了 gstatic）
web-build:
	cd rustfrp-app && $(FLUTTER) build web --no-web-resources-cdn
# 本机起 Controller 并托管面板：先 make web-build，再 RUSTFRP_ADMIN_PASSWORD=... target/release/rustfrp-server admin create root
web-run:
	RUSTFRP_PANEL_DIR=$(PWD)/rustfrp-app/build/web cargo run --release -p rustfrp-server -- run
# 全栈冒烟：真实 server + agent + REST
e2e:
	cargo build --release
	python3 tools/e2e/full_stack.py
# 评审用截图（真实字体）：make app-shots DIR=/tmp/shots
app-shots:
	cd rustfrp-app && SCREENSHOTS=$(or $(DIR),/tmp/rustfrp-shots) $(FLUTTER) test test/screenshots_test.dart

.PHONY: installer-test
installer-test:
	python3 tools/installer/test_install.py

# ───────── 原生共享管理 UI / 系统 WebView（迁移期间并行保留旧入口）─────────
NPM ?= npm
.PHONY: ui-deps ui-test ui-cov ui-build ui-e2e desktop-test desktop-lint desktop-build desktop-e2e
ui-deps:
	cd rustfrp-ui && $(NPM) ci
ui-test:
	cd rustfrp-ui && $(NPM) run check && $(NPM) test
ui-cov:
	cd rustfrp-ui && $(NPM) run coverage
ui-build:
	cd rustfrp-ui && $(NPM) run check && $(NPM) run build && $(NPM) run bench
# 使用 ./start_dev.sh 在固定 9674 启动隔离测试数据库后执行；需要 release Agent。
ui-e2e:
	$(CARGO) build --release -p rustfrp-agent
	cd rustfrp-ui && $(NPM) run e2e
desktop-test:
	$(CARGO) test --manifest-path rustfrp-desktop/Cargo.toml --workspace
desktop-lint:
	$(CARGO) fmt --manifest-path rustfrp-desktop/Cargo.toml --all -- --check
	$(CARGO) clippy --manifest-path rustfrp-desktop/Cargo.toml --all-targets -- -D warnings
desktop-build: ui-build
	cd rustfrp-desktop && ../rustfrp-ui/node_modules/.bin/tauri build
desktop-e2e:
	$(CARGO) build --release -p rustfrp-agent
	$(CARGO) build --manifest-path rustfrp-desktop/Cargo.toml
	$(CARGO) build --manifest-path rustfrp-desktop/Cargo.toml -p rustfrp-desktop-support --example local_probe
	python3 tools/e2e/native_local.py
