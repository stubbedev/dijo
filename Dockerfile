# syntax=docker/dockerfile:1

FROM debian:trixie@sha256:913f6706df59a68922d1dd08f78c2476560a8d367897200a6005b00e5f67c2d5 AS build
ARG TARGETARCH
ARG RUSTUP_VERSION=1.29.1
ARG RUST_VERSION=1.98.1
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl gcc libc6-dev zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:$PATH
COPY ci/ /ci/
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) platform=linux-x64 ;; \
      arm64) platform=linux-aarch64 ;; \
      *) echo "unsupported architecture $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    home=$(/ci/install-graalvm.sh "$platform" /opt); \
    ln -s "$home" /opt/graalvm
ENV GRAALVM_HOME=/opt/graalvm \
    JAVA_HOME=/opt/graalvm
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) triple=x86_64-unknown-linux-gnu ;; \
      arm64) triple=aarch64-unknown-linux-gnu ;; \
      *) echo "unsupported architecture $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    url="https://static.rust-lang.org/rustup/archive/$RUSTUP_VERSION/$triple/rustup-init"; \
    curl --fail --silent --show-error --location --output /tmp/rustup-init "$url"; \
    curl --fail --silent --show-error --location --output /tmp/rustup-init.sha256 "$url.sha256"; \
    echo "$(cut -d ' ' -f 1 /tmp/rustup-init.sha256)  /tmp/rustup-init" | sha256sum --check --strict; \
    chmod +x /tmp/rustup-init; \
    /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain "$RUST_VERSION"; \
    rm /tmp/rustup-init /tmp/rustup-init.sha256
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/opt/cargo/registry \
    --mount=type=cache,target=/root/.gradle \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --package extractous \
    && cp target/release/extractous /usr/local/bin/extractous
RUN /ci/fetch-tessdata.sh /opt/tessdata

FROM debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f
RUN apt-get update \
    && apt-get install --yes --no-install-recommends tesseract-ocr tesseract-ocr-eng \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin extractous
COPY --from=build /usr/local/bin/extractous /usr/local/bin/extractous
COPY --from=build /opt/tessdata /usr/share/extractous/tessdata
COPY LICENSE THIRD-PARTY-LICENSES.md /usr/share/doc/extractous/
ENV XDG_CACHE_HOME=/var/cache \
    EXTRACTOUS_LISTEN=0.0.0.0:9998 \
    OMP_THREAD_LIMIT=1 \
    TESSDATA_PREFIX=/usr/share/extractous/tessdata
RUN printf 'probe\n' > /tmp/probe.txt \
    && extractous detect /tmp/probe.txt \
    && rm /tmp/probe.txt
USER extractous
EXPOSE 9998
ENTRYPOINT ["extractous"]
CMD ["serve"]
