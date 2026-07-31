# Satellite1 local overlay components

The `pcm5122` and `satellite1` component folders in this directory are copied
from `futureproofhomes/satellite1-esphome` at
`592a9687206709046f475b5464941702beacb093`.

They are intentionally loaded from `source: local` after the upstream git
component source in `conduit-sat1.yaml`. ESPHome 2026.7 changed
`GPIOPin::dump_summary` from `std::string dump_summary() const` to
`size_t dump_summary(char *buffer, size_t len) const`; the upstream Satellite1
components still used the old signature at this ref, which prevents firmware
compilation.

Local changes:

- `pcm5122/pcm_gpio.h`: update `PCMGPIOPin::dump_summary` to the ESPHome
  2026.7 signature.
- `satellite1/sat_gpio.h`: update `Satellite1GPIOPin::dump_summary` to the
  ESPHome 2026.7 signature.

## Licensing

The copied upstream code follows the ESPHome license split: C++ runtime files
are GPLv3, Python component files are MIT. It is **not** covered by the Conduit
root `MIT OR Apache-2.0` license.

The local changes listed above modify GPLv3-licensed files; this section is the
record of modification required by GPLv3 section 5(a).

See:

- [`LICENSE-VENDORED.md`](./LICENSE-VENDORED.md) — provenance, terms, and what
  the GPLv3 mix means for distributing firmware images.
- [`pcm5122/LICENSE-UPSTREAM`](./pcm5122/LICENSE-UPSTREAM) and
  [`satellite1/LICENSE-UPSTREAM`](./satellite1/LICENSE-UPSTREAM) — verbatim
  upstream license text (authoritative).
- [`../../../NOTICE`](../../../NOTICE) — repository-wide license summary.
