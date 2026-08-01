import {
  Activity,
  ArrowRight,
  Bell,
  Boxes,
  CircleAlert,
  CircleCheck,
  KeyRound,
  ListFilter,
  Maximize2,
  Minus,
  Network,
  Play,
  Plus,
  Radio,
  RotateCcw,
  Save,
  Settings,
  ShieldCheck,
  Trash2,
  Workflow,
  X,
} from "lucide-react";
import {
  type CSSProperties,
  Fragment,
  type FormEvent,
  type ReactNode,
  type PointerEvent as ReactPointerEvent,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import conduitLogo from "./assets/conduit-logo.png";
import "./App.css";
import { createSnapshotClient } from "./apiClient";
import type { OperatorDataMode, SnapshotState } from "./apiClient";
import type {
  ComponentConfigProperty,
  NodeKind,
  PipelineComponentCatalog,
  PipelineComponentDescriptor,
  PipelineEdge,
  PipelineGraph,
  PipelineNode,
  PipelineView,
  TurnSnapshot,
} from "./contracts/client";
import {
  eventEnvelopeFixtures,
  type EventEnvelope,
  type Event,
} from "./contracts/events";
import type {
  OperatorStatusSnapshot,
  PipelineStatus,
  ProviderKind,
  ProviderStatus,
  RuntimeFailure,
  ComponentKind,
} from "./contracts/status";
import { initialEventStreamPlan } from "./eventStream";
import type { EventStreamPosture } from "./eventStream";
import {
  type OperatorAccess,
  clearOperatorAccess,
  loadOperatorAccess,
  saveAnonymousAccess,
  saveBearerAccess,
} from "./operatorAccess";

const sections = [
  { id: "overview", label: "Overview", icon: Activity },
  { id: "pipelines", label: "Pipelines", icon: Workflow },
  { id: "providers", label: "Providers", icon: Boxes },
  { id: "events", label: "Events", icon: Radio },
  { id: "settings", label: "Settings", icon: Settings },
] as const;

type SectionId = (typeof sections)[number]["id"];

export type PipelineValidationResult =
  { ok: true; order: string[] } | { ok: false; message: string };
type PipelineValidator = (
  graph: PipelineGraph,
) => PipelineValidationResult | Promise<PipelineValidationResult>;
type PipelineTester = (name: string) => Promise<string>;
type ProviderTester = (providerId: string) => Promise<string>;

interface PipelineEditorDraftState {
  draft: PipelineGraph;
  history: PipelineGraph[];
  validation: PipelineValidationResult | null;
  notice: string | null;
}

interface OrbitPosition {
  x: number;
  y: number;
}

interface AugmentDragState {
  nodeId: string;
  startClientX: number;
  startClientY: number;
  startPosition: OrbitPosition;
}

interface AppProps {
  initialSnapshot?: OperatorStatusSnapshot;
  initialEvents?: readonly EventEnvelope[];
  initialEventPosture?: EventStreamPosture;
  initialComponentCatalog?: PipelineComponentCatalog;
  initialPipelineViews?: readonly PipelineView[];
  initialSmallScreen?: boolean;
  dataMode?: OperatorDataMode;
  onPipelineSaved?: (graph: PipelineGraph) => void;
  onPipelineValidate?: PipelineValidator;
  onPipelineTest?: PipelineTester;
}

function App({
  initialSnapshot,
  initialEvents,
  initialEventPosture,
  initialComponentCatalog,
  initialPipelineViews,
  initialSmallScreen = false,
  dataMode = defaultDataMode(),
  onPipelineSaved,
  onPipelineValidate,
  onPipelineTest,
}: AppProps = {}) {
  const [access, setAccess] = useState<OperatorAccess>(() =>
    loadOperatorAccess(),
  );
  const [activeSection, setActiveSection] = useState<SectionId>("overview");

  if (access.mode === "none") {
    return <OperatorAccessScreen onAccess={setAccess} />;
  }

  return (
    <OperatorWorkspace
      access={access}
      activeSection={activeSection}
      initialEvents={initialEvents}
      initialEventPosture={initialEventPosture}
      initialComponentCatalog={initialComponentCatalog}
      initialPipelineViews={initialPipelineViews}
      initialSmallScreen={initialSmallScreen}
      initialSnapshot={initialSnapshot}
      dataMode={dataMode}
      onPipelineSaved={onPipelineSaved}
      onPipelineValidate={onPipelineValidate}
      onPipelineTest={onPipelineTest}
      onSectionChange={setActiveSection}
      onClearAccess={() => {
        clearOperatorAccess();
        setAccess({ mode: "none" });
      }}
    />
  );
}

function OperatorAccessScreen({
  onAccess,
}: {
  onAccess: (access: OperatorAccess) => void;
}) {
  const [token, setToken] = useState("");
  const [remember, setRemember] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function connect(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    try {
      setError(null);
      onAccess(saveBearerAccess(token, remember));
    } catch (caught) {
      setError(
        caught instanceof Error ? caught.message : "Unable to store access",
      );
    }
  }

  return (
    <main className="access-screen">
      <section className="access-panel" aria-labelledby="operator-access-title">
        <div className="brand-block">
          <img src={conduitLogo} alt="Conduit" className="brand-logo" />
          <div>
            <p className="eyebrow">Conduit</p>
            <h1 id="operator-access-title">Operator Access</h1>
          </div>
        </div>

        <form className="access-form" onSubmit={connect}>
          <label className="field">
            <span>Management bearer token</span>
            <input
              autoComplete="off"
              inputMode="text"
              value={token}
              onChange={(event) => setToken(event.target.value)}
            />
          </label>

          <label className="check-row">
            <input
              type="checkbox"
              checked={remember}
              onChange={(event) => setRemember(event.target.checked)}
            />
            <span>Remember on this browser</span>
          </label>

          {error ? <p className="form-error">{error}</p> : null}

          <div className="access-actions">
            <button className="primary-action" type="submit">
              <KeyRound size={17} aria-hidden="true" />
              Connect
            </button>
            <button
              className="secondary-action"
              type="button"
              onClick={() => onAccess(saveAnonymousAccess())}
            >
              <Network size={17} aria-hidden="true" />
              Use anonymous mode
            </button>
          </div>
        </form>
      </section>
    </main>
  );
}

function OperatorWorkspace({
  access,
  activeSection,
  initialEvents,
  initialEventPosture,
  initialComponentCatalog,
  initialPipelineViews,
  initialSmallScreen,
  initialSnapshot,
  dataMode,
  onPipelineSaved,
  onPipelineValidate,
  onPipelineTest,
  onSectionChange,
  onClearAccess,
}: {
  access: OperatorAccess;
  activeSection: SectionId;
  initialEvents?: readonly EventEnvelope[];
  initialEventPosture?: EventStreamPosture;
  initialComponentCatalog?: PipelineComponentCatalog;
  initialPipelineViews?: readonly PipelineView[];
  initialSmallScreen: boolean;
  initialSnapshot?: OperatorStatusSnapshot;
  dataMode: OperatorDataMode;
  onPipelineSaved?: (graph: PipelineGraph) => void;
  onPipelineValidate?: PipelineValidator;
  onPipelineTest?: PipelineTester;
  onSectionChange: (section: SectionId) => void;
  onClearAccess: () => void;
}) {
  const snapshotClient = useMemo(
    () =>
      createSnapshotClient({
        baseUrl: window.location.origin,
        access,
        snapshot: initialSnapshot,
        dataMode,
      }),
    [access, dataMode, initialSnapshot],
  );
  const [snapshot, setSnapshot] = useState<OperatorStatusSnapshot | null>(
    snapshotClient.snapshot,
  );
  const [snapshotState, setSnapshotState] = useState<SnapshotState>(
    snapshotClient.snapshot ? "live" : snapshotClient.state,
  );
  const [loadError, setLoadError] = useState<string | null>(null);
  const [pipelineViews, setPipelineViews] = useState<readonly PipelineView[]>(
    () => initialPipelineViews ?? defaultPipelineViews(snapshotClient.snapshot),
  );
  const [componentCatalog, setComponentCatalog] =
    useState<PipelineComponentCatalog>(
      () => initialComponentCatalog ?? { components: [] },
    );
  const [providerDefinitions, setProviderDefinitions] = useState<
    ProviderDefinition[]
  >(() =>
    loadProviderDefinitions(
      initialComponentCatalog ?? { components: [] },
      initialPipelineViews ?? defaultPipelineViews(snapshotClient.snapshot),
      snapshotClient.snapshot,
    ),
  );
  const [turnSnapshot, setTurnSnapshot] = useState<TurnSnapshot | null>(null);
  const smallScreen = useSmallScreenMode(initialSmallScreen);
  const eventPlan = useMemo(() => {
    const plan = initialEventStreamPlan();
    return initialEventPosture
      ? { ...plan, posture: initialEventPosture }
      : plan;
  }, [initialEventPosture]);
  const hasStoredPipeline =
    pipelineViews.length > 0 || (snapshot?.pipelines.length ?? 0) > 0;
  const firstRun =
    snapshot?.runtime.launch_state === "first_run_setup" && !hasStoredPipeline;
  const accessLabel =
    access.mode === "anonymous"
      ? "Anonymous operator access"
      : access.mode === "bearer" && access.persisted
        ? "Remembered management token"
        : "Session management token";

  async function savePipeline(graph: PipelineGraph) {
    const view = await snapshotClient.savePipeline(graph);
    onPipelineSaved?.(view.graph);
    setPipelineViews((current) =>
      upsertPipelineView(current, view.graph, view.order),
    );
    await refreshSnapshotFromApi();
    onSectionChange("overview");
  }

  async function storePipelineGraph(graph: PipelineGraph) {
    const view = await snapshotClient.savePipeline(graph);
    onPipelineSaved?.(view.graph);
    setPipelineViews((current) =>
      upsertPipelineView(current, view.graph, view.order),
    );
    await refreshSnapshotFromApi();
  }

  async function runPipelineTest(name: string): Promise<string> {
    const result = await snapshotClient.runPipelineTest(name, {
      utterance: "conduit test",
    });
    await refreshSnapshotFromApi();
    return `Test turn completed for ${result.pipeline}: ${
      result.reply_text || `${result.audio_bytes} audio bytes`
    }`;
  }

  async function runProviderTest(providerId: string): Promise<string> {
    const loadedSnapshot = await refreshSnapshotFromApi();
    const provider = loadedSnapshot.providers.find(
      (candidate) => candidate.id === providerId,
    );
    if (!provider) {
      return `Provider ${providerId} is not in the latest status snapshot`;
    }
    if (provider.reachable) {
      return `Provider ${provider.id} is reachable`;
    }
    return `Provider ${provider.id} is ${provider.state}${
      provider.message ? `: ${provider.message}` : ""
    }`;
  }

  async function refreshSnapshotFromApi(): Promise<OperatorStatusSnapshot> {
    const loadedSnapshot = await snapshotClient.loadSnapshot();
    setSnapshot(loadedSnapshot);
    setProviderDefinitions((current) =>
      mergeProviderDefinitions(
        defaultProviderDefinitions(
          componentCatalog,
          pipelineViews,
          loadedSnapshot,
        ),
        current,
      ),
    );
    setSnapshotState("live");
    setLoadError(null);
    return loadedSnapshot;
  }

  useEffect(() => {
    if (access.mode === "none" || initialSnapshot) {
      return;
    }

    let cancelled = false;

    async function loadOperatorData() {
      try {
        await Promise.resolve();
        if (cancelled) {
          return;
        }

        setSnapshotState("loading");
        setLoadError(null);

        const [
          loadedSnapshot,
          loadedPipelineViews,
          loadedComponentCatalog,
          loadedTurns,
        ] = await Promise.all([
          snapshotClient.loadSnapshot(),
          initialPipelineViews
            ? Promise.resolve([...initialPipelineViews])
            : snapshotClient.loadPipelineViews(),
          initialComponentCatalog
            ? Promise.resolve(initialComponentCatalog)
            : snapshotClient.loadComponentCatalog(),
          initialEvents
            ? Promise.resolve({ turns: [] })
            : snapshotClient.loadTurns().catch(() => ({ turns: [] })),
        ]);

        if (cancelled) {
          return;
        }

        const latestTurn = loadedTurns.turns[0]?.turn_id;
        const loadedTurnSnapshot = latestTurn
          ? await snapshotClient.loadTurn(latestTurn).catch(() => null)
          : null;

        if (cancelled) {
          return;
        }

        setSnapshot(loadedSnapshot);
        setPipelineViews(initialPipelineViews ?? loadedPipelineViews);
        setComponentCatalog(initialComponentCatalog ?? loadedComponentCatalog);
        setProviderDefinitions((current) =>
          mergeProviderDefinitions(
            defaultProviderDefinitions(
              initialComponentCatalog ?? loadedComponentCatalog,
              initialPipelineViews ?? loadedPipelineViews,
              loadedSnapshot,
            ),
            current,
          ),
        );
        setTurnSnapshot(loadedTurnSnapshot);
        setSnapshotState("live");
      } catch (caught) {
        if (cancelled) {
          return;
        }

        setSnapshot(null);
        setPipelineViews(initialPipelineViews ?? []);
        setComponentCatalog(initialComponentCatalog ?? { components: [] });
        setSnapshotState("error");
        setLoadError(
          caught instanceof Error
            ? caught.message
            : "Unable to load operator data",
        );
      }
    }

    void loadOperatorData();

    return () => {
      cancelled = true;
    };
  }, [
    access.mode,
    initialEvents,
    initialComponentCatalog,
    initialPipelineViews,
    initialSnapshot,
    snapshotClient,
  ]);

  useEffect(() => {
    saveProviderDefinitions(providerDefinitions);
  }, [providerDefinitions]);

  if (firstRun) {
    return (
      <main className="first-run-shell">
        <header className="first-run-header">
          <div className="rail-brand">
            <img src={conduitLogo} alt="Conduit" />
            <div>
              <p>Conduit</p>
              <strong>Operator Console</strong>
            </div>
          </div>
          <div>
            <p className="eyebrow">{accessLabel}</p>
            <h1>First-Run Setup</h1>
          </div>
          <button className="sign-out" type="button" onClick={onClearAccess}>
            Clear access
          </button>
        </header>
        <GuidedSetupPanel onPipelineSaved={savePipeline} />
      </main>
    );
  }

  return (
    <main className="workspace-shell">
      <aside className="workspace-rail" aria-label="Operator Console sections">
        <div className="rail-brand">
          <img src={conduitLogo} alt="Conduit" />
          <div>
            <p>Conduit</p>
            <strong>Operator Console</strong>
          </div>
        </div>

        <div
          className="section-tabs"
          role="tablist"
          aria-label="Operator sections"
        >
          {sections.map((section) => {
            const Icon = section.icon;
            const selected = activeSection === section.id;
            return (
              <button
                key={section.id}
                type="button"
                role="tab"
                aria-label={section.label}
                aria-selected={selected}
                className={selected ? "section-tab selected" : "section-tab"}
                onClick={() => onSectionChange(section.id)}
              >
                <Icon size={17} aria-hidden="true" />
                <span>{section.label}</span>
              </button>
            );
          })}
        </div>

        <button className="sign-out" type="button" onClick={onClearAccess}>
          Clear access
        </button>
      </aside>

      <section className="workspace-content" aria-live="polite">
        <header className="workspace-header">
          <div>
            <p className="eyebrow">{accessLabel}</p>
            <h1>
              {firstRun
                ? "First-Run Setup"
                : sections.find((section) => section.id === activeSection)
                    ?.label}
            </h1>
          </div>
          <div className="runtime-strip">
            {snapshotState === "live" ? null : (
              <StatusPill
                label="Snapshot"
                value={snapshotState}
                tone="caution"
              />
            )}
            <StatusPill
              label="Events"
              value={eventPlan.posture}
              tone="neutral"
            />
          </div>
        </header>

        <SectionPanel
          section={activeSection}
          events={initialEvents ?? eventEnvelopeFixtures}
          turnSnapshot={turnSnapshot}
          componentCatalog={componentCatalog}
          providerDefinitions={providerDefinitions}
          pipelineViews={pipelineViews}
          snapshot={snapshot}
          eventPosture={eventPlan.posture}
          initialSmallScreen={smallScreen}
          loadError={loadError}
          onSectionChange={onSectionChange}
          onPipelineValidate={
            onPipelineValidate ??
            (async (graph) =>
              pipelineViewToValidation(
                await snapshotClient.validatePipeline(graph),
              ))
          }
          onPipelineTest={onPipelineTest ?? runPipelineTest}
          onProviderTest={runProviderTest}
          onProviderDefinitionsChange={setProviderDefinitions}
          onPipelineStored={storePipelineGraph}
        />
      </section>
    </main>
  );
}

function defaultDataMode(): OperatorDataMode {
  return import.meta.env.VITE_CONDUIT_DATA_SOURCE === "mock" ? "mock" : "live";
}

function SectionPanel({
  section,
  events,
  turnSnapshot,
  componentCatalog,
  providerDefinitions,
  pipelineViews,
  snapshot,
  eventPosture,
  initialSmallScreen,
  loadError,
  onSectionChange,
  onPipelineStored,
  onPipelineValidate,
  onPipelineTest,
  onProviderTest,
  onProviderDefinitionsChange,
}: {
  section: SectionId;
  events: readonly EventEnvelope[];
  turnSnapshot: TurnSnapshot | null;
  componentCatalog: PipelineComponentCatalog;
  providerDefinitions: readonly ProviderDefinition[];
  pipelineViews: readonly PipelineView[];
  snapshot: OperatorStatusSnapshot | null;
  eventPosture: EventStreamPosture;
  initialSmallScreen: boolean;
  loadError: string | null;
  onSectionChange: (section: SectionId) => void;
  onPipelineStored: (graph: PipelineGraph, order: string[]) => Promise<void>;
  onPipelineValidate: PipelineValidator;
  onPipelineTest: PipelineTester;
  onProviderTest: ProviderTester;
  onProviderDefinitionsChange: (definitions: ProviderDefinition[]) => void;
}) {
  if (loadError) {
    return (
      <div className="overview-empty" role="alert">
        <CircleAlert size={18} aria-hidden="true" />
        <span>{loadError}</span>
      </div>
    );
  }

  if (section === "overview") {
    return (
      <OverviewPanel
        snapshot={snapshot}
        eventPosture={eventPosture}
        onOpenFailureEvents={() => onSectionChange("events")}
      />
    );
  }

  if (section === "events") {
    return (
      <EventsPanel
        events={events}
        turnSnapshot={turnSnapshot}
        eventPosture={eventPosture}
      />
    );
  }

  if (section === "pipelines") {
    return (
      <PipelinesPanel
        providerDefinitions={providerDefinitions}
        pipelineViews={pipelineViews}
        readOnly={initialSmallScreen}
        onPipelineStored={onPipelineStored}
        onPipelineValidate={onPipelineValidate}
        onPipelineTest={onPipelineTest}
      />
    );
  }

  if (section === "providers") {
    return (
      <ProvidersPanel
        componentCatalog={componentCatalog}
        pipelineViews={pipelineViews}
        providerDefinitions={providerDefinitions}
        providers={snapshot?.providers ?? []}
        onProviderTest={onProviderTest}
        onProviderDefinitionsChange={onProviderDefinitionsChange}
      />
    );
  }

  return <SettingsPanel pipelineViews={pipelineViews} access={snapshot} />;
}

const OPERATOR_SETTINGS_STORAGE_KEY = "conduit.operator.settings";
const PROVIDER_DEFINITIONS_STORAGE_KEY = "conduit.provider.definitions";
const retentionOptions = ["7 d", "30 d", "90 d", "forever"] as const;
const logLevelOptions = ["debug", "info", "warn", "error"] as const;

type RetentionOption = (typeof retentionOptions)[number];
type LogLevelOption = (typeof logLevelOptions)[number];

interface OperatorConsoleSettings {
  deploymentName: string;
  localOnly: boolean;
  defaultPipeline: string;
  retention: RetentionOption;
  logLevel: LogLevelOption;
}

type ProviderFilter = "all" | ProviderKind;

interface ProviderDefinition {
  id: string;
  label: string;
  kind: NodeKind;
  component: string;
  config: Record<string, unknown>;
}

interface ProviderCardView {
  id: string;
  label: string;
  kind: ProviderKind;
  component: string | null;
  definition: ProviderDefinition | null;
  status: ProviderStatus | null;
}

function ProvidersPanel({
  componentCatalog,
  pipelineViews,
  providerDefinitions,
  providers,
  onProviderTest,
  onProviderDefinitionsChange,
}: {
  componentCatalog: PipelineComponentCatalog;
  pipelineViews: readonly PipelineView[];
  providerDefinitions: readonly ProviderDefinition[];
  providers: readonly ProviderStatus[];
  onProviderTest: ProviderTester;
  onProviderDefinitionsChange: (definitions: ProviderDefinition[]) => void;
}) {
  const [filter, setFilter] = useState<ProviderFilter>("all");
  const [draftProvider, setDraftProvider] = useState<ProviderDefinition | null>(
    null,
  );
  const [editingProviderId, setEditingProviderId] = useState<string | null>(
    null,
  );
  const [addProviderDialogOpen, setAddProviderDialogOpen] = useState(false);
  const [selectedProviderKind, setSelectedProviderKind] =
    useState<ProviderKind | null>(null);
  const [providerNotices, setProviderNotices] = useState<
    Record<string, string>
  >({});
  const providerCards = providerCardViews(providerDefinitions, providers);
  const visibleProviderCards = providerCards.filter(
    (provider) => filter === "all" || provider.kind === filter,
  );
  const providerKinds: ProviderFilter[] = ["all", "stt", "llm", "tool", "tts"];
  const providerIds = new Set([
    ...providers.map((provider) => provider.id),
    ...providerDefinitions.map((provider) => provider.id),
  ]);
  const referencedProviderCount = new Set(
    pipelineViews.flatMap((view) =>
      view.graph.nodes
        .map((node) => node.provider)
        .filter((provider) => providerIds.has(provider)),
    ),
  ).size;
  const selectedDraftComponent = draftProvider
    ? (componentCatalog.components.find(
        (component) => component.id === draftProvider.component,
      ) ?? null)
    : null;
  const draftProviderValidation =
    draftProvider && selectedDraftComponent
      ? validateProviderDefinitionConfig(draftProvider, selectedDraftComponent)
      : ({
          ok: false,
          message: "Choose a provider component",
        } satisfies PipelineValidationResult);
  const selectedKindComponents = selectedProviderKind
    ? componentCatalog.components.filter(
        (component) =>
          component.kind === nodeKindForProviderKind(selectedProviderKind),
      )
    : [];

  function startNewProvider(component: PipelineComponentDescriptor) {
    const kind = providerKindForNodeKind(component.kind);
    if (!kind) {
      return;
    }

    setDraftProvider({
      id: `${kind}-${providerDefinitions.length + 1}`,
      label: component.label,
      kind: component.kind,
      component: component.id,
      config: {},
    });
    setEditingProviderId("new");
    setSelectedProviderKind(null);
  }

  function editProviderCard(card: ProviderCardView) {
    if (card.definition) {
      setDraftProvider(cloneProviderDefinition(card.definition));
      setEditingProviderId(card.id);
      setAddProviderDialogOpen(true);
      setSelectedProviderKind(null);
      return;
    }

    setDraftProvider({
      id: card.id,
      label: card.label,
      kind: nodeKindForProviderKind(card.kind),
      component: card.component ?? card.id,
      config: {},
    });
    setEditingProviderId(card.id);
    setAddProviderDialogOpen(true);
    setSelectedProviderKind(null);
  }

  function updateDraftConfig(
    field: string,
    property: ComponentConfigProperty,
    value: string | boolean,
  ) {
    setDraftProvider((current) => {
      if (!current) {
        return current;
      }

      return {
        ...current,
        config: updateConfigValue(current.config, field, property, value),
      };
    });
  }

  function updateDraftProvider(
    updater: (current: ProviderDefinition) => ProviderDefinition,
  ) {
    setDraftProvider((current) => (current ? updater(current) : current));
  }

  function saveDraftProvider() {
    if (!draftProvider) {
      return;
    }

    const id = draftProvider.id.trim();
    const component = componentCatalog.components.find(
      (candidate) => candidate.id === draftProvider.component,
    );
    if (!id || !component) {
      return;
    }
    const validation = validateProviderDefinitionConfig(
      draftProvider,
      component,
    );
    if (!validation.ok) {
      return;
    }

    const next: ProviderDefinition = {
      ...draftProvider,
      id,
      label: draftProvider.label.trim() || id,
      kind: component.kind,
      config: pruneEmptyConfig(draftProvider.config),
    };
    onProviderDefinitionsChange(
      mergeProviderDefinitions(
        providerDefinitions.filter((provider) => provider.id !== next.id),
        [next],
      ),
    );
    setProviderNotices((current) => ({
      ...current,
      [next.id]: `Provider ${next.id} saved`,
    }));
    setDraftProvider(null);
    setEditingProviderId(null);
    setAddProviderDialogOpen(false);
    setSelectedProviderKind(null);
  }

  function cancelDraftProvider() {
    setDraftProvider(null);
    setEditingProviderId(null);
    setAddProviderDialogOpen(false);
    setSelectedProviderKind(null);
  }

  function openAddProviderDialog() {
    setDraftProvider(null);
    setEditingProviderId("new");
    setSelectedProviderKind(null);
    setAddProviderDialogOpen(true);
  }

  function deleteProviderDefinition(provider: ProviderCardView) {
    if (provider.status) {
      setProviderNotices((current) => ({
        ...current,
        [provider.id]: `Provider ${provider.id} is registered by the server; remove it from server configuration to delete it.`,
      }));
      return;
    }

    const providerId = provider.id;
    onProviderDefinitionsChange(
      providerDefinitions.filter((provider) => provider.id !== providerId),
    );
    if (editingProviderId === providerId) {
      cancelDraftProvider();
    }
    setProviderNotices((current) => ({
      ...current,
      [providerId]: `Provider ${providerId} deleted`,
    }));
  }

  async function testProvider(provider: ProviderCardView) {
    try {
      let notice: string;
      if (provider.status) {
        notice = await onProviderTest(provider.id);
      } else if (provider.definition) {
        const component = componentCatalog.components.find(
          (candidate) => candidate.id === provider.definition?.component,
        );
        const validation = component
          ? validateProviderDefinitionConfig(provider.definition, component)
          : {
              ok: false,
              message: `Unknown component ${provider.definition.component}`,
            };
        notice = validation.ok
          ? `Provider ${provider.id} configuration is valid for ${component?.label}`
          : validation.message;
      } else {
        notice = `Provider ${provider.id} has no configuration to test`;
      }
      setProviderNotices((current) => ({
        ...current,
        [provider.id]: notice,
      }));
    } catch (caught) {
      setProviderNotices((current) => ({
        ...current,
        [provider.id]:
          caught instanceof Error
            ? caught.message
            : `Unable to test ${provider.id}`,
      }));
    }
  }

  return (
    <div className="providers-stack">
      <section className="summary-grid" aria-label="Provider summary">
        <MetricTile
          label="Visible providers"
          value={`${visibleProviderCards.length} visible`}
        />
        <MetricTile
          label="Provider configs"
          value={providerDefinitions.length.toString()}
        />
        <MetricTile
          label="Configured in graphs"
          value={referencedProviderCount.toString()}
        />
        <MetricTile
          label="Warnings"
          value={providers
            .filter((provider) => provider.state !== "proven")
            .length.toString()}
        />
      </section>

      <div className="providers-controls">
        <div
          className="segmented-control"
          role="toolbar"
          aria-label="Provider stage filter"
        >
          {providerKinds.map((kind) => (
            <button
              key={kind}
              type="button"
              className={filter === kind ? "selected" : ""}
              onClick={() => setFilter(kind)}
            >
              {kind === "all" ? "All" : kind.toUpperCase()}
            </button>
          ))}
        </div>
        <button
          className="secondary-action"
          type="button"
          onClick={openAddProviderDialog}
        >
          <Plus size={17} aria-hidden="true" />
          Add provider
        </button>
      </div>

      {Object.values(providerNotices).map((notice) => (
        <p className="panel-notice" key={notice}>
          {notice}
        </p>
      ))}

      <section className="provider-card-grid" aria-label="Provider cards">
        {visibleProviderCards.map((provider) => (
          <article
            className={`provider-card ${provider.status?.state ?? "configured"}`}
            key={provider.id}
          >
            <div className="provider-card-header">
              <span className="provider-kind">
                {provider.kind.toUpperCase()}
              </span>
              <div className="provider-card-controls">
                <StatusPill
                  label="Status"
                  value={provider.status?.state ?? "configured"}
                  tone={
                    provider.status?.state === "proven" ? "neutral" : "caution"
                  }
                />
                <button
                  className="icon-action"
                  type="button"
                  aria-label={`Edit ${provider.id}`}
                  onClick={() => editProviderCard(provider)}
                >
                  <Settings size={17} aria-hidden="true" />
                </button>
                {provider.definition ? (
                  <button
                    className="icon-action danger"
                    type="button"
                    aria-label={`Delete ${provider.id}`}
                    onClick={() => deleteProviderDefinition(provider)}
                  >
                    <Trash2 size={17} aria-hidden="true" />
                  </button>
                ) : null}
              </div>
            </div>
            <h2>{provider.label}</h2>
            <p>
              {provider.status?.message ??
                provider.definition?.id ??
                "Provider has no runtime status yet"}
            </p>

            <div className="provider-facts">
              <div>
                <span>Component</span>
                <strong>{provider.component ?? "unknown"}</strong>
              </div>
              <div>
                <span>Configured</span>
                <strong>
                  {provider.status?.configured || provider.definition
                    ? "yes"
                    : "no"}
                </strong>
              </div>
              <div>
                <span>Reachable</span>
                <strong>{provider.status?.reachable ? "yes" : "no"}</strong>
              </div>
              <div>
                <span>Pipelines</span>
                <strong>
                  {provider.status?.affects_pipelines.join(", ") || "none"}
                </strong>
              </div>
            </div>

            {provider.status || provider.definition ? (
              <div className="provider-actions">
                <button
                  className="secondary-action provider-test-action"
                  type="button"
                  aria-label={`Test ${provider.id}`}
                  onClick={() => testProvider(provider)}
                >
                  <Play size={17} aria-hidden="true" />
                  Test
                </button>
              </div>
            ) : null}
          </article>
        ))}
        {visibleProviderCards.length === 0 ? (
          <div className="overview-empty" role="status">
            <Boxes size={18} aria-hidden="true" />
            <span>No providers match this stage filter</span>
          </div>
        ) : null}
      </section>
      {addProviderDialogOpen ? (
        <ProviderAddDialog
          componentCatalog={componentCatalog}
          draftProvider={draftProvider}
          providerKinds={providerKinds}
          selectedComponent={selectedDraftComponent}
          validation={draftProviderValidation}
          selectedKind={selectedProviderKind}
          selectedKindComponents={selectedKindComponents}
          onCancel={cancelDraftProvider}
          onConfigChange={updateDraftConfig}
          onDraftChange={updateDraftProvider}
          onKindChange={setSelectedProviderKind}
          onSave={saveDraftProvider}
          onSelectComponent={startNewProvider}
        />
      ) : null}
    </div>
  );
}

function ProviderAddDialog({
  componentCatalog,
  draftProvider,
  providerKinds,
  selectedComponent,
  validation,
  selectedKind,
  selectedKindComponents,
  onCancel,
  onConfigChange,
  onDraftChange,
  onKindChange,
  onSave,
  onSelectComponent,
}: {
  componentCatalog: PipelineComponentCatalog;
  draftProvider: ProviderDefinition | null;
  providerKinds: readonly ProviderFilter[];
  selectedComponent: PipelineComponentDescriptor | null;
  validation: PipelineValidationResult;
  selectedKind: ProviderKind | null;
  selectedKindComponents: readonly PipelineComponentDescriptor[];
  onCancel: () => void;
  onConfigChange: (
    field: string,
    property: ComponentConfigProperty,
    value: string | boolean,
  ) => void;
  onDraftChange: (
    updater: (current: ProviderDefinition) => ProviderDefinition,
  ) => void;
  onKindChange: (kind: ProviderKind | null) => void;
  onSave: () => void;
  onSelectComponent: (component: PipelineComponentDescriptor) => void;
}) {
  return (
    <div className="modal-backdrop">
      <section
        className="provider-add-dialog"
        role="dialog"
        aria-modal="true"
        aria-label="Add provider"
      >
        <div className="provider-card-header">
          <div>
            <p className="eyebrow">Add Provider</p>
            <h2>{draftProvider ? "Configure provider" : "Choose type"}</h2>
          </div>
          <div className="provider-card-controls">
            {draftProvider ? (
              <button
                className="icon-action success"
                type="button"
                aria-label="Save provider"
                disabled={!validation.ok}
                onClick={onSave}
              >
                <Save size={17} aria-hidden="true" />
              </button>
            ) : null}
            <button
              className="icon-action danger"
              type="button"
              aria-label="Cancel provider edit"
              onClick={onCancel}
            >
              <X size={17} aria-hidden="true" />
            </button>
          </div>
        </div>

        {draftProvider ? (
          <ProviderEditorFields
            componentCatalog={componentCatalog}
            draftProvider={draftProvider}
            selectedComponent={selectedComponent}
            validation={validation}
            onConfigChange={onConfigChange}
            onDraftChange={onDraftChange}
          />
        ) : (
          <div className="provider-kind-menu inline" role="menu">
            {selectedKind ? (
              <>
                <button
                  className="provider-kind-back"
                  type="button"
                  role="menuitem"
                  onClick={() => onKindChange(null)}
                >
                  Provider types
                </button>
                {selectedKindComponents.map((component) => (
                  <button
                    key={component.id}
                    type="button"
                    role="menuitem"
                    onClick={() => onSelectComponent(component)}
                  >
                    {component.label}
                  </button>
                ))}
                {selectedKindComponents.length === 0 ? (
                  <span className="provider-kind-empty">
                    No components for {selectedKind.toUpperCase()}
                  </span>
                ) : null}
              </>
            ) : (
              providerKinds
                .filter((kind): kind is ProviderKind => kind !== "all")
                .map((kind) => (
                  <button
                    key={kind}
                    type="button"
                    role="menuitem"
                    onClick={() => onKindChange(kind)}
                  >
                    {kind.toUpperCase()}
                  </button>
                ))
            )}
          </div>
        )}
      </section>
    </div>
  );
}

function ProviderEditorFields({
  componentCatalog,
  draftProvider,
  selectedComponent,
  validation,
  onConfigChange,
  onDraftChange,
}: {
  componentCatalog: PipelineComponentCatalog;
  draftProvider: ProviderDefinition;
  selectedComponent: PipelineComponentDescriptor | null;
  validation: PipelineValidationResult;
  onConfigChange: (
    field: string,
    property: ComponentConfigProperty,
    value: string | boolean,
  ) => void;
  onDraftChange: (
    updater: (current: ProviderDefinition) => ProviderDefinition,
  ) => void;
}) {
  return (
    <div className="provider-definition-form">
      <label className="field">
        <span>Provider id</span>
        <input
          value={draftProvider.id}
          onChange={(event) =>
            onDraftChange((current) => ({
              ...current,
              id: event.target.value,
            }))
          }
        />
      </label>
      <label className="field">
        <span>Provider label</span>
        <input
          value={draftProvider.label}
          onChange={(event) =>
            onDraftChange((current) => ({
              ...current,
              label: event.target.value,
            }))
          }
        />
      </label>
      <label className="field">
        <span>Provider component</span>
        <select
          value={draftProvider.component}
          onChange={(event) => {
            const component = componentCatalog.components.find(
              (candidate) => candidate.id === event.target.value,
            );
            onDraftChange((current) =>
              component
                ? {
                    ...current,
                    component: component.id,
                    kind: component.kind,
                    config: {},
                  }
                : current,
            );
          }}
        >
          {componentCatalog.components.map((component) => (
            <option value={component.id} key={component.id}>
              {component.label}
            </option>
          ))}
        </select>
      </label>
      {selectedComponent ? (
        <ComponentConfigFields
          component={selectedComponent}
          config={draftProvider.config}
          readOnly={false}
          onChange={onConfigChange}
        />
      ) : null}
      {!validation.ok ? (
        <p className="form-error">{validation.message}</p>
      ) : null}
    </div>
  );
}

function SettingsPanel({
  pipelineViews,
  access,
}: {
  pipelineViews: readonly PipelineView[];
  access: OperatorStatusSnapshot | null;
}) {
  const defaultSettings = useMemo(
    () => defaultOperatorSettings(pipelineViews, access),
    [access, pipelineViews],
  );
  const [settings, setSettings] = useState<OperatorConsoleSettings>(() =>
    loadOperatorSettings(defaultSettings, pipelineViews),
  );
  const [resetOpen, setResetOpen] = useState(false);
  const [resetConfirmation, setResetConfirmation] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const deploymentName = settings.deploymentName;
  const localOnly = settings.localOnly;
  const defaultPipeline = settings.defaultPipeline;
  const retention = settings.retention;
  const logLevel = settings.logLevel;

  function saveSettings() {
    const next = {
      ...settings,
      deploymentName: deploymentName.trim() || "conduit-local",
    };
    setSettings(next);
    saveOperatorSettings(next);
    setNotice(`Settings saved for ${deploymentName.trim() || "conduit-local"}`);
  }

  function resetLocalState() {
    if (resetConfirmation !== "RESET") {
      setNotice("Type RESET to confirm");
      return;
    }

    clearOperatorSettings();
    setSettings(defaultSettings);
    setResetOpen(false);
    setResetConfirmation("");
    setNotice("Local console state reset");
  }

  return (
    <div className="settings-stack">
      <section className="settings-identity">
        <ShieldCheck size={28} aria-hidden="true" />
        <div>
          <p className="eyebrow">Frontend-only console settings</p>
          <h2>{deploymentName || "conduit-local"}</h2>
          <p>
            Snapshot{" "}
            {access?.generated_at
              ? formatTime(access.generated_at)
              : "unavailable"}{" "}
            / default pipeline {defaultPipeline}
          </p>
        </div>
        <StatusPill
          label="Network"
          value={localOnly ? "local-only" : "outbound allowed"}
          tone="neutral"
        />
      </section>

      <section className="settings-card" aria-labelledby="settings-identity">
        <div className="section-heading">
          <div>
            <p className="eyebrow">Deployment</p>
            <h2 id="settings-identity">Identity</h2>
          </div>
          <button
            className="primary-action"
            type="button"
            onClick={saveSettings}
          >
            <Save size={17} aria-hidden="true" />
            Save settings
          </button>
        </div>

        <div className="settings-grid">
          <label className="field">
            <span>Deployment name</span>
            <input
              value={deploymentName}
              onChange={(event) =>
                setSettings((current) => ({
                  ...current,
                  deploymentName: event.target.value,
                }))
              }
            />
          </label>
          <label className="field">
            <span>Default pipeline</span>
            <select
              value={defaultPipeline}
              onChange={(event) =>
                setSettings((current) => ({
                  ...current,
                  defaultPipeline: event.target.value,
                }))
              }
            >
              {pipelineViews.length > 0 ? (
                pipelineViews.map((view) => (
                  <option key={view.graph.name} value={view.graph.name}>
                    {view.graph.name}
                  </option>
                ))
              ) : (
                <option value={defaultPipeline}>{defaultPipeline}</option>
              )}
            </select>
          </label>
          <label className="toggle-row">
            <span>
              <strong>Local-only mode</strong>
              <small>Block outbound provider calls in local UI policy.</small>
            </span>
            <input
              aria-label="Local-only mode"
              type="checkbox"
              checked={localOnly}
              onChange={(event) =>
                setSettings((current) => ({
                  ...current,
                  localOnly: event.target.checked,
                }))
              }
            />
          </label>
          <label className="field">
            <span>Log level</span>
            <select
              aria-label="Log level"
              value={logLevel}
              onChange={(event) =>
                setSettings((current) => ({
                  ...current,
                  logLevel: event.target.value as LogLevelOption,
                }))
              }
            >
              {logLevelOptions.map((level) => (
                <option key={level} value={level}>
                  {level}
                </option>
              ))}
            </select>
          </label>
        </div>
      </section>

      <section className="settings-card" aria-labelledby="retention-title">
        <div className="section-heading">
          <div>
            <p className="eyebrow">Turn data</p>
            <h2 id="retention-title">Retention</h2>
          </div>
          <StatusPill label="Current" value={retention} tone="neutral" />
        </div>
        <div
          className="segmented-control"
          role="toolbar"
          aria-label="Retention"
        >
          {retentionOptions.map((option) => (
            <button
              key={option}
              type="button"
              aria-pressed={retention === option}
              className={retention === option ? "selected" : ""}
              onClick={() =>
                setSettings((current) => ({
                  ...current,
                  retention: option,
                }))
              }
            >
              {option}
            </button>
          ))}
        </div>
      </section>

      <section
        className="settings-card danger-zone"
        aria-labelledby="danger-title"
      >
        <div className="section-heading">
          <div>
            <p className="eyebrow">Local browser state</p>
            <h2 id="danger-title">Danger Zone</h2>
          </div>
          <button
            className="danger-action"
            type="button"
            onClick={() => setResetOpen(true)}
          >
            <Trash2 size={17} aria-hidden="true" />
            Reset local state
          </button>
        </div>
        {resetOpen ? (
          <div className="reset-confirmation">
            <p className="reset-instructions">
              Type RESET to permanently clear saved UI settings.
            </p>
            <label className="field">
              <span>Reset confirmation</span>
              <input
                value={resetConfirmation}
                onChange={(event) => setResetConfirmation(event.target.value)}
              />
            </label>
            <button
              className="danger-action"
              type="button"
              onClick={resetLocalState}
            >
              Confirm reset
            </button>
          </div>
        ) : null}
      </section>

      {notice ? <p className="panel-notice">{notice}</p> : null}
    </div>
  );
}

function defaultOperatorSettings(
  pipelineViews: readonly PipelineView[],
  access: OperatorStatusSnapshot | null,
): OperatorConsoleSettings {
  return {
    deploymentName: "conduit-local",
    localOnly: true,
    defaultPipeline:
      pipelineViews[0]?.graph.name ?? access?.pipelines[0]?.name ?? "default",
    retention: "30 d",
    logLevel: "info",
  };
}

function loadOperatorSettings(
  defaults: OperatorConsoleSettings,
  pipelineViews: readonly PipelineView[],
): OperatorConsoleSettings {
  try {
    return normalizeOperatorSettings(
      JSON.parse(localStorage.getItem(OPERATOR_SETTINGS_STORAGE_KEY) ?? "null"),
      defaults,
      pipelineViews,
    );
  } catch {
    return defaults;
  }
}

function saveOperatorSettings(settings: OperatorConsoleSettings): void {
  localStorage.setItem(OPERATOR_SETTINGS_STORAGE_KEY, JSON.stringify(settings));
}

function clearOperatorSettings(): void {
  localStorage.removeItem(OPERATOR_SETTINGS_STORAGE_KEY);
}

function normalizeOperatorSettings(
  value: unknown,
  defaults: OperatorConsoleSettings,
  pipelineViews: readonly PipelineView[],
): OperatorConsoleSettings {
  if (!value || typeof value !== "object") {
    return defaults;
  }

  const saved = value as Partial<OperatorConsoleSettings>;
  const pipelineNames = new Set(pipelineViews.map((view) => view.graph.name));
  const defaultPipeline =
    typeof saved.defaultPipeline === "string" &&
    (pipelineNames.size === 0 || pipelineNames.has(saved.defaultPipeline))
      ? saved.defaultPipeline
      : defaults.defaultPipeline;

  return {
    deploymentName:
      typeof saved.deploymentName === "string" && saved.deploymentName.trim()
        ? saved.deploymentName
        : defaults.deploymentName,
    localOnly:
      typeof saved.localOnly === "boolean"
        ? saved.localOnly
        : defaults.localOnly,
    defaultPipeline,
    retention: isRetentionOption(saved.retention)
      ? saved.retention
      : defaults.retention,
    logLevel: isLogLevelOption(saved.logLevel)
      ? saved.logLevel
      : defaults.logLevel,
  };
}

function isRetentionOption(value: unknown): value is RetentionOption {
  return retentionOptions.includes(value as RetentionOption);
}

function isLogLevelOption(value: unknown): value is LogLevelOption {
  return logLevelOptions.includes(value as LogLevelOption);
}

function MetricTile({ label, value }: { label: string; value: string }) {
  return (
    <article className="metric-tile">
      <span>{label}</span>
      <strong>{value}</strong>
    </article>
  );
}

function EventsPanel({
  events,
  turnSnapshot,
  eventPosture,
}: {
  events: readonly EventEnvelope[];
  turnSnapshot: TurnSnapshot | null;
  eventPosture: EventStreamPosture;
}) {
  const [activeView, setActiveView] = useState<"story" | "raw">("story");
  const [filter, setFilter] = useState("");
  const [selectedEventId, setSelectedEventId] = useState<string | null>(null);
  const turn = useMemo(
    () =>
      turnSnapshot
        ? reconstructServerTurn(turnSnapshot)
        : reconstructTurn(events),
    [events, turnSnapshot],
  );
  const rawEvents = useMemo(
    () => filterRawEvents(events, filter),
    [events, filter],
  );
  const stale = eventPosture === "stale" || eventPosture === "disconnected";

  function selectStoryEvent(id: string) {
    setSelectedEventId(id);
    window.requestAnimationFrame(() => {
      const row = document.getElementById(eventStepId(id));
      row?.scrollIntoView({ block: "nearest", behavior: "smooth" });
      row?.focus();
    });
  }

  return (
    <div className="events-stack">
      {stale ? <EventStaleBanner /> : null}

      <div className="events-tabs" role="tablist" aria-label="Events views">
        <button
          type="button"
          role="tab"
          aria-selected={activeView === "story"}
          className={activeView === "story" ? "selected" : ""}
          onClick={() => setActiveView("story")}
        >
          <Workflow size={16} aria-hidden="true" />
          Turn Reconstruction
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={activeView === "raw"}
          className={activeView === "raw" ? "selected" : ""}
          onClick={() => setActiveView("raw")}
        >
          <ListFilter size={16} aria-hidden="true" />
          Raw stream
        </button>
      </div>

      {activeView === "story" ? (
        <section className="event-reconstruction" aria-labelledby="turn-title">
          <div className="section-heading">
            <div>
              <div className="turn-context">
                <span aria-label="Turn pipeline">Pipeline {turn.pipeline}</span>
                <span>{turn.conversation}</span>
              </div>
              <h2 id="turn-title">Turn Reconstruction</h2>
            </div>
            <StatusPill
              label="Turn"
              value={turn.status}
              tone={turn.status === "failed" ? "caution" : "neutral"}
            />
          </div>

          <section className="stage-timeline" aria-labelledby="stage-title">
            <div className="section-heading compact">
              <div>
                <p className="eyebrow">Visual grouping</p>
                <h3 id="stage-title">Stage Timeline</h3>
              </div>
              <StatusPill
                label="Stages"
                value={turn.groups.length.toString()}
                tone="neutral"
              />
            </div>
            <div className="stage-track">
              {turn.groups.map((group) => (
                <article
                  aria-label={`${group.component} stage`}
                  className={`stage-group ${group.status}`}
                  key={group.component}
                  role="group"
                >
                  <div className="stage-group-header">
                    <strong>{displayStageComponent(group.component)}</strong>
                    <span>{group.durationLabel}</span>
                  </div>
                  <div className="stage-event-chips">
                    {group.steps.map((step) => (
                      <button
                        key={step.id}
                        type="button"
                        className={storyEventClassName("stage-event-chip", {
                          selected: selectedEventId === step.id,
                          error: step.error,
                        })}
                        aria-describedby={
                          step.error ? eventErrorId(step.id) : undefined
                        }
                        onClick={() => selectStoryEvent(step.id)}
                      >
                        {displayEventType(step.type)}
                      </button>
                    ))}
                  </div>
                </article>
              ))}
            </div>
          </section>

          <ol className="event-story">
            {turn.steps.map((step) => (
              <li
                id={eventStepId(step.id)}
                className={storyEventClassName("event-step", {
                  selected: selectedEventId === step.id,
                  error: step.error,
                })}
                key={step.id}
                tabIndex={-1}
                aria-label={`${step.type} ${step.component}${step.error ? " error" : ""}`}
              >
                <div className="event-meta">
                  <strong>{step.type}</strong>
                  <span>{step.component}</span>
                  <time dateTime={step.at}>{formatTime(step.at)}</time>
                </div>
                {step.detail ? (
                  <p id={step.error ? eventErrorId(step.id) : undefined}>
                    {step.detail}
                  </p>
                ) : null}
              </li>
            ))}
          </ol>
        </section>
      ) : (
        <section className="raw-events" aria-labelledby="raw-events-title">
          <div className="section-heading">
            <div>
              <p className="eyebrow">Secondary inspection</p>
              <h2 id="raw-events-title">Raw stream</h2>
            </div>
            <StatusPill
              label="Visible"
              value={rawEvents.length.toString()}
              tone="neutral"
            />
          </div>

          <label className="field raw-filter">
            <span>Filter events</span>
            <input
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
            />
          </label>

          <div className="raw-event-list">
            {rawEvents.map((envelope) => (
              <article className="raw-event" key={envelope.id}>
                <strong>{envelope.event.type}</strong>
                <span>{formatTime(envelope.at)}</span>
                <code>{JSON.stringify(envelope.event)}</code>
              </article>
            ))}
          </div>
        </section>
      )}
    </div>
  );
}

function PipelinesPanel({
  providerDefinitions,
  pipelineViews,
  readOnly,
  onPipelineStored,
  onPipelineValidate,
  onPipelineTest,
}: {
  providerDefinitions: readonly ProviderDefinition[];
  pipelineViews: readonly PipelineView[];
  readOnly: boolean;
  onPipelineStored: (graph: PipelineGraph, order: string[]) => void;
  onPipelineValidate: PipelineValidator;
  onPipelineTest: PipelineTester;
}) {
  const [selectedName, setSelectedName] = useState(
    pipelineViews[0]?.graph.name ?? "",
  );
  const selectedView =
    pipelineViews.find((view) => view.graph.name === selectedName) ??
    pipelineViews[0] ??
    null;
  const [draftsByPipeline, setDraftsByPipeline] = useState<
    Record<string, PipelineEditorDraftState>
  >(() => initializePipelineDrafts(pipelineViews));
  const selectedDraftState =
    selectedView && draftsByPipeline[selectedView.graph.name]
      ? draftsByPipeline[selectedView.graph.name]
      : selectedView
        ? {
            draft: cloneGraph(selectedView.graph),
            history: [],
            validation: null,
            notice: null,
          }
        : null;
  const draft = selectedDraftState?.draft ?? null;
  const history = selectedDraftState?.history ?? [];
  const validation = selectedDraftState?.validation ?? null;
  const notice = selectedDraftState?.notice ?? null;
  const [selectedNodeByPipeline, setSelectedNodeByPipeline] = useState<
    Record<string, string>
  >(
    () =>
      Object.fromEntries(
        pipelineViews.map((view) => [
          view.graph.name,
          view.graph.nodes[0]?.id ?? "",
        ]),
      ) as Record<string, string>,
  );
  const [pendingPipelineName, setPendingPipelineName] = useState<string | null>(
    null,
  );
  const [graphZoom, setGraphZoom] = useState(100);
  const [graphMotionEnabled, setGraphMotionEnabled] = useState(true);
  const [toolProviderMenuOpen, setToolProviderMenuOpen] = useState(false);
  const [draggingAugment, setDraggingAugment] =
    useState<AugmentDragState | null>(null);
  const [dragPreviewPositions, setDragPreviewPositions] = useState<
    Record<string, OrbitPosition>
  >({});
  const activeDragPositionRef = useRef<OrbitPosition | null>(null);
  const selectedNodeId =
    (selectedName ? selectedNodeByPipeline[selectedName] : "") ??
    draft?.nodes[0]?.id ??
    "";
  const [editingNodeId, setEditingNodeId] = useState<string | null>(null);
  const selectedNode =
    draft?.nodes.find((node) => node.id === selectedNodeId) ??
    draft?.nodes[0] ??
    null;
  const graphFlow = draft ? pipelineGraphFlow(draft) : null;
  const pendingPipeline = pendingPipelineName
    ? (pipelineViews.find((view) => view.graph.name === pendingPipelineName) ??
      null)
    : null;
  const hasUnsavedEdits = history.length > 0;
  const configuredToolProviders = providerDefinitions.filter(
    (provider) => provider.kind === "tool",
  );
  const unusedToolProviders = draft
    ? configuredToolProviders.filter(
        (provider) =>
          !draft.nodes.some(
            (node) => node.kind === "tool" && node.provider === provider.id,
          ),
      )
    : [];

  function ensurePipelineDraft(
    current: Record<string, PipelineEditorDraftState>,
    view: PipelineView,
  ): Record<string, PipelineEditorDraftState> {
    if (current[view.graph.name]) {
      return current;
    }

    return {
      ...current,
      [view.graph.name]: {
        draft: cloneGraph(view.graph),
        history: [],
        validation: null,
        notice: null,
      },
    };
  }

  function switchToPipeline(view: PipelineView) {
    setSelectedName(view.graph.name);
    setDraftsByPipeline((current) => ensurePipelineDraft(current, view));
    setSelectedNodeByPipeline((current) => ({
      ...current,
      [view.graph.name]:
        current[view.graph.name] ?? view.graph.nodes[0]?.id ?? "",
    }));
    setEditingNodeId(null);
    setPendingPipelineName(null);
  }

  function selectPipeline(view: PipelineView) {
    if (view.graph.name === selectedName) {
      return;
    }

    if (hasUnsavedEdits && draft) {
      setPendingPipelineName(view.graph.name);
      setDraftsByPipeline((current) => ({
        ...current,
        [draft.name]: {
          draft,
          history,
          validation,
          notice: `Save or discard changes before switching to ${view.graph.name}`,
        },
      }));
      return;
    }

    switchToPipeline(view);
  }

  function setSelectedNodeId(nodeId: string) {
    setSelectedNodeByPipeline((current) => ({
      ...current,
      [selectedName]: nodeId,
    }));
  }

  function updateCurrentDraftState(
    update: (current: PipelineEditorDraftState) => PipelineEditorDraftState,
  ) {
    if (!selectedDraftState) {
      return;
    }

    setDraftsByPipeline((current) => ({
      ...current,
      [selectedDraftState.draft.name]: update(selectedDraftState),
    }));
  }

  function resetPipelineDraft(view: PipelineView) {
    setDraftsByPipeline((current) => ({
      ...current,
      [view.graph.name]: {
        draft: cloneGraph(view.graph),
        history: [],
        validation: null,
        notice: null,
      },
    }));
    setSelectedNodeByPipeline((current) => ({
      ...current,
      [view.graph.name]: view.graph.nodes[0]?.id ?? "",
    }));
  }

  function discardAndSwitch() {
    if (!selectedView || !pendingPipeline) {
      return;
    }

    resetPipelineDraft(selectedView);
    switchToPipeline(pendingPipeline);
  }

  function switchKeepingDraft() {
    if (!pendingPipeline) {
      return;
    }

    updateCurrentDraftState((current) => ({
      ...current,
      notice: null,
    }));
    switchToPipeline(pendingPipeline);
  }

  async function saveAndSwitch() {
    if (!pendingPipeline) {
      return;
    }

    const saved = await saveCurrentDraft();
    if (saved) {
      switchToPipeline(pendingPipeline);
    }
  }

  function cancelPipelineSwitch() {
    setPendingPipelineName(null);
    updateCurrentDraftState((current) => ({
      ...current,
      notice: null,
    }));
  }

  function markCurrentDraftNotice(message: string) {
    updateCurrentDraftState((current) => ({
      ...current,
      notice: message,
    }));
  }

  function setCurrentValidation(result: PipelineValidationResult | null) {
    updateCurrentDraftState((current) => ({
      ...current,
      validation: result,
    }));
  }

  function updateCurrentDraftAfterSave(message: string) {
    updateCurrentDraftState((current) => ({
      ...current,
      history: [],
      notice: message,
    }));
  }

  function replaceCurrentDraft(
    nextDraft: PipelineGraph,
    nextHistory: PipelineGraph[],
  ) {
    setDraftsByPipeline((current) => ({
      ...current,
      [nextDraft.name]: {
        draft: nextDraft,
        history: nextHistory,
        validation: null,
        notice: null,
      },
    }));
  }

  function applyDraftEdit(edit: (graph: PipelineGraph) => PipelineGraph) {
    if (!draft) {
      return;
    }

    replaceCurrentDraft(edit(cloneGraph(draft)), [
      ...history,
      cloneGraph(draft),
    ]);
  }

  function startAugmentDrag(
    nodeId: string,
    position: OrbitPosition,
    event: ReactPointerEvent<HTMLElement>,
  ) {
    if (readOnly) {
      return;
    }

    event.preventDefault();
    event.stopPropagation();
    setSelectedNodeId(nodeId);
    activeDragPositionRef.current = position;
    setDraggingAugment({
      nodeId,
      startClientX: event.clientX,
      startClientY: event.clientY,
      startPosition: position,
    });
  }

  useEffect(() => {
    if (!draggingAugment) {
      return;
    }

    const activeDrag = draggingAugment;

    function pointerPosition(event: PointerEvent): OrbitPosition {
      return {
        x: clampOrbitCoordinate(
          activeDrag.startPosition.x + event.clientX - activeDrag.startClientX,
        ),
        y: clampOrbitCoordinate(
          activeDrag.startPosition.y + event.clientY - activeDrag.startClientY,
        ),
      };
    }

    function handlePointerMove(event: PointerEvent) {
      const nextPosition = pointerPosition(event);
      activeDragPositionRef.current = nextPosition;
      setDragPreviewPositions((current) => ({
        ...current,
        [activeDrag.nodeId]: nextPosition,
      }));
    }

    function handlePointerUp(event: PointerEvent) {
      const nextPosition =
        activeDragPositionRef.current ?? pointerPosition(event);
      setDraggingAugment(null);
      setDragPreviewPositions((current) => {
        const next = { ...current };
        delete next[activeDrag.nodeId];
        return next;
      });
      setDraftsByPipeline((current) => {
        const currentState = current[selectedName];
        if (!currentState) {
          return current;
        }

        const previousDraft = cloneGraph(currentState.draft);
        const nextDraft = {
          ...previousDraft,
          nodes: previousDraft.nodes.map((node) =>
            node.id === activeDrag.nodeId
              ? {
                  ...node,
                  config: withOrbitPosition(node.config, nextPosition),
                }
              : node,
          ),
        };
        return {
          ...current,
          [nextDraft.name]: {
            draft: nextDraft,
            history: [...currentState.history, previousDraft],
            validation: null,
            notice: null,
          },
        };
      });
      activeDragPositionRef.current = null;
    }

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
    };
  }, [draggingAugment, selectedName]);

  function addReasoningAugment({
    id,
    kind,
    provider,
  }: {
    id: string;
    kind: NodeKind;
    provider: string;
  }) {
    applyDraftEdit((graph) => {
      if (graph.nodes.some((node) => node.id === id)) {
        return graph;
      }

      return {
        ...graph,
        nodes: [
          ...graph.nodes,
          {
            id,
            kind,
            provider,
            config: withOrbitPosition(
              undefined,
              nextAugmentOrbitPosition(graph, "llm"),
            ),
          },
        ],
        edges: [...graph.edges, { from: id, to: "llm" }],
      };
    });
  }

  function addToolProviderNode(provider: ProviderDefinition) {
    if (!draft) {
      return;
    }

    addReasoningAugment({
      id: uniqueNodeId(draft, toolNodeIdBase(provider.id)),
      kind: "tool",
      provider: provider.id,
    });
    setToolProviderMenuOpen(false);
  }

  function updateNodeProvider(nodeId: string, providerId: string) {
    applyDraftEdit((graph) => ({
      ...graph,
      nodes: graph.nodes.map((node) => {
        if (node.id !== nodeId) {
          return node;
        }

        return {
          ...node,
          provider: providerId,
          config: undefined,
        };
      }),
    }));
  }

  function deleteNode(nodeId: string) {
    applyDraftEdit((graph) => {
      const target = graph.nodes.find((node) => node.id === nodeId);
      if (!target || graph.nodes.length <= 1) {
        return graph;
      }

      const incoming = graph.edges.filter((edge) => edge.to === nodeId);
      const outgoing = graph.edges.filter((edge) => edge.from === nodeId);
      const bridgedEdges = incoming.flatMap((fromEdge) =>
        outgoing
          .filter((toEdge) => fromEdge.from !== toEdge.to)
          .map((toEdge) => ({ from: fromEdge.from, to: toEdge.to })),
      );
      const remainingEdges = graph.edges.filter(
        (edge) => edge.from !== nodeId && edge.to !== nodeId,
      );
      const edgeKeys = new Set(
        remainingEdges.map(
          (edge) => `${edge.from}->${edge.to}:${edge.port ?? ""}`,
        ),
      );

      return {
        ...graph,
        nodes: graph.nodes.filter((node) => node.id !== nodeId),
        edges: [
          ...remainingEdges,
          ...bridgedEdges.filter((edge) => {
            const key = `${edge.from}->${edge.to}:`;
            if (edgeKeys.has(key)) {
              return false;
            }
            edgeKeys.add(key);
            return true;
          }),
        ],
      };
    });

    if (selectedNodeId === nodeId && draft) {
      setSelectedNodeId(
        draft.nodes.find((node) => node.id !== nodeId)?.id ?? "",
      );
    }
    setEditingNodeId((current) => (current === nodeId ? null : current));
  }

  function deleteSelectedNode() {
    if (selectedNode) {
      deleteNode(selectedNode.id);
    }
  }

  function addToolNode() {
    if (configuredToolProviders.length > 1) {
      setToolProviderMenuOpen((open) => !open);
      return;
    }

    const provider = unusedToolProviders[0];
    if (provider) {
      addToolProviderNode(provider);
      return;
    }

    addReasoningAugment({
      id: "confirm",
      kind: "tool",
      provider: "builtin.confirm",
    });
  }

  function addMemoryNode() {
    if (!draft) {
      return;
    }

    addReasoningAugment({
      id: uniqueNodeId(draft, "memory"),
      kind: "memory",
      provider: "builtin.memory",
    });
  }

  function addConfiguredToolProvider(providerId: string) {
    const provider = unusedToolProviders.find(
      (candidate) => candidate.id === providerId,
    );
    if (!provider) {
      markCurrentDraftNotice("No unused configured tool providers");
      setToolProviderMenuOpen(false);
      return;
    }

    addToolProviderNode(provider);
  }

  function undoLastEdit() {
    const previous = history.at(-1);
    if (!previous) {
      return;
    }

    replaceCurrentDraft(previous, history.slice(0, -1));
  }

  async function validateDraft() {
    if (!draft) {
      return;
    }

    try {
      const result = await onPipelineValidate(draft);
      updateCurrentDraftState((current) => ({
        ...current,
        validation: result,
        notice: null,
      }));
    } catch (caught) {
      setCurrentValidation({
        ok: false,
        message:
          caught instanceof Error ? caught.message : "Unable to validate graph",
      });
    }
  }

  async function saveCurrentDraft(): Promise<boolean> {
    if (!draft || validation?.ok !== true) {
      return false;
    }

    try {
      await onPipelineStored(draft, validation.order);
      updateCurrentDraftAfterSave(`Saved graph for ${draft.name}`);
      return true;
    } catch (caught) {
      markCurrentDraftNotice(
        caught instanceof Error ? caught.message : "Unable to save graph",
      );
      return false;
    }
  }

  async function saveDraft() {
    await saveCurrentDraft();
  }

  async function runTestTurn() {
    if (!draft) {
      return;
    }

    try {
      const result = await onPipelineValidate(draft);
      updateCurrentDraftState((current) => ({
        ...current,
        validation: result,
        notice: null,
      }));
      if (!result.ok) {
        return;
      }
      if (hasUnsavedEdits) {
        await onPipelineStored(draft, result.order);
        updateCurrentDraftAfterSave(`Saved graph for ${draft.name}`);
      }
      const message = await onPipelineTest(draft.name);
      markCurrentDraftNotice(message);
    } catch (caught) {
      markCurrentDraftNotice(
        caught instanceof Error ? caught.message : "Unable to run test turn",
      );
    }
  }

  if (!draft || !selectedView) {
    return (
      <div className="overview-empty" role="status">
        <Workflow size={18} aria-hidden="true" />
        <span>No stored pipeline graphs</span>
      </div>
    );
  }
  const draftNodeCount = draft.nodes.length;

  function renderNodeCard({
    node,
    compact = false,
  }: {
    node: PipelineNode;
    compact?: boolean;
  }) {
    const componentKind = componentKindForNode(node);
    const providerChoices = providerDefinitionsForNode(
      providerDefinitions,
      node,
    );

    return (
      <article
        aria-label={`${node.id} ${componentKind}`}
        className={`graph-node kind-${node.kind} ${compact ? "compact" : ""} ${
          !compact && node.kind !== "llm" ? "linear" : ""
        } ${selectedNode?.id === node.id ? "selected" : ""}`}
        role="group"
        onClick={() => setSelectedNodeId(node.id)}
      >
        <div className="graph-node-header">
          {!readOnly ? (
            <div className="node-card-actions">
              <button
                className="icon-action"
                type="button"
                aria-label={`Edit provider for ${node.id}`}
                onClick={(event) => {
                  event.stopPropagation();
                  setSelectedNodeId(node.id);
                  setEditingNodeId((current) =>
                    current === node.id ? null : node.id,
                  );
                }}
              >
                <Settings size={16} aria-hidden="true" />
              </button>
              <button
                className="icon-action danger"
                type="button"
                aria-label={`Delete ${node.id}`}
                disabled={draftNodeCount <= 1}
                onClick={(event) => {
                  event.stopPropagation();
                  deleteNode(node.id);
                }}
              >
                <Trash2 size={16} aria-hidden="true" />
              </button>
            </div>
          ) : null}
        </div>
        <strong className="node-label" title={node.id}>
          {node.id}
        </strong>
        <p className="node-provider-label" title={node.provider}>
          {node.provider}
        </p>
        {editingNodeId === node.id ? (
          <label className="field node-provider-select">
            <span>Provider</span>
            <select
              aria-label={`Provider for ${node.id}`}
              value={node.provider}
              onChange={(event) =>
                updateNodeProvider(node.id, event.target.value)
              }
            >
              {providerChoices.map((provider) => (
                <option value={provider.id} key={provider.id}>
                  {provider.label} ({provider.id})
                </option>
              ))}
            </select>
          </label>
        ) : null}
      </article>
    );
  }

  const atomFlowNodes = graphFlow?.mainNodes ?? [];
  const atomEdges = graphFlow?.mainEdges ?? [];
  const atomNodeById = new Map(atomFlowNodes.map((node) => [node.id, node]));

  function attachesFlowLinkToTarget(edge: PipelineEdge) {
    return atomNodeById.get(edge.from)?.kind === "llm";
  }

  function renderAtomFlowLink(edge: PipelineEdge, attachedToTarget = false) {
    return (
      <span
        className={`atom-flow-link ${
          attachedToTarget ? "attached-to-target" : ""
        }`}
        aria-label={`${edge.from} to ${edge.to}`}
        key={`${edge.from}-${edge.to}-${edge.port ?? "default"}`}
      >
        <ArrowRight size={18} aria-hidden="true" />
      </span>
    );
  }

  return (
    <div className="pipelines-stack">
      <section className="pipeline-toolbar" aria-label="Stored pipelines">
        <div className="pipeline-toolbar-main">
          <div>
            <p className="eyebrow">Advanced configuration</p>
            <h2>Graph Editor</h2>
          </div>
          <div className="pipeline-picker" aria-label="Pipeline selector">
            <div className="pipeline-picker-label">
              <span>Pipeline</span>
              <strong>{pipelineViews.length}</strong>
            </div>
            <div className="pipeline-selector">
              {pipelineViews.map((view) => (
                <button
                  key={view.graph.name}
                  type="button"
                  className={view.graph.name === draft.name ? "selected" : ""}
                  onClick={() => selectPipeline(view)}
                >
                  {view.graph.name}
                </button>
              ))}
            </div>
          </div>
        </div>
        {!readOnly ? (
          <div
            className="graph-actions"
            role="toolbar"
            aria-label="Graph editor actions"
          >
            <div className="graph-action-group compact">
              <button
                className="icon-action"
                type="button"
                aria-label="Undo last edit"
                title="Undo last edit"
                disabled={history.length === 0}
                onClick={undoLastEdit}
              >
                <RotateCcw size={17} aria-hidden="true" />
              </button>
              {history.length > 0 ? (
                <span className="edit-badge">
                  {history.length} unsaved{" "}
                  {history.length === 1 ? "edit" : "edits"}
                </span>
              ) : null}
            </div>
            <div
              className="graph-action-group add-group"
              aria-label="Add nodes"
            >
              <button
                className="secondary-action compact-action"
                type="button"
                aria-label="Add tool node"
                onClick={addToolNode}
                disabled={
                  configuredToolProviders.length > 0 &&
                  unusedToolProviders.length === 0
                }
              >
                <Plus size={16} aria-hidden="true" />
                Tool into LLM
              </button>
              {configuredToolProviders.length > 1 && toolProviderMenuOpen ? (
                <div className="provider-kind-menu inline" role="menu">
                  {unusedToolProviders.length > 0 ? (
                    unusedToolProviders.map((provider) => (
                      <button
                        key={provider.id}
                        type="button"
                        role="menuitem"
                        onClick={() => addConfiguredToolProvider(provider.id)}
                      >
                        {provider.label}
                      </button>
                    ))
                  ) : (
                    <span className="provider-kind-empty">
                      No unused configured tool providers
                    </span>
                  )}
                </div>
              ) : null}
              {configuredToolProviders.length > 0 &&
              unusedToolProviders.length === 0 ? (
                <span className="provider-kind-empty">
                  No unused configured tool providers
                </span>
              ) : null}
              <button
                className="secondary-action compact-action"
                type="button"
                aria-label="Add memory node"
                onClick={addMemoryNode}
              >
                <Plus size={16} aria-hidden="true" />
                Memory into LLM
              </button>
            </div>
            <div className="graph-action-group">
              <button
                className="secondary-action compact-action"
                type="button"
                aria-label="Validate Graph"
                onClick={validateDraft}
              >
                <CircleCheck size={16} aria-hidden="true" />
                Validate
              </button>
              <button
                className="secondary-action compact-action"
                type="button"
                aria-label="Run test turn"
                onClick={runTestTurn}
              >
                <Play size={16} aria-hidden="true" />
                Test
              </button>
              <button
                className="primary-action compact-action"
                type="button"
                aria-label="Save Graph"
                disabled={validation?.ok !== true}
                onClick={saveDraft}
              >
                <Save size={16} aria-hidden="true" />
                Save
              </button>
            </div>
            <button
              className="icon-action danger subtle-danger"
              type="button"
              aria-label="Delete selected node"
              title="Delete selected node"
              disabled={!selectedNode}
              onClick={deleteSelectedNode}
            >
              <Trash2 size={17} aria-hidden="true" />
            </button>
          </div>
        ) : null}
      </section>

      {pendingPipeline ? (
        <section
          className="dirty-switch-banner"
          aria-label="Unsaved pipeline changes"
        >
          <CircleAlert size={18} aria-hidden="true" />
          <div>
            <strong>{draft.name} has unsaved edits</strong>
            <span>
              Switching to {pendingPipeline.graph.name} can keep, save, or
              discard this draft.
            </span>
          </div>
          <div className="dirty-switch-actions">
            <button
              className="primary-action compact-action"
              type="button"
              disabled={validation?.ok !== true}
              onClick={saveAndSwitch}
            >
              <Save size={16} aria-hidden="true" />
              Save current and switch
            </button>
            <button
              className="secondary-action compact-action"
              type="button"
              onClick={switchKeepingDraft}
            >
              Switch without saving
            </button>
            <button
              className="danger-action compact"
              type="button"
              onClick={discardAndSwitch}
            >
              <Trash2 size={16} aria-hidden="true" />
              Discard changes
            </button>
            <button
              className="secondary-action compact-action"
              type="button"
              onClick={cancelPipelineSwitch}
            >
              Cancel
            </button>
          </div>
        </section>
      ) : null}

      {readOnly ? (
        <section className="stale-banner" aria-label="Small screen graph mode">
          <CircleAlert size={18} aria-hidden="true" />
          <div>
            <strong>Read-only on small screens</strong>
            <span>Use a desktop viewport for graph editing controls</span>
          </div>
        </section>
      ) : null}

      <div className="pipeline-editor-grid">
        <section className="graph-surface" aria-label="Pipeline graph">
          <div
            className={`pipeline-atom-canvas ${
              graphMotionEnabled ? "motion-enabled" : ""
            }`}
          >
            <div className="atom-stage-labels" aria-hidden="true">
              <span>Input</span>
              <span />
              <span>Output</span>
            </div>
            <div
              className="pipeline-atom-map"
              style={{ transform: `scale(${graphZoom / 100})` }}
            >
              {atomFlowNodes.map((node) => {
                const outgoingEdges = atomEdges.filter(
                  (edge) =>
                    edge.from === node.id && !attachesFlowLinkToTarget(edge),
                );
                const incomingAttachedEdges = atomEdges.filter(
                  (edge) =>
                    edge.to === node.id && attachesFlowLinkToTarget(edge),
                );
                const spokes = graphFlow?.spokesByTarget.get(node.id) ?? [];
                return (
                  <Fragment key={node.id}>
                    <div
                      className={`atom-flow-item ${
                        node.kind === "llm" ? "core-flow-item" : ""
                      }`}
                    >
                      {incomingAttachedEdges.map((edge) =>
                        renderAtomFlowLink(edge, true),
                      )}
                      <div
                        className={
                          node.kind === "llm" ? "atom-core-wrap" : "atom-stage"
                        }
                      >
                        {node.kind === "llm" ? (
                          <>
                            <div
                              className="atom-orbit-ring"
                              aria-hidden="true"
                            />
                            <div
                              className={`atom-orbitals ${
                                graphMotionEnabled ? "motion-enabled" : ""
                              }`}
                              aria-label={`${node.id} augments`}
                            >
                              {spokes.map((spoke, spokeIndex) => {
                                const position =
                                  dragPreviewPositions[spoke.node.id] ??
                                  orbitPositionForNode(spoke.node, spokeIndex);
                                const slot = spokeIndex + 1;
                                return (
                                  <div
                                    aria-label={`Move ${spoke.node.id} augment`}
                                    className="atom-orbital"
                                    data-orbit-slot={slot.toString()}
                                    draggable={false}
                                    key={spoke.node.id}
                                    onPointerDown={(event) =>
                                      startAugmentDrag(
                                        spoke.node.id,
                                        position,
                                        event,
                                      )
                                    }
                                    style={
                                      {
                                        "--orbit-x": `${position.x}px`,
                                        "--orbit-y": `${position.y}px`,
                                        "--orbit-start-x": `${position.x}px`,
                                        "--orbit-start-y": `${position.y}px`,
                                      } as CSSProperties
                                    }
                                  >
                                    {renderNodeCard({
                                      node: spoke.node,
                                      compact: true,
                                    })}
                                  </div>
                                );
                              })}
                              {graphMotionEnabled ? (
                                <>
                                  <span className="atom-motion-particle particle-1" />
                                  <span className="atom-motion-particle particle-2" />
                                  <span className="atom-motion-particle particle-3" />
                                </>
                              ) : null}
                            </div>
                            <span className="atom-label">Reasoning core</span>
                          </>
                        ) : null}
                        <div
                          className={
                            node.kind === "llm" ? "reasoning-atom" : ""
                          }
                        >
                          {renderNodeCard({ node })}
                        </div>
                      </div>
                      {outgoingEdges.map((edge) => renderAtomFlowLink(edge))}
                    </div>
                  </Fragment>
                );
              })}
            </div>
            <div
              className="graph-canvas-controls"
              role="toolbar"
              aria-label="Graph canvas controls"
            >
              <button
                className="icon-action"
                type="button"
                aria-label="Zoom in graph"
                onClick={() =>
                  setGraphZoom((current) => Math.min(current + 10, 140))
                }
              >
                <Plus size={16} aria-hidden="true" />
              </button>
              <span aria-label="Graph zoom level">{graphZoom}%</span>
              <button
                className="icon-action"
                type="button"
                aria-label="Zoom out graph"
                onClick={() =>
                  setGraphZoom((current) => Math.max(current - 10, 70))
                }
              >
                <Minus size={16} aria-hidden="true" />
              </button>
              <span className="toolbar-divider" aria-hidden="true" />
              <button
                className="icon-action"
                type="button"
                aria-label="Reset graph view"
                onClick={() => setGraphZoom(100)}
              >
                <Maximize2 size={15} aria-hidden="true" />
              </button>
              <button
                className={`icon-action ${graphMotionEnabled ? "selected" : ""}`}
                type="button"
                aria-label="Toggle graph motion"
                onClick={() => setGraphMotionEnabled((current) => !current)}
              >
                <Play size={15} aria-hidden="true" />
              </button>
            </div>
          </div>
        </section>

        {validation || notice ? (
          <section
            className="pipeline-editor-status"
            aria-label="Pipeline graph status"
          >
            {validation ? (
              <p className={validation.ok ? "validation-ok" : "form-error"}>
                {validation.ok ? "Validation passed" : validation.message}
              </p>
            ) : null}

            {notice ? <p className="panel-notice">{notice}</p> : null}
          </section>
        ) : null}
      </div>
    </div>
  );
}

