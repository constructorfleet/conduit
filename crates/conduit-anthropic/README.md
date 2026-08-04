# conduit-anthropic

Language models over Anthropic's Messages API.

| Provider | Endpoint | Trait |
| --- | --- | --- |
| `Anthropic` | `/messages` | `LanguageModel` |

## Why a Second Crate

Most model servers speak chat completions, so `conduit-openai` reaches them by
changing a base URL. This API is not one of them, and the differences are not
cosmetic:

| | Chat completions | Messages |
| --- | --- | --- |
| Credential | `Authorization: Bearer …` | `x-api-key` header |
| Version | implied by the path | required `anthropic-version` header |
| System framing | a message with `role: "system"` | a top-level `system` field |
| Token cap | optional | `max_tokens` is required |
| Streaming | uniform `choices[].delta` chunks | typed events opening and closing content blocks |
| Tool schema | `parameters` | `input_schema` |

Both crates share `conduit-http` for request sending, failure classification,
and server-sent event framing, so only the translation lives here.

## Configuration

`AnthropicConfig` mirrors `OpenAiConfig`'s field set: `base_url`, `api_key`,
registration `name`, optional `label`, connect and read timeouts, advertised
`models`, a `system_prompt`, and `default_settings`.

Naming no models advertises the current ones (`DEFAULT_MODELS`) rather than
leaving an operator with an empty menu.

`API_VERSION` is pinned in this crate rather than configured. The header selects
a wire contract, and the one this code was written against is the one it can
decode.

## Settings

The declared settings are `output_config`, `thinking`, `stop_sequences`, and
`metadata`. Anything else is refused when the definition is saved.

Sampling controls are deliberately absent. Current models reject `temperature`,
`top_p`, and `top_k` with a 400, so declaring them would let an operator
configure a failure that only appears mid-conversation. `CompletionRequest`'s
own `temperature` is not forwarded for the same reason.

## Streaming

A response arrives as blocks that open, accumulate deltas, and close. Two things
make decoding more than a parse:

- A tool call's arguments arrive as JSON *text* in fragments that are each
  invalid alone. They are accumulated per block index, so two calls in one
  response cannot mix their arguments, and parsed once the response ends.
- The runtime waits for a `Finished`. Exactly one is emitted — after any
  assembled tool calls, and even when the server simply stops talking.

Thinking deltas become `Completion::Reasoning`, never `Completion::Token`, so
reasoning is never spoken aloud.

Usage accumulates rather than replaces: input tokens arrive at `message_start`
and output tokens at `message_delta`.

## Accepted Limitations

**A tool result travels as user-role text**, not a `tool_result` block. The
block would have to name the `tool_use` that requested it, and Conduit's history
keeps the result and the call id but not the originating block; an unpaired
`tool_result` is rejected. Sent as text, the model reads the answer without the
pairing.

**A trailing assistant turn is dropped.** Prefilling the assistant's reply was
removed from the API and is now a 400, so a history ending on an assistant
message cannot be sent as it stands. Dropping it beats failing a turn over
context the model is about to regenerate.

## Health

`health()` lists models. The Messages API has no unauthenticated liveness route,
and this is the cheapest call that exercises the credential as well as the
connection — a rejected key is something an operator should see before a turn
discovers it.
