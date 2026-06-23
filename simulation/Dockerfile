FROM rust:1-slim AS build
WORKDIR /src
COPY Cargo.toml ./
COPY config.json ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=build /src/target/release/simulation /usr/local/bin/simulation
ENV RUST_LOG=info
EXPOSE 9003/tcp
ENTRYPOINT ["/usr/local/bin/simulation"]
