# extractous dev tasks.

core := "extractous-core"

# List all recipes with their descriptions.
default:
    @just --list

# Run every gate in order: format check, lint, tests.
check: fmt-check lint test

# Build the Rust core; the first build compiles Tika into a native library.
build:
    cargo build --manifest-path {{core}}/Cargo.toml

# Run the test suite with nextest, then the doc tests nextest cannot run.
test *args:
    cargo nextest run --manifest-path {{core}}/Cargo.toml {{args}}
    cargo test --manifest-path {{core}}/Cargo.toml --doc

# Lint every target; every warning fails.
lint:
    cargo clippy --manifest-path {{core}}/Cargo.toml --all-targets -- -D warnings

# Format every Rust source in place.
fmt:
    cargo fmt --manifest-path {{core}}/Cargo.toml

# Fail if any Rust source is not formatted.
fmt-check:
    cargo fmt --manifest-path {{core}}/Cargo.toml --check

# Run the criterion benchmarks.
bench *args:
    cargo bench --manifest-path {{core}}/Cargo.toml --bench extractor {{args}}

# Run Tika under the GraalVM tracing agent to regenerate native-image metadata.
tika-agent:
    cd {{core}}/tika-native && ./gradlew --no-daemon runAgent
