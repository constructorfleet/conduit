import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import App, { OverviewPanel } from "./App";
import type {
  EnrolledSpeaker,
  ProviderComponentCatalog,
  PipelineGraph,
  PipelineView,
  ProviderDefinition,
  ProviderDefinitionView,
} from "./contracts/client";
import { eventEnvelopeFixtures, type EventEnvelope } from "./contracts/events";
import {
  operatorStatusSnapshotFixture,
  type OperatorStatusSnapshot,
  type ProviderStatus,
} from "./contracts/status";
import { applySnapshotEvent, transitionEventStream } from "./eventStream";

beforeEach(() => {
  vi.restoreAllMocks();
  sessionStorage.clear();
  localStorage.clear();
  mockOperatorApi();
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

  it("enters explicit anonymous mode and exposes every top-level section", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );

    for (const section of [
      "Overview",
      "Pipelines",
      "Providers",
      "Speakers",
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

  it("renders current exceptions with exception semantics", () => {
    render(<OverviewPanel snapshot={snapshotFixture()} eventPosture="live" />);

    expect(
      screen.getByRole("list", { name: "Current Exceptions" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("listitem", { name: "Runtime exception: kitchen" }),
    ).toHaveClass("critical");
    expect(
      screen.getByRole("listitem", { name: "Pipeline exception: kitchen" }),
    ).toHaveClass("warning");
    expect(
      screen.getByRole("listitem", { name: "Provider exception: piper-local" }),
    ).toHaveClass("warning");
  });

  it("does not report a reachable provider as an exception", () => {
    // `reachable` means a probe succeeded. Only `proven` counted as healthy,
    // so an operator who had just tested every provider still saw one warning
    // per provider and no way to clear them: a tool provider a turn never
    // calls can never reach `proven` at all.
    const snapshot = healthySnapshot();
    snapshot.providers = snapshot.providers.map((provider) => ({
      ...provider,
      state: "reachable",
      reachable: true,
      proven_by_turn: null,
    }));

    render(<OverviewPanel snapshot={snapshot} eventPosture="live" />);

    expect(screen.getByText("No current exceptions")).toBeInTheDocument();
  });

  it("still reports a provider that is not reachable", () => {
    const snapshot = healthySnapshot();
    snapshot.providers = snapshot.providers.map((provider) => ({
      ...provider,
      state: "configured",
      reachable: false,
      proven_by_turn: null,
      message: "connection refused",
    }));

    render(<OverviewPanel snapshot={snapshot} eventPosture="live" />);

    expect(
      screen.getByRole("listitem", { name: "Provider exception: piper-local" }),
    ).toHaveClass("warning");
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
    expect(screen.queryByText("Snapshot")).not.toBeInTheDocument();
    expect(screen.getAllByText("live").length).toBeGreaterThan(0);
  });

  it("loads status and pipeline graph data from the API after access", async () => {
    const user = userEvent.setup();
    const snapshot = liveApiSnapshot();
    const pipeline = liveApiPipelineView();
    const fetchMock = mockOperatorApi({
      snapshot,
      pipelineViews: [pipeline],
    });
    render(<App />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );

    expect(await screen.findByText("Garage Satellite")).toBeInTheDocument();
    expect(screen.getAllByText("garage-tts").length).toBeGreaterThan(0);
    expect(screen.queryByText("piper-local")).not.toBeInTheDocument();

    await user.click(screen.getByRole("tab", { name: "Pipelines" }));

    expect(screen.getByText("garage_mic")).toBeInTheDocument();
    expect(screen.getByText("garage_tts")).toBeInTheDocument();
    expect(screen.getByLabelText("Pipeline stages")).toBeInTheDocument();
    expect(fetchMock).toHaveBeenCalledWith(
      new URL("/v1/status", window.location.origin),
      expect.objectContaining({
        headers: expect.objectContaining({ accept: "application/json" }),
      }),
    );
  });

  it("can use explicit mock data without calling the live API", async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    render(<App dataMode="mock" />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );

    expect(await screen.findByText("Kitchen Satellite")).toBeInTheDocument();
    expect(screen.getAllByText("piper-local").length).toBeGreaterThan(0);
    expect(fetchMock).not.toHaveBeenCalled();
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
      screen.queryByLabelText("Operator Console sections"),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("tab", { name: "Overview" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Guided Setup" }),
    ).toBeInTheDocument();
  });

  it("keeps stored but unusable pipelines out of full-screen First-Run Setup", async () => {
    const user = userEvent.setup();
    mockOperatorApi({
      snapshot: storedButNotUsableSnapshot(),
      pipelineViews: [pipelineView()],
    });
    render(<App />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );

    expect(
      await screen.findByLabelText("Operator Console sections"),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "First-Run Setup" }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("tab", { name: "Pipelines" }));

    expect(screen.getByText("kitchen")).toBeInTheDocument();
    expect(screen.getByLabelText("Pipeline stages")).toHaveTextContent("stt");
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
          {
            id: "core",
            kind: "core",
            core: { model: { provider: "openai" }, max_rounds: 4 },
          },
          { id: "tts", kind: "tts", provider: "piper" },
          { id: "speaker", kind: "sink", provider: "websocket" },
        ],
        edges: [
          { from: "mic", to: "stt" },
          { from: "stt", to: "core" },
          { from: "core", to: "tts" },
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

  it("asks which model the language model provider should serve", async () => {
    // Guided Setup stored no model at all, so the pipeline it built had
    // nothing to ask the provider for — and the runtime then asked for a
    // model named after the provider itself.
    const user = userEvent.setup();
    render(<App initialSnapshot={firstRunSnapshot()} />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );
    await user.click(
      screen.getByRole("button", { name: "Configure Providers" }),
    );

    const model = screen.getByLabelText("Language model");
    expect(model).toHaveValue("gpt-4o-mini");
    await user.clear(model);
    await user.type(model, "gpt-4o");
    expect(model).toHaveValue("gpt-4o");
  });

  it("builds a text pipeline that asks for no speech providers", async () => {
    // The minimal working loop for a text assistant needs a language model and
    // nothing else, so guided setup must not demand speech providers that
    // would never run.
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
    await user.selectOptions(screen.getByLabelText("Pipeline shape"), "text");

    // Opened deliberately: the fields must be absent because the shape needs
    // no speech providers, not merely because the panel is closed.
    await user.click(
      screen.getByRole("button", { name: "Configure Providers" }),
    );
    expect(
      screen.getByLabelText("Language model provider"),
    ).toBeInTheDocument();
    expect(screen.queryByLabelText("Speech-to-text provider")).toBeNull();
    expect(screen.queryByLabelText("Text-to-speech provider")).toBeNull();

    await user.clear(screen.getByLabelText("Pipeline name"));
    await user.type(screen.getByLabelText("Pipeline name"), "chat");
    await user.click(screen.getByRole("button", { name: "Skip tool setup" }));
    await user.click(screen.getByRole("button", { name: "Validate and Save" }));

    expect(savedGraphs).toEqual([
      {
        name: "chat",
        nodes: [
          { id: "in", kind: "source", provider: "websocket", modality: "text" },
          {
            id: "core",
            kind: "core",
            core: { model: { provider: "openai" }, max_rounds: 4 },
          },
          {
            id: "out",
            kind: "sink",
            provider: "websocket",
            modality: "text",
          },
        ],
        edges: [
          { from: "in", to: "core" },
          { from: "core", to: "out" },
        ],
      },
    ]);
  });

  it("persists Guided Setup pipeline saves across reloads", async () => {
    const user = userEvent.setup();
    mockOperatorApi({ snapshot: firstRunSnapshot(), pipelineViews: [] });
    const firstLoad = render(<App />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );
    await user.clear(screen.getByLabelText("Pipeline name"));
    await user.type(screen.getByLabelText("Pipeline name"), "kitchen");
    await user.click(screen.getByRole("button", { name: "Validate and Save" }));

    expect(
      await screen.findByRole("heading", { name: "Overview" }),
    ).toBeInTheDocument();

    firstLoad.unmount();
    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "Overview" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "First-Run Setup" }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("tab", { name: "Pipelines" }));

    expect(screen.getByText("kitchen")).toBeInTheDocument();
    expect(screen.getByLabelText("Pipeline stages")).toHaveTextContent("stt");
  });

  it("creates provider definitions before saving the first guided pipeline", async () => {
    const user = userEvent.setup();
    const fetchMock = mockOperatorApi({
      snapshot: firstRunSnapshot(),
      pipelineViews: [],
    });
    render(<App />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );
    await user.clear(screen.getByLabelText("Pipeline name"));
    await user.type(screen.getByLabelText("Pipeline name"), "kitchen");
    await user.click(screen.getByRole("button", { name: "Validate and Save" }));

    expect(
      await screen.findByRole("heading", { name: "Overview" }),
    ).toBeInTheDocument();
    const writes = fetchMock.mock.calls
      .map(([input, init]) => ({
        route: new URL(input.toString()).pathname,
        method: init?.method ?? "GET",
      }))
      .filter((call) => call.method === "PUT");

    expect(writes.slice(0, 3)).toEqual([
      { route: "/v1/providers/whisper", method: "PUT" },
      { route: "/v1/providers/openai", method: "PUT" },
      { route: "/v1/providers/piper", method: "PUT" },
    ]);
    expect(writes[3]).toEqual({
      route: "/v1/pipelines/kitchen",
      method: "PUT",
    });
  });

  it("leaves First-Run Setup after reload when the pipeline API has a saved graph but status is stale", async () => {
    const user = userEvent.setup();
    mockOperatorApi({
      snapshot: firstRunSnapshot(),
      pipelineViews: [],
      updateSnapshotOnPipelineSave: false,
    });
    const firstLoad = render(<App />);

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );
    await user.clear(screen.getByLabelText("Pipeline name"));
    await user.type(screen.getByLabelText("Pipeline name"), "kitchen");
    await user.click(screen.getByRole("button", { name: "Validate and Save" }));

    expect(
      await screen.findByRole("heading", { name: "Overview" }),
    ).toBeInTheDocument();

    firstLoad.unmount();
    render(<App />);

    expect(
      await screen.findByLabelText("Operator Console sections"),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "First-Run Setup" }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("tab", { name: "Pipelines" }));

    expect(screen.getByText("kitchen")).toBeInTheDocument();
    expect(screen.getByLabelText("Pipeline stages")).toHaveTextContent("stt");
  });
});

describe("Events turn reconstruction", () => {
  it("groups reconstructed events into visual component stages", async () => {
    const user = userEvent.setup();
    render(<App initialEvents={successfulTurnEvents()} />);

    await enterEventsSection(user);

    expect(screen.getByText("Stage Timeline")).toBeInTheDocument();
    expect(screen.getAllByRole("group", { name: /stage$/ })).toHaveLength(5);
    expect(
      screen.queryByRole("group", { name: "conversation stage" }),
    ).not.toBeInTheDocument();
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

  it("attributes node failures to the matching visual component stage", async () => {
    const user = userEvent.setup();
    render(<App initialEvents={eventFixture()} />);

    await enterEventsSection(user);

    expect(
      screen.queryByRole("group", { name: "node: tts stage" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("group", { name: "synthesis stage" }),
    ).toHaveTextContent("Stage Failed");
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
    expect(screen.getByLabelText("Turn pipeline")).toHaveTextContent(
      "Pipeline kitchen",
    );
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
    expect(
      screen.getByRole("listitem", { name: "StageFailed tts error" }),
    ).toHaveClass("error");
  });

  it("uses stage event pills as ordered shortcuts into reconstruction rows", async () => {
    const user = userEvent.setup();
    render(<App initialEvents={eventFixture()} />);

    await enterEventsSection(user);

    const synthesis = screen.getByRole("group", { name: "synthesis stage" });
    const pills = within(synthesis).getAllByRole("button");

    expect(pills.map((pill) => pill.textContent)).toEqual([
      "Tts Started",
      "Audio Streaming",
      "Tts Finished",
      "Stage Failed",
    ]);
    expect(pills[3]).toHaveClass("error");
    expect(pills[3]).toHaveAccessibleDescription("connection refused");

    await user.click(pills[3]);

    expect(
      screen.getByRole("listitem", { name: "StageFailed tts error" }),
    ).toHaveClass("selected");
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

  it("does not mark events stale before a stream has gone stale", async () => {
    const user = userEvent.setup();
    render(<App initialEvents={successfulTurnEvents()} />);

    await enterEventsSection(user);

    expect(screen.queryByLabelText("Stale state")).not.toBeInTheDocument();
    expect(
      screen.queryByText("Reconnect refresh required"),
    ).not.toBeInTheDocument();
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
      screen.getByRole("heading", { name: "Pipeline Editor" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Pipeline stages")).toHaveTextContent(
      "micsource / websocketsttstt / whisperllmcore / openaittstts / piper-localspeakersink / websocket",
    );
    expect(screen.getByLabelText("Pipeline selector")).toHaveTextContent(
      "Pipeline1kitchen",
    );
    expect(screen.getByLabelText("Speech to text provider")).toHaveValue(
      "whisper",
    );
    expect(screen.getByLabelText("Text to speech provider")).toHaveValue(
      "piper-local",
    );
  });

  it("validates edits through the pipeline validation seam before saving", async () => {
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    render(
      <App
        initialPipelineViews={[pipelineView()]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
        onPipelineValidate={(graph) =>
          graph.nodes.some(
            (node) =>
              node.kind === "core" && (node.core.tools?.length ?? 0) > 0,
          )
            ? { ok: true, order: graph.nodes.map((node) => node.id) }
            : { ok: false, message: "graph is disconnected" }
        }
      />,
    );

    await enterPipelinesSection(user);
    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));

    expect(screen.getByText("graph is disconnected")).toBeInTheDocument();
    expect(savedGraphs).toEqual([]);

    await user.click(screen.getByRole("button", { name: "Add tool" }));
    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));
    await user.click(screen.getByRole("button", { name: "Save pipeline" }));

    expect(screen.getByText("Validation passed")).toBeInTheDocument();
    // Binding a tool adds no node and no edge: the graph it validated is the
    // graph it had, with one more thing bound to the core.
    const core = savedGraphs[0]?.nodes.find((node) => node.kind === "core");
    expect(core?.kind === "core" ? core.core.tools : []).toContainEqual({
      provider: "builtin.confirm",
      confirm: "never",
    });
  });

  it("validates pipeline edits through the API by default", async () => {
    const user = userEvent.setup();
    const fetchMock = mockOperatorApi({
      pipelineViews: [pipelineView()],
    });
    render(<App initialPipelineViews={[pipelineView()]} />);

    await enterPipelinesSection(user);
    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));

    expect(await screen.findByText("Validation passed")).toBeInTheDocument();
    expect(fetchMock).toHaveBeenCalledWith(
      expect.objectContaining({
        pathname: "/v1/pipelines/validate",
      }),
      expect.objectContaining({
        method: "POST",
      }),
    );
  });

  it("selects a configured provider for a pipeline node", async () => {
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView()]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
      />,
    );

    await enterPipelinesSection(user);
    await user.selectOptions(screen.getByLabelText("Model provider"), "openai");
    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));
    await user.click(screen.getByRole("button", { name: "Save pipeline" }));

    const llm = savedGraphs[0]?.nodes.find((node) => node.id === "llm");
    expect(llm).toMatchObject({
      kind: "core",
      core: { model: { provider: "openai" } },
    });
  });

  it("asks one pipeline's model of a provider definition shared with another", async () => {
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView()]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
      />,
    );

    await enterPipelinesSection(user);
    await user.type(screen.getByLabelText("Model"), "qwen3:8b");
    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));
    await user.click(screen.getByRole("button", { name: "Save pipeline" }));

    const llm = savedGraphs[0]?.nodes.find((node) => node.id === "llm");
    expect(llm).toMatchObject({
      kind: "core",
      core: { model: { model: "qwen3:8b" } },
    });
  });

  it("carries a voice on the synthesis node", async () => {
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView()]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
      />,
    );

    await enterPipelinesSection(user);
    await user.type(screen.getByLabelText("Voice"), "en_GB-alba");
    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));
    await user.click(screen.getByRole("button", { name: "Save pipeline" }));

    const tts = savedGraphs[0]?.nodes.find((node) => node.id === "tts");
    expect(tts).toMatchObject({ kind: "tts", voice: "en_GB-alba" });
  });

  it("clears an emptied model rather than asking for a model named nothing", async () => {
    // The graph field is optional, and absent means "whichever model the
    // provider serves first". An empty string would instead ask the provider
    // for a model with no name.
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView()]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
      />,
    );

    await enterPipelinesSection(user);
    const model = screen.getByLabelText("Model");
    await user.type(model, "gpt-4o");
    await user.clear(model);
    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));
    await user.click(screen.getByRole("button", { name: "Save pipeline" }));

    const llm = savedGraphs[0]?.nodes.find((node) => node.id === "llm");
    expect(llm).toMatchObject({ kind: "core" });
    expect(
      llm?.kind === "core" ? llm.core.model.model : "unset",
    ).toBeUndefined();
  });

  it("marks each pipeline link with the modality it carries", async () => {
    const user = userEvent.setup();
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView()]}
      />,
    );

    await enterPipelinesSection(user);

    // Recognition turns audio into text and the model answers with an
    // utterance, so three consecutive stages hand on three different things.
    expect(screen.getByLabelText("mic stage").dataset.modality).toBe("audio");
    expect(screen.getByLabelText("stt stage").dataset.modality).toBe("text");
    expect(screen.getByLabelText("llm stage").dataset.modality).toBe(
      "utterance",
    );
  });

  it("declares whether a source carries speech or written words", async () => {
    // Changing what an endpoint carries changes what every link downstream of
    // it carries, which is how an operator sees a miswiring before saving it.
    const user = userEvent.setup();
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView()]}
      />,
    );

    await enterPipelinesSection(user);
    expect(screen.getByLabelText("mic stage").dataset.modality).toBe("audio");

    await user.selectOptions(screen.getByLabelText("Source modality"), "text");

    expect(screen.getByLabelText("mic stage").dataset.modality).toBe("text");
  });

  it("uses providers configured on the Providers page as pipeline choices", async () => {
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView()]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
      />,
    );

    await enterProvidersSection(user);
    await user.click(screen.getByRole("button", { name: "Add provider" }));
    await user.click(screen.getByRole("menuitem", { name: "Language model" }));
    await user.click(
      screen.getByRole("menuitem", { name: "OpenAI Responses" }),
    );
    expect(
      screen.getByRole("button", { name: "Cancel provider edit" }),
    ).toBeInTheDocument();
    await user.clear(screen.getByLabelText("Provider id"));
    await user.type(screen.getByLabelText("Provider id"), "openai-fast");
    await user.clear(screen.getByLabelText("Provider label"));
    await user.type(screen.getByLabelText("Provider label"), "OpenAI Fast");
    await user.selectOptions(
      screen.getByLabelText("Provider component"),
      "openai.completions",
    );
    await user.type(
      screen.getByLabelText("Base URL"),
      "https://api.openai.com/v1",
    );
    await user.type(screen.getByLabelText("Model"), "gpt.5");
    await user.click(screen.getByLabelText("Streaming"));
    await user.click(screen.getByRole("button", { name: "Save provider" }));

    expect(screen.getByText("Provider openai-fast saved")).toBeInTheDocument();

    await user.click(screen.getByRole("tab", { name: "Pipelines" }));
    await user.selectOptions(
      screen.getByLabelText("Model provider"),
      "openai-fast",
    );
    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));
    await user.click(screen.getByRole("button", { name: "Save pipeline" }));

    expect(
      savedGraphs[0]?.nodes.find((node) => node.id === "llm"),
    ).toMatchObject({
      kind: "core",
      core: { model: { provider: "openai-fast" } },
    });
  });

  it("does not copy already-selected provider definitions into saved graphs", async () => {
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    localStorage.setItem(
      "conduit.provider.definitions",
      JSON.stringify([
        {
          id: "openai-fast",
          label: "OpenAI Fast",
          kind: "llm",
          component: "openai.responses",
          config: {
            base_url: "https://api.openai.com/v1",
            model: "gpt-5",
            streaming: true,
          },
        },
      ]),
    );
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[
          {
            ...pipelineView(),
            graph: {
              ...pipelineView().graph,
              nodes: pipelineView().graph.nodes.map((node) =>
                node.kind === "core"
                  ? {
                      ...node,
                      core: {
                        ...node.core,
                        model: { provider: "openai-fast" },
                      },
                    }
                  : node,
              ),
            },
          },
        ]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
      />,
    );

    await enterPipelinesSection(user);
    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));
    await user.click(screen.getByRole("button", { name: "Save pipeline" }));

    expect(
      savedGraphs[0]?.nodes.find((node) => node.id === "llm"),
    ).toMatchObject({
      kind: "core",
      core: { model: { provider: "openai-fast" } },
    });
  });

  it("does not repair loaded graphs by embedding browser provider definitions", async () => {
    const user = userEvent.setup();
    const staleView = {
      ...pipelineView(),
      graph: {
        ...pipelineView().graph,
        nodes: pipelineView().graph.nodes.map((node) =>
          node.id === "tts" ? { ...node, provider: "piper" } : node,
        ),
      },
    };
    localStorage.setItem(
      "conduit.provider.definitions",
      JSON.stringify([
        {
          id: "piper",
          label: "piper",
          kind: "tts",
          component: "wyoming",
          config: {
            url: "tcp://10.0.10.100:10200",
            model: "en_US-ryan-high",
            streaming: true,
          },
        },
      ]),
    );
    mockOperatorApi({ pipelineViews: [staleView] });

    render(<App />);
    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );

    await user.click(screen.getByRole("tab", { name: "Pipelines" }));

    expect(await screen.findByLabelText("tts stage")).toHaveTextContent(
      "piper",
    );
  });

  it("keeps saved OpenAI and Wyoming provider configuration over inferred defaults", async () => {
    const user = userEvent.setup();
    const providerDefinitions = [
      providerDefinitionFixture({
        id: "openai",
        label: "OpenAI Primary",
        component: "openai.responses",
        config: {
          base_url: "https://api.openai.com/v1",
          api_key: "sk-test",
          model: "gpt-5",
          streaming: true,
        },
      }),
      providerDefinitionFixture({
        id: "whisper",
        label: "Whisper Local",
        component: "wyoming",
        config: {
          url: "tcp://whisper.local:10300",
          model: "tiny-int8",
          streaming: true,
        },
      }),
      providerDefinitionFixture({
        id: "piper-local",
        label: "Piper Local",
        component: "wyoming.tts",
        config: {
          url: "tcp://piper.local:10200",
          voice: "en_US-lessac-medium",
        },
      }),
    ];

    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView()]}
        initialProviderDefinitions={providerDefinitions}
      />,
    );

    await enterProvidersSection(user);
    const openAiRow = screen.getByRole("row", { name: /OpenAI Primary/ });
    await user.click(
      within(openAiRow).getByRole("button", {
        name: "Edit openai",
      }),
    );

    expect(screen.getByLabelText("Provider component")).toHaveDisplayValue(
      "OpenAI Responses",
    );
    expect(screen.getByLabelText("Base URL")).toHaveDisplayValue(
      "https://api.openai.com/v1",
    );
    expect(screen.getByLabelText("API Key")).toHaveDisplayValue("sk-test");
    expect(screen.getByLabelText("Model")).toHaveDisplayValue("gpt-5");
    await user.click(
      screen.getByRole("button", { name: "Cancel provider edit" }),
    );

    const whisperRow = screen.getByRole("row", { name: /Whisper Local/ });
    await user.click(
      within(whisperRow).getByRole("button", {
        name: "Edit whisper",
      }),
    );

    expect(screen.getByLabelText("Provider component")).toHaveDisplayValue(
      "Wyoming",
    );
    expect(screen.getByLabelText("URL")).toHaveDisplayValue(
      "tcp://whisper.local:10300",
    );
    expect(screen.getByLabelText("Model")).toHaveDisplayValue("tiny-int8");
    expect(screen.getByLabelText("Streaming")).toBeChecked();
    await user.click(
      screen.getByRole("button", { name: "Cancel provider edit" }),
    );

    const piperRow = screen.getByRole("row", { name: /Piper Local/ });
    await user.click(
      within(piperRow).getByRole("button", {
        name: "Edit piper-local",
      }),
    );

    expect(screen.getByLabelText("Provider component")).toHaveDisplayValue(
      "Wyoming TTS",
    );
    expect(screen.getByLabelText("URL")).toHaveDisplayValue(
      "tcp://piper.local:10200",
    );
    expect(screen.getByLabelText("Voice")).toHaveDisplayValue(
      "en_US-lessac-medium",
    );
  });

  it("normalizes saved TTS Wyoming provider definitions to the TTS schema", async () => {
    const user = userEvent.setup();
    const providerDefinitions = [
      providerDefinitionFixture({
        id: "piper",
        label: "piper",
        kind: "tts",
        component: "wyoming",
        config: {
          url: "tcp://10.0.10.100:10200",
          model: "en_US-ryan-high",
          streaming: true,
        },
      }),
    ];

    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialProviderDefinitions={providerDefinitions}
      />,
    );

    await enterProvidersSection(user);
    const piperRow = screen
      .getByRole("button", { name: "Edit piper" })
      .closest("tr") as HTMLElement;
    await user.click(
      within(piperRow).getByRole("button", {
        name: "Edit piper",
      }),
    );

    expect(screen.getByLabelText("Provider component")).toHaveDisplayValue(
      "Wyoming TTS",
    );
    expect(screen.getByLabelText("URL")).toHaveDisplayValue(
      "tcp://10.0.10.100:10200",
    );
    expect(screen.getByLabelText("Voice")).toHaveDisplayValue(
      "en_US-ryan-high",
    );
    expect(screen.getByLabelText("Streaming")).toBeChecked();
  });

  it("does not overwrite an existing provider with an invalid configuration", async () => {
    const user = userEvent.setup();
    render(<App initialComponentCatalog={componentCatalog()} />);

    await enterProvidersSection(user);
    await user.click(screen.getByRole("button", { name: "Add provider" }));
    await user.click(screen.getByRole("menuitem", { name: "Language model" }));
    await user.click(
      screen.getByRole("menuitem", { name: "OpenAI Responses" }),
    );
    await user.clear(screen.getByLabelText("Provider id"));
    await user.type(screen.getByLabelText("Provider id"), "openai");
    await user.clear(screen.getByLabelText("Provider label"));
    await user.type(screen.getByLabelText("Provider label"), "Broken OpenAI");

    expect(
      screen.getByText("Missing required fields: Base URL, Model"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Save provider" }),
    ).toBeDisabled();

    await user.type(
      screen.getByLabelText("Base URL"),
      "https://api.openai.com/v1",
    );
    expect(
      screen.getByText("Missing required fields: Model"),
    ).toBeInTheDocument();
    await user.type(screen.getByLabelText("Model"), "gpt-5");
    expect(
      screen.queryByText(/Missing required fields/),
    ).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Save provider" }));

    expect(
      screen.getByRole("row", { name: /Broken OpenAI/ }),
    ).toBeInTheDocument();
  });

  it("renders the pipeline as a form and exposes editor actions", async () => {
    const user = userEvent.setup();
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView()]}
      />,
    );

    await enterPipelinesSection(user);

    const editor = screen.getByLabelText("Pipeline configuration");
    for (const stage of ["Input", "Reasoning", "Output"]) {
      expect(within(editor).getByLabelText(stage)).toBeInTheDocument();
    }
    expect(within(editor).getByLabelText("Pipeline stages")).toHaveTextContent(
      "micsource / websocketsttstt / whisperllmcore / openaittstts / piper-localspeakersink / websocket",
    );

    const toolbar = screen.getByRole("toolbar", {
      name: "Pipeline editor actions",
    });
    for (const action of [
      "Validate pipeline",
      "Run test turn",
      "Save pipeline",
    ]) {
      expect(
        within(toolbar).getByRole("button", { name: action }),
      ).toBeInTheDocument();
    }

    await user.click(screen.getByLabelText("Remove Text to speech"));

    expect(
      within(screen.getByLabelText("Pipeline stages")).queryByText(
        "tts / piper-local",
      ),
    ).not.toBeInTheDocument();
    expect(screen.getByText("1 unsaved edit")).toBeInTheDocument();
    expect(screen.getByLabelText("Add Text to speech")).toBeEnabled();
  });

  it("keeps mic and speaker as functional graph endpoints when saving", async () => {
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    const scrambled = pipelineView();
    const nodeById = Object.fromEntries(
      scrambled.graph.nodes.map((node) => [node.id, node]),
    ) as Record<string, PipelineGraph["nodes"][number]>;
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[
          {
            graph: {
              ...scrambled.graph,
              nodes: [
                nodeById.speaker,
                nodeById.llm,
                nodeById.tts,
                nodeById.stt,
                nodeById.mic,
              ],
              edges: [
                { from: "speaker", to: "tts" },
                { from: "tts", to: "llm" },
                { from: "llm", to: "stt" },
                { from: "stt", to: "mic" },
              ],
            },
            order: ["speaker", "tts", "llm", "stt", "mic"],
          },
        ]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
      />,
    );

    await enterPipelinesSection(user);
    expect(screen.getByLabelText("Pipeline stages")).toHaveTextContent(
      "micsource / websocketsttstt / whisperllmcore / openaittstts / piper-localspeakersink / websocket",
    );

    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));
    await user.click(screen.getByRole("button", { name: "Save pipeline" }));

    expect(savedGraphs[0]?.nodes.map((node) => node.id)).toEqual([
      "mic",
      "stt",
      "llm",
      "tts",
      "speaker",
    ]);
    expect(savedGraphs[0]?.edges).toEqual([
      { from: "mic", to: "stt" },
      { from: "stt", to: "llm" },
      { from: "llm", to: "tts" },
      { from: "tts", to: "speaker" },
    ]);
  });

  it("restores deleted TTS between the LLM and speaker endpoints", async () => {
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView()]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
      />,
    );

    await enterPipelinesSection(user);
    await user.click(screen.getByLabelText("Remove Text to speech"));
    await user.click(screen.getByLabelText("Add Text to speech"));
    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));
    await user.click(screen.getByRole("button", { name: "Save pipeline" }));

    expect(savedGraphs[0]?.nodes.map((node) => node.id)).toEqual([
      "mic",
      "stt",
      "llm",
      "tts",
      "speaker",
    ]);
    expect(savedGraphs[0]?.edges).toContainEqual({ from: "llm", to: "tts" });
    expect(savedGraphs[0]?.edges).toContainEqual({
      from: "tts",
      to: "speaker",
    });
  });

  it("keeps unsaved drafts when switching between pipelines", async () => {
    const user = userEvent.setup();
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView(), liveApiPipelineView()]}
      />,
    );

    await enterPipelinesSection(user);
    await user.click(screen.getByRole("button", { name: "Add memory" }));
    expect(screen.getByText("builtin.memory")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "garage" }));
    expect(
      screen.getByRole("region", { name: "Unsaved pipeline changes" }),
    ).toHaveTextContent("kitchen has unsaved edits");
    expect(
      screen.getByRole("button", { name: "Save current and switch" }),
    ).toBeDisabled();
    await user.click(
      screen.getByRole("button", { name: "Switch without saving" }),
    );

    expect(screen.getByText("garage_mic")).toBeInTheDocument();
    expect(screen.queryByText("builtin.memory")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "kitchen" }));
    expect(screen.getByText("builtin.memory")).toBeInTheDocument();
    expect(screen.getByText("1 unsaved edit")).toBeInTheDocument();
  });

  it("binds a tool to the core rather than inserting it after the selected node", async () => {
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView()]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
      />,
    );

    await enterPipelinesSection(user);
    await user.click(screen.getByRole("button", { name: "Add tool" }));
    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));
    await user.click(screen.getByRole("button", { name: "Save pipeline" }));

    // Selecting a node used to decide where a tool was inserted. A binding
    // has nowhere to be inserted, so the selection cannot misplace it.
    const core = savedGraphs[0]?.nodes.find((node) => node.kind === "core");
    expect(core?.kind === "core" ? core.core.tools : []).toContainEqual({
      provider: "builtin.confirm",
      confirm: "never",
    });
    expect(savedGraphs[0]?.edges).toEqual(pipelineView().graph.edges);
  });

  it("adds separate tool augments for distinct configured tool provider ids", async () => {
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    const providerDefinitions = [
      providerDefinitionFixture({
        id: "calendar-tool",
        label: "Calendar Tool",
        component: "mcp.sse",
        config: { url: "https://calendar.example.test/sse" },
      }),
      providerDefinitionFixture({
        id: "lights-tool",
        label: "Lights Tool",
        component: "mcp.streamable_http",
        config: { url: "https://lights.example.test/mcp" },
      }),
    ];
    mockOperatorApi({ providerDefinitions });
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView()]}
        initialProviderDefinitions={providerDefinitions}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
      />,
    );

    await enterPipelinesSection(user);
    await user.click(screen.getByRole("button", { name: "Add tool" }));
    await user.click(screen.getByRole("button", { name: "Add tool" }));
    await user.selectOptions(
      screen.getByLabelText("Tool 2 provider"),
      "lights-tool",
    );
    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));
    await user.click(screen.getByRole("button", { name: "Save pipeline" }));

    // Two bindings on the one core, and no new nodes or edges: a tool is
    // configuration rather than a stage the reply passes through.
    const core = savedGraphs[0]?.nodes.find((node) => node.kind === "core");
    expect(core?.kind === "core" ? core.core.tools : []).toEqual([
      { provider: "calendar-tool", confirm: "never" },
      { provider: "lights-tool", confirm: "never" },
    ]);
    expect(
      savedGraphs[0]?.nodes.filter((node) => node.kind === "core"),
    ).toHaveLength(1);
  });

  it("binds each memory store separately to the core", async () => {
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    render(
      <App
        initialPipelineViews={[pipelineView()]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
      />,
    );

    await enterPipelinesSection(user);
    await user.click(screen.getByRole("button", { name: "Add memory" }));
    await user.click(screen.getByRole("button", { name: "Add memory" }));
    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));
    await user.click(screen.getByRole("button", { name: "Save pipeline" }));

    // Two bindings, not two nodes with invented unique ids. A binding is
    // identified by its position on the core, so nothing has to be made
    // unique for it to be told apart from its neighbour.
    const core = savedGraphs[0]?.nodes.find((node) => node.kind === "core");
    expect(core?.kind === "core" ? core.core.memory : []).toHaveLength(2);
    expect(savedGraphs[0]?.edges).toEqual(pipelineView().graph.edges);
  });

  it("persists saved graph edits across reloads", async () => {
    const user = userEvent.setup();
    mockOperatorApi({
      snapshot: snapshotFixture(),
      pipelineViews: [pipelineView()],
    });
    const firstLoad = render(<App />);

    await enterPipelinesSection(user);
    await user.click(screen.getByRole("button", { name: "Add tool" }));
    await user.click(screen.getByRole("button", { name: "Validate pipeline" }));
    await user.click(screen.getByRole("button", { name: "Save pipeline" }));

    expect(
      await screen.findByText("Saved graph for kitchen"),
    ).toBeInTheDocument();

    firstLoad.unmount();
    render(<App />);
    await user.click(screen.getByRole("tab", { name: "Pipelines" }));

    expect(await screen.findByLabelText("Tool 1 provider")).toHaveValue(
      "builtin.confirm",
    );
  });

  it("supports undo and test run over core bindings", async () => {
    const user = userEvent.setup();
    render(<App initialPipelineViews={[pipelineView()]} />);

    await enterPipelinesSection(user);
    await user.click(screen.getByRole("button", { name: "Add memory" }));

    expect(screen.getByText("builtin.memory")).toBeInTheDocument();
    expect(screen.getByLabelText("Memory 1 mode")).toHaveValue("read_write");
    expect(screen.getByText("1 unsaved edit")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Undo last edit" }));
    expect(screen.queryByText("builtin.memory")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Run test turn" }));
    expect(
      await screen.findByText(
        "Test turn completed for kitchen: 24 audio bytes",
      ),
    ).toBeInTheDocument();

    // The reply is audio. It used to be rendered as lossy UTF-8 of the raw
    // samples, which put mojibake on screen instead of something an operator
    // could listen to.
    const player = await screen.findByLabelText("Test turn reply audio");
    expect(player).toHaveAttribute(
      "src",
      expect.stringContaining("data:audio/wav;base64,"),
    );
  });

  it("validates and saves the current pipeline draft before running a test turn", async () => {
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    const testedPipelines: string[] = [];
    render(
      <App
        initialPipelineViews={[pipelineView()]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
        onPipelineValidate={(graph) => ({
          ok: true,
          order: graph.nodes.map((node) => node.id),
        })}
        onPipelineTest={async (name) => {
          testedPipelines.push(name);
          return {
            message: `Test turn completed for ${name}: validated draft.`,
            replyAudio: null,
          };
        }}
      />,
    );

    await enterPipelinesSection(user);
    await user.click(screen.getByRole("button", { name: "Add memory" }));
    await user.click(screen.getByRole("button", { name: "Run test turn" }));

    expect(
      await screen.findByText(
        "Test turn completed for kitchen: validated draft.",
      ),
    ).toBeInTheDocument();
    const savedCore = savedGraphs[0]?.nodes.find(
      (node) => node.kind === "core",
    );
    expect(
      savedCore?.kind === "core" ? savedCore.core.memory : [],
    ).toHaveLength(1);
    expect(testedPipelines).toEqual(["kitchen"]);
  });

  it("lists an MCP server once and still offers each tool it advertises", async () => {
    // A server advertising a dozen tools used to put a dozen entries on the
    // Providers page — and health-checked every one of them per snapshot.
    const user = userEvent.setup();
    const snapshot = snapshotFixture();
    snapshot.providers = [
      {
        id: "plex",
        kind: "tool",
        state: "reachable",
        configured: true,
        reachable: true,
        proven_by_turn: null,
        message: null,
        affects_pipelines: [],
        offers_tools: ["plex.search_movies", "plex.play_media"],
      },
    ];
    render(
      <App
        initialSnapshot={snapshot}
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView()]}
      />,
    );

    await user.click(
      screen.getByRole("button", { name: "Use anonymous mode" }),
    );
    await user.click(screen.getByRole("tab", { name: "Providers" }));

    // One card for the one thing configured, and none for its tools.
    expect(screen.getAllByText("plex").length).toBeGreaterThan(0);
    expect(screen.queryByText("plex.search_movies")).toBeNull();
    expect(screen.queryByText("plex.play_media")).toBeNull();

    // And its tools are still bindable, because a core binds them one at a
    // time even though the server is one provider.
    await user.click(screen.getByRole("tab", { name: "Pipelines" }));
    await user.click(screen.getByRole("button", { name: "Add tool" }));
    expect(
      within(screen.getByLabelText("Tool 1 provider")).getByRole("option", {
        name: "plex.search_movies",
      }),
    ).toBeInTheDocument();
  });

  it("shows a pipeline the server cannot read, and offers to delete it", async () => {
    // Stored graphs are not migrated across schema changes, so a name whose
    // graph will not parse is a state an operator can land in. Before this,
    // one of them hid every other pipeline and there was no way back.
    const user = userEvent.setup();
    const deleted: string[] = [];
    render(
      <App
        initialPipelineViews={[pipelineView()]}
        initialUnreadablePipelines={[
          { name: "broken", detail: "unknown variant `llm`" },
        ]}
        onPipelineDeleted={(name) => deleted.push(name)}
      />,
    );

    await enterPipelinesSection(user);

    // The readable pipeline is still there, which is the part that used to be
    // lost entirely.
    expect(screen.getByLabelText("Pipeline configuration")).toBeInTheDocument();

    expect(screen.getByText("unknown variant `llm`")).toBeInTheDocument();
    await user.click(
      screen.getByRole("button", { name: "Delete pipeline broken" }),
    );

    expect(deleted).toEqual(["broken"]);
    expect(
      screen.queryByRole("button", { name: "Delete pipeline broken" }),
    ).toBeNull();
  });

  it("creates a pipeline when none are stored", async () => {
    // The trap this closes: delete your only pipeline, or have it stop
    // parsing, and the section showed "No stored pipelines" with no way
    // to make another. Guided Setup does not come back, so that was permanent.
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[]}
        initialProviderDefinitions={[
          providerDefinitionFixture({
            id: "openai",
            label: "OpenAI",
            kind: "llm",
            component: "openai.responses",
            config: {},
          }),
        ]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
      />,
    );

    await enterPipelinesSection(user);
    expect(screen.getByText("No stored pipelines")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Add pipeline" }));
    await user.clear(screen.getByLabelText("New pipeline name"));
    await user.type(screen.getByLabelText("New pipeline name"), "kitchen");
    await user.click(screen.getByRole("button", { name: "Create pipeline" }));

    expect(savedGraphs).toHaveLength(1);
    expect(savedGraphs[0]?.name).toBe("kitchen");
    // Built from a configured provider, so it is a pipeline that can validate
    // rather than one naming a model nobody registered.
    expect(
      savedGraphs[0]?.nodes.some(
        (node) => node.kind === "core" && node.core.model.provider === "openai",
      ),
    ).toBe(true);
  });

  it("adds a second pipeline beside the first", async () => {
    // Guided Setup only runs on first launch, so without this an operator
    // with one pipeline could never make another.
    const user = userEvent.setup();
    const savedGraphs: PipelineGraph[] = [];
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialPipelineViews={[pipelineView()]}
        onPipelineSaved={(graph) => savedGraphs.push(graph)}
      />,
    );

    await enterPipelinesSection(user);
    await user.click(screen.getByRole("button", { name: "Add pipeline" }));

    // Named at creation, because the graph editor has no rename: a pipeline
    // stored under a generated name would keep it.
    const name = screen.getByLabelText("New pipeline name");
    expect(name).toHaveValue("kitchen-2");
    await user.clear(name);
    await user.type(name, "bedroom");
    await user.click(screen.getByRole("button", { name: "Create pipeline" }));

    // Copied from the one on screen, because a second pipeline is usually a
    // variant of the first and its providers already exist.
    expect(savedGraphs).toHaveLength(1);
    expect(savedGraphs[0]?.name).toBe("bedroom");
    expect(savedGraphs[0]?.nodes).toEqual(pipelineView().graph.nodes);

    // And it is selectable, so the operator lands on the thing they made.
    expect(screen.getByRole("button", { name: "bedroom" })).toBeInTheDocument();
  });
});

