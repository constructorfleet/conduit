# conduit-openai

OpenAI-compatible provider implementations.

This crate implements the provider traits for the OpenAI-style endpoints used
by hosted OpenAI and many local model or speech servers.

| Provider | Endpoint | Trait |
| --- | --- | --- |
| `OpenAi` | `/chat/completions` | `LanguageModel` |
| `OpenAiStt` | `/audio/transcriptions` | `SpeechToText` |
| `OpenAiTts` | `/audio/speech` | `TextToSpeech` |

## Configuration Model

`OpenAiConfig` describes one server:

- `base_url`, including any `/v1` prefix
- optional bearer `api_key`
- provider registration `name`
- connect timeout
- read timeout
- advertised model list

A single server can back language, transcription, and synthesis providers. Each
provider uses the same registration name by default, so configure distinct names
when registering multiple servers side by side.

## Language Model

`OpenAi` converts Conduit's completion request into a chat-completions request
and decodes the streaming response into tokens, reasoning deltas, tool calls,
finish reasons, and usage.

The provider reports `supports_tools() == true`.

## Speech Recognition

`OpenAiStt` buffers the complete utterance, packages PCM as WAV when needed,
uploads it as multipart form data, and emits one final transcript. It does not
invent partial transcripts because the endpoint does not provide them.

Accepted upload encodings are signed 16-bit PCM, 32-bit float PCM, and FLAC.
Raw Opus is refused because the code does not build an Opus container.

## Speech Synthesis

`OpenAiTts` posts to the audio speech endpoint and forwards response bytes as
speech chunks as they arrive. Hosted voices are used as a fallback catalogue;
local servers can replace the voice list with `with_voices`.

The speech endpoint can produce signed 16-bit PCM, FLAC, and Opus. It cannot
produce 32-bit float PCM.
