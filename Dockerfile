# Pinned to the same version as rust-toolchain.toml, and CI fails if the two
# drift apart. Dependabot's docker ecosystem proposes bumps to this line.
#
# The pin is not redundant with rust-toolchain.toml: rustup would honour that
# file regardless, but only by downloading a second toolchain into every image
# build. Matching the tag means the toolchain already in the image is the one
# used.
FROM rust:1.97.1-bookworm AS builder

WORKDIR /src
COPY . .
RUN cargo build --locked --release -p conduit-api

FROM debian:bookworm-slim AS runtime

# No ca-certificates package is installed on purpose: reqwest is built with
# `rustls-tls` and sqlx with `tls-rustls-ring`, both of which compile Mozilla's
# root store in via `webpki-roots`. Nothing here reads /etc/ssl/certs. Revisit
# if a dependency switches to `rustls-native-certs` or native-tls.
RUN useradd --create-home --shell /usr/sbin/nologin conduit
COPY --from=builder /src/target/release/conduit-api /usr/local/bin/conduit-api

USER conduit
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/conduit-api"]
