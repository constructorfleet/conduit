//! The conversation socket: a device talking to a pipeline.
//!
//! The protocol is deliberately small. Binary frames are audio in both
//! directions; text frames are JSON control messages. Everything else about
//! the turn — partial transcripts, tool calls, timings — is on the event
//! stream, tagged with the conversation id this socket announces, so the
//! audio path stays free of anything that is not audio.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use conduit_core::id::ConversationId;
use conduit_provider::stt::AudioChunk;
use conduit_provider::ChunkStream;
use conduit_runtime::Runner;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::{ApiError, AppState};

/// How many captured chunks may queue before the socket reader waits.
///
/// Small on purpose: audio that has been waiting is audio the recognizer
/// should already have had.
const CAPTURE_BUFFER: usize = 32;

/// What a client can say that is not audio.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Command {
    /// The utterance is over; answer it.
    End,
}

/// What the server says that is not audio.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Notice {
    /// Sent before any audio, so the client can follow its own events.
    Started {
        /// The conversation this turn is filed under.
        conversation: ConversationId,
    },
    /// The turn finished normally.
    Done,
    /// The turn failed. The detail is also on the event stream.
    Failed {
        /// What went wrong.
        error: String,
    },
}

/// `GET /v1/pipelines/{name}/converse` — hold a conversation over a socket.
///
/// The pipeline is resolved *before* upgrading, so a client learns that a
/// pipeline is missing or unrunnable from an HTTP status rather than from a
/// socket that opens and then dies.
///
/// # Errors
///
/// Returns 404 if no such pipeline is stored, and 422 if it cannot be executed
/// with the providers this server has.
pub async fn converse(
    State(state): State<AppState>,
    Path(name): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let graph = state
        .pipeline(&name)
        .await
        .map_err(|error| ApiError::unavailable(error.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("no pipeline named `{name}`")))?;

    let providers = state.providers().ok_or_else(|| {
        ApiError::unprocessable("this server has no providers configured".to_owned())
    })?;

    let runner = Runner::prepare(&graph, &providers, state.bus.clone())
        .map_err(|error| ApiError::unprocessable(error.to_string()))?;

    tracing::info!(pipeline = %name, "conversation socket opened");
    Ok(upgrade.on_upgrade(move |socket| run(socket, runner)))
}

/// Drives one turn over `socket`.
async fn run(socket: WebSocket, runner: Runner) {
    let (mut outgoing, incoming) = socket.split();
    let (audio, captured) = capture(incoming);

    let conversation = runner.run(audio);
    if send(&mut outgoing, Notice::Started { conversation: conversation.id }).await.is_err() {
        return;
    }

    let mut speech = conversation.audio;
    let mut failed = None;
    while let Some(chunk) = speech.next().await {
        match chunk {
            Ok(chunk) => {
                use futures_util::SinkExt;
                if outgoing.send(Message::Binary(chunk.data)).await.is_err() {
                    // The device hung up. Dropping the stream cancels the turn.
                    tracing::debug!("device disconnected mid-reply");
                    return;
                }
            }
            Err(error) => {
                failed = Some(error.to_string());
                break;
            }
        }
    }

    let notice = match failed {
        Some(error) => Notice::Failed { error },
        None => Notice::Done,
    };
    let _ = send(&mut outgoing, notice).await;

    // Close deliberately rather than by dropping the socket: a client that
    // sees a reset cannot tell a finished turn from a crashed server.
    use futures_util::SinkExt;
    let _ = outgoing.send(Message::Close(None)).await;
    let _ = outgoing.close().await;

    // The reader holds the other half until the client stops sending.
    captured.abort();
}

/// Turns incoming frames into an audio stream.
///
/// Returns the stream and a handle to the task filling it, so the caller can
/// stop reading once the turn is over.
fn capture(
    mut incoming: futures_util::stream::SplitStream<WebSocket>,
) -> (ChunkStream<AudioChunk>, tokio::task::AbortHandle) {
    let (sender, receiver) = mpsc::channel(CAPTURE_BUFFER);

    let task = tokio::spawn(async move {
        let mut sequence = 0_u64;
        while let Some(frame) = incoming.next().await {
            let Ok(frame) = frame else { break };
            match frame {
                Message::Binary(data) => {
                    let chunk = AudioChunk { sequence, data };
                    sequence += 1;
                    if sender.send(Ok(chunk)).await.is_err() {
                        break;
                    }
                }
                Message::Text(text) => match serde_json::from_str::<Command>(&text) {
                    // Closing the sender ends the utterance, which is what
                    // tells the recognizer to produce its final transcript.
                    Ok(Command::End) => break,
                    Err(error) => {
                        tracing::warn!(%error, %text, "ignoring unreadable control frame");
                    }
                },
                Message::Close(_) => break,
                // Ping and pong are handled by the server for us.
                _ => {}
            }
        }
    });

    (Box::pin(ReceiverStream::new(receiver)), task.abort_handle())
}

/// Sends one control frame.
async fn send(
    outgoing: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    notice: Notice,
) -> Result<(), axum::Error> {
    use futures_util::SinkExt;
    let text = serde_json::to_string(&notice).unwrap_or_else(|_| {
        // Notice is a closed set of serializable variants; this cannot happen,
        // but a socket is no place to panic.
        r#"{"type":"failed","error":"could not encode notice"}"#.to_owned()
    });
    outgoing.send(Message::Text(text.into())).await
}
