FROM rust:1.96.0-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock build.rs ./
COPY src/ ./src/
RUN cargo build --release --locked --bin iroh-ssh

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -m 0700 -o 10001 -g 10001 /keys
COPY --from=build /build/target/release/iroh-ssh /usr/local/bin/iroh-ssh
USER 10001:10001
ENTRYPOINT ["iroh-ssh"]
