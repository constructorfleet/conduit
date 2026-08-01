import type { OperatorStatusSnapshot } from "./contracts/status";
import type { OperatorAccess } from "./operatorAccess";

export type SnapshotState = "idle" | "loading" | "live" | "stale" | "error";

export interface SnapshotClientConfig {
  baseUrl: string;
  access: OperatorAccess;
}

export interface SnapshotClient {
  readonly statusRoute: "/v1/status";
  readonly eventRoute: "/v1/events";
  readonly state: SnapshotState;
  readonly snapshot: OperatorStatusSnapshot | null;
}

export function createSnapshotClient(
  config: SnapshotClientConfig,
): SnapshotClient {
  return {
    statusRoute: "/v1/status",
    eventRoute: "/v1/events",
    state: config.access.mode === "none" ? "idle" : "loading",
    snapshot: null,
  };
}

export function authorizationHeaders(access: OperatorAccess): HeadersInit {
  if (access.mode !== "bearer") {
    return {};
  }
  return { authorization: `Bearer ${access.token}` };
}
