import type { EventEnvelope, EventType } from "./contracts/events";
import type {
  ComponentKind,
  OperatorStatusSnapshot,
  StaleState,
} from "./contracts/status";
import { operatorStatusSnapshotFixture } from "./contracts/status";
import type { SnapshotState } from "./apiClient";

export type EventStreamPosture =
  "disconnected" | "connecting" | "live" | "stale";

export interface EventStreamPlan {
  route: "/v1/events";
  posture: EventStreamPosture;
  refreshSnapshotAfterReconnect: boolean;
  snapshotEventTypes: readonly EventType[];
}

export interface OverviewRuntimeState {
  readonly snapshot: OperatorStatusSnapshot | null;
  readonly eventPosture: EventStreamPosture;
  readonly snapshotState: SnapshotState;
}

export type EventStreamTransition =
  | { type: "event"; envelope: EventEnvelope }
  | { type: "disconnected" }
  | { type: "reconnected"; snapshot: OperatorStatusSnapshot };

export function initialEventStreamPlan(): EventStreamPlan {
  const snapshotEventTypes: EventType[] = Array.from(
    new Set(
      operatorStatusSnapshotFixture.event_stream.bindings.flatMap(
        (binding) => binding.events,
      ),
    ),
  );

  return {
    route: "/v1/events",
    posture: "live",
    refreshSnapshotAfterReconnect: true,
    snapshotEventTypes,
  };
}

export function transitionEventStream(
  state: OverviewRuntimeState,
  transition: EventStreamTransition,
): OverviewRuntimeState {
  if (transition.type === "disconnected") {
    return {
      ...state,
      eventPosture: "stale",
      snapshotState: "stale",
      snapshot: state.snapshot
        ? withRuntimeStaleState(state.snapshot, "stale")
        : null,
    };
  }

  if (transition.type === "reconnected") {
    return {
      snapshot: withRuntimeStaleState(transition.snapshot, "fresh"),
      eventPosture: "live",
      snapshotState: "live",
    };
  }

  return {
    ...state,
    snapshot: state.snapshot
      ? applySnapshotEvent(state.snapshot, transition.envelope)
      : null,
  };
}

export function applySnapshotEvent(
  snapshot: OperatorStatusSnapshot,
  envelope: EventEnvelope,
): OperatorStatusSnapshot {
  const next = cloneSnapshot(snapshot);
  next.generated_at = envelope.at;

  if (envelope.device) {
    recordSatelliteActivity(next, envelope);
  }

  if (envelope.pipeline && envelope.event.type === "TurnStarted") {
    const event = envelope.event;
    const exists = next.active_turns.some((turn) => turn.turn === event.turn);
    if (!exists && envelope.conversation) {
      next.active_turns.push({
        pipeline: envelope.pipeline,
        conversation: envelope.conversation,
        turn: event.turn,
        trace: envelope.trace,
        started_at: envelope.at,
        invoked_components: [],
      });
    }
  }

  if (envelope.pipeline && envelope.event.type === "StageFailed") {
    const component = componentKindFromNode(envelope.event.node);
    const failedTurn = activeTurnForConversation(next, envelope.conversation);
    next.recent_failures.unshift({
      pipeline: envelope.pipeline,
      turn: failedTurn,
      component,
      provider: null,
      message: envelope.event.error,
      at: envelope.at,
    });
    markPipelineFailure(
      next,
      envelope.pipeline,
      component,
      envelope.event.error,
      failedTurn,
    );
  }

  if (
    envelope.event.type === "ConversationCompleted" ||
    envelope.event.type === "ConversationCancelled"
  ) {
    next.active_turns = next.active_turns.filter(
      (turn) => turn.conversation !== envelope.conversation,
    );
  }

  return next;
}

function withRuntimeStaleState(
  snapshot: OperatorStatusSnapshot,
  staleState: StaleState,
): OperatorStatusSnapshot {
  return {
    ...cloneSnapshot(snapshot),
    runtime: {
      ...snapshot.runtime,
      stale_state: staleState,
    },
  };
}

function cloneSnapshot(
  snapshot: OperatorStatusSnapshot,
): OperatorStatusSnapshot {
  return JSON.parse(JSON.stringify(snapshot)) as OperatorStatusSnapshot;
}

function recordSatelliteActivity(
  snapshot: OperatorStatusSnapshot,
  envelope: EventEnvelope,
) {
  if (!envelope.device) {
    return;
  }

  const known =
    snapshot.satellites.connected.find(
      (satellite) => satellite.device === envelope.device,
    ) ??
    snapshot.satellites.recently_active.find(
      (satellite) => satellite.device === envelope.device,
    );
  const name = known?.name ?? envelope.device;

  snapshot.satellites.recently_active =
    snapshot.satellites.recently_active.filter(
      (satellite) => satellite.device !== envelope.device,
    );
  snapshot.satellites.recently_active.unshift({
    device: envelope.device,
    name,
    last_seen_at: envelope.at,
    last_event: envelope.event.type,
  });

  if (
    envelope.event.type === "ConversationStarted" &&
    envelope.conversation &&
    envelope.pipeline
  ) {
    snapshot.satellites.connected = snapshot.satellites.connected.filter(
      (satellite) => satellite.device !== envelope.device,
    );
    snapshot.satellites.connected.unshift({
      device: envelope.device,
      name,
      connected_since: envelope.at,
      conversation: envelope.conversation,
      pipeline: envelope.pipeline,
    });
  }
}

function activeTurnForConversation(
  snapshot: OperatorStatusSnapshot,
  conversation: string | null,
): string | null {
  return (
    snapshot.active_turns.find((turn) => turn.conversation === conversation)
      ?.turn ?? null
  );
}

function markPipelineFailure(
  snapshot: OperatorStatusSnapshot,
  pipelineName: string,
  componentKind: ComponentKind,
  error: string,
  failedTurn: string | null,
) {
  snapshot.pipelines = snapshot.pipelines.map((pipeline) => {
    if (pipeline.name !== pipelineName) {
      return pipeline;
    }

    return {
      ...pipeline,
      health: {
        ...pipeline.health,
        state: "unhealthy",
        summary: error,
        last_failed_turn: failedTurn,
      },
      components: pipeline.components.map((component) =>
        component.kind === componentKind
          ? { ...component, state: "unhealthy", detail: error }
          : component,
      ),
    };
  });
}

function componentKindFromNode(node: string): ComponentKind {
  if (node === "stt" || node === "transcription") {
    return "transcription";
  }
  if (node === "llm" || node === "reasoning") {
    return "reasoning";
  }
  if (node === "tool" || node === "tools") {
    return "tools";
  }
  if (node === "tts" || node === "synthesis") {
    return "synthesis";
  }
  return "capture";
}
