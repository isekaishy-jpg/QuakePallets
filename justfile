# justfile — Pallet workspace convenience commands
# (This is a contract; commands assume a future Cargo workspace exists.)

set dotenv-load := true

default:
  @just --list

# --- Quality gates (must be green before merge) ---
fmt:
  cargo fmt --check

clippy:
  cargo clippy --all-targets --all-features -- -D warnings

test:
  cargo test --all-features

deny:
  cargo deny check

ci: fmt clippy test deny

# --- Build/run ---
build:
  cargo build --all-features

package-dev *args:
  cargo run -p tools --all-features -- content build {{args}}

run *args:
  cargo run -p pallet --all-features -- {{args}}

run-release *args:
  cargo run -p pallet --release --all-features -- {{args}}

# --- Smoke tests ---
# "No-assets" smoke test: runs on synthetic data and is CI-friendly.
smoke *args:
  cargo run -p tools --all-features -- smoke --mode no-assets {{args}}

# Quake harness smoke test: requires a user-supplied Quake install directory.
# Example:
#   just smoke-quake "C:\Games\Quake" e1m1
smoke-quake quake_dir map="e1m1":
  cargo run -p tools --all-features -- smoke --mode quake --quake-dir "{{quake_dir}}" --map "{{map}}"

# --- Tools ---
pak-list quake_dir:
  cargo run -p tools --all-features -- pak list --quake-dir "{{quake_dir}}"

pak-extract quake_dir out_dir:
  cargo run -p tools --all-features -- pak extract --quake-dir "{{quake_dir}}" --out "{{out_dir}}"

# --- Fuzzing (nightly + cargo-fuzz required) ---
fuzz-install:
  cargo install cargo-fuzz --locked

fuzz-pak:
  cd compat_quake && cargo fuzz run fuzz_pak -- -max_total_time=60

fuzz-bsp:
  cd compat_quake && cargo fuzz run fuzz_bsp -- -max_total_time=60

fuzz: fuzz-pak fuzz-bsp
