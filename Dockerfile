FROM rust:1.98-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock askama.toml rustfmt.toml ./
COPY src ./src
COPY static ./static
COPY registry ./registry
COPY release ./release
RUN cargo build --release --locked

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS runtime
RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates wget \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /app qed
WORKDIR /app
COPY --from=builder /src/target/release/qed /usr/local/bin/qed
COPY --from=builder /src/static ./static
COPY --from=builder /src/registry ./registry
COPY --from=builder /src/release ./release
RUN mkdir -p /data && chown -R qed:qed /app /data
USER qed
ENV QED_ENV=production \
    QED_BIND=0.0.0.0:8080 \
    QED_DATA_DIR=/data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 CMD wget --quiet --tries=1 --spider http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["/usr/local/bin/qed"]
