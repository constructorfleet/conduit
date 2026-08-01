export type EventStreamPosture =
  "disconnected" | "connecting" | "live" | "stale";

export interface EventStreamPlan {
  route: "/v1/events";
  posture: EventStreamPosture;
  refreshSnapshotAfterReconnect: boolean;
}

export function initialEventStreamPlan(): EventStreamPlan {
  return {
    route: "/v1/events",
    posture: "disconnected",
    refreshSnapshotAfterReconnect: true,
  };
}
