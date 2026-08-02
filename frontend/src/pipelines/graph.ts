/// The pipeline graph model, independent of how it is drawn.
///
/// These are the pure functions the editor sits on: they take a graph and
/// return a graph, so they are testable without React and reusable by any
/// surface that edits a pipeline.

import type {
  Modality,
  NodeKind,
  PipelineEdge,
  PipelineGraph,
  PipelineNode,
  PipelineView,
} from "../contracts/client";
import type { ComponentKind } from "../contracts/status";

export const DEFAULT_MAX_ROUNDS = 4;
export const DEFAULT_MEMORY_LIMIT = 8;

/// The order stages run in, which is also the order they are drawn in.
export const LINEAR_STAGE_ORDER: NodeKind[] = [
  "source",
  "wake_word",
  "stt",
  "speaker_id",
  "core",
  "tts",
  "sink",
];

export type PipelineValidationResult =
  { ok: true; order: string[] } | { ok: false; message: string };

export type PipelineValidator = (
  graph: PipelineGraph,
) => PipelineValidationResult | Promise<PipelineValidationResult>;

/// What a test turn produced: a line for the operator, and the reply itself
/// when the pipeline synthesized one.
export interface PipelineTestOutcome {
  message: string;
  replyAudio: string | null;
}

export type PipelineTester = (name: string) => Promise<PipelineTestOutcome>;

export interface PipelineEditorDraftState {
  draft: PipelineGraph;
  history: PipelineGraph[];
  validation: PipelineValidationResult | null;
  notice: string | null;
  /// Playable reply from the last test turn, as a data URI.
  replyAudio?: string | null;
}

/// A reasoning core bound to one model and nothing else.
///
/// Guided setup binds no tools and no memory: the point is the smallest
/// pipeline that works, and bindings are what the editor is for.
export function coreNode(llmProvider: string): PipelineNode {
  return {
    id: "core",
    kind: "core",
    core: { model: { provider: llmProvider }, max_rounds: DEFAULT_MAX_ROUNDS },
  };
}

export function buildMinimalVoiceLoopGraph({
  name,
  sttProvider,
  llmProvider,
  ttsProvider,
}: {
  name: string;
  sttProvider: string;
  llmProvider: string;
  ttsProvider: string;
}): PipelineGraph {
  return {
    name,
    nodes: [
      { id: "mic", kind: "source", provider: "websocket" },
      { id: "stt", kind: "stt", provider: sttProvider },
      coreNode(llmProvider),
      { id: "tts", kind: "tts", provider: ttsProvider },
      { id: "speaker", kind: "sink", provider: "websocket" },
    ],
    edges: [
      { from: "mic", to: "stt" },
      { from: "stt", to: "core" },
      { from: "core", to: "tts" },
      { from: "tts", to: "speaker" },
    ],
  };
}

/// The minimal loop for a text assistant: typed in, written out.
///
/// No recognizer and no synthesizer, so this runs on a deployment where a
/// language model is the only configured provider.
export function buildMinimalTextLoopGraph({
  name,
  llmProvider,
}: {
  name: string;
  llmProvider: string;
}): PipelineGraph {
  return {
    name,
    nodes: [
      { id: "in", kind: "source", provider: "websocket", modality: "text" },
      coreNode(llmProvider),
      { id: "out", kind: "sink", provider: "websocket", modality: "text" },
    ],
    edges: [
      { from: "in", to: "core" },
      { from: "core", to: "out" },
    ],
  };
}

/// The provider a node shows and is edited through.
///
/// A core has no single provider — it binds a model plus any number of tools
/// and stores — so it answers with its model's. Everything a node card offers
/// today is about that one binding; its tools and memory are edited as
/// bindings rather than as a provider field.
export function nodeProvider(node: PipelineNode): string {
  return node.kind === "core" ? node.core.model.provider : node.provider;
}

export function componentKindForNode(node: PipelineNode): ComponentKind {
  if (node.kind === "stt") {
    return "transcription";
  }
  if (node.kind === "core") {
    return "reasoning";
  }
  if (node.kind === "tts") {
    return "synthesis";
  }
  return "capture";
}

/// What a node writes to its outgoing edges, or `undefined` when the kind is
/// not a modality transform and its edges therefore carry nothing named.
///
/// Mirrors `Node::output_modality` in `conduit-core`. The backend remains the
/// authority — this exists so the editor can show an operator what a link
/// carries without a round trip.
export function outputModality(
  node: PipelineNode | undefined,
): Modality | undefined {
  switch (node?.kind) {
    case "source":
      return node.modality ?? "audio";
    case "wake_word":
    case "speaker_id":
    case "tts":
      return "audio";
    case "stt":
      return "text";
    case "core":
      return "utterance";
    default:
      return undefined;
  }
}

/// One thing bound to a core, drawn beside it.
export interface CoreSpoke {
  /// Identifies the binding across renders. A binding has no id of its own, so
  /// it is identified by the core it hangs off and its position in that core's
  /// list.
  key: string;
  label: string;
  kind: "tool" | "memory";
}

export interface PipelineGraphFlow {
  mainNodes: PipelineNode[];
  mainEdges: PipelineGraph["edges"];
  spokesByTarget: Map<string, CoreSpoke[]>;
}

