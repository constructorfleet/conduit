//! The conversation socket: a device talking to a pipeline.
//!
//! The protocol is deliberately small. Binary frames are audio in both
//! directions; text frames are JSON control messages. Everything else about
//! the turn — partial transcripts, tool calls, timings — is on the event
//! stream, tagged with the conversation id this socket announces, so the
//! audio path stays free of anything that is not audio.
//!
//! Control messages flow for the whole turn, not only until the utterance
//! ends, because the useful moment to say "stop talking" is while the assistant
//! is talking.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::Response;
use conduit_core::audio::{AudioFormat, Encoding};
use conduit_core::device::{Command, Notice};
use conduit_provider::stt::AudioChunk;
use conduit_provider::ChunkStream;
use conduit_runtime::Runner;
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::auth::{Device, DeviceCaller};
use crate::status::RuntimeStatus;
use crate::{ApiError, AppState};

/// How many captured chunks may queue before the socket reader waits.
///
/// Small on purpose: audio that has been waiting is audio the recognizer
/// should already have had.
const CAPTURE_BUFFER: usize = 32;

/// `GET /v1/pipelines/{name}/converse` — hold a conversation over a socket.
///
/// The pipeline is resolved *before* upgrading, so a client learns that a
/// pipeline is missing or unrunnable from an HTTP status rather than from a
/// socket that opens and then dies.
///
/// # Errors
///
/// Returns 401 without a usable device token, 403 if that device is restricted
/// to other pipelines, 404 if no such pipeline is stored, and 422 if it cannot
/// be executed with the providers this server has or the requested audio format
/// is not usable.
pub(crate) async fn converse(
    // First in the list on purpose: axum runs extractors in declaration order,
    // so an unauthenticated caller is refused before anything else is parsed.
    DeviceCaller(device): DeviceCaller,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(format): Query<AudioFormatQuery>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    // Checked before the pipeline is looked up, so a device restricted away
    // from a pipeline cannot use the 404 to learn whether it exists.
    if !device.may_use(&name) {
        tracing::warn!(
            device = %device.name,
            pipeline = %name,
            "rejected a device that is not permitted to use this pipeline"
        );
        return Err(ApiError::forbidden(format!(
            "device `{}` is not permitted to use pipeline `{name}`",
            device.name
        )));
    }

    let format = format.into_format()?;
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
    let runner = runner
        .with_format(format)
        .map_err(|error| ApiError::unprocessable(error.to_string()))?
        // Without this, a provider that accepts a request and never answers
        // holds the socket open for as long as the device is willing to wait.
        .with_idle_timeout(state.turn_idle_timeout());

    tracing::info!(pipeline = %name, device = %device.name, "conversation socket opened");
    let status = state.status();
    Ok(upgrade.on_upgrade(move |socket| run(socket, runner, device, name, status)))
}

/// Device-negotiated audio format for one conversation.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct AudioFormatQuery {
    encoding: Option<Encoding>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
}

impl AudioFormatQuery {
    fn into_format(self) -> Result<AudioFormat, ApiError> {
        let default = AudioFormat::DEFAULT;
        let format = AudioFormat {
            encoding: self.encoding.unwrap_or(default.encoding),
            sample_rate: self.sample_rate.unwrap_or(default.sample_rate),
            channels: self.channels.unwrap_or(default.channels),
        };
        if format.sample_rate == 0 {
            return Err(ApiError::unprocessable("audio sample_rate must be greater than zero"));
        }
        if format.channels == 0 {
            return Err(ApiError::unprocessable("audio channels must be greater than zero"));
        }
        Ok(format)
    }
}

