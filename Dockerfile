# Stage 1: Build the React Web Viewer UI
FROM node:22-alpine AS web-builder
WORKDIR /web
COPY app/package.json app/package-lock.json ./
RUN npm ci
COPY app/index.html app/tsconfig.json app/vite.config.ts ./
COPY app/src ./src
RUN npm run build

# Stage 2: Build the Rust Rendezvous & E2E Relay Server
FROM rust:1-bookworm AS rust-builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p remoteconnect-server

# Stage 3: Minimal production runtime
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY --from=rust-builder /src/target/release/remoteconnect-server /usr/local/bin/remoteconnect-server
COPY --from=web-builder /web/dist /srv/web

ENV REMOTECONNECT_BIND=0.0.0.0:8080
ENV REMOTECONNECT_WEB_DIR=/srv/web
ENV RUST_LOG=info

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8080/api/health || exit 1

CMD ["remoteconnect-server"]
