# Conduit Vox

Conduit Vox is the reference speaker identification service. Conduit does not
recognize voices itself: it packages an utterance and asks a service over the
contract documented on the [`conduit-speaker`](../../crates/conduit-speaker)
crate, and Vox is that contract implemented over a swappable embedding model —
SpeechBrain's ECAPA-TDNN, pyannote's x-vectors, or NVIDIA NeMo's TitaNet.

Vox identifies; it does not diarize. "Who is speaking, out of the people I
know" and "how many people are in this recording and when did each talk" are
different questions, and only the first one has a stage in a Conduit pipeline.

The name is new, the capability is not: this used to be `services/vox`
and shipped as `conduit-speaker-id`. The `--profile speaker-id`, the
`http://speaker-id:8080` DNS name, and the `CONDUIT_SPEAKER_ID_IMAGE` variable
are all still honoured so an in-place upgrade keeps working; new deployments
should prefer the `vox` names.

```
docker compose --profile vox up
```

Then either:

- open Vox's UI at `http://vox:8080/ui` (or through the Conduit Console's
  **Vox** section) and click **Link to Conduit** — the two exchange keys and
  the provider definition is created for you; **or**
- create a `http_speaker_id` Provider Definition whose `base_url` is
  `http://vox:8080` by hand, and add a Speaker ID stage to a pipeline.

## The UI

Vox ships a small management UI at `/ui` (with `/` redirecting there). It is
a single self-contained HTML file — no build step, no external assets, one
`<script>` — so the container serves it as static content and there is
nothing extra to install on the operator's machine. Features:

- **Health** — engine, model, device, embedding width, roster count, and
  whether the encoder has been loaded yet.
- **Enrolled speakers** — inline rename via `PATCH /speakers/{uuid}`, and a
  Forget button.
- **Enroll** — browser microphone (WebAudio → WAV, mono, matching Vox's
  decoder) with a level meter and a running duration, or a file upload for
  clips captured elsewhere. Optional label saved in the same click.
- **Test identification** — records a clip and shows the closest match, its
  confidence, and the operator-set label if any.
- **Link** — posts a one-time operator token to Conduit, receives a scoped
  sync token and provider definition id, and persists the link locally.

The UI is served without authentication so an operator with the key in their
head can load the page and paste it in; every route it calls carries the
bearer token. The token, if any, is taken from `?api_key=` on load and held
only in memory for the tab's lifetime — never persisted, because a page that
put a bearer in localStorage would hand it to every other page on the origin.

## The API

| Request | Body | Response |
| --- | --- | --- |
| `POST /identify` | `audio/wav` or `audio/flac` | `{"speaker": "<uuid>" \| null, "confidence": 0.0–1.0}` |
| `POST /speakers/{uuid}/enroll` | `audio/wav` or `audio/flac` | `{"speaker": "<uuid>", "samples": n}` |
| `GET /speakers` | — | `{"speakers": [{"uuid", "label", "samples", "created_at", "updated_at"}, …]}` |
| `PATCH /speakers/{uuid}` | `{"label": "<name>" \| null}` | The updated entry; `404` if nobody was enrolled |
| `DELETE /speakers/{uuid}` | — | `204`, or `404` if nobody was enrolled |
| `GET /link` | — | Link status, redacted: `linked`, `unlinked`, or `config-managed` |
| `POST /link` | `{"conduit_url", "operator_token", "peer_name", "force"?}` | Link status. If Vox generated a local API key, it is returned once for the current UI tab. The Conduit sync token and operator token are never returned. |
| `DELETE /link` | — | `204` after best-effort Conduit revocation and local link removal |
| `GET /health` | — | `{"status": "ok", …}` |

Conduit owns the identifier and this service stores it as an opaque file name,
so a deployment can change embedding models without every speaker becoming a
stranger to the tools that check who is asking.

Vox holds an optional **label** for each speaker alongside the print. Conduit
remains the source of truth for the roster; Vox's label is a convenience for
its own UI so an operator sees "Alice" rather than a UUID. Setting a label is
`PATCH /speakers/{uuid}` with `{"label": "…"}`; clearing one is the same
request with `{"label": null}`. Labels are capped at 100 characters and never
enter Conduit unless a sync (below) writes them there.

