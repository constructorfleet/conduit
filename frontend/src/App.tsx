import {
  Activity,
  Bell,
  Boxes,
  ChevronRight,
  CircuitBoard,
  CircleAlert,
  CircleCheck,
  KeyRound,
  ListFilter,
  Mic,
  Network,
  Play,
  Plus,
  Radio,
  RotateCcw,
  Save,
  Search,
  Settings,
  ShieldCheck,
  Square,
  Trash2,
  Upload,
  UserPlus,
  Users,
  Workflow,
  X,
} from "lucide-react";
import {
  Fragment,
  type FormEvent,
  type ReactNode,
  useEffect,
  useMemo,
  useState,
} from "react";

import conduitLogo from "./assets/conduit-logo.png";
import "./App.css";
import { createSnapshotClient } from "./apiClient";
import type {
  OperatorDataMode,
  SnapshotState,
  UnreadablePipeline,
} from "./apiClient";
import type {
  ComponentConfigProperty,
  EnrolledSpeaker,
  NodeKind,
  ProviderComponentCatalog,
  ProviderComponentDescriptor,
  ProviderCapability,
  PipelineGraph,
  PipelineNode,
  PipelineView,
  ProviderDefinition as ApiProviderDefinition,
  ProviderDefinitionVariant,
  ProviderDefinitionView,
  ProviderSecret,
  ScriptEngine,
  SpeakerEngine,
  TurnSnapshot,
  TransformRule,
  WakeRuntime,
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
} from "./contracts/status";
import {
  buildMinimalTextLoopGraph,
  buildMinimalVoiceLoopGraph,
  cloneGraph,
  defaultPipelineViews,
  initializePipelineDrafts,
  nextPipelineName,
  nodeProvider,
  normalizePipelineGraph,
  pipelineGraphsEqual,
  upsertPipelineView,
} from "./pipelines/graph";
import type {
  PipelineEditorDraftState,
  PipelineTestOutcome,
  PipelineTester,
  PipelineValidationResult,
  PipelineValidator,
} from "./pipelines/graph";
import { PipelineFormEditor } from "./pipelines/PipelineFormEditor";
import type {
  ProviderOptions,
  VoiceCatalog,
} from "./pipelines/PipelineFormEditor";

/// Asks a provider what voices it offers, answering `null` when it could not
/// be asked at all.
type VoiceLoader = (providerId: string) => Promise<VoiceCatalog>;
/// The phrases a wake detector has models for, empty when it cannot enumerate.
type PhraseLoader = (providerId: string) => Promise<string[]>;
import {
  FirmwarePanel,
  type FirmwareFlasher,
  type FirmwareRenderer,
} from "./firmware/FirmwarePanel";
import { formFromGraph, graphFromForm } from "./pipelines/form";
import { initialEventStreamPlan } from "./eventStream";
import type { EventStreamPosture } from "./eventStream";
import { fieldLabel, fieldLabels } from "./fieldLabel";
import { startRecording, type Recording } from "./recorder";
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
  { id: "speakers", label: "Speakers", icon: Users },
  { id: "firmware", label: "Firmware", icon: CircuitBoard },
  { id: "events", label: "Events", icon: Radio },
  { id: "settings", label: "Settings", icon: Settings },
] as const;

/// Defaults the graph model applies when a core omits them.
type SectionId = (typeof sections)[number]["id"];

type ProviderTester = (providerId: string) => Promise<string>;

/// Everything the Speakers section does, in the terms it does it in.
///
/// Narrower than the snapshot client on purpose: the panel manages people,
/// and nothing about it should be able to reach a pipeline.
export interface SpeakerApi {
  load: () => Promise<EnrolledSpeaker[]>;
  create: (name: string) => Promise<EnrolledSpeaker>;
  rename: (id: string, name: string) => Promise<EnrolledSpeaker>;
  enroll: (id: string, audio: Blob) => Promise<EnrolledSpeaker>;
  remove: (id: string) => Promise<void>;
}

interface AppProps {
  initialSnapshot?: OperatorStatusSnapshot;
  initialEvents?: readonly EventEnvelope[];
  initialEventPosture?: EventStreamPosture;
  initialComponentCatalog?: ProviderComponentCatalog;
  initialPipelineViews?: readonly PipelineView[];
  initialUnreadablePipelines?: readonly UnreadablePipeline[];
  initialProviderDefinitions?: readonly ProviderDefinitionView[];
  dataMode?: OperatorDataMode;
  onPipelineSaved?: (graph: PipelineGraph) => void;
  onPipelineDeleted?: (name: string) => void;
  /// The definition as it goes to the API, for tests that care what shape the
  /// console actually sends.
  onProviderDefinitionSaved?: (definition: ApiProviderDefinition) => void;
  /// The phrases a detector reports, for tests that care what the form offers.
  onProviderPhrases?: PhraseLoader;
  onPipelineValidate?: PipelineValidator;
  onPipelineTest?: PipelineTester;
}

function App({
  initialSnapshot,
  initialEvents,
  initialEventPosture,
  initialComponentCatalog,
  initialPipelineViews,
  initialUnreadablePipelines,
  initialProviderDefinitions,
  dataMode = defaultDataMode(),
  onPipelineSaved,
  onPipelineDeleted,
  onProviderDefinitionSaved,
  onProviderPhrases,
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
      initialUnreadablePipelines={initialUnreadablePipelines}
      initialProviderDefinitions={initialProviderDefinitions}
      initialSnapshot={initialSnapshot}
      dataMode={dataMode}
      onPipelineSaved={onPipelineSaved}
      onPipelineDeleted={onPipelineDeleted}
      onProviderDefinitionSaved={onProviderDefinitionSaved}
      onProviderPhrases={onProviderPhrases}
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
  initialUnreadablePipelines,
  initialProviderDefinitions,
  initialSnapshot,
  dataMode,
  onPipelineSaved,
  onPipelineDeleted,
  onProviderDefinitionSaved,
  onProviderPhrases,
  onPipelineValidate,
  onPipelineTest,
  onSectionChange,
  onClearAccess,
}: {
  access: OperatorAccess;
  activeSection: SectionId;
  initialEvents?: readonly EventEnvelope[];
  initialEventPosture?: EventStreamPosture;
  initialComponentCatalog?: ProviderComponentCatalog;
  initialPipelineViews?: readonly PipelineView[];
  initialUnreadablePipelines?: readonly UnreadablePipeline[];
  initialProviderDefinitions?: readonly ProviderDefinitionView[];
  initialSnapshot?: OperatorStatusSnapshot;
  dataMode: OperatorDataMode;
  onPipelineSaved?: (graph: PipelineGraph) => void;
  onPipelineDeleted?: (name: string) => void;
  /// The definition as it goes to the API, for tests that care what shape the
  /// console actually sends.
  onProviderDefinitionSaved?: (definition: ApiProviderDefinition) => void;
  /// The phrases a detector reports, for tests that care what the form offers.
  onProviderPhrases?: PhraseLoader;
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
  /// Pipelines the server has a name for and cannot read. Kept beside the
  /// readable ones rather than folded in: there is no graph to show, and the
  /// only thing the console can offer is deleting them.
  const [unreadablePipelines, setUnreadablePipelines] = useState<
    readonly UnreadablePipeline[]
  >(() => initialUnreadablePipelines ?? []);
  /// Throws away a pipeline the console cannot repair.
  ///
  /// Deletion never parses the stored graph, so this reaches a pipeline
  /// nothing else can — which is what makes it the way out of a corrupt one.
  function discardPipeline(name: string) {
    setUnreadablePipelines((current) =>
      current.filter((pipeline) => pipeline.name !== name),
    );
    onPipelineDeleted?.(name);
    void snapshotClient.deletePipeline(name).catch((error: unknown) => {
      setLoadError(
        error instanceof Error ? error.message : `could not delete ${name}`,
      );
    });
  }

  const [pipelineViews, setPipelineViews] = useState<readonly PipelineView[]>(
    () => initialPipelineViews ?? defaultPipelineViews(snapshotClient.snapshot),
  );
  const [componentCatalog, setComponentCatalog] =
    useState<ProviderComponentCatalog>(
      () => initialComponentCatalog ?? { components: [] },
    );
  const [providerDefinitions, setProviderDefinitions] = useState<
    ProviderDefinition[]
  >(() =>
    loadProviderDefinitions(
      initialComponentCatalog ?? { components: [] },
      initialPipelineViews ?? defaultPipelineViews(snapshotClient.snapshot),
      snapshotClient.snapshot,
      initialProviderDefinitions ?? [],
    ),
  );
  const [turnSnapshot, setTurnSnapshot] = useState<TurnSnapshot | null>(null);
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

  async function savePipeline(
    graph: PipelineGraph,
    providerDefinitionsToSave: readonly ApiProviderDefinition[] = [],
  ) {
    for (const definition of providerDefinitionsToSave) {
      const saved = await snapshotClient.saveProviderDefinition(definition);
      const mapped = fromApiProviderDefinition(componentCatalog, saved);
      setProviderDefinitions((current) =>
        mergeProviderDefinitions(
          current.filter((provider) => provider.id !== mapped.id),
          [mapped],
        ),
      );
    }
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

  async function runPipelineTest(name: string): Promise<PipelineTestOutcome> {
    const result = await snapshotClient.runPipelineTest(name, {
      utterance: "conduit test",
    });
    await refreshSnapshotFromApi();
    // A text pipeline reports what it wrote. Reporting "0 audio bytes" for a
    // pipeline that was never meant to speak reads as a failure.
    return {
      message: result.reply_text
        ? `Test turn completed for ${result.pipeline}: ${result.reply_text}`
        : `Test turn completed for ${result.pipeline}: ${result.audio_bytes} audio bytes`,
      replyAudio: result.reply_audio
        ? `data:audio/wav;base64,${result.reply_audio}`
        : null,
    };
  }

  async function runProviderTest(providerId: string): Promise<string> {
    const provider = await snapshotClient.testProviderDefinition(providerId);
    await refreshSnapshotFromApi();
    if (provider.reachable) {
      return `Provider ${provider.id} is reachable`;
    }
    return `Provider ${provider.id} is ${provider.state}${
      provider.message ? `: ${provider.message}` : ""
    }`;
  }

  /// The voices a synthesizer offers, or `null` when it could not be asked.
  ///
  /// A provider that is saved but unreachable must not stop an operator
  /// editing the pipeline that uses it, so a failure falls back to the typed
  /// voice field rather than surfacing an error.
  async function loadProviderVoices(providerId: string): Promise<VoiceCatalog> {
    try {
      const { voices } = await snapshotClient.loadProviderVoices(providerId);
      return voices.map((voice) => ({
        id: voice.id,
        label:
          voice.name === voice.id ? voice.id : `${voice.name} (${voice.id})`,
      }));
    } catch {
      return null;
    }
  }

  /// The phrases a wake detector has models for, or none when it cannot say.
  ///
  /// A detector scoring models in this process knows exactly which phrases it
  /// loaded, so an operator narrowing the list should see them rather than
  /// having to remember how the files were named. A Wyoming server enumerates
  /// nothing and a satellite knows only what it was flashed with; both answer
  /// with an empty list and the field stays a plain text box.
  async function loadProviderPhrases(providerId: string): Promise<string[]> {
    try {
      const { phrases } = await snapshotClient.loadProviderPhrases(providerId);
      return phrases;
    } catch {
      return [];
    }
  }

  /// Saves a definition, moving it first when `previousId` names a different
  /// one.
  ///
  /// An id is not private to its definition — pipeline nodes name it — so
  /// changing it is a rename rather than a save under the new name: saving
  /// alone would leave the old definition in place and every pipeline still
  /// pointing at it, which reads as the edit having created a second provider.
  /// The rename goes first so the settings that follow are written to the
  /// definition that now exists.
  async function saveProviderDefinition(
    definition: ProviderDefinition,
    previousId?: string,
  ): Promise<ProviderDefinition> {
    if (previousId && previousId !== definition.id) {
      const { renamed_pipelines } =
        await snapshotClient.renameProviderDefinition(
          previousId,
          definition.id,
        );
      setProviderDefinitions((current) =>
        current.filter((provider) => provider.id !== previousId),
      );
      // Reloaded rather than rewritten here: the server is what rewrote the
      // graphs, and a stage's reference may be qualified by a tool name the
      // console would have to reconstruct to patch in place.
      if (renamed_pipelines.length > 0) {
        const { views } = await snapshotClient.loadPipelineViews();
        setPipelineViews(views);
      }
    }
    const request = toApiProviderDefinition(definition);
    onProviderDefinitionSaved?.(request);
    const saved = await snapshotClient.saveProviderDefinition(request);
    const mapped = fromApiProviderDefinition(componentCatalog, saved);
    setProviderDefinitions((current) =>
      mergeProviderDefinitions(
        current.filter((provider) => provider.id !== mapped.id),
        [mapped],
      ),
    );
    await refreshSnapshotFromApi();
    return mapped;
  }

  async function deleteProviderDefinition(id: string): Promise<void> {
    await snapshotClient.deleteProviderDefinition(id);
    setProviderDefinitions((current) =>
      current.filter((provider) => provider.id !== id),
    );
    await refreshSnapshotFromApi();
  }

  /// The roster operations, bound to whichever client this workspace has.
  ///
  /// Memoized because the panel loads on mount and would otherwise reload on
  /// every render of the workspace around it.
  const speakerApi = useMemo<SpeakerApi>(
    () => ({
      load: () => snapshotClient.loadSpeakers(),
      create: (name) => snapshotClient.createSpeaker(name),
      rename: (id, name) => snapshotClient.renameSpeaker(id, name),
      enroll: (id, audio) => snapshotClient.enrollSpeaker(id, audio),
      remove: (id) => snapshotClient.deleteSpeaker(id),
    }),
    [snapshotClient],
  );

  /// Renders a device's fragment, bound to whichever client this workspace has.
  ///
  /// Not memoized like `speakerApi`: the firmware panel renders on a click
  /// rather than on mount, so a new identity between renders costs nothing.
  const renderFirmware: FirmwareRenderer = (device, request) =>
    snapshotClient.renderFirmware(device, request);

  /// Hands a rendered fragment to the operator's ESPHome dashboard, if they
  /// configured one. Unmemoized for the same reason as `renderFirmware`.
  const flashFirmware: FirmwareFlasher = (device, request) =>
    snapshotClient.flashFirmware(device, request);

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
          loadedProviderDefinitionViews,
          loadedTurns,
        ] = await Promise.all([
          snapshotClient.loadSnapshot(),
          initialPipelineViews
            ? Promise.resolve({
                views: [...initialPipelineViews],
                unreadable: [],
              })
            : snapshotClient.loadPipelineViews(),
          initialComponentCatalog
            ? Promise.resolve(initialComponentCatalog)
            : snapshotClient.loadComponentCatalog(),
          initialProviderDefinitions
            ? Promise.resolve([...initialProviderDefinitions])
            : snapshotClient.loadProviderDefinitions(),
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

        const basePipelineViews =
          initialPipelineViews ?? loadedPipelineViews.views;
        if (!initialUnreadablePipelines) {
          setUnreadablePipelines(loadedPipelineViews.unreadable);
        }
        const baseComponentCatalog =
          initialComponentCatalog ?? loadedComponentCatalog;
        const loadedProviderDefinitions = loadProviderDefinitions(
          baseComponentCatalog,
          basePipelineViews,
          loadedSnapshot,
          loadedProviderDefinitionViews,
        );
        let nextSnapshot = loadedSnapshot;
        let nextPipelineViews = basePipelineViews;

        if (!initialPipelineViews) {
          const repairedPipelineViews = basePipelineViews.map((view) => ({
            ...view,
            graph: normalizePipelineGraph(view.graph),
          }));
          const changedViews = repairedPipelineViews.filter(
            (view, index) =>
              !pipelineGraphsEqual(view.graph, basePipelineViews[index].graph),
          );

          if (changedViews.length > 0) {
            const savedViews = await Promise.all(
              changedViews.map((view) =>
                snapshotClient.savePipeline(view.graph),
              ),
            );
            const savedByName = new Map(
              savedViews.map((view) => [view.graph.name, view]),
            );
            nextPipelineViews = repairedPipelineViews.map(
              (view) => savedByName.get(view.graph.name) ?? view,
            );
            nextSnapshot = await snapshotClient
              .loadSnapshot()
              .catch(() => loadedSnapshot);
          }
        }

        if (cancelled) {
          return;
        }

        setSnapshot(nextSnapshot);
        setPipelineViews(nextPipelineViews);
        setComponentCatalog(baseComponentCatalog);
        setProviderDefinitions((current) =>
          mergeProviderDefinitions(loadedProviderDefinitions, current),
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
    initialUnreadablePipelines,
    initialPipelineViews,
    initialProviderDefinitions,
    initialSnapshot,
    snapshotClient,
  ]);

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
          speakers={speakerApi}
          onFirmwareRender={renderFirmware}
          onFirmwareFlash={flashFirmware}
          events={initialEvents ?? eventEnvelopeFixtures}
          turnSnapshot={turnSnapshot}
          componentCatalog={componentCatalog}
          providerDefinitions={providerDefinitions}
          pipelineViews={pipelineViews}
          unreadablePipelines={unreadablePipelines}
          onPipelineDiscarded={discardPipeline}
          snapshot={snapshot}
          eventPosture={eventPlan.posture}
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
          onProviderVoices={loadProviderVoices}
          onProviderPhrases={onProviderPhrases ?? loadProviderPhrases}
          onProviderDefinitionSave={saveProviderDefinition}
          onProviderDefinitionDelete={deleteProviderDefinition}
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
  speakers,
  onFirmwareRender,
  onFirmwareFlash,
  events,
  turnSnapshot,
  componentCatalog,
  providerDefinitions,
  pipelineViews,
  unreadablePipelines,
  snapshot,
  eventPosture,
  loadError,
  onSectionChange,
  onPipelineStored,
  onPipelineDiscarded,
  onPipelineValidate,
  onPipelineTest,
  onProviderTest,
  onProviderVoices,
  onProviderPhrases,
  onProviderDefinitionSave,
  onProviderDefinitionDelete,
}: {
  section: SectionId;
  speakers: SpeakerApi;
  onFirmwareRender: FirmwareRenderer;
  onFirmwareFlash: FirmwareFlasher;
  events: readonly EventEnvelope[];
  turnSnapshot: TurnSnapshot | null;
  componentCatalog: ProviderComponentCatalog;
  providerDefinitions: readonly ProviderDefinition[];
  pipelineViews: readonly PipelineView[];
  unreadablePipelines: readonly UnreadablePipeline[];
  onPipelineDiscarded: (name: string) => void;
  snapshot: OperatorStatusSnapshot | null;
  eventPosture: EventStreamPosture;
  loadError: string | null;
  onSectionChange: (section: SectionId) => void;
  onPipelineStored: (graph: PipelineGraph, order: string[]) => Promise<void>;
  onPipelineValidate: PipelineValidator;
  onPipelineTest: PipelineTester;
  onProviderTest: ProviderTester;
  onProviderVoices: VoiceLoader;
  onProviderPhrases: PhraseLoader;
  /// Saves a definition. `previousId` is the id it was stored under, when it
  /// was stored at all: a save that changes it is a rename, and the server has
  /// to be told which definition moved.
  onProviderDefinitionSave: (
    definition: ProviderDefinition,
    previousId?: string,
  ) => Promise<ProviderDefinition>;
  onProviderDefinitionDelete: (id: string) => Promise<void>;
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
        unreadablePipelines={unreadablePipelines}
        onPipelineStored={onPipelineStored}
        onPipelineDiscarded={onPipelineDiscarded}
        onPipelineValidate={onPipelineValidate}
        onPipelineTest={onPipelineTest}
        onProviderVoices={onProviderVoices}
      />
    );
  }

  if (section === "speakers") {
    return <SpeakersPanel speakers={speakers} />;
  }

  if (section === "firmware") {
    return (
      <FirmwarePanel
        pipelineNames={pipelineViews.map((view) => view.graph.name)}
        onRender={onFirmwareRender}
        onFlash={onFirmwareFlash}
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
        onProviderPhrases={onProviderPhrases}
        onProviderDefinitionSave={onProviderDefinitionSave}
        onProviderDefinitionDelete={onProviderDefinitionDelete}
      />
    );
  }

  return <SettingsPanel pipelineViews={pipelineViews} access={snapshot} />;
}