function ComponentConfigFields({
  component,
  config,
  readOnly,
  onChange,
}: {
  component: PipelineComponentDescriptor;
  config: Record<string, unknown>;
  readOnly: boolean;
  onChange: (
    field: string,
    property: ComponentConfigProperty,
    value: string | boolean,
  ) => void;
}) {
  const required = new Set(component.schema.required);

  return (
    <fieldset
      className="component-config-fields"
      disabled={readOnly}
      aria-label={`${component.label} configuration`}
    >
      {Object.entries(component.schema.properties).map(([field, property]) => {
        const requiredLabel = required.has(field) ? " required" : "";
        const label = `${field}${requiredLabel}`;
        if (property.type === "boolean") {
          return (
            <label className="check-row" key={field}>
              <input
                type="checkbox"
                checked={config[field] === true}
                onChange={(event) =>
                  onChange(field, property, event.target.checked)
                }
              />
              <span>{field}</span>
            </label>
          );
        }

        return (
          <label className="field" key={field}>
            <span>{label}</span>
            <input
              type={property.format === "url" ? "url" : "text"}
              pattern={property.pattern}
              required={required.has(field)}
              value={typeof config[field] === "string" ? config[field] : ""}
              onChange={(event) =>
                onChange(field, property, event.target.value)
              }
            />
          </label>
        );
      })}
    </fieldset>
  );
}

