# dijo dev tasks.

set windows-shell := ["pwsh.exe", "-NoLogo", "-NoProfile", "-Command"]

core := "dijo-core"

# List all recipes with their descriptions.
default:
    @just --list

# Run every gate in order: format check, lint, tests.
check: fmt-check lint test

# Build every crate.
build:
    cargo build --workspace

# Run the test suite with nextest, then the doc tests nextest cannot run.
test *args:
    cargo nextest run --workspace {{args}}
    cargo test --workspace --doc

# Extract generated gigabyte inputs with a release build and check memory stays bounded.
test-large:
    cargo nextest run --package dijo --cargo-profile release --run-ignored only --test large

# Extract randomly damaged copies of the test documents with a release build and check none
# crashes or hangs; FUZZ_SEED repeats a run, FUZZ_MUTANTS sets the copies per document.
test-fuzz:
    cargo nextest run --package dijo --cargo-profile release --run-ignored only --test fuzz

# Assemble the tesseract library a build loads into its process (built with vcpkg, ci/ocr-vcpkg.sh), its
# language data and their licenses, for a build that carries its own OCR:
# DIJO_OCR_BUNDLE=<dir> cargo build --release.
[unix]
ocr-bundle dir="target/ocr-bundle":
    ci/ocr-bundle.sh {{dir}}

# Regenerate the encrypted Word documents and workbooks of dijo-office/tests/fixtures/crypt:
# LibreOffice writes the plain ones, and encrypt.py encrypts them with XOR obfuscation and RC4
# CryptoAPI, checked against what msoffcrypto-tool decrypts. Needs soffice and python3.
crypt-fixtures:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v soffice >/dev/null || { echo "crypt-fixtures needs LibreOffice's soffice" >&2; exit 1; }
    fixtures=dijo-office/tests/fixtures/crypt
    work=target/crypt-fixtures
    rm -rf "$work"
    mkdir -p "$work/source"
    python3 -m venv "$work/venv"
    "$work/venv/bin/pip" install --quiet msoffcrypto-tool==6.0.0 olefile==0.47
    printf 'Fortrolig tekst i et krypteret dokument.\nAndet afsnit med æøå.\n' > "$work/source/plain.txt"
    printf 'Navn,Tal\nKrypteret,42\nÆble,7\n' > "$work/source/plain.csv"
    profile="file://$(realpath "$work")/profile"
    soffice -env:UserInstallation="$profile" --headless --convert-to 'doc:MS Word 97' \
        --outdir "$fixtures" "$work/source/plain.txt"
    soffice -env:UserInstallation="$profile" --headless \
        --infilter='Text - txt - csv (StarCalc):44,34,76' --convert-to 'xls:MS Excel 97' \
        --outdir "$fixtures" "$work/source/plain.csv"
    "$work/venv/bin/python" -I "$fixtures/encrypt.py" "$fixtures/plain.doc" "$fixtures/plain.xls" "$fixtures"

# Regenerate the static media type tables (dijo-mime/src/mime/mime_types/tables.rs) from tika-mimetypes.xml
# and custom-mimetypes.xml.
mime-tables:
    MIME_TABLES_BLESS=1 cargo nextest run --package dijo-mime --lib the_tables_are_generated_from_the_xml

# The image is the one `nix build .#container` builds (ci/container.nix).
# Build the container image dijo:local around target/static/dijo and load it into Docker.
[linux]
container:
    docker load --input "$(ci/container.sh target/static/dijo)"

