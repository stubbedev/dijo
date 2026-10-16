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

# Assemble the tesseract library a build loads into its process (built with vcpkg, ci/ocr-vcpkg.sh), its
# language data and their licenses, for a build that carries its own OCR:
# EXTRACTOUS_OCR_BUNDLE=<dir> cargo build --release.
[unix]
ocr-bundle dir="target/ocr-bundle":
    ci/ocr-bundle.sh {{dir}}

# Regenerate the JDK charset tables and Apache Tika's own encoding detection answers that the Rust port is
# tested against, after checking that the embedded models are the ones in the pinned Tika version's jar.
[unix]
encoding-reference:
    #!/usr/bin/env bash
    set -euo pipefail
    jars=target/reference-jars
    ci/fetch-reference-jars.sh "$jars"
    for model in nb-bigram.bin utf16-specialist.bin; do
        unzip -p "$jars"/tika-encoding-detector-mojibuster-*.jar "org/apache/tika/ml/chardetect/$model" \
            | cmp - "extractous-core/models/$model"
    done
    eval "$(conformance/prepare.sh {{justfile_directory()}}/target/conformance)"
    fixtures=extractous-core/tests/fixtures/charset
    java "$fixtures/CharsetTables.java" extractous-core/models/java-charsets.bin "$fixtures/decoder-reference.tsv"
    rules=$(just native-image-rules)
    java -cp "$rules:$jars/*" "$fixtures/EncodingReference.java" quality/truth quality/corpus \
        "$CONFORMANCE_CORPUS" "$fixtures/encoding-reference.tsv.gz"

# Compile Tika's encoding rules with the change the native image substitutes into them, for the reference
# recipes to detect encodings as extractous does, and print the class directory to put ahead of Tika's jars.
[unix]
native-image-rules:
    #!/usr/bin/env bash
    set -euo pipefail
    jars=target/reference-jars
    ci/fetch-reference-jars.sh "$jars"
    rules=target/native-image-rules
    rm -rf "$rules"
    mkdir -p "$rules"
    java extractous-core/tests/fixtures/charset/NativeImageRules.java \
        "$jars/sources/tika-encoding-detector-mojibuster-4.1.0-sources.jar" "$jars/*" "$rules" >&2
    echo "$rules"

# Regenerate what Tika's XHTMLContentHandler and our handler chains write for the parser calls in
# extractous-core/tests/fixtures/handlers/scripts.txt, which the Rust handlers are tested against.
[unix]
handler-reference:
    #!/usr/bin/env bash
    set -euo pipefail
    jars=target/reference-jars
    ci/fetch-reference-jars.sh "$jars"
    fixtures=extractous-core/tests/fixtures/handlers
    sources=target/handler-reference/ai/yobix
    rm -rf "$sources"
    mkdir -p "$sources"
    for handler in AnchoredBlockHandler BlockBreakHandler WellFormedContentHandler; do
        cp "extractous-core/tika-native/src/main/java/ai/yobix/$handler.java" "$sources/"
    done
    cp "$fixtures/HandlerChainReference.java" "$sources/"
    java -cp "$jars/*" "$sources/HandlerChainReference.java" "$fixtures/scripts.txt" "$fixtures/chains.tsv"

# Regenerate what Apache Tika's TextAndCSVParser makes of the plain text, CSV and TSV inputs of the corpora,
# which the Rust port of its decisions is tested against.
[unix]
text-reference:
    #!/usr/bin/env bash
    set -euo pipefail
    jars=target/reference-jars
    ci/fetch-reference-jars.sh "$jars"
    eval "$(conformance/prepare.sh {{justfile_directory()}}/target/conformance)"
    fixtures=extractous-core/tests/fixtures/text
    rules=$(just native-image-rules)
    java -cp "$rules:$jars/*:extractous-core/tika-native/src/main/resources" \
        extractous-core/tests/fixtures/formats/TextReference.java \
        "$fixtures/text-reference.tsv.gz" documents=test_files/documents corpus=quality/corpus \
        truth=quality/truth tika="$CONFORMANCE_CORPUS"

