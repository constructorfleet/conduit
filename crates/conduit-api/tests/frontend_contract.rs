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
use conduit_core::graph::{Edge, Node, NodeKind, PipelineGraph};
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
            state: ProviderStatusState::Configured,
            configured: true,
            reachable: false,
            proven_by_turn: None,
            message: Some("no successful reachability check yet".to_owned()),
            affects_pipelines: vec!["kitchen".to_owned()],
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
        .with_node(Node::new("mic", NodeKind::Source, "websocket"))
        .with_node(Node::new("stt", NodeKind::Stt, "whisper"))
        .with_node(Node::new("llm", NodeKind::Llm, "openai"))
        .with_node(Node::new("tts", NodeKind::Tts, "piper-local"))
        .with_node(Node::new("speaker", NodeKind::Sink, "websocket"))
        .with_edge(Edge::new("mic", "stt"))
        .with_edge(Edge::new("stt", "llm"))
        .with_edge(Edge::new("llm", "tts"))
        .with_edge(Edge::new("tts", "speaker"));

    let order = graph
        .topological_order()
        .expect("fixture graph is valid")
        .iter()
        .map(|node| node.id.clone())
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
                "kind": "spoken_segment",
                "id": "assistant-preamble-1",
                "sequence": 2,
                "role": "assistant_preamble",
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
                "kind": "spoken_segment",
                "id": "assistant-response-1",
                "sequence": 4,
                "role": "assistant_response",
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
  | "router"
  | "llm"
  | "tool"
  | "memory"
  | "tts"
  | "sink";

export interface PipelineNode {{
  id: IdString;
  kind: NodeKind;
  provider: string;
}}

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
  reply_audio?: string;
}}

export type ComponentConfigValueType = "string" | "boolean";
export type ComponentConfigFormat = "url";

export interface ComponentConfigProperty {{
  type: ComponentConfigValueType;
  format?: ComponentConfigFormat;
  pattern?: string;
}}

export interface ComponentConfigSchema {{
  properties: Record<string, ComponentConfigProperty>;
  required: string[];
}}

export interface ProviderComponentDescriptor {{
  id: string;
  label: string;
  kind: NodeKind;
  definition_variant: ProviderDefinitionVariantType;
  schema: ComponentConfigSchema;
}}

export interface ProviderComponentCatalog {{
  components: ProviderComponentDescriptor[];
}}

export type ProviderCapability = "stt" | "llm" | "tts" | "tool";
export type ProviderDefinitionVariantType =
  | "openai_llm"
  | "openai_stt"
  | "openai_tts"
  | "wyoming_stt"
  | "wyoming_tts"
  | "mcp_tool";

export type ProviderSecret =
  | {{ type: "inline"; value: string }}
  | {{ type: "external"; reference: string }}
  | {{ type: "redacted" }};

export type ProviderDefinitionVariant =
  | {{
      type: "openai_llm";
      base_url: string;
      api_key?: ProviderSecret;
      models: string[];
      streaming: boolean;
      system_prompt?: string;
    }}
  | {{
      type: "openai_stt";
      base_url: string;
      model: string;
      api_key?: ProviderSecret;
      stream: boolean;
    }}
  | {{
      type: "openai_tts";
      base_url: string;
      model: string;
      api_key?: ProviderSecret;
      voices: string[];
    }}
  | {{
      type: "wyoming_stt";
      url: string;
      model?: string;
      streaming: boolean;
    }}
  | {{
      type: "wyoming_tts";
      url: string;
      voice?: string;
      streaming: boolean;
    }}
  | {{
      type: "mcp_tool";
      transport: McpTransport;
    }};

export type McpTransport =
  | {{ type: "sse"; url: string }}
  | {{ type: "streamable_http"; url: string }}
  | {{ type: "stdio"; command: string; args: string[] }};

export interface ProviderDefinition {{
  id: string;
  label: string;
  variant: ProviderDefinitionVariant;
}}

export interface ProviderDefinitionView {{
  id: string;
  label: string;
  kind: ProviderCapability;
  variant: ProviderDefinitionVariant;
}}

export type TurnStatus = "running" | "completed" | "cancelled" | "failed" | "degraded";
export type SpokenSegmentRole = "assistant_preamble" | "tool_output" | "assistant_response";
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

export type ReconstructionItem = SpokenSegmentItem | ToolBatchItem;

export interface SpokenSegmentItem {{
  kind: "spoken_segment";
  id: string;
  sequence: number;
  role: SpokenSegmentRole;
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
  deleteProviderDefinition: (id: string) => Promise<void>;
  testProviderDefinition: (id: string) => Promise<ProviderStatus>;
  getPipeline: (name: string) => Promise<PipelineView>;
  putPipeline: (name: string, graph: PipelineGraph) => Promise<PipelineView>;
  deletePipeline: (name: string) => Promise<void>;
  validatePipeline: (graph: PipelineGraph) => Promise<PipelineView>;
  testPipeline: (
    name: string,
    request?: PipelineTestRequest,
  ) => Promise<PipelineTestResult>;
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
  providerTest: "/v1/providers/{{id}}/test",
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
    deleteProviderDefinition: async (id) => {{
      await requestJson<void>(request, config, providerRoute(id), {{
        method: "DELETE",
      }});
    }},
    testProviderDefinition: (id) =>
      requestJson<ProviderStatus>(request, config, providerTestRoute(id), {{
        method: "POST",
      }}),
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
    listTurns: () => requestJson<TurnList>(request, config, conduitApiRoutes.turns),
    getTurn: (turnId) =>
      requestJson<TurnSnapshot>(request, config, turnRoute(turnId)),
    getTurnEvents: (turnId) =>
      requestJson<RawTurnEvents>(request, config, turnEventsRoute(turnId)),
  }};
}}

function pipelineRoute(name: string): string {{
  return conduitApiRoutes.pipeline.replace("{{name}}", encodeURIComponent(name));
}}

function providerRoute(id: string): string {{
  return conduitApiRoutes.provider.replace("{{id}}", encodeURIComponent(id));
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
      ...(init.body ? {{ "content-type": "application/json" }} : {{}}),
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
export type ProviderKind = "stt" | "llm" | "tool" | "tts";
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

export interface ProviderStatus {{
  id: string;
  kind: ProviderKind;
  state: ProviderStatusState;
  configured: boolean;
  reachable: boolean;
  proven_by_turn: IdString | null;
  message: string | null;
  affects_pipelines: string[];
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
export type SpokenSegmentRole =
  | "assistant_preamble"
  | "tool_output"
  | "assistant_response";

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
  | {{ type: "SpokenSegmentStarted"; segment: string; role: SpokenSegmentRole; text: string }}
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