function componentForNode(
  catalog: PipelineComponentCatalog,
  node: PipelineNode,
): PipelineComponentDescriptor | null {
  const exact = catalog.components.find(
    (component) =>
      component.id === node.provider && component.kind === node.kind,
  );
  if (exact) {
    return exact;
  }

  if (node.provider === "openai") {
    const openAiResponses = catalog.components.find(
      (component) =>
        component.id === "openai.responses" && component.kind === node.kind,
    );
    if (openAiResponses) {
      return openAiResponses;
    }
  }

  if (node.kind === "stt" && isWyomingProviderName(node.provider)) {
    const wyomingStt = catalog.components.find(
      (component) => component.id === "wyoming" && component.kind === "stt",
    );
    if (wyomingStt) {
      return wyomingStt;
    }
  }

  if (node.kind === "tts" && isWyomingProviderName(node.provider)) {
    const wyomingTts = catalog.components.find(
      (component) => component.id === "wyoming.tts" && component.kind === "tts",
    );
    if (wyomingTts) {
      return wyomingTts;
    }
  }

  return (
    catalog.components.find(
      (component) =>
        component.id === node.provider && component.kind === node.kind,
    ) ?? null
  );
}

function componentForProviderStatus(
  catalog: PipelineComponentCatalog,
  provider: ProviderStatus,
): PipelineComponentDescriptor | null {
  const nodeKind = nodeKindForProviderKind(provider.kind);
  return componentForNode(catalog, {
    id: provider.id,
    kind: nodeKind,
    provider: provider.id,
  });
}

