# Multi-stage build for the access-controlled browser. Targets
# linux/amd64 and linux/arm64 via `docker buildx build --platform ...`.
#
# Stages:
#   1. ui-build    — Next.js static export -> ui/out
#   2. rust-build  — cargo build --release (runs natively on target arch
#                    via buildx's QEMU emulation)
#   3. runtime     — Debian bookworm-slim + chromium + tini, non-root user

# ---------- Stage 1: UI -----------------------------------------------------
FROM --platform=$BUILDPLATFORM node:22-bookworm-slim AS ui-build
WORKDIR /ui
COPY ui/package.json ui/package-lock.json ./
RUN npm ci
COPY ui/ ./
RUN npm run build

# ---------- Stage 2: Rust ---------------------------------------------------
FROM rust:1.95-slim-bookworm AS rust-build
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# Cache dependency builds via a dummy workspace first.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY --from=ui-build /ui/out ./ui/out
COPY crates/injected-js ./crates/injected-js
RUN cargo build --release --bin acb-daemon --bin acb-cli && \
    mkdir -p /out && \
    cp target/release/acb-daemon /out/ && \
    cp target/release/acb-cli    /out/

# ---------- Stage 3: Runtime ------------------------------------------------
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
