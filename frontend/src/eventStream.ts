import type { EventType } from "./contracts/events";
import { operatorStatusSnapshotFixture } from "./contracts/status";

export type EventStreamPosture =
  "disconnected" | "connecting" | "live" | "stale";

export interface EventStreamPlan {
  route: "/v1/events";
  posture: EventStreamPosture;
  refreshSnapshotAfterReconnect: boolean;
  snapshotEventTypes: readonly EventType[];
}

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
    posture: "disconnected",
    refreshSnapshotAfterReconnect: true,
    snapshotEventTypes,
  };
}
