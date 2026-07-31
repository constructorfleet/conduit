# Vendored third-party ESPHome components

The `pcm5122/` and `satellite1/` directories in this folder are **third-party
code** and are **NOT** covered by the Conduit root license
(`MIT OR Apache-2.0`, see `/LICENSE-MIT` and `/LICENSE-APACHE`).

Do not apply the root dual license to these directories, and do not assume the
Apache-2.0 patent grant or the permissive MIT terms extend to their C++ files.

## Provenance

| Directory     | Upstream project                        | Ref                                        |
| ------------- | --------------------------------------- | ------------------------------------------ |
| `pcm5122/`    | `futureproofhomes/satellite1-esphome`   | `592a9687206709046f475b5464941702beacb093` |
| `satellite1/` | `futureproofhomes/satellite1-esphome`   | `592a9687206709046f475b5464941702beacb093` |

Upstream copyright is held by the FutureProofHomes and ESPHome authors.
`satellite1-esphome` adopts the ESPHome license verbatim; a verbatim copy of
that upstream license text is vendored alongside the code it covers:

- `pcm5122/LICENSE-UPSTREAM`
- `satellite1/LICENSE-UPSTREAM`

## Terms

The ESPHome license is a per-file-type split:

- **C++/runtime files** (`.c`, `.cpp`, `.h`, `.hpp`, `.tcc`, `.ino`) are
  licensed under the **GNU General Public License, version 3 (GPLv3)**.
- **Python files and everything else** are licensed under the **MIT License**.

In these directories that means:

- GPLv3: `pcm5122/*.cpp`, `pcm5122/*.h`, `satellite1/**/*.cpp`,
  `satellite1/**/*.h`
- MIT: `pcm5122/*.py`, `satellite1/**/*.py`

The authoritative terms are the vendored `LICENSE-UPSTREAM` files, not this
summary.

## Local modifications

Conduit modifies some of these GPLv3 files. GPLv3 section 5 requires modified
files to carry prominent notices of the change; the modifications are recorded
in `SAT1_OVERLAY.md` in this directory. Keep that record accurate when the
overlay changes.

## What this means for distribution

Conduit's own code is `MIT OR Apache-2.0`. The compiled ESPHome **firmware
images** for the Satellite1 target link this GPLv3 C++ code, so the resulting
binaries are subject to GPLv3 copyleft obligations (source availability,
GPLv3-compatible distribution terms) even though the Conduit sources outside
these directories are not.

This does **not** affect the Rust crates under `/crates`, which do not link any
GPLv3 code. It affects only the Satellite1 firmware artifacts.

Note: `conduit_voice/` in this folder is first-party Conduit code, not vendored,
and is covered by the root `MIT OR Apache-2.0` license. It is, however, built
as an ESPHome external component and is linked into the same GPLv3 firmware
image described above.
