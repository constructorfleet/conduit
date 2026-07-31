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

The copied upstream code follows the ESPHome license split: C++ runtime files
are GPLv3, Python component files are MIT.
