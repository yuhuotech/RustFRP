FROM rust:1.85-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY rustfrp-core ./rustfrp-core
COPY rustfrp-agent ./rustfrp-agent
COPY rustfrp-server ./rustfrp-server
COPY rustfrp-protocol ./rustfrp-protocol
RUN cargo build --release -p rustfrp-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home-dir /var/lib/rustfrp --create-home rustfrp
COPY --from=build /src/target/release/rustfrp-server /usr/local/bin/rustfrp-server
USER rustfrp
WORKDIR /var/lib/rustfrp
EXPOSE 7000/tcp 8080/tcp
ENTRYPOINT ["/usr/local/bin/rustfrp-server"]
CMD ["run"]