The roster is persisted as `roster.json` alongside the `.npy` files. Missing
or corrupt manifests are rebuilt from the prints themselves on the next read,
so an in-place upgrade from a version that only wrote prints keeps every
enrolled voice — just without labels until an operator supplies them.

The Conduit link is persisted as `link.json` beside the prints with mode
`0600`. Vox refuses to read it if group or world permissions are present,
because it contains the sync token and the local API key Conduit will use.
When `SPEAKER_ID_API_KEY` is unset, linking generates that local API key and
Vox accepts it on protected routes. The generated key is returned only from the
successful `POST /link` response so the embedded UI can keep working in that
tab; `GET /link` stays redacted. When `SPEAKER_ID_API_KEY` is set, that
configured key is sent to Conduit instead and remains the only accepted key.

Enrolling the same speaker again adds a sample rather than replacing one;
identification compares against the mean of everything they enrolled. Three or
four utterances from different sittings makes a better print than one long one.

## Engines

Each image serves one engine, because an image carrying all three would pull
every framework to run one. `SPEAKER_ID_ENGINE` selects it and defaults to
whichever the image was built for.

| Engine | Default model | Licence | Access | Width |
| --- | --- | --- | --- | --- |
| `speechbrain` | `speechbrain/spkrec-ecapa-voxceleb` | Apache-2.0 | ungated | 192 |
| `pyannote` | `pyannote/embedding` | MIT weights, **gated repository** | accept the model's conditions, then supply `HF_TOKEN` | 512 |
| `nemo` | `nvidia/speakerverification_en_titanet_large` | CC-BY-4.0 | ungated, attribution required | 192 |

**No image ships weights.** Every engine downloads its model on first use into
the `/models` volume. That is not only a size decision for pyannote: an image
containing gated weights could be pulled by somebody who never accepted the
agreement those weights are behind, so the image cannot legally carry them and
the tag would be a licence violation rather than a convenience.

### pyannote: what an operator must accept

`pyannote/embedding` is a gated Hugging Face repository. Before it will
download:

