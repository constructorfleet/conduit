//! Running one input through several pipelines and reporting the difference.
//!
//! A [`Pipeline Test Turn`] answers what one pipeline said. It does not answer
//! whether a different recognizer would have said the same thing, or which of
//! the two cost more — and those are the questions an operator choosing between
//! two engines, two models, or two hosts actually has. Comparison composes the
//! test turn rather than reimplementing it, so a change to how a turn runs
//! cannot make comparison describe a runtime nobody has.
//!
//! Agreement between candidates is the referee, per
//! [ADR-0018](../../../docs/adr/0018-comparison-judged-by-agreement.md). Nothing
//! is labelled: when every pipeline produces the same reply the choice collapses
//! to cost, and when they differ the differences are the few cases worth
//! listening to. What agreement cannot do is notice that both candidates are
//! wrong the same way, which is why [`Verdict::reliability`] exists and why the
//! normalization applied is reported rather than hidden.

use std::collections::HashMap;
use std::time::Duration;

use axum::extract::State;
use axum::Json;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use bytes::Bytes;
use conduit_core::audio::AudioFormat;
use conduit_core::bus::{Filter, Subscription};
use conduit_core::event::{Envelope, Event, Stage};
use conduit_core::graph::{Node, PipelineGraph};
use conduit_core::id::ConversationId;
use conduit_provider::stt::AudioChunk;
use conduit_provider::ChunkStream;
use conduit_runtime::{Reply, Runner};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::auth::ManagementCaller;
use crate::error::JsonBody;
use crate::{ApiError, AppState};

/// Fewest pipelines a comparison can be run over.
///
/// One pipeline is a test turn, and calling it a comparison would report a
/// verdict about nothing.
pub const MINIMUM_PIPELINES: usize = 2;

/// Most pipelines one comparison may name.
///
/// Candidates run in sequence and each one runs a real turn against real
/// providers, so an unbounded list is an unbounded request. Six is enough for
/// every comparison the feature exists for — two engines, two models, two
/// locations — with room to spare.
pub const MAXIMUM_PIPELINES: usize = 6;

/// Body limit for a comparison, which may carry a recorded fixture.
///
/// The same reasoning as speaker enrollment: the service-wide budget is sized
/// for JSON, and a request carrying seconds of audio needs its own. Base64
/// inflates by a third, so this holds roughly six megabytes of samples.
pub const COMPARISON_BODY_LIMIT_BYTES: usize = 8 * 1024 * 1024;

/// What to feed every pipeline in a comparison.
///
/// Exactly one of the two, and untagged so an operator writes `{"utterance":
/// "..."}` or `{"audio": "..."}` rather than naming a discriminant. A recorded
/// fixture is what makes comparing *recognizers* meaningful — a typed utterance
/// reaches a real recognizer as bytes it was never trained to read — and the
/// typed path stays because comparing text pipelines should not require a
/// microphone.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ComparisonInput {
    /// Words to hand a pipeline directly, as the test turn does.
    Utterance(String),
    /// A recorded WAV file, base64-encoded.
    Audio(String),
}

/// Input for an operator-triggered pipeline comparison.
#[derive(Debug, Deserialize)]
pub struct ComparisonRequest {
    /// Stored pipelines to run, in the order they should run.
    pub pipelines: Vec<String>,
    /// The one input every pipeline receives.
    pub input: ComparisonInput,
    /// Audio format advertised to providers.
    ///
    /// Ignored for a recorded fixture, whose own header says what it is.
    #[serde(default)]
    pub format: AudioFormat,
}

/// How a comparison ran its candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Execution {
    /// One candidate at a time.
    ///
    /// The default and the only mode offered. Candidates run against real
    /// providers, and running them together would let two of them contend for
    /// the same CPU or the same GPU and report latencies nobody could reproduce
    /// in production — which is the one thing a comparison must not do.
    Sequential,
}

/// Whether an agreement verdict can be trusted for this set of pipelines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reliability {
    /// The pipelines differ only in how they recognize or rewrite, so equal
    /// replies mean equivalent behavior.
    Reliable,
    /// The pipelines reason with different cores.
    ///
    /// Two models phrase the same correct answer differently, so textual
    /// equality is not evidence either way and a reported disagreement is not a
    /// finding. Marked rather than refused: an operator comparing latency across
    /// cores has a real question, and only the verdict is unreliable.
    CoresDiffer,
}

