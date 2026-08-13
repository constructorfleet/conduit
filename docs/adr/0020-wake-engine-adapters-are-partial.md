# Wake Engine Adapters Are Partial

Excita's engine abstraction (spec [0011](../specs/0011-excita-wake-word-ops.md))
declares four operations — `load`/`feed`, `score`, `train`, `package` — but
**no engine adapter is required to implement all four**. Each adapter
advertises the subset of capabilities that make sense for its runtime and
returns `NotSupportedError` for the rest. The HTTP surface treats an absent
capability as an honest gap and reports it to the operator, rather than a
missing method or a silent stub.

Concretely, when microWakeWord and nanoWakeWord land, the capability matrix
looks like:

| Engine         | load/feed | score | train | package |
|----------------|-----------|-------|-------|---------|
| openWakeWord   | ✅        | ✅    | ✅    | ✅      |
| nanoWakeWord   | ✅        | ✅    | ⛔    | ✅      |
| microWakeWord  | ⛔        | ✅    | ⛔    | ✅      |
| Porcupine      | ✅        | ✅    | ⛔    | ✅      |

**microWakeWord has no live host-side detection.** microWakeWord exists to
run on the ESP32; its whole point is the streaming TFLite-Micro inference
that fits in an MCU's memory budget. Running the same model on a Python
host through `tensorflow-lite` would work, but there is no Conduit use case
for it — the device is where µWW detects. Excita's µWW adapter therefore
implements `score` (offline curves on stored clips, for regression testing
new models against old ones inside Excita's own UI), `package` (produce the
TFLite-Micro blob for OTA), and returns `NotSupportedError` for `load` and
`feed`. This keeps `tflite-runtime` out of Excita's hot path — `score` uses
it on demand — and it keeps the abstraction honest: an operator who tries
to arm a live µWW detector inside Excita is told plainly that µWW detects
on-device.

**Training is `NotSupported` for µWW and nanoWakeWord until a training
worker exists.** In-process training is fine for openWakeWord on a laptop.
It is not fine for microWakeWord's TF pipeline or nanoWakeWord's
multi-architecture trainer, both of which are wrong to run on the same
Python process serving Excita's HTTP surface. The escape hatch flagged in
spec 0011 — `EXCITA_TRAIN_WORKER_URL` — is a spec of its own (queue
semantics, artifact upload, credentials, cancellation) and shouldn't be
coupled to landing two new engines. Until that worker exists, `train` for
µWW and nanoWakeWord returns `NotSupported` with a message pointing at the
config variable. Porcupine has always done this and pointed at Picovoice
Console; µWW and nanoWakeWord follow the same pattern for the same reason.

**Alternatives considered**

- *Every engine implements every operation.* Rejected: forces stubs for
  runtimes that shouldn't have them (µWW live detection, µWW training)
  and lets operators arm configurations that don't actually work.
- *Separate Protocols per capability.* Would be cleaner in isolation but
  doubles the surface a new engine has to opt into and pushes the
  capability decision into the type system rather than the runtime, where
  the operator-visible error message needs to be. The Protocol stays flat;
  `NotSupportedError` is the shape.
- *Ship µWW and nanoWakeWord behind the training worker.* Delays value
  from a working train-elsewhere / package-here / detect-on-device µWW
  loop that operators already want, for a worker whose design decisions
  are independent.

**Consequences**

- The HTTP surface must translate `NotSupportedError` into a stable error
  code so the frontend can render "this engine doesn't do that here"
  rather than a 500.
- Deploy targets are per-engine: µWW publishes to devices; openWakeWord
  and nanoWakeWord can publish to host-side runtimes too.
- Adding a fifth engine still costs one file plus one enum entry — with
  the additional freedom to declare, in that file, exactly which
  operations the new engine supports.
