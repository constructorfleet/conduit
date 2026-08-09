import { describe, expect, it } from "vitest";

import type { PipelineGraph } from "../contracts/client";
import {
  buildMinimalTextLoopGraph,
  buildMinimalVoiceLoopGraph,
  normalizePipelineGraph,
} from "./graph";
import { formFromGraph, graphFromForm } from "./form";

function voiceLoop(): PipelineGraph {
  return buildMinimalVoiceLoopGraph({
    name: "kitchen",
    sttProvider: "whisper",
    llmProvider: "openai",
    ttsProvider: "piper",
  });
}

describe("formFromGraph", () => {
  it("reads each stage of a voice loop into its own field", () => {
    const form = formFromGraph(voiceLoop());

    expect(form.name).toBe("kitchen");
    expect(form.source).toMatchObject({ id: "mic", provider: "websocket" });
    expect(form.stt).toMatchObject({ id: "stt", provider: "whisper" });
    expect(form.tts).toMatchObject({ id: "tts", provider: "piper" });
    expect(form.sink).toMatchObject({ id: "speaker", provider: "websocket" });
    expect(form.core.model.provider).toBe("openai");
  });

  it("leaves speech stages absent for a text loop", () => {
    const form = formFromGraph(
      buildMinimalTextLoopGraph({ name: "desk", llmProvider: "openai" }),
    );

    expect(form.stt).toBeNull();
    expect(form.tts).toBeNull();
    expect(form.transform).toBeNull();
    expect(form.wakeWord).toBeNull();
    expect(form.speakerId).toBeNull();
    expect(form.source.modality).toBe("text");
    expect(form.sink.modality).toBe("text");
  });

  it("reads a core's tools and memory as lists rather than nodes", () => {
    const graph = voiceLoop();
    graph.nodes = graph.nodes.map((node) =>
      node.kind === "core"
        ? {
            ...node,
            core: {
              ...node.core,
              system: "be brief",
              tools: [{ provider: "search", confirm: "always" as const }],
              memory: [
                {
                  provider: "sqlite",
                  mode: "read_write" as const,
                  limit: 8,
                },
              ],
            },
          }
        : node,
    );

    const form = formFromGraph(graph);

    expect(form.core.system).toBe("be brief");
    expect(form.core.tools).toEqual([
      { provider: "search", confirm: "always" },
    ]);
    expect(form.core.memory).toEqual([
      { provider: "sqlite", mode: "read_write", limit: 8 },
    ]);
  });
});

