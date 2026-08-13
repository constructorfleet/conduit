# Conduit Excita

Wake-word **operations service** — labelling, debugging, training, and
configuring wake-word models. Runs standalone or integrated with Conduit as a
linked service (spec 0005). Not itself a runtime wake-word detector; runtime
detectors POST clips into Excita and consume the trained models it publishes.

- Spec: [`specs/0011-excita-wake-word-ops.md`](specs/0011-excita-wake-word-ops.md)
- Wake-event side channel (detector → Conduit): [`specs/0007-excita-wake-events-side-channel.md`](specs/0007-excita-wake-events-side-channel.md)
- Service: [`services/excita/`](../services/excita)
