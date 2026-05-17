# Multi-stage build for the access-controlled browser. Targets
# linux/amd64 and linux/arm64 via `docker buildx build --platform ...`.
#
# Rust is cross-compiled with cargo-zigbuild on the BUILDPLATFORM so the
# arm64 leg does not pay QEMU emulation cost. Without this, the arm64
# build was taking ~1 hour on a 2-vCPU ubuntu-latest runner.

# ---------- Stage 1: UI -----------------------------------------------------
FROM --platform=$BUILDPLATFORM node:22-bookworm-slim AS ui-build
WORKDIR /ui
COPY ui/package.json ui/package-lock.json ./
RUN npm ci
COPY ui/ ./
RUN npm run build

# ---------- Stage 2: Rust (cross-compiles on BUILDPLATFORM) -----------------
FROM --platform=$BUILDPLATFORM rust:1.95-slim-bookworm AS rust-build
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev ca-certificates curl xz-utils python3 \
 && rm -rf /var/lib/apt/lists/*

# zig provides a portable cross-linker that lets cargo-zigbuild target
# foreign GNU triples without QEMU.
RUN curl -fsSL https://ziglang.org/download/0.13.0/zig-linux-$(uname -m)-0.13.0.tar.xz \
      | tar -xJ -C /opt \
 && ln -sf /opt/zig-linux-*-0.13.0/zig /usr/local/bin/zig
RUN cargo install --locked cargo-zigbuild@0.20.0

WORKDIR /src
ARG TARGETPLATFORM
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY --from=ui-build /ui/out ./ui/out

RUN case "$TARGETPLATFORM" in \
      "linux/amd64") TRIPLE=x86_64-unknown-linux-gnu ;; \
      "linux/arm64") TRIPLE=aarch64-unknown-linux-gnu ;; \
      *) echo "unsupported TARGETPLATFORM: $TARGETPLATFORM" >&2 && exit 1 ;; \
    esac \
 && rustup target add "$TRIPLE" \
 && cargo zigbuild --release --target "$TRIPLE" --bin acb-daemon --bin acb-cli \
 && mkdir -p /out \
 && cp "target/$TRIPLE/release/acb-daemon" /out/ \
 && cp "target/$TRIPLE/release/acb-cli"    /out/

# ---------- Stage 3: Runtime (TARGETPLATFORM native) ------------------------
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates fonts-liberation libnss3 libatk-bridge2.0-0 libdrm2 libxkbcommon0 \
      libxcomposite1 libxdamage1 libxfixes3 libxrandr2 libgbm1 libasound2 libpango-1.0-0 \
      libcairo2 libcups2 chromium tini \
 && rm -rf /var/lib/apt/lists/* \
 && useradd -m -u 1000 acb
WORKDIR /app
COPY --from=rust-build /out/acb-daemon /usr/local/bin/acb-daemon
COPY --from=rust-build /out/acb-cli    /usr/local/bin/acb-cli
COPY config.example.yaml /app/config.yaml
RUN mkdir -p /app/logs /app/var/profile && chown -R acb:acb /app
USER acb:acb
ENV RUST_LOG=info \
    ACB_BIND=0.0.0.0 \
    ACB_PORT=39100 \
    ACB_HEADLESS=true
EXPOSE 39100
ENTRYPOINT ["/usr/bin/tini","--"]
CMD ["acb-daemon","--foreground","--config","/app/config.yaml","--bind","0.0.0.0","--port","39100","--headless"]
