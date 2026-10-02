FROM rust:1.98-slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY web web
RUN cargo build --release

FROM debian:trixie-slim
COPY --from=build /src/target/release/s15 /usr/local/bin/s15
CMD ["s15", "relay", "--listen", "0.0.0.0:8080"]
