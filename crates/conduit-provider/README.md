# conduit-provider

Provider interfaces for replaceable Conduit components.

This crate defines object-safe traits and shared request/response types. It does
not implement vendor integrations except for optional in-memory test providers
behind the `testing` feature.

## Core Shape

Every provider implements `Provider` plus one capability trait. The stable
`Provider::name()` is the value pipeline graph nodes refer to.

Streaming methods return `ChunkStream<T>`, a boxed stream of fallible items.
Dropping a returned stream is the cancellation signal.

## Capability Traits

| Module | Trait | Purpose |
| --- | --- | --- |
| `stt` | `SpeechToText` | Convert captured audio chunks into transcript events. |
| `llm` | `LanguageModel` | Stream model tokens, reasoning deltas, tool calls, and finish events. |
| `tts` | `TextToSpeech` | Stream synthesized audio chunks. |
| `tool` | `Tool` | Execute model-requested tools with permission checks. |
| `storage` | `PipelineStore` | Store and retrieve pipeline graphs. |
| `wake` | `WakeWordDetector` | Describe wake-word providers; not wired into the runtime yet. |
| `speaker` | `SpeakerIdentifier` | Describe speaker identification providers; not wired into production yet. |
| `memory` | `Memory` | Describe memory providers; graph-only today. |

## Storage Contract

`PipelineStore` implementors must validate names on every method, distinguish
absence from unreadable data, and return only list entries that can later be
read. The conformance tests in `conduit-store` exercise that contract for every
backend.

## Testing Feature

Enable `testing` to expose `EchoStt`, `EchoLlm`, and `EchoTts`. They treat audio
as UTF-8 text and exist for development and tests, not production speech.
