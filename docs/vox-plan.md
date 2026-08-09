# Conduit Vox — implementation plan

This is a live plan for turning `services/speaker-id` into **Conduit Vox**: a
first-class managed service with its own UI, its own roster, and a one-click
trust-establishing link with Conduit. Each numbered section is one atomic
commit on the `vox-service` branch. Tick a section when its commit lands; add
follow-ups inline rather than in a separate doc.

## Guiding decisions

These were settled before implementation started; changing one requires
revisiting the sections it affects.

- **Rename is branding + service unit only.** Directory, Docker image
  (`conduit-vox`), compose service, README titles. Rust-side identifiers
  (`SpeakerEngine`, the `http_speaker_id` provider variant, `SPEAKER_ID_*`
  environment variables) name a **capability**, not a product — they stay.
- **Back-compat during rename.** Compose profile name `speaker-id` and the DNS
  alias `speaker-id` stay for a release cycle. `CONDUIT_SPEAKER_ID_IMAGE`
  still overrides the image when set. Data volumes (`speaker-id-data`,
  `speaker-id-models`) keep their names so an in-place upgrade does not orphan
  prints.
- **Conduit remains the source of truth for the speaker roster.** Vox has its
  own local store so it works standalone, but on link the two sync and Conduit
  wins on label conflicts. This preserves the invariant in
  `crates/conduit-api/src/speakers.rs` ("the service never holds a person's
  name" is now weakened only in the "Vox may cache the name for its own UI"
  sense).
- **Console reaches Vox through Conduit.** The browser never talks to Vox
  directly. Conduit reverse-proxies `/vox/*` to the linked Vox base_url,
  using the api_key stored at link time. Vox is not exposed to the LAN.
- **Engine swap is within-engine only.** Bundling every engine in one image
  is a licence and size problem; swapping models inside the running engine is
  cheap. UI shows the engine, lets an operator pick a different model for the
  **same** engine, and refuses a swap that would change embedding width while
  prints exist.
- **Link is bidirectional and one-click.** The Vox operator pastes a Conduit
  operator token once; Vox and Conduit exchange scoped tokens and Conduit
  provisions the `http_speaker_id` provider definition automatically.

## Repo shape after this branch

```
services/vox/                     # was services/speaker-id
  app.py                          # FastAPI + roster + link + UI mount
  static/                         # single-file HTML/CSS/JS SPA
  test_app.py
crates/conduit-api/src/
  vox.rs                          # /v1/vox/links + /vox/* reverse proxy
frontend/src/App.tsx              # Speakers section becomes an iframe of /vox/ui
```

## Commits

Each item is one commit. Do not merge until the last one lands unless the
partial state runs (compose config, tests, and the frontend build all pass on
every intermediate commit).

### 1. Rename `services/speaker-id` → `services/vox`

Directory move via `git mv`. Docker image `conduit-vox:<engine>`. Compose
service renamed to `vox` with `profiles: ["vox", "speaker-id"]` and
`aliases: [speaker-id]` under the default network. `CONDUIT_VOX_IMAGE` with
fallback to `CONDUIT_SPEAKER_ID_IMAGE`. README/docs/publish workflow refer to
Conduit Vox. `app.py` title becomes "Conduit Vox"; logger name becomes `vox`;
Dockerfile user renamed to `vox`. Tests pass unchanged.

### 2. Vox-side roster (labels alongside prints)

Add a `Roster` alongside `VoicePrints`, persisted as `roster.json` in the data
dir. Records `{uuid, label?, samples, created_at, updated_at}` per speaker.
`add`, `remove`, and `identify` update it. New endpoints:

- `GET /speakers` — list of roster entries.
- `PATCH /speakers/{uuid}` — `{label?: string}`; nullable label to clear.

The manifest is written through a temp-file rename, same pattern as the print
files. Missing manifest is not an error; it is rebuilt from the `.npy` files
on the first read (uuid + samples; label unknown until set).

### 3. Vox embedded UI

`services/vox/static/index.html` — a single file, inlined CSS and JS, no build
step. Sections:

- **Health** — engine, model, device, embedding width, model_loaded, roster
  count. Refreshed on demand.
- **Speakers** — table (label, uuid, samples, delete). Inline rename.
- **Enroll** — uuid picker (existing or new), mic recorder with duration
  guard + level meter, or file upload. Reports the resulting sample count.
- **Test identify** — mic capture, POSTs to `/identify`, shows the label +
  confidence.
- **Link** — status card (linked / unlinked / config-managed).

FastAPI mounts it: `app.mount("/ui", StaticFiles(directory=..., html=True))`.
No auth on `/ui` itself; the routes it calls carry the api_key.

### 4. [x] Conduit-side `/v1/vox/links`