/// One rewrite applied before replies were compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Normalization {
    /// Letters folded to lower case.
    Case,
    /// Leading, trailing, and repeated whitespace collapsed to single spaces.
    Whitespace,
    /// Punctuation removed.
    Punctuation,
}

impl Normalization {
    /// Every rule, in the order they are applied.
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Case, Self::Punctuation, Self::Whitespace]
    }
}

/// Reduces a reply to the form replies are compared in.
///
/// Deliberately conservative: case, punctuation, and whitespace only. Filler
/// words and numeral spelling were considered and left out, because "two" and
/// "2" are a real difference between recognizers and folding them would hide
/// exactly what an operator is comparing. What is applied is reported, so a
/// disagreement can be read as substantive or not.
#[must_use]
pub fn normalize(text: &str) -> String {
    let folded: String = text
        .chars()
        .map(|character| {
            if character.is_ascii_punctuation() {
                ' '
            } else {
                character.to_ascii_lowercase()
            }
        })
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What one candidate produced, or why it did not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum Outcome {
    /// The candidate ran to completion.
    Completed {
        /// What the recognizer heard, when the pipeline listened.
        ///
        /// The comparison an operator is usually making is between recognizers,
        /// and this is the thing they differ about. Reported beside the reply
        /// because a reply can be identical while the transcripts differ — a
        /// model asked two slightly different questions may answer the same way.
        #[serde(skip_serializing_if = "Option::is_none")]
        transcript: Option<String>,
        /// The utterance the core emitted, however it was rendered.
        ///
        /// Read from the bus rather than from the reply stream: a voice pipeline
        /// returns its reply as audio, so a comparison that only read the stream
        /// saw nothing and reported two voice pipelines as agreeing on the empty
        /// string — vacuous agreement for the exact case this exists to judge.
        /// The utterance is what a core said, per ADR-0012, and speech is one
        /// rendering of it.
        #[serde(skip_serializing_if = "Option::is_none")]
        reply_text: Option<String>,
        /// The text that was compared, before normalization.
        ///
        /// The transcript for a pipeline that listened, and the utterance for
        /// one that read: a recognizer comparison turns on what was heard, and
        /// a text pipeline never heard anything.
        compared_raw: String,
        /// That text after normalization, which is what agreement was decided on.
        normalized: String,
        /// Number of synthesized audio bytes.
        audio_bytes: usize,
        /// The synthesized reply as a playable WAV, base64-encoded.
        ///
        /// Judging whether a voice sounds right is something only a person
        /// listening can do, and no automatic verdict replaces it.
        #[serde(skip_serializing_if = "Option::is_none")]
        reply_audio: Option<String>,
    },
    /// The candidate could not be prepared or did not finish.
    ///
    /// Reported as its own status rather than as an empty reply, because a
    /// crash counted as a disagreement is what makes a comparison report
    /// actively misleading.
    Failed {
        /// What went wrong.
        error: String,
        /// The pipeline node that failed, when the runtime named one.
        #[serde(skip_serializing_if = "Option::is_none")]
        node: Option<String>,
    },
}

/// How long one stage of a candidate's turn took.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StageTiming {
    /// The stage this describes.
    pub stage: Stage,
    /// Milliseconds between the stage's first and last event.
    pub elapsed_ms: u64,
}

/// One candidate's contribution to a comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// The pipeline that ran.
    pub pipeline: String,
    /// The conversation this turn was tagged with, when it started one.
    ///
    /// Present so an operator can open the full `Turn Reconstruction` for a
    /// candidate that looks wrong: this report is a summary, and the
    /// reconstruction is the detail it does not duplicate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation: Option<ConversationId>,
    /// What it produced, or why it did not.
    pub outcome: Outcome,
    /// Milliseconds from the start of the turn to its terminal event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// Per-stage timings, in stage order.
    ///
    /// Derived from the event bus, which is the same source
    /// `Turn Reconstruction` reads, so comparison and reconstruction cannot
    /// disagree about what happened. A remote component's steps are emitted by
    /// the runtime and appear here identically to an in-process component's.
    pub stages: Vec<StageTiming>,
}