# Retrain the models that tell closely related languages apart (dijo-base/models/language-groups.bin) from the
# Wikipedia shards pinned in dijo-base/models/language-groups.tsv, fetched into target/wikipedia (about 7 GB).
[unix]
language-groups:
    #!/usr/bin/env bash
    set -euo pipefail
    sources=dijo-base/models/language-groups.tsv
    ci/fetch-wikipedia.sh "$sources" target/wikipedia
    python=$(nix build --no-link --print-out-paths --impure --expr \
        "((import $PWD/ci/nixpkgs.nix).legacyPackages.\${builtins.currentSystem}.python3.withPackages (ps: [ ps.pyarrow ]))")
    "$python/bin/python3" -I dijo-base/models/language-groups.py "$sources" target/wikipedia target/language-groups
    LANGUAGE_GROUPS_SENTENCES="$PWD/target/language-groups" cargo test --release --package dijo-base --lib \
        language::groups::tests::train -- --ignored

# Compare language detection with Tika's CharSoup per language on a TSV of the expected ISO 639-3 code and a text per
# line, and print the shares each detects right as a Markdown table.
language-accuracy file:
    LANGUAGE_EVALUATION={{absolute_path(file)}} cargo test --release --package dijo-base --lib \
        language::tests::accuracy_against_tika -- --ignored --nocapture