New module `crates/conduit-api/src/vox.rs`. New table `vox_links` (peer_id
PK, peer_name, peer_base_url, sync_token_hash, provider_definition_id,
granted_by, granted_at, last_seen). Routes:

- `POST /v1/vox/links` — body `{peer_name, peer_id, vox_base_url,
  vox_api_key}`. Bearer is the operator's own token; requires
  Operator permission. Mints a scoped sync token
  (read-only on `/v1/speakers`), upserts an `http_speaker_id` provider
  definition pointing at `vox_base_url` with `vox_api_key`, records the
  link. Returns `{sync_token, provider_definition_id}`.
- `GET /v1/vox/links` — list linked peers (redacted).
- `DELETE /v1/vox/links/{peer_id}` — revoke sync token, do **not** delete the
  provider definition automatically (operator may still want to use it).

Migrations: SQLite + Postgres, per the dual-schema policy.

### 5. Vox link flow

New module in `app.py` for link state:

- `LinkStore` — reads/writes `link.json` in the data dir, mode 0600. Fields:
  `conduit_url`, `sync_token`, `peer_id`, `local_api_key`, `linked_at`.
- Routes:
  - `POST /link` — body `{conduit_url, operator_token, peer_name}`. Generates
    a `local_api_key` if `SPEAKER_ID_API_KEY` is unset, POSTs to Conduit,
    persists both tokens on success. Refuses if already linked (must unlink
    first, or use `force: true`).
  - `DELETE /link` — best-effort DELETE against Conduit, then remove
    `link.json`. Local api_key stays valid (Conduit will just no longer sync).
  - `GET /link` — status (linked/unlinked/config-managed, no tokens).
- `authorize()` now accepts either `SPEAKER_ID_API_KEY` (env) or the
  `local_api_key` from `link.json`. Env takes precedence.

Tokens never enter logs. `link.json` is refused if group- or world-readable,
mirroring how Conduit already handles its tokens file.

### 6. Vox → Conduit roster sync

New `Syncer` task in `app.py`, started at app startup if a link exists:

- On start and every N seconds (configurable, default 300), GET Conduit's
  `/v1/speakers` with the sync_token. Reconcile: for each remote speaker,
  upsert the label in the local roster. Do **not** delete local-only speakers
  — they may be enrolled directly against Vox for testing.
- Failure logged with useful context, exponential backoff up to a ceiling
  (max 15 min between retries). Never fatal.

### 7. Within-engine model swap

- `POST /engine/reload` — body `{model}`. Rebuilds the encoder using the
  current `engine` and the new `model`. `check_width` still enforces the
  declared width. If prints exist and the new model would produce a
  different width, refused with a 409.
- UI: form under Health showing current engine + model; "Reload" button
  disables while the encoder is loading.

### 8. Conduit reverse proxy `/vox/*`

- `crates/conduit-api/src/vox.rs` gains a `proxy` handler.
- Looks up the single linked peer (multi-peer is an explicit non-goal for
  this branch), streams the request body forward with the stored api_key,
  streams the response back. Rewrites `Location` headers if any.
- Auth stays with Conduit: the caller needs an operator session, same as
  every other console-facing route.
- If no peer is linked, 404 with a body pointing at the Link flow.

### 9. Replace Console SpeakersPanel with Vox iframe

- In `frontend/src/App.tsx`: sidebar label "Speakers" → "Vox". Replace the
  `SpeakersPanel` (~340 lines) plus its tests with an `<iframe
  src="/vox/ui" title="Conduit Vox" />` inside a section wrapper. Preserve
  the operator-permission gate.
- Delete the `SpeakerApi` interface uses that only served this panel (grep
  first — some are also used by pipeline editors and stay).
- Add a small **Linked services** admin surface elsewhere (Providers page?)
  listing `GET /v1/vox/links` with a revoke button.

## Non-goals for this branch

- **Multi-peer.** Conduit talks to at most one Vox at a time. The table
  schema allows more, but the reverse proxy assumes one linked peer.
- **Cross-engine swap** in the same image.
- **Removing Conduit-side speaker storage.** The roster still lives there;
  Vox sync is one-way (Conduit → Vox).
- **Weight migration between engines.** Swapping `SPEAKER_ID_ENGINE` still
  invalidates prints — the README continues to say so; the UI will warn.

## Open questions / follow-ups

Track here and turn into GitHub issues once agreed:

- Should Conduit's `/v1/speakers/{id}/enroll` still accept audio directly, or
  redirect operators to the Vox UI once linked? For now: both work.
- Multi-tenant Vox: could one Vox serve two Conduits? The link table already
  keys on `peer_id`; the constraint would be a per-caller view of speakers.
  Out of scope here, not blocked here.
- Do we want a Vox-side event stream (SSE) so the UI updates when Conduit
  enrolls someone through the reverse proxy?
