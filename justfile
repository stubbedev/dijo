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

# Assemble a statically linked tesseract, its language data and their licenses, for a build that
# carries its own OCR: EXTRACTOUS_OCR_BUNDLE=<dir> cargo build --release.
[unix]
ocr-bundle dir="target/ocr-bundle":
    ci/ocr-bundle.sh {{dir}}

# Regenerate the answers of Apache Tika's own CharSoup language detector that the Rust port is tested against, after
# checking that the embedded model is the one in the pinned Tika version's jar.
[unix]
charsoup-reference:
    #!/usr/bin/env bash
    set -euo pipefail
    jars=target/charsoup
    ci/fetch-charsoup.sh "$jars"
    model=org/apache/tika/langdetect/charsoup/langdetect-20260320.bin
    unzip -p "$jars"/tika-langdetect-charsoup-core-*.jar "$model" | cmp - "extractous-cli/models/$(basename "$model")"
    fixtures=extractous-cli/tests/fixtures/language
    java -cp "$jars/*" "$fixtures/CharSoupReference.java" "$fixtures" quality/truth "$fixtures/charsoup-reference.tsv"

# Build the statically linked Linux executable target/static-image/extractous: the program linked into
# Tika's GraalVM native image, with musl, needing nothing at run time.
[linux]
static:
    ci/static-image.sh target/static-image/extractous

# Run the server under load for SOAK_SECONDS (120 by default) per test with a release build:
# answers stay alike and memory level over a long run, and damaged documents are answered while
# worker processes recover; SOAK_SEED repeats a run's damage.
test-soak:
    cargo nextest run --package extractous --cargo-profile release --run-ignored only --test soak --no-capture

# Fuzz the parsers of untrusted input (XHTML splitter, normaliser, query decoder, metadata) with
# coverage guidance for the given seconds each; a failing input is kept under
# extractous-cli/fuzz/artifacts.
[unix]
fuzz seconds="300":
    #!/usr/bin/env bash
    set -euo pipefail
    cd extractous-cli
    for target in $(cargo fuzz list); do
        cargo fuzz run --sanitizer none "$target" -- -max_total_time={{seconds}} -rss_limit_mb=2048
    done

# Measure the CLI's extraction throughput per format, peak memory and cold start with a release
# build; BENCH_OUTPUT writes the Markdown table to a file, BENCH_ROUNDS sets the repetitions.
throughput:
    cargo nextest run --package extractous --cargo-profile release --run-ignored only --test bench --no-capture

# Compare native extraction of Apache Tika's own test documents with Tika on the JVM, fail on
# any regression and write conformance/MATRIX.md; the corpus and tika-app are cached in target/.
[unix]
conformance:
    #!/usr/bin/env bash
    set -euo pipefail
    eval "$(conformance/prepare.sh {{justfile_directory()}}/target/conformance)"
    cargo nextest run --package extractous --cargo-profile release --run-ignored only --test conformance

# Score Tika on the JVM, Kreuzberg and Docling (in Docker) against the quality corpus too, each
# with its defaults, and write quality/COMPARISON.md with all their scores side by side.
[unix]
quality-compare:
    #!/usr/bin/env bash
    set -euo pipefail
    dir={{justfile_directory()}}/target/quality-compare
    version=$(extractous-core/tika-native/tika-version.sh)
    rm -rf "$dir/tools"
    mkdir -p "$dir/tools"
    conformance/tika-app.sh "$dir" quality/corpus "$dir/tools/Tika $version" > "$dir/tika.log" 2>&1
    quality/compare-python.sh "$dir/cache" "$dir/tools" > "$dir/python.log" 2>&1
    QUALITY_COMPARE="$dir/tools" QUALITY_COMPARISON={{justfile_directory()}}/quality/COMPARISON.md \
        cargo nextest run --package extractous --test quality

# Load tika-server of the bundled Tika version and `extractous serve` alike on this machine, and
# write their throughput, latency and memory to target/server-compare/comparison.md.
[unix]
server-compare:
    #!/usr/bin/env bash
    set -euo pipefail
    dir={{justfile_directory()}}/target/server-compare
    server=$(conformance/fetch-tika.sh "$dir" server-standard)
    version=$(extractous-core/tika-native/tika-version.sh)
    port=19998
    java -jar "$server/tika-server-standard-$version.jar" -p "$port" > "$dir/tika-server.log" 2>&1 &
    pid=$!
    trap 'kill "$pid"' EXIT
    until curl --silent --fail --output /dev/null "http://127.0.0.1:$port/tika"; do
        kill -0 "$pid"
        sleep 1
    done
    TIKA_SERVER_ADDRESS="127.0.0.1:$port" TIKA_SERVER_PID="$pid" SERVER_COMPARISON="$dir/comparison.md" \
        cargo nextest run --package extractous --cargo-profile release --run-ignored only --test server_compare --no-capture

# Accept the current conformance results as conformance/baseline.json.
[unix]
conformance-bless:
    #!/usr/bin/env bash
    set -euo pipefail
    eval "$(conformance/prepare.sh {{justfile_directory()}}/target/conformance)"
    CONFORMANCE_BLESS=1 cargo nextest run --package extractous --cargo-profile release --run-ignored only --test conformance

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
    nix shell nixpkgs#libreoffice nixpkgs#pandoc nixpkgs#poppler-utils nixpkgs#imagemagick nixpkgs#exiftool nixpkgs#python3 nixpkgs#texliveSmall nixpkgs#djvulibre nixpkgs#jbig2enc \
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

# Parse every file under the given paths on the JVM under GraalVM's tracing agent, and merge
# what it records into the native-image metadata of every OS for the pinned Tika version.
[unix]
tika-agent +inputs="test_files":
    #!/usr/bin/env bash
    set -euo pipefail
    inputs=()
    for input in {{inputs}}; do
        inputs+=("$(realpath "$input")")
    done
    cd {{core}}/tika-native
    version="$(./tika-version.sh)"
    rm -rf graal-agent
    ./gradlew --no-daemon runAgent --args="${inputs[*]}"
    for os in linux macos windows; do
        metadata="src/main/resources/META-INF/ai.yobix/tika-$version-$os"
        merged="$(mktemp -d)"
        native-image-utils generate --input-dir="$metadata" --input-dir=graal-agent \
            --output-dir="$merged"
        rm -rf "$metadata"
        mv "$merged" "$metadata"
    done
    rm -rf graal-agent

# Regenerate THIRD-PARTY-LICENSES.md from the Rust crates, models and Java libraries in the binary.
[unix]
licenses:
    cd {{core}}/tika-native && ./gradlew --quiet generateLicenseReport
    cargo about generate --workspace --fail --output-file target/rust-licenses.md about.hbs
    cat target/rust-licenses.md extractous-cli/models/LICENSES.md {{core}}/tika-native/build/licenses/java.md \
        > target/THIRD-PARTY-LICENSES.md
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