/// Whether the candidates agreed, and how far that can be trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict {
    /// Whether every candidate that completed produced the same normalized
    /// reply.
    pub agreed: bool,
    /// Whether the raw replies were byte-identical.
    ///
    /// Reported beside [`agreed`](Self::agreed) so an operator can tell
    /// equivalence from lenience: agreement that needed normalization to appear
    /// is a weaker claim than agreement that did not.
    pub identical: bool,
    /// How far this verdict can be trusted for these pipelines.
    pub reliability: Reliability,
    /// How many candidates completed and were therefore compared.
    ///
    /// A failed candidate is excluded rather than counted as disagreeing.
    pub compared: usize,
    /// The distinct normalized replies, in the order first seen.
    ///
    /// One entry when the candidates agreed; the differences to read or listen
    /// to when they did not.
    pub replies: Vec<String>,
}

/// The report from one comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComparisonReport {
    /// What each candidate produced, in the order they ran.
    pub candidates: Vec<Candidate>,
    /// Whether they agreed.
    pub verdict: Verdict,
    /// Rewrites applied to every reply before comparison.
    pub normalization: Vec<Normalization>,
    /// How the candidates were run.
    pub execution: Execution,
}

/// `POST /v1/pipelines/compare` — runs one input through several pipelines.
///
/// # Errors
///
/// Returns 422 when fewer than [`MINIMUM_PIPELINES`] or more than
/// [`MAXIMUM_PIPELINES`] are named, when a named pipeline is not stored, when no
/// runtime providers are configured, or when a recorded fixture is not readable
/// as WAV. A candidate that fails to prepare or fails while running is reported
/// as a failed candidate rather than failing the request.
pub async fn compare(
    _caller: ManagementCaller,
    State(state): State<AppState>,
    JsonBody(request): JsonBody<ComparisonRequest>,
) -> Result<Json<ComparisonReport>, ApiError> {
    if request.pipelines.len() < MINIMUM_PIPELINES {
        return Err(ApiError::unprocessable(format!(
            "a comparison needs at least {MINIMUM_PIPELINES} pipelines; \
             one pipeline is a test turn"
        )));
    }
    if request.pipelines.len() > MAXIMUM_PIPELINES {
        return Err(ApiError::unprocessable(format!(
            "a comparison runs at most {MAXIMUM_PIPELINES} pipelines, and {} were named",
            request.pipelines.len()
        )));
    }

    let providers = state
        .providers()
        .ok_or_else(|| ApiError::unprocessable("no providers are configured".to_owned()))?;

    // Every pipeline is looked up before any of them runs. A comparison whose
    // third candidate does not exist must not first spend two real turns
    // proving it: a partial report read as a complete one is the failure worth
    // preventing here.
    let mut graphs = Vec::with_capacity(request.pipelines.len());
    for name in &request.pipelines {
        let graph = state
            .pipeline(name)
            .await
            .map_err(crate::pipelines::store_failure)?
            .ok_or_else(|| ApiError::not_found(format!("no pipeline named `{name}`")))?;
        graphs.push((name.clone(), graph));
    }

    // Decoded once, before anything runs. The same bytes reach every candidate,
    // so a difference in output is attributable to the pipelines rather than to
    // the fixture — and a bad fixture is one error rather than N identical ones.
    let (input, format) = match &request.input {
        ComparisonInput::Utterance(utterance) => {
            (Input::Text(utterance.clone()), request.format)
        }
        ComparisonInput::Audio(encoded) => {
            let bytes = BASE64.decode(encoded).map_err(|error| {
                ApiError::unprocessable(format!("audio fixture is not valid base64: {error}"))
            })?;
            let pcm = conduit_core::wav::parse(&bytes).map_err(|error| {
                ApiError::unprocessable(format!("audio fixture is not readable: {error}"))
            })?;
            // The file's own header wins over the request's format. A fixture
            // that says what it is and a caller that says otherwise is a
            // disagreement, and the recording is the one that knows.
            (Input::Audio(Bytes::from(pcm.samples)), pcm.format)
        }
    };

    let reliability = reliability_of(graphs.iter().map(|(_, graph)| graph));

    let mut candidates = Vec::with_capacity(graphs.len());
    for (name, graph) in graphs {
        candidates.push(run_candidate(&state, &providers, name, &graph, &input, format).await);
    }

    let verdict = judge(&candidates, reliability);

    Ok(Json(ComparisonReport {
        candidates,
        verdict,
        normalization: Normalization::all().to_vec(),
        execution: Execution::Sequential,
    }))
}