function isWyomingProviderName(provider: string): boolean {
  const normalized = provider.toLowerCase();
  return (
    normalized.includes("wyoming") ||
    normalized.includes("piper") ||
    normalized.includes("whisper")
  );
}

function providerDefinitionsForNode(
  definitions: readonly ProviderDefinition[],
  node: PipelineNode,
): ProviderDefinition[] {
  const matching = definitions.filter(
    (provider) => provider.kind === node.kind,
  );
  if (matching.some((provider) => provider.id === node.provider)) {
    return matching;
  }

  return [
    ...matching,
    {
      id: node.provider,
      label: node.provider,
      kind: node.kind,
      component: node.provider,
      config: nodeConfigObject(node.config),
    },
  ];
}

function providerCardViews(
  definitions: readonly ProviderDefinition[],
  statuses: readonly ProviderStatus[],
): ProviderCardView[] {
  const cards = new Map<string, ProviderCardView>();

  for (const definition of definitions) {
    const kind = providerKindForNodeKind(definition.kind);
    if (!kind) {
      continue;
    }

    cards.set(definition.id, {
      id: definition.id,
      label: definition.label,
      kind,
      component: definition.component,
      definition,
      status: null,
    });
  }

  for (const status of statuses) {
    const existing = cards.get(status.id);
    cards.set(status.id, {
      id: status.id,
      label: existing?.label ?? status.id,
      kind: status.kind,
      component: existing?.component ?? null,
      definition: existing?.definition ?? null,
      status,
    });
  }

  return [...cards.values()].sort((left, right) =>
    left.id.localeCompare(right.id),
  );
}