describe("graphFromForm", () => {
  it("round-trips a voice loop unchanged", () => {
    const graph = voiceLoop();

    expect(graphFromForm(formFromGraph(graph))).toEqual(
      normalizePipelineGraph(graph),
    );
  });

  it("round-trips a text loop unchanged", () => {
    const graph = buildMinimalTextLoopGraph({
      name: "desk",
      llmProvider: "openai",
    });

    expect(graphFromForm(formFromGraph(graph))).toEqual(
      normalizePipelineGraph(graph),
    );
  });

  it("chains the stages an operator left present", () => {
    const form = formFromGraph(voiceLoop());
    form.stt = null;
    form.tts = null;

    const graph = graphFromForm(form);

    expect(graph.nodes.map((node) => node.id)).toEqual([
      "mic",
      "core",
      "speaker",
    ]);
    expect(graph.edges).toEqual([
      { from: "mic", to: "core" },
      { from: "core", to: "speaker" },
    ]);
  });

  it("wires an added transform between the model and what speaks it", () => {
    // The stage exists because a model writes for a reader: the rewrite has
    // to sit after reasoning and before anything renders it, or it changes
    // the wrong words.
    const form = formFromGraph(voiceLoop());
    form.transform = { id: "clean", provider: "speech-cleanup" };

    const graph = graphFromForm(form);

    expect(graph.nodes.map((node) => node.id)).toEqual([
      "mic",
      "stt",
      "core",
      "clean",
      "tts",
      "speaker",
    ]);
    expect(graph.edges).toContainEqual({ from: "core", to: "clean" });
    expect(graph.edges).toContainEqual({ from: "clean", to: "tts" });
  });

  it("splices an added stage into the chain in run order", () => {
    const form = formFromGraph(
      buildMinimalTextLoopGraph({ name: "desk", llmProvider: "openai" }),
    );
    form.stt = { id: "stt", provider: "whisper" };

    const graph = graphFromForm(form);

    expect(graph.nodes.map((node) => node.id)).toEqual([
      "in",
      "stt",
      "core",
      "out",
    ]);
  });

  it("carries the core's bindings onto the core node", () => {
    const form = formFromGraph(voiceLoop());
    form.core.system = "be brief";
    form.core.tools = [{ provider: "search", confirm: "never" }];
    form.core.memory = [{ provider: "sqlite", mode: "read", limit: 4 }];

    const core = graphFromForm(form).nodes.find((node) => node.kind === "core");

    expect(core).toMatchObject({
      kind: "core",
      core: {
        system: "be brief",
        tools: [{ provider: "search", confirm: "never" }],
        memory: [{ provider: "sqlite", mode: "read", limit: 4 }],
      },
    });
  });

  it("puts trimming between the wake word and the recognizer", () => {
    // Where a trimmer goes is not a preference: after the gate, because the
    // gate's pre-roll is deliberately silence a trimmer placed first would
    // remove, and before the recognizer, which is the whole point.
    const form = formFromGraph(voiceLoop());
    form.wakeWord = { id: "wake", provider: "openwakeword" };
    form.vad = { id: "trim", provider: "silero" };

    expect(graphFromForm(form).nodes.map((node) => node.id)).toEqual([
      "mic",
      "wake",
      "trim",
      "stt",
      "core",
      "tts",
      "speaker",
    ]);
  });

  it("never produces an edge naming a node it did not build", () => {
    const form = formFromGraph(voiceLoop());
    form.wakeWord = { id: "wake", provider: "porcupine" };
    form.speakerId = { id: "who", provider: "resemblyzer" };

    const graph = graphFromForm(form);
    const ids = new Set(graph.nodes.map((node) => node.id));

    expect(
      graph.edges
        .flatMap((edge) => [edge.from, edge.to])
        .filter((id) => !ids.has(id)),
    ).toEqual([]);
    expect(graph.nodes.map((node) => node.id)).toEqual([
      "mic",
      "wake",
      "stt",
      "who",
      "core",
      "tts",
      "speaker",
    ]);
  });

  it("gives the sink an audio modality when synthesis feeds it", () => {
    // The sink's modality is what the backend checks the incoming edge
    // against, so a text sink behind a synthesizer refuses a saved pipeline
    // with "carries audio, but ... accepts text or utterance". An operator
    // picking the wrong value here has no way to know that ahead of time —
    // the value is derived from what feeds the sink, so the form emits it.
    const form = formFromGraph(voiceLoop());
    form.wakeWord = { id: "wake", provider: "microwakeword" };
    form.sink = { ...form.sink, modality: "text" };

    const sink = graphFromForm(form).nodes.find((node) => node.kind === "sink");

    expect(sink?.kind === "sink" ? sink.modality ?? "audio" : null).toBe(
      "audio",
    );
  });

  it("gives the sink a text modality when a text source feeds a core", () => {
    // The other end of the same rule: with only a core between the endpoints,
    // what leaves it is an utterance, which a text sink can render.
    const form = formFromGraph(
      buildMinimalTextLoopGraph({ name: "desk", llmProvider: "openai" }),
    );
    form.sink = { ...form.sink, modality: "audio" };

    const sink = graphFromForm(form).nodes.find((node) => node.kind === "sink");

    expect(sink?.kind === "sink" ? sink.modality ?? "audio" : null).toBe(
      "text",
    );
  });
});
