#!/usr/bin/env python3
"""Manage the Conduit workspace version without pulling in a TOML dependency."""

from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CARGO_TOML = ROOT / "Cargo.toml"
INTERNAL_DEPENDENCIES = (
    "conduit-core",
    "conduit-provider",
    "conduit-metrics",
    "conduit-openai",
    "conduit-store",
    "conduit-runtime",
)
SEMVER_RE = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z.-]+))?$")


def read_cargo_toml() -> str:
    return CARGO_TOML.read_text(encoding="utf-8")


def workspace_version(text: str) -> str:
    match = re.search(r'(?m)^\[workspace\.package\]\nversion = "([^"]+)"$', text)
    if match is None:
        raise SystemExit("workspace.package.version was not found")
    return match.group(1)


def validate_semver(version: str) -> None:
    if SEMVER_RE.match(version) is None:
        raise SystemExit(f"{version!r} is not a supported SemVer version")


def bump_version(version: str, part: str) -> str:
    validate_semver(version)
    major, minor, patch, _prerelease = SEMVER_RE.match(version).groups()  # type: ignore[union-attr]
    major_int = int(major)
    minor_int = int(minor)
    patch_int = int(patch)
    if part == "major":
        return f"{major_int + 1}.0.0"
    if part == "minor":
        return f"{major_int}.{minor_int + 1}.0"
    if part == "patch":
        return f"{major_int}.{minor_int}.{patch_int + 1}"
    raise SystemExit(f"unknown bump part: {part}")


def replace_once(text: str, pattern: str, replacement: str, description: str) -> str:
    updated, count = re.subn(pattern, replacement, text, count=1, flags=re.MULTILINE)
    if count != 1:
        raise SystemExit(f"expected to update exactly one {description}, updated {count}")
    return updated


def set_version(text: str, version: str) -> str:
    validate_semver(version)
    text = replace_once(
        text,
        r'(^\[workspace\.package\]\nversion = )"[^"]+"$',
        rf'\1"{version}"',
        "workspace package version",
    )
    for dependency in INTERNAL_DEPENDENCIES:
        text = replace_once(
            text,
            rf'(^{re.escape(dependency)} = \{{ path = "crates/{re.escape(dependency)}", version = )"[^"]+"( \}}$)',
            rf'\1"{version}"\2',
            f"{dependency} dependency version",
        )
    return text


def mismatched_internal_versions(text: str, version: str) -> list[str]:
    mismatches: list[str] = []
    for dependency in INTERNAL_DEPENDENCIES:
        pattern = rf'^{re.escape(dependency)} = \{{ path = "crates/{re.escape(dependency)}", version = "([^"]+)" \}}$'
        match = re.search(pattern, text, flags=re.MULTILINE)
        if match is None:
            mismatches.append(f"{dependency}: missing")
        elif match.group(1) != version:
            mismatches.append(f"{dependency}: {match.group(1)}")
    return mismatches


def write_github_output(name: str, value: str) -> None:
    output_path = os.environ.get("GITHUB_OUTPUT")
    if not output_path:
        return
    with Path(output_path).open("a", encoding="utf-8") as output:
        output.write(f"{name}={value}\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    subparsers.add_parser("current", help="print the current workspace version")
    subparsers.add_parser("check", help="verify internal versions match")

    bump = subparsers.add_parser("bump", help="bump the workspace version")
    bump.add_argument("part", choices=("major", "minor", "patch"))
    bump.add_argument("--write", action="store_true", help="write the bumped version")

    set_parser = subparsers.add_parser("set", help="set an exact workspace version")
    set_parser.add_argument("version")
    set_parser.add_argument("--write", action="store_true", help="write the exact version")

    args = parser.parse_args()
    text = read_cargo_toml()
    current = workspace_version(text)

    if args.command == "current":
        print(current)
        write_github_output("version", current)
        return 0

    if args.command == "check":
        validate_semver(current)
        mismatches = mismatched_internal_versions(text, current)
        if mismatches:
            for mismatch in mismatches:
                print(f"version mismatch: {mismatch}", file=sys.stderr)
            return 1
        print(current)
        write_github_output("version", current)
        return 0

    if args.command == "bump":
        next_version = bump_version(current, args.part)
        if args.write:
            CARGO_TOML.write_text(set_version(text, next_version), encoding="utf-8")
        print(next_version)
        write_github_output("version", next_version)
        return 0

    if args.command == "set":
        validate_semver(args.version)
        if args.write:
            CARGO_TOML.write_text(set_version(text, args.version), encoding="utf-8")
        print(args.version)
        write_github_output("version", args.version)
        return 0

    return 1


if __name__ == "__main__":
    raise SystemExit(main())
