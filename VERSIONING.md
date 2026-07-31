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

Creating it is idempotent. The decision that a version is unreleased is made
before the packages are built, so two pushes landing close together can both
reach the tagging step and race for the same tag. An existing tag is left exactly
where it is — re-tagging would break the immutability above — and the build that
lost the race still succeeds, because it did nothing wrong. A push that fails for
any other reason still fails the workflow.

`Publish` tags with the workflow's own `GITHUB_TOKEN`. Pushes made with it
deliberately do not trigger other workflows, which costs nothing while nothing in
this repository triggers on tags; adding a workflow that must react to a release
tag means giving the tagging job a token that does.

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

## What Guards `main`

A repository ruleset governs the default branch. It is recorded here because a
release process is only as immutable as the branch it publishes from, and the
ruleset is otherwise visible only in repository settings.

- Changes reach `main` by pull request, squash-merged, with linear history.
  Deleting the branch and non-fast-forward pushes are refused.
- Every `CI` job — `check`, `coverage`, `msrv`, `firmware`, `docker`, and
  `audit` — must pass before a merge. `Publish` calls the same workflow, so a
  release runs the gates a pull request ran.
- CodeQL must report no errors and no high-or-higher security alerts.
- Coverage must stay at or above 85%, and may not drop more than 3 percentage
  points against `main`. The `coverage` job measures it; that rule blocks a merge
  when the measurement is missing as well as when it is low, so the job existing
  is part of the gate rather than a convenience.
- Organization admins can bypass all of the above. That is deliberate: a gate
  nobody can override in an emergency is a gate that gets deleted during one.
