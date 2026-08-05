//! Frontend contract artifact gate.
//!
//! Rust owns the API and event wire vocabulary. The Operator Console consumes
//! generated TypeScript and fixtures checked in under `frontend/src/contracts`.
//! This test fails when those artifacts drift from the Rust source of truth.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use conduit_api::pipelines::PipelineView;
use conduit_api::status::{
    ActiveTurnStatus, ComponentHealth, ComponentHealthState, ComponentKind, ConnectedSatellite,
    EventStreamContract, LaunchState, OperatorStatusSnapshot, PipelineHealth,
    PipelineHealthState, PipelineStatus, ProviderKind, ProviderStatus, ProviderStatusState,
    RecentlyActiveSatellite, RuntimeFailure, RuntimeState, SatelliteStatus,
    SnapshotEventBinding, SnapshotResource, StaleState,
};
use conduit_core::event::{Envelope, Event};
use conduit_core::graph::{
    Edge, Modality, ModelBinding, Node, PipelineGraph, ReasoningCore, DEFAULT_MAX_ROUNDS,
};
use conduit_core::id::{ConversationId, DeviceId, EventId, TraceId, TurnId};
use uuid::Uuid;

const UPDATE_ENV: &str = "CONDUIT_UPDATE_FRONTEND_CONTRACTS";

#[test]
fn frontend_contract_artifacts_are_current() {
    let root = repo_root();
    let artifacts = contract_artifacts();
    let update = env::var_os(UPDATE_ENV).is_some();

    for artifact in artifacts {
        let path = root.join(artifact.path);
        if update {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create contract artifact directory");
            }
            fs::write(&path, artifact.contents).expect("write contract artifact");
            continue;
        }

        let actual = fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!("{} is missing or unreadable: {error}", artifact.path)
        });
        assert_eq!(
            actual, artifact.contents,
            "{} is stale; run `{UPDATE_ENV}=1 cargo test -p conduit-api --test frontend_contract`",
            artifact.path
        );
    }
}

#[test]
fn frontend_event_bindings_name_generated_event_variants() {
    let event_names = Event::contract_examples()
        .into_iter()
        .map(|event| {
            let json = serde_json::to_value(&event).expect("event serializes");
            assert_eq!(
                json["type"].as_str(),
                Some(event.contract_type()),
                "contract type helper must match serde"
            );
            event.contract_type().to_owned()
        })
        .collect::<BTreeSet<_>>();

    for binding in status_fixture().event_stream.bindings {
        for event in binding.events {
            assert!(
                event_names.contains(&event),
                "{event} is bound to {:?} but has no generated frontend event fixture",
                binding.resource
            );
        }
    }
}

fn contract_artifacts() -> Vec<Artifact> {
    let status = status_fixture();
    let events = event_fixtures();
    let pipeline = pipeline_fixture();
    let turn = turn_snapshot_fixture();

    vec![
        Artifact {
            path: "frontend/src/contracts/client.ts",
            contents: client_types(&pipeline, &turn),
        },
        Artifact { path: "frontend/src/contracts/status.ts", contents: status_types(&status) },
        Artifact { path: "frontend/src/contracts/events.ts", contents: event_types(&events) },
        Artifact {
            path: "frontend/src/contracts/fixtures/pipeline.view.json",
            contents: pretty_json(&pipeline),
        },
        Artifact {
            path: "frontend/src/contracts/fixtures/turn.snapshot.json",
            contents: pretty_json(&turn),
        },
        Artifact {
            path: "frontend/src/contracts/fixtures/status.snapshot.json",
            contents: pretty_json(&status),
        },
        Artifact {
            path: "frontend/src/contracts/fixtures/events.json",
            contents: pretty_json(&events),
        },
    ]
}

struct Artifact {
    path: &'static str,
    contents: String,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate lives under crates/conduit-api")
        .to_path_buf()
}

