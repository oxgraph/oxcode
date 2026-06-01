set shell := ["bash", "-cu"]

default: ci

# --- formatting ---

fmt:
    cargo +nightly fmt --all
    taplo format

fmt-check:
    cargo +nightly fmt --all -- --check

fmt-toml:
    taplo format

fmt-toml-check:
    taplo format --check

# --- lint / test / deny ---

lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

lint-fix:
    cargo clippy --workspace --all-targets --all-features --fix --allow-dirty -- -D warnings

test:
    cargo test --workspace --all-features

test-default:
    cargo test --workspace --all-features

deny:
    cargo deny --all-features check advisories bans sources

# --- benches, miri, kani ---

bench:
    cargo bench --workspace --all-features

# `-Zmiri-disable-isolation` lets tests that touch the filesystem run.
miri:
    MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test --workspace

kani:
    cargo kani --workspace

# --- aggregate ---

# Fast gate — seconds-to-minutes. Runs in prek pre-commit and on every change.
ci: fmt-check fmt-toml-check lint deny test-default

# Heavy verification — miri for UB, kani for invariant proofs. Runs before
# major PRs, not per-commit.
verify: miri kani

# --- hooks ---

hooks-install:
    prek install --install-hooks
