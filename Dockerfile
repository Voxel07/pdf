# --- frontend -------------------------------------------------------------------------
FROM oven/bun:1.4.5 AS web
WORKDIR /web
COPY web/package.json web/bun.lock ./
RUN bun install --frozen-lockfile
COPY web/ ./
COPY calendar/src/lib/dates.ts calendar/src/lib/events.ts /calendar/src/lib/
RUN bun run build

# --- backend --------------------------------------------------------------------------
FROM rust:1.100.0-slim-trixie AS server
WORKDIR /src
COPY server/Cargo.toml server/Cargo.lock ./
# Cache dependencies by building them against a placeholder main.rs. The placeholder uses none
# of them, so cargo's unused_dependencies lint is allowed for this step only; the real manifest
# is copied back before the actual build.
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
 && printf '\n[lints.cargo]\nunused_dependencies = "allow"\n' >> Cargo.toml \
 && cargo build --release --locked && rm -rf src
COPY server/Cargo.toml ./
COPY server/src ./src
RUN touch src/main.rs && cargo build --release --locked

# --- runtime --------------------------------------------------------------------------
FROM debian:13.8-slim
ARG VERSION=dev
LABEL org.opencontainers.image.title="pdfsign" org.opencontainers.image.version="${VERSION}"
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates tzdata \
      libzstd1=1.5.7+dfsg-1+deb13u1 \
      zlib1g=1:1.3.dfsg+really1.3.1-1+deb13u1 \
 && rm -rf /var/lib/apt/lists/* \
 && install -d -m 0700 -o 10001 -g 10001 /data /export /backups
COPY --from=server /src/target/release/pdfsign /usr/local/bin/pdfsign
COPY --from=web /web/dist /app/web
ENV PDFSIGN_VERSION=${VERSION} DATA_DIR=/data EXPORT_DIR=/export BACKUP_DIR=/backups WEB_DIR=/app/web BIND=0.0.0.0:8080
USER 10001:10001
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8080"]
CMD ["pdfsign", "serve"]
