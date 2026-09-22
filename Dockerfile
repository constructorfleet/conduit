# Pinned to the same version as rust-toolchain.toml, and CI fails if the two
# drift apart. Dependabot's docker ecosystem proposes bumps to this line.
#
# The pin is not redundant with rust-toolchain.toml: rustup would honour that
# file regardless, but only by downloading a second toolchain into every image
# build. Matching the tag means the toolchain already in the image is the one
# used.
#
# The Debian release is trixie, not bookworm, because of `ort`: `conduit-vad`
# links the prebuilt static onnxruntime that `ort-sys` downloads, and those
# objects are compiled with GCC 14. Linking them against bookworm's libstdc++
# (GCC 12) fails with thousands of undefined references to symbols such as
# `std::string::_M_replace_cold` and `__cxa_call_terminate`, which only exist
# from GCC 14 on. Trixie ships GCC 14. CI checks that the runtime stage below
# names the same release, because the linked binary needs that libstdc++ at
# run time too.
FROM rust:1.97.1-trixie AS builder

WORKDIR /src
COPY . .
RUN cargo build --locked --release -p conduit-api

# Same Debian release as the builder, and not by habit: the binary loads the
# libstdc++ it was linked against, so an older runtime image would fail at
# start-up with a missing `GLIBCXX_3.4.33`, which is the same mismatch as the
# build failure above, deferred until nobody is watching.
FROM debian:trixie-slim AS runtime

# No ca-certificates package is installed on purpose: reqwest is built with
# `rustls-tls` and sqlx with `tls-rustls-ring`, both of which compile Mozilla's
# root store in via `webpki-roots`. Nothing here reads /etc/ssl/certs. Revisit
# if a dependency switches to `rustls-native-certs` or native-tls.
RUN useradd --create-home --shell /usr/sbin/nologin conduit
COPY --from=builder /src/target/release/conduit-api /usr/local/bin/conduit-api

USER conduit
# 8080 is the authenticated service API. 9090 serves /health and /metrics with
# no authentication at all, so publish it only within your trust boundary — a
# `docker run -p 8080:8080` that omits 9090 is the intended default.
EXPOSE 8080 9090
ENTRYPOINT ["/usr/local/bin/conduit-api"]