1. Sign in to Hugging Face and visit
   [`pyannote/embedding`](https://huggingface.co/pyannote/embedding). Accept the
   conditions the page states — pyannote asks for your company and use case, and
   grants access to that account.
2. Create a **read** token and pass it as `HF_TOKEN` (or
   `HUGGING_FACE_HUB_TOKEN`; both are honoured, because the hub honours both).

With no token, the container refuses at load with a message naming the model
page and the variable, not a 401. With a token whose account has not been
granted the model, the failure says that instead — the two are different
operator actions and a bare 401 distinguishes neither. The token is never
logged.

### NeMo: what attribution means

TitaNet-Large is CC-BY-4.0 and needs no token. CC-BY does require attribution,
so a product that identifies speakers with it should credit NVIDIA's model in
whatever notices it already ships.

### Widths, and why they are in this table

A voice print records the dimensions it was built with, and comparing a
512-dimension print to a 192-dimension voice is not a low score — it is
nonsense. Pointing a `pyannote` store at a `speechbrain` or `nemo` image is
refused with a `409` naming the mismatch.

**SpeechBrain and NeMo are both 192 dimensions, so that guard cannot catch a
swap between them.** The store will compare them, and the comparison will be
wrong: two models that agree on a vector length do not agree on what its
directions mean. Swapping between those two means re-enrolling everybody, and
nothing in this service will tell you that you did not.

## Configuration

| Variable | Default | Description |
| --- | --- | --- |
| `SPEAKER_ID_ENGINE` | the engine the image was built for | Which embedding backend to load: `speechbrain`, `pyannote`, or `nemo`. |
| `HF_TOKEN` | unset | Hugging Face read token. Required by `pyannote`, unused by the others. Never logged. |
| `SPEAKER_ID_MODEL` | the engine's default, above | Model the engine loads. A model whose embeddings are not the width its engine declares is refused at load, because the alternative is confident wrong matches. |
| `SPEAKER_ID_DEVICE` | `cpu` | `cuda` on the GPU image. Startup fails loudly if CUDA was asked for and is not there, because a GPU image silently running on the CPU is a deployment that looks fine and is far slower. |
| `SPEAKER_ID_DATA_DIR` | `/data` | Voice prints, one `.npy` per speaker. |
| `SPEAKER_ID_MODEL_DIR` | `/models` | Model cache, so a restart does not re-download it. |
| `SPEAKER_ID_API_KEY` | unset | When set, every route except `/health` needs `Authorization: Bearer …`. |
| `SPEAKER_ID_BASE_URL` | request base URL | Base URL Conduit should store for reaching Vox when linked. In Docker Compose this should be `http://vox:8080`, not the operator's browser URL. |

The model downloads on the first request rather than at startup, so the
container becomes healthy immediately and a first identification is slow.

## Images

One Dockerfile builds every tag, because one per engine would drift and only
one of them would be tested. Two dimensions vary: `ENGINE`, and the
CPU/GPU triple that was already there.

```
# CPU
docker build -t conduit-vox:speechbrain services/vox
docker build -t conduit-vox:pyannote --build-arg ENGINE=pyannote services/vox
docker build -t conduit-vox:nemo     --build-arg ENGINE=nemo     services/vox

# GPU — the same three, plus the CUDA triple
docker build -t conduit-vox:speechbrain-gpu \
  --build-arg BASE_IMAGE=nvidia/cuda:12.6.3-runtime-ubuntu24.04 \
  --build-arg TORCH_INDEX=https://download.pytorch.org/whl/cu126 \
  --build-arg DEVICE=cuda \
  services/vox
```

The CPU images pull CPU torch wheels from torch's own index, which is what keeps
them from silently becoming multi-gigabyte CUDA images. The SpeechBrain CPU
image is about 1.8 GB, most of which is torch; the pyannote and NeMo images are
larger, NeMo substantially so. The GPU images are larger again and need a
container runtime with GPU access.

## Speaker identification is remote only

All three engines are Python and want more memory than an ESP32 has, so there is
no on-device counterpart to a wake definition's `device` runtime. More engines
does not change this: a satellite can wake itself; it cannot tell who woke it.

## Choosing a threshold

This service reports how close the nearest voice print was and never decides
who is speaking. Conduit applies `threshold_percent` from the Provider
Definition, so two deployments sharing one service can disagree about how sure
they want to be before a voice unlocks a door.

**The 50% default is a starting point, not a calibrated value.** Cosine
similarities depend on your microphones, your room, and how much audio each
turn captures.

**They also depend on the engine.** Each of the three has its own similarity
distribution — a threshold tuned against ECAPA is not a threshold tuned against
TitaNet or pyannote, and carrying one over after an engine swap is how a
deployment starts making confident wrong matches. Re-tune after changing
engines, the same way you re-enrol.

Tune it against your own voices: Conduit publishes every
`SpeakerIdentified` event with its confidence, including the ones that matched
nobody, so a few turns from each household member will show you where the two
populations separate.

## Adding an engine

`ENGINE_CLASSES` and `build_encoder` in [`app.py`](app.py) are the whole seam. A
backend is a class with an `embed` method and a declared `width`, an entry in
that table, a default model in `DEFAULT_MODELS`, a width in `EMBEDDING_WIDTHS`,
and a `requirements-<engine>.txt` the Dockerfile's `ENGINE` argument selects.
The routes and the contract are unchanged.

Conduit's side is *not* an open string: `SpeakerEngine` in
`conduit-provider` is a closed enum, currently `speechbrain`, `resemblyzer`, and
`pyannote`. So `pyannote` is already selectable from a provider definition and
`nemo` is not — reaching it needs a variant added to that enum, and until then
`nemo` is reachable only by setting `SPEAKER_ID_ENGINE` on the service directly.

Voice prints record the dimensions they were built with. Pointing a store built
by one encoder at another of a different width refuses the enrollment with a
`409` naming the mismatch rather than scoring a comparison that means nothing —
see the caveat above about the two engines that share a width.

## Tests

The tests inject their own encoder, so they neither download a model nor
install torch:

```
python -m venv .venv && .venv/bin/pip install -r requirements-dev.txt
.venv/bin/python -m pytest
```
