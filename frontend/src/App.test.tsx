import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import App, { OverviewPanel } from "./App";
import type { PipelineGraph, PipelineView } from "./contracts/client";
import { eventEnvelopeFixtures, type EventEnvelope } from "./contracts/events";
import {
  operatorStatusSnapshotFixture,
  type OperatorStatusSnapshot,
} from "./contracts/status";
import { applySnapshotEvent, transitionEventStream } from "./eventStream";

beforeEach(() => {
  sessionStorage.clear();
  localStorage.clear();
  mockSmallScreen(false);
});

describe("Operator Console shell", () => {
  it("starts at Operator Access and stores bearer tokens in session memory", async () => {
    const user = userEvent.setup();
    render(<App />);

    expect(
      screen.getByRole("heading", { name: "Operator Access" }),
    ).toBeInTheDocument();

    await user.type(
      screen.getByLabelText("Management bearer token"),
      "management-token",
    );
    await user.click(screen.getByRole("button", { name: "Connect" }));

    expect(screen.getByRole("tab", { name: "Overview" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(sessionStorage.getItem("conduit.operator.access")).toContain(
      "management-token",
    );
    expect(localStorage.getItem("conduit.operator.access")).toBeNull();
  });

  it("requires an explicit choice before local token persistence", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.type(
      screen.getByLabelText("Management bearer token"),
      "remembered-token",
    );
    await user.click(screen.getByLabelText("Remember on this browser"));
    await user.click(screen.getByRole("button", { name: "Connect" }));

    expect(localStorage.getItem("conduit.operator.access")).toContain(
      "remembered-token",
    );
    expect(sessionStorage.getItem("conduit.operator.access")).toBeNull();
  });

  it("enters explicit anonymous mode and exposes the five top-level sections", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );

    for (const section of [
      "Overview",
      "Pipelines",
      "Providers",
      "Events",
      "Settings",
    ]) {
      expect(screen.getByRole("tab", { name: section })).toBeInTheDocument();
    }
    expect(screen.getByText("Anonymous operator access")).toBeInTheDocument();
  });
});