fn status_fixture() -> OperatorStatusSnapshot {
    let device = device_id(1);
    let conversation = conversation_id(2);
    let turn = turn_id(3);
    let trace = trace_id(4);

    OperatorStatusSnapshot {
        generated_at: Utc.with_ymd_and_hms(2026, 8, 1, 1, 2, 3).unwrap(),
        runtime: RuntimeState {
            launch_state: LaunchState::OperationsWorkspace,
            stale_state: StaleState::Fresh,
        },
        pipelines: vec![PipelineStatus {
            name: "kitchen".to_owned(),
            usable: true,
            health: PipelineHealth {
                state: PipelineHealthState::Unhealthy,
                summary: "speech synthesis failed after the model completed".to_owned(),
                last_successful_turn: None,
                last_failed_turn: Some(turn),
            },
            components: vec![
                ComponentHealth {
                    kind: ComponentKind::Reasoning,
                    provider: Some("openai-primary".to_owned()),
                    state: ComponentHealthState::Healthy,
                    detail: Some("last invoked turn completed".to_owned()),
                    last_turn: Some(turn),
                },
                ComponentHealth {
                    kind: ComponentKind::Synthesis,
                    provider: Some("piper-local".to_owned()),
                    state: ComponentHealthState::Unhealthy,
                    detail: Some("connection refused".to_owned()),
                    last_turn: Some(turn),
                },
            ],
            affected_providers: vec!["piper-local".to_owned()],
        }],
        providers: vec![ProviderStatus {
            id: "piper-local".to_owned(),
            kind: ProviderKind::Tts,
            provider: Some("piper".to_owned()),
            label: Some("Piper".to_owned()),
            version: Some("0.1.0".to_owned()),
            state: ProviderStatusState::Configured,
            configured: true,
            reachable: false,
            proven_by_turn: None,
            message: Some("no successful reachability check yet".to_owned()),
            affects_pipelines: vec!["kitchen".to_owned()],
            offers_tools: Vec::new(),
        }],
        satellites: SatelliteStatus {
            connected: vec![ConnectedSatellite {
                device,
                name: "Kitchen Satellite".to_owned(),
                connected_since: Utc.with_ymd_and_hms(2026, 8, 1, 1, 1, 50).unwrap(),
                conversation: Some(conversation),
                pipeline: "kitchen".to_owned(),
            }],
            recently_active: vec![RecentlyActiveSatellite {
                device,
                name: "Kitchen Satellite".to_owned(),
                last_seen_at: Utc.with_ymd_and_hms(2026, 8, 1, 1, 1, 58).unwrap(),
                last_event: "TtsStarted".to_owned(),
            }],
            recent_window_seconds: 300,
        },
        active_turns: vec![ActiveTurnStatus {
            pipeline: "kitchen".to_owned(),
            conversation,
            turn,
            trace,
            started_at: Utc.with_ymd_and_hms(2026, 8, 1, 1, 1, 59).unwrap(),
            invoked_components: vec![ComponentKind::Reasoning, ComponentKind::Synthesis],
        }],
        recent_failures: vec![RuntimeFailure {
            pipeline: "kitchen".to_owned(),
            turn: Some(turn),
            component: ComponentKind::Synthesis,
            provider: Some("piper-local".to_owned()),
            message: "connection refused".to_owned(),
            at: Utc.with_ymd_and_hms(2026, 8, 1, 1, 2, 1).unwrap(),
        }],
        event_stream: EventStreamContract {
            route: "/v1/events".to_owned(),
            stale_state_on_disconnect: StaleState::Stale,
            refresh_snapshot_after_reconnect: true,
            bindings: vec![
                SnapshotEventBinding {
                    resource: SnapshotResource::PipelineHealth,
                    events: vec![
                        "TurnStarted".to_owned(),
                        "StageFailed".to_owned(),
                        "ConversationCompleted".to_owned(),
                        "ConversationCancelled".to_owned(),
                    ],
                },
                SnapshotEventBinding {
                    resource: SnapshotResource::ActiveTurns,
                    events: vec![
                        "TurnStarted".to_owned(),
                        "ConversationCompleted".to_owned(),
                        "ConversationCancelled".to_owned(),
                    ],
                },
                SnapshotEventBinding {
                    resource: SnapshotResource::RecentFailures,
                    events: vec!["StageFailed".to_owned(), "ConversationCompleted".to_owned()],
                },
                SnapshotEventBinding {
                    resource: SnapshotResource::SatelliteStatus,
                    events: vec![
                        "ConversationStarted".to_owned(),
                        "AudioStarted".to_owned(),
                        "ConversationCompleted".to_owned(),
                        "ConversationCancelled".to_owned(),
                    ],
                },
            ],
        },
    }
}

fn event_fixtures() -> Vec<Envelope> {
    Event::contract_examples()
        .into_iter()
        .enumerate()
        .map(|(index, event)| Envelope {
            id: event_id(100 + index as u128),
            trace: trace_id(200),
            at: Utc.with_ymd_and_hms(2026, 8, 1, 2, 0, index as u32).unwrap(),
            device: Some(device_id(201)),
            conversation: Some(conversation_id(202)),
            pipeline: Some("kitchen".to_owned()),
            event,
        })
        .collect()
}