describe("Providers workspace", () => {
  it("builds a wake word definition with the field types the server expects", async () => {
    // The schema grew three shapes the form could not render: a closed set, a
    // list, and a number. Typing them all into text boxes produced a
    // definition the API refuses — `threshold_percent: "70"` is not a number —
    // and the refusal arrived long after the operator filled the form in.
    const user = userEvent.setup();
    const saved: ProviderDefinition[] = [];
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        onProviderDefinitionSaved={(definition) => saved.push(definition)}
      />,
    );

    await enterProvidersSection(user);
    await user.click(screen.getByRole("button", { name: "Add provider" }));
    await user.click(screen.getByRole("menuitem", { name: "Wake word" }));
    await user.click(screen.getByRole("menuitem", { name: "openWakeWord" }));
    await user.clear(screen.getByLabelText("Provider id"));
    await user.type(screen.getByLabelText("Provider id"), "openwakeword");
    await user.clear(screen.getByLabelText("Provider label"));
    await user.type(screen.getByLabelText("Provider label"), "openWakeWord");
    await user.selectOptions(screen.getByLabelText("Where"), "wyoming");
    await user.type(
      screen.getByLabelText("URL"),
      "tcp://openwakeword.local:10400",
    );
    await user.type(
      screen.getByLabelText("Phrases (comma separated)"),
      "hey jarvis, okay nabu",
    );
    await user.type(screen.getByLabelText("Threshold Percent"), "70");
    await user.click(screen.getByRole("button", { name: "Save provider" }));

    expect(screen.getByText("Provider openwakeword saved")).toBeInTheDocument();
    expect(saved[0]?.variant).toMatchObject({
      type: "wake",
      variant: {
        type: "openwakeword",
        runtime: {
          where: "wyoming",
          url: "tcp://openwakeword.local:10400",
          threshold_percent: 70,
        },
        phrases: ["hey jarvis", "okay nabu"],
      },
    });
  });

  it("names config fields the way a person reads them and enforces the required ones", async () => {
    // The wire spelling belongs on the wire. A form that showed `base_url` and
    // the word "required" beside it was asking the operator to translate, and
    // it left the control itself saying nothing about having to be answered.
    const user = userEvent.setup();
    render(<App initialComponentCatalog={componentCatalog()} />);

    await enterProvidersSection(user);
    await user.click(screen.getByRole("button", { name: "Add provider" }));
    await user.click(screen.getByRole("menuitem", { name: "Language model" }));
    await user.click(
      screen.getByRole("menuitem", { name: "OpenAI Responses" }),
    );

    const baseUrl = screen.getByLabelText("Base URL");
    expect(baseUrl).toBeRequired();
    expect(baseUrl).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByLabelText("Model")).toBeRequired();
    // Optional fields read the same way and are not enforced.
    expect(screen.getByLabelText("API Key")).not.toBeRequired();
    expect(screen.getByLabelText("Streaming")).not.toBeRequired();
    expect(screen.queryByText(/base_url/)).not.toBeInTheDocument();

    expect(
      screen.getByText("Missing required fields: Base URL, Model"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Save provider" }),
    ).toBeDisabled();

    await user.type(baseUrl, "https://api.openai.com/v1");
    await user.type(screen.getByLabelText("Model"), "gpt-5");

    expect(baseUrl).not.toHaveAttribute("aria-invalid");
    expect(
      screen.queryByText(/Missing required fields/),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save provider" })).toBeEnabled();
  });

  it("builds a speech cleanup definition from the rules it was given", async () => {
    // The rules are a closed set held several at a time, so the field is a
    // list with the set as suggestions rather than a menu — a menu picks one,
    // and the order the operator wrote is what decides whether an emoji inside
    // a link is seen as text.
    const user = userEvent.setup();
    const saved: ProviderDefinition[] = [];
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        onProviderDefinitionSaved={(definition) => saved.push(definition)}
      />,
    );

    await enterProvidersSection(user);
    await user.click(screen.getByRole("button", { name: "Add provider" }));
    await user.click(screen.getByRole("menuitem", { name: "Transform" }));
    await user.click(screen.getByRole("menuitem", { name: "Speech cleanup" }));
    await user.clear(screen.getByLabelText("Provider id"));
    await user.type(screen.getByLabelText("Provider id"), "speech-cleanup");
    await user.type(
      screen.getByLabelText("Rules (comma separated)"),
      "markdown_to_speech, strip_emoji",
    );
    await user.click(screen.getByRole("button", { name: "Save provider" }));

    expect(saved[0]?.variant).toMatchObject({
      type: "transform",
      variant: {
        type: "builtin",
        rules: ["markdown_to_speech", "strip_emoji"],
      },
    });
  });

  it("keeps a satellite off the engines it is too small to run", async () => {
    // The engine used to be a field beside the place, so a definition could say
    // openWakeWord on a satellite and only find out at the server. Now each
    // engine is its own component, offering only the places it runs.
    const user = userEvent.setup();
    const saved: ProviderDefinition[] = [];
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        onProviderDefinitionSaved={(definition) => saved.push(definition)}
      />,
    );

    await enterProvidersSection(user);
    await user.click(screen.getByRole("button", { name: "Add provider" }));
    await user.click(screen.getByRole("menuitem", { name: "Wake word" }));
    await user.click(screen.getByRole("menuitem", { name: "microWakeWord" }));

    const places = screen.getByLabelText("Where");
    expect(
      within(places)
        .queryAllByRole("option")
        .map((option) => option.textContent),
    ).toEqual(["choose one", "device", "wyoming"]);

    await user.clear(screen.getByLabelText("Provider id"));
    await user.type(screen.getByLabelText("Provider id"), "satellite");
    await user.selectOptions(places, "device");
    await user.type(
      screen.getByLabelText("Phrases (comma separated)"),
      "okay nabu",
    );
    await user.click(screen.getByRole("button", { name: "Save provider" }));

    expect(saved[0]?.variant).toMatchObject({
      type: "wake",
      variant: {
        type: "microwakeword",
        runtime: { where: "device" },
        phrases: ["okay nabu"],
      },
    });
  });

  it("offers the phrases a saved detector reports having models for", async () => {
    // A detector scoring models in this process knows exactly which phrases it
    // loaded. Suggestions rather than a menu: the field holds several values,
    // and a detector that enumerates some has not forbidden the rest.
    const user = userEvent.setup();
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        onProviderPhrases={async () => ["hey jarvis", "alexa"]}
      />,
    );

    await enterProvidersSection(user);
    await user.click(screen.getByRole("button", { name: "Add provider" }));
    await user.click(screen.getByRole("menuitem", { name: "Wake word" }));
    await user.click(screen.getByRole("menuitem", { name: "openWakeWord" }));
    await user.clear(screen.getByLabelText("Provider id"));
    await user.type(screen.getByLabelText("Provider id"), "openwakeword");
    await user.clear(screen.getByLabelText("Provider label"));
    await user.type(screen.getByLabelText("Provider label"), "openWakeWord");
    await user.selectOptions(screen.getByLabelText("Where"), "local");
    await user.click(screen.getByRole("button", { name: "Save provider" }));

    // Nothing to ask until a detector is registered, so the suggestions only
    // arrive once the definition has been saved and is being edited again.
    const card = screen.getByRole("row", { name: /openWakeWord/ });
    await user.click(
      within(card).getByRole("button", {
        name: "Edit openwakeword",
      }),
    );

    const field = await screen.findByLabelText("Phrases (comma separated)");
    const list = field.getAttribute("list");
    expect(list).toBeTruthy();
    const options = Array.from(
      document.getElementById(list as string)?.querySelectorAll("option") ?? [],
    ).map((option) => option.getAttribute("value"));
    expect(options).toEqual(["hey jarvis", "alexa"]);
  });

  it("creates and edits schema-backed provider instances from provider cards", async () => {
    const user = userEvent.setup();
    render(<App initialComponentCatalog={componentCatalog()} />);

    await enterProvidersSection(user);
    await user.click(screen.getByRole("button", { name: "Add provider" }));
    await user.click(screen.getByRole("menuitem", { name: "Language model" }));
    await user.click(
      screen.getByRole("menuitem", { name: "OpenAI Responses" }),
    );
    await user.clear(screen.getByLabelText("Provider id"));
    await user.type(screen.getByLabelText("Provider id"), "openai-primary");
    await user.clear(screen.getByLabelText("Provider label"));
    await user.type(screen.getByLabelText("Provider label"), "OpenAI Primary");
    await user.type(
      screen.getByLabelText("Base URL"),
      "https://api.openai.com/v1",
    );
    await user.type(screen.getByLabelText("Model"), "gpt.5");
    await user.click(screen.getByRole("button", { name: "Save provider" }));

    expect(
      screen.getByText("Provider openai-primary saved"),
    ).toBeInTheDocument();
    const providerRow = screen.getByRole("row", { name: /OpenAI Primary/ });
    expect(providerRow).toHaveTextContent("openai-primary");
    expect(providerRow).toHaveTextContent("OpenAI Responses");

    await user.click(
      within(providerRow).getByRole("button", {
        name: "Edit openai-primary",
      }),
    );
    expect(
      screen.getByRole("button", { name: "Cancel provider edit" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Provider id")).toHaveDisplayValue(
      "openai-primary",
    );
    expect(screen.getByLabelText("Provider label")).toHaveDisplayValue(
      "OpenAI Primary",
    );
    expect(screen.getByLabelText("Provider component")).toHaveDisplayValue(
      "OpenAI Responses",
    );
    expect(screen.getByLabelText("Base URL")).toHaveDisplayValue(
      "https://api.openai.com/v1",
    );
    expect(screen.getByLabelText("Model")).toHaveDisplayValue("gpt.5");
    await user.click(
      screen.getByRole("button", { name: "Cancel provider edit" }),
    );
    expect(
      screen.getByRole("row", { name: /OpenAI Primary/ }),
    ).toBeInTheDocument();

    const restoredProviderRow = screen.getByRole("row", {
      name: /OpenAI Primary/,
    });
    await user.click(
      within(restoredProviderRow).getByRole("button", {
        name: "Edit openai-primary",
      }),
    );
    await user.clear(screen.getByLabelText("Provider label"));
    await user.type(screen.getByLabelText("Provider label"), "OpenAI Main");
    await user.click(screen.getByRole("button", { name: "Save provider" }));

    expect(
      screen.getByRole("row", { name: /OpenAI Main/ }),
    ).toBeInTheDocument();
    expect(screen.queryByText("OpenAI Primary")).not.toBeInTheDocument();
  });

  it("starts a new provider card from a kind menu and closes the active editor", async () => {
    const user = userEvent.setup();
    render(<App initialComponentCatalog={componentCatalog()} />);

    await enterProvidersSection(user);
    await user.click(screen.getByRole("button", { name: "Edit openai" }));
    expect(screen.getAllByDisplayValue("openai").length).toBeGreaterThan(0);

    await user.click(screen.getByRole("button", { name: "Add provider" }));
    await user.click(screen.getByRole("menuitem", { name: "Language model" }));
    expect(screen.queryByRole("menuitem", { name: "All kinds" })).toBeNull();
    expect(
      screen.getByRole("menuitem", { name: "← Provider types" }),
    ).toBeInTheDocument();
    await user.click(
      screen.getByRole("menuitem", { name: "OpenAI Responses" }),
    );

    expect(
      screen.getByRole("heading", { name: "Configure provider" }),
    ).toBeInTheDocument();
    expect(screen.getAllByText("OpenAI Responses")).toHaveLength(1);
    expect(screen.queryByDisplayValue("openai")).not.toBeInTheDocument();
    expect(screen.getByDisplayValue("llm-4")).toBeInTheDocument();
    expect(screen.getByLabelText("Provider label")).toHaveDisplayValue(
      "OpenAI Responses",
    );

    await user.click(
      screen.getByRole("button", { name: "Cancel provider edit" }),
    );
    expect(screen.queryByDisplayValue("llm-4")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "OpenAI Responses" }),
    ).not.toBeInTheDocument();
  });

  it("deletes configured provider cards", async () => {
    const user = userEvent.setup();
    const providerDefinitions = [
      providerDefinitionFixture({
        id: "openai",
        label: "openai",
        component: "openai.responses",
        config: {
          base_url: "https://api.openai.com/v1",
          model: "gpt-5",
        },
      }),
    ];
    mockOperatorApi({ providerDefinitions });
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialProviderDefinitions={providerDefinitions}
      />,
    );

    await enterProvidersSection(user);
    await user.click(screen.getByRole("button", { name: "Delete openai" }));

    expect(screen.getByText("Provider openai deleted")).toBeInTheDocument();
    const openAiRow = screen.getByRole("row", { name: /openai/ });
    expect(
      within(openAiRow).queryByRole("button", {
        name: "Delete openai",
      }),
    ).not.toBeInTheDocument();
  });

  it("renders provider status from the snapshot grouped by stage", async () => {
    const user = userEvent.setup();
    render(<App />);

    await enterProvidersSection(user);

    expect(
      screen.getByRole("heading", { name: "Providers" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("row", { name: /piper-local/ }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("no successful reachability check yet"),
    ).toBeInTheDocument();
    expect(screen.getByText("Used in graphs")).toBeInTheDocument();

    // Providers sit under their stage rather than behind a stage filter, so a
    // TTS provider is found in the Text-to-speech section.
    const ttsSection = screen.getByRole("region", {
      name: "Text-to-speech providers",
    });
    expect(
      within(ttsSection).getByRole("row", { name: /piper-local/ }),
    ).toBeInTheDocument();
  });

  it("filters providers by search and narrows to issues only", async () => {
    const user = userEvent.setup();
    render(<App />);

    await enterProvidersSection(user);

    await user.type(screen.getByLabelText("Filter providers"), "piper");
    expect(
      screen.getByRole("row", { name: /piper-local/ }),
    ).toBeInTheDocument();
    expect(screen.queryByText("openai")).not.toBeInTheDocument();
    expect(screen.queryByText("whisper")).not.toBeInTheDocument();

    await user.clear(screen.getByLabelText("Filter providers"));
    await user.click(screen.getByLabelText("Show issues only"));
    expect(
      screen.getByRole("row", { name: /piper-local/ }),
    ).toBeInTheDocument();
    expect(screen.queryByText("openai")).not.toBeInTheDocument();
  });

  it("refreshes provider status from the API when testing a provider", async () => {
    const user = userEvent.setup();
    const refreshed = snapshotFixture();
    refreshed.providers = refreshed.providers.map((provider) =>
      provider.id === "piper-local"
        ? {
            ...provider,
            state: "reachable",
            reachable: true,
            message: "provider health check passed",
          }
        : provider,
    );
    mockOperatorApi({ statusSnapshots: [snapshotFixture(), refreshed] });
    render(<App />);

    await enterProvidersSection(user);
    await user.click(screen.getByRole("button", { name: "Test piper-local" }));

    expect(
      await screen.findByText("Provider piper-local is reachable"),
    ).toBeInTheDocument();
    const piperRow = screen.getByRole("row", { name: /piper-local/ });
    expect(piperRow).toHaveClass("healthy");
    expect(
      within(piperRow).getByText("provider health check passed"),
    ).toBeInTheDocument();
    expect(within(piperRow).getByText("reachable")).toBeInTheDocument();

    expect(
      within(piperRow).queryByRole("button", { name: "Use fallback" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByText("Fallback selected for piper-local"),
    ).not.toBeInTheDocument();
    expect(
      within(piperRow).getByRole("button", { name: "Test piper-local" }),
    ).toHaveTextContent("Test");
  });

  it("blocks deleting provider definitions that are still referenced by pipelines", async () => {
    const user = userEvent.setup();
    mockOperatorApi({
      providerDefinitions: [
        providerDefinitionFixture({
          id: "piper-local",
          label: "piper-local",
          component: "wyoming.tts",
          config: {
            url: "tcp://piper.local:10200",
            voice: "en_US-lessac-medium",
          },
        }),
      ],
    });
    render(<App />);

    await enterProvidersSection(user);
    const piperRow = screen.getByRole("row", { name: /piper-local/ });

    await user.click(
      within(piperRow).getByRole("button", {
        name: "Delete piper-local",
      }),
    );

    expect(
      screen.getByText(
        "Provider piper-local is used by pipeline kitchen; remove it from those pipeline graphs before deleting it.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("row", { name: /piper-local/ }),
    ).toBeInTheDocument();
    expect(
      within(piperRow).getByRole("button", {
        name: "Delete piper-local",
      }),
    ).toBeInTheDocument();
  });

  it("does not offer fake deletion for providers inferred from status or graphs", async () => {
    const user = userEvent.setup();
    render(<App />);

    await enterProvidersSection(user);
    const piperRow = screen.getByRole("row", { name: /piper-local/ });
    expect(
      within(piperRow).queryByRole("button", {
        name: "Delete piper-local",
      }),
    ).not.toBeInTheDocument();
  });

  it("deletes unreferenced provider definitions even when runtime status exists", async () => {
    const user = userEvent.setup();
    const snapshot = snapshotFixture();
    snapshot.providers = [
      {
        id: "llm",
        kind: "llm",
        state: "reachable",
        configured: true,
        reachable: true,
        proven_by_turn: null,
        message: "provider health check passed",
        affects_pipelines: [],
      },
    ];
    const providerDefinitions = [
      providerDefinitionFixture({
        id: "llm",
        label: "llm",
        component: "openai.responses",
        config: {
          base_url: "https://api.openai.com/v1",
          model: "gpt-5",
        },
      }),
    ];
    mockOperatorApi({ snapshot, providerDefinitions });
    render(<App />);

    await enterProvidersSection(user);
    const llmRow = screen.getByRole("row", { name: /llm/ });

    await user.click(
      within(llmRow).getByRole("button", {
        name: "Delete llm",
      }),
    );

    expect(screen.getByText("Provider llm deleted")).toBeInTheDocument();
    expect(screen.getByRole("row", { name: /llm/ })).toBeInTheDocument();
    expect(
      within(llmRow).queryByRole("button", {
        name: "Delete llm",
      }),
    ).not.toBeInTheDocument();
  });

  it("tests configured providers through the backend reachability endpoint", async () => {
    const user = userEvent.setup();
    const providerDefinitions = [
      providerDefinitionFixture({
        id: "openai-fast",
        label: "OpenAI Fast",
        component: "openai.responses",
        config: {
          base_url: "https://api.openai.com/v1",
          model: "gpt-5",
          streaming: true,
        },
      }),
    ];
    mockOperatorApi({ providerDefinitions });
    render(
      <App
        initialComponentCatalog={componentCatalog()}
        initialProviderDefinitions={providerDefinitions}
      />,
    );

    await enterProvidersSection(user);
    await user.click(screen.getByRole("button", { name: "Test openai-fast" }));

    expect(
      await screen.findByText("Provider openai-fast is reachable"),
    ).toBeInTheDocument();
  });
});

describe("Settings workspace", () => {
  it("persists saved operator settings across reloads", async () => {
    const user = userEvent.setup();
    const firstLoad = render(<App initialPipelineViews={[pipelineView()]} />);

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

    firstLoad.unmount();
    render(<App initialPipelineViews={[pipelineView()]} />);

    await user.click(screen.getByRole("tab", { name: "Settings" }));

    expect(screen.getByLabelText("Deployment name")).toHaveValue("clinic-prod");
    expect(screen.getByLabelText("Local-only mode")).not.toBeChecked();
    expect(screen.getByRole("button", { name: "90 d" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByLabelText("Log level")).toHaveValue("debug");
  });

  it("requires explicit confirmation before resetting local console state", async () => {
    const user = userEvent.setup();
    render(<App initialPipelineViews={[pipelineView()]} />);

    await enterSettingsSection(user);
    await user.clear(screen.getByLabelText("Deployment name"));
    await user.type(screen.getByLabelText("Deployment name"), "clinic-prod");
    await user.click(screen.getByLabelText("Local-only mode"));
    await user.click(screen.getByRole("button", { name: "90 d" }));
    await user.selectOptions(screen.getByLabelText("Log level"), "debug");
    await user.click(screen.getByRole("button", { name: "Save settings" }));

    await user.click(screen.getByRole("button", { name: "Reset local state" }));

    expect(
      screen.getByText("Type RESET to permanently clear saved UI settings."),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Confirm reset" }));

    expect(screen.getByText("Type RESET to confirm")).toBeInTheDocument();

    await user.type(screen.getByLabelText("Reset confirmation"), "RESET");
    await user.click(screen.getByRole("button", { name: "Confirm reset" }));

    expect(screen.getByText("Local console state reset")).toBeInTheDocument();
    expect(screen.getByLabelText("Deployment name")).toHaveValue(
      "conduit-local",
    );
    expect(screen.getByLabelText("Local-only mode")).toBeChecked();
    expect(screen.getByRole("button", { name: "30 d" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByLabelText("Log level")).toHaveValue("info");
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

describe("Speakers workspace", () => {
  /// Somebody named but never recorded, which is the state every speaker
  /// starts in.
  function named(id: string, name: string): EnrolledSpeaker {
    return { id, name, samples: 0, created_at: "2025-01-01T00:00:00Z" };
  }

  it("says plainly that an empty roster identifies nobody", async () => {
    // The stage exists whether or not anyone is enrolled, and a pipeline that
    // identifies nobody reaches every tool's permission check with no
    // speaker. An empty page that did not say so would look like it worked.
    const user = userEvent.setup();
    mockOperatorApi({ speakers: [] });
    render(<App />);

    await enterSpeakersSection(user);

    expect(
      await screen.findByText(/Nobody is enrolled yet/),
    ).toBeInTheDocument();
  });

  it("names somebody before anyone has recorded them", async () => {
    const user = userEvent.setup();
    const api = mockOperatorApi({ speakers: [] });
    render(<App />);

    await enterSpeakersSection(user);
    await user.type(await screen.findByLabelText("Name"), "Ada");
    await user.click(screen.getByRole("button", { name: "Add speaker" }));

    expect(await screen.findByText("Ada")).toBeInTheDocument();
    expect(screen.getByText("no voice yet")).toBeInTheDocument();
    expect([...api.roster.values()].map((speaker) => speaker.name)).toEqual([
      "Ada",
    ]);
  });

  it("sends an uploaded sample as a file and shows what came back", async () => {
    const user = userEvent.setup();
    const api = mockOperatorApi({
      speakers: [named("00000000-0000-4000-8000-000000000001", "Ada")],
    });
    render(<App />);

    await enterSpeakersSection(user);
    const upload = await screen.findByLabelText("Upload a sample for Ada");
    const file = new File([new Uint8Array([1, 2, 3])], "ada.wav", {
      type: "audio/wav",
    });
    await user.upload(upload.querySelector("input") as HTMLInputElement, file);

    expect(
      await screen.findByText("Sample accepted — 1 on file"),
    ).toBeInTheDocument();
    expect(screen.getByText("1 on file")).toBeInTheDocument();
    expect(screen.getByText("voices")).toBeInTheDocument();
    expect(api.enrollments).toHaveLength(1);
    expect(api.enrollments[0].body).toBe(file);
  });

  it("reports what the identification service said when it refused", async () => {
    // The service's own words are the only thing that tells an operator
    // whether to record again or go fix a deployment.
    const user = userEvent.setup();
    mockOperatorApi({
      speakers: [named("00000000-0000-4000-8000-000000000001", "Ada")],
      enrollmentFails: "the audio is too short to build a voice print",
    });
    render(<App />);

    await enterSpeakersSection(user);
    const upload = await screen.findByLabelText("Upload a sample for Ada");
    await user.upload(
      upload.querySelector("input") as HTMLInputElement,
      new File([new Uint8Array([1])], "short.wav", { type: "audio/wav" }),
    );

    expect(
      await screen.findByText("the audio is too short to build a voice print"),
    ).toBeInTheDocument();
    expect(screen.getByText("no voice yet")).toBeInTheDocument();
  });

  it("renames somebody without disturbing the voice behind the name", async () => {
    const user = userEvent.setup();
    const api = mockOperatorApi({
      speakers: [
        {
          ...named("00000000-0000-4000-8000-000000000001", "Ada"),
          samples: 2,
          provider: "voices",
        },
      ],
    });
    render(<App />);

    await enterSpeakersSection(user);
    await user.click(await screen.findByLabelText("Rename Ada"));
    const field = screen.getByLabelText("New name for Ada");
    await user.clear(field);
    await user.type(field, "Ada Lovelace{Enter}");

    expect(await screen.findByText("Ada Lovelace")).toBeInTheDocument();
    expect(screen.getByText("2 on file")).toBeInTheDocument();
    expect(api.roster.get("00000000-0000-4000-8000-000000000001")?.name).toBe(
      "Ada Lovelace",
    );
  });

  it("removes somebody from the roster", async () => {
    const user = userEvent.setup();
    const api = mockOperatorApi({
      speakers: [named("00000000-0000-4000-8000-000000000001", "Ada")],
    });
    render(<App />);

    await enterSpeakersSection(user);
    await user.click(await screen.findByLabelText("Remove Ada"));

    expect(
      await screen.findByText(/Nobody is enrolled yet/),
    ).toBeInTheDocument();
    expect(api.roster.size).toBe(0);
  });

  it("offers to record when the browser will give the page a microphone", async () => {
    // The button is the whole point of the page for a household that has no
    // WAV files lying about.
    const user = userEvent.setup();
    mockOperatorApi({
      speakers: [named("00000000-0000-4000-8000-000000000001", "Ada")],
    });
    render(<App />);

    await enterSpeakersSection(user);

    expect(
      await screen.findByLabelText("Record a sample for Ada"),
    ).toBeEnabled();
  });

  it("says so when the browser will not give the page a microphone", async () => {
    // jsdom has no `getUserMedia`, which is exactly the case a locked-down
    // browser presents: the operator needs to be told to upload instead.
    const user = userEvent.setup();
    mockOperatorApi({
      speakers: [named("00000000-0000-4000-8000-000000000001", "Ada")],
    });
    render(<App />);

    await enterSpeakersSection(user);
    await user.click(await screen.findByLabelText("Record a sample for Ada"));

    expect(
      await screen.findByText(/upload a WAV file instead/),
    ).toBeInTheDocument();
  });
});

async function enterProvidersSection(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "Use anonymous mode" }));
  await user.click(screen.getByRole("tab", { name: "Providers" }));
}

async function enterSpeakersSection(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "Use anonymous mode" }));
  await user.click(screen.getByRole("tab", { name: "Speakers" }));
}

async function enterSettingsSection(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "Use anonymous mode" }));
  await user.click(screen.getByRole("tab", { name: "Settings" }));
}

/// The model a graph's core binds, for the status fixtures.
function coreProvider(graph: PipelineGraph): string | null {
  const core = graph.nodes.find((node) => node.kind === "core");
  return core?.kind === "core" ? core.core.model.provider : null;
}

function pipelineView(): PipelineView {
  const graph: PipelineGraph = {
    name: "kitchen",
    nodes: [
      { id: "mic", kind: "source", provider: "websocket" },
      { id: "stt", kind: "stt", provider: "whisper" },
      {
        id: "llm",
        kind: "core",
        core: { model: { provider: "openai" }, max_rounds: 4 },
      },
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

function componentCatalog(): ProviderComponentCatalog {
  return {
    components: [
      {
        id: "openai.responses",
        label: "OpenAI Responses",
        kind: "llm",
        definition_variant: "openai",
        schema: {
          properties: {
            base_url: { type: "string", format: "url" },
            api_key: { type: "string" },
            model: { type: "string", pattern: "[a-z0-9.]+" },
            streaming: { type: "boolean" },
          },
          required: ["base_url", "model"],
        },
      },
      {
        id: "openai.completions",
        label: "OpenAI Completions",
        kind: "llm",
        definition_variant: "openai",
        schema: {
          properties: {
            base_url: { type: "string", format: "url" },
            api_key: { type: "string" },
            model: { type: "string", pattern: "[a-z0-9.]+" },
            streaming: { type: "boolean" },
          },
          required: ["base_url", "model"],
        },
      },
      {
        id: "openwakeword",
        label: "openWakeWord",
        kind: "wake",
        definition_variant: "openwakeword",
        schema: {
          properties: {
            where: { type: "string", options: ["local", "wyoming"] },
            url: { type: "string", format: "url" },
            models_dir: { type: "string" },
            phrases: { type: "string_list" },
            threshold_percent: { type: "integer" },
          },
          required: ["where"],
        },
      },
      {
        id: "microwakeword",
        label: "microWakeWord",
        kind: "wake",
        definition_variant: "microwakeword",
        schema: {
          properties: {
            where: { type: "string", options: ["device", "wyoming"] },
            url: { type: "string", format: "url" },
            phrases: { type: "string_list" },
            threshold_percent: { type: "integer" },
          },
          required: ["where"],
        },
      },
      {
        id: "wyoming",
        label: "Wyoming",
        kind: "stt",
        definition_variant: "wyoming",
        schema: {
          properties: {
            url: { type: "string", format: "url" },
            model: { type: "string" },
            streaming: { type: "boolean" },
          },
          required: ["url"],
        },
      },
      {
        id: "openai.transcription",
        label: "OpenAI Transcription",
        kind: "stt",
        definition_variant: "openai",
        schema: {
          properties: {
            base_url: { type: "string", format: "url" },
            model: { type: "string" },
            stream: { type: "boolean" },
          },
          required: ["model"],
        },
      },
      {
        id: "openai.speech",
        label: "OpenAI Speech",
        kind: "tts",
        definition_variant: "openai",
        schema: {
          properties: {
            base_url: { type: "string", format: "url" },
            model: { type: "string" },
          },
          required: ["model"],
        },
      },
      {
        id: "wyoming.tts",
        label: "Wyoming TTS",
        kind: "tts",
        definition_variant: "wyoming",
        schema: {
          properties: {
            url: { type: "string", format: "url" },
            voice: { type: "string" },
            model: { type: "string" },
            mode: { type: "string" },
            streaming: { type: "boolean" },
          },
          required: ["url"],
        },
      },
      {
        id: "transform.builtin",
        label: "Speech cleanup",
        kind: "transform",
        definition_variant: "builtin",
        schema: {
          properties: {
            rules: {
              type: "string_list",
              options: [
                "markdown_to_speech",
                "strip_emoji",
                "collapse_whitespace",
              ],
            },
          },
          required: ["rules"],
        },
      },
      {
        id: "mcp.sse",
        label: "MCP SSE",
        kind: "tool",
        definition_variant: "mcp",
        schema: {
          properties: {
            url: { type: "string", format: "url" },
          },
          required: ["url"],
        },
      },
      {
        id: "mcp.streamable_http",
        label: "MCP Streamable HTTP",
        kind: "tool",
        definition_variant: "mcp",
        schema: {
          properties: {
            url: { type: "string", format: "url" },
          },
          required: ["url"],
        },
      },
      {
        id: "mcp.stdio",
        label: "MCP STDIO",
        kind: "tool",
        definition_variant: "mcp",
        schema: {
          properties: {
            command: { type: "string" },
          },
          required: ["command"],
        },
      },
    ],
  };
}

function liveApiPipelineView(): PipelineView {
  const graph: PipelineGraph = {
    name: "garage",
    nodes: [
      { id: "garage_mic", kind: "source", provider: "websocket" },
      { id: "garage_stt", kind: "stt", provider: "garage-whisper" },
      {
        id: "garage_llm",
        kind: "core",
        core: { model: { provider: "garage-openai" }, max_rounds: 4 },
      },
      { id: "garage_tts", kind: "tts", provider: "garage-tts" },
      { id: "garage_speaker", kind: "sink", provider: "websocket" },
    ],
    edges: [
      { from: "garage_mic", to: "garage_stt" },
      { from: "garage_stt", to: "garage_llm" },
      { from: "garage_llm", to: "garage_tts" },
      { from: "garage_tts", to: "garage_speaker" },
    ],
  };

  return {
    graph,
    order: graph.nodes.map((node) => node.id),
  };
}

function liveApiSnapshot(): OperatorStatusSnapshot {
  const snapshot = healthySnapshot();
  snapshot.generated_at = "2026-08-01T03:00:00Z";
  snapshot.pipelines = snapshot.pipelines.map((pipeline) => ({
    ...pipeline,
    name: "garage",
    affected_providers: ["garage-tts"],
    components: pipeline.components.map((component) =>
      component.kind === "synthesis"
        ? { ...component, provider: "garage-tts" }
        : component,
    ),
  }));
  snapshot.providers = [
    {
      id: "garage-tts",
      kind: "tts",
      state: "reachable",
      configured: true,
      reachable: true,
      proven_by_turn: null,
      message: "garage endpoint responded",
      affects_pipelines: ["garage"],
    },
  ];
  snapshot.satellites = {
    connected: [
      {
        device: "00000000-0000-0000-0000-000000000201",
        name: "Garage Satellite",
        connected_since: "2026-08-01T02:59:00Z",
        conversation: "00000000-0000-0000-0000-000000000202",
        pipeline: "garage",
      },
    ],
    recently_active: [
      {
        device: "00000000-0000-0000-0000-000000000201",
        name: "Garage Satellite",
        last_seen_at: "2026-08-01T02:59:30Z",
        last_event: "AudioStarted",
      },
    ],
    recent_window_seconds: 300,
  };
  return snapshot;
}

function mockOperatorApi({
  snapshot = snapshotFixture(),
  statusSnapshots,
  pipelineViews = [pipelineView()],
  componentCatalog: catalog = componentCatalog(),
  providerDefinitions = [],
  speakers = [],
  enrollmentFails,
  updateSnapshotOnPipelineSave = true,
}: {
  snapshot?: OperatorStatusSnapshot;
  statusSnapshots?: OperatorStatusSnapshot[];
  pipelineViews?: PipelineView[];
  componentCatalog?: ProviderComponentCatalog;
  providerDefinitions?: ProviderDefinitionView[];
  speakers?: EnrolledSpeaker[];
  /// What the identification service says when it refuses a sample, if it
  /// does. Refusal is the case the console has to report faithfully.
  enrollmentFails?: string;
  updateSnapshotOnPipelineSave?: boolean;
} = {}) {
  let currentSnapshot = snapshot;
  const pendingStatusSnapshots = [...(statusSnapshots ?? [])];
  const pipelines = new Map(
    pipelineViews.map((view) => [view.graph.name, view] as const),
  );
  const savedProviderDefinitions = new Map(
    providerDefinitions.map(
      (definition) => [definition.id, definition] as const,
    ),
  );
  const roster = new Map(speakers.map((speaker) => [speaker.id, speaker]));
  /// Every enrollment body the console sent, so a test can assert it was a
  /// WAV file rather than whatever the browser felt like producing.
  const enrollments: { id: string; body: BodyInit | null | undefined }[] = [];
  const fetchMock = vi.fn(
    async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = input instanceof URL ? input : new URL(input.toString());
      const route = decodeURIComponent(url.pathname);
      const method = init?.method ?? "GET";

      if (route === "/v1/status" && method === "GET") {
        currentSnapshot = pendingStatusSnapshots.shift() ?? currentSnapshot;
        return jsonResponse(currentSnapshot);
      }

      if (route === "/v1/speakers" && method === "GET") {
        return jsonResponse([...roster.values()]);
      }

      if (route === "/v1/speakers" && method === "POST") {
        const { name } = JSON.parse(String(init?.body)) as { name: string };
        const created: EnrolledSpeaker = {
          id: `00000000-0000-4000-8000-00000000000${roster.size + 1}`,
          name,
          samples: 0,
          created_at: "2025-01-01T00:00:00Z",
        };
        roster.set(created.id, created);
        return jsonResponse(created, { status: 201 });
      }

      if (route.startsWith("/v1/speakers/")) {
        const rest = route.slice("/v1/speakers/".length);
        const id = rest.replace(/\/enroll$/, "");
        const speaker = roster.get(id);
        if (!speaker) {
          return jsonResponse(
            { error: "not_found", detail: `no speaker ${id}` },
            { status: 404 },
          );
        }

        if (rest.endsWith("/enroll") && method === "POST") {
          enrollments.push({ id, body: init?.body });
          if (enrollmentFails) {
            return jsonResponse(
              { error: "unavailable", detail: enrollmentFails },
              { status: 503 },
            );
          }
          const enrolled: EnrolledSpeaker = {
            ...speaker,
            samples: speaker.samples + 1,
            provider: "voices",
            enrolled_at: "2025-01-02T00:00:00Z",
          };
          roster.set(id, enrolled);
          return jsonResponse(enrolled);
        }

        if (method === "PUT") {
          const { name } = JSON.parse(String(init?.body)) as { name: string };
          const renamed = { ...speaker, name };
          roster.set(id, renamed);
          return jsonResponse(renamed);
        }

        if (method === "DELETE") {
          roster.delete(id);
          return new Response(null, { status: 204 });
        }

        return jsonResponse(speaker);
      }

      if (route === "/v1/pipelines" && method === "GET") {
        return jsonResponse([...pipelines.keys()]);
      }

      if (route === "/v1/catalog/providers" && method === "GET") {
        return jsonResponse(catalog);
      }

      if (route === "/v1/providers" && method === "GET") {
        return jsonResponse([...savedProviderDefinitions.keys()]);
      }

      if (route.startsWith("/v1/providers/")) {
        const id = route.slice("/v1/providers/".length);
        if (id.endsWith("/test") && method === "POST") {
          const providerId = id.replace(/\/test$/, "");
          currentSnapshot = pendingStatusSnapshots.shift() ?? currentSnapshot;
          const provider = currentSnapshot.providers.find(
            (provider) => provider.id === providerId,
          );
          if (provider) {
            return jsonResponse(provider);
          }
          const definition = savedProviderDefinitions.get(providerId);
          if (!definition) {
            return jsonResponse({ error: "not_found" }, { status: 404 });
          }
          return jsonResponse(providerStatusForDefinition(definition));
        }
        if (method === "PUT") {
          const definition = JSON.parse(
            init?.body?.toString() ?? "{}",
          ) as ProviderDefinition;
          const view: ProviderDefinitionView = {
            ...definition,
            kind: providerKindForVariant(definition.variant.type),
          };
          savedProviderDefinitions.set(id, view);
          return jsonResponse(view, { status: 201 });
        }
        if (method === "DELETE") {
          savedProviderDefinitions.delete(id);
          return new Response(null, { status: 204 });
        }

        const definition = savedProviderDefinitions.get(id);
        if (!definition) {
          return jsonResponse({ error: "not_found" }, { status: 404 });
        }
        return jsonResponse(definition);
      }

      if (route === "/v1/pipelines/validate" && method === "POST") {
        const graph = JSON.parse(
          init?.body?.toString() ?? "{}",
        ) as PipelineGraph;
        return jsonResponse({
          graph,
          order: graph.nodes.map((node) => node.id),
        });
      }

      if (route.startsWith("/v1/pipelines/")) {
        if (route.endsWith("/test-turn") && method === "POST") {
          const name = route
            .slice("/v1/pipelines/".length)
            .replace(/\/test-turn$/, "");
          const request = JSON.parse(init?.body?.toString() ?? "{}") as {
            utterance?: string;
          };
          return jsonResponse({
            pipeline: name,
            conversation: "00000000-0000-0000-0000-000000000999",
            status: "completed",
            audio_bytes: 24,
            // A minimal RIFF/WAVE header, base64-encoded, standing in for the
            // synthesized reply.
            reply_audio: btoa(
              `RIFF$\u0000\u0000\u0000WAVE${request.utterance ?? "conduit test"}`,
            ),
          });
        }

        const name = route.slice("/v1/pipelines/".length);
        if (method === "PUT") {
          const graph = JSON.parse(
            init?.body?.toString() ?? "{}",
          ) as PipelineGraph;
          const view: PipelineView = {
            graph,
            order: graph.nodes.map((node) => node.id),
          };
          pipelines.set(name, view);
          if (updateSnapshotOnPipelineSave) {
            currentSnapshot = snapshotWithStoredPipeline(
              currentSnapshot,
              graph,
            );
          }
          return jsonResponse(view);
        }

        const view = pipelines.get(name);
        if (!view) {
          return jsonResponse({ error: "not_found" }, { status: 404 });
        }
        return jsonResponse(view);
      }

      return jsonResponse({ error: "not_found" }, { status: 404 });
    },
  );

  vi.stubGlobal("fetch", fetchMock);
  return Object.assign(fetchMock, { enrollments, roster });
}

/// The outer provider definition variant is the capability itself.
function providerKindForVariant(
  variant: ProviderDefinition["variant"]["type"],
): ProviderDefinitionView["kind"] {
  return variant;
}

function providerStatusForDefinition(
  definition: ProviderDefinitionView,
): ProviderStatus {
  return {
    id: definition.id,
    kind: definition.kind,
    state: "reachable",
    configured: true,
    reachable: true,
    proven_by_turn: null,
    message: null,
    affects_pipelines: [],
  };
}

function providerDefinitionFixture({
  id,
  label,
  kind,
  component,
  config,
}: {
  id: string;
  label: string;
  kind?: ProviderDefinitionView["kind"];
  component: string;
  config: Record<string, unknown>;
}): ProviderDefinitionView {
  const text = (field: string) =>
    typeof config[field] === "string" ? config[field] : "";
  const flag = (field: string) => config[field] === true;
  const apiKey = text("api_key")
    ? ({ type: "inline", value: text("api_key") } as const)
    : undefined;

  if (component === "openai.responses" || component === "openai.completions") {
    return {
      id,
      label,
      kind: "llm",
      variant: {
        type: "llm",
        variant: {
          type: "openai",
          base_url: text("base_url"),
          ...(apiKey ? { api_key: apiKey } : {}),
          models: text("model") ? [text("model")] : [],
          streaming: flag("streaming"),
        },
      },
    };
  }
  if (
    component === "wyoming.tts" ||
    (component === "wyoming" && kind === "tts")
  ) {
    return {
      id,
      label,
      kind: "tts",
      variant: {
        type: "tts",
        variant: {
          type: "wyoming",
          url: text("url"),
          ...(text("voice") || text("model")
            ? { voice: text("voice") || text("model") }
            : {}),
          streaming: flag("streaming"),
        },
      },
    };
  }
  if (component === "mcp.sse" || component === "mcp.streamable_http") {
    return {
      id,
      label,
      kind: "tool",
      variant: {
        type: "tool",
        variant: {
          type: "mcp",
          transport: {
            type: component === "mcp.sse" ? "sse" : "streamable_http",
            url: text("url"),
          },
        },
      },
    };
  }
  return {
    id,
    label,
    kind: "stt",
    variant: {
      type: "stt",
      variant: {
        type: "wyoming",
        url: text("url"),
        ...(text("model") ? { model: text("model") } : {}),
        streaming: flag("streaming"),
      },
    },
  };
}

function snapshotWithStoredPipeline(
  snapshot: OperatorStatusSnapshot,
  graph: PipelineGraph,
): OperatorStatusSnapshot {
  return {
    ...snapshot,
    runtime: {
      ...snapshot.runtime,
      launch_state: "operations_workspace",
    },
    pipelines: [
      ...snapshot.pipelines.filter((pipeline) => pipeline.name !== graph.name),
      {
        name: graph.name,
        usable: true,
        health: {
          state: "unproven",
          summary: "awaiting first successful turn",
          last_successful_turn: null,
          last_failed_turn: null,
        },
        components: [
          {
            kind: "capture",
            provider: "websocket",
            state: "unproven",
            detail: "pipeline saved",
            last_turn: null,
          },
          {
            kind: "transcription",
            provider:
              graph.nodes.find((node) => node.kind === "stt")?.provider ?? null,
            state: "unproven",
            detail: "pipeline saved",
            last_turn: null,
          },
          {
            kind: "reasoning",
            provider: coreProvider(graph),
            state: "unproven",
            detail: "pipeline saved",
            last_turn: null,
          },
          {
            kind: "tools",
            provider: coreProvider(graph),
            state: graph.nodes.some((node) => node.kind === "core")
              ? "unproven"
              : "unused",
            detail: graph.nodes.some((node) => node.kind === "core")
              ? "pipeline saved"
              : null,
            last_turn: null,
          },
          {
            kind: "synthesis",
            provider:
              graph.nodes.find((node) => node.kind === "tts")?.provider ?? null,
            state: "unproven",
            detail: "pipeline saved",
            last_turn: null,
          },
        ],
        affected_providers: [],
      },
    ],
  };
}

function jsonResponse(body: unknown, init: ResponseInit = {}) {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
    ...init,
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

function storedButNotUsableSnapshot(): OperatorStatusSnapshot {
  const snapshot = snapshotWithStoredPipeline(
    firstRunSnapshot(),
    pipelineView().graph,
  );
  snapshot.runtime.launch_state = "first_run_setup";
  snapshot.pipelines = snapshot.pipelines.map((pipeline) => ({
    ...pipeline,
    usable: false,
    health: {
      state: "not_runnable",
      summary: "pipeline is not runnable",
      last_successful_turn: null,
      last_failed_turn: null,
    },
    components: pipeline.components.map((component) => ({
      ...component,
      state: component.kind === "tools" ? "unused" : "unproven",
    })),
  }));
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
