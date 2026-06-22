FROM rust:1-slim AS build
WORKDIR /src
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=build /src/target/release/simulation /usr/local/bin/simulation
ENV RUST_LOG=info
EXPOSE 9001/tcp 9002/udp
ENTRYPOINT ["/usr/local/bin/simulation"]
