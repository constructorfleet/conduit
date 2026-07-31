# Versioning

Conduit uses SemVer for the API server, firmware YAML package, and container
image.

## Source of Truth

The workspace version in `Cargo.toml` is the only release version source:

```toml
[workspace.package]
version = "0.1.0"
```

All internal workspace dependency versions in the root `Cargo.toml` must match
that value. CI checks this with:

```sh
python3 scripts/workspace_version.py check
```

## Version Rules

- Patch: compatible bug fixes, CI fixes, documentation, and firmware YAML fixes
  that do not require operators to change configuration.
- Minor: new API endpoints, provider capabilities, firmware options, metrics,
  or package contents that remain backward compatible.
- Major: incompatible API, storage, wire-protocol, firmware configuration, or
  deployment changes.
- Prerelease versions use SemVer prerelease syntax such as `0.2.0-rc.1`.

Versioned package tags are immutable in intent. `latest` is only a moving
convenience tag for the newest successful `main` publish.

## Automation

Use the `Version Bump` GitHub workflow to open a version bump PR. It supports
`patch`, `minor`, `major`, or an exact SemVer value. The workflow updates
`Cargo.toml`, refreshes `Cargo.lock`, and opens a PR into `main`.

Local equivalent:

```sh
python3 scripts/workspace_version.py bump patch --write
cargo metadata --format-version 1 >/dev/null
python3 scripts/workspace_version.py check
```

After a version bump PR merges, the `Publish` workflow publishes:

- `ghcr.io/<owner>/<repo>:latest`
- `ghcr.io/<owner>/<repo>:<version>`
- `ghcr.io/<owner>/<repo>:v<version>`
- `ghcr.io/<owner>/<repo>/conduit-artifacts:latest`
- `ghcr.io/<owner>/<repo>/conduit-artifacts:<version>`
- `ghcr.io/<owner>/<repo>/conduit-artifacts:v<version>`
