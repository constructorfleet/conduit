# 0011 — Excita: wake-word service

Draft. Excita is Conduit's **wake-word service**: one process that both **runs wake-word detection** on live audio and provides the **ops plane** (labelling, debugging, training, configuring) that makes the models it runs better over time. Ops and detection live together on purpose — they share the engine adapters, they share the model store, and unifying them is what closes the data loop without an operator shipping files by scp.

Other runtimes still exist (openWakeWord baked into a satellite, microWakeWord on an ESPHome device, `crates/conduit-wake` in Conduit itself). Excita is one of them; it is *also* the tool that trains their models and, via a shared engine-agnostic package format, can publish updates to them.

Anchors: [0005](0005-link-protocol.md) (link protocol), [0007](0007-excita-wake-events-side-channel.md) (`excita.wake-events` — Excita **is** the sender when it is the detector), [0010](0010-linked-service-lifecycle-and-dev.md) (linked-service lifecycle), [0004](0004-embedded-service-visual-consistency.md) (embedded panel visual consistency). Reference implementation shape: `services/instrumenta/`.

---

## Purpose

Wake-word detection quality is a **data-loop problem**, not an algorithms problem. A device fires or misses; a human decides whether it was right; the labelled clip enters a training set; a new model ships; the loop repeats. Today the only place that loop can happen is inside whatever process runs the detector — which means it doesn't happen at all in Conduit, and every operator ends up hand-editing YAML and shipping model files by scp.

Excita closes that loop by being **both ends of it** in one process:

1. **Detect** — accept live audio (streaming from a satellite or an in-Conduit stage), score it against loaded phrase models, and emit wake events via the [0007](0007-excita-wake-events-side-channel.md) `excita.wake-events` side channel to Conduit. Excita is a first-class runtime detector.
2. **Ingest** — the same detection path retains a pre-roll clip on every fire/near-miss so it lands in the labelling queue automatically. Operators can also upload a WAV/OGG or record in-browser.
3. **Label** clips as positive / negative / ambiguous / discard, tagged with the phrase they were meant for.
4. **Debug** by replaying stored audio against any registered engine adapter and inspecting the score curve — "why did this fire?" / "why didn't this fire?" Same code path as (1), just against a stored clip instead of a live frame.
5. **Train / fine-tune** a model against the current labelled set, per engine.
6. **Configure & publish** which phrase, which model version, and which thresholds are active — for Excita's own detection loop, and for other runtimes (satellite firmware, `crates/conduit-wake`) that pull from Excita's deploy targets.

Because detection and ops share the process, a fire produces its own training candidate with no cross-service glue. That is the whole point.

## Non-goals

- Being a general audio-labeling tool. Wake words only; scope creep to STT/intent labelling belongs elsewhere.
- Replacing engine-specific training tooling. Excita orchestrates each engine's own training entry point; it does not reimplement openWakeWord's optimiser.
- Model marketplace / sharing across Conduit instances. Local loop only in this revision.
- Replacing on-device detectors where their latency or power profile matters (microWakeWord on ESP32). Excita hosts the same model type so it can publish updates to those devices; it doesn't require every deployment to route audio to Excita.

## Engine abstraction

Three engines matter today: **openWakeWord**, **microWakeWord**, **Porcupine**. They differ enough that a single interface would be a leaky abstraction, so Excita defines its own **normalized clip + label schema** and each engine plugs in via a narrow adapter:

```python
class WakeWordEngine(Protocol):
    kind: EngineKind                        # "openwakeword" | "microwakeword" | "porcupine"

    def load(self, model_ref: ModelRef) -> Detector: ...
    def score(self, clip: Clip, model_ref: ModelRef) -> ScoreCurve: ...
    def train(self, dataset: Dataset, base: ModelRef | None) -> TrainJob: ...
    def package(self, model_ref: ModelRef, target: DeployTarget) -> Bundle: ...


class Detector(Protocol):
    """Warm, per-model handle used by the live detection loop."""
    kind: EngineKind
    phrase_id: str
    sample_rate: int

    def feed(self, pcm_frame: bytes) -> Detection | None: ...
    def reset(self) -> None: ...
```

