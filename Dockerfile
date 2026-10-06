FROM --platform=linux/amd64 registry.access.redhat.com/ubi9/ubi:9.7@sha256:ee3871c273cc65a2bd24970b47c15b90fbf962841bc2e41e4f915b1c4c0208e2 AS builder
ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:${PATH}
WORKDIR /src

RUN dnf install -y \
        --setopt=install_weak_deps=0 \
        --setopt=tsflags=nodocs \
        ca-certificates gcc gcc-c++ glibc-devel make perl \
    && dnf clean all \
    && rm -rf /var/cache/dnf \
    && curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
        --output /tmp/rustup-init \
        https://static.rust-lang.org/rustup/archive/1.29.1/x86_64-unknown-linux-gnu/rustup-init \
    && echo 'dda7234360b7f578ca8b0ddcb80145646fa61a67c1720a5abc7051b35c9fcb71  /tmp/rustup-init' \
        | sha256sum --check --strict \
    && chmod 0755 /tmp/rustup-init \
    && /tmp/rustup-init --default-toolchain 1.98.0 --profile minimal --no-modify-path -y \
    && rm -f /tmp/rustup-init

COPY Cargo.toml Cargo.lock askama.toml rustfmt.toml ./
COPY CHANGELOG.md SECURITY.md ./
COPY docs ./docs
COPY src ./src
COPY static ./static
COPY registry ./registry
COPY release ./release
RUN cargo build --release --locked

FROM --platform=linux/amd64 registry.access.redhat.com/ubi9/ubi-micro:9.7@sha256:efa877c7a38fc7f37a3f942ed4b6d357bec75360f572099c59da1ce09ca37917 AS ubi-micro-base
FROM --platform=linux/amd64 registry.access.redhat.com/ubi9/ubi:9.7@sha256:ee3871c273cc65a2bd24970b47c15b90fbf962841bc2e41e4f915b1c4c0208e2 AS runtime-packages
COPY --from=ubi-micro-base / /rootfs/
RUN dnf upgrade --installroot /rootfs -y \
        --releasever=9.7 \
        --setopt=install_weak_deps=0 \
        --setopt=tsflags=nodocs \
        --setopt=keepcache=0 \
    && dnf install --installroot /rootfs -y \
        --releasever=9.7 \
        --setopt=install_weak_deps=0 \
        --setopt=tsflags=nodocs \
        --setopt=keepcache=0 \
        ca-certificates libgcc \
    && dnf --installroot /rootfs clean all \
    && rm -rf /rootfs/var/cache/dnf \
        /rootfs/var/lib/dnf \
        /rootfs/etc/dnf \
        /rootfs/etc/yum.repos.d \
        /rootfs/etc/yum.conf

FROM --platform=linux/amd64 registry.access.redhat.com/ubi9/ubi-micro:9.7@sha256:efa877c7a38fc7f37a3f942ed4b6d357bec75360f572099c59da1ce09ca37917 AS runtime
COPY --from=runtime-packages /rootfs/ /
WORKDIR /app
COPY --from=builder /src/target/release/qed /usr/local/bin/qed
COPY --from=builder /src/target/release/qed-healthcheck /usr/local/bin/qed-healthcheck
COPY --from=builder /src/static ./static
COPY --from=builder /src/registry ./registry
COPY --from=builder /src/release ./release
COPY --from=builder /src/CHANGELOG.md ./CHANGELOG.md
RUN mkdir -p /data && chown -R 10001:0 /app /data
RUN rm -rf /var/cache/dnf \
        /var/lib/dnf \
        /etc/dnf \
        /etc/yum.repos.d \
        /etc/yum.conf \
        /etc/bash_completion.d \
    && rm -f /usr/bin/dnf \
        /usr/bin/dnf-3 \
        /usr/bin/microdnf \
        /usr/bin/yum \
        /usr/bin/yum-3 \
        /usr/bin/rpm \
        /bin/dnf \
        /bin/microdnf \
        /bin/yum \
        /bin/rpm \
        /usr/bin/curl \
        /usr/bin/curl-minimal \
        /usr/bin/wget \
        /bin/curl \
        /bin/wget \
        /usr/bin/bash \
        /usr/bin/bashbug \
        /usr/bin/bashbug-64 \
        /usr/bin/sh \
        /usr/bin/rbash \
        /bin/bash \
        /bin/sh \
        /bin/rbash \
        /etc/bashrc \
        /etc/skel/.bash_logout \
        /etc/skel/.bash_profile \
        /etc/skel/.bashrc \
        /var/log/dnf.librepo.log \
        /var/log/dnf.log \
        /var/log/dnf.rpm.log \
        /var/log/hawkey.log
ENV QED_ENV=production \
    QED_BIND=0.0.0.0:8080 \
    QED_DATA_DIR=/data
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/qed"]
CMD []
USER 10001
