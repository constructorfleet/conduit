# Python 3.11 because openwakeword pulls tflite-runtime<3 as a hard dep and
# tflite-runtime does not publish wheels for 3.12+ yet. Pinned deliberately
# rather than tracking the newest CPython — the runtime constraint here is
# not ours to solve.
FROM python:3.11-slim

WORKDIR /app

COPY services/excita/requirements.txt ./requirements.txt
COPY packages/conduit-link ./conduit-link
RUN pip install --no-cache-dir -r requirements.txt \
 && pip install --no-cache-dir ./conduit-link

COPY services/excita ./excita

ENV EXCITA_DATA_DIR=/data \
    EXCITA_WAKE_MODELS_DIR=/wake-models \
    EXCITA_MODEL_IMPORT_DIR=/model-import
# `/data` holds SQLite + clips (write-heavy); `/wake-models` holds the two
# shared openWakeWord ONNX files + any per-phrase classifier — read-only in
# production, populated by `scripts/fetch-wake-models.sh` or a bind mount.
# Separate volume so an image update doesn't rewrite fetched models.
# `/model-import` is the pre-trained model drop point (spec 0013): scanned
# on boot and on SIGHUP, the volume is the source of truth for anything
# mounted there (ADR-0021).
VOLUME ["/data", "/wake-models"]
EXPOSE 8084

CMD ["python", "-m", "excita.app"]
