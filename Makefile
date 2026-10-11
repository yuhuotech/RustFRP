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
	RUSTC_BOOTSTRAP=1 $(CARGO) llvm-cov --branch -p rustfrp-core --lcov --output-path target/cov/core.lcov
	python3 tools/cov/check_lcov.py target/cov/core.lcov --name rustfrp-core --list-gaps

# 全仓库
cov: core-cov
