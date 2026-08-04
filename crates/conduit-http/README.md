# conduit-http

Shared HTTP plumbing for Conduit's HTTP-backed providers.

Every provider that talks to a JSON-over-HTTP vendor needs the same three
things, and none of them belong to any one vendor:

| Type | What it does |
| --- | --- |
| `Http` | Sends authenticated requests and turns a non-2xx status into a classified error |
| `Failure` | Says *why* a request failed, in enough detail to decide between retrying, failing over, and giving up |
| `sse::Decoder` | Reassembles server-sent events from arbitrarily split byte packets |

What differs between vendors is the request and response shape, and that stays
in the vendor's own crate: `conduit-openai` for the chat completions family,
`conduit-anthropic` for the Messages API.

## Credentials

`Credential` names the authentication mechanism rather than assuming one,
because vendors disagree on more than the value:

| Variant | Wire form | Used by |
| --- | --- | --- |
| `None` | nothing | local servers on the LAN |
| `Bearer(token)` | `Authorization: Bearer <token>` | OpenAI-compatible servers |
| `Header { name, value }` | `<name>: <value>` | Anthropic, via `x-api-key` |

`Credential` implements `Debug` by hand and prints `<redacted>` in place of
every secret. Providers derive `Debug` and hold one of these, so that
implementation is what stands between an API key and a log line. Pinned
non-secret headers — an API version, a feature opt-in — go in
`HttpConfig::headers`, which does print itself.

## Timeouts

`HttpConfig` takes a connect timeout and an optional *read* timeout, not a total
request timeout. A long answer and a slow synthesis both stream for as long as
they need, so bounding the whole response would truncate work that is going
fine. What is never fine is silence.
