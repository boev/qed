FROM rust:1.98-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock askama.toml rustfmt.toml ./
COPY src ./src
COPY static ./static
COPY registry ./registry
COPY release ./release
RUN cargo build --release --locked

# ECS runs this amd64 runtime image; pin the official amd64 manifest digest.
FROM debian:bookworm-slim@sha256:f3034a6ec3c1205360777c4aae76234998866ad18806ae62b63a3f84ccad782b AS runtime
RUN apt-get update \
    && apt-get upgrade -y \
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
