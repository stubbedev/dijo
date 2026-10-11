# extractous dev tasks.

set windows-shell := ["pwsh.exe", "-NoLogo", "-NoProfile", "-Command"]

core := "extractous-core"

# List all recipes with their descriptions.
default:
    @just --list

# Run every gate in order: format check, lint, tests.
check: fmt-check lint test

# Build every crate; the first build compiles Tika into a native library.
build:
    cargo build --workspace

# Run the test suite with nextest, then the doc tests nextest cannot run.
test *args:
    cargo nextest run --workspace {{args}}
    cargo test --workspace --doc

# Lint every target; every warning fails.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Format every Rust source in place.
fmt:
    cargo fmt --all

# Fail if any Rust source is not formatted.
fmt-check:
    cargo fmt --all --check

# Run the criterion benchmarks.
bench *args:
    cargo bench --package extractous-core --bench extractor {{args}}

# Run Tika under the GraalVM tracing agent to regenerate native-image metadata.
tika-agent:
    cd {{core}}/tika-native && ./gradlew --no-daemon runAgent
