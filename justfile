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

# Extract generated gigabyte inputs with a release build and check memory stays bounded.
test-large:
    cargo nextest run --package extractous --cargo-profile release --run-ignored only --test large

# Extract randomly damaged copies of the test documents with a release build and check none
# crashes or hangs; FUZZ_SEED repeats a run, FUZZ_MUTANTS sets the copies per document.
test-fuzz:
    cargo nextest run --package extractous --cargo-profile release --run-ignored only --test fuzz

# Score extraction against the quality corpus and write quality/SCOREBOARD.md.
[unix]
quality:
    QUALITY_SCOREBOARD={{justfile_directory()}}/quality/SCOREBOARD.md cargo nextest run --package extractous --test quality

# Accept the current quality scores as the baseline that CI holds later changes to.
[unix]
quality-bless:
    QUALITY_BLESS=1 QUALITY_SCOREBOARD={{justfile_directory()}}/quality/SCOREBOARD.md cargo nextest run --package extractous --test quality

# Regenerate the quality corpus from quality/truth with LibreOffice, pandoc, LaTeX, poppler, ImageMagick and exiftool.
[unix]
quality-corpus:
    #!/usr/bin/env bash
    set -euo pipefail
    fonts=$(nix build --no-link --print-out-paths nixpkgs#noto-fonts nixpkgs#noto-fonts-cjk-sans)
    # shellcheck disable=SC2086
    nix shell nixpkgs#libreoffice nixpkgs#pandoc nixpkgs#poppler-utils nixpkgs#imagemagick nixpkgs#exiftool nixpkgs#python3 nixpkgs#texliveSmall \
        --command quality/generate.sh $fonts

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

# Record the SHA-256 of every Gradle dependency; run after changing build.gradle.
[unix]
gradle-verification:
    #!/usr/bin/env bash
    set -euo pipefail
    gradle_home="$(mktemp -d)"
    trap 'rm -rf "$gradle_home"' EXIT
    cd {{core}}/tika-native
    GRADLE_USER_HOME="$gradle_home" ./gradlew --no-daemon --quiet --write-verification-metadata sha256 \
        nativeCompile generateLicenseReport cyclonedxDirectBom checkLicense

# Run Tika under the GraalVM tracing agent to regenerate native-image metadata.
tika-agent:
    cd {{core}}/tika-native && ./gradlew --no-daemon runAgent

# Regenerate THIRD-PARTY-LICENSES.md from the Rust crates and Java libraries in the binary.
[unix]
licenses:
    cd {{core}}/tika-native && ./gradlew --quiet generateLicenseReport
    cargo about generate --workspace --fail --output-file target/rust-licenses.md about.hbs
    cat target/rust-licenses.md {{core}}/tika-native/build/licenses/java.md > target/THIRD-PARTY-LICENSES.md
    mv target/THIRD-PARTY-LICENSES.md THIRD-PARTY-LICENSES.md

# Write CycloneDX SBOMs of the Rust crates and Java libraries in the binary to target/sbom.
[unix]
sbom:
    cd {{core}}/tika-native && ./gradlew --quiet cyclonedxDirectBom
    cargo cyclonedx --format json --spec-version 1.5 --no-build-deps --describe binaries --target all
    mkdir -p target/sbom
    mv extractous-cli/extractous_bin.cdx.json target/sbom/extractous-rust.cdx.json
    cp {{core}}/tika-native/build/sbom/java.cdx.json target/sbom/extractous-java.cdx.json

# Check dependencies for advisories, licences and bans, and that THIRD-PARTY-LICENSES.md is current.
[unix]
deps: licenses
    cargo deny check
    cd {{core}}/tika-native && ./gradlew --quiet checkLicense
    git diff --exit-code THIRD-PARTY-LICENSES.md
