#!/usr/bin/env bash
# The checks to run before every commit, stopping at the first failure. GitHub Actions runs these too.
set -euo pipefail
cd "$(dirname "$0")/.."
# the tests that need the game's files find them through MH3U_GAME_DIR; without it they skip
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all
python3 scripts/package-release.py self-check
# the schema download needs network and the jsonschema package; CI always runs this
if python3 -c 'import jsonschema' >/dev/null 2>&1; then
    python3 scripts/check-recomplauncher.py
else
    echo "note: python jsonschema is not installed, so recomplauncher.json was not validated here" >&2
fi
# known vulnerabilities in the dependencies (https://rustsec.org); this runs when cargo-audit is installed (cargo install cargo-audit --locked)
if command -v cargo-audit >/dev/null 2>&1; then
    cargo audit
else
    echo "note: cargo-audit is not installed, so the dependency audit was skipped here" >&2
fi