/// The one input, decoded and ready to hand to every candidate.
enum Input {
    Text(String),
    Audio(Bytes),
}

impl Input {
    /// A fresh stream over the same bytes.
    ///
    /// A `ChunkStream` is consumed by the turn that reads it, so each candidate
    /// needs its own handle to identical bytes.
    fn audio(&self) -> ChunkStream<AudioChunk> {
        let data = match self {
            Self::Text(text) => Bytes::from(text.clone().into_bytes()),
            Self::Audio(samples) => samples.clone(),
        };
        Box::pin(futures_util::stream::iter([Ok(AudioChunk { sequence: 0, data })]))
    }

    /// The words a text pipeline is handed.
    fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            // A recorded fixture handed to a pipeline that reads rather than
            // listens: there is nothing to type, and refusing the candidate is
            // more honest than typing the bytes.
            Self::Audio(_) => String::new(),
        }
    }
}

/// Whether these pipelines can be refereed by agreement.
fn reliability_of<'graph>(graphs: impl Iterator<Item = &'graph PipelineGraph>) -> Reliability {
    let mut cores: Vec<Option<String>> = Vec::new();
    for graph in graphs {
        let core = graph.nodes.iter().find_map(|node| match node {
            Node::Core { core, .. } => Some(core.model.provider.clone()),
            _ => None,
        });
        cores.push(core);
    }
    // Compared on the provider definition a core reasons with. Two pipelines
    // naming one definition reason with one model, so any difference in reply
    // came from somewhere a comparison can attribute.
    let first = cores.first().cloned().flatten();
    if cores.iter().all(|core| core.clone() == first) {
        Reliability::Reliable
    } else {
        Reliability::CoresDiffer
    }
}

