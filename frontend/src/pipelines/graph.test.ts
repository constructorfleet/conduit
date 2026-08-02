import { describe, expect, it } from "vitest";

import type { PipelineGraph } from "../contracts/client";
import type { OperatorStatusSnapshot } from "../contracts/status";
import {
  buildMinimalTextLoopGraph,
  buildMinimalVoiceLoopGraph,
  defaultPipelineViews,
  insertLinearStageNode,
  nextPipelineName,
  normalizePipelineGraph,
  pipelineGraphFlow,
  sortLinearNodes,
  uniqueNodeId,
} from "./graph";

/// Edge endpoints that name no node in the graph.
///
/// The backend refuses a graph like this, so any we build ourselves is a bug
/// the operator sees as a broken diagram before the save is ever rejected.
function danglingEdges(graph: PipelineGraph): string[] {
  const ids = new Set(graph.nodes.map((node) => node.id));
  return graph.edges
    .flatMap((edge) => [edge.from, edge.to])
    .filter((id) => !ids.has(id));
}

function snapshotWithPipeline(name: string): OperatorStatusSnapshot {
  return {
    pipelines: [
      {
        name,
        usable: true,
        health: {
          state: "healthy",
          summary: "",
          last_successful_turn: null,
          last_failed_turn: null,
        },
        components: [],
        affected_providers: [],
      },
    ],
  } as unknown as OperatorStatusSnapshot;
}

describe("graphs the console builds", () => {
  it("wires the voice loop to nodes that exist", () => {
    const graph = buildMinimalVoiceLoopGraph({
      name: "kitchen",
      sttProvider: "whisper",
      llmProvider: "openai",
      ttsProvider: "piper",
    });

    expect(danglingEdges(graph)).toEqual([]);
  });

  it("wires the text loop to nodes that exist", () => {
    const graph = buildMinimalTextLoopGraph({
      name: "desk",
      llmProvider: "openai",
    });

    expect(danglingEdges(graph)).toEqual([]);
  });

  it("wires the snapshot seed view to nodes that exist", () => {
    const [view] = defaultPipelineViews(snapshotWithPipeline("kitchen"));

    expect(danglingEdges(view.graph)).toEqual([]);
  });

  it("seeds nothing when the snapshot reports no pipeline", () => {
    expect(defaultPipelineViews(null)).toEqual([]);
  });
});

describe("normalizePipelineGraph", () => {
  it("rewires a graph into one chain in stage order", () => {
    const graph: PipelineGraph = {
      name: "kitchen",
      nodes: [
        { id: "speaker", kind: "sink", provider: "websocket" },
        { id: "mic", kind: "source", provider: "websocket" },
        { id: "stt", kind: "stt", provider: "whisper" },
      ],
      edges: [],
    };

    expect(normalizePipelineGraph(graph).edges).toEqual([
      { from: "mic", to: "stt" },
      { from: "stt", to: "speaker" },
    ]);
  });

  it("discards an edge that does not follow stage order", () => {
    const graph: PipelineGraph = {
      name: "kitchen",
      nodes: [
        { id: "mic", kind: "source", provider: "websocket" },
        { id: "stt", kind: "stt", provider: "whisper" },
        { id: "speaker", kind: "sink", provider: "websocket" },
      ],
      edges: [{ from: "mic", to: "speaker" }],
    };

    expect(normalizePipelineGraph(graph).edges).toEqual([
      { from: "mic", to: "stt" },
      { from: "stt", to: "speaker" },
    ]);
  });
});

describe("insertLinearStageNode", () => {
  it("splices a stage between the ones it runs after and before", () => {
    const graph = buildMinimalTextLoopGraph({
      name: "desk",
      llmProvider: "openai",
    });

    const spliced = insertLinearStageNode(graph, {
      id: "stt",
      kind: "stt",
      provider: "whisper",
    });

    expect(spliced.nodes.map((node) => node.id)).toEqual([
      "in",
      "stt",
      "core",
      "out",
    ]);
    expect(danglingEdges(spliced)).toEqual([]);
  });
});

describe("pipelineGraphFlow", () => {
  it("reports a core's tools and memory as bindings rather than nodes", () => {
    const graph: PipelineGraph = {
      name: "kitchen",
      nodes: [
        {
          id: "core",
          kind: "core",
          core: {
            model: { provider: "openai" },
            tools: [{ provider: "search", confirm: "never" }],
            memory: [{ provider: "sqlite", mode: "read_write", limit: 8 }],
            max_rounds: 4,
          },
        },
      ],
      edges: [],
    };

    const flow = pipelineGraphFlow(graph);

    expect(flow.mainNodes.map((node) => node.id)).toEqual(["core"]);
    expect(flow.spokesByTarget.get("core")).toEqual([
      { key: "core-tool-0", label: "search", kind: "tool" },
      { key: "core-memory-0", label: "sqlite", kind: "memory" },
    ]);
  });
});

describe("naming", () => {
  it("numbers a copy from two and skips names already taken", () => {
    expect(nextPipelineName("kitchen", [])).toBe("kitchen-2");
    expect(nextPipelineName("kitchen", ["kitchen-2"])).toBe("kitchen-3");
    expect(nextPipelineName("kitchen-2", ["kitchen-2"])).toBe("kitchen-3");
  });

  it("suffixes a node id only when the base is taken", () => {
    const graph = buildMinimalTextLoopGraph({
      name: "desk",
      llmProvider: "openai",
    });

    expect(uniqueNodeId(graph, "stt")).toBe("stt");
    expect(uniqueNodeId(graph, "core")).toBe("core_2");
  });
});

describe("sortLinearNodes", () => {
  it("orders nodes by the stage they run in", () => {
    const graph = buildMinimalVoiceLoopGraph({
      name: "kitchen",
      sttProvider: "whisper",
      llmProvider: "openai",
      ttsProvider: "piper",
    });

    expect(
      sortLinearNodes([...graph.nodes].reverse()).map((n) => n.id),
    ).toEqual(["mic", "stt", "core", "tts", "speaker"]);
  });
});
