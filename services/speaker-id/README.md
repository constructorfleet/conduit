# Conduit speaker identification

Conduit does not recognize voices itself. It packages an utterance and asks a
service over the contract documented on the [`conduit-speaker`](../../crates/conduit-speaker)
crate, and this is that contract implemented over SpeechBrain's ECAPA-TDNN
embeddings.

```
docker compose --profile speaker-id up
```

Then create a `http_speaker_id` Provider Definition whose `base_url` is
`http://speaker-id:8080`, and add a Speaker ID stage to a pipeline.

## The API

| Request | Body | Response |
| --- | --- | --- |
| `POST /identify` | `audio/wav` or `audio/flac` | `{"speaker": "<uuid>" \| null, "confidence": 0.0–1.0}` |
| `POST /speakers/{uuid}/enroll` | `audio/wav` or `audio/flac` | `{"speaker": "<uuid>", "samples": n}` |
| `DELETE /speakers/{uuid}` | — | `204`, or `404` if nobody was enrolled |
| `GET /health` | — | `{"status": "ok", …}` |

Conduit owns the identifier and this service stores it as an opaque file name,
so a deployment can change embedding models without every speaker becoming a
stranger to the tools that check who is asking.

Enrolling the same speaker again adds a sample rather than replacing one;
identification compares against the mean of everything they enrolled. Three or
four utterances from different sittings makes a better print than one long one.

## Configuration

| Variable | Default | Description |
| --- | --- | --- |
| `SPEAKER_ID_ENGINE` | `speechbrain` | Which embedding backend to load. |
| `SPEAKER_ID_MODEL` | `speechbrain/spkrec-ecapa-voxceleb` | Model the engine loads. |
| `SPEAKER_ID_DEVICE` | `cpu` | `cuda` on the GPU image. Startup fails loudly if CUDA was asked for and is not there, because a GPU image silently running on the CPU is a deployment that looks fine and is far slower. |
| `SPEAKER_ID_DATA_DIR` | `/data` | Voice prints, one `.npy` per speaker. |
| `SPEAKER_ID_MODEL_DIR` | `/models` | Model cache, so a restart does not re-download it. |
| `SPEAKER_ID_API_KEY` | unset | When set, every route except `/health` needs `Authorization: Bearer …`. |

The model downloads on the first request rather than at startup, so the
container becomes healthy immediately and a first identification is slow.

## Images

One Dockerfile builds both tags, because two would drift and only one of them
would be tested.

```
# CPU
docker build -t conduit-speaker-id:speechbrain services/speaker-id

# GPU
docker build -t conduit-speaker-id:speechbrain-gpu \
  --build-arg BASE_IMAGE=nvidia/cuda:12.6.3-runtime-ubuntu24.04 \
  --build-arg TORCH_INDEX=https://download.pytorch.org/whl/cu126 \
  --build-arg DEVICE=cuda \
  services/speaker-id
```

The CPU image is about 1.8 GB, most of which is torch. The GPU image is
substantially larger and needs a container runtime with GPU access.

## Choosing a threshold

This service reports how close the nearest voice print was and never decides
who is speaking. Conduit applies `threshold_percent` from the Provider
Definition, so two deployments sharing one service can disagree about how sure
they want to be before a voice unlocks a door.

**The 50% default is a starting point, not a calibrated value.** ECAPA cosine
similarities depend on your microphones, your room, and how much audio each
turn captures. Tune it against your own voices: Conduit publishes every
`SpeakerIdentified` event with its confidence, including the ones that matched
nobody, so a few turns from each household member will show you where the two
populations separate.

## Adding an engine

`build_encoder` in [`app.py`](app.py) is the whole seam. A pyannote or NeMo
backend is a class with an `embed` method and an entry in that function; the
routes, the contract, and everything Conduit knows about are unchanged.
Conduit's `engine` field is an open string, so a new backend needs no change
there either.

Voice prints record the dimensions they were built with. Pointing a store built
by one encoder at another refuses the enrollment with a `409` naming the
mismatch rather than scoring a comparison that means nothing.

## Tests

The tests inject their own encoder, so they neither download a model nor
install torch:

```
python -m venv .venv && .venv/bin/pip install -r requirements-dev.txt
.venv/bin/python -m pytest
```