/// Splits a graph into the transport pipeline and each core's bindings.
///
/// Every node is on the spine now, because every node *is* a stage: a core's
/// tools and memory are configuration on it rather than nodes beside it.
export function pipelineGraphFlow(graph: PipelineGraph): PipelineGraphFlow {
  const mainNodes = sortLinearNodes(graph.nodes);
  const spokesByTarget = new Map<string, CoreSpoke[]>();

  for (const node of graph.nodes) {
    if (node.kind !== "core") {
      continue;
    }
    const spokes: CoreSpoke[] = [
      ...(node.core.tools ?? []).map((tool, index) => ({
        key: `${node.id}-tool-${index}`,
        label: tool.provider,
        kind: "tool" as const,
      })),
      ...(node.core.memory ?? []).map((store, index) => ({
        key: `${node.id}-memory-${index}`,
        label: store.provider,
        kind: "memory" as const,
      })),
    ];
    if (spokes.length > 0) {
      spokesByTarget.set(node.id, spokes);
    }
  }

  return { mainNodes, mainEdges: graph.edges, spokesByTarget };
}

export function initializePipelineDrafts(
  pipelineViews: readonly PipelineView[],
): Record<string, PipelineEditorDraftState> {
  return Object.fromEntries(
    pipelineViews.map((view) => [
      view.graph.name,
      {
        draft: cloneGraph(view.graph),
        history: [],
        validation: null,
        notice: null,
      },
    ]),
  ) as Record<string, PipelineEditorDraftState>;
}

export function cloneGraph(graph: PipelineGraph): PipelineGraph {
  return JSON.parse(JSON.stringify(graph)) as PipelineGraph;
}

export function pipelineGraphsEqual(
  left: PipelineGraph,
  right: PipelineGraph,
): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

export function upsertPipelineView(
  views: readonly PipelineView[],
  graph: PipelineGraph,
  order = graph.nodes.map((node) => node.id),
): readonly PipelineView[] {
  const next = { graph: cloneGraph(graph), order };
  const existing = views.findIndex((view) => view.graph.name === graph.name);
  if (existing === -1) {
    return [...views, next];
  }

  return views.map((view, index) => (index === existing ? next : view));
}

export function uniqueNodeId(graph: PipelineGraph, base: string): string {
  const existing = new Set(graph.nodes.map((node) => node.id));
  if (!existing.has(base)) {
    return base;
  }

  let suffix = 2;
  while (existing.has(`${base}_${suffix}`)) {
    suffix += 1;
  }
  return `${base}_${suffix}`;
}

export function isEndpointNode(node: PipelineNode): boolean {
  return node.kind === "source" || node.kind === "sink";
}

export function linearStageRank(node: PipelineNode): number {
  const rank = LINEAR_STAGE_ORDER.indexOf(node.kind);
  return rank === -1 ? LINEAR_STAGE_ORDER.length : rank;
}

export function sortLinearNodes(
  nodes: readonly PipelineNode[],
): PipelineNode[] {
  return [...nodes].sort((left, right) => {
    const rankDelta = linearStageRank(left) - linearStageRank(right);
    return rankDelta === 0 ? left.id.localeCompare(right.id) : rankDelta;
  });
}

/// A name for a copy of `base` that no stored pipeline already uses.
///
/// Suffixed rather than prefixed so copies of one pipeline sort beside it, and
/// numbered from two because the original is the first.
export function nextPipelineName(
  base: string,
  taken: readonly string[],
): string {
  const root = base.replace(/-\d+$/, "");
  for (let suffix = 2; ; suffix += 1) {
    const candidate = `${root}-${suffix}`;
    if (!taken.includes(candidate)) {
      return candidate;
    }
  }
}

/// Rewires a draft into one chain in stage order.
///
/// Every node is on that chain: a core's tools and memory are bindings, so
/// there is nothing left that hangs off the pipeline rather than sitting in
/// it, and every edge is a link between two stages.
export function normalizePipelineGraph(graph: PipelineGraph): PipelineGraph {
  const linearNodes = sortLinearNodes(graph.nodes);
  const linearEdges = linearNodes.slice(0, -1).map((node, index) => ({
    from: node.id,
    to: linearNodes[index + 1].id,
  }));

  return {
    ...graph,
    nodes: linearNodes,
    edges: dedupeEdges(linearEdges),
  };
}

export function dedupeEdges(edges: readonly PipelineEdge[]): PipelineEdge[] {
  const seen = new Set<string>();
  return edges.filter((edge) => {
    const key = `${edge.from}->${edge.to}:${edge.port ?? ""}`;
    if (seen.has(key)) {
      return false;
    }
    seen.add(key);
    return true;
  });
}

export function insertLinearStageNode(
  graph: PipelineGraph,
  node: PipelineNode,
): PipelineGraph {
  const nodeRank = linearStageRank(node);
  const existingMainNodes = graph.nodes
    .slice()
    .sort((left, right) => linearStageRank(left) - linearStageRank(right));
  const previous = [...existingMainNodes]
    .reverse()
    .find((candidate) => linearStageRank(candidate) < nodeRank);
  const next = existingMainNodes.find(
    (candidate) => linearStageRank(candidate) > nodeRank,
  );
  const edges = graph.edges.filter(
    (edge) =>
      !(previous && next && edge.from === previous.id && edge.to === next.id),
  );

  return normalizePipelineGraph({
    ...graph,
    nodes: [...graph.nodes, node],
    edges: [
      ...edges,
      ...(previous ? [{ from: previous.id, to: node.id }] : []),
      ...(next ? [{ from: node.id, to: next.id }] : []),
    ],
  });
}
