FROM rust:1-slim AS build
WORKDIR /src
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
# `curl` is here purely for the docker HEALTHCHECK that polls /health.
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /src/target/release/simulation /usr/local/bin/simulation
COPY config.json /app/config.json
ENV RUST_LOG=info
EXPOSE 9003/tcp
ENTRYPOINT ["/usr/local/bin/simulation"]
