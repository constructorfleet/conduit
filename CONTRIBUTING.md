# Contributing

Conduit uses the same rules for humans and agents. Read [AGENTS.md](AGENTS.md)
before changing code; this file is the short operating guide, not a replacement
for the repository standard.

## Development Flow

Work from `origin/main` unless a maintainer asks for another base. Keep each
branch focused on one issue or one logical change, and keep commits atomic.

Prefer test-driven development for behavior changes:

1. Add or update the smallest test that proves the behavior.
2. Run it and verify it fails for the expected reason.
3. Implement the minimal change.
4. Re-run the test, then the broader gates below.

Documentation is part of the implementation. If behavior, configuration,
public API, routes, metrics, or examples change, update the matching docs in
the same commit.

To see a change in the real stack, `scripts/dev.sh` runs the API and the
Operator Console together and stops both on Ctrl-C. It defaults to an anonymous
API on loopback with real providers; `--tokens FILE` authenticates instead,
`--echo` builds the providers that need no speech engine, and `--help` lists the
rest.

## Quality Gates

CI runs these Rust gates:

```sh
python3 scripts/workspace_version.py check
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --no-default-features
```

CI also checks the declared MSRV:

```sh
msrv=$(sed -ne 's/^rust-version = "\([0-9.]*\)"$/\1/p' Cargo.toml)
RUSTUP_TOOLCHAIN="$msrv" cargo check --locked --workspace --all-features --all-targets
RUSTUP_TOOLCHAIN="$msrv" cargo check --locked --workspace --no-default-features
```

Additional CI jobs run:

```sh
firmware/test.sh
scripts/tests/dev_test.sh
cargo audit
cargo llvm-cov --workspace --all-features --cobertura --output-path cobertura.xml
docker buildx build --load -t conduit-check .
```

PostgreSQL store tests run only when `CONDUIT_TEST_POSTGRES_URL` is set. CI
provides a PostgreSQL service, so a local run without that variable is not the
same coverage as CI.

## Pull Requests

Every PR should state:

- what changed
- why it changed
- how it was tested
- whether it changes configuration, API behavior, metrics, storage, or firmware

Draft PRs are fine while checks or review are still pending. A PR is ready only
when the relevant gates pass and the docs match the implementation.

## Security

Never commit secrets. Token files, API keys, database URLs with credentials,
and captured user audio do not belong in commits, logs, or examples.

The service API uses bearer tokens. Keep auth changes especially small and
verify that failures do not reveal token values or distinguish unknown tokens
from missing ones.
