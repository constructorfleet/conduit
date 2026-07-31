FROM rust:bookworm AS builder

WORKDIR /src
COPY . .
RUN cargo build --locked --release -p conduit-api

FROM debian:bookworm-slim AS runtime

RUN useradd --create-home --shell /usr/sbin/nologin conduit
COPY --from=builder /src/target/release/conduit-api /usr/local/bin/conduit-api

USER conduit
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/conduit-api"]