- `load` returns a `Detector` — a warm instance with model weights held in memory, ready to consume frames. The runtime loop calls `feed(pcm_frame)` per audio chunk and gets back either `None` (no wake) or a `Detection` (confidence, timestamp, pre-roll ref). Cold-loading per frame would be untenable for real audio; the runtime pins one `Detector` per active `(phrase, model)` pair.
- `score` is the offline equivalent for the debug view — same math, but over a stored clip, returning the full per-frame score curve rather than a single fire/no-fire.
- `train` returns a job handle; each adapter shells out to the engine's own trainer. Porcupine's adapter returns a `NotSupported` job with a link to Picovoice Console — an honest gap, not a stub.
- `package` turns a trained model into whatever the runtime expects: ONNX for openWakeWord, TFLite-micro for microWakeWord, PPN blob for Porcupine.

Adapters live at `services/excita/engines/<kind>.py`. Adding a fourth engine is one file plus one enum entry — no schema migrations, no UI change.

## Data model

Persisted in SQLite for the standalone case, per the Instrumenta pattern (`services/instrumenta/backend.py`). Postgres backend is a follow-on.

- **`phrase`** — canonical name of a wake phrase (e.g. `"hey jarvis"`), plus display label, language, notes. Everything else keys off phrase id.
- **`clip`** — one audio file. Fields: id, phrase id, sample rate, duration_ms, source (`detector` | `upload` | `browser`), source_peer (nullable — set when a linked detector POSTs it), source_ref (opaque; matches [0007's](0007-excita-wake-events-side-channel.md) `audio_clip_ref` when the detector supplied one), sha256 (dedup), created_at. Raw bytes live on disk at `<data_dir>/clips/<sha256>.<ext>`; the row holds the path.
- **`label`** — labelling verdict on a clip. Fields: clip id, verdict (`positive` | `negative` | `ambiguous` | `discard`), labeller (free-text; operator identity), split (`train` | `val` | `test`; nullable until assigned), notes, labelled_at. **One active label per (clip, labeller)** — re-labelling supersedes; history is kept in `label_history`.
- **`model`** — a trained model. Fields: id, phrase id, engine (`openwakeword` | `microwakeword` | `porcupine`), version, trained_from (dataset snapshot id), artifact_path, metrics_json (per-engine eval numbers), created_at, notes.
- **`dataset_snapshot`** — the exact set of clips + labels a `model` was trained from. Immutable. Storing the snapshot is what makes "why did model v3 regress" answerable.
- **`deploy_target`** — where a model gets published: kind (`file` | `http_push` | `linked_service_config`), config (JSON), current_model_id. A "publish" is `deploy_target.current_model_id = <model_id>`; the target's poller picks it up.

Deletes are soft (`deleted_at` column) except for `clip` raw bytes, which are hard-deleted from disk when their row's `deleted_at` is older than a configurable retention (default 30 days). Rationale: audio is heavy and often accidentally contains real speech; a soft-delete that keeps the WAV forever is a privacy footgun.

## Runtime detection loop

Excita runs a **detector supervisor** that owns one `Detector` instance per active `(phrase, model)` pair and dispatches every incoming audio frame to all of them in parallel (a wake phrase and its "hey X" variant can both be armed simultaneously). Active bindings are rows in `deploy_target` with `kind = "excita_local"` — publishing to that target is what arms a detector in-process.

### Audio ingress

Two transports, chosen at bind time per `deploy_target`:

- **HTTP frame POST** (`POST /v1/audio/{source_device}/frames`, bearer-authed) — 16-bit 16 kHz PCM in `application/octet-stream`, one HTTP request per ~20–40 ms frame. Trivially compatible with any satellite that can `curl`; the natural default.
- **WebSocket stream** (`WS /v1/audio/{source_device}`, bearer-authed) — same PCM shape, one binary message per frame, kept open for the session's lifetime. Chosen when the source device's per-frame HTTP overhead dominates its budget.

Both carry the same PCM contract; the choice is deployment ergonomics, not protocol. Adding a third transport later (Wyoming, gRPC-stream) is one supervisor entry.

`source_device` is the same identifier the satellite reports on the [0007](0007-excita-wake-events-side-channel.md) event — one axis, one meaning across the loop.

### Fire → wake event

On a `Detector.feed()` returning a `Detection`, the supervisor:

1. Writes the pre-roll clip (last ~2 s of PCM buffered per source) to disk as a `clip` row with `source = "detector"` and a fresh `audio_clip_ref`.
2. POSTs to the Conduit `POST /v1/wake-events` endpoint per [0007](0007-excita-wake-events-side-channel.md), using Excita's Conduit-side `sync_token`. The `audio_clip_ref` on the event points at the clip row.
3. The clip lands in the labelling inbox automatically — no operator action to close the loop.

A near-miss (score peaked above a lower `retain_threshold` but under `fire_threshold`) writes the clip with `source = "detector"` and `pre_verdict = "rejected"`; no wake event fires, but the operator still sees it for training.

### Detector lifecycle

Detectors are loaded lazily on first frame for a bound `(phrase, model)` and unloaded after an idle window (default 5 min) to free memory. Reload latency is what it is per engine — this is a memory-pressure choice, not a correctness one. Publishing a new model to an `excita_local` target atomically swaps the `Detector` for that binding on the next frame; the old instance drains and is dropped.

### Standalone posture

Detection works with **zero Conduit reachable**. When Conduit is unlinked, fire events go to a local ring buffer (`GET /v1/wake-events/recent`, up to 256 entries) — this is the debug view for a satellite operator wiring Excita up before touching Conduit at all. Once the link is established, backlog is not replayed to Conduit (the events reference real-time turns, not history); the ring buffer stays for local diagnostics.

## Ingest paths

### Detector push

A runtime detector paired with an Excita instance POSTs a clip alongside its [0007](0007-excita-wake-events-side-channel.md) wake event. This is a **new side-channel** in the [0005](0005-link-protocol.md) sense, running Conduit-agnostic (peer→Excita, not peer→Conduit): `POST /v1/clips` on Excita, multipart body with the audio and a JSON part naming the phrase, the detector's own verdict (`detected` | `rejected`), and the `audio_clip_ref` echoed back into Conduit's wake-event stream so the two can be joined. Auth is a **peer bearer** (Excita's own `sync_token`, symmetric to what Conduit uses — Excita is a linked service in its own right; a detector may be linked to Excita even when it is not linked to Conduit). This channel is defined here at the schema level; a follow-up spec (`0012-excita-clip-ingest-side-channel.md`) covers the retry/idempotency/timeout details in the same shape as [0007](0007-excita-wake-events-side-channel.md).

### Operator upload

`POST /clips` (no `/v1/` prefix; UI-facing surface) with a WAV/OGG file. Excita sniffs the format, refuses anything that isn't PCM WAV or Opus/OGG, computes sha256, and dedups against existing clips for the same phrase. Response includes the clip id and a URL to fetch the audio for playback in the labelling UI.

### Browser record

Same endpoint as upload; the frontend just captures a Blob via `MediaRecorder` and POSTs it. No server-side distinction — the `source` column is set from a header (`X-Excita-Source: browser`) so the debug view can surface "how was this captured" without hunting through logs.

## Labelling workflow

The core UI loop is a **clip inbox → verdict → next clip** flow. An operator picks a phrase, gets the queue of unlabeled clips in reverse-chronological order, plays each one, and presses one of four keys:

- `p` — positive
- `n` — negative
- `a` — ambiguous (keep for review, don't train on)
- `d` — discard (won't count against any model)

The verdict is written via `POST /clips/{id}/label`. The next clip loads immediately. Split assignment (`train`/`val`/`test`) defaults to a stable per-clip hash so re-labelling doesn't shuffle a clip between splits and pollute a hold-out set — the operator can override.

## Debug workflow

Any clip can be re-scored against any registered model via `POST /debug/score` with a `clip_id` and `model_id`. The response is the `ScoreCurve` from the engine adapter — a per-frame array of confidence values plus threshold(s). The UI renders it as a waveform with the score curve overlaid; this is the "why did it fire" view.

The same endpoint accepts `model_id: null` to run against every model registered for the clip's phrase, so a regression check is one call.

## Training workflow

`POST /train` with a phrase id, an engine kind, and optional `base_model_id`:

1. Excita snapshots the current labelled dataset for the phrase (`dataset_snapshot` row).
2. Adapter's `train()` runs — this is the long-running part. Job state (`queued` | `running` | `succeeded` | `failed`) is polled at `GET /train/{job_id}`.
3. On success, a `model` row is written, artifact stored under `<data_dir>/models/<engine>/<version>/`, metrics computed against the `val` split.

The trainer runs in the same process as the API for v1 (a single worker + a `asyncio.Queue`). Splitting into a separate worker is a scaling problem for later — Excita is one-operator-at-a-time.

## Configure / publish workflow

A `deploy_target` describes a destination. Four kinds:

- **`excita_local`** — bind the model to Excita's own detector supervisor. Publishing arms detection in-process for a `(phrase, model, source_device)` tuple; unpublishing disarms it. This is the target Excita uses on itself.
- **`file`** — write the packaged model bundle to a directory. For a satellite that mounts a shared volume.
- **`http_push`** — POST the bundle to a URL. For a Conduit-linked detector service that has a `PUT /wake-word/model` endpoint.
- **`linked_service_config`** — update a config row on a peer via the [0005](0005-link-protocol.md) config side-channel (defined per-peer; Excita just carries the payload).

Publishing is `POST /deploy_targets/{id}/publish` with a `model_id`. The write is transactional against SQLite; the actual push (for `http_push` and `linked_service_config`) happens on a background task and its outcome is visible on the target's status endpoint. **A failed publish does not roll back the DB row** — the operator sees "model v4 selected, last push failed, retry" and can retry with the same call. Same reasoning as [0007's](0007-excita-wake-events-side-channel.md) at-least-once: silently reverting hides intent.

## HTTP API summary

```
GET    /health                    — link-health per 0010 (200 iff listening)
GET    /ready                     — richer readiness (DB open, adapters loaded)
POST   /link                      — per 0005/0010 (via conduit-link's make_link_router)

GET    /phrases                   — list
POST   /phrases                   — create
DELETE /phrases/{id}              — soft-delete

GET    /clips?phrase_id=&label=   — list (paged)
POST   /clips                     — upload (multipart) or browser record
GET    /clips/{id}                — metadata
GET    /clips/{id}/audio          — raw bytes for playback
POST   /clips/{id}/label          — assign verdict
DELETE /clips/{id}                — soft-delete

POST   /v1/clips                  — detector push (peer-bearer auth; see follow-up 0012)
POST   /v1/audio/{source}/frames  — live PCM frame (peer-bearer auth)
WS     /v1/audio/{source}         — live PCM stream (peer-bearer auth)
GET    /v1/wake-events/recent     — local ring buffer of recent fires (standalone diag)
GET    /detectors                 — armed (phrase, model, source) bindings
POST   /detectors/{id}/reset      — drop internal state (e.g. VAD/pre-roll)

GET    /models?phrase_id=         — list
POST   /debug/score               — score clip(s) against model(s)

POST   /train                     — start job
GET    /train/{job_id}            — poll status
GET    /train                     — list recent jobs

GET    /deploy_targets            — list
POST   /deploy_targets            — create
POST   /deploy_targets/{id}/publish
GET    /deploy_targets/{id}/status
```

Everything under `/v1/` is peer-facing (bearer-authed side channels); everything else is UI-facing (session/operator-authed). Same split Instrumenta uses for `/mcp` vs `/servers`.

## UI shape

Embedded panel served at `/ui/` (per [0004](0004-embedded-service-visual-consistency.md) — component library from `@constructorfleet/ui`, dark/light respected, no external requests). Five tabs:

1. **Live** — currently armed detectors, per-source frame rate, last fires, running-score sparklines. First tab because a mis-wired audio ingress is the loudest failure mode and belongs on the landing page.
2. **Inbox** — labelling queue with keyboard-first workflow.
3. **Clips** — filterable list, search, replay, re-label.
4. **Models** — per-phrase model history, metrics, "make active", debug-score any clip.
5. **Deploy** — targets and their current model + last-publish status (including `excita_local` bindings).

Home (`/ui/`) is **Live**. A new operator with no clips lands on an empty state that explains the two ways audio arrives (frame POST vs WebSocket) and how to arm the first detector.

## Configuration

Env vars (mirroring Instrumenta's `Config.from_env`):

- `EXCITA_DATA_DIR` (default `/data`) — clips, models, SQLite live under here.
- `EXCITA_BACKEND` (default `sqlite`) — `sqlite` for v1; `postgres` reserved.
- `EXCITA_BASE_URL` (default `http://localhost:8084`) — reachability probe target.
- `EXCITA_SECRET_KEY` — Fernet key for any peer bearers Excita stores (detector push + audio ingress).
- `EXCITA_PREROLL_MS` (default `2000`) — pre-roll retained per source for the fire clip.
- `EXCITA_DETECTOR_IDLE_MS` (default `300000`) — unload a `Detector` after this many ms with no frames.
- `CONDUIT_LINK_URL` — per [0010](0010-linked-service-lifecycle-and-dev.md); default for the operator's link form.

Port `8084` is Excita's slot per [0010 §Local URLs](0010-linked-service-lifecycle-and-dev.md#local-urls-dev-vs-prod-handshake).

## Standalone posture

Per [0005 §Standalone posture](0005-link-protocol.md#standalone-posture), Excita boots, accepts audio, runs detection, and serves the full UI with zero Conduit reachable. The Conduit link is opt-in: it exists so wake events reach Conduit's pipeline (via [0007](0007-excita-wake-events-side-channel.md)) and so Conduit's operator console can surface an "Open Excita" tile. Nothing in the detect / label / train / publish loop requires Conduit — an unlinked Excita's fires accumulate in the local ring buffer described in §Runtime detection loop.

## Implementation scope (this spec's follow-up work)

Delivered in the scaffold PR:

- `services/excita/` (Python + FastAPI, Instrumenta's shape).
- Data model as SQLite migrations; abstract `Backend` protocol.
- Engine adapter Protocol (`load`/`score`/`train`/`package`) + `Detector` Protocol + a **null adapter** ("`<op>` not implemented for `<kind>`") so the API surface can be exercised end-to-end without any engine installed.
- Ops workflow end-to-end: **upload clip → label → list filtered by label**.
- Detection surface stubs: `POST /v1/audio/{source}/frames`, `GET /detectors`, `GET /v1/wake-events/recent` — all wired to the null engine so armed detectors respond with a `NotSupported` error rather than silently dropping frames.

Follow-up PRs (each its own review):

- Real openWakeWord adapter (`load` + `feed` + `score` + basic `train`). Motivation: it's the only engine where the training loop is realistically in-process on a laptop.
- Detector supervisor: pre-roll buffer, `excita_local` deploy target, wake-event POST per [0007](0007-excita-wake-events-side-channel.md).
- WebSocket audio ingress (`WS /v1/audio/{source}`).
- microWakeWord adapter (`package` for TFLite-micro; detection likely stays on-device).
- Porcupine adapter (`load` + `feed`; `train` → `NotSupported` with Picovoice Console link).
- Follow-up specs: `0012-excita-clip-ingest-side-channel.md` (detector push details), `0013-excita-audio-ingress-side-channel.md` (frame POST + WebSocket contract), `0014-excita-deploy-side-channel.md` (linked-service config push).

## Open questions

- **Multi-operator labelling.** The schema supports `(clip, labeller)` but the UI is single-operator for v1. Do we need reconciliation UX when two operators disagree? Deferred until a second operator exists.
- **Clip retention beyond the delete window.** Legal-hold on a clip an operator wants to keep forever? Add a `pinned` boolean on `clip` when this comes up.
- **Training compute.** In-process is fine for openWakeWord on a laptop. microWakeWord's TF training will not be. Escape hatch is `EXCITA_TRAIN_WORKER_URL` — punt to an external worker if set — but not built until asked.
