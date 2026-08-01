# conduit-wyoming

Wyoming protocol provider implementations.

[Wyoming](https://github.com/rhasspy/wyoming) is the wire protocol spoken by
Rhasspy's speech services — Piper for synthesis, faster-whisper for recognition.

| Provider | Trait | Endpoint |
| --- | --- | --- |
| `WyomingStt` | `SpeechToText` | `tcp://host:port` |
| `WyomingTts` | `TextToSpeech` | `tcp://host:port` |

## Wire Protocol

A Wyoming message is a JSON header line, optionally followed by a binary
payload whose length the header declares. `protocol` implements that framing;
each provider builds the events its capability needs.

Both providers are built from a `tcp://host:port` URL and connect per request.
Construction never touches the network, so registering a provider from a saved
provider definition cannot fail because a server is down.

## Speech Recognition

`WyomingStt` sends `audio-start`, streams `audio-chunk` events carrying raw
samples, and ends with `audio-stop`. The server answers with `transcript`
events; a transcript is final when its data carries a `result` object, and
partials are forwarded only when the request asks for them.

The optional `model` from the provider definition is sent as a hint in
`audio-start`.

## Speech Synthesis

`WyomingTts` sends one `synthesize` event carrying the text and the configured
voice, then forwards `audio-chunk` payloads as speech chunks until
`audio-stop`. Wyoming servers always stream, so synthesis emits audio as it
arrives rather than buffering the utterance.

The optional `voice` from the provider definition is the canonical voice
selection; the server's own default is used when it is absent.

## Health

`health()` opens a TCP connection and closes it. It reports `Healthy` when the
endpoint accepts the connection and `Unhealthy` with the connection error
otherwise, which is what a provider reachability test surfaces to operators.