describe("Overview operations workspace", () => {
  it("surfaces failures before baseline pipeline status", () => {
    render(<OverviewPanel snapshot={snapshotFixture()} eventPosture="live" />);

    const failure = screen.getByText("connection refused");
    const pipelineHealth = screen.getByRole("heading", {
      name: "Pipeline Health",
    });

    expect(
      screen.getByRole("heading", { name: "Current Exceptions" }),
    ).toBeInTheDocument();
    expect(
      failure.compareDocumentPosition(pipelineHealth) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(screen.getByText("synthesis / piper-local")).toBeInTheDocument();
  });

  it("keeps healthy baseline quiet", () => {
    render(<OverviewPanel snapshot={healthySnapshot()} eventPosture="live" />);

    expect(screen.getByText("No current exceptions")).toBeInTheDocument();
    expect(screen.getByText("healthy")).toBeInTheDocument();
    expect(screen.queryByText("connection refused")).not.toBeInTheDocument();
  });

  it("marks stale stream state while preserving the last known snapshot", () => {
    render(<OverviewPanel snapshot={snapshotFixture()} eventPosture="stale" />);

    expect(screen.getByLabelText("Stale state")).toHaveTextContent(
      "Stale state",
    );
    expect(screen.getByText("Reconnect refresh required")).toBeInTheDocument();
    expect(screen.getByText("Kitchen Satellite")).toBeInTheDocument();
  });

  it("refreshes the snapshot before clearing stale state after reconnect", () => {
    const disconnected = transitionEventStream(
      {
        snapshot: snapshotFixture(),
        eventPosture: "live",
        snapshotState: "live",
      },
      { type: "disconnected" },
    );

    const refreshedSnapshot = healthySnapshot();
    refreshedSnapshot.generated_at = "2026-08-01T01:03:00Z";
    const reconnected = transitionEventStream(disconnected, {
      type: "reconnected",
      snapshot: refreshedSnapshot,
    });
    const { rerender } = render(
      <OverviewPanel
        snapshot={disconnected.snapshot}
        eventPosture={disconnected.eventPosture}
      />,
    );

    expect(screen.getByLabelText("Stale state")).toBeInTheDocument();

    rerender(
      <OverviewPanel
        snapshot={reconnected.snapshot}
        eventPosture={reconnected.eventPosture}
      />,
    );

    expect(screen.queryByLabelText("Stale state")).not.toBeInTheDocument();
    expect(screen.getByText("No current exceptions")).toBeInTheDocument();
  });

  it("keeps connected and recently active satellites separate", () => {
    render(<OverviewPanel snapshot={snapshotFixture()} eventPosture="live" />);

    expect(
      screen.getByRole("heading", { name: "Connected Satellites" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Recently Active Satellites" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Kitchen Satellite / TtsStarted"),
    ).toBeInTheDocument();
  });

  it("renders durable visible state from snapshot event updates", () => {
    const started = applySnapshotEvent(healthySnapshot(), {
      id: "00000000-0000-0000-0000-000000000110",
      trace: "00000000-0000-0000-0000-000000000111",
      at: "2026-08-01T01:04:00Z",
      device: "00000000-0000-0000-0000-000000000001",
      conversation: "00000000-0000-0000-0000-000000000112",
      pipeline: "kitchen",
      event: {
        type: "TurnStarted",
        turn: "00000000-0000-0000-0000-000000000113",
      },
    });
    const updated = applySnapshotEvent(started, {
      id: "00000000-0000-0000-0000-000000000114",
      trace: "00000000-0000-0000-0000-000000000111",
      at: "2026-08-01T01:04:01Z",
      device: "00000000-0000-0000-0000-000000000001",
      conversation: "00000000-0000-0000-0000-000000000112",
      pipeline: "kitchen",
      event: {
        type: "StageFailed",
        node: "tts",
        error: "speaker endpoint refused audio",
        recovered: false,
      },
    });

    render(<OverviewPanel snapshot={updated} eventPosture="live" />);

    expect(updated.pipelines[0]?.health.last_failed_turn).toBe(
      "00000000-0000-0000-0000-000000000113",
    );
    expect(screen.getByText("1 running")).toBeInTheDocument();
    expect(
      screen.getAllByText("speaker endpoint refused audio").length,
    ).toBeGreaterThan(0);
    expect(screen.getAllByText("kitchen").length).toBeGreaterThan(0);
    expect(screen.getByText("started")).toBeInTheDocument();
    expect(
      screen.getByText("Kitchen Satellite / StageFailed"),
    ).toBeInTheDocument();
  });

  it("loads the overview from the generated status contract after access", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );

    expect(
      screen.getByRole("heading", { name: "Current Exceptions" }),
    ).toBeInTheDocument();
    expect(screen.getAllByText("piper-local").length).toBeGreaterThan(0);
    expect(screen.getByText("Snapshot")).toBeInTheDocument();
    expect(screen.getByText("live")).toBeInTheDocument();
  });

  it("links current failures to reconstructed turn events when possible", async () => {
    const user = userEvent.setup();
    render(<App initialEvents={eventFixture()} />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );
    await user.click(
      screen.getAllByRole("button", { name: "Open turn events" })[0],
    );

    expect(
      screen.getByRole("heading", { name: "Turn Reconstruction" }),
    ).toBeInTheDocument();
    expect(screen.getByText("StageFailed")).toBeInTheDocument();
  });
});

