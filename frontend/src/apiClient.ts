import { conduitApiRoutes, createConduitApiClient } from "./contracts/client";
import {
  operatorStatusSnapshotFixture,
  type OperatorStatusSnapshot,
} from "./contracts/status";
import type { OperatorAccess } from "./operatorAccess";

export type SnapshotState = "idle" | "loading" | "live" | "stale" | "error";

export interface SnapshotClientConfig {
  baseUrl: string;
  access: OperatorAccess;
  snapshot?: OperatorStatusSnapshot;
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
  const client = createConduitApiClient({
    baseUrl: config.baseUrl,
    headers: () => authorizationHeaders(config.access),
  });

  return {
    statusRoute: client.routes.status,
    eventRoute: conduitApiRoutes.events,
    state: config.access.mode === "none" ? "idle" : "live",
    snapshot:
      config.access.mode === "none"
        ? null
        : (config.snapshot ?? operatorStatusSnapshotFixture),
  };
}

export function authorizationHeaders(access: OperatorAccess): HeadersInit {
  if (access.mode !== "bearer") {
    return {};
  }
  return { authorization: `Bearer ${access.token}` };
}
