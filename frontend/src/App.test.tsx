import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";

import App, { OverviewPanel } from "./App";
import {
  operatorStatusSnapshotFixture,
  type OperatorStatusSnapshot,
} from "./contracts/status";
import { applySnapshotEvent, transitionEventStream } from "./eventStream";

beforeEach(() => {
  sessionStorage.clear();
  localStorage.clear();
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
});

function snapshotFixture(): OperatorStatusSnapshot {
  return JSON.parse(
    JSON.stringify(operatorStatusSnapshotFixture),
  ) as OperatorStatusSnapshot;
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
