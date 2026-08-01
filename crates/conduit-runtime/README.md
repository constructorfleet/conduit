# conduit-runtime

Pipeline execution for Conduit.

The runtime resolves a `PipelineGraph` against registered providers once, then
runs one conversation turn per utterance: captured audio in, synthesized audio
out, events throughout.

## Responsibilities

- validate that a graph is executable by the current runtime
- resolve provider names into concrete provider trait objects
- transcribe captured audio
- stream model output
- run model-requested tools
- synthesize complete sentences as soon as they are available
- publish lifecycle, transcript, reasoning, tool, synthesis, failure, and
  cancellation events
- enforce idle turn deadlines
- stop a turn when the caller requests it

## Executable Graph Shape

The graph model can describe more than the runtime currently runs. Today the
runtime accepts:

- one `stt` node
- one `llm` node selecting a registered provider id
- one `tts` node
- any number of `tool` nodes downstream of the model
- optional `source` and `sink` endpoint nodes

The runtime refuses duplicate STT/LLM/TTS nodes, missing required stages,
unsupported node kinds, router nodes, and graphs whose edges do not place STT
upstream of LLM and LLM upstream of TTS and tools.

## Streaming Behavior

The runtime does not wait for a full model answer before speaking. It buffers
text only until a complete sentence is available, then starts synthesis while
later model output or tool work can continue.

Tool preambles are spoken while the tool runs. Tools requested together run
together. Every tool outcome is returned to the model, including failures,
permission denials, unknown tools, and confirmation-required refusals.

## Deadlines And Cancellation

`Runner` defaults to `DEFAULT_IDLE_TIMEOUT`. The idle timer bounds silence, not
turn length: every published event counts as progress. If no progress is
published before the deadline, the turn emits an idle-timeout cancellation and
returns a timeout error on the audio stream.

`Conversation::stop` requests a graceful stop and records the outcome as
`user_requested`. Dropping the audio stream cancels as a disconnected listener.
`Conversation::abort` is reserved for shutdown.