const OPERATOR_SETTINGS_STORAGE_KEY = "conduit.operator.settings";
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

/// The acceptance threshold a definition falls back to, mirroring
/// `conduit_provider::storage::DEFAULT_THRESHOLD_PERCENT`. A definition saved
/// without one must behave as the API documents rather than as zero, which
/// would be a detector that accepts everything it hears.
const DEFAULT_THRESHOLD_PERCENT = 50;

type ProviderFilter = "all" | ProviderKind;

/// What an operator reads for a provider capability.
///
/// Written out rather than upper-cased, because `SPEAKER_ID` names a field in
/// a JSON document and not a thing a house does.
function providerKindLabel(kind: ProviderFilter): string {
  if (kind === "stt") {
    return "Speech-to-text";
  }
  if (kind === "llm") {
    return "Language model";
  }
  if (kind === "tool") {
    return "Tools";
  }
  if (kind === "tts") {
    return "Text-to-speech";
  }
  if (kind === "transform") {
    return "Transform";
  }
  if (kind === "wake") {
    return "Wake word";
  }
  if (kind === "speaker_id") {
    return "Speaker ID";
  }
  if (kind === "vad") {
    return "Voice activity";
  }
  if (kind === "memory") {
    return "Memory";
  }
  return kind.toUpperCase();
}