# Regenerate the PDFBox resources the Rust PDF parser reads, from the PDFBox jars Tika's native image holds: the
# glyph lists, the Unicode mirroring pairs, the fallback font it measures fonts it finds no program for with, the
# standard 14 fonts' metrics without their kerning pairs, the built-in simple font encodings, and FontBox's
# predefined CFF strings, charsets and encodings.
[unix]
pdf-resources:
    #!/usr/bin/env bash
    set -euo pipefail
    jars=target/reference-jars
    ci/fetch-reference-jars.sh "$jars"
    pdfbox="$jars/pdf/pdfbox-3.0.8.jar"
    models=extractous-core/models/pdf
    mkdir -p "$models/afm"
    for list in glyphlist additional zapfdingbats; do
        unzip -p "$pdfbox" "org/apache/pdfbox/resources/glyphlist/$list.txt" > "$models/$list.txt"
    done
    unzip -p "$pdfbox" org/apache/pdfbox/resources/text/BidiMirroring.txt > "$models/BidiMirroring.txt"
    unzip -p "$pdfbox" org/apache/pdfbox/resources/ttf/LiberationSans-Regular.ttf > "$models/LiberationSans-Regular.ttf"
    for font in Courier Courier-Bold Courier-BoldOblique Courier-Oblique Helvetica Helvetica-Bold \
        Helvetica-BoldOblique Helvetica-Oblique Times-Roman Times-Bold Times-BoldItalic Times-Italic Symbol \
        ZapfDingbats; do
        unzip -p "$pdfbox" "org/apache/pdfbox/resources/afm/$font.afm" | tr -d '\r' \
            | grep -v -e '^KPX' -e '^Comment' -e '^$' > "$models/afm/$font.afm"
    done
    java -cp "$jars/pdf/*" extractous-core/tests/fixtures/pdf/PdfEncodings.java "$models/encodings.tsv"
    java -cp "$jars/pdf/*" extractous-core/tests/fixtures/pdf/CffTables.java "$models/cff.tsv"

# Regenerate what Apache Tika's JSoupParser makes of the HTML inputs of the corpora, of synthetic inputs and of
# html5lib's tree construction tests, the trees jsoup builds for them, and jsoup's named character references,
# which the Rust port of the parser is tested against and reads.
[unix]
html-reference:
    #!/usr/bin/env bash
    set -euo pipefail
    jars=target/reference-jars
    ci/fetch-reference-jars.sh "$jars"
    eval "$(conformance/prepare.sh {{justfile_directory()}}/target/conformance)"
    # shellcheck source=ci/fetch-pinned.sh
    source ci/fetch-pinned.sh
    commit=9329e64694e7835d0dcff9811e22856ef6ad16f9
    tests=target/html5lib-tests
    fetch_pinned "https://codeload.github.com/html5lib/html5lib-tests/tar.gz/$commit" \
        37d2a0619856234e0631cfb08cf77f32420cba07ad4f3b3871ea991126cc5b57 "$tests.tar.gz"
    rm -rf "$tests"
    mkdir -p "$tests"
    tar -xzf "$tests.tar.gz" -C "$tests" --strip-components 1
    mkdir -p extractous-core/tests/fixtures/html
    rules=$(just native-image-rules)
    java -cp "$rules:$jars/*:extractous-core/tika-native/src/main/resources" \
        extractous-core/tests/fixtures/formats/HtmlReference.java \
        extractous-core/tests/fixtures/html extractous-core/models \
        "$tests/tree-construction" documents=test_files/documents corpus=quality/corpus \
        truth=quality/truth tika="$CONFORMANCE_CORPUS"

# Regenerate what mime4j and Tika's mail commons make of the email header values of the corpora and of
# synthetic ones, which the Rust port of Tika's RFC822Parser is tested against.
[unix]
mail-reference:
    #!/usr/bin/env bash
    set -euo pipefail
    jars=target/reference-jars
    ci/fetch-reference-jars.sh "$jars"
    eval "$(conformance/prepare.sh {{justfile_directory()}}/target/conformance)"
    mkdir -p extractous-core/tests/fixtures/mail
    java -cp "$jars/*:extractous-core/tika-native/src/main/resources" \
        extractous-core/tests/fixtures/formats/MailReference.java \
        extractous-core/tests/fixtures/mail/fields.tsv.gz documents=test_files/documents \
        corpus=quality/corpus truth=quality/truth tika="$CONFORMANCE_CORPUS"

