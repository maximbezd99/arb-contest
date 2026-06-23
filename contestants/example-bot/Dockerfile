FROM rust:1-slim AS build
WORKDIR /src
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=build /src/target/release/example-bot /usr/local/bin/contestant
ENTRYPOINT ["/usr/local/bin/contestant"]