describe("First-Run Guided Setup", () => {
  it("routes no-pipeline launch state into Guided Setup", async () => {
    const user = userEvent.setup();
    render(<App initialSnapshot={firstRunSnapshot()} />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );

    expect(
      screen.getByRole("heading", { name: "First-Run Setup" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Guided Setup" }),
    ).toBeInTheDocument();
  });

  it("invokes inline Provider Settings and allows optional tool setup to be skipped", async () => {
    const user = userEvent.setup();
    render(<App initialSnapshot={firstRunSnapshot()} />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );
    await user.click(
      screen.getByRole("button", { name: "Configure Providers" }),
    );
    await user.click(screen.getByRole("button", { name: "Skip tool setup" }));

    expect(screen.getByLabelText("Speech-to-text provider")).toHaveValue(
      "whisper",
    );
    expect(screen.getByLabelText("Language model provider")).toHaveValue(
      "openai",
    );
    expect(screen.getByLabelText("Text-to-speech provider")).toHaveValue(
      "piper",
    );
    expect(screen.getByText("Tool setup skipped")).toBeInTheDocument();
  });

  it("reports validation feedback before saving an incomplete voice loop", async () => {
    const user = userEvent.setup();
    render(<App initialSnapshot={firstRunSnapshot()} />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );
    await user.clear(screen.getByLabelText("Pipeline name"));
    await user.click(screen.getByRole("button", { name: "Validate and Save" }));

    expect(screen.getByText("Pipeline name is required")).toBeInTheDocument();
  });

  it("saves a real minimal voice-loop graph and transitions to Operations Workspace", async () => {
    const user = userEvent.setup();
    const savedGraphs: unknown[] = [];
    render(
      <App
        initialSnapshot={firstRunSnapshot()}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
      />,
    );

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );
    await user.clear(screen.getByLabelText("Pipeline name"));
    await user.type(screen.getByLabelText("Pipeline name"), "kitchen");
    await user.click(screen.getByRole("button", { name: "Skip tool setup" }));
    await user.click(screen.getByRole("button", { name: "Validate and Save" }));

    expect(savedGraphs).toEqual([
      {
        name: "kitchen",
        nodes: [
          { id: "mic", kind: "source", provider: "websocket" },
          { id: "stt", kind: "stt", provider: "whisper" },
          { id: "llm", kind: "llm", provider: "openai" },
          { id: "tts", kind: "tts", provider: "piper" },
          { id: "speaker", kind: "sink", provider: "websocket" },
        ],
        edges: [
          { from: "mic", to: "stt" },
          { from: "stt", to: "llm" },
          { from: "llm", to: "tts" },
          { from: "tts", to: "speaker" },
        ],
      },
    ]);
    expect(
      screen.getByRole("heading", { name: "Overview" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Current Exceptions" }),
    ).toBeInTheDocument();
  });
});

describe("Events turn reconstruction", () => {
  it("groups reconstructed events into visual component stages", async () => {
    const user = userEvent.setup();
    render(<App initialEvents={successfulTurnEvents()} />);

    await enterEventsSection(user);

    expect(screen.getByText("Stage Timeline")).toBeInTheDocument();
    expect(
      screen.getByRole("group", { name: "transcription stage" }),
    ).toHaveTextContent("Speech Final");
    expect(
      screen.getByRole("group", { name: "reasoning stage" }),
    ).toHaveTextContent("Llm Token");
    expect(
      screen.getByRole("group", { name: "tools stage" }),
    ).toHaveTextContent("Tool Requested");
    expect(
      screen.getByRole("group", { name: "synthesis stage" }),
    ).toHaveTextContent("Tts Finished");
  });

  it("renders a successful turn as an ordered component story", async () => {
    const user = userEvent.setup();
    render(<App initialEvents={successfulTurnEvents()} />);

    await enterEventsSection(user);

    const speech = screen.getByText("turn on the kitchen lights");
    const reasoning = screen.getByText("The lights are on.");
    const synthesis = screen.getByText("TtsFinished");

    expect(
      screen.getByRole("heading", { name: "Turn Reconstruction" }),
    ).toBeInTheDocument();
    expect(screen.getByText("completed")).toBeInTheDocument();
    expect(
      speech.compareDocumentPosition(reasoning) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(
      reasoning.compareDocumentPosition(synthesis) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("preserves tool activity and confirmation boundaries", async () => {
    const user = userEvent.setup();
    render(<App initialEvents={successfulTurnEvents()} />);

    await enterEventsSection(user);

    expect(screen.getByText("lights.turn_on")).toBeInTheDocument();
    expect(screen.getByText("Turn on the kitchen lights?")).toBeInTheDocument();
    expect(screen.getByText("ToolCompleted")).toBeInTheDocument();
  });

  it("surfaces synthesis failures as component-attributed turn failures", async () => {
    const user = userEvent.setup();
    render(<App initialEvents={eventFixture()} />);

    await enterEventsSection(user);

    expect(screen.getByText("failed")).toBeInTheDocument();
    expect(screen.getByText("StageFailed")).toBeInTheDocument();
    expect(screen.getByText("connection refused")).toBeInTheDocument();
    expect(screen.getByText("tts")).toBeInTheDocument();
  });

  it("marks reconstructed turns stale when the event stream is stale", async () => {
    const user = userEvent.setup();
    render(
      <App
        initialEventPosture="stale"
        initialEvents={successfulTurnEvents()}
      />,
    );

    await enterEventsSection(user);

    expect(screen.getByLabelText("Stale state")).toHaveTextContent(
      "Stale state",
    );
    expect(screen.getByText("completed")).toBeInTheDocument();
  });

  it("keeps raw event filtering secondary to reconstruction", async () => {
    const user = userEvent.setup();
    render(<App initialEvents={successfulTurnEvents()} />);

    await enterEventsSection(user);
    await user.click(screen.getByRole("tab", { name: "Raw stream" }));
    await user.type(screen.getByLabelText("Filter events"), "Tool");

    expect(screen.getByText("ToolRequested")).toBeInTheDocument();
    expect(screen.getByText("ToolCompleted")).toBeInTheDocument();
    expect(screen.queryByText("SpeechFinal")).not.toBeInTheDocument();
  });
});

describe("Pipelines graph editor", () => {
  it("renders a real stored pipeline graph with health and component overlays", async () => {
    const user = userEvent.setup();
    render(<App initialPipelineViews={[pipelineView()]} />);

    await enterPipelinesSection(user);

    expect(
      screen.getByRole("heading", { name: "Graph Editor" }),
    ).toBeInTheDocument();
    expect(screen.getByText("mic")).toBeInTheDocument();
    expect(screen.getByText("stt")).toBeInTheDocument();
    expect(screen.getByText("mic -> stt")).toBeInTheDocument();
    expect(screen.getAllByText("unhealthy").length).toBeGreaterThan(0);
    expect(screen.getByText("synthesis / piper-local")).toBeInTheDocument();
  });

  it("validates edits through the pipeline validation seam before saving", async () => {
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    render(
      <App
        initialPipelineViews={[pipelineView()]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
        onPipelineValidate={(graph) =>
          graph.nodes.some((node) => node.id === "confirm")
            ? { ok: true, order: graph.nodes.map((node) => node.id) }
            : { ok: false, message: "graph is disconnected" }
        }
      />,
    );

    await enterPipelinesSection(user);
    await user.click(screen.getByRole("button", { name: "Validate Graph" }));

    expect(screen.getByText("graph is disconnected")).toBeInTheDocument();
    expect(savedGraphs).toEqual([]);

    await user.click(screen.getByRole("button", { name: "Add tool node" }));
    await user.click(screen.getByRole("button", { name: "Validate Graph" }));
    await user.click(screen.getByRole("button", { name: "Save Graph" }));

    expect(screen.getByText("Validation passed")).toBeInTheDocument();
    expect(savedGraphs[0]?.nodes.map((node) => node.id)).toContain("confirm");
    expect(savedGraphs[0]?.edges).toContainEqual({
      from: "llm",
      to: "confirm",
    });
  });

  it("supports undo, test run, and multiple frontend-only node actions", async () => {
    const user = userEvent.setup();
    render(<App initialPipelineViews={[pipelineView()]} />);

    await enterPipelinesSection(user);
    await user.click(screen.getByRole("button", { name: "Add memory node" }));
    await user.click(screen.getByRole("button", { name: "Add fallback TTS" }));

    expect(screen.getByText("memory")).toBeInTheDocument();
    expect(screen.getByText("tts_fallback")).toBeInTheDocument();
    expect(screen.getByText("2 unsaved edits")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Undo last edit" }));
    expect(screen.queryByText("tts_fallback")).not.toBeInTheDocument();
    expect(screen.getByText("1 unsaved edit")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Run test turn" }));
    expect(
      screen.getByText("Test turn queued for kitchen"),
    ).toBeInTheDocument();
  });

  it("keeps graph editing read-only on small screens", async () => {
    const user = userEvent.setup();
    render(
      <App initialPipelineViews={[pipelineView()]} initialSmallScreen={true} />,
    );

    await enterPipelinesSection(user);

    expect(screen.getByText("Read-only on small screens")).toBeInTheDocument();
    expect(screen.getByText("mic -> stt")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Add tool node" }),
    ).not.toBeInTheDocument();
  });

  it("derives read-only graph mode from the current viewport", async () => {
    const user = userEvent.setup();
    mockSmallScreen(true);
    render(<App initialPipelineViews={[pipelineView()]} />);

    await enterPipelinesSection(user);

    expect(screen.getByText("Read-only on small screens")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Validate Graph" }),
    ).not.toBeInTheDocument();
  });
});

describe("Providers workspace", () => {
  it("renders provider status from the snapshot and filters by stage", async () => {
    const user = userEvent.setup();
    render(<App />);

    await enterProvidersSection(user);

    expect(
      screen.getByRole("heading", { name: "Providers" }),
    ).toBeInTheDocument();
    expect(screen.getByText("piper-local")).toBeInTheDocument();
    expect(
      screen.getByText("no successful reachability check yet"),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "TTS" }));
    expect(screen.getByText("1 visible")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "LLM" }));
    expect(screen.getByText("0 visible")).toBeInTheDocument();
    expect(screen.queryByText("piper-local")).not.toBeInTheDocument();
  });

  it("supports frontend-only provider reachability and fallback actions", async () => {
    const user = userEvent.setup();
    render(<App />);

    await enterProvidersSection(user);
    await user.click(screen.getByRole("button", { name: "Test piper-local" }));

    expect(screen.getByText("Reachability check passed")).toBeInTheDocument();
    expect(screen.getByText("reachable")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Use fallback" }));
    expect(
      screen.getByText("Fallback selected for piper-local"),
    ).toBeInTheDocument();
  });
});

describe("Settings workspace", () => {
  it("stores operator settings in local UI state", async () => {
    const user = userEvent.setup();
    render(<App initialPipelineViews={[pipelineView()]} />);

    await enterSettingsSection(user);
    await user.clear(screen.getByLabelText("Deployment name"));
    await user.type(screen.getByLabelText("Deployment name"), "clinic-prod");
    await user.click(screen.getByLabelText("Local-only mode"));
    await user.click(screen.getByRole("button", { name: "90 d" }));
    await user.selectOptions(screen.getByLabelText("Log level"), "debug");
    await user.click(screen.getByRole("button", { name: "Save settings" }));

    expect(
      screen.getByText("Settings saved for clinic-prod"),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Local-only mode")).not.toBeChecked();
    expect(screen.getByRole("button", { name: "90 d" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByLabelText("Log level")).toHaveValue("debug");
  });

  it("requires explicit confirmation before resetting local console state", async () => {
    const user = userEvent.setup();
    render(<App />);

    await enterSettingsSection(user);
    await user.click(screen.getByRole("button", { name: "Reset local state" }));
    await user.click(screen.getByRole("button", { name: "Confirm reset" }));

    expect(screen.getByText("Type RESET to confirm")).toBeInTheDocument();

    await user.type(screen.getByLabelText("Reset confirmation"), "RESET");
    await user.click(screen.getByRole("button", { name: "Confirm reset" }));

    expect(screen.getByText("Local console state reset")).toBeInTheDocument();
  });
});

function snapshotFixture(): OperatorStatusSnapshot {
  return JSON.parse(
    JSON.stringify(operatorStatusSnapshotFixture),
  ) as OperatorStatusSnapshot;
}

async function enterEventsSection(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "Use anonymous mode" }));
  await user.click(screen.getByRole("tab", { name: "Events" }));
}

function eventFixture(): EventEnvelope[] {
  return JSON.parse(JSON.stringify(eventEnvelopeFixtures)) as EventEnvelope[];
}

function successfulTurnEvents(): EventEnvelope[] {
  return eventFixture().filter(
    (envelope) =>
      envelope.event.type !== "ConversationCancelled" &&
      envelope.event.type !== "ToolFailed" &&
      envelope.event.type !== "StageFailed",
  );
}

async function enterPipelinesSection(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "Use anonymous mode" }));
  await user.click(screen.getByRole("tab", { name: "Pipelines" }));
}

async function enterProvidersSection(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "Use anonymous mode" }));
  await user.click(screen.getByRole("tab", { name: "Providers" }));
}

async function enterSettingsSection(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "Use anonymous mode" }));
  await user.click(screen.getByRole("tab", { name: "Settings" }));
}

