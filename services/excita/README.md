# Conduit Excita

Wake-word **operations service** — label, debug, train, and configure wake-word
models. Not the runtime detector; runtime detectors POST clips into Excita.
See [`docs/specs/0011-excita-wake-word-ops.md`](../../docs/specs/0011-excita-wake-word-ops.md)
and [issue #213](https://github.com/constructorfleet/conduit/issues/213) for the
microWakeWord / nanoWakeWord extension.

## Engines

Each engine honestly declares which capabilities it implements
([ADR-0020](../../docs/adr/0020-wake-engine-adapters-are-partial.md));
`GET /engines` returns the matrix, and asking for a missing capability
returns a structured `501` with `{code: "engine_capability_missing", engine,
capability, message}` ([ADR-0023](../../docs/adr/0023-engine-capability-gaps-return-501.md)):

| Engine         | load/feed | score | train | package       |
|----------------|-----------|-------|-------|---------------|
| openWakeWord   | yes¹      | yes   | —     | `onnx`        |
| nanoWakeWord   | yes       | yes   | —²    | `onnx`        |
| microWakeWord  | —³        | yes   | —²    | `tflite_micro`|
| Porcupine      | adapter not landed yet (all gaps → 501)   |

¹ Requires the shared ONNX models (`scripts/fetch-wake-models.sh`); otherwise a null slot answers 501.
² Training waits on the `EXCITA_TRAIN_WORKER_URL` worker protocol (future spec).
³ microWakeWord detects on the ESP32; Excita scores stored clips offline and packages for flash.

Phrases are engine-agnostic ([ADR-0022](../../docs/adr/0022-phrase-is-engine-agnostic.md)):
one "hey jarvis" carries models across engines, so cross-engine comparison is
one phrase row with several model rows.

## Model import

Two paths land models in Excita:

- **Upload**: `POST /models/import` (multipart artifact + JSON `metadata`
  form field: `engine`, `phrase_name`, `version`, optional
  `engine_phrase_key` / `metrics_json` / `notes`). Unknown phrases are
  created; an `.excita.json` sidecar is written next to the stored artifact.
- **Filesystem drop**: bind-mount a directory at `EXCITA_MODEL_IMPORT_DIR`.
  Scanned on boot and on `SIGHUP`; `POST /models/scan` triggers a scan too.
  Each `<artifact>.excita.json` sidecar describes its artifact. The volume
  is the source of truth: new files appear, removed files disappear,
  sidecar `version` bumps mint new model rows (history preserved), and
  re-saving without a bump changes nothing.
  [ADR-0021](../../docs/adr/0021-filesystem-imported-models-are-read-only-in-ui.md):
  filesystem-imported models cannot be deleted through the API (`409`,
  `code: "filesystem_imported_read_only"`) — remove the file instead.

## Deploy targets

Three transports (`file`, `http_push`, `linked_service_config`) work for every
engine's native package. Publishing is one call:
`POST /deploy_targets/{id}/publish {"model_id": ...}` sets the target's current
model and pushes; a failed push never rolls back the selection (retry the same
call). `http_push` sends headers `X-Excita-Engine`, `X-Excita-Phrase`,
`X-Excita-Version`.

## Run locally

```sh
cd services/excita
python -m venv .venv && . .venv/bin/activate
pip install -r requirements-dev.txt
pip install -e ../../packages/conduit-link

# One-time: fetch the shared openWakeWord ONNX models. Without them the
# openwakeword engine stays a NullEngine and detection endpoints return 501.
../../scripts/fetch-wake-models.sh ./wake-models

EXCITA_DATA_DIR=./data EXCITA_WAKE_MODELS_DIR=./wake-models \
  python -m excita.app
```

Serves on `:8084` per spec 0010. UI at `/ui/`, JSON API on the routes listed
in `static/index.html`.

## Tests

```sh
../../scripts/fetch-wake-models.sh   # once; puts models where tests look
PYTHONPATH=.. pytest
```

Detection tests skip when the fetched models are missing. The µWW / nanoWakeWord
adapter tests skip unless their artifacts are dropped next to the openWakeWord
ones (`hey_jarvis_v0.1.tflite`, `hey_jarvis_v0.1.nww.onnx`).

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `EXCITA_DATA_DIR` | `/data` | SQLite, clips, uploaded model artifacts |
| `EXCITA_BACKEND` | `sqlite` | Backend type |
| `EXCITA_BASE_URL` | `http://localhost:8084` | Advertised link URL |
| `EXCITA_WAKE_MODELS_DIR` | `<data>/wake-models` | Shared openWakeWord ONNX files |
| `EXCITA_MODEL_IMPORT_DIR` | unset | Filesystem model-import mount (scanner) |
| `EXCITA_PREROLL_MS` | `2000` | Per-source pre-roll ring buffer |
| `EXCITA_TRAIN_WORKER_URL` | referenced in 501 messages | Training worker (future spec) |