/// Drives one turn over `socket` on behalf of `device`.
async fn run(
    socket: WebSocket,
    runner: Runner,
    device: Device,
    pipeline: String,
    status: RuntimeStatus,
) {
    let (mut outgoing, incoming) = socket.split();
    let (audio, captured, stopped) = capture(incoming);

    // Every event this turn publishes carries the device, which is what lets an
    // operator filter the event stream by satellite.
    let conversation = runner.run_for_device(device.id, audio);
    // The upgrade is logged before the conversation exists, so this is what
    // ties a socket to the id every turn event and every close below carries.
    tracing::info!(
        conversation = %conversation.id,
        device = %device.name,
        pipeline = %pipeline,
        "conversation turn started"
    );
    status.connect_satellite(device.id, device.name.clone(), pipeline, conversation.id).await;

    // The reader cannot hold the turn's stop handle, because the turn needs the
    // audio the reader produces to exist first. So it signals, and this relays.
    // Ends by itself when the reader stops: the sender drops, and the receiver
    // resolves with an error.
    let stop = conversation.stop.clone();
    let relay = tokio::spawn(async move {
        if stopped.await.is_ok() {
            stop.request();
        }
    });

    if send(&mut outgoing, Notice::Started { conversation: conversation.id }).await.is_err() {
        // Every exit from here on says why, and how much reply audio the device
        // actually got. `conversation socket opened` with no matching close is
        // what makes a turn that dies mid-flight indistinguishable from one
        // still running, and which side hung up first is exactly the thing that
        // cannot be reconstructed afterwards from a provider's logs.
        tracing::info!(
            conversation = %conversation.id,
            device = %device.name,
            reason = "device_gone_before_start",
            reply_chunks = 0,
            "conversation socket closed"
        );
        relay.abort();
        status.disconnect_satellite(device.id).await;
        return;
    }

    // Captured before `speech` consumes the conversation; the id is still
    // what every log line about this turn is filed under.
    let conversation_id = conversation.id;
    let mut speech = conversation.speech();
    let mut failed = None;
    let mut reply_chunks = 0_u64;
    while let Some(chunk) = speech.next().await {
        match chunk {
            Ok(chunk) => {
                use futures_util::SinkExt;
                if outgoing.send(Message::Binary(chunk.data)).await.is_err() {
                    // The device hung up. Dropping the stream cancels the turn,
                    // reported as a disconnection rather than an interruption.
                    tracing::info!(
                        conversation = %conversation_id,
                        device = %device.name,
                        reason = "device_disconnected_mid_reply",
                        reply_chunks,
                        "conversation socket closed"
                    );
                    relay.abort();
                    status.disconnect_satellite(device.id).await;
                    return;
                }
                reply_chunks += 1;
            }
            Err(error) => {
                failed = Some(error.to_string());
                break;
            }
        }
    }

    // A turn that ends with no reply audio at all is the shape of a stage that
    // failed rather than a turn that answered, so the count is worth having
    // even on the successful path.
    match &failed {
        Some(error) => tracing::warn!(
            conversation = %conversation_id,
            device = %device.name,
            reason = "turn_failed",
            reply_chunks,
            error = %error,
            "conversation socket closed"
        ),
        None => tracing::info!(
            conversation = %conversation_id,
            device = %device.name,
            reason = "completed",
            reply_chunks,
            "conversation socket closed"
        ),
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
    // Nothing left to stop.
    relay.abort();
    status.disconnect_satellite(device.id).await;
}

/// Turns incoming frames into an audio stream, watching for a stop.
///
/// Returns the audio, a handle to the task filling it so the caller can stop
/// reading once the turn is over, and a receiver that resolves if the client
/// asks the turn to stop.
///
/// Reading continues past the end of the utterance, because a stop is most
/// useful while the assistant is already talking — a reader that finished with
/// the audio would never see one.
fn capture(
    mut incoming: futures_util::stream::SplitStream<WebSocket>,
) -> (ChunkStream<AudioChunk>, tokio::task::AbortHandle, tokio::sync::oneshot::Receiver<()>) {
    let (sender, receiver) = mpsc::channel(CAPTURE_BUFFER);
    let (stopped, on_stop) = tokio::sync::oneshot::channel();

    let task = tokio::spawn(async move {
        let mut sequence = 0_u64;
        // Taken to end the utterance. Dropping it is what tells the recognizer
        // to produce its final transcript.
        let mut sender = Some(sender);

        while let Some(frame) = incoming.next().await {
            let Ok(frame) = frame else { break };
            match frame {
                Message::Binary(data) => {
                    let Some(audio) = &sender else {
                        // Audio after the utterance ended belongs to a turn this
                        // socket is not holding.
                        tracing::warn!("ignoring audio sent after the end of the utterance");
                        continue;
                    };
                    let chunk = AudioChunk { sequence, data };
                    sequence += 1;
                    if audio.send(Ok(chunk)).await.is_err() {
                        break;
                    }
                }
                Message::Text(text) => match serde_json::from_str::<Command>(&text) {
                    Ok(Command::End) => {
                        // Reading continues, in case a stop follows.
                        sender = None;
                    }
                    Ok(Command::Stop) => {
                        tracing::debug!("device asked the turn to stop");
                        let _ = stopped.send(());
                        break;
                    }
                    // `Command` is non-exhaustive: a newer client may send
                    // something this server predates.
                    Ok(unknown) => {
                        tracing::warn!(?unknown, "ignoring unsupported command");
                    }
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

    (Box::pin(ReceiverStream::new(receiver)), task.abort_handle(), on_stop)
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
