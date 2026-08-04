# conduit-bedrock

Language models over Amazon Bedrock's Converse API.

| Provider | Operation | Trait |
| --- | --- | --- |
| `Bedrock` | `ConverseStream` | `LanguageModel` |

## Why a Third Crate

`conduit-anthropic` exists because the Messages API's *wire format* differs from
chat completions. This crate exists for a different reason: the models are often
the same ones, and what differs is everything around the request.

| | Messages | Converse |
| --- | --- | --- |
| Endpoint | a base URL | a region — there is no URL to configure |
| Credential | a key an operator types | whatever the AWS chain resolves: a task role, an instance profile, a named profile, `AWS_ACCESS_KEY_ID` |
| Signing | a header | SigV4, per request, over the canonical request |
| Conversation | any order of roles | user and assistant must alternate |
| Sampling controls | body fields | `inferenceConfig`, a field of its own |
| Model-specific controls | body fields | `additionalModelRequestFields` |
| Streaming | server-sent events | an `application/vnd.amazon.eventstream` frame protocol |

None of that is reachable by changing a base URL, and SigV4 alone is a reason
not to hand-roll it: a signature covers the canonical request, so a
Conduit-side implementation would have to stay correct across every future
header the service adds.

## The `bedrock` Feature

On by default. Off, the AWS SDK and its ~40 transitive crates are not compiled,
and `Bedrock::new` refuses with a message naming the feature — so a lean build
says so when a definition is saved rather than when someone speaks to it.

The SDK's own `rustls` feature routes through `aws-smithy-runtime/tls-rustls`,
which selects `aws-lc-rs`: a C library, and a second cryptographic provider
beside the ring the rest of this workspace links. So `aws-smithy-http-client` is
a direct dependency with `rustls-ring`, and the client it builds is handed to
`aws_config` explicitly. `grep -c aws-lc Cargo.lock` is 0, and should stay 0.

## Configuration

`BedrockConfig` carries `region`, an optional shared-config `profile`, an
optional `api_key`, the registration `name`, an optional `label`, connect and
read timeouts, advertised `models`, a `system_prompt`, and `default_settings`.

Naming no models advertises the current ones (`DEFAULT_MODELS`) rather than
leaving an operator with an empty menu. They are inference profile ids — the
`us.`-prefixed form — because that is what a cross-region model is invoked as.

`api_key` is a Bedrock API key, which is a bearer token rather than an access
key pair. Naming one also sets bearer auth as preferred: left implicit the SDK
would accept the key and then sign with an unrelated instance role instead.
Most deployments should leave it unset.

Building the client is infallible on purpose. Every failure available here — no
credentials, an unknown profile, an unreachable metadata endpoint — is one the
SDK reports when a request is *sent*. Turning those into a build failure would
mean a provider that cannot be registered and therefore cannot report its own
health, when what an operator needs is a provider that appears and says
`Unhealthy` with the reason.

## Settings

The declared settings are `top_k`, `thinking`, and `anthropic_beta`, and they
travel in `additionalModelRequestFields`. Which of them a given model accepts
depends on the model, and Bedrock is a door onto many, so they are declared as
the open object the API treats them as.

`temperature`, `max_tokens`, and `top_p` are deliberately *not* settings.
Converse takes them in `inferenceConfig`, read from the request, so declaring
them would send the same control twice in two places with the API choosing a
winner.

## Streaming

The event stream is decoded in `stream.rs`. Two orderings matter:

- A tool call's arguments arrive as JSON *text* in fragments that are each
  invalid alone, keyed by content block index. They accumulate per index, so two
  calls in one response cannot mix arguments, and are parsed once the block or
  the stream ends.
- Token counts arrive in a `metadata` event *after* `messageStop`. The stop
  reason is therefore held rather than emitted, and the single `Finished` is
  built when metadata arrives — or at stream end, if the stream is cut short
  before it does.

Reasoning deltas become `Completion::Reasoning`, never `Completion::Token`, so
reasoning is never spoken aloud. A reasoning block's signature and redacted
content are dropped: they exist to be replayed on a later request, and Conduit's
history has nowhere to keep them.

## Accepted Limitations

**Consecutive same-side turns are joined.** Converse rejects a conversation that
does not alternate, and the runtime's history routinely does not: a memory
recall, a tool result, and a spoken utterance are three consecutive user-side
turns. They are joined into one rather than letting the API refuse the request.

**A tool result travels as user-role text**, not a `toolResult` block. The block
must name the `toolUse` that requested it, and Conduit's history keeps the result
and the call id but not the requesting block.

**A trailing assistant turn is dropped**, as is a leading one, because Converse
requires a conversation to begin with a user turn.

## Health

`health()` counts the tokens in a one-word conversation. The runtime endpoint has
no list-models route and no unauthenticated liveness one, so `CountTokens` is the
cheapest call that exercises the credential, the region, and the model id
together — and it runs no inference. A red provider reports the region alongside
the failure, because "which region" is half of what is usually wrong.
