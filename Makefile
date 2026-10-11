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

# 全仓库
cov: core-cov agent-cov

.PHONY: cov-baseline bench-mem
# 新增模块导致比例变化时，在同一提交里重设基线（需在提交信息中说明原因）。
cov-baseline: cov
	python3 tools/cov/check_lcov.py target/cov/core.lcov --name rustfrp-core --update-baseline --min 90
	python3 tools/cov/check_lcov.py target/cov/agent.lcov --name rustfrp-agent --update-baseline --min 90

# 内存基准：先 cargo build --release；超预算则失败
bench-mem:
	cargo build --release -p rustfrp-agent
	python3 tools/bench/mem.py --tunnels 10 --conns 100 --max-edge-mb 10 --max-client-mb 10