# Regenerate the tables of the JFlex scanners jhighlight highlights Java, Groovy and C++ with, which the
# native SourceCodeParser scans source code with as Tika's does.
[unix]
source-code-tables:
    #!/usr/bin/env bash
    set -euo pipefail
    jars=target/reference-jars
    ci/fetch-reference-jars.sh "$jars"
    java -cp "$jars/source-code/*" extractous-core/tests/fixtures/source_code/LexerTables.java \
        extractous-core/src/formats/source_code/tables.rs

# Regenerate what Apache Tika's RTFParser makes of the code pages RTF names and of the dates in its
# info group, which the Rust port is tested against, after refreshing the fixture zoneinfo files the
# dates are computed with from the system's.
[unix]
rtf-reference:
    #!/usr/bin/env bash
    set -euo pipefail
    jars=target/reference-jars
    ci/fetch-reference-jars.sh "$jars"
    fixtures=extractous-core/tests/fixtures/rtf
    system=$(readlink -f /etc/localtime | sed 's|/zoneinfo/.*|/zoneinfo|')
    (cd "$fixtures/zoneinfo" && find . -type f) | while IFS= read -r zone; do
        cp -L "$system/$zone" "$fixtures/zoneinfo/$zone"
        chmod u+w "$fixtures/zoneinfo/$zone"
    done
    java -cp "$jars/*" "$fixtures/RtfReference.java" "$fixtures"

# Regenerate what POI's DataFormatter, as Tika's Excel extractor formats cells with it, makes of
# numbers in the built-in, corpus (formats.txt) and further number formats, which the Rust port is
# tested against, in the locale of the native image and the time zone of the test's zoneinfo file.
xlsx-reference:
    #!/usr/bin/env bash
    set -euo pipefail
    jars=target/reference-jars
    ci/fetch-reference-jars.sh "$jars"
    fixtures=extractous-core/tests/fixtures/xlsx
    java -Duser.language=en -Duser.country=US -Duser.timezone=Europe/Copenhagen \
        -cp "$jars/mime/*" "$fixtures/FormatReference.java" "$fixtures"

# Regenerate what Tika's SummaryExtractor reads from the OLE2 documents among the test files and
# Tika's own test documents, which the Rust port of HPSF is tested against.
ole2-reference:
    #!/usr/bin/env bash
    set -euo pipefail
    : "${CONFORMANCE_CORPUS:?must name the documents conformance/prepare.sh checks out}"
    jars=target/reference-jars
    ci/fetch-reference-jars.sh "$jars"
    fixtures=extractous-core/tests/fixtures/ole2
    java -cp "$jars/mime/*" "$fixtures/SummaryReference.java" "$fixtures/summaries.tsv.gz" \
        documents=test_files/documents corpus="$CONFORMANCE_CORPUS"

# Regenerate Apache Tika's own JunkDetector scores that the Rust port is tested against, after checking
# that the embedded model is the one in the pinned Tika version's jar.
[unix]
junk-reference:
    #!/usr/bin/env bash
    set -euo pipefail
    jars=target/reference-jars
    ci/fetch-reference-jars.sh "$jars"
    model=org/apache/tika/ml/junkdetect/junkdetect.bin
    unzip -p "$jars"/tika-ml-junkdetect-*.jar "$model" | cmp - "extractous-core/models/$(basename "$model")"
    fixtures=extractous-core/tests/fixtures/junk
    java -cp "$jars/*" "$fixtures/JunkReference.java" "$fixtures" \
        extractous-cli/tests/fixtures/language/tatoeba.tsv quality/truth "$fixtures/junk-reference.tsv"

