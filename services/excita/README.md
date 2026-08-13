# Conduit Excita

Wake-word **operations service** — label, debug, train, and configure wake-word
models. Not the runtime detector; runtime detectors POST clips into Excita.
See [`docs/specs/0011-excita-wake-word-ops.md`](../../docs/specs/0011-excita-wake-word-ops.md).

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

Detection tests skip when the fetched models are missing.