function loadProviderDefinitions(
  catalog: PipelineComponentCatalog,
  pipelineViews: readonly PipelineView[],
  snapshot: OperatorStatusSnapshot | null,
): ProviderDefinition[] {
  try {
    const saved = window.localStorage.getItem(PROVIDER_DEFINITIONS_STORAGE_KEY);
    if (saved) {
      const parsed = JSON.parse(saved) as ProviderDefinition[];
      return mergeProviderDefinitions(
        defaultProviderDefinitions(catalog, pipelineViews, snapshot),
        parsed.filter(isProviderDefinition),
      );
    }
  } catch {
    // Bad local UI state should not block the console from loading.
  }

  return defaultProviderDefinitions(catalog, pipelineViews, snapshot);
}

function saveProviderDefinitions(definitions: readonly ProviderDefinition[]) {
  window.localStorage.setItem(
    PROVIDER_DEFINITIONS_STORAGE_KEY,
    JSON.stringify(definitions),
  );
}

function defaultProviderDefinitions(
  catalog: PipelineComponentCatalog,
  pipelineViews: readonly PipelineView[],
  snapshot: OperatorStatusSnapshot | null,
): ProviderDefinition[] {
  const fromGraphs = pipelineViews.flatMap((view) =>
    view.graph.nodes.flatMap((node) => {
      if (!providerKindForNodeKind(node.kind)) {
        return [];
      }

      const component = componentForNode(catalog, node);
      return [
        {
          id: node.provider,
          label: node.provider,
          kind: node.kind,
          component: component?.id ?? node.provider,
          config: nodeConfigObject(node.config),
        },
      ];
    }),
  );
  const fromStatus =
    snapshot?.providers.map((provider) => ({
      id: provider.id,
      label: provider.id,
      kind: nodeKindForProviderKind(provider.kind),
      component:
        componentForProviderStatus(catalog, provider)?.id ?? provider.id,
      config: {},
    })) ?? [];

  return mergeProviderDefinitions([], [...fromGraphs, ...fromStatus]);
}