function pipelineView(): PipelineView {
  const graph: PipelineGraph = {
    name: "kitchen",
    nodes: [
      { id: "mic", kind: "source", provider: "websocket" },
      { id: "stt", kind: "stt", provider: "whisper" },
      { id: "llm", kind: "llm", provider: "openai" },
      { id: "tts", kind: "tts", provider: "piper-local" },
      { id: "speaker", kind: "sink", provider: "websocket" },
    ],
    edges: [
      { from: "mic", to: "stt" },
      { from: "stt", to: "llm" },
      { from: "llm", to: "tts" },
      { from: "tts", to: "speaker" },
    ],
  };
  return {
    graph,
    order: graph.nodes.map((node) => node.id),
  };
}

function mockSmallScreen(matches: boolean) {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    writable: true,
    value: (query: string): MediaQueryList => ({
      matches,
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(() => false),
    }),
  });
}

function firstRunSnapshot(): OperatorStatusSnapshot {
  const snapshot = healthySnapshot();
  snapshot.runtime.launch_state = "first_run_setup";
  snapshot.pipelines = [];
  snapshot.active_turns = [];
  snapshot.recent_failures = [];
  snapshot.satellites = {
    connected: [],
    recently_active: [],
    recent_window_seconds: 300,
  };
  return snapshot;
}

function healthySnapshot(): OperatorStatusSnapshot {
  const snapshot = snapshotFixture();
  snapshot.recent_failures = [];
  snapshot.active_turns = [];
  snapshot.pipelines = snapshot.pipelines.map((pipeline) => ({
    ...pipeline,
    health: {
      state: "healthy",
      summary: "last invoked turn completed successfully",
      last_successful_turn: "00000000-0000-0000-0000-000000000003",
      last_failed_turn: null,
    },
    components: pipeline.components.map((component) => ({
      ...component,
      state: "healthy",
      detail: "last invoked turn completed",
    })),
  }));
  snapshot.providers = snapshot.providers.map((provider) => ({
    ...provider,
    state: "proven",
    reachable: true,
    proven_by_turn: "00000000-0000-0000-0000-000000000003",
    message: null,
  }));
  return snapshot;
}
