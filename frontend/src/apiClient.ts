import { conduitApiRoutes, createConduitApiClient } from "./contracts/client";
import type {
  PipelineComponentCatalog,
  PipelineGraph,
  PipelineView,
  RawTurnEvents,
  TurnList,
  TurnSnapshot,
} from "./contracts/client";
import { pipelineViewFixture, turnSnapshotFixture } from "./contracts/client";
import {
  operatorStatusSnapshotFixture,
  type OperatorStatusSnapshot,
} from "./contracts/status";
import type { OperatorAccess } from "./operatorAccess";

export type SnapshotState = "idle" | "loading" | "live" | "stale" | "error";
export type OperatorDataMode = "live" | "mock";

export interface SnapshotClientConfig {
  baseUrl: string;
  access: OperatorAccess;
  snapshot?: OperatorStatusSnapshot;
  dataMode?: OperatorDataMode;
}

export interface SnapshotClient {
  readonly statusRoute: "/v1/status";
  readonly eventRoute: "/v1/events";
  readonly state: SnapshotState;
  readonly snapshot: OperatorStatusSnapshot | null;
  loadSnapshot: () => Promise<OperatorStatusSnapshot>;
  loadPipelineViews: () => Promise<PipelineView[]>;
  loadComponentCatalog: () => Promise<PipelineComponentCatalog>;
  loadTurns: () => Promise<TurnList>;
  loadTurn: (turnId: string) => Promise<TurnSnapshot>;
  loadTurnEvents: (turnId: string) => Promise<RawTurnEvents>;
  savePipeline: (graph: PipelineGraph) => Promise<PipelineView>;
  validatePipeline: (graph: PipelineGraph) => Promise<PipelineView>;
}

export function createSnapshotClient(
  config: SnapshotClientConfig,
): SnapshotClient {
  if (config.dataMode === "mock") {
    return createMockSnapshotClient(config);
  }

  const client = createConduitApiClient({
    baseUrl: config.baseUrl,
    headers: () => authorizationHeaders(config.access),
  });

  return {
    statusRoute: client.routes.status,
    eventRoute: conduitApiRoutes.events,
    state:
      config.access.mode === "none"
        ? "idle"
        : config.snapshot
          ? "live"
          : "loading",
    snapshot: config.access.mode === "none" ? null : (config.snapshot ?? null),
    loadSnapshot: () => client.status(),
    loadPipelineViews: async () => {
      const names = await client.listPipelines();
      return Promise.all(names.map((name) => client.getPipeline(name)));
    },
    loadComponentCatalog: () => client.listPipelineComponents(),
    loadTurns: () => client.listTurns(),
    loadTurn: (turnId) => client.getTurn(turnId),
    loadTurnEvents: (turnId) => client.getTurnEvents(turnId),
    savePipeline: (graph) => client.putPipeline(graph.name, graph),
    validatePipeline: (graph) => client.validatePipeline(graph),
  };
}

function createMockSnapshotClient(
  config: SnapshotClientConfig,
): SnapshotClient {
  return {
    statusRoute: conduitApiRoutes.status,
    eventRoute: conduitApiRoutes.events,
    state:
      config.access.mode === "none"
        ? "idle"
        : config.snapshot
          ? "live"
          : "loading",
    snapshot: config.access.mode === "none" ? null : (config.snapshot ?? null),
    loadSnapshot: async () => config.snapshot ?? operatorStatusSnapshotFixture,
    loadPipelineViews: async () => [pipelineViewFixture],
    loadComponentCatalog: async () => ({ components: [] }),
    loadTurns: async () => ({ turns: [turnSnapshotFixture] }),
    loadTurn: async () => turnSnapshotFixture,
    loadTurnEvents: async (turnId) => ({ turn_id: turnId, events: [] }),
    savePipeline: async (graph) => ({
      graph,
      order: graph.nodes.map((node) => node.id),
    }),
    validatePipeline: async (graph) => ({
      graph,
      order: graph.nodes.map((node) => node.id),
    }),
  };
}

export function authorizationHeaders(access: OperatorAccess): HeadersInit {
  if (access.mode !== "bearer") {
    return {};
  }
  return { authorization: `Bearer ${access.token}` };
}