function mergeProviderDefinitions(
  base: readonly ProviderDefinition[],
  incoming: readonly ProviderDefinition[],
): ProviderDefinition[] {
  const byId = new Map<string, ProviderDefinition>();
  for (const provider of [...base, ...incoming]) {
    byId.set(provider.id, cloneProviderDefinition(provider));
  }
  return [...byId.values()].sort((left, right) =>
    left.id.localeCompare(right.id),
  );
}

function cloneProviderDefinition(
  provider: ProviderDefinition,
): ProviderDefinition {
  return {
    ...provider,
    config: { ...provider.config },
  };
}

function isProviderDefinition(value: unknown): value is ProviderDefinition {
  if (!value || typeof value !== "object") {
    return false;
  }
  const provider = value as Partial<ProviderDefinition>;
  return (
    typeof provider.id === "string" &&
    typeof provider.label === "string" &&
    typeof provider.kind === "string" &&
    typeof provider.component === "string" &&
    !!provider.config &&
    typeof provider.config === "object" &&
    !Array.isArray(provider.config)
  );
}

function validateProviderDefinitionConfig(
  provider: ProviderDefinition,
  component: PipelineComponentDescriptor,
): PipelineValidationResult {
  const config = pruneEmptyConfig(provider.config);
  const missing = component.schema.required.filter((field) => {
    const value = config[field];
    return typeof value !== "string" || value.trim().length === 0;
  });
  if (missing.length > 0) {
    return {
      ok: false,
      message: `Missing required fields: ${missing.join(", ")}`,
    };
  }

  for (const [field, property] of Object.entries(component.schema.properties)) {
    if (!(field in config)) {
      continue;
    }
    const value = config[field];
    if (property.type === "string" && typeof value !== "string") {
      return { ok: false, message: `${field} must be a string` };
    }
    if (property.type === "boolean" && typeof value !== "boolean") {
      return { ok: false, message: `${field} must be a boolean` };
    }
  }

  return { ok: true, order: [] };
}

function updateConfigValue(
  config: Record<string, unknown>,
  field: string,
  property: ComponentConfigProperty,
  value: string | boolean,
): Record<string, unknown> {
  const next = { ...config };
  if (property.type === "boolean") {
    next[field] = value === true;
  } else if (typeof value === "string" && value.length > 0) {
    next[field] = value;
  } else {
    delete next[field];
  }
  return next;
}

function pruneEmptyConfig(
  config: Record<string, unknown>,
): Record<string, unknown> {
  return Object.fromEntries(
    Object.entries(config).filter(([, value]) => value !== "" && value != null),
  );
}

function nodeConfigObject(config: unknown): Record<string, unknown> {
  return config && typeof config === "object" && !Array.isArray(config)
    ? { ...(config as Record<string, unknown>) }
    : {};
}

function nodeKindForProviderKind(kind: ProviderKind): NodeKind {
  if (kind === "stt") {
    return "stt";
  }
  if (kind === "tts") {
    return "tts";
  }
  if (kind === "tool") {
    return "tool";
  }
  return "llm";
}

