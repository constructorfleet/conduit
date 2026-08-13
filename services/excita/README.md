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
EXCITA_DATA_DIR=./data python -m excita.app     # or: python app.py
```

Serves on `:8084` per spec 0010. UI at `/ui/`, JSON API on the routes listed
in the scaffold's `static/index.html`.

## Tests

```sh
pytest
```
