/// A pipeline as a form rather than a graph.
///
/// The stored model is a chain: a source, optional speech stages, exactly one
/// reasoning core, and a sink — with a core's tools and memory held on the
/// core itself rather than beside it. `normalizePipelineGraph` already rewires
/// any draft into that chain, so authoring edges is authoring something the
/// model discards.
///
/// This module is the honest shape: every field is a stage the operator either
/// has or does not, and the graph is derived. Every form therefore describes a
/// connected chain, which is what the backend accepts.

import type {
  MemoryBinding,
  Modality,
  ModelBinding,
  PipelineGraph,
  PipelineNode,
  ToolBinding,
} from "../contracts/client";
import { DEFAULT_MAX_ROUNDS, normalizePipelineGraph } from "./graph";

/// A stage the operator can add or remove, identified by the node id it keeps
/// across edits so a round-trip does not churn ids.
export interface StageField {
  id: string;
  provider: string;
}

export interface EndpointField extends StageField {
  modality: Modality;
}

export interface SynthesisField extends StageField {
  voice?: string;
}

export interface CoreField {
  id: string;
  model: ModelBinding;
  system?: string;
  tools: ToolBinding[];
  memory: MemoryBinding[];
  maxRounds: number;
}

export interface PipelineForm {
  name: string;
  source: EndpointField;
  wakeWord: StageField | null;
  stt: StageField | null;
  speakerId: StageField | null;
  core: CoreField;
  tts: SynthesisField | null;
  sink: EndpointField;
}

/// Ids used for a stage the operator adds, matching what the console has
/// always called them so an existing pipeline reads the same after an edit.
const DEFAULT_STAGE_IDS = {
  source: "mic",
  wake_word: "wake_word",
  stt: "stt",
  speaker_id: "speaker_id",
  core: "core",
  tts: "tts",
  sink: "speaker",
} as const;

export function formFromGraph(graph: PipelineGraph): PipelineForm {
  const find = <K extends PipelineNode["kind"]>(kind: K) =>
    graph.nodes.find((node) => node.kind === kind) as
      Extract<PipelineNode, { kind: K }> | undefined;

  const source = find("source");
  const sink = find("sink");
  const core = find("core");
  const wakeWord = find("wake_word");
  const stt = find("stt");
  const speakerId = find("speaker_id");
  const tts = find("tts");

  return {
    name: graph.name,
    source: {
      id: source?.id ?? DEFAULT_STAGE_IDS.source,
      provider: source?.provider ?? "websocket",
      modality: source?.modality ?? "audio",
    },
    wakeWord: wakeWord
      ? { id: wakeWord.id, provider: wakeWord.provider }
      : null,
    stt: stt ? { id: stt.id, provider: stt.provider } : null,
    speakerId: speakerId
      ? { id: speakerId.id, provider: speakerId.provider }
      : null,
    core: {
      id: core?.id ?? DEFAULT_STAGE_IDS.core,
      model: core?.core.model ?? { provider: "" },
      system: core?.core.system,
      tools: core?.core.tools ? [...core.core.tools] : [],
      memory: core?.core.memory ? [...core.core.memory] : [],
      maxRounds: core?.core.max_rounds ?? DEFAULT_MAX_ROUNDS,
    },
    tts: tts ? { id: tts.id, provider: tts.provider, voice: tts.voice } : null,
    sink: {
      id: sink?.id ?? DEFAULT_STAGE_IDS.sink,
      provider: sink?.provider ?? "websocket",
      modality: sink?.modality ?? "audio",
    },
  };
}

/// Builds the graph a form describes.
///
/// Stages are emitted in run order and chained by `normalizePipelineGraph`, so
/// the result is connected by construction: there is no way to express a
/// dangling edge or an orphaned node through a form.
export function graphFromForm(form: PipelineForm): PipelineGraph {
  const nodes: PipelineNode[] = [
    {
      id: form.source.id,
      kind: "source",
      provider: form.source.provider,
      ...(form.source.modality === "audio"
        ? {}
        : { modality: form.source.modality }),
    },
    ...(form.wakeWord
      ? [
          {
            id: form.wakeWord.id,
            kind: "wake_word" as const,
            provider: form.wakeWord.provider,
          },
        ]
      : []),
    ...(form.stt
      ? [
          {
            id: form.stt.id,
            kind: "stt" as const,
            provider: form.stt.provider,
          },
        ]
      : []),
    ...(form.speakerId
      ? [
          {
            id: form.speakerId.id,
            kind: "speaker_id" as const,
            provider: form.speakerId.provider,
          },
        ]
      : []),
    {
      id: form.core.id,
      kind: "core",
      core: {
        model: form.core.model,
        ...(form.core.system === undefined ? {} : { system: form.core.system }),
        ...(form.core.tools.length > 0 ? { tools: form.core.tools } : {}),
        ...(form.core.memory.length > 0 ? { memory: form.core.memory } : {}),
        max_rounds: form.core.maxRounds,
      },
    },
    ...(form.tts
      ? [
          {
            id: form.tts.id,
            kind: "tts" as const,
            provider: form.tts.provider,
            ...(form.tts.voice === undefined ? {} : { voice: form.tts.voice }),
          },
        ]
      : []),
    {
      id: form.sink.id,
      kind: "sink",
      provider: form.sink.provider,
      ...(form.sink.modality === "audio"
        ? {}
        : { modality: form.sink.modality }),
    },
  ];

  return normalizePipelineGraph({ name: form.name, nodes, edges: [] });
}