function providerKindForNodeKind(kind: NodeKind): ProviderKind | null {
  if (kind === "stt") {
    return "stt";
  }
  if (kind === "tts") {
    return "tts";
  }
  if (kind === "tool") {
    return "tool";
  }
  if (kind === "llm") {
    return "llm";
  }
  return null;
}

function EventStaleBanner() {
  return (
    <section className="stale-banner" aria-label="Stale state">
      <CircleAlert size={18} aria-hidden="true" />
      <div>
        <strong>Stale state</strong>
        <span>Last known event stream remains visible</span>
      </div>
      <span className="mini-badge">Reconnect refresh required</span>
    </section>
  );
}

type TurnStatus = "completed" | "failed" | "cancelled" | "running" | "degraded";

interface ReconstructedTurn {
  conversation: string;
  pipeline: string;
  status: TurnStatus;
  groups: ReconstructedGroup[];
  steps: ReconstructedStep[];
}

interface ReconstructedGroup {
  component: string;
  durationLabel: string;
  status: "ok" | "failed" | "running";
  steps: ReconstructedStep[];
}

interface ReconstructedStep {
  id: string;
  at: string;
  type: string;
  component: string;
  detail: string | null;
  error: boolean;
}

function reconstructServerTurn(snapshot: TurnSnapshot): ReconstructedTurn {
  const steps = snapshot.items.flatMap((item): ReconstructedStep[] => {
    if (item.kind === "spoken_segment") {
      return [
        {
          id: item.id,
          at: item.started_at,
          type:
            item.role === "assistant_preamble"
              ? "Assistant Preamble"
              : item.role === "tool_output"
                ? "Tool Spoken Output"
                : "Assistant Response",
          component: "synthesis",
          detail: item.text,
          error: false,
        },
      ];
    }

    return [
      {
        id: item.id,
        at: item.started_at,
        type: "Tool Batch",
        component: "tools",
        detail: `${item.calls.length} ${item.calls.length === 1 ? "call" : "calls"} from model round ${item.model_round}`,
        error: item.calls.some((call) =>
          ["failed", "denied"].includes(call.status),
        ),
      },
      ...item.calls.map((call) => ({
        id: `${item.id}-${call.id}`,
        at: item.started_at,
        type: "Tool Call",
        component: "tools",
        detail: `${call.name ?? call.id} / ${call.status}`,
        error: ["failed", "denied"].includes(call.status),
      })),
    ];
  });

  return {
    conversation: snapshot.conversation_id,
    pipeline: snapshot.pipeline_name,
    status: snapshot.status,
    groups: groupTurnSteps(steps),
    steps,
  };
}

function reconstructTurn(events: readonly EventEnvelope[]): ReconstructedTurn {
  const ordered = [...events].sort((left, right) =>
    left.at.localeCompare(right.at),
  );
  const steps = ordered
    .filter((envelope) => !isReconstructionBoundaryEvent(envelope.event))
    .map((envelope) => ({
      id: envelope.id,
      at: envelope.at,
      type: envelope.event.type,
      component: eventComponent(envelope.event),
      detail: eventDetail(envelope.event),
      error: isErrorEvent(envelope.event),
    }));

  return {
    conversation:
      ordered.find((envelope) => envelope.conversation)?.conversation ??
      "unknown conversation",
    pipeline:
      ordered.find((envelope) => envelope.pipeline)?.pipeline ??
      "unknown pipeline",
    status: turnStatus(ordered),
    groups: groupTurnSteps(steps),
    steps,
  };
}

function groupTurnSteps(
  steps: readonly ReconstructedStep[],
): ReconstructedGroup[] {
  const groups = new Map<string, ReconstructedStep[]>();
  for (const step of steps) {
    const component = visualStageComponent(step.component);
    if (!component) {
      continue;
    }

    groups.set(component, [...(groups.get(component) ?? []), step]);
  }

  return Array.from(groups.entries()).map(([component, groupedSteps]) => {
    const failed = groupedSteps.some(
      (step) => step.type === "StageFailed" || step.type === "ToolFailed",
    );
    return {
      component,
      durationLabel: durationLabel(groupedSteps),
      status: failed ? "failed" : "ok",
      steps: groupedSteps,
    };
  });
}

function visualStageComponent(component: string): string | null {
  if (component === "conversation") {
    return null;
  }
  if (["mic", "source", "capture"].includes(component)) {
    return "capture";
  }
  if (["stt", "transcription"].includes(component)) {
    return "transcription";
  }
  if (["llm", "reasoning"].includes(component)) {
    return "reasoning";
  }
  if (["tool", "tools"].includes(component)) {
    return "tools";
  }
  if (["tts", "speaker", "sink", "synthesis"].includes(component)) {
    return "synthesis";
  }
  return component;
}

function durationLabel(steps: readonly ReconstructedStep[]): string {
  const first = Date.parse(steps[0]?.at ?? "");
  const last = Date.parse(steps.at(-1)?.at ?? "");
  if (!Number.isFinite(first) || !Number.isFinite(last)) {
    return `${steps.length} events`;
  }

  const seconds = Math.max(0, (last - first) / 1000);
  return seconds === 0 ? "instant" : `${seconds.toFixed(1)}s`;
}

function turnStatus(events: readonly EventEnvelope[]): TurnStatus {
  if (events.some((envelope) => envelope.event.type === "StageFailed")) {
    return "failed";
  }
  if (events.some((envelope) => envelope.event.type === "ToolFailed")) {
    return "failed";
  }
  if (
    events.some((envelope) => envelope.event.type === "ConversationCancelled")
  ) {
    return "cancelled";
  }
  if (
    events.some((envelope) => envelope.event.type === "ConversationCompleted")
  ) {
    return "completed";
  }
  return "running";
}

function isErrorEvent(event: Event): boolean {
  return event.type === "StageFailed" || event.type === "ToolFailed";
}

function isReconstructionBoundaryEvent(event: Event): boolean {
  return (
    event.type === "SpokenSegmentStarted" || event.type === "ToolBatchStarted"
  );
}

function eventStepId(id: string): string {
  return `event-step-${id}`;
}

function eventErrorId(id: string): string {
  return `event-error-${id}`;
}

function storyEventClassName(
  base: string,
  state: { selected: boolean; error: boolean },
): string {
  return [base, state.error ? "error" : "", state.selected ? "selected" : ""]
    .filter(Boolean)
    .join(" ");
}

function eventComponent(event: Event): string {
  switch (event.type) {
    case "WakeWordDetected":
    case "WakeWordRejected":
    case "AudioStarted":
    case "AudioChunkReceived":
    case "AudioFinished":
      return "capture";
    case "SpeechPartial":
    case "SpeechFinal":
    case "SpeakerIdentified":
      return "transcription";
    case "ConversationStarted":
    case "TurnStarted":
    case "ConversationCancelled":
    case "ConversationCompleted":
      return "conversation";
    case "LlmRequestStarted":
    case "LlmToken":
    case "LlmFinished":
      return "reasoning";
    case "ToolRequested":
    case "ToolBatchStarted":
    case "ToolStarted":
    case "ToolConfirmationRequested":
    case "ToolCompleted":
    case "ToolFailed":
      return "tools";
    case "TtsStarted":
    case "SpokenSegmentStarted":
    case "AudioStreaming":
    case "TtsFinished":
      return "synthesis";
    case "StageFailed":
      return event.node;
  }
}

function eventDetail(event: Event): string | null {
  switch (event.type) {
    case "WakeWordDetected":
    case "WakeWordRejected":
      return `${event.phrase} (${Math.round(event.confidence * 100)}%)`;
    case "AudioStarted":
      return `${event.format.encoding}, ${event.format.sample_rate} Hz, ${event.format.channels} channel`;
    case "AudioChunkReceived":
    case "AudioStreaming":
      return `sequence ${event.sequence}, ${event.bytes} bytes`;
    case "AudioFinished":
    case "TtsFinished":
      return `${event.duration_ms} ms`;
    case "SpeechPartial":
    case "SpeechFinal":
      return event.text;
    case "SpeakerIdentified":
      return event.speaker ?? "unknown speaker";
    case "TurnStarted":
      return event.turn;
    case "ConversationCancelled":
      return event.reason;
    case "LlmRequestStarted":
      return event.model;
    case "LlmToken":
      return event.delta;
    case "LlmFinished":
      return event.reason;
    case "ToolRequested":
      return event.name;
    case "ToolBatchStarted":
      return `${event.calls.length} calls from model round ${event.model_round}`;
    case "ToolStarted":
    case "ToolCompleted":
      return event.call;
    case "ToolConfirmationRequested":
      return event.prompt;
    case "ToolFailed":
    case "StageFailed":
      return event.error;
    case "SpokenSegmentStarted":
      return event.text;
    case "ConversationStarted":
    case "ConversationCompleted":
    case "TtsStarted":
      return "boundary";
  }
}

function displayEventType(type: string): string {
  return type.replace(/([a-z])([A-Z])/g, "$1 $2");
}

function displayStageComponent(component: string): string {
  return [
    "capture",
    "transcription",
    "conversation",
    "reasoning",
    "tools",
    "synthesis",
  ].includes(component)
    ? component
    : `node: ${component}`;
}

function filterRawEvents(
  events: readonly EventEnvelope[],
  filter: string,
): readonly EventEnvelope[] {
  const needle = filter.trim().toLowerCase();
  if (!needle) {
    return events;
  }

  return events.filter((envelope) =>
    JSON.stringify(envelope).toLowerCase().includes(needle),
  );
}

function GuidedSetupPanel({
  onPipelineSaved,
}: {
  onPipelineSaved: (graph: PipelineGraph) => Promise<void>;
}) {
  const [pipelineName, setPipelineName] = useState("default");
  const [sttProvider, setSttProvider] = useState("whisper");
  const [llmProvider, setLlmProvider] = useState("openai");
  const [ttsProvider, setTtsProvider] = useState("piper");
  const [providerSettingsOpen, setProviderSettingsOpen] = useState(false);
  const [toolSetupSkipped, setToolSetupSkipped] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const name = pipelineName.trim();
    if (!name) {
      setError("Pipeline name is required");
      return;
    }
    if (!sttProvider.trim() || !llmProvider.trim() || !ttsProvider.trim()) {
      setError("Provider settings are required");
      return;
    }

    setError(null);
    setSaving(true);
    try {
      await onPipelineSaved(
        buildMinimalVoiceLoopGraph({
          name,
          sttProvider: sttProvider.trim(),
          llmProvider: llmProvider.trim(),
          ttsProvider: ttsProvider.trim(),
        }),
      );
    } catch (caught) {
      setError(
        caught instanceof Error ? caught.message : "Unable to save pipeline",
      );
    } finally {
      setSaving(false);
    }
  }

  return (
    <form className="guided-setup" onSubmit={save}>
      <section className="setup-band" aria-labelledby="guided-setup-title">
        <div className="section-heading">
          <div>
            <p className="eyebrow">Minimal voice loop</p>
            <h2 id="guided-setup-title">Guided Setup</h2>
          </div>
          <StatusPill
            label="Tools"
            value={toolSetupSkipped ? "skipped" : "optional"}
            tone="neutral"
          />
        </div>

        <div className="setup-grid">
          <label className="field">
            <span>Pipeline name</span>
            <input
              value={pipelineName}
              onChange={(event) => setPipelineName(event.target.value)}
            />
          </label>

          <div className="setup-actions">
            <button
              className="secondary-action"
              type="button"
              onClick={() => setProviderSettingsOpen((open) => !open)}
            >
              <Boxes size={17} aria-hidden="true" />
              Configure Providers
            </button>
            <button
              className="secondary-action"
              type="button"
              onClick={() => setToolSetupSkipped(true)}
            >
              <Plus size={17} aria-hidden="true" />
              Skip tool setup
            </button>
          </div>
        </div>

        {providerSettingsOpen ? (
          <div className="provider-settings" aria-label="Provider Settings">
            <label className="field">
              <span>Speech-to-text provider</span>
              <input
                value={sttProvider}
                onChange={(event) => setSttProvider(event.target.value)}
              />
            </label>
            <label className="field">
              <span>Language model provider</span>
              <input
                value={llmProvider}
                onChange={(event) => setLlmProvider(event.target.value)}
              />
            </label>
            <label className="field">
              <span>Text-to-speech provider</span>
              <input
                value={ttsProvider}
                onChange={(event) => setTtsProvider(event.target.value)}
              />
            </label>
          </div>
        ) : null}

        {toolSetupSkipped ? (
          <div className="calm-state">
            <CircleCheck size={18} aria-hidden="true" />
            <span>Tool setup skipped</span>
          </div>
        ) : null}

        {error ? <p className="form-error">{error}</p> : null}

        <button className="primary-action" type="submit" disabled={saving}>
          <CircleCheck size={17} aria-hidden="true" />
          {saving ? "Saving" : "Validate and Save"}
        </button>
      </section>
    </form>
  );
}

export function OverviewPanel({
  snapshot,
  eventPosture,
  onOpenFailureEvents,
}: {
  snapshot: OperatorStatusSnapshot | null;
  eventPosture: EventStreamPosture;
  onOpenFailureEvents?: () => void;
}) {
  if (!snapshot) {
    return (
      <div className="overview-empty" role="status">
        <Bell size={18} aria-hidden="true" />
        <span>Awaiting operator status snapshot</span>
      </div>
    );
  }

  const unhealthyPipelines = snapshot.pipelines.filter((pipeline) =>
    ["degraded", "unhealthy", "not_runnable"].includes(pipeline.health.state),
  );
  const providerWarnings = snapshot.providers.filter(
    (provider) => provider.state !== "proven",
  );
  const exceptions =
    snapshot.recent_failures.length +
    unhealthyPipelines.length +
    providerWarnings.length;
  const stale =
    snapshot.runtime.stale_state === "stale" ||
    eventPosture === "stale" ||
    eventPosture === "disconnected";

  return (
    <div className="overview-stack">
      {stale ? <StaleBanner snapshot={snapshot} /> : null}

      <section className="exception-band" aria-labelledby="exceptions-title">
        <div className="section-heading">
          <div>
            <p className="eyebrow">Exception-first</p>
            <h2 id="exceptions-title">Current Exceptions</h2>
          </div>
          <StatusPill
            label="Visible"
            value={exceptions.toString()}
            tone={exceptions > 0 ? "caution" : "neutral"}
          />
        </div>

        {exceptions === 0 ? (
          <div className="calm-state">
            <CircleCheck size={18} aria-hidden="true" />
            <span>No current exceptions</span>
          </div>
        ) : (
          <div
            className="exception-list"
            role="list"
            aria-labelledby="exceptions-title"
          >
            {snapshot.recent_failures.map((failure) => (
              <FailureItem
                key={`${failure.pipeline}-${failure.at}`}
                failure={failure}
                onOpenEvents={onOpenFailureEvents}
              />
            ))}
            {unhealthyPipelines.map((pipeline) => (
              <PipelineException
                key={pipeline.name}
                pipeline={pipeline}
                onOpenEvents={onOpenFailureEvents}
              />
            ))}
            {providerWarnings.map((provider) => (
              <ProviderWarning key={provider.id} provider={provider} />
            ))}
          </div>
        )}
      </section>

      <div className="overview-grid">
        <PipelineOverview pipelines={snapshot.pipelines} />
        <SatelliteOverview snapshot={snapshot} />
        <TurnOverview snapshot={snapshot} />
        <ProviderOverview providers={snapshot.providers} />
      </div>
    </div>
  );
}