# The program alone, with musl, tesseract and libheif linked in, needing nothing at run time: the flake's
# static output, built with Crane.
# Build the statically linked Linux executable as released into target/static/dijo.
[linux]
static-native:
    #!/usr/bin/env bash
    set -euo pipefail
    built=$(nix build --no-link --print-out-paths .#static)
    mkdir -p target/static
    install -m 755 "$built/bin/dijo" target/static/dijo.partial
    mv target/static/dijo.partial target/static/dijo

# Run the server under load for SOAK_SECONDS (120 by default) per test with a release build:
# answers stay alike and memory level over a long run, and damaged documents are answered while
# worker processes recover; SOAK_SEED repeats a run's damage.
test-soak:
    cargo nextest run --package dijo --cargo-profile release --run-ignored only --test soak --no-capture

# Fuzz the parsers of untrusted input (XHTML splitter, normaliser, query decoder, metadata) with
# coverage guidance for the given seconds each; a failing input is kept under
# dijo-cli/fuzz/artifacts.
[unix]
fuzz seconds="300":
    #!/usr/bin/env bash
    set -euo pipefail
    cd dijo-cli
    for target in $(cargo fuzz list); do
        cargo fuzz run --sanitizer none "$target" -- -max_total_time={{seconds}} -rss_limit_mb=2048
    done

# Measure the CLI's extraction throughput per format, peak memory and cold start with a release
# build; BENCH_OUTPUT writes the Markdown table to a file, BENCH_ROUNDS sets the repetitions.
throughput:
    cargo nextest run --package dijo --cargo-profile release --run-ignored only --test bench --no-capture

# The corpus and tika-app are cached in target/.
# Compare native extraction of Apache Tika's own test documents with Tika on the JVM, fail on
# any regression and write conformance/MATRIX.md.
[unix]
conformance:
    #!/usr/bin/env bash
    set -euo pipefail
    eval "$(conformance/prepare.sh {{justfile_directory()}}/target/conformance)"
    cargo nextest run --package dijo --cargo-profile release --run-ignored only --test conformance

# Score Tika on the JVM, Kreuzberg and Docling (in Docker) against the quality corpus too, each
# with its defaults, and write quality/COMPARISON.md with all their scores side by side.
[unix]
quality-compare:
    #!/usr/bin/env bash
    set -euo pipefail
    dir={{justfile_directory()}}/target/quality-compare
    version=$(conformance/tika-version.sh)
    rm -rf "$dir/tools"
    mkdir -p "$dir/tools"
    conformance/tika-app.sh "$dir" quality/corpus "$dir/tools/Tika $version" > "$dir/tika.log" 2>&1
    quality/compare-python.sh "$dir/cache" "$dir/tools" > "$dir/python.log" 2>&1
    QUALITY_COMPARE="$dir/tools" QUALITY_COMPARISON={{justfile_directory()}}/quality/COMPARISON.md \
        cargo nextest run --package dijo --test quality

# Load tika-server of the pinned Tika version and `dijo serve` alike on this machine, and
# write their throughput, latency and memory to target/server-compare/comparison.md;
# SERVER_COMPARE_BINARY serves another build, such as target/static/dijo.
[unix]
server-compare:
    #!/usr/bin/env bash
    set -euo pipefail
    dir={{justfile_directory()}}/target/server-compare
    server=$(conformance/fetch-tika.sh "$dir" server-standard)
    version=$(conformance/tika-version.sh)
    port=19998
    java -jar "$server/tika-server-standard-$version.jar" -p "$port" > "$dir/tika-server.log" 2>&1 &
    pid=$!
    trap 'kill "$pid"' EXIT
    until curl --silent --fail --output /dev/null "http://127.0.0.1:$port/tika"; do
        kill -0 "$pid"
        sleep 1
    done
    TIKA_SERVER_ADDRESS="127.0.0.1:$port" TIKA_SERVER_PID="$pid" SERVER_COMPARISON="$dir/comparison.md" \
        cargo nextest run --package dijo --cargo-profile release --run-ignored only --test server_compare --no-capture

# Accept the current conformance results as conformance/baseline.json.
[unix]
conformance-bless:
    #!/usr/bin/env bash
    set -euo pipefail
    eval "$(conformance/prepare.sh {{justfile_directory()}}/target/conformance)"
    CONFORMANCE_BLESS=1 cargo nextest run --package dijo --cargo-profile release --run-ignored only \
        --test conformance

# Score extraction against the quality corpus and write quality/SCOREBOARD.md.
[unix]
quality:
    QUALITY_SCOREBOARD={{justfile_directory()}}/quality/SCOREBOARD.md cargo nextest run --package dijo --test quality

# Accept the current quality scores as the baseline that CI holds later changes to.
[unix]
quality-bless:
    QUALITY_BLESS=1 QUALITY_SCOREBOARD={{justfile_directory()}}/quality/SCOREBOARD.md cargo nextest run --package dijo --test quality

# Regenerate the quality corpus from quality/truth with LibreOffice, pandoc, LaTeX, poppler, ImageMagick and exiftool.
[unix]
quality-corpus:
    #!/usr/bin/env bash
    set -euo pipefail
    fonts=$(nix build --no-link --print-out-paths nixpkgs#noto-fonts nixpkgs#noto-fonts-cjk-sans)
    # shellcheck disable=SC2086
    nix shell nixpkgs#libreoffice nixpkgs#pandoc nixpkgs#poppler-utils nixpkgs#imagemagick nixpkgs#exiftool nixpkgs#python3 nixpkgs#texliveSmall nixpkgs#djvulibre nixpkgs#jbig2enc nixpkgs#openjpeg \
        --command quality/generate.sh $fonts

# Lint every target; warnings fail.
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
    cargo bench --package dijo-core --bench extractor {{args}}

# Regenerate THIRD-PARTY-LICENSES.md from the Rust crates and models in the binary.
[unix]
licenses:
    mkdir -p target
    cargo about generate --workspace --fail --output-file target/rust-licenses.md about.hbs
    cat target/rust-licenses.md dijo-base/models/LICENSES.md > target/THIRD-PARTY-LICENSES.md
    mv target/THIRD-PARTY-LICENSES.md THIRD-PARTY-LICENSES.md

# Write the CycloneDX SBOM of the Rust crates in the binary to target/sbom.
[unix]
sbom:
    cargo cyclonedx --format json --spec-version 1.5 --no-build-deps --describe binaries --target all
    mkdir -p target/sbom
    mv dijo-cli/dijo_bin.cdx.json target/sbom/dijo-rust.cdx.json

# Check dependencies for advisories, licences and bans, and that THIRD-PARTY-LICENSES.md is current.
[unix]
deps: licenses
    cargo deny check
    git diff --exit-code THIRD-PARTY-LICENSES.md
