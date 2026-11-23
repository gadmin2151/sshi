FROM rust:1.96.0-bookworm AS toolchain

# Link against Debian 11 glibc for compatibility with Bullseye and newer releases.
FROM debian:bullseye-slim AS compiler
RUN sed -i 's|http://deb.debian.org/debian-security|http://archive.debian.org/debian-security|' /etc/apt/sources.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates gcc gcc-aarch64-linux-gnu gcc-arm-linux-gnueabihf \
        libc6-dev libc6-dev-arm64-cross libc6-dev-armhf-cross libstdc++6 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=toolchain /usr/local/cargo/bin/ /usr/local/cargo/bin/
COPY --from=toolchain /usr/local/rustup/ /usr/local/rustup/
ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc \
    CC_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc
RUN rustup target add aarch64-unknown-linux-gnu armv7-unknown-linux-gnueabihf

FROM compiler AS build
WORKDIR /iroh-ssh
COPY Cargo.toml Cargo.lock ./
COPY build.rs LICENSE ./
COPY src/ ./src/
COPY desktop/linux/ ./desktop/linux/
COPY assets/windows/sshi-logo.png ./assets/windows/sshi-logo.png
COPY packaging/ ./packaging/
ARG SSHI_TARGETS="x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu armv7-unknown-linux-gnueabihf"
RUN set -eu; \
    package_id=$(cargo pkgid --locked -p iroh-ssh); \
    version=${package_id##*#}; \
    version=${version##*@}; \
    for target in $SSHI_TARGETS; do \
        cargo build --release --locked --target "$target" --bin sshi; \
        sh packaging/build-deb.sh "target/$target/release/sshi" /debs "$version"; \
    done

FROM scratch AS packages
COPY --from=build /debs/ /