function buildMinimalVoiceLoopGraph({
  name,
  sttProvider,
  llmProvider,
  ttsProvider,
}: {
  name: string;
  sttProvider: string;
  llmProvider: string;
  ttsProvider: string;
}): PipelineGraph {
  return {
    name,
    nodes: [
      { id: "mic", kind: "source", provider: "websocket" },
      { id: "stt", kind: "stt", provider: sttProvider },
      { id: "llm", kind: "llm", provider: llmProvider },
      { id: "tts", kind: "tts", provider: ttsProvider },
      { id: "speaker", kind: "sink", provider: "websocket" },
    ],
    edges: [
      { from: "mic", to: "stt" },
      { from: "stt", to: "llm" },
      { from: "llm", to: "tts" },
      { from: "tts", to: "speaker" },
    ],
  };
}

function defaultPipelineViews(
  snapshot: OperatorStatusSnapshot | null,
): readonly PipelineView[] {
  const pipeline = snapshot?.pipelines[0];
  if (!pipeline) {
    return [];
  }

  const graph: PipelineGraph = {
    name: pipeline.name,
    nodes: [
      { id: "mic", kind: "source", provider: "websocket" },
      { id: "stt", kind: "stt", provider: "whisper" },
      { id: "llm", kind: "llm", provider: "openai" },
      {
        id: "tts",
        kind: "tts",
        provider:
          pipeline.components.find(
            (component) => component.kind === "synthesis",
          )?.provider ?? "piper",
      },
      { id: "speaker", kind: "sink", provider: "websocket" },
    ],
    edges: [
      { from: "mic", to: "stt" },
      { from: "stt", to: "llm" },
      { from: "llm", to: "tts" },
      { from: "tts", to: "speaker" },
    ],
  };

  return [{ graph, order: graph.nodes.map((node) => node.id) }];
}

function pipelineViewToValidation(
  view: PipelineView,
): PipelineValidationResult {
  return { ok: true, order: view.order };
}

function componentKindForNode(node: PipelineNode): ComponentKind {
  if (node.kind === "stt") {
    return "transcription";
  }
  if (node.kind === "llm") {
    return "reasoning";
  }
  if (node.kind === "tool") {
    return "tools";
  }
  if (node.kind === "tts") {
    return "synthesis";
  }
  return "capture";
}

interface PipelineGraphFlow {
  mainNodes: PipelineNode[];
  mainEdges: PipelineGraph["edges"];
  spokesByTarget: Map<string, { node: PipelineNode; index: number }[]>;
}

function pipelineGraphFlow(graph: PipelineGraph): PipelineGraphFlow {
  const augmentNodeIds = new Set(
    graph.nodes
      .filter((node) => node.kind === "tool" || node.kind === "memory")
      .map((node) => node.id),
  );
  const mainNodes = graph.nodes.filter((node) => !augmentNodeIds.has(node.id));
  const mainNodeIds = new Set(mainNodes.map((node) => node.id));
  const mainEdges = graph.edges.filter(
    (edge) => mainNodeIds.has(edge.from) && mainNodeIds.has(edge.to),
  );
  const spokesByTarget = new Map<
    string,
    { node: PipelineNode; index: number }[]
  >();

  graph.nodes.forEach((node, index) => {
    if (!augmentNodeIds.has(node.id)) {
      return;
    }

    const target =
      graph.edges.find((edge) => edge.from === node.id)?.to ?? "llm";
    spokesByTarget.set(target, [
      ...(spokesByTarget.get(target) ?? []),
      { node, index },
    ]);
  });

  return { mainNodes, mainEdges, spokesByTarget };
}

function orbitPositionForNode(
  node: PipelineNode,
  fallbackIndex: number,
): OrbitPosition {
  const config = objectConfig(node.config);
  const ui = objectConfig(config.ui);
  const orbit = objectConfig(ui.orbit);
  const x = typeof orbit.x === "number" ? orbit.x : undefined;
  const y = typeof orbit.y === "number" ? orbit.y : undefined;
  if (x !== undefined && y !== undefined) {
    return { x, y };
  }

  return defaultAugmentOrbitPosition(fallbackIndex);
}

function nextAugmentOrbitPosition(
  graph: PipelineGraph,
  targetId: string,
): OrbitPosition {
  const existingAugmentCount = graph.nodes.filter((node) => {
    if (node.kind !== "tool" && node.kind !== "memory") {
      return false;
    }

    return (
      (graph.edges.find((edge) => edge.from === node.id)?.to ?? "llm") ===
      targetId
    );
  }).length;

  return defaultAugmentOrbitPosition(existingAugmentCount);
}

function defaultAugmentOrbitPosition(index: number): OrbitPosition {
  const angle = -Math.PI / 2 + index * ((2 * Math.PI) / 6);
  return {
    x: Math.round(Math.cos(angle) * 175),
    y: Math.round(Math.sin(angle) * 175),
  };
}

function withOrbitPosition(
  config: unknown,
  position: OrbitPosition,
): Record<string, unknown> {
  const configObject = objectConfig(config);
  const ui = objectConfig(configObject.ui);
  return {
    ...configObject,
    ui: {
      ...ui,
      orbit: {
        x: position.x,
        y: position.y,
      },
    },
  };
}

function objectConfig(value: unknown): Record<string, unknown> {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    return value as Record<string, unknown>;
  }

  return {};
}

function clampOrbitCoordinate(value: number): number {
  return Math.max(-360, Math.min(360, Math.round(value)));
}

function initializePipelineDrafts(
  pipelineViews: readonly PipelineView[],
): Record<string, PipelineEditorDraftState> {
  return Object.fromEntries(
    pipelineViews.map((view) => [
      view.graph.name,
      {
        draft: cloneGraph(view.graph),
        history: [],
        validation: null,
        notice: null,
      },
    ]),
  ) as Record<string, PipelineEditorDraftState>;
}

function cloneGraph(graph: PipelineGraph): PipelineGraph {
  return JSON.parse(JSON.stringify(graph)) as PipelineGraph;
}

function upsertPipelineView(
  views: readonly PipelineView[],
  graph: PipelineGraph,
  order = graph.nodes.map((node) => node.id),
): readonly PipelineView[] {
  const next = { graph: cloneGraph(graph), order };
  const existing = views.findIndex((view) => view.graph.name === graph.name);
  if (existing === -1) {
    return [...views, next];
  }

  return views.map((view, index) => (index === existing ? next : view));
}

function toolNodeIdBase(providerId: string): string {
  const normalized = providerId
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
  return `tool_${normalized || "provider"}`;
}

function uniqueNodeId(graph: PipelineGraph, base: string): string {
  const existing = new Set(graph.nodes.map((node) => node.id));
  if (!existing.has(base)) {
    return base;
  }

  let suffix = 2;
  while (existing.has(`${base}_${suffix}`)) {
    suffix += 1;
  }
  return `${base}_${suffix}`;
}

function useSmallScreenMode(forcedSmallScreen: boolean) {
  const [smallScreen, setSmallScreen] = useState(readsSmallScreenQuery);

  useEffect(() => {
    if (forcedSmallScreen) {
      return;
    }

    if (typeof window === "undefined" || !window.matchMedia) {
      return;
    }

    const query = window.matchMedia("(max-width: 700px)");
    const handleChange = (event: MediaQueryListEvent) => {
      setSmallScreen(event.matches);
    };

    query.addEventListener("change", handleChange);
    return () => query.removeEventListener("change", handleChange);
  }, [forcedSmallScreen]);

  return forcedSmallScreen || smallScreen;
}

function readsSmallScreenQuery() {
  if (typeof window === "undefined" || !window.matchMedia) {
    return false;
  }

  return window.matchMedia("(max-width: 700px)").matches;
}

function StaleBanner({ snapshot }: { snapshot: OperatorStatusSnapshot }) {
  return (
    <section className="stale-banner" aria-label="Stale state">
      <CircleAlert size={18} aria-hidden="true" />
      <div>
        <strong>Stale state</strong>
        <span>
          Last known snapshot from {formatTime(snapshot.generated_at)} remains
          visible
        </span>
      </div>
      {snapshot.event_stream.refresh_snapshot_after_reconnect ? (
        <span className="mini-badge">Reconnect refresh required</span>
      ) : null}
    </section>
  );
}

function FailureItem({
  failure,
  onOpenEvents,
}: {
  failure: RuntimeFailure;
  onOpenEvents?: () => void;
}) {
  return (
    <article
      className="exception-item critical"
      role="listitem"
      aria-label={`Runtime exception: ${failure.pipeline}`}
    >
      <CircleAlert size={17} aria-hidden="true" />
      <div>
        <h3>{failure.pipeline}</h3>
        <p>{failure.message}</p>
        <span>
          {failure.component}
          {failure.provider ? ` / ${failure.provider}` : ""}
        </span>
        {failure.turn && onOpenEvents ? (
          <button className="link-action" type="button" onClick={onOpenEvents}>
            <Radio size={15} aria-hidden="true" />
            Open turn events
          </button>
        ) : null}
      </div>
    </article>
  );
}

function PipelineException({
  pipeline,
  onOpenEvents,
}: {
  pipeline: PipelineStatus;
  onOpenEvents?: () => void;
}) {
  const affected = pipeline.components.filter((component) =>
    ["degraded", "unhealthy", "not_configured"].includes(component.state),
  );

  return (
    <article
      className="exception-item warning"
      role="listitem"
      aria-label={`Pipeline exception: ${pipeline.name}`}
    >
      <CircleAlert size={17} aria-hidden="true" />
      <div>
        <h3>{pipeline.name}</h3>
        <p>{pipeline.health.summary}</p>
        <span>
          {affected.length > 0
            ? affected.map((component) => component.kind).join(", ")
            : pipeline.health.state}
        </span>
        {pipeline.health.last_failed_turn && onOpenEvents ? (
          <button className="link-action" type="button" onClick={onOpenEvents}>
            <Radio size={15} aria-hidden="true" />
            Open turn events
          </button>
        ) : null}
      </div>
    </article>
  );
}

function ProviderWarning({ provider }: { provider: ProviderStatus }) {
  return (
    <article
      className="exception-item warning"
      role="listitem"
      aria-label={`Provider exception: ${provider.id}`}
    >
      <CircleAlert size={17} aria-hidden="true" />
      <div>
        <h3>{provider.id}</h3>
        <p>{provider.message ?? provider.state}</p>
        <span>{provider.affects_pipelines.join(", ") || provider.kind}</span>
      </div>
    </article>
  );
}

function PipelineOverview({ pipelines }: { pipelines: PipelineStatus[] }) {
  return (
    <StatusPanel
      title="Pipeline Health"
      value={`${pipelines.length} tracked`}
      icon={Bell}
    >
      <div className="compact-list">
        {pipelines.map((pipeline) => (
          <div className="metric-row" key={pipeline.name}>
            <span>{pipeline.name}</span>
            <strong className={`state-text ${pipeline.health.state}`}>
              {pipeline.health.state}
            </strong>
          </div>
        ))}
      </div>
    </StatusPanel>
  );
}

function SatelliteOverview({ snapshot }: { snapshot: OperatorStatusSnapshot }) {
  return (
    <StatusPanel title="Satellites" value="Presence and activity" icon={Radio}>
      <div className="split-list">
        <div>
          <h3>Connected Satellites</h3>
          {snapshot.satellites.connected.map((satellite) => (
            <p key={satellite.device}>{satellite.name}</p>
          ))}
          {snapshot.satellites.connected.length === 0 ? (
            <p>None connected</p>
          ) : null}
        </div>
        <div>
          <h3>Recently Active Satellites</h3>
          {snapshot.satellites.recently_active.map((satellite) => (
            <p key={satellite.device}>
              {satellite.name} / {satellite.last_event}
            </p>
          ))}
          {snapshot.satellites.recently_active.length === 0 ? (
            <p>No recent activity</p>
          ) : null}
        </div>
      </div>
    </StatusPanel>
  );
}

function TurnOverview({ snapshot }: { snapshot: OperatorStatusSnapshot }) {
  return (
    <StatusPanel
      title="Active Turns"
      value={`${snapshot.active_turns.length} running`}
      icon={Activity}
    >
      <div className="compact-list">
        {snapshot.active_turns.map((turn) => (
          <div className="metric-row" key={turn.turn}>
            <span>{turn.pipeline}</span>
            <strong>
              {turn.invoked_components.length > 0
                ? turn.invoked_components.join(", ")
                : "started"}
            </strong>
          </div>
        ))}
        {snapshot.active_turns.length === 0 ? <p>No active turns</p> : null}
      </div>
    </StatusPanel>
  );
}

function ProviderOverview({ providers }: { providers: ProviderStatus[] }) {
  return (
    <StatusPanel
      title="Provider Status"
      value={`${providers.length} configured`}
      icon={Boxes}
    >
      <div className="compact-list">
        {providers.map((provider) => (
          <div className="metric-row" key={provider.id}>
            <span>{provider.id}</span>
            <strong className={`state-text ${provider.state}`}>
              {provider.state}
            </strong>
          </div>
        ))}
      </div>
    </StatusPanel>
  );
}

function StatusPanel({
  title,
  value,
  icon: Icon,
  children,
}: {
  title: string;
  value: string;
  icon: typeof Activity;
  children?: ReactNode;
}) {
  return (
    <article className="status-panel">
      <Icon size={18} aria-hidden="true" />
      <div>
        <h2>{title}</h2>
        <p>{value}</p>
        {children}
      </div>
    </article>
  );
}

function formatTime(value: string): string {
  return new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date(value));
}

function StatusPill({
  label,
  value,
  tone,
}: {
  label: string;
  value: string;
  tone: "neutral" | "caution";
}) {
  return (
    <span className={`status-pill ${tone}`}>
      <span>{label}</span>
      <strong>{value}</strong>
    </span>
  );
}

export default App;