# Regenerate the media types and detection metadata of Apache Tika's own default detector that the Rust port
# is tested against, on the jars of the native image's class path in its order, which decides the order of
# the ZIP container detectors, and with its custom-mimetypes.xml, after checking that the embedded
# tika-mimetypes.xml is the pinned Tika version's.
[unix]
mime-reference:
    #!/usr/bin/env bash
    set -euo pipefail
    jars=target/reference-jars
    ci/fetch-reference-jars.sh "$jars"
    unzip -p "$jars"/mime/tika-core-*.jar org/apache/tika/mime/tika-mimetypes.xml | cmp - {{core}}/models/tika-mimetypes.xml
    eval "$(conformance/prepare.sh {{justfile_directory()}}/target/conformance)"
    classpath={{core}}/tika-native/src/main/resources
    for jar in slf4j-nop-2.0.20 log4j-to-slf4j-2.26.1 tika-parser-microsoft-module-4.1.0 \
        tika-parser-miscoffice-module-4.1.0 tika-core-4.1.0 tika-parser-apple-module-4.1.0 \
        tika-parser-audiovideo-module-4.1.0 tika-parser-pkg-module-4.1.0 slf4j-api-2.0.20 poi-ooxml-5.5.1 \
        poi-5.5.1 log4j-api-2.26.1 tika-parser-zip-commons-4.1.0 commons-compress-1.28.0 commons-io-2.22.0 \
        tika-parser-image-module-4.1.0 commons-codec-1.22.1 commons-lang3-3.20.0 dd-plist-1.30 \
        vorbis-java-core-0.8 commons-collections4-4.6.0 commons-math3-3.6.1 SparseBitSet-1.3 xmlbeans-5.3.0 \
        curvesapi-1.08; do
        classpath="$classpath:$jars/mime/$jar.jar"
    done
    fixtures={{core}}/tests/fixtures/mime
    java -cp "$classpath" "$fixtures/MimeReference.java" test_files/documents quality/corpus \
        "$CONFORMANCE_CORPUS" "$fixtures/mime-reference.tsv"

# Regenerate the answers of Apache Tika's own CharSoup language detector that the Rust port is tested against, after
# checking that the embedded model is the one in the pinned Tika version's jar.
[unix]
charsoup-reference:
    #!/usr/bin/env bash
    set -euo pipefail
    jars=target/reference-jars
    ci/fetch-reference-jars.sh "$jars"
    model=org/apache/tika/langdetect/charsoup/langdetect-20260320.bin
    unzip -p "$jars"/tika-langdetect-charsoup-core-*.jar "$model" | cmp - "extractous-cli/models/$(basename "$model")"
    fixtures=extractous-cli/tests/fixtures/language
    java -cp "$jars/*" "$fixtures/CharSoupReference.java" "$fixtures" quality/truth "$fixtures/charsoup-reference.tsv"

# Build the container image extractous:local, FROM scratch around the static executable that
# `just static` built.
[linux]
container:
    #!/usr/bin/env bash
    set -euo pipefail
    context=$(mktemp -d)
    trap 'rm -rf "$context"' EXIT
    cp target/static-image/extractous "$context/"
    docker build --file Dockerfile --tag extractous:local "$context"

# Retrain the models that tell closely related languages apart (extractous-cli/models/language-groups.bin) from the
# Wikipedia shards pinned in extractous-cli/models/language-groups.tsv, fetched into target/wikipedia (about 7 GB).
[unix]
language-groups:
    #!/usr/bin/env bash
    set -euo pipefail
    sources=extractous-cli/models/language-groups.tsv
    ci/fetch-wikipedia.sh "$sources" target/wikipedia
    rev=$(nix eval --raw --impure --expr "(builtins.fromJSON (builtins.readFile $PWD/devenv.lock)).nodes.nixpkgs.locked.rev")
    python=$(nix build --no-link --print-out-paths --impure --expr \
        "((builtins.getFlake \"github:cachix/devenv-nixpkgs/$rev\").legacyPackages.\${builtins.currentSystem}.python3.withPackages (ps: [ ps.pyarrow ]))")
    "$python/bin/python3" -I extractous-cli/models/language-groups.py "$sources" target/wikipedia target/language-groups
    LANGUAGE_GROUPS_SENTENCES="$PWD/target/language-groups" cargo test --release --package extractous --lib \
        language::groups::tests::train -- --ignored

# Compare language detection with Tika's CharSoup per language on a TSV of the expected ISO 639-3 code and a text per
# line, and print the shares each detects right as a Markdown table.
language-accuracy file:
    LANGUAGE_EVALUATION={{absolute_path(file)}} cargo test --release --package extractous --lib \
        language::tests::accuracy_against_tika -- --ignored --nocapture

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
# write their throughput, latency and memory to target/server-compare/comparison.md;
# SERVER_COMPARE_BINARY serves another build, such as target/static-image/extractous.
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
