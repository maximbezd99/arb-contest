FROM rust:1-slim AS build
WORKDIR /src
COPY Cargo.toml ./
COPY config.json ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
# `curl` is here purely for the docker HEALTHCHECK that polls /health.
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/simulation /usr/local/bin/simulation
ENV RUST_LOG=info
EXPOSE 9003/tcp
ENTRYPOINT ["/usr/local/bin/simulation"]