/// Runs one candidate and collects what it produced and what it cost.
async fn run_candidate(
    state: &AppState,
    providers: &conduit_runtime::Providers,
    name: String,
    graph: &PipelineGraph,
    input: &Input,
    format: AudioFormat,
) -> Candidate {
    let failed = |error: String, node: Option<String>| Candidate {
        pipeline: name.clone(),
        conversation: None,
        outcome: Outcome::Failed { error, node },
        elapsed_ms: None,
        stages: Vec::new(),
    };

    let runner = match Runner::prepare(graph, providers, state.bus.clone()) {
        Ok(runner) => runner,
        Err(error) => return failed(error.to_string(), None),
    };
    let runner = match runner.with_format(format) {
        Ok(runner) => runner,
        Err(error) => return failed(error.to_string(), None),
    }
    .with_idle_timeout(state.turn_idle_timeout());

    if !runner.expects_audio() && matches!(input, Input::Audio(_)) {
        return failed(
            "this pipeline reads rather than listens, so a recorded fixture cannot reach it"
                .to_owned(),
            None,
        );
    }

    // Subscribed before the turn starts, because the events that say when a
    // stage began are published as it begins. The filter cannot name the
    // conversation yet — the id does not exist until `run` returns — so it
    // narrows by pipeline and the collector matches the conversation once it is
    // known.
    let mut events = state.bus.subscribe_filtered(Filter::all());

    let conversation = if runner.expects_audio() {
        runner.run(input.audio())
    } else {
        runner.run_text(input.text())
    };
    let conversation_id = conversation.id;
    let mut replies = conversation.output;

    let mut audio = Vec::new();
    let mut written = String::new();
    let mut failure = None;

    while let Some(reply) = replies.next().await {
        match reply {
            Ok(Reply::Speech(chunk)) => audio.extend_from_slice(&chunk.data),
            Ok(Reply::Text(segment)) => {
                if !written.is_empty() {
                    written.push(' ');
                }
                written.push_str(&segment);
            }
            Err(error) => {
                failure = Some(error.to_string());
                break;
            }
        }
    }

    let observed = observe(&mut events, conversation_id).await;

    if let Some(error) = failure {
        return Candidate {
            pipeline: name,
            conversation: Some(conversation_id),
            outcome: Outcome::Failed { error, node: observed.failed_node },
            elapsed_ms: observed.elapsed_ms,
            stages: observed.stages,
        };
    }

    let reply_audio = if audio.is_empty() {
        None
    } else {
        match conduit_core::wav::package(format, audio.clone()) {
            Ok(upload) => Some(BASE64.encode(&upload.bytes)),
            // The samples were produced; only the container failed. Reporting
            // the candidate as failed would misdescribe a working synthesizer,
            // so the byte count stands and nothing is offered for playback.
            Err(_) => None,
        }
    };
    // What the core said, from the bus. `written` is only populated for a
    // pipeline that renders text, so it is the fallback rather than the source:
    // a voice pipeline's reply arrives as samples and says nothing here.
    let utterance =
        observed.utterance.clone().or_else(|| (!written.is_empty()).then(|| written.clone()));

    // A recognizer comparison turns on what was heard. A pipeline that read its
    // input never heard anything, so it is compared on what it said instead —
    // which is also the only thing a text and a voice pipeline have in common.
    let compared_raw =
        observed.transcript.clone().or_else(|| utterance.clone()).unwrap_or_default();

    Candidate {
        pipeline: name,
        conversation: Some(conversation_id),
        outcome: Outcome::Completed {
            transcript: observed.transcript,
            reply_text: utterance,
            normalized: normalize(&compared_raw),
            compared_raw,
            audio_bytes: audio.len(),
            reply_audio,
        },
        elapsed_ms: observed.elapsed_ms,
        stages: observed.stages,
    }
}

/// What the bus said about one turn.
#[derive(Default)]
struct Observed {
    stages: Vec<StageTiming>,
    elapsed_ms: Option<u64>,
    failed_node: Option<String>,
    /// The final transcript, for a pipeline that listened.
    transcript: Option<String>,
    /// What the core said, whether it was spoken or written.
    utterance: Option<String>,
}

/// Reads this turn's events and reduces them to per-stage durations.
///
/// The turn has already finished by the time this runs, so every event it
/// published is buffered on the subscription and draining stops at the terminal
/// event rather than waiting on a bus that will not speak again.
async fn observe(events: &mut Subscription, conversation: ConversationId) -> Observed {
    let mut first: HashMap<Stage, chrono::DateTime<chrono::Utc>> = HashMap::new();
    let mut last: HashMap<Stage, chrono::DateTime<chrono::Utc>> = HashMap::new();
    let mut order = Vec::new();
    let mut observed = Observed::default();
    let mut started = None;
    let mut ended = None;

    while let Some(envelope) = next_before_deadline(events).await {
        if envelope.conversation != Some(conversation) {
            continue;
        }
        record(&envelope, &mut first, &mut last, &mut order);
        if started.is_none() {
            started = Some(envelope.at);
        }
        match &envelope.event {
            // The last final wins: a recognizer that revises itself has said the
            // later thing, and comparing on a superseded transcript would judge
            // a reading it withdrew.
            Event::SpeechFinal { text, .. } => observed.transcript = Some(text.clone()),
            // Segments are appended, because a turn that spoke a preamble and
            // then an answer said both, per ADR-0010's utterance segments.
            Event::UtteranceSegmentStarted { text, .. } => {
                let utterance = observed.utterance.get_or_insert_with(String::new);
                if !utterance.is_empty() {
                    utterance.push(' ');
                }
                utterance.push_str(text);
            }
            _ => {}
        }
        if let Event::StageFailed { node, recovered, .. } = &envelope.event {
            // A recovered failure is not what failed the turn. Reporting it as
            // the cause would point an operator at a stage that carried on.
            if !recovered {
                observed.failed_node = Some(node.clone());
            }
        }
        if envelope.event.is_terminal() {
            ended = Some(envelope.at);
            break;
        }
    }

    observed.stages = order
        .into_iter()
        .filter_map(|stage| {
            let start = first.get(&stage)?;
            let end = last.get(&stage)?;
            Some(StageTiming {
                stage,
                elapsed_ms: u64::try_from((*end - *start).num_milliseconds()).unwrap_or(0),
            })
        })
        .collect();
    observed.elapsed_ms = started
        .zip(ended)
        .map(|(start, end)| u64::try_from((end - start).num_milliseconds()).unwrap_or(0));
    observed
}