fn pipeline_fixture() -> PipelineView {
    let graph = PipelineGraph::new("kitchen")
        .with_node(Node::source("mic", "websocket", Modality::Audio))
        .with_node(Node::stt("stt", "whisper"))
        .with_node(Node::Core {
            id: "llm".to_owned(),
            core: ReasoningCore {
                // The fixture names a model because that is the point of a
                // binding: the frontend has to render a field the wire may
                // omit.
                model: ModelBinding {
                    provider: "openai".to_owned(),
                    model: Some("gpt-4o-mini".to_owned()),
                    settings: serde_json::Map::new(),
                },
                system: None,
                tools: Vec::new(),
                memory: Vec::new(),
                max_rounds: DEFAULT_MAX_ROUNDS,
            },
        })
        .with_node(Node::tts("tts", "piper-local"))
        .with_node(Node::sink("speaker", "websocket", Modality::Audio))
        .with_edge(Edge::new("mic", "stt"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
        .with_edge(Edge::new("tts", "speaker"));

    let order = graph
        .topological_order()
        .expect("fixture graph is valid")
        .iter()
        .map(|node| node.id().clone())
        .collect();

    PipelineView { graph, order }
}

fn turn_snapshot_fixture() -> serde_json::Value {
    serde_json::json!({
        "turn_id": turn_id(3),
        "conversation_id": conversation_id(2),
        "pipeline_name": "kitchen",
        "status": "completed",
        "started_at": "2026-08-01T01:01:59Z",
        "ended_at": "2026-08-01T01:02:02Z",
        "sequence": 5,
        "items": [
            {
                "kind": "utterance_segment",
                "id": "assistant-preamble-1",
                "sequence": 2,
                "role": "assistant_preamble",
                "modality": "audio",
                "text": "I will check the lights.",
                "started_at": "2026-08-01T01:02:00Z",
                "evidence": [event_id(301)]
            },
            {
                "kind": "tool_batch",
                "id": "round-1",
                "sequence": 3,
                "model_round": 1,
                "calls": [
                    {
                        "id": "call_contract",
                        "name": "lights.turn_on",
                        "status": "completed",
                        "duration_ms": 34,
                        "evidence": [event_id(302), event_id(303), event_id(304)]
                    }
                ],
                "started_at": "2026-08-01T01:02:00Z",
                "completed_at": "2026-08-01T01:02:01Z",
                "evidence": [event_id(300)]
            },
            {
                "kind": "utterance_segment",
                "id": "assistant-response-1",
                "sequence": 4,
                "role": "assistant_response",
                "modality": "audio",
                "text": "The lights are on.",
                "started_at": "2026-08-01T01:02:01Z",
                "evidence": [event_id(305)]
            }
        ]
    })
}

fn client_types(pipeline: &PipelineView, turn: &serde_json::Value) -> String {
    let mut text = generated_header().to_owned();
    text.push_str(&format!(
        r#"import type {{ EventEnvelope }} from "./events";
import type {{ OperatorStatusSnapshot, ProviderStatus }} from "./status";

export type DateTimeString = string;
export type IdString = string;
export type NodeKind =
  | "source"
  | "wake_word"
  | "stt"
  | "speaker_id"
  | "core"
  | "transform"
  | "tts"
  | "sink";

export type MemoryMode = "read" | "write" | "read_write";
export type MemoryScope = "conversation" | "speaker" | "global";
export type Modality = "audio" | "text" | "utterance";

export interface PipelineNodeBase {{
  id: IdString;
  provider: string;
}}

/// Provider-specific settings, whose names and value types come from the
/// settings schema the provider's descriptor declares rather than from this
/// contract — which is the point: adding a setting to a provider must not mean
/// regenerating the frontend's types.
export type ProviderSettings = Record<string, unknown>;

export type ConfirmPolicy = "never" | "always";

/// `settings` is what this pipeline overrides on the Configured Provider it
/// names, and nothing more: everything the provider was configured with still
/// applies to settings the binding leaves out.
export interface ModelBinding {{
  provider: string;
  model?: string;
  settings?: ProviderSettings;
}}

export interface ToolBinding {{
  provider: string;
  confirm: ConfirmPolicy;
}}

export interface MemoryBinding {{
  provider: string;
  mode: MemoryMode;
  scope?: MemoryScope;
  limit: number;
}}

export interface ReasoningCore {{
  model: ModelBinding;
  system?: string;
  tools?: ToolBinding[];
  memory?: MemoryBinding[];
  max_rounds: number;
}}

export type PipelineNode =
  | (PipelineNodeBase & {{ kind: "source"; modality?: Modality }})
  | (PipelineNodeBase & {{ kind: "wake_word" }})
  | (PipelineNodeBase & {{ kind: "stt"; settings?: ProviderSettings }})
  | (PipelineNodeBase & {{ kind: "speaker_id" }})
  | (PipelineNodeBase & {{ kind: "transform" }})
  | (PipelineNodeBase & {{ kind: "tts"; voice?: string; settings?: ProviderSettings }})
  | (PipelineNodeBase & {{ kind: "sink"; modality?: Modality }})
  | {{ kind: "core"; id: IdString; core: ReasoningCore }};

export interface PipelineEdge {{
  from: IdString;
  to: IdString;
  port?: string;
}}

export interface PipelineGraph {{
  name: string;
  nodes: PipelineNode[];
  edges: PipelineEdge[];
}}

export interface PipelineView {{
  graph: PipelineGraph;
  order: IdString[];
}}

export type AudioEncoding = "pcm_s16_le" | "pcm_f32_le" | "opus" | "flac";

export interface AudioFormat {{
  encoding: AudioEncoding;
  sample_rate: number;
  channels: number;
}}

export interface PipelineTestRequest {{
  utterance?: string;
  format?: AudioFormat;
}}

export interface PipelineTestResult {{
  pipeline: string;
  conversation: IdString;
  status: "completed";
  audio_bytes: number;
  reply_text?: string;
  reply_audio?: string;
}}

export type ComponentConfigValueType =
  | "string"
  | "boolean"
  | "integer"
  | "string_list";
/// A hint about the input a field wants rather than about the value it holds:
/// `multiline` accepts everything a single-line box does, and says a person is
/// going to want to see more than one line of it at once.
export type ComponentConfigFormat = "url" | "multiline";

export interface ComponentConfigProperty {{
  type: ComponentConfigValueType;
  format?: ComponentConfigFormat;
  pattern?: string;
  /// The only values this field accepts, when it is a closed set — a wake
  /// word engine, for instance. Absent means the field is open.
  options?: string[];
  /// What the field starts as when a form is opened fresh. A suggestion rather
  /// than a constraint: an operator can replace it, and a definition that omits
  /// the field is not filled in behind their back.
  default?: string;
}}

export interface ComponentConfigSchema {{
  properties: Record<string, ComponentConfigProperty>;
  required: string[];
}}

export interface ProviderComponentDescriptor {{
  id: string;
  label: string;
  kind: ProviderCapability;
  definition_variant: ProviderDefinitionVariantType;
  schema: ComponentConfigSchema;
}}

export interface ProviderComponentCatalog {{
  components: ProviderComponentDescriptor[];
}}

export type ProviderCapability =
  | "stt"
  | "llm"
  | "tts"
  | "transform"
  | "tool"
  | "wake"
  | "speaker_id"
  | "memory";
/// Inner provider definition variant, paired with `kind` (the outer variant)
/// to name the full two-level provider definition variant.
export type ProviderDefinitionVariantType =
  | "openai"
  | "anthropic"
  | "bedrock"
  | "wyoming"
  | "elevenlabs"
  | "deepgram"
  | "polly"
  | "google"
  | "marytts"
  | "mcp"
  | "builtin"
  | "script"
  | "openwakeword"
  | "nanowakeword"
  | "microwakeword"
  | "http"
  | "diarization_server"
  | "pgvector";

/// The three wake word detectors Conduit speaks to. Each is its own wake
/// variant, because the three do not run in the same places.
export type WakeEngine = "microwakeword" | "openwakeword" | "nanowakeword";

/// The embedding models a speaker identification service may be running.
export type SpeakerEngine = "speechbrain" | "resemblyzer" | "pyannote";

export type ProviderSecret =
  | {{ type: "inline"; value: string }}
  | {{ type: "external"; reference: string }}
  | {{ type: "redacted" }};

/// A language model endpoint.
///
/// The two HTTP wire formats take the same settings, so they differ only in the
/// tag that says which one to speak. Bedrock is a case of its own: it is named
/// by region rather than by URL, because the region is the endpoint, and its
/// credential is usually the deployment's rather than one an operator typed.
export type LlmVariant =
  | {{
      type: "openai" | "anthropic";
      base_url: string;
      api_key?: ProviderSecret;
      models: string[];
      streaming: boolean;
      system_prompt?: string;
    }}
  | {{
      type: "bedrock";
      region: string;
      profile?: string;
      api_key?: ProviderSecret;
      models: string[];
      streaming: boolean;
      system_prompt?: string;
    }};

/// A speech recognizer.
///
/// The vendors that discover their own credentials have no `api_key` field at
/// all rather than an optional one — Google's arrive from the environment, so a
/// box to paste one into would be a box that does nothing. ElevenLabs has no
/// `streaming` flag for the same kind of reason: its realtime transcription is a
/// separate websocket protocol, not a setting on the batch endpoint.
export type SttVariant =
  | {{
      type: "openai";
      base_url: string;
      model: string;
      api_key?: ProviderSecret;
      stream: boolean;
    }}
  | {{
      type: "wyoming";
      url: string;
      model?: string;
      streaming: boolean;
    }}
  | {{
      type: "elevenlabs";
      api_key?: ProviderSecret;
      model?: string;
    }}
  | {{
      type: "google";
      language?: string;
      model?: string;
    }};

/// A speech synthesizer.
///
/// MaryTTS names a URL and no credential: it is self-hosted and has no
/// authentication to configure. Google, again, discovers its own. Deepgram takes
/// a key and a model and nothing else, because the model is the voice.
export type TtsVariant =
  | {{
      type: "openai";
      base_url: string;
      model: string;
      api_key?: ProviderSecret;
      voices: string[];
    }}
  | {{
      type: "wyoming";
      url: string;
      voice?: string;
      streaming: boolean;
    }}
  | {{
      type: "deepgram";
      api_key?: ProviderSecret;
      model?: string;
    }}
  | {{
      type: "polly";
      region: string;
      profile?: string;
      voice?: string;
      engine?: string;
    }}
  | {{
      type: "elevenlabs";
      api_key?: ProviderSecret;
      model?: string;
      voice?: string;
    }}
  | {{
      type: "google";
      language?: string;
      voice?: string;
    }}
  | {{
      type: "marytts";
      url: string;
      voice?: string;
      locale?: string;
    }};

export type ToolVariant = {{
  type: "mcp";
  transport: McpTransport;
}};

/// One rewriting rule that ships with Conduit. Named rather than configurable
/// because each is a statement about how speech differs from writing.
export type TransformRule =
  | "strip_emoji"
  | "markdown_to_speech"
  | "collapse_whitespace";

/// A rewrite of what a model said, on its way to being spoken.
///
/// `builtin` names rules somebody had to write and release; `script` is the
/// other half of that trade, and takes effect on the next utterance. A script
/// carries no credential — its source is the definition rather than a secret in
/// it — so it survives a redacted read-back whole, and the editor reopens the
/// script the operator wrote.
export type TransformVariant =
  | {{
      type: "builtin";
      rules: TransformRule[];
    }}
  | {{
      type: "script";
      engine: ScriptEngine;
      source: string;
      /// How long one evaluation may run. Optional because a definition that
      /// names no deadline is stored with one anyway: a transform sits inside
      /// the turn loop, so a script that never returns would end every turn on
      /// the pipeline rather than one segment.
      timeout_ms?: number;
    }};

/// The interpreter a scripted transform runs on. One today, and still named in
/// the definition rather than assumed, so a second engine could arrive without
/// every stored script silently changing language.
export type ScriptEngine = "rhai";

/// Where a detector Conduit can score itself is running.
export type WakeRuntime =
  | {{
      where: "local";
      models_dir?: string;
      threshold_percent: number;
    }}
  | {{
      where: "wyoming";
      url: string;
      threshold_percent: number;
    }};

/// Where microWakeWord is running. A different set from `WakeRuntime`, not a
/// subset: its models cannot be scored in process, and it is the only engine
/// small enough for satellite hardware.
export type MicroWakeWordRuntime =
  | {{ where: "device" }}
  | {{
      where: "wyoming";
      url: string;
      threshold_percent: number;
    }};

export type WakeVariant =
  | {{
      type: "openwakeword";
      runtime: WakeRuntime;
      phrases: string[];
    }}
  | {{
      type: "nanowakeword";
      runtime: WakeRuntime;
      phrases: string[];
    }}
  | {{
      type: "microwakeword";
      runtime: MicroWakeWordRuntime;
      phrases: string[];
    }};

export type SpeakerIdVariant =
  | {{
      type: "diarization_server";
      base_url: string;
      threshold_percent: number;
    }}
  | {{
      type: "http";
      base_url: string;
      api_key?: ProviderSecret;
      engine: SpeakerEngine;
      threshold_percent: number;
    }};

/// Where the assistant keeps what it should remember.
///
/// The two are not two places to put the same records: a question phrased in
/// words the stored record never used is found by `pgvector` and missed by
/// `builtin`. Neither of the built-in store's fields is required — nothing
/// written anywhere and a default bound is a real configuration — and the
/// vector store's width is, because it is what the vector column is declared
/// with and nothing can discover it before the first embedding exists.
export type MemoryVariant =
  | {{
      type: "builtin";
      path?: string;
      capacity?: number;
    }}
  | {{
      type: "pgvector";
      url: string;
      embedding_base_url: string;
      api_key?: ProviderSecret;
      embedding_model: string;
      dimensions: number;
    }};

export type ProviderDefinitionVariant =
  | {{ type: "llm"; variant: LlmVariant }}
  | {{ type: "stt"; variant: SttVariant }}
  | {{ type: "tts"; variant: TtsVariant }}
  | {{ type: "transform"; variant: TransformVariant }}
  | {{ type: "tool"; variant: ToolVariant }}
  | {{ type: "wake"; variant: WakeVariant }}
  | {{ type: "speaker_id"; variant: SpeakerIdVariant }}
  | {{ type: "memory"; variant: MemoryVariant }};

export type McpTransport =
  | {{ type: "sse"; url: string }}
  | {{ type: "streamable_http"; url: string }}
  | {{ type: "stdio"; command: string; args: string[] }};

export interface ProviderDefinition {{
  id: string;
  label: string;
  variant: ProviderDefinitionVariant;
}}

/// One voice a synthesizer can speak with.
export interface Voice {{
  id: string;
  name: string;
  language: string;
}}

/// The voices a synthesizer offers.
///
/// An empty list is a real answer: a provider that accepts any voice name has
/// no catalogue, and the editor lets an operator type one instead.
export interface ProviderVoices {{
  provider: string;
  voices: Voice[];
}}

/// The phrases a wake word detector offers.
///
/// An empty list is a real answer, for the same reason: a Wyoming server
/// scores whatever it loaded and enumerates nothing, and a satellite knows
/// only what it was flashed with.
export interface ProviderPhrases {{
  provider: string;
  phrases: string[];
}}

export interface ProviderDefinitionView {{
  id: string;
  label: string;
  kind: ProviderCapability;
  variant: ProviderDefinitionVariant;
  settings?: ProviderSettings;
}}

/// What a rename moved.
///
/// The pipelines are reported rather than counted because a rename of a shared
/// provider rewrites graphs the operator was not looking at, and they should be
/// told which.
export interface ProviderRenameResult {{
  provider: ProviderDefinitionView;
  renamed_pipelines: string[];
}}

export type TurnStatus = "running" | "completed" | "cancelled" | "failed" | "degraded";
export type UtteranceSegmentRole = "assistant_preamble" | "tool_output" | "assistant_response";
export type ToolCallStatus =
  | "requested"
  | "running"
  | "completed"
  | "failed"
  | "denied"
  | "awaiting_confirmation";

export interface TurnList {{
  turns: TurnSummary[];
}}

export interface TurnSummary {{
  turn_id: IdString;
  conversation_id: IdString;
  pipeline_name: string;
  status: TurnStatus;
  started_at: DateTimeString;
  ended_at?: DateTimeString;
  sequence: number;
}}

export interface TurnSnapshot {{
  turn_id: IdString;
  conversation_id: IdString;
  pipeline_name: string;
  status: TurnStatus;
  cancellation_reason?: string;
  started_at: DateTimeString;
  ended_at?: DateTimeString;
  sequence: number;
  items: ReconstructionItem[];
}}

export type ReconstructionItem = UtteranceSegmentItem | ToolBatchItem;

export interface UtteranceSegmentItem {{
  kind: "utterance_segment";
  id: string;
  sequence: number;
  role: UtteranceSegmentRole;
  modality: Modality;
  text: string;
  started_at: DateTimeString;
  evidence: IdString[];
}}

export interface ToolBatchItem {{
  kind: "tool_batch";
  id: string;
  sequence: number;
  model_round: number;
  calls: ToolCall[];
  started_at: DateTimeString;
  completed_at?: DateTimeString;
  evidence: IdString[];
}}

export interface ToolCall {{
  id: IdString;
  name?: string;
  status: ToolCallStatus;
  duration_ms?: number;
  error?: string;
  evidence: IdString[];
}}

export interface RawTurnEvents {{
  turn_id: IdString;
  events: EventEnvelope[];
}}

export interface TurnReconstructionUpdate {{
  turn_id: IdString;
  conversation_id: IdString;
  pipeline_name: string;
  sequence: number;
  update: "snapshot_changed";
}}

export interface ConduitApiClientConfig {{
  baseUrl: string;
  headers?: () => HeadersInit;
  fetch?: typeof fetch;
}}

/// Somebody the deployment has named, and possibly recorded.
///
/// The id is Conduit's and the identification service stores it as an opaque
/// label, so the name only ever exists here.
export interface EnrolledSpeaker {{
  id: IdString;
  name: string;
  samples: number;
  /// Which identification provider holds the voice prints. Absent until a
  /// sample has been accepted.
  provider?: string;
  created_at: DateTimeString;
  /// When a sample was last accepted, absent for somebody only named.
  enrolled_at?: DateTimeString;
}}

export interface ConduitApiClient {{
  readonly routes: typeof conduitApiRoutes;
  status: () => Promise<OperatorStatusSnapshot>;
  listPipelines: () => Promise<string[]>;
  listProviderComponents: () => Promise<ProviderComponentCatalog>;
  listProviderDefinitions: () => Promise<string[]>;
  getProviderDefinition: (id: string) => Promise<ProviderDefinitionView>;
  putProviderDefinition: (
    id: string,
    definition: ProviderDefinition,
  ) => Promise<ProviderDefinitionView>;
  /// Moves a definition to a new id, rewriting every pipeline that named it.
  ///
  /// A provider id is not private to its definition, so changing it is an
  /// operation rather than a save under the new name: a save would leave the
  /// old definition in place and every pipeline still pointing at it.
  renameProviderDefinition: (
    id: string,
    newId: string,
  ) => Promise<ProviderRenameResult>;
  deleteProviderDefinition: (id: string) => Promise<void>;
  testProviderDefinition: (id: string) => Promise<ProviderStatus>;
  listProviderVoices: (id: string) => Promise<ProviderVoices>;
  listProviderPhrases: (id: string) => Promise<ProviderPhrases>;
  getPipeline: (name: string) => Promise<PipelineView>;
  putPipeline: (name: string, graph: PipelineGraph) => Promise<PipelineView>;
  deletePipeline: (name: string) => Promise<void>;
  validatePipeline: (graph: PipelineGraph) => Promise<PipelineView>;
  testPipeline: (
    name: string,
    request?: PipelineTestRequest,
  ) => Promise<PipelineTestResult>;
  listSpeakers: () => Promise<EnrolledSpeaker[]>;
  createSpeaker: (name: string) => Promise<EnrolledSpeaker>;
  renameSpeaker: (id: string, name: string) => Promise<EnrolledSpeaker>;
  /// Sends one utterance as a WAV file. The service decides whether it is
  /// usable, so the failure an operator sees is the service's own.
  enrollSpeaker: (
    id: string,
    audio: Blob,
    provider?: string,
  ) => Promise<EnrolledSpeaker>;
  deleteSpeaker: (id: string) => Promise<void>;
  listTurns: () => Promise<TurnList>;
  getTurn: (turnId: string) => Promise<TurnSnapshot>;
  getTurnEvents: (turnId: string) => Promise<RawTurnEvents>;
}}

export const conduitApiRoutes = {{
  status: "/v1/status",
  events: "/v1/events",
  turns: "/v1/turns",
  liveTurns: "/v1/turns/live",
  turn: "/v1/turns/{{turn_id}}",
  turnEvents: "/v1/turns/{{turn_id}}/events",
  providerCatalog: "/v1/catalog/providers",
  providers: "/v1/providers",
  provider: "/v1/providers/{{id}}",
  providerRename: "/v1/providers/{{id}}/rename",
  providerTest: "/v1/providers/{{id}}/test",
  providerVoices: "/v1/providers/{{id}}/voices",
  providerPhrases: "/v1/providers/{{id}}/phrases",
  speakers: "/v1/speakers",
  speaker: "/v1/speakers/{{id}}",
  speakerEnroll: "/v1/speakers/{{id}}/enroll",
  pipelines: "/v1/pipelines",
  pipeline: "/v1/pipelines/{{name}}",
  pipelineTest: "/v1/pipelines/{{name}}/test-turn",
  validatePipeline: "/v1/pipelines/validate",
}} as const;

export function createConduitApiClient(
  config: ConduitApiClientConfig,
): ConduitApiClient {{
  const request = config.fetch ?? fetch;

  return {{
    routes: conduitApiRoutes,
    status: () =>
      requestJson<OperatorStatusSnapshot>(request, config, conduitApiRoutes.status),
    listPipelines: () =>
      requestJson<string[]>(request, config, conduitApiRoutes.pipelines),
    listProviderComponents: () =>
      requestJson<ProviderComponentCatalog>(
        request,
        config,
        conduitApiRoutes.providerCatalog,
      ),
    listProviderDefinitions: () =>
      requestJson<string[]>(request, config, conduitApiRoutes.providers),
    getProviderDefinition: (id) =>
      requestJson<ProviderDefinitionView>(request, config, providerRoute(id)),
    putProviderDefinition: (id, definition) =>
      requestJson<ProviderDefinitionView>(request, config, providerRoute(id), {{
        method: "PUT",
        body: JSON.stringify(definition),
      }}),
    renameProviderDefinition: (id, newId) =>
      requestJson<ProviderRenameResult>(request, config, providerRenameRoute(id), {{
        method: "POST",
        body: JSON.stringify({{ id: newId }}),
      }}),
    deleteProviderDefinition: async (id) => {{
      await requestJson<void>(request, config, providerRoute(id), {{
        method: "DELETE",
      }});
    }},
    testProviderDefinition: (id) =>
      requestJson<ProviderStatus>(request, config, providerTestRoute(id), {{
        method: "POST",
      }}),
    listProviderVoices: (id) =>
      requestJson<ProviderVoices>(request, config, providerVoicesRoute(id)),
    listProviderPhrases: (id) =>
      requestJson<ProviderPhrases>(request, config, providerPhrasesRoute(id)),
    getPipeline: (name) =>
      requestJson<PipelineView>(request, config, pipelineRoute(name)),
    putPipeline: (name, graph) =>
      requestJson<PipelineView>(request, config, pipelineRoute(name), {{
        method: "PUT",
        body: JSON.stringify(graph),
      }}),
    deletePipeline: async (name) => {{
      await requestJson<void>(request, config, pipelineRoute(name), {{
        method: "DELETE",
      }});
    }},
    validatePipeline: (graph) =>
      requestJson<PipelineView>(request, config, conduitApiRoutes.validatePipeline, {{
        method: "POST",
        body: JSON.stringify(graph),
      }}),
    testPipeline: (name, testRequest = {{}}) =>
      requestJson<PipelineTestResult>(request, config, pipelineTestRoute(name), {{
        method: "POST",
        body: JSON.stringify(testRequest),
      }}),
    listSpeakers: () =>
      requestJson<EnrolledSpeaker[]>(request, config, conduitApiRoutes.speakers),
    createSpeaker: (name) =>
      requestJson<EnrolledSpeaker>(request, config, conduitApiRoutes.speakers, {{
        method: "POST",
        body: JSON.stringify({{ name }}),
      }}),
    renameSpeaker: (id, name) =>
      requestJson<EnrolledSpeaker>(request, config, speakerRoute(id), {{
        method: "PUT",
        body: JSON.stringify({{ name }}),
      }}),
    enrollSpeaker: (id, audio, provider) =>
      requestJson<EnrolledSpeaker>(
        request,
        config,
        provider
          ? `${{speakerEnrollRoute(id)}}?provider=${{encodeURIComponent(provider)}}`
          : speakerEnrollRoute(id),
        {{
          method: "POST",
          body: audio,
          // Stated rather than left to the browser: a `Blob` with no type
          // sends nothing, and the route has to know it is being handed a
          // file rather than a JSON document.
          headers: {{ "content-type": "audio/wav" }},
        }},
      ),
    deleteSpeaker: async (id) => {{
      await requestJson<void>(request, config, speakerRoute(id), {{
        method: "DELETE",
      }});
    }},
    listTurns: () => requestJson<TurnList>(request, config, conduitApiRoutes.turns),
    getTurn: (turnId) =>
      requestJson<TurnSnapshot>(request, config, turnRoute(turnId)),
    getTurnEvents: (turnId) =>
      requestJson<RawTurnEvents>(request, config, turnEventsRoute(turnId)),
  }};
}}

function speakerRoute(id: string): string {{
  return conduitApiRoutes.speaker.replace("{{id}}", encodeURIComponent(id));
}}

function speakerEnrollRoute(id: string): string {{
  return conduitApiRoutes.speakerEnroll.replace("{{id}}", encodeURIComponent(id));
}}

function pipelineRoute(name: string): string {{
  return conduitApiRoutes.pipeline.replace("{{name}}", encodeURIComponent(name));
}}

function providerRoute(id: string): string {{
  return conduitApiRoutes.provider.replace("{{id}}", encodeURIComponent(id));
}}

function providerRenameRoute(id: string): string {{
  return conduitApiRoutes.providerRename.replace(
    "{{id}}",
    encodeURIComponent(id),
  );
}}

function providerVoicesRoute(id: string): string {{
  return conduitApiRoutes.providerVoices.replace(
    "{{id}}",
    encodeURIComponent(id),
  );
}}

function providerPhrasesRoute(id: string): string {{
  return conduitApiRoutes.providerPhrases.replace(
    "{{id}}",
    encodeURIComponent(id),
  );
}}

function providerTestRoute(id: string): string {{
  return conduitApiRoutes.providerTest.replace(
    "{{id}}",
    encodeURIComponent(id),
  );
}}

function pipelineTestRoute(name: string): string {{
  return conduitApiRoutes.pipelineTest.replace(
    "{{name}}",
    encodeURIComponent(name),
  );
}}

function turnRoute(turnId: string): string {{
  return conduitApiRoutes.turn.replace("{{turn_id}}", encodeURIComponent(turnId));
}}

function turnEventsRoute(turnId: string): string {{
  return conduitApiRoutes.turnEvents.replace("{{turn_id}}", encodeURIComponent(turnId));
}}

async function requestJson<T>(
  request: typeof fetch,
  config: ConduitApiClientConfig,
  route: string,
  init: RequestInit = {{}},
): Promise<T> {{
  const response = await request(new URL(route, config.baseUrl), {{
    ...init,
    headers: {{
      accept: "application/json",
      // A body that is already a file says what it is; only a serialized
      // document needs to be announced as JSON.
      ...(init.body && typeof init.body === "string"
        ? {{ "content-type": "application/json" }}
        : {{}}),
      ...config.headers?.(),
      ...init.headers,
    }},
  }});

  if (!response.ok) {{
    throw new Error(await failureMessage(response));
  }}

  if (response.status === 204) {{
    return undefined as T;
  }}

  return (await response.json()) as T;
}}

/**
 * The API answers every failure with `{{"error": ..., "detail": ...}}`, and the
 * detail is the only part that says why — "no providers are configured" rather
 * than "422". Anything else is a body we cannot read, so the status line is all
 * that is left to report.
 */
async function failureMessage(response: Response): Promise<string> {{
  const fallback =
    `Conduit API request failed: ${{response.status}} ${{response.statusText}}`.trimEnd();

  try {{
    const body = (await response.json()) as {{ detail?: unknown }};
    return typeof body.detail === "string" && body.detail.length > 0
      ? body.detail
      : fallback;
  }} catch {{
    return fallback;
  }}
}}

export const pipelineViewFixture = {} as const satisfies PipelineView;
export const turnSnapshotFixture = {} as const satisfies TurnSnapshot;
"#,
        pretty_json_inline(pipeline),
        pretty_json_inline(turn)
    ));
    text
}

fn status_types(fixture: &OperatorStatusSnapshot) -> String {
    let mut text = generated_header().to_owned();
    text.push_str(&format!(
        r#"import type {{ EventType }} from "./events";

export type DateTimeString = string;
export type IdString = string;

export type LaunchState = "first_run_setup" | "operations_workspace";
export type StaleState = "fresh" | "stale";
export type PipelineHealthState = "not_runnable" | "unproven" | "healthy" | "degraded" | "unhealthy";
export type ComponentKind = "capture" | "transcription" | "reasoning" | "tools" | "synthesis";
export type ComponentHealthState = "not_configured" | "unused" | "unproven" | "healthy" | "degraded" | "unhealthy";
export type ProviderKind =
  | "stt"
  | "llm"
  | "tool"
  | "tts"
  | "transform"
  | "wake"
  | "speaker_id"
  | "memory";
export type ProviderStatusState = "unavailable" | "configured" | "reachable" | "proven";
export type SnapshotResource =
  | "runtime_state"
  | "pipeline_health"
  | "provider_status"
  | "satellite_status"
  | "active_turns"
  | "recent_failures";

export interface OperatorStatusSnapshot {{
  generated_at: DateTimeString;
  runtime: RuntimeState;
  pipelines: PipelineStatus[];
  providers: ProviderStatus[];
  satellites: SatelliteStatus;
  active_turns: ActiveTurnStatus[];
  recent_failures: RuntimeFailure[];
  event_stream: EventStreamContract;
}}

export interface RuntimeState {{
  launch_state: LaunchState;
  stale_state: StaleState;
}}

export interface PipelineStatus {{
  name: string;
  usable: boolean;
  health: PipelineHealth;
  components: ComponentHealth[];
  affected_providers: string[];
}}

export interface PipelineHealth {{
  state: PipelineHealthState;
  summary: string;
  last_successful_turn: IdString | null;
  last_failed_turn: IdString | null;
}}

export interface ComponentHealth {{
  kind: ComponentKind;
  provider: string | null;
  state: ComponentHealthState;
  detail: string | null;
  last_turn: IdString | null;
}}

/// `id` is the selector — what a pipeline node names and what the operator
/// configured. `provider`, `label` and `version` come from the registered
/// implementation's descriptor and are absent when nothing was built under that
/// selector: a definition whose service would not start, or a node naming a
/// provider nobody configured.
export interface ProviderStatus {{
  id: string;
  kind: ProviderKind;
  provider?: string;
  label?: string;
  version?: string;
  state: ProviderStatusState;
  configured: boolean;
  reachable: boolean;
  proven_by_turn: IdString | null;
  message: string | null;
  affects_pipelines: string[];
  offers_tools?: string[];
}}

export interface SatelliteStatus {{
  connected: ConnectedSatellite[];
  recently_active: RecentlyActiveSatellite[];
  recent_window_seconds: number;
}}

export interface ConnectedSatellite {{
  device: IdString;
  name: string;
  connected_since: DateTimeString;
  conversation: IdString | null;
  pipeline: string;
}}

export interface RecentlyActiveSatellite {{
  device: IdString;
  name: string;
  last_seen_at: DateTimeString;
  last_event: string;
}}

export interface ActiveTurnStatus {{
  pipeline: string;
  conversation: IdString;
  turn: IdString;
  trace: IdString;
  started_at: DateTimeString;
  invoked_components: ComponentKind[];
}}

export interface RuntimeFailure {{
  pipeline: string;
  turn: IdString | null;
  component: ComponentKind;
  provider: string | null;
  message: string;
  at: DateTimeString;
}}

export interface EventStreamContract {{
  route: string;
  stale_state_on_disconnect: StaleState;
  refresh_snapshot_after_reconnect: boolean;
  bindings: SnapshotEventBinding[];
}}

export interface SnapshotEventBinding {{
  resource: SnapshotResource;
  events: EventType[];
}}

export const operatorStatusSnapshotFixture = {} as const satisfies OperatorStatusSnapshot;
"#,
        pretty_json_inline(fixture)
    ));
    text
}

fn event_types(fixtures: &[Envelope]) -> String {
    let mut text = generated_header().to_owned();
    text.push_str(&format!(
        r#"export type DateTimeString = string;
export type IdString = string;
export type ToolCallId = string;

export type AudioEncoding = "pcm_s16_le" | "pcm_f32_le" | "opus" | "flac";
export type CancelReason =
  | "barge_in"
  | "idle_timeout"
  | "user_requested"
  | "disconnected"
  | "error"
  | "shutdown";
export type FinishReason = "stop" | "length" | "tool_use" | "cancelled";
export type UtteranceSegmentRole =
  | "assistant_preamble"
  | "tool_output"
  | "assistant_response";
export type Modality = "audio" | "text" | "utterance";

export interface AudioFormat {{
  encoding: AudioEncoding;
  sample_rate: number;
  channels: number;
}}

export interface EventEnvelope {{
  id: IdString;
  trace: IdString;
  at: DateTimeString;
  device: IdString | null;
  conversation: IdString | null;
  pipeline: string | null;
  event: Event;
}}

export type Event =
  | {{ type: "WakeWordDetected"; phrase: string; confidence: number }}
  | {{ type: "WakeWordRejected"; phrase: string; confidence: number }}
  | {{ type: "AudioStarted"; format: AudioFormat }}
  | {{ type: "AudioChunkReceived"; sequence: number; bytes: number }}
  | {{ type: "AudioFinished"; duration_ms: number }}
  | {{ type: "SpeechPartial"; text: string }}
  | {{ type: "SpeechFinal"; text: string; confidence: number | null; language: string | null }}
  | {{ type: "SpeakerIdentified"; speaker: IdString | null; confidence: number }}
  | {{ type: "ConversationStarted" }}
  | {{ type: "TurnStarted"; turn: IdString }}
  | {{ type: "ConversationCancelled"; reason: CancelReason }}
  | {{ type: "ConversationCompleted" }}
  | {{ type: "LlmRequestStarted"; model: string }}
  | {{ type: "LlmToken"; delta: string }}
  | {{
      type: "LlmFinished";
      reason: FinishReason;
      prompt_tokens: number | null;
      completion_tokens: number | null;
    }}
  | {{ type: "ToolRequested"; call: ToolCallId; name: string }}
  | {{ type: "ToolBatchStarted"; batch: string; calls: ToolCallId[]; model_round: number }}
  | {{ type: "ToolStarted"; call: ToolCallId }}
  | {{ type: "ToolConfirmationRequested"; call: ToolCallId; prompt: string }}
  | {{ type: "ToolCompleted"; call: ToolCallId; duration_ms: number }}
  | {{ type: "ToolFailed"; call: ToolCallId; error: string }}
  | {{ type: "TtsStarted"; voice: string }}
  | {{
      type: "UtteranceSegmentStarted";
      segment: string;
      role: UtteranceSegmentRole;
      modality: Modality;
      text: string;
    }}
  | {{ type: "AudioStreaming"; sequence: number; bytes: number }}
  | {{ type: "TtsFinished"; duration_ms: number }}
  | {{ type: "StageFailed"; node: string; error: string; recovered: boolean }};

export type EventType = Event["type"];

export const eventEnvelopeFixtures = {} as const satisfies readonly EventEnvelope[];
"#,
        pretty_json_inline(fixtures)
    ));
    text
}

fn generated_header() -> &'static str {
    "// Generated by crates/conduit-api/tests/frontend_contract.rs. Do not edit by hand.\n\n"
}

fn pretty_json<T: serde::Serialize + ?Sized>(value: &T) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("serialize fixture");
    text.push('\n');
    text
}

fn pretty_json_inline<T: serde::Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string_pretty(value).expect("serialize fixture")
}

fn event_id(value: u128) -> EventId {
    EventId::from_uuid(uuid(value))
}

fn conversation_id(value: u128) -> ConversationId {
    ConversationId::from_uuid(uuid(value))
}

fn turn_id(value: u128) -> TurnId {
    TurnId::from_uuid(uuid(value))
}

fn device_id(value: u128) -> DeviceId {
    DeviceId::from_uuid(uuid(value))
}

fn trace_id(value: u128) -> TraceId {
    TraceId::from_uuid(uuid(value))
}

fn uuid(value: u128) -> Uuid {
    Uuid::from_u128(value)
}
