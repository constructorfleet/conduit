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

## Tag Immutability

Versioned package tags are immutable, and the `Publish` workflow enforces it
rather than trusting intent. `Publish` runs on every push to `main`, not only on
version bumps, so it first asks whether the current version has been released:

- If the git tag `v<version>` is already on the remote, this commit is a
  follow-up to an existing release. Only `latest` and `sha-<commit>` move; the
  `<version>` and `v<version>` tags are left pointing at the build that first
  claimed them.
- If it is not, the version tags are published and `Publish` then creates the
  git tag `v<version>`, which is what makes every later push take the branch
  above.

The git tag is the release record. It is created only after both the container
image and the artifact package publish successfully, so a failed publish can be
retried by re-running the workflow rather than needing a version bump.

`latest` is a moving convenience tag for the newest successful `main` publish.
`sha-<commit>` is published on every build so that whatever `latest` points at
can always be named by something immutable — including for commits that publish
no version tag at all.

To release a new version, bump the version.

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

After a version bump PR merges, the `Publish` workflow publishes, for both
`ghcr.io/constructorfleet/conduit` and
`ghcr.io/constructorfleet/conduit/conduit-artifacts`:

- `:latest`
- `:sha-<commit>`
- `:<version>`
- `:v<version>`

and then creates the git tag `v<version>`.

Any other push to `main` publishes only `:latest` and `:sha-<commit>`, per
[Tag Immutability](#tag-immutability).