/// Files one event under its stage, remembering the order stages first appeared.
fn record(
    envelope: &Envelope,
    first: &mut HashMap<Stage, chrono::DateTime<chrono::Utc>>,
    last: &mut HashMap<Stage, chrono::DateTime<chrono::Utc>>,
    order: &mut Vec<Stage>,
) {
    let stage = envelope.event.stage();
    if let std::collections::hash_map::Entry::Vacant(slot) = first.entry(stage) {
        slot.insert(envelope.at);
        order.push(stage);
    }
    last.insert(stage, envelope.at);
}

/// The turn's events are already buffered, so a missing terminal event is a
/// bounded wait rather than a hang.
///
/// A turn that ends by having its output stream dropped can finish without
/// publishing a terminal event, and a comparison that waited forever for one
/// would take the whole request down with it. Losing the last few milliseconds
/// of a timing is the better failure.
async fn next_before_deadline(events: &mut Subscription) -> Option<std::sync::Arc<Envelope>> {
    tokio::time::timeout(Duration::from_millis(250), events.recv()).await.ok().flatten()
}

/// Decides whether the candidates that completed said the same thing.
fn judge(candidates: &[Candidate], reliability: Reliability) -> Verdict {
    let mut normalized = Vec::new();
    let mut raw = Vec::new();
    for candidate in candidates {
        if let Outcome::Completed { normalized: text, compared_raw, .. } = &candidate.outcome {
            normalized.push(text.clone());
            raw.push(compared_raw.clone());
        }
    }

    let mut distinct: Vec<String> = Vec::new();
    for reply in &normalized {
        if !distinct.contains(reply) {
            distinct.push(reply.clone());
        }
    }

    // Fewer than two candidates completed, so there is nothing to agree about.
    // Claiming agreement from a single survivor would report a verdict the run
    // did not earn.
    let compared = normalized.len();
    let agreed = compared >= MINIMUM_PIPELINES && distinct.len() == 1;
    let identical = agreed && raw.windows(2).all(|pair| pair[0] == pair[1]);

    Verdict { agreed, identical, reliability, compared, replies: distinct }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_folds_case_punctuation_and_whitespace() {
        assert_eq!(normalize("Turn on the light."), "turn on the light");
        assert_eq!(normalize("turn  on\tthe\nlight"), "turn on the light");
        assert_eq!(normalize("  Turn on the light!  "), "turn on the light");
        assert_eq!(normalize("Turn on the light?"), "turn on the light");
    }

    #[test]
    fn normalization_treats_differing_words_as_differing() {
        // The whole point: normalization must not erase the differences a
        // comparison exists to find.
        assert_ne!(normalize("turn on the light"), normalize("turn on the lights"));
        assert_ne!(normalize("two"), normalize("2"));
        assert_ne!(normalize("turn on the light"), normalize("turn off the light"));
    }

    #[test]
    fn normalization_is_idempotent() {
        let once = normalize("Turn on the light.");
        assert_eq!(normalize(&once), once);
    }

    #[test]
    fn normalization_of_nothing_is_nothing() {
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("   "), "");
        assert_eq!(normalize("..."), "");
    }

    fn completed(pipeline: &str, reply: &str) -> Candidate {
        Candidate {
            pipeline: pipeline.to_owned(),
            conversation: None,
            outcome: Outcome::Completed {
                transcript: Some(reply.to_owned()),
                reply_text: Some(reply.to_owned()),
                compared_raw: reply.to_owned(),
                normalized: normalize(reply),
                audio_bytes: 0,
                reply_audio: None,
            },
            elapsed_ms: Some(1),
            stages: Vec::new(),
        }
    }

    fn failed(pipeline: &str) -> Candidate {
        Candidate {
            pipeline: pipeline.to_owned(),
            conversation: None,
            outcome: Outcome::Failed { error: "boom".to_owned(), node: Some("stt".to_owned()) },
            elapsed_ms: None,
            stages: Vec::new(),
        }
    }

    #[test]
    fn identical_replies_agree_and_are_identical() {
        let verdict = judge(
            &[completed("a", "the light is on"), completed("b", "the light is on")],
            Reliability::Reliable,
        );

        assert!(verdict.agreed);
        assert!(verdict.identical);
        assert_eq!(verdict.compared, 2);
        assert_eq!(verdict.replies, vec!["the light is on".to_owned()]);
    }

    #[test]
    fn replies_differing_only_in_punctuation_agree_without_being_identical() {
        let verdict = judge(
            &[completed("a", "The light is on."), completed("b", "the light is on")],
            Reliability::Reliable,
        );

        assert!(verdict.agreed, "normalization is what makes these comparable");
        assert!(
            !verdict.identical,
            "an operator must be able to tell equivalence from lenience"
        );
    }

    #[test]
    fn differing_replies_disagree_and_both_are_reported() {
        let verdict = judge(
            &[completed("a", "the light is on"), completed("b", "the lights are on")],
            Reliability::Reliable,
        );

        assert!(!verdict.agreed);
        assert_eq!(verdict.replies.len(), 2, "both readings are what there is to judge");
    }

    #[test]
    fn a_failed_candidate_is_excluded_rather_than_counted_as_disagreeing() {
        // The tripwire. Counting a crash as a disagreement is what would make
        // this report actively misleading: an operator would read a broken
        // provider as a recognition difference and change the wrong thing.
        let verdict = judge(
            &[
                completed("a", "the light is on"),
                completed("b", "the light is on"),
                failed("c"),
            ],
            Reliability::Reliable,
        );

        assert!(verdict.agreed, "the two that ran said the same thing");
        assert_eq!(verdict.compared, 2, "the failure is not a third opinion");
        assert_eq!(verdict.replies.len(), 1);
    }

    #[test]
    fn one_survivor_agrees_with_nothing() {
        let verdict =
            judge(&[completed("a", "the light is on"), failed("b")], Reliability::Reliable);

        assert!(
            !verdict.agreed,
            "a single completed candidate has not been compared with anything"
        );
        assert_eq!(verdict.compared, 1);
    }

    #[test]
    fn every_candidate_failing_agrees_with_nothing() {
        let verdict = judge(&[failed("a"), failed("b")], Reliability::Reliable);

        assert!(!verdict.agreed);
        assert_eq!(verdict.compared, 0);
        assert!(verdict.replies.is_empty());
    }

    #[test]
    fn reliability_is_carried_into_the_verdict() {
        let verdict = judge(
            &[completed("a", "the light is on"), completed("b", "the light is on")],
            Reliability::CoresDiffer,
        );

        assert_eq!(verdict.reliability, Reliability::CoresDiffer);
        assert!(verdict.agreed, "the verdict is still computed, only marked");
    }

    #[test]
    fn pipelines_sharing_a_core_are_refereeable() {
        let graphs = [
            conduit_core::testing::voice_graph("a")
                .stt("whisper")
                .core("ollama")
                .tts("piper")
                .build(),
            conduit_core::testing::voice_graph("b")
                .stt("sherpa")
                .core("ollama")
                .tts("piper")
                .build(),
        ];

        assert_eq!(reliability_of(graphs.iter()), Reliability::Reliable);
    }

    #[test]
    fn pipelines_reasoning_with_different_cores_are_not_refereeable() {
        let graphs = [
            conduit_core::testing::voice_graph("a")
                .stt("whisper")
                .core("ollama")
                .tts("piper")
                .build(),
            conduit_core::testing::voice_graph("b")
                .stt("whisper")
                .core("anthropic")
                .tts("piper")
                .build(),
        ];

        assert_eq!(reliability_of(graphs.iter()), Reliability::CoresDiffer);
    }
}
