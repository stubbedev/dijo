# syntax=docker/dockerfile:1

FROM ghcr.io/graalvm/native-image-community:25@sha256:0d936f32bb8acb5bc60c41b33e05f064d7a6aaf36b726538296c54949bd4a3c0 AS build
ARG TARGETARCH
ARG RUSTUP_VERSION=1.29.1
ARG RUST_VERSION=1.98.1
ENV GRAALVM_HOME=/usr/lib64/graalvm/graalvm-community-java25 \
    RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:$PATH
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

FROM debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f
RUN apt-get update \
    && apt-get install --yes --no-install-recommends tesseract-ocr tesseract-ocr-eng \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin extractous
COPY --from=build /usr/local/bin/extractous /usr/local/bin/extractous
COPY LICENSE THIRD-PARTY-LICENSES.md /usr/share/doc/extractous/
ENV XDG_CACHE_HOME=/var/cache \
    EXTRACTOUS_LISTEN=0.0.0.0:9998 \
    OMP_THREAD_LIMIT=1
RUN printf 'probe\n' > /tmp/probe.txt \
    && extractous detect /tmp/probe.txt \
    && rm /tmp/probe.txt
USER extractous
EXPOSE 9998
ENTRYPOINT ["extractous"]
CMD ["serve"]