interface ProviderDefinition {
  id: string;
  label: string;
  /// What the provider can do, not where it sits. Tools and memory are core
  /// bindings rather than graph stages, so they have no node kind.
  kind: ProviderCapability;
  component: string;
  config: Record<string, unknown>;
  source: "local" | "inferred";
  /// The definition this belongs to, for a tool discovered from an MCP
  /// server. Set means it is bindable but not separately configured, so the
  /// Providers page leaves it out: one server the operator set up is one card,
  /// however many tools it advertises.
  partOf?: string;
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
  onProviderPhrases,
  onProviderDefinitionSave,
  onProviderDefinitionDelete,
}: {
  componentCatalog: ProviderComponentCatalog;
  pipelineViews: readonly PipelineView[];
  providerDefinitions: readonly ProviderDefinition[];
  providers: readonly ProviderStatus[];
  onProviderTest: ProviderTester;
  onProviderPhrases: PhraseLoader;
  /// Saves a definition. `previousId` is the id it was stored under, when it
  /// was stored at all: a save that changes it is a rename, and the server has
  /// to be told which definition moved.
  onProviderDefinitionSave: (
    definition: ProviderDefinition,
    previousId?: string,
  ) => Promise<ProviderDefinition>;
  onProviderDefinitionDelete: (id: string) => Promise<void>;
}) {
  const [query, setQuery] = useState("");
  const [issuesOnly, setIssuesOnly] = useState(false);
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
  /// The provider whose delete button has been armed. A single click cannot
  /// delete a provider because deletion is not undoable and the button is
  /// small; a second click on the armed button is what actually removes it.
  const [confirmingDeleteFor, setConfirmingDeleteFor] = useState<string | null>(
    null,
  );
  /// Phrases the detector being edited reports having models for.
  ///
  /// Only a saved definition has a registered detector to ask, so a brand new
  /// one has nothing to offer and the field stays a plain text box — which is
  /// what it was before this existed.
  const [draftPhrases, setDraftPhrases] = useState<readonly string[]>([]);
  const draftWakeId =
    draftProvider && draftProvider.kind === "wake" && editingProviderId
      ? draftProvider.id
      : null;
  useEffect(() => {
    if (!draftWakeId) {
      setDraftPhrases([]);
      return;
    }
    let current = true;
    void onProviderPhrases(draftWakeId).then((phrases) => {
      if (current) {
        setDraftPhrases(phrases);
      }
    });
    return () => {
      current = false;
    };
  }, [draftWakeId, onProviderPhrases]);
  const draftProviderSuggestions =
    draftPhrases.length > 0 ? { phrases: draftPhrases } : undefined;
  const providerCards = providerCardViews(providerDefinitions, providers);
  const providerIds = new Set([
    ...providers.map((provider) => provider.id),
    ...providerDefinitions.map((provider) => provider.id),
  ]);
  const providerUses = providerPipelineUses(providerIds, pipelineViews);
  const normalizedQuery = query.trim().toLowerCase();
  const visibleProviderCards = providerCards.filter((provider) => {
    if (issuesOnly && !providerNeedsAttention(provider)) {
      return false;
    }
    if (
      normalizedQuery &&
      !`${provider.label} ${provider.id}`
        .toLowerCase()
        .includes(normalizedQuery)
    ) {
      return false;
    }
    return true;
  });
  const providerStageGroups: {
    kind: ProviderKind;
    label: string;
    cards: readonly ProviderCardView[];
  }[] = PROVIDER_STAGE_ORDER.map((kind) => ({
    kind,
    label: providerKindLabel(kind),
    cards: visibleProviderCards.filter((provider) => provider.kind === kind),
  })).filter((group) => group.cards.length > 0);
  const providerKinds: ProviderFilter[] = [
    "all",
    "stt",
    "llm",
    "tool",
    "tts",
    "transform",
    "wake",
    "speaker_id",
    "vad",
    "memory",
  ];
  const referencedProviderCount = new Set(
    pipelineViews.flatMap((view) =>
      view.graph.nodes
        .map(nodeProvider)
        .filter((provider) => providerIds.has(provider)),
    ),
  ).size;
  const selectedDraftComponent = draftProvider
    ? componentForProviderDefinition(componentCatalog, draftProvider)
    : null;
  const draftProviderValidation =
    draftProvider && selectedDraftComponent
      ? validateProviderDefinitionConfig(draftProvider, selectedDraftComponent)
      : ({
          ok: false,
          message: "Choose a provider component",
        } satisfies PipelineValidationResult);
  const selectedKindCapability = selectedProviderKind
    ? capabilityForProviderKind(selectedProviderKind)
    : null;
  const selectedKindComponents = selectedKindCapability
    ? componentCatalog.components.filter(
        (component) => component.kind === selectedKindCapability,
      )
    : [];

  function startNewProvider(component: ProviderComponentDescriptor) {
    const kind = providerKindForCapability(component.kind);

    setDraftProvider({
      id: `${kind}-${providerDefinitions.length + 1}`,
      label: component.label,
      kind: component.kind,
      component: component.id,
      config: configDefaults(component),
      source: "local",
    });
    setEditingProviderId("new");
    setSelectedProviderKind(null);
  }

  /// Opens a card's row for editing, or closes it if it is the one already open.
  ///
  /// The editor belongs to the row rather than to a dialog over the table: what
  /// an operator is deciding from — the state, the pipelines that use it, the
  /// other providers in the stage — stays readable while they change it.
  function editProviderCard(card: ProviderCardView) {
    if (editingProviderId === card.id) {
      cancelDraftProvider();
      return;
    }

    if (card.definition) {
      setDraftProvider(cloneProviderDefinition(card.definition));
      setEditingProviderId(card.id);
      setAddProviderDialogOpen(false);
      setSelectedProviderKind(null);
      return;
    }

    setDraftProvider({
      id: card.id,
      label: card.label,
      kind: capabilityForProviderKind(card.kind),
      component: card.component ?? card.id,
      config: {},
      source: "local",
    });
    setEditingProviderId(card.id);
    setAddProviderDialogOpen(false);
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

  async function saveDraftProvider() {
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
      source: "local",
    };
    // The id the definition is stored under, when it is stored at all: only a
    // saved definition can be moved, and a card standing for a provider the
    // runtime holds without one has nothing on the server to rename.
    const storedId = providerDefinitions.some(
      (provider) =>
        provider.id === editingProviderId && provider.source === "local",
    )
      ? (editingProviderId ?? undefined)
      : undefined;
    try {
      const saved = await onProviderDefinitionSave(next, storedId);
      setProviderNotices((current) => ({
        ...current,
        [saved.id]:
          storedId && storedId !== saved.id
            ? `Provider ${storedId} renamed to ${saved.id}`
            : `Provider ${saved.id} saved`,
      }));
      setDraftProvider(null);
      setEditingProviderId(null);
      setAddProviderDialogOpen(false);
      setSelectedProviderKind(null);
    } catch (caught) {
      setProviderNotices((current) => ({
        ...current,
        [next.id]:
          caught instanceof Error
            ? caught.message
            : `Unable to save provider ${next.id}`,
      }));
    }
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

  async function deleteProviderDefinition(provider: ProviderCardView) {
    const affectedPipelines = provider.status?.affects_pipelines ?? [];
    if (affectedPipelines.length > 0) {
      setProviderNotices((current) => ({
        ...current,
        [provider.id]: `Provider ${provider.id} is used by pipeline ${affectedPipelines.join(", ")}; remove it from those pipeline graphs before deleting it.`,
      }));
      setConfirmingDeleteFor(null);
      return;
    }

    // Two-click gate: the first click arms the button so the label changes to
    // "Confirm delete ${id}", the second one on that same armed button runs
    // the deletion. Deleting a provider is not undoable and the button lives
    // in a dense row of icon actions, so an accidental click should not lose
    // configuration.
    if (confirmingDeleteFor !== provider.id) {
      setConfirmingDeleteFor(provider.id);
      return;
    }

    const providerId = provider.id;
    setConfirmingDeleteFor(null);
    try {
      await onProviderDefinitionDelete(providerId);
      if (editingProviderId === providerId) {
        cancelDraftProvider();
      }
      setProviderNotices((current) => ({
        ...current,
        [providerId]: `Provider ${providerId} deleted`,
      }));
    } catch (caught) {
      setProviderNotices((current) => ({
        ...current,
        [providerId]:
          caught instanceof Error
            ? caught.message
            : `Unable to delete provider ${providerId}`,
      }));
    }
  }

  async function testProvider(provider: ProviderCardView) {
    try {
      let notice: string;
      if (!provider.definition) {
        notice = `Provider ${provider.id} has no configuration to test`;
      } else {
        notice = await onProviderTest(provider.id);
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
          label="Total providers"
          value={providerCards.length.toString()}
        />
        <MetricTile
          label="Provider configs"
          value={providerDefinitions.length.toString()}
        />
        <MetricTile
          label="Used in graphs"
          value={referencedProviderCount.toString()}
        />
        <MetricTile
          label="Needs attention"
          value={providerCards
            .filter((provider) => providerNeedsAttention(provider))
            .length.toString()}
        />
      </section>

      <div className="providers-controls">
        <label className="provider-search">
          <Search size={16} aria-hidden="true" />
          <input
            type="search"
            aria-label="Filter providers"
            placeholder="Filter providers"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </label>
        <label className="provider-issues-toggle">
          <input
            type="checkbox"
            checked={issuesOnly}
            onChange={(event) => setIssuesOnly(event.target.checked)}
          />
          Show issues only
        </label>
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

      {providerStageGroups.map((group) => (
        <section
          className="provider-stage"
          aria-label={`${group.label} providers`}
          key={group.kind}
        >
          <header className="provider-stage-header">
            <h2>{group.label}</h2>
            <span>{group.cards.length}</span>
          </header>
          <table className="provider-table">
            <colgroup>
              {PROVIDER_COLUMNS.map((column) => (
                <col key={column.label} style={{ width: column.width }} />
              ))}
            </colgroup>
            <thead>
              <tr>
                {PROVIDER_COLUMNS.map((column) => (
                  <th scope="col" key={column.label}>
                    {column.label === "Actions" ? (
                      <span className="visually-hidden">{column.label}</span>
                    ) : (
                      column.label
                    )}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {group.cards.map((provider) => (
                <Fragment key={provider.id}>
                  <tr
                    className={`provider-row ${providerCardStateClass(provider)}${
                      editingProviderId === provider.id ? " expanded" : ""
                    }`}
                    // The whole row is the target, because the whole row is
                    // what an operator reads before deciding to change it. The
                    // name cell holds the control that says so, and carries the
                    // keyboard path; this only widens where a pointer may land.
                    onClick={(event) => {
                      if (
                        event.target instanceof Element &&
                        event.target.closest("button, a, input, select")
                      ) {
                        return;
                      }
                      editProviderCard(provider);
                    }}
                  >
                    <td>
                      <button
                        className="provider-name-toggle"
                        type="button"
                        aria-label={`Configure ${provider.id}`}
                        aria-expanded={editingProviderId === provider.id}
                        onClick={() => editProviderCard(provider)}
                      >
                        <ChevronRight
                          className="provider-disclosure"
                          size={15}
                          aria-hidden="true"
                        />
                        <span className="provider-name">
                          <strong>{provider.label}</strong>
                          <span>{provider.id}</span>
                        </span>
                      </button>
                    </td>
                    <td>
                      <div className="provider-impl">
                        <strong>
                          {providerImplementationLabel(
                            componentCatalog,
                            provider,
                          )}
                        </strong>
                        {provider.status?.version ? (
                          <span>v{provider.status.version}</span>
                        ) : null}
                      </div>
                    </td>
                    <td>
                      <div className="provider-state">
                        <span
                          className={`state-dot ${providerStateTone(provider.status)}`}
                          aria-hidden="true"
                        />
                        <span>
                          {provider.status?.state ?? "not configured"}
                        </span>
                        {provider.status?.message ? (
                          <span className="provider-state-message">
                            {provider.status.message}
                          </span>
                        ) : null}
                      </div>
                    </td>
                    <td>
                      <div className="provider-used-by">
                        {providerPipelineUsesForCard(
                          provider,
                          providerUses,
                        ).map((use) => (
                          <span
                            className="usage-chip"
                            key={`${use.pipeline} ${use.stage ?? ""}`}
                          >
                            {use.pipeline}
                            {use.stage ? (
                              <span className="usage-stage">· {use.stage}</span>
                            ) : null}
                          </span>
                        ))}
                        {providerPipelineUsesForCard(provider, providerUses)
                          .length === 0 ? (
                          <span className="muted">none</span>
                        ) : null}
                      </div>
                    </td>
                    <td className="provider-actions-cell">
                      <div className="provider-actions">
                        {provider.status || provider.definition ? (
                          <button
                            className="secondary-action provider-test-action"
                            type="button"
                            aria-label={`Test ${provider.id}`}
                            onClick={() => testProvider(provider)}
                          >
                            <Play size={17} aria-hidden="true" />
                            Test
                          </button>
                        ) : null}
                        {provider.definition?.source === "local" ? (
                          <button
                            className={
                              confirmingDeleteFor === provider.id
                                ? "icon-action danger armed"
                                : "icon-action danger"
                            }
                            type="button"
                            aria-label={
                              confirmingDeleteFor === provider.id
                                ? `Confirm delete ${provider.id}`
                                : `Delete ${provider.id}`
                            }
                            onClick={() => deleteProviderDefinition(provider)}
                            onBlur={() => {
                              if (confirmingDeleteFor === provider.id) {
                                setConfirmingDeleteFor(null);
                              }
                            }}
                          >
                            <Trash2 size={17} aria-hidden="true" />
                          </button>
                        ) : null}
                      </div>
                    </td>
                  </tr>
                  {editingProviderId === provider.id && draftProvider ? (
                    <tr className="provider-editor-row">
                      <td colSpan={PROVIDER_COLUMNS.length}>
                        <section
                          className="provider-inline-editor"
                          aria-label={`${provider.id} configuration`}
                        >
                          <div className="provider-inline-editor-actions">
                            <button
                              className="icon-action success"
                              type="button"
                              aria-label="Save provider"
                              disabled={!draftProviderValidation.ok}
                              onClick={saveDraftProvider}
                            >
                              <Save size={17} aria-hidden="true" />
                            </button>
                            <button
                              className="icon-action danger"
                              type="button"
                              aria-label="Cancel provider edit"
                              onClick={cancelDraftProvider}
                            >
                              <X size={17} aria-hidden="true" />
                            </button>
                          </div>
                          <ProviderEditorFields
                            componentCatalog={componentCatalog}
                            draftProvider={draftProvider}
                            selectedComponent={selectedDraftComponent}
                            validation={draftProviderValidation}
                            suggestions={draftProviderSuggestions}
                            onConfigChange={updateDraftConfig}
                            onDraftChange={updateDraftProvider}
                          />
                        </section>
                      </td>
                    </tr>
                  ) : null}
                </Fragment>
              ))}
            </tbody>
          </table>
        </section>
      ))}
      {providerStageGroups.length === 0 ? (
        <div className="overview-empty" role="status">
          <Boxes size={18} aria-hidden="true" />
          <span>No providers match this filter</span>
        </div>
      ) : null}
      {addProviderDialogOpen ? (
        <ProviderAddDialog
          componentCatalog={componentCatalog}
          draftProvider={draftProvider}
          providerKinds={providerKinds}
          selectedComponent={selectedDraftComponent}
          validation={draftProviderValidation}
          selectedKind={selectedProviderKind}
          selectedKindComponents={selectedKindComponents}
          suggestions={draftProviderSuggestions}
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
  suggestions,
  onCancel,
  onConfigChange,
  onDraftChange,
  onKindChange,
  onSave,
  onSelectComponent,
}: {
  componentCatalog: ProviderComponentCatalog;
  draftProvider: ProviderDefinition | null;
  providerKinds: readonly ProviderFilter[];
  selectedComponent: ProviderComponentDescriptor | null;
  validation: PipelineValidationResult;
  selectedKind: ProviderKind | null;
  selectedKindComponents: readonly ProviderComponentDescriptor[];
  suggestions?: Record<string, readonly string[]>;
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
  onSelectComponent: (component: ProviderComponentDescriptor) => void;
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
            suggestions={suggestions}
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
                  &larr; Provider types
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
                    No components for {providerKindLabel(selectedKind)}
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
                    {providerKindLabel(kind)}
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
  suggestions,
  onConfigChange,
  onDraftChange,
}: {
  componentCatalog: ProviderComponentCatalog;
  draftProvider: ProviderDefinition;
  selectedComponent: ProviderComponentDescriptor | null;
  validation: PipelineValidationResult;
  suggestions?: Record<string, readonly string[]>;
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
          value={selectedComponent?.id ?? draftProvider.component}
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
                    config: configDefaults(component),
                  }
                : current,
            );
          }}
        >
          {componentCatalog.components
            .filter((component) => component.kind === draftProvider.kind)
            .map((component) => (
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
          suggestions={suggestions}
          onChange={onConfigChange}
        />
      ) : null}
      {!validation.ok ? (
        <p className="form-error">{validation.message}</p>
      ) : null}
    </div>
  );
}

/// A row's transient state: what it is doing and what it last said.
interface SpeakerRowState {
  recording: Recording | null;
  busy: boolean;
  notice: string | null;
  renaming: string | null;
}

const idleRow: SpeakerRowState = {
  recording: null,
  busy: false,
  notice: null,
  renaming: null,
};

/// Who the deployment knows, and how it came to know them.
///
/// Identification has been a pipeline stage for a while, but a stage that
/// matches a voice against enrolled prints is useless until something enrolls
/// one. This is that something — and it is also the only place a
/// [`SpeakerId`] is tied to a name, because the identification service is
/// deliberately never told who anybody is.
function SpeakersPanel({ speakers: api }: { speakers: SpeakerApi }) {
  const [speakers, setSpeakers] = useState<readonly EnrolledSpeaker[] | null>(
    null,
  );
  const [loadError, setLoadError] = useState<string | null>(null);
  const [newName, setNewName] = useState("");
  const [creating, setCreating] = useState(false);
  const [rows, setRows] = useState<Record<string, SpeakerRowState>>({});

  useEffect(() => {
    let cancelled = false;
    api
      .load()
      .then((loaded) => {
        if (!cancelled) {
          setSpeakers(loaded);
          setLoadError(null);
        }
      })
      .catch((caught: unknown) => {
        if (!cancelled) {
          setSpeakers([]);
          setLoadError(messageOf(caught, "Unable to load the speaker roster"));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [api]);

  function rowState(id: string): SpeakerRowState {
    return rows[id] ?? idleRow;
  }

  function updateRow(id: string, update: Partial<SpeakerRowState>) {
    setRows((current) => ({
      ...current,
      [id]: { ...(current[id] ?? idleRow), ...update },
    }));
  }

  /// Replaces one entry with what the server just said about it.
  function replaceSpeaker(speaker: EnrolledSpeaker) {
    setSpeakers((current) =>
      (current ?? []).map((entry) =>
        entry.id === speaker.id ? speaker : entry,
      ),
    );
  }

  async function addSpeaker(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const name = newName.trim();
    if (!name || creating) {
      return;
    }

    setCreating(true);
    try {
      const created = await api.create(name);
      setSpeakers((current) => [...(current ?? []), created]);
      setNewName("");
      setLoadError(null);
    } catch (caught) {
      setLoadError(messageOf(caught, `Unable to add ${name}`));
    } finally {
      setCreating(false);
    }
  }

  async function renameSpeaker(speaker: EnrolledSpeaker) {
    const name = (rowState(speaker.id).renaming ?? "").trim();
    if (!name || name === speaker.name) {
      updateRow(speaker.id, { renaming: null });
      return;
    }

    updateRow(speaker.id, { busy: true });
    try {
      replaceSpeaker(await api.rename(speaker.id, name));
      updateRow(speaker.id, { renaming: null, notice: null, busy: false });
    } catch (caught) {
      updateRow(speaker.id, {
        busy: false,
        notice: messageOf(caught, `Unable to rename ${speaker.name}`),
      });
    }
  }

  /// Sends one utterance, however it was captured.
  ///
  /// Recording and uploading differ only in where the file came from, so they
  /// share everything after that — including what the service said when it
  /// refused, which is the part an operator acts on.
  async function sendSample(speaker: EnrolledSpeaker, audio: Blob) {
    updateRow(speaker.id, { busy: true, notice: null });
    try {
      const enrolled = await api.enroll(speaker.id, audio);
      replaceSpeaker(enrolled);
      updateRow(speaker.id, {
        busy: false,
        notice: `Sample accepted — ${enrolled.samples} on file`,
      });
    } catch (caught) {
      updateRow(speaker.id, {
        busy: false,
        notice: messageOf(caught, `Unable to enroll ${speaker.name}`),
      });
    }
  }

  async function startSample(speaker: EnrolledSpeaker) {
    try {
      const recording = await startRecording();
      updateRow(speaker.id, { recording, notice: null });
    } catch (caught) {
      updateRow(speaker.id, {
        notice: messageOf(caught, "Unable to reach a microphone"),
      });
    }
  }

  async function finishSample(speaker: EnrolledSpeaker) {
    const recording = rowState(speaker.id).recording;
    if (!recording) {
      return;
    }
    updateRow(speaker.id, { recording: null });
    await sendSample(speaker, await recording.stop());
  }

  async function removeSpeaker(speaker: EnrolledSpeaker) {
    updateRow(speaker.id, { busy: true, notice: null });
    try {
      await api.remove(speaker.id);
      setSpeakers((current) =>
        (current ?? []).filter((entry) => entry.id !== speaker.id),
      );
    } catch (caught) {
      updateRow(speaker.id, {
        busy: false,
        // The roster entry is still there on purpose when the service will
        // not forget the voice print, so this is a state to show rather than
        // a failure to swallow.
        notice: messageOf(caught, `Unable to remove ${speaker.name}`),
      });
    }
  }

  const roster = speakers ?? [];
  const enrolled = roster.filter((speaker) => speaker.samples > 0).length;

  return (
    <div className="providers-stack">
      <section className="summary-grid" aria-label="Speaker summary">
        <MetricTile label="Speakers" value={roster.length.toString()} />
        <MetricTile label="Enrolled" value={enrolled.toString()} />
        <MetricTile
          label="Not yet recorded"
          value={(roster.length - enrolled).toString()}
        />
      </section>

      <form className="speaker-add" onSubmit={addSpeaker}>
        <label className="field">
          <span className="field-label">Name</span>
          <input
            value={newName}
            placeholder="Who is this?"
            onChange={(event) => setNewName(event.target.value)}
          />
        </label>
        <button
          className="secondary-action"
          type="submit"
          disabled={creating || newName.trim().length === 0}
        >
          <UserPlus size={16} aria-hidden="true" />
          Add speaker
        </button>
      </form>

      {loadError ? (
        <p className="form-error" role="alert">
          {loadError}
        </p>
      ) : null}

      {speakers === null ? (
        <p className="panel-notice">Loading the speaker roster…</p>
      ) : roster.length === 0 ? (
        <p className="panel-notice">
          Nobody is enrolled yet. Add a name, then record a sample of their
          voice — until a voice is enrolled, every turn reaches a tool&rsquo;s
          permission check with no speaker.
        </p>
      ) : (
        <table className="provider-table">
          <thead>
            <tr>
              <th scope="col">Speaker</th>
              <th scope="col">Samples</th>
              <th scope="col">Enrolled against</th>
              <th scope="col">
                <span className="visually-hidden">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {roster.map((speaker) => {
              const row = rowState(speaker.id);
              return (
                <tr className="provider-row" key={speaker.id}>
                  <td>
                    {row.renaming === null ? (
                      <div className="provider-name">
                        <strong>{speaker.name}</strong>
                        <span>{speaker.id}</span>
                      </div>
                    ) : (
                      <input
                        aria-label={`New name for ${speaker.name}`}
                        value={row.renaming}
                        autoFocus
                        onChange={(event) =>
                          updateRow(speaker.id, {
                            renaming: event.target.value,
                          })
                        }
                        onBlur={() => void renameSpeaker(speaker)}
                        onKeyDown={(event) => {
                          if (event.key === "Enter") {
                            void renameSpeaker(speaker);
                          }
                          if (event.key === "Escape") {
                            updateRow(speaker.id, { renaming: null });
                          }
                        }}
                      />
                    )}
                    {row.notice ? (
                      <p className="speaker-notice">{row.notice}</p>
                    ) : null}
                  </td>
                  <td>
                    <div className="provider-state">
                      <span
                        className={`state-dot ${speaker.samples > 0 ? "good" : "warn"}`}
                        aria-hidden="true"
                      />
                      <span>
                        {speaker.samples > 0
                          ? `${speaker.samples} on file`
                          : "no voice yet"}
                      </span>
                    </div>
                  </td>
                  <td>{speaker.provider ?? "—"}</td>
                  <td>
                    <div className="provider-card-controls">
                      {row.recording ? (
                        <button
                          className="icon-action success"
                          type="button"
                          aria-label={`Stop recording ${speaker.name}`}
                          onClick={() => void finishSample(speaker)}
                        >
                          <Square size={16} aria-hidden="true" />
                        </button>
                      ) : (
                        <button
                          className="icon-action"
                          type="button"
                          aria-label={`Record a sample for ${speaker.name}`}
                          disabled={row.busy}
                          onClick={() => void startSample(speaker)}
                        >
                          <Mic size={16} aria-hidden="true" />
                        </button>
                      )}
                      <label
                        className="icon-action"
                        aria-label={`Upload a sample for ${speaker.name}`}
                      >
                        <Upload size={16} aria-hidden="true" />
                        <input
                          type="file"
                          accept="audio/wav,.wav"
                          className="visually-hidden"
                          disabled={row.busy}
                          onChange={(event) => {
                            const file = event.target.files?.[0];
                            // Cleared so the same file can be chosen twice:
                            // a second take of the same recording is a
                            // normal thing to want.
                            event.target.value = "";
                            if (file) {
                              void sendSample(speaker, file);
                            }
                          }}
                        />
                      </label>
                      <button
                        className="icon-action"
                        type="button"
                        aria-label={`Rename ${speaker.name}`}
                        disabled={row.busy}
                        onClick={() =>
                          updateRow(speaker.id, { renaming: speaker.name })
                        }
                      >
                        <KeyRound size={16} aria-hidden="true" />
                      </button>
                      <button
                        className="icon-action danger"
                        type="button"
                        aria-label={`Remove ${speaker.name}`}
                        disabled={row.busy}
                        onClick={() => void removeSpeaker(speaker)}
                      >
                        <Trash2 size={16} aria-hidden="true" />
                      </button>
                    </div>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </div>
  );
}

/// What went wrong, in the words of whoever refused.
function messageOf(caught: unknown, fallback: string): string {
  return caught instanceof Error && caught.message ? caught.message : fallback;
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
  unreadablePipelines,
  onPipelineStored,
  onPipelineDiscarded,
  onPipelineValidate,
  onPipelineTest,
  onProviderVoices,
}: {
  providerDefinitions: readonly ProviderDefinition[];
  pipelineViews: readonly PipelineView[];
  unreadablePipelines: readonly UnreadablePipeline[];
  onPipelineDiscarded: (name: string) => void;
  onPipelineStored: (graph: PipelineGraph, order: string[]) => void;
  onPipelineValidate: PipelineValidator;
  onPipelineTest: PipelineTester;
  onProviderVoices: VoiceLoader;
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
  const replyAudio = selectedDraftState?.replyAudio ?? null;
  const [pendingPipelineName, setPendingPipelineName] = useState<string | null>(
    null,
  );
  /// The name being typed for a new pipeline, or `null` when none is being
  /// created. Named at creation because the graph editor has no rename, so a
  /// pipeline stored under a generated name would keep it.
  const [newPipelineName, setNewPipelineName] = useState<string | null>(null);
  const pendingPipeline = pendingPipelineName
    ? (pipelineViews.find((view) => view.graph.name === pendingPipelineName) ??
      null)
    : null;
  const hasUnsavedEdits = history.length > 0;
  /// The providers the form offers, grouped by what they do.
  const formProviderOptions: ProviderOptions = {
    stt: providerOptionsFor("stt"),
    llm: providerOptionsFor("llm"),
    tts: providerOptionsFor("tts"),
    tool: providerOptionsFor("tool"),
    transform: providerOptionsFor("transform"),
    wake: providerOptionsFor("wake"),
    speakerId: providerOptionsFor("speaker_id"),
    vad: providerOptionsFor("vad"),
  };
  /// The voices the draft's synthesizer offers, asked of the provider itself.
  ///
  /// Keyed by provider id so switching synthesizers does not briefly offer the
  /// previous one's voices, and so a provider that was already asked is not
  /// asked again on every keystroke elsewhere in the form.
  const [voicesByProvider, setVoicesByProvider] = useState<
    Record<string, VoiceCatalog>
  >({});
  const synthesisProvider = draft
    ? (formFromGraph(draft).tts?.provider ?? null)
    : null;

  useEffect(() => {
    if (!synthesisProvider || synthesisProvider in voicesByProvider) {
      return;
    }
    let current = true;
    void onProviderVoices(synthesisProvider).then((voices) => {
      if (current) {
        setVoicesByProvider((known) => ({
          ...known,
          [synthesisProvider]: voices,
        }));
      }
    });
    return () => {
      current = false;
    };
  }, [synthesisProvider, voicesByProvider, onProviderVoices]);

  function providerOptionsFor(capability: ProviderCapability) {
    return providerDefinitions
      .filter((provider) => provider.kind === capability)
      .map((provider) => ({ id: provider.id, label: provider.label }));
  }

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
    setPendingPipelineName(null);
  }

  /// Stores a new pipeline under `name`.
  ///
  /// Copies the pipeline on screen when there is one, because a second
  /// pipeline is usually a variant of the first and its providers already
  /// exist. With nothing to copy it builds the smallest graph the configured
  /// providers support — which is the case that matters, because an operator
  /// who has deleted their last pipeline has no other way back: Guided Setup
  /// runs on first launch and does not return.
  async function addPipeline(name: string) {
    if (!canCreatePipeline(name)) {
      return;
    }
    const graph = selectedView
      ? { ...selectedView.graph, name }
      : minimalGraphFor(name, providerDefinitions);
    if (!graph) {
      return;
    }
    await onPipelineStored(graph, selectedView ? [...selectedView.order] : []);
    setNewPipelineName(null);
    setSelectedName(name);
  }

  /// The name offered when the operator asks for a new pipeline.
  function suggestedPipelineName(): string {
    return selectedView
      ? nextPipelineName(
          selectedView.graph.name,
          pipelineViews.map((view) => view.graph.name),
        )
      : "pipeline";
  }

  /// Whether `name` could be stored: the server refuses names that mean
  /// something to a filesystem, and refuses to overwrite by accident.
  function canCreatePipeline(name: string): boolean {
    const trimmed = name.trim();
    return (
      trimmed.length > 0 &&
      !/[^A-Za-z0-9_-]/.test(trimmed) &&
      !pipelineViews.some((view) => view.graph.name === trimmed)
    );
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
      // Any other notice supersedes the last test turn, so its player goes
      // with it rather than leaving stale audio to play under new text.
      replyAudio: null,
    }));
  }

  function markCurrentDraftTestResult(outcome: PipelineTestOutcome) {
    updateCurrentDraftState((current) => ({
      ...current,
      notice: outcome.message,
      replyAudio: outcome.replyAudio,
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

  /// Offers a tool to the core, if it is not already offered.
  ///
  /// A binding rather than a node: there is no id to make unique and no edge
  /// to draw, because a tool is configuration on the core rather than a stage
  /// the reply passes through.
  /// Sets one configuration field on a node, or removes it when the operator
  /// empties the input.
  ///
  /// Every config field is optional, and absent carries a meaning of its own —
  /// an absent model means whichever model the provider serves first. Writing
  /// an empty string instead would ask the provider for a model named nothing.
  /// The configuration a node kind accepts, beyond which provider serves it.
  ///
  /// Only the kinds that carry configuration render anything, so a source or a
  /// sink still shows just its provider.
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
      const hydratedDraft = normalizePipelineGraph(draft);
      const result = await onPipelineValidate(hydratedDraft);
      updateCurrentDraftState((current) => ({
        ...current,
        draft: hydratedDraft,
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
    const hydratedDraft = normalizePipelineGraph(draft);

    try {
      await onPipelineStored(hydratedDraft, validation.order);
      updateCurrentDraftState((current) => ({
        ...current,
        draft: hydratedDraft,
        history: [],
        notice: `Saved graph for ${hydratedDraft.name}`,
      }));
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
      const hydratedDraft = normalizePipelineGraph(draft);
      const result = await onPipelineValidate(hydratedDraft);
      updateCurrentDraftState((current) => ({
        ...current,
        draft: hydratedDraft,
        validation: result,
        notice: null,
      }));
      if (!result.ok) {
        return;
      }
      if (hasUnsavedEdits) {
        await onPipelineStored(hydratedDraft, result.order);
        updateCurrentDraftAfterSave(`Saved graph for ${hydratedDraft.name}`);
      }
      markCurrentDraftTestResult(await onPipelineTest(hydratedDraft.name));
    } catch (caught) {
      markCurrentDraftNotice(
        caught instanceof Error ? caught.message : "Unable to run test turn",
      );
    }
  }

  /// The band listing pipelines the server cannot read.
  function renderUnreadablePipelines() {
    return (
      <section className="exception-band" aria-label="Unreadable pipelines">
        <div className="section-heading">
          <div>
            <p className="eyebrow">Stored but unusable</p>
            <h2>Unreadable Pipelines</h2>
          </div>
        </div>
        <p className="hint">
          These are stored under a name the server can list and cannot read, so
          there is no graph to edit. Deleting one is the only repair from here;
          its definition can then be recreated.
        </p>
        <div className="exception-list" role="list">
          {unreadablePipelines.map((pipeline) => (
            <div className="exception-item" role="listitem" key={pipeline.name}>
              <div>
                <strong>{pipeline.name}</strong>
                <p className="node-provider-label">{pipeline.detail}</p>
              </div>
              <button
                className="secondary-action danger"
                type="button"
                aria-label={`Delete pipeline ${pipeline.name}`}
                onClick={() => onPipelineDiscarded(pipeline.name)}
              >
                <Trash2 size={16} aria-hidden="true" />
                Delete
              </button>
            </div>
          ))}
        </div>
      </section>
    );
  }

  /// The add button, and the name field it opens.
  function renderNewPipelineControls() {
    if (newPipelineName === null) {
      return (
        <button
          className="icon-action"
          type="button"
          aria-label="Add pipeline"
          title="Add pipeline"
          onClick={() => setNewPipelineName(suggestedPipelineName())}
        >
          <Plus size={16} aria-hidden="true" />
        </button>
      );
    }

    return (
      <div className="new-pipeline">
        <input
          aria-label="New pipeline name"
          value={newPipelineName}
          onChange={(event) => setNewPipelineName(event.target.value)}
        />
        <button
          type="button"
          className="secondary-action"
          disabled={!canCreatePipeline(newPipelineName)}
          onClick={() => void addPipeline(newPipelineName)}
        >
          Create pipeline
        </button>
        <button
          type="button"
          className="secondary-action"
          onClick={() => setNewPipelineName(null)}
        >
          Cancel
        </button>
      </div>
    );
  }

  if (!draft || !selectedView) {
    const buildable = minimalGraphFor("probe", providerDefinitions) !== null;
    return (
      <div className="pipelines-stack">
        {unreadablePipelines.length > 0 ? renderUnreadablePipelines() : null}
        <section className="pipeline-toolbar" aria-label="Stored pipelines">
          <div className="overview-empty" role="status">
            <Workflow size={18} aria-hidden="true" />
            <span>No stored pipelines</span>
          </div>
          {buildable ? (
            renderNewPipelineControls()
          ) : (
            <p className="hint">
              Configure a language model provider before creating a pipeline: a
              graph that names no provider cannot be saved.
            </p>
          )}
        </section>
      </div>
    );
  }

  return (
    <div className="pipelines-stack">
      {unreadablePipelines.length > 0 ? renderUnreadablePipelines() : null}

      <section className="pipeline-toolbar" aria-label="Stored pipelines">
        <div className="pipeline-toolbar-main">
          <div>
            <p className="eyebrow">Configuration</p>
            <h2>Pipeline Editor</h2>
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
              {renderNewPipelineControls()}
            </div>
          </div>
        </div>
        <div
          className="graph-actions"
          role="toolbar"
          aria-label="Pipeline editor actions"
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
          <div className="graph-action-group">
            <button
              className="secondary-action compact-action"
              type="button"
              aria-label="Validate pipeline"
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
              aria-label="Save pipeline"
              disabled={validation?.ok !== true}
              onClick={saveDraft}
            >
              <Save size={16} aria-hidden="true" />
              Save
            </button>
          </div>
        </div>
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

      <div className="pipeline-editor-grid">
        <section className="graph-surface" aria-label="Pipeline configuration">
          <PipelineFormEditor
            form={formFromGraph(draft)}
            providers={formProviderOptions}
            voices={
              synthesisProvider
                ? (voicesByProvider[synthesisProvider] ?? null)
                : null
            }
            readOnly={false}
            onChange={(next) => applyDraftEdit(() => graphFromForm(next))}
          />
        </section>

        {validation || notice ? (
          <section
            className="pipeline-editor-status"
            aria-label="Pipeline editor status"
          >
            {validation ? (
              <p className={validation.ok ? "validation-ok" : "form-error"}>
                {validation.ok ? "Validation passed" : validation.message}
              </p>
            ) : null}

            {notice ? <p className="panel-notice">{notice}</p> : null}

            {replyAudio ? (
              <audio
                className="test-turn-reply"
                controls
                src={replyAudio}
                aria-label="Test turn reply audio"
              >
                <track kind="captions" />
              </audio>
            ) : null}
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
  suggestions,
  onChange,
}: {
  component: ProviderComponentDescriptor;
  config: Record<string, unknown>;
  readOnly: boolean;
  /// Values a field is known to accept, offered as suggestions rather than as
  /// a closed menu. A wake detector reports the phrases it has models for, and
  /// an operator narrowing a list should see them without having to remember
  /// how the files were named.
  suggestions?: Record<string, readonly string[]>;
  onChange: (
    field: string,
    property: ComponentConfigProperty,
    value: string | boolean,
  ) => void;
}) {
  const required = new Set(component.schema.required);

  /// The field's name, marked when it must be answered.
  ///
  /// The mark is drawn by CSS rather than written into the label, so the
  /// control's accessible name stays the field's name; that it is required is
  /// carried by the `required` attribute, which is also what enforces it.
  function labelFor(field: string, suffix = "") {
    return (
      <span
        className={required.has(field) ? "field-label required" : "field-label"}
      >
        {`${fieldLabel(field)}${suffix}`}
      </span>
    );
  }

  /// Whether a required field is still empty, which is what marks the control
  /// rather than only the message under the form.
  function isMissing(field: string, property: ComponentConfigProperty) {
    if (!required.has(field)) {
      return false;
    }
    const value = config[field];
    if (property.type === "boolean") {
      return value !== true;
    }
    if (property.type === "integer") {
      return typeof value !== "number";
    }
    return typeof value !== "string" || value.trim().length === 0;
  }

  return (
    <fieldset
      className="component-config-fields"
      disabled={readOnly}
      aria-label={`${component.label} configuration`}
    >
      {Object.entries(component.schema.properties).map(([field, property]) => {
        const label = labelFor(field);
        if (property.type === "boolean") {
          return (
            <label className="check-row" key={field}>
              <input
                type="checkbox"
                required={required.has(field)}
                aria-invalid={isMissing(field, property) || undefined}
                checked={config[field] === true}
                onChange={(event) =>
                  onChange(field, property, event.target.checked)
                }
              />
              {label}
            </label>
          );
        }

        // A closed set — a wake word engine, an embedding model — is a menu.
        // A text box for it lets an operator save a definition the server then
        // refuses, and the refusal arrives long after they typed it. A field
        // holding *several* of a closed set is not a menu, though: a menu
        // picks one, and the list below keeps the order the operator wrote.
        if (
          property.type !== "string_list" &&
          property.options &&
          property.options.length > 0
        ) {
          return (
            <label className="field" key={field}>
              {label}
              <select
                required={required.has(field)}
                aria-invalid={isMissing(field, property) || undefined}
                value={typeof config[field] === "string" ? config[field] : ""}
                onChange={(event) =>
                  onChange(field, property, event.target.value)
                }
              >
                <option value="">choose one</option>
                {property.options.map((option) => (
                  <option key={option} value={option}>
                    {option}
                  </option>
                ))}
              </select>
            </label>
          );
        }

        // Several of a closed set: a menu of what is left, and a tag for each
        // one taken. A single text box offered the set only while it was
        // empty, so an operator adding a second entry was retyping a name from
        // memory and learned they had misremembered it when the save was
        // refused. The tags keep the order they were added, which for a
        // transform's rules is the order they are applied.
        if (
          property.type === "string_list" &&
          property.options &&
          property.options.length > 0
        ) {
          const chosen = listEntries(config[field]);
          const remaining = property.options.filter(
            (option) => !chosen.includes(option),
          );
          const selectId = `${component.id}-${field}`;
          const choose = (entries: readonly string[]) =>
            onChange(field, property, entries.join(", "));
          return (
            <div className="field" key={field}>
              <label htmlFor={selectId}>{label}</label>
              {chosen.length > 0 ? (
                <ul className="tag-list">
                  {chosen.map((entry) => (
                    <li className="tag" key={entry}>
                      {entry}
                      <button
                        type="button"
                        className="tag-remove"
                        aria-label={`Remove ${entry}`}
                        onClick={() =>
                          choose(chosen.filter((held) => held !== entry))
                        }
                      >
                        <X size={12} aria-hidden="true" />
                      </button>
                    </li>
                  ))}
                </ul>
              ) : null}
              <select
                id={selectId}
                // Required until something is chosen: the menu itself is left
                // empty once there are tags, and a control that both holds no
                // value and must have one would refuse a filled-in form.
                required={required.has(field) && chosen.length === 0}
                aria-invalid={isMissing(field, property) || undefined}
                // Always empty: the menu is how an entry is added, not where
                // the answer is kept, and leaving the last pick selected would
                // read as though it were the whole answer.
                value=""
                disabled={remaining.length === 0}
                onChange={(event) => choose([...chosen, event.target.value])}
              >
                <option value="">
                  {remaining.length === 0
                    ? "all added"
                    : chosen.length > 0
                      ? "add another"
                      : "choose one or more"}
                </option>
                {remaining.map((option) => (
                  <option key={option} value={option}>
                    {option}
                  </option>
                ))}
              </select>
            </div>
          );
        }

        // Held as the text an operator is typing, and split into a list when
        // the definition is built. Splitting on every keystroke would trim the
        // space they just typed and run the words together.
        if (property.type === "string_list") {
          // Suggestions rather than a menu: the field holds several values,
          // and a provider that enumerates some of them has not thereby
          // forbidden the rest — a wake detector reporting the phrases it has
          // models for has not forbidden the ones it does not.
          const known = suggestions?.[field] ?? [];
          const listId =
            known.length > 0 ? `${component.id}-${field}` : undefined;
          return (
            <label className="field" key={field}>
              {labelFor(field, " (comma separated)")}
              <input
                type="text"
                required={required.has(field)}
                aria-invalid={isMissing(field, property) || undefined}
                list={listId}
                value={typeof config[field] === "string" ? config[field] : ""}
                onChange={(event) =>
                  onChange(field, property, event.target.value)
                }
              />
              {listId ? (
                <datalist id={listId}>
                  {known.map((value) => (
                    <option key={value} value={value} />
                  ))}
                </datalist>
              ) : null}
            </label>
          );
        }

        // A program rather than a value: a script is written over several lines
        // and read back to be edited, so a one-line box would show a few words
        // of it and scroll the rest out of sight.
        if (property.format === "multiline") {
          return (
            <label className="field" key={field}>
              {label}
              <textarea
                rows={8}
                spellCheck={false}
                required={required.has(field)}
                aria-invalid={isMissing(field, property) || undefined}
                value={typeof config[field] === "string" ? config[field] : ""}
                onChange={(event) =>
                  onChange(field, property, event.target.value)
                }
              />
            </label>
          );
        }

        return (
          <label className="field" key={field}>
            {label}
            <input
              type={
                property.type === "integer"
                  ? "number"
                  : property.format === "url"
                    ? "url"
                    : "text"
              }
              pattern={
                property.type === "integer" ? undefined : property.pattern
              }
              required={required.has(field)}
              aria-invalid={isMissing(field, property) || undefined}
              value={
                property.type === "integer"
                  ? typeof config[field] === "number"
                    ? String(config[field])
                    : ""
                  : typeof config[field] === "string"
                    ? config[field]
                    : ""
              }
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
  catalog: ProviderComponentCatalog,
  node: PipelineNode,
): ProviderComponentDescriptor | null {
  const exact = catalog.components.find(
    (component) =>
      component.id === nodeProvider(node) && component.kind === node.kind,
  );
  if (exact) {
    return exact;
  }

  if (nodeProvider(node) === "openai") {
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
        component.id === nodeProvider(node) && component.kind === node.kind,
    ) ?? null
  );
}

/// What a fresh form for `component` starts as.
///
/// Only the fields whose schema names a default, so a component that suggests
/// nothing still opens empty. A suggestion rather than a constraint: every one
/// of them is a field the operator can clear or replace before saving.
function configDefaults(
  component: ProviderComponentDescriptor,
): Record<string, unknown> {
  return Object.fromEntries(
    Object.entries(component.schema.properties).flatMap(([field, property]) =>
      property.default === undefined ? [] : [[field, property.default]],
    ),
  );
}

function componentForProviderStatus(
  catalog: ProviderComponentCatalog,
  provider: ProviderStatus,
): ProviderComponentDescriptor | null {
  return componentForNode(catalog, {
    id: provider.id,
    kind: capabilityForProviderKind(provider.kind),
    provider: provider.id,
  } as PipelineNode);
}

function componentForProviderDefinition(
  catalog: ProviderComponentCatalog,
  provider: ProviderDefinition,
): ProviderComponentDescriptor | null {
  const componentId =
    provider.kind === "tts" && provider.component === "wyoming"
      ? "wyoming.tts"
      : provider.component;
  return (
    catalog.components.find(
      (component) =>
        component.id === componentId && component.kind === provider.kind,
    ) ?? null
  );
}

function isWyomingProviderName(provider: string): boolean {
  const normalized = provider.toLowerCase();
  return (
    normalized.includes("wyoming") ||
    normalized.includes("piper") ||
    normalized.includes("whisper")
  );
}

function providerCardViews(
  definitions: readonly ProviderDefinition[],
  statuses: readonly ProviderStatus[],
): ProviderCardView[] {
  const cards = new Map<string, ProviderCardView>();

  for (const definition of definitions) {
    // A tool belonging to a server is configured through that server, so it
    // gets no card of its own.
    if (definition.partOf) {
      continue;
    }
    const kind = providerKindForCapability(definition.kind);

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

function providerStatusIsGood(status: ProviderStatus | null): boolean {
  return (
    !!status &&
    (status.reachable ||
      status.state === "reachable" ||
      status.state === "proven")
  );
}

function providerCardStateClass(provider: ProviderCardView): string {
  if (providerStatusIsGood(provider.status)) {
    return "healthy";
  }
  return provider.status?.state ?? "configured";
}

/// The order provider stages are presented in, mirroring the pipeline's.
const PROVIDER_STAGE_ORDER: readonly ProviderKind[] = [
  "stt",
  "llm",
  "tool",
  "tts",
  "transform",
  "wake",
  "speaker_id",
  "vad",
  "memory",
];

/// The columns every provider stage table shows, and how wide each one is.
///
/// Stated here rather than left to the browser because there is one table per
/// stage: each would size its own columns from its own rows, so a group holding
/// one long provider id put its Provider column somewhere no other group's was,
/// and the page read as several unrelated tables instead of one list under
/// headings. Fixed widths line them up — the cells wrap rather than widen, which
/// is why every one of them already handles overflow.
const PROVIDER_COLUMNS: readonly { label: string; width: string }[] = [
  { label: "Provider", width: "24%" },
  { label: "Implementation", width: "20%" },
  { label: "State", width: "22%" },
  { label: "Used by", width: "20%" },
  // Two or three buttons of a known size, so this is the one column whose
  // content decides its width rather than the other way round.
  { label: "Actions", width: "14%" },
];

/// Where each provider is bound, as "pipeline · stage", read from the stored
/// graphs rather than from runtime status: the graphs know the stage a binding
/// feeds, while `affects_pipelines` only names the pipeline.
function providerPipelineUses(
  providerIds: Set<string>,
  pipelineViews: readonly PipelineView[],
): Map<string, { pipeline: string; stage: string | null }[]> {
  const uses = new Map<string, { pipeline: string; stage: string | null }[]>();
  const add = (provider: string, pipeline: string, stage: string) => {
    if (!providerIds.has(provider)) {
      return;
    }
    const list = uses.get(provider) ?? [];
    if (!list.some((use) => use.pipeline === pipeline && use.stage === stage)) {
      list.push({ pipeline, stage });
      uses.set(provider, list);
    }
  };

  for (const view of pipelineViews) {
    const pipeline = view.graph.name;
    for (const node of view.graph.nodes) {
      const capability = capabilityForNodeKind(node.kind);
      if (capability) {
        add(nodeProvider(node), pipeline, providerKindLabel(capability));
      }
      if (node.kind === "core") {
        for (const tool of node.core.tools ?? []) {
          add(tool.provider, pipeline, "Tools");
        }
        for (const store of node.core.memory ?? []) {
          add(store.provider, pipeline, "Memory");
        }
      }
    }
  }
  return uses;
}

/// The places a provider is used, preferring the graph-derived bindings and
/// falling back to runtime status when a pipeline names the provider but no
/// stored graph is detailed enough to say which stage it feeds.
function providerPipelineUsesForCard(
  provider: ProviderCardView,
  uses: Map<string, { pipeline: string; stage: string | null }[]>,
): { pipeline: string; stage: string | null }[] {
  const fromGraphs = uses.get(provider.id) ?? [];
  if (fromGraphs.length > 0 || provider.status === null) {
    return fromGraphs;
  }
  return provider.status.affects_pipelines.map((pipeline) => ({
    pipeline,
    stage: null,
  }));
}

/// Whether an operator needs to look at a provider: it has runtime status and
/// that status is not healthy. A definition nobody has ever checked stays out
/// until the runtime has something to say about it.
function providerNeedsAttention(provider: ProviderCardView): boolean {
  return provider.status !== null && !providerStatusIsGood(provider.status);
}

function providerStateTone(
  status: ProviderStatus | null,
): "success" | "caution" | "danger" | "neutral" {
  if (status === null) {
    return "neutral";
  }
  if (status.state === "unavailable") {
    return "danger";
  }
  if (status.state === "configured") {
    return "caution";
  }
  return "success";
}

/// What the operator configured or the runtime registered, by the name the
/// catalog gives the component — the implementation column of the table.
function providerImplementationLabel(
  catalog: ProviderComponentCatalog,
  provider: ProviderCardView,
): string {
  if (provider.definition) {
    return (
      componentForProviderDefinition(catalog, provider.definition)?.label ??
      provider.definition.component
    );
  }
  return provider.status?.provider ?? provider.id;
}

function loadProviderDefinitions(
  catalog: ProviderComponentCatalog,
  pipelineViews: readonly PipelineView[],
  snapshot: OperatorStatusSnapshot | null,
  savedDefinitions: readonly ProviderDefinitionView[],
): ProviderDefinition[] {
  return mergeProviderDefinitions(
    defaultProviderDefinitions(catalog, pipelineViews, snapshot),
    savedDefinitions.map((definition) =>
      fromApiProviderDefinition(catalog, definition),
    ),
  );
}

function fromApiProviderDefinition(
  catalog: ProviderComponentCatalog,
  definition: ProviderDefinitionView,
): ProviderDefinition {
  const component = componentForApiProviderDefinition(catalog, definition);
  return {
    id: definition.id,
    label: definition.label,
    // Already a capability: a stored definition can only describe one an
    // operator can configure, so there is nothing to narrow here.
    kind: definition.kind,
    component: component?.id ?? definition.variant.type,
    config: configFromProviderVariant(definition.variant),
    source: "local",
  };
}

function componentForApiProviderDefinition(
  catalog: ProviderComponentCatalog,
  definition: Pick<ProviderDefinitionView, "kind" | "variant">,
): ProviderComponentDescriptor | null {
  const kind = definition.kind;
  if (definition.variant.type === "tool") {
    const transport = definition.variant.variant.transport.type;
    const componentId =
      transport === "streamable_http"
        ? "mcp.streamable_http"
        : `mcp.${transport}`;
    return (
      catalog.components.find(
        (component) => component.id === componentId && component.kind === kind,
      ) ?? null
    );
  }

  return (
    catalog.components.find(
      (component) =>
        component.definition_variant === definition.variant.variant.type &&
        component.kind === kind,
    ) ?? null
  );
}

function configFromProviderVariant(
  variant: ProviderDefinitionVariant,
): Record<string, unknown> {
  if (variant.type === "llm") {
    // Bedrock is the one model variant named by region rather than by URL: the
    // region *is* the endpoint, and the credential usually comes from the
    // deployment rather than from the form.
    if (variant.variant.type === "bedrock") {
      return {
        region: variant.variant.region,
        profile: variant.variant.profile ?? "",
        api_key: secretToConfigValue(variant.variant.api_key),
        model: variant.variant.models[0] ?? "",
        streaming: variant.variant.streaming,
        system_prompt: variant.variant.system_prompt ?? "",
      };
    }
    return {
      base_url: variant.variant.base_url,
      api_key: secretToConfigValue(variant.variant.api_key),
      model: variant.variant.models[0] ?? "",
      streaming: variant.variant.streaming,
      system_prompt: variant.variant.system_prompt ?? "",
    };
  }
  if (variant.type === "stt") {
    if (variant.variant.type === "openai") {
      return {
        base_url: variant.variant.base_url,
        api_key: secretToConfigValue(variant.variant.api_key),
        model: variant.variant.model,
        stream: variant.variant.stream,
      };
    }
    if (variant.variant.type === "elevenlabs") {
      return {
        api_key: secretToConfigValue(variant.variant.api_key),
        model: variant.variant.model ?? "",
      };
    }
    if (variant.variant.type === "google") {
      // No `api_key` key at all rather than an empty one: an empty string here
      // would come back as a field the form thinks it can fill.
      return {
        language: variant.variant.language ?? "",
        model: variant.variant.model ?? "",
      };
    }
    return {
      url: variant.variant.url,
      model: variant.variant.model ?? "",
      streaming: variant.variant.streaming,
    };
  }
  if (variant.type === "tts") {
    if (variant.variant.type === "openai") {
      return {
        base_url: variant.variant.base_url,
        api_key: secretToConfigValue(variant.variant.api_key),
        model: variant.variant.model,
        voices: variant.variant.voices.join(", "),
      };
    }
    if (variant.variant.type === "elevenlabs") {
      return {
        api_key: secretToConfigValue(variant.variant.api_key),
        model: variant.variant.model ?? "",
        voice: variant.variant.voice ?? "",
      };
    }
    if (variant.variant.type === "deepgram") {
      return {
        api_key: secretToConfigValue(variant.variant.api_key),
        model: variant.variant.model ?? "",
      };
    }
    // Named by region rather than by URL, like Bedrock — and with no key field
    // at all, because Polly has none. Nothing here is redacted on the way back,
    // so a reopened form shows exactly what was saved.
    if (variant.variant.type === "polly") {
      return {
        region: variant.variant.region,
        profile: variant.variant.profile ?? "",
        voice: variant.variant.voice ?? "",
        engine: variant.variant.engine ?? "",
      };
    }
    if (variant.variant.type === "google") {
      return {
        language: variant.variant.language ?? "",
        voice: variant.variant.voice ?? "",
      };
    }
    if (variant.variant.type === "marytts") {
      return {
        url: variant.variant.url,
        voice: variant.variant.voice ?? "",
        locale: variant.variant.locale ?? "",
      };
    }
    return {
      url: variant.variant.url,
      voice: variant.variant.voice ?? "",
      streaming: variant.variant.streaming,
    };
  }
  if (variant.type === "wake") {
    // The engine is the variant and the place is inside it, so the form flattens
    // the runtime back out: `where` picks which of the remaining fields matter.
    const runtime = variant.variant.runtime;
    const phrases = variant.variant.phrases.join(", ");
    if (runtime.where === "device") {
      return { where: runtime.where, phrases };
    }
    if (runtime.where === "local") {
      return {
        where: runtime.where,
        models_dir: runtime.models_dir ?? "",
        phrases,
        threshold_percent: runtime.threshold_percent,
      };
    }
    return {
      where: runtime.where,
      url: runtime.url,
      phrases,
      threshold_percent: runtime.threshold_percent,
    };
  }
  if (variant.type === "transform") {
    if (variant.variant.type === "script") {
      // The source verbatim: it is the definition rather than a secret in it, so
      // reopening the form has to show the script there is to edit.
      return {
        engine: variant.variant.engine,
        source: variant.variant.source,
        ...(variant.variant.timeout_ms === undefined
          ? {}
          : { timeout_ms: variant.variant.timeout_ms }),
      };
    }
    // Held as the text an operator is typing, and split when the definition is
    // built, like every other list field in this form.
    return { rules: variant.variant.rules.join(", ") };
  }
  if (variant.type === "memory") {
    if (variant.variant.type === "builtin") {
      return {
        path: variant.variant.path ?? "",
        // Written back as the number the server stored, so an operator sees the
        // bound they got rather than an empty box that looks unset.
        ...(typeof variant.variant.capacity === "number"
          ? { capacity: variant.variant.capacity }
          : {}),
      };
    }
    return {
      url: variant.variant.url,
      embedding_base_url: variant.variant.embedding_base_url,
      api_key: secretToConfigValue(variant.variant.api_key),
      embedding_model: variant.variant.embedding_model,
      dimensions: variant.variant.dimensions,
    };
  }
  if (variant.type === "vad") {
    return {
      ...(variant.variant.model_path
        ? { model_path: variant.variant.model_path }
        : {}),
      threshold_percent: variant.variant.threshold_percent,
      silence_ms: variant.variant.silence_ms,
    };
  }
  if (variant.type === "speaker_id") {
    if (variant.variant.type === "diarization_server") {
      return {
        base_url: variant.variant.base_url,
        threshold_percent: variant.variant.threshold_percent,
      };
    }
    return {
      base_url: variant.variant.base_url,
      api_key: secretToConfigValue(variant.variant.api_key),
      engine: variant.variant.engine,
      threshold_percent: variant.variant.threshold_percent,
    };
  }
  if (variant.variant.transport.type === "stdio") {
    return {
      command: variant.variant.transport.command,
      args: variant.variant.transport.args.join(" "),
    };
  }
  return { url: variant.variant.transport.url };
}

function toApiProviderDefinition(
  definition: ProviderDefinition,
): ApiProviderDefinition {
  return {
    id: definition.id,
    label: definition.label,
    variant: variantFromProviderDefinition(definition),
  };
}

function variantFromProviderDefinition(
  definition: ProviderDefinition,
): ProviderDefinitionVariant {
  const config = pruneEmptyConfig(definition.config);
  const text = (field: string) =>
    typeof config[field] === "string" ? config[field].trim() : "";
  const flag = (field: string) => config[field] === true;
  /// A comma-separated field, as the form holds it while it is being typed.
  const list = (field: string) => listEntries(config[field]);
  /// A number field. The default is the server's own, so a definition saved
  /// without one behaves as the API documents rather than as zero — which
  /// would be a detector that accepts everything it hears.
  const whole = (field: string) =>
    typeof config[field] === "number"
      ? (config[field] as number)
      : DEFAULT_THRESHOLD_PERCENT;
  const apiKey = secretFromConfig(text("api_key"));

  if (definition.component === "anthropic.messages") {
    return {
      type: "llm",
      variant: {
        type: "anthropic",
        // The public API is what an operator who typed nothing meant, unlike an
        // OpenAI-compatible server, which could be anywhere.
        base_url: text("base_url") || "https://api.anthropic.com/v1",
        ...(apiKey ? { api_key: apiKey } : {}),
        models: text("model") ? [text("model")] : [],
        streaming: flag("streaming"),
        ...(text("system_prompt")
          ? { system_prompt: text("system_prompt") }
          : {}),
      },
    };
  }
  if (definition.component === "bedrock.converse") {
    return {
      type: "llm",
      variant: {
        type: "bedrock",
        region: text("region"),
        ...(text("profile") ? { profile: text("profile") } : {}),
        ...(apiKey ? { api_key: apiKey } : {}),
        models: text("model") ? [text("model")] : [],
        streaming: flag("streaming"),
        ...(text("system_prompt")
          ? { system_prompt: text("system_prompt") }
          : {}),
      },
    };
  }
  // Every remaining language model component is an OpenAI-compatible endpoint,
  // including the named presets, which are that form with a URL already in it.
  // Keyed on the capability rather than on a list of component ids, so adding a
  // preset to the catalogue does not mean adding an arm here — and a component
  // this forgot would otherwise fall through to the Wyoming default and save a
  // model as a transcriber.
  if (definition.kind === "llm") {
    return {
      type: "llm",
      variant: {
        type: "openai",
        base_url: text("base_url"),
        ...(apiKey ? { api_key: apiKey } : {}),
        models: text("model") ? [text("model")] : [],
        streaming: flag("streaming"),
        ...(text("system_prompt")
          ? { system_prompt: text("system_prompt") }
          : {}),
      },
    };
  }
  if (definition.component === "openai.transcription") {
    return {
      type: "stt",
      variant: {
        type: "openai",
        base_url: text("base_url") || "https://api.openai.com/v1",
        model: text("model"),
        ...(apiKey ? { api_key: apiKey } : {}),
        stream: flag("stream"),
      },
    };
  }
  if (definition.component === "openai.speech") {
    return {
      type: "tts",
      variant: {
        type: "openai",
        base_url: text("base_url") || "https://api.openai.com/v1",
        model: text("model"),
        ...(apiKey ? { api_key: apiKey } : {}),
        voices: list("voices"),
      },
    };
  }
  if (definition.component === "elevenlabs.transcription") {
    return {
      type: "stt",
      variant: {
        type: "elevenlabs",
        // No base URL and no streaming flag: there is one ElevenLabs, and its
        // realtime transcription is a different protocol rather than a setting.
        ...(apiKey ? { api_key: apiKey } : {}),
        ...(text("model") ? { model: text("model") } : {}),
      },
    };
  }
  if (definition.component === "elevenlabs.speech") {
    return {
      type: "tts",
      variant: {
        type: "elevenlabs",
        ...(apiKey ? { api_key: apiKey } : {}),
        ...(text("model") ? { model: text("model") } : {}),
        ...(text("voice") ? { voice: text("voice") } : {}),
      },
    };
  }
  if (definition.component === "deepgram.speech") {
    return {
      type: "tts",
      variant: {
        type: "deepgram",
        // No base URL: there is one Deepgram, and the voice is the model, so a
        // single `model` field is the whole of what an operator chooses.
        ...(apiKey ? { api_key: apiKey } : {}),
        ...(text("model") ? { model: text("model") } : {}),
      },
    };
  }
  if (definition.component === "polly.speech") {
    return {
      type: "tts",
      variant: {
        type: "polly",
        // A region, not a base URL: the SDK builds the endpoint. And no
        // `api_key`, because Polly authenticates through the AWS credential
        // chain — a box to paste a key into would be a box that does nothing.
        region: text("region"),
        ...(text("profile") ? { profile: text("profile") } : {}),
        ...(text("voice") ? { voice: text("voice") } : {}),
        ...(text("engine") ? { engine: text("engine") } : {}),
      },
    };
  }
  if (definition.component === "google.transcription") {
    return {
      type: "stt",
      // No credential: Google's are discovered from the environment, so there
      // is nothing the form could have collected to send.
      variant: {
        type: "google",
        ...(text("language") ? { language: text("language") } : {}),
        ...(text("model") ? { model: text("model") } : {}),
      },
    };
  }
  if (definition.component === "google.speech") {
    return {
      type: "tts",
      variant: {
        type: "google",
        ...(text("language") ? { language: text("language") } : {}),
        ...(text("voice") ? { voice: text("voice") } : {}),
      },
    };
  }
  if (definition.component === "marytts") {
    return {
      type: "tts",
      variant: {
        type: "marytts",
        url: text("url"),
        ...(text("voice") ? { voice: text("voice") } : {}),
        ...(text("locale") ? { locale: text("locale") } : {}),
      },
    };
  }
  if (definition.component === "wyoming.tts") {
    return {
      type: "tts",
      variant: {
        type: "wyoming",
        url: text("url"),
        ...(text("voice") ? { voice: text("voice") } : {}),
        streaming: flag("streaming"),
      },
    };
  }
  if (definition.component === "microwakeword") {
    // The satellite is the reason microWakeWord exists on this list, so a
    // definition arriving without a place named means the device.
    const url = text("url");
    return {
      type: "wake",
      variant: {
        type: "microwakeword",
        runtime:
          text("where") === "wyoming"
            ? {
                where: "wyoming",
                url,
                threshold_percent: whole("threshold_percent"),
              }
            : { where: "device" },
        phrases: list("phrases"),
      },
    };
  }
  if (
    definition.component === "openwakeword" ||
    definition.component === "nanowakeword"
  ) {
    // Both are ONNX end-to-end, so both can be scored here or handed to a
    // server; the component says which engine, `where` says which of those.
    const modelsDir = text("models_dir");
    const runtime: WakeRuntime =
      text("where") === "wyoming"
        ? {
            where: "wyoming",
            url: text("url"),
            threshold_percent: whole("threshold_percent"),
          }
        : {
            where: "local",
            ...(modelsDir ? { models_dir: modelsDir } : {}),
            threshold_percent: whole("threshold_percent"),
          };
    return {
      type: "wake",
      variant: {
        type: definition.component,
        runtime,
        phrases: list("phrases"),
      },
    };
  }
  if (definition.component === "transform.builtin") {
    return {
      type: "transform",
      variant: {
        type: "builtin",
        // Order is what the operator wrote: flattening markdown before
        // stripping emoji means an emoji inside a link's text is seen as text.
        rules: list("rules") as TransformRule[],
      },
    };
  }
  if (definition.component === "transform.script") {
    return {
      type: "transform",
      variant: {
        type: "script",
        // Named rather than assumed, so the definition says what its source is
        // written in even though the menu offers one engine.
        engine: (text("engine") || "rhai") as ScriptEngine,
        // Untrimmed, unlike every other text field here: this is a program, and
        // an operator gets back what they wrote rather than a tidied version of
        // it.
        source: typeof config.source === "string" ? config.source : "",
        // Omitted rather than guessed when the box is empty: the server stores
        // its own deadline, and a zero here would be a script that can never
        // finish.
        ...(typeof config.timeout_ms === "number"
          ? { timeout_ms: config.timeout_ms }
          : {}),
      },
    };
  }
  if (definition.component === "memory.builtin") {
    return {
      type: "memory",
      variant: {
        type: "builtin",
        // Both omitted rather than guessed when the box is empty: an absent
        // path means nothing is written anywhere, and an absent bound means the
        // server's own. A `""` here would be a file in whatever directory the
        // server was started from, and a `0` a store that remembers nothing.
        ...(text("path") ? { path: text("path") } : {}),
        ...(typeof config.capacity === "number"
          ? { capacity: config.capacity }
          : {}),
      },
    };
  }
  if (definition.component === "memory.pgvector") {
    return {
      type: "memory",
      variant: {
        type: "pgvector",
        url: text("url"),
        embedding_base_url: text("embedding_base_url"),
        ...(apiKey ? { api_key: apiKey } : {}),
        embedding_model: text("embedding_model"),
        // Required, unlike every other number in this form: it is the width the
        // vector column is declared with, so there is no server-side default to
        // fall back to. Zero is refused by the API, which is what an empty box
        // becomes and what an operator is then told about.
        dimensions:
          typeof config.dimensions === "number" ? config.dimensions : 0,
      },
    };
  }
  if (definition.component === "silero.vad") {
    return {
      type: "vad",
      variant: {
        type: "silero",
        ...(text("model_path") ? { model_path: text("model_path") } : {}),
        threshold_percent: whole("threshold_percent"),
        silence_ms: whole("silence_ms"),
      },
    };
  }
  if (definition.component === "speaker.diarization_server") {
    return {
      type: "speaker_id",
      variant: {
        type: "diarization_server",
        base_url: text("base_url"),
        threshold_percent: whole("threshold_percent"),
      },
    };
  }
  if (definition.component === "speaker.http") {
    return {
      type: "speaker_id",
      variant: {
        type: "http",
        base_url: text("base_url"),
        ...(apiKey ? { api_key: apiKey } : {}),
        engine: (text("engine") || "speechbrain") as SpeakerEngine,
        threshold_percent: whole("threshold_percent"),
      },
    };
  }
  if (definition.component === "mcp.sse") {
    return {
      type: "tool",
      variant: { type: "mcp", transport: { type: "sse", url: text("url") } },
    };
  }
  if (definition.component === "mcp.streamable_http") {
    return {
      type: "tool",
      variant: {
        type: "mcp",
        transport: { type: "streamable_http", url: text("url") },
      },
    };
  }
  if (definition.component === "mcp.stdio") {
    return {
      type: "tool",
      variant: {
        type: "mcp",
        transport: {
          type: "stdio",
          command: text("command"),
          args: text("args").split(/\s+/).filter(Boolean),
        },
      },
    };
  }
  return {
    type: "stt",
    variant: {
      type: "wyoming",
      url: text("url"),
      ...(text("model") ? { model: text("model") } : {}),
      streaming: flag("streaming"),
    },
  };
}

function secretToConfigValue(secret: ProviderSecret | undefined): string {
  if (!secret) {
    return "";
  }
  if (secret.type === "inline") {
    return secret.value;
  }
  if (secret.type === "external") {
    return secret.reference;
  }
  return "";
}

function secretFromConfig(value: string): ProviderSecret | undefined {
  return value ? { type: "inline", value } : undefined;
}

function defaultProviderDefinitions(
  catalog: ProviderComponentCatalog,
  pipelineViews: readonly PipelineView[],
  snapshot: OperatorStatusSnapshot | null,
): ProviderDefinition[] {
  const fromGraphs: ProviderDefinition[] = pipelineViews.flatMap((view) =>
    view.graph.nodes.flatMap((node) => {
      const capability = capabilityForNodeKind(node.kind);
      if (!capability) {
        return [];
      }

      const component = componentForNode(catalog, node);
      return [
        {
          id: nodeProvider(node),
          label: nodeProvider(node),
          kind: capability,
          component: component?.id ?? nodeProvider(node),
          config: {},
          source: "inferred",
        },
      ];
    }),
  );
  const fromStatus: ProviderDefinition[] =
    snapshot?.providers.flatMap((provider) => {
      return [
        {
          id: provider.id,
          label: provider.id,
          kind: capabilityForProviderKind(provider.kind),
          component:
            componentForProviderStatus(catalog, provider)?.id ?? provider.id,
          config: {},
          source: "inferred" as const,
        },
        // A server's tools are bindable individually even though the server is
        // one provider. They are listed under it rather than reported beside
        // it, so this is where they become selectable — the Providers page still
        // shows one card for the one thing that was configured.
        ...(provider.offers_tools ?? []).map((tool) => ({
          id: tool,
          label: tool,
          kind: "tool" as const,
          component: provider.id,
          config: {},
          source: "inferred" as const,
          partOf: provider.id,
        })),
      ];
    }) ?? [];

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

function validateProviderDefinitionConfig(
  provider: ProviderDefinition,
  component: ProviderComponentDescriptor,
): PipelineValidationResult {
  const config = pruneEmptyConfig(provider.config);
  const missing = component.schema.required.filter((field) => {
    const value = config[field];
    // Checked against the type the field declares: a required number left
    // blank is missing, but a required number the operator set to zero is
    // answered, and a string test would call both of them empty.
    const property = component.schema.properties[field];
    if (property?.type === "boolean") {
      return value !== true;
    }
    if (property?.type === "integer") {
      return typeof value !== "number";
    }
    return typeof value !== "string" || value.trim().length === 0;
  });
  if (missing.length > 0) {
    return {
      ok: false,
      message: `Missing required fields: ${fieldLabels(missing)}`,
    };
  }

  for (const [field, property] of Object.entries(component.schema.properties)) {
    if (!(field in config)) {
      continue;
    }
    const value = config[field];
    if (property.type === "string" && typeof value !== "string") {
      return { ok: false, message: `${fieldLabel(field)} must be a string` };
    }
    if (property.type === "boolean" && typeof value !== "boolean") {
      return { ok: false, message: `${fieldLabel(field)} must be a boolean` };
    }
    if (property.type === "integer" && typeof value !== "number") {
      return { ok: false, message: `${fieldLabel(field)} must be a number` };
    }
    if (property.type === "string_list" && typeof value !== "string") {
      return { ok: false, message: `${fieldLabel(field)} must be a list` };
    }
    if (
      property.options &&
      property.options.length > 0 &&
      typeof value === "string"
    ) {
      // A list field holds several of the closed set, so each entry is checked
      // rather than the text as a whole.
      const entries =
        property.type === "string_list" ? listEntries(value) : [value];
      const unknown = entries.find(
        (entry) => !property.options?.includes(entry),
      );
      if (unknown !== undefined) {
        return {
          ok: false,
          message: `${fieldLabel(field)} must be one of ${property.options.join(", ")}`,
        };
      }
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
    return next;
  }
  if (typeof value !== "string" || value.length === 0) {
    delete next[field];
    return next;
  }
  if (property.type === "integer") {
    // A number, not the digits an operator typed: the server reads
    // `threshold_percent` as a number and refuses a string of one.
    const parsed = Number(value);
    if (Number.isFinite(parsed)) {
      next[field] = Math.trunc(parsed);
    } else {
      delete next[field];
    }
    return next;
  }
  next[field] = value;
  return next;
}

/// The entries a list field holds, read from the text the form keeps it as.
///
/// A list is held as the comma-separated text an operator types, because
/// splitting on every keystroke would trim the space they just typed. That one
/// representation serves every list field, open or closed, so everything that
/// needs the entries themselves — the tag list, validation, the definition
/// being built — reads them through here.
function listEntries(value: unknown): string[] {
  if (typeof value !== "string") {
    return [];
  }
  return value
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);
}

function pruneEmptyConfig(
  config: Record<string, unknown>,
): Record<string, unknown> {
  return Object.fromEntries(
    Object.entries(config).filter(([, value]) => value !== "" && value != null),
  );
}

/// The capability a status kind names.
///
/// Total now that a memory store has a definition variant. It was not: memory
/// used to be a kind the runtime could register and an operator could not write
/// down, so this returned `null` for it and every caller carried a branch for a
/// provider that appeared on the status page with nothing to edit.
function capabilityForProviderKind(kind: ProviderKind): ProviderCapability {
  return kind;
}

/// What a node's provider has to be able to do, when the node names one.
///
/// A core answers `llm` for its model; its tools and memory are bindings with
/// capabilities of their own rather than properties of the node.
function capabilityForNodeKind(kind: NodeKind): ProviderCapability | null {
  if (kind === "stt") {
    return "stt";
  }
  if (kind === "tts") {
    return "tts";
  }
  if (kind === "core") {
    return "llm";
  }
  if (kind === "wake_word") {
    return "wake";
  }
  if (kind === "speaker_id") {
    return "speaker_id";
  }
  if (kind === "vad") {
    return "vad";
  }
  if (kind === "transform") {
    return "transform";
  }
  return null;
}

/// A capability and a provider kind are the same vocabulary, seen from the
/// catalog and from the status snapshot.
function providerKindForCapability(kind: ProviderCapability): ProviderKind {
  return kind;
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
    if (item.kind === "utterance_segment") {
      const spoken = item.modality === "audio";
      return [
        {
          id: item.id,
          at: item.started_at,
          type:
            item.role === "assistant_preamble"
              ? "Assistant Preamble"
              : item.role === "tool_output"
                ? spoken
                  ? "Tool Spoken Output"
                  : "Tool Output"
                : "Assistant Response",
          // A text pipeline never ran synthesis, so attributing its segments
          // to that component would send an operator to a stage that did not
          // execute.
          component: spoken ? "synthesis" : "reasoning",
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
    event.type === "UtteranceSegmentStarted" ||
    event.type === "ToolBatchStarted"
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
    case "UtteranceSegmentStarted":
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
    case "UtteranceSegmentStarted":
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
  onPipelineSaved: (
    graph: PipelineGraph,
    providerDefinitions: readonly ApiProviderDefinition[],
  ) => Promise<void>;
}) {
  const [pipelineName, setPipelineName] = useState("default");
  const [shape, setShape] = useState<"voice" | "text">("voice");
  const [sttProvider, setSttProvider] = useState("whisper");
  const [llmProvider, setLlmProvider] = useState("openai");
  /// Which model the language model provider should serve.
  ///
  /// Asked here because a provider that advertises none leaves a pipeline
  /// nothing to request: resolution refuses rather than inventing a name, so
  /// setup that never asked produced a pipeline that could not run.
  const [llmModel, setLlmModel] = useState("gpt-4o-mini");
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
    const speech = shape === "voice";
    if (
      !llmProvider.trim() ||
      (speech && (!sttProvider.trim() || !ttsProvider.trim()))
    ) {
      setError("Provider settings are required");
      return;
    }

    setError(null);
    setSaving(true);
    try {
      const providerIds = {
        sttProvider: sttProvider.trim(),
        llmProvider: llmProvider.trim(),
        llmModel: llmModel.trim(),
        ttsProvider: ttsProvider.trim(),
      };
      await onPipelineSaved(
        speech
          ? buildMinimalVoiceLoopGraph({ name, ...providerIds })
          : buildMinimalTextLoopGraph({
              name,
              llmProvider: providerIds.llmProvider,
            }),
        guidedSetupProviderDefinitions(providerIds).filter(
          (definition) => speech || definition.id === providerIds.llmProvider,
        ),
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

        <label className="field">
          <span>Pipeline shape</span>
          <select
            value={shape}
            onChange={(event) =>
              setShape(event.target.value === "text" ? "text" : "voice")
            }
          >
            <option value="voice">Voice — speak and listen</option>
            <option value="text">Text — type and read</option>
          </select>
        </label>

        {providerSettingsOpen ? (
          <div className="provider-settings" aria-label="Provider Settings">
            {shape === "voice" ? (
              <label className="field">
                <span>Speech-to-text provider</span>
                <input
                  value={sttProvider}
                  onChange={(event) => setSttProvider(event.target.value)}
                />
              </label>
            ) : null}
            <label className="field">
              <span>Language model provider</span>
              <input
                value={llmProvider}
                onChange={(event) => setLlmProvider(event.target.value)}
              />
            </label>
            <label className="field">
              <span>Language model</span>
              <input
                aria-label="Language model"
                value={llmModel}
                onChange={(event) => setLlmModel(event.target.value)}
              />
            </label>
            {shape === "voice" ? (
              <label className="field">
                <span>Text-to-speech provider</span>
                <input
                  value={ttsProvider}
                  onChange={(event) => setTtsProvider(event.target.value)}
                />
              </label>
            ) : null}
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
  // A provider is a warning when nothing has reached it, not when no turn has
  // happened to use it. `reachable` is exactly "a probe succeeded", so testing
  // a provider can clear it — where `proven` could not: that needs a real turn
  // to exercise the provider, and a tool a model never calls never gets one.
  const providerWarnings = snapshot.providers.filter(
    (provider) => !provider.reachable,
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

function guidedSetupProviderDefinitions({
  sttProvider,
  llmProvider,
  llmModel,
  ttsProvider,
}: {
  sttProvider: string;
  llmProvider: string;
  llmModel: string;
  ttsProvider: string;
}): ApiProviderDefinition[] {
  return [
    {
      id: sttProvider,
      label: sttProvider,
      variant: {
        type: "stt",
        variant: {
          type: "openai",
          base_url: "https://api.openai.com/v1",
          model: "whisper-1",
          stream: false,
        },
      },
    },
    {
      id: llmProvider,
      label: llmProvider,
      variant: {
        type: "llm",
        variant: {
          type: "openai",
          base_url: "https://api.openai.com/v1",
          models: llmModel ? [llmModel] : [],
          streaming: true,
        },
      },
    },
    {
      id: ttsProvider,
      label: ttsProvider,
      variant: {
        type: "tts",
        variant: {
          type: "openai",
          base_url: "https://api.openai.com/v1",
          model: "tts-1",
          voices: [],
        },
      },
    },
  ];
}

function pipelineViewToValidation(
  view: PipelineView,
): PipelineValidationResult {
  return { ok: true, order: view.order };
}

/// Rewires a draft into one chain in stage order.
///
/// Every node is on that chain now: a core's tools and memory are bindings, so
/// there is nothing left that hangs off the pipeline rather than sitting in
/// it, and every edge is a link between two stages.
/// The smallest pipeline the configured providers can support, or `null` when
/// they cannot support one.
///
/// Voice when speech providers exist and text otherwise, because a language
/// model is the one thing no pipeline can do without. Built from providers
/// that are actually registered so the result validates — a graph naming a
/// provider nobody configured is refused on save, which is a poor way to
/// learn that setup is incomplete.
function minimalGraphFor(
  name: string,
  definitions: readonly ProviderDefinition[],
): PipelineGraph | null {
  const first = (kind: ProviderCapability) =>
    definitions.find((definition) => definition.kind === kind)?.id;

  const llm = first("llm");
  if (!llm) {
    return null;
  }

  const stt = first("stt");
  const tts = first("tts");
  return stt && tts
    ? buildMinimalVoiceLoopGraph({
        name,
        sttProvider: stt,
        llmProvider: llm,
        ttsProvider: tts,
      })
    : buildMinimalTextLoopGraph({ name, llmProvider: llm });
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
