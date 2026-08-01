import {
  Activity,
  Bell,
  Boxes,
  CircleAlert,
  CircleCheck,
  KeyRound,
  ListFilter,
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
} from "lucide-react";
import {
  type FormEvent,
  type ReactNode,
  useEffect,
  useMemo,
  useState,
} from "react";

import conduitLogo from "./assets/conduit-logo.png";
import "./App.css";
import { createSnapshotClient } from "./apiClient";
import type { NodeKind, PipelineGraph, PipelineView } from "./contracts/client";
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
  ProviderStatusState,
  RuntimeFailure,
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

interface AppProps {
  initialSnapshot?: OperatorStatusSnapshot;
  initialEvents?: readonly EventEnvelope[];
  initialEventPosture?: EventStreamPosture;
  initialPipelineViews?: readonly PipelineView[];
  initialSmallScreen?: boolean;
  onPipelineSaved?: (graph: PipelineGraph) => void;
  onPipelineValidate?: (graph: PipelineGraph) => PipelineValidationResult;
}

function App({
  initialSnapshot,
  initialEvents,
  initialEventPosture,
  initialPipelineViews,
  initialSmallScreen = false,
  onPipelineSaved,
  onPipelineValidate,
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
      initialPipelineViews={initialPipelineViews}
      initialSmallScreen={initialSmallScreen}
      initialSnapshot={initialSnapshot}
      onPipelineSaved={onPipelineSaved}
      onPipelineValidate={onPipelineValidate}
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
  initialPipelineViews,
  initialSmallScreen,
  initialSnapshot,
  onPipelineSaved,
  onPipelineValidate,
  onSectionChange,
  onClearAccess,
}: {
  access: OperatorAccess;
  activeSection: SectionId;
  initialEvents?: readonly EventEnvelope[];
  initialEventPosture?: EventStreamPosture;
  initialPipelineViews?: readonly PipelineView[];
  initialSmallScreen: boolean;
  initialSnapshot?: OperatorStatusSnapshot;
  onPipelineSaved?: (graph: PipelineGraph) => void;
  onPipelineValidate?: (graph: PipelineGraph) => PipelineValidationResult;
  onSectionChange: (section: SectionId) => void;
  onClearAccess: () => void;
}) {
  const snapshotClient = useMemo(
    () =>
      createSnapshotClient({
        baseUrl: window.location.origin,
        access,
        snapshot: initialSnapshot,
      }),
    [access, initialSnapshot],
  );
  const [snapshot, setSnapshot] = useState(snapshotClient.snapshot);
  const [pipelineViews, setPipelineViews] = useState<readonly PipelineView[]>(
    () => initialPipelineViews ?? defaultPipelineViews(snapshotClient.snapshot),
  );
  const smallScreen = useSmallScreenMode(initialSmallScreen);
  const eventPlan = useMemo(() => {
    const plan = initialEventStreamPlan();
    return initialEventPosture
      ? { ...plan, posture: initialEventPosture }
      : plan;
  }, [initialEventPosture]);
  const firstRun = snapshot?.runtime.launch_state === "first_run_setup";
  const accessLabel =
    access.mode === "anonymous"
      ? "Anonymous operator access"
      : access.mode === "bearer" && access.persisted
        ? "Remembered management token"
        : "Session management token";

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
            <StatusPill
              label="Snapshot"
              value={snapshotClient.state}
              tone="caution"
            />
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
          pipelineViews={pipelineViews}
          snapshot={snapshot}
          eventPosture={eventPlan.posture}
          initialSmallScreen={smallScreen}
          onSectionChange={onSectionChange}
          onPipelineValidate={onPipelineValidate ?? validatePipelineGraph}
          onPipelineSaved={(graph) => {
            onPipelineSaved?.(graph);
            setPipelineViews((current) => upsertPipelineView(current, graph));
            setSnapshot((current) => promoteSavedPipeline(current, graph));
            onSectionChange("overview");
          }}
          onPipelineStored={(graph, order) => {
            onPipelineSaved?.(graph);
            setPipelineViews((current) =>
              upsertPipelineView(current, graph, order),
            );
          }}
        />
      </section>
    </main>
  );
}

function SectionPanel({
  section,
  events,
  pipelineViews,
  snapshot,
  eventPosture,
  initialSmallScreen,
  onSectionChange,
  onPipelineSaved,
  onPipelineStored,
  onPipelineValidate,
}: {
  section: SectionId;
  events: readonly EventEnvelope[];
  pipelineViews: readonly PipelineView[];
  snapshot: OperatorStatusSnapshot | null;
  eventPosture: EventStreamPosture;
  initialSmallScreen: boolean;
  onSectionChange: (section: SectionId) => void;
  onPipelineSaved: (graph: PipelineGraph) => void;
  onPipelineStored: (graph: PipelineGraph, order: string[]) => void;
  onPipelineValidate: (graph: PipelineGraph) => PipelineValidationResult;
}) {
  if (snapshot?.runtime.launch_state === "first_run_setup") {
    return <GuidedSetupPanel onPipelineSaved={onPipelineSaved} />;
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
    return <EventsPanel events={events} eventPosture={eventPosture} />;
  }

  if (section === "pipelines") {
    return (
      <PipelinesPanel
        pipelineViews={pipelineViews}
        readOnly={initialSmallScreen}
        snapshot={snapshot}
        onPipelineStored={onPipelineStored}
        onPipelineValidate={onPipelineValidate}
      />
    );
  }

  if (section === "providers") {
    return (
      <ProvidersPanel
        pipelineViews={pipelineViews}
        providers={snapshot?.providers ?? []}
      />
    );
  }

  return <SettingsPanel pipelineViews={pipelineViews} access={snapshot} />;
}

type ProviderFilter = "all" | ProviderKind;

interface ProviderOverride {
  state?: ProviderStatusState;
  reachable?: boolean;
  message?: string;
  fallbackSelected?: boolean;
}

function ProvidersPanel({
  pipelineViews,
  providers,
}: {
  pipelineViews: readonly PipelineView[];
  providers: readonly ProviderStatus[];
}) {
  const [filter, setFilter] = useState<ProviderFilter>("all");
  const [overrides, setOverrides] = useState<Record<string, ProviderOverride>>(
    {},
  );
  const visibleProviders = providers
    .map((provider) => ({ ...provider, ...overrides[provider.id] }))
    .filter((provider) => filter === "all" || provider.kind === filter);
  const providerKinds: ProviderFilter[] = ["all", "stt", "llm", "tool", "tts"];
  const referencedProviderCount = new Set(
    pipelineViews.flatMap((view) =>
      view.graph.nodes.map((node) => node.provider),
    ),
  ).size;

  function testProvider(provider: ProviderStatus) {
    setOverrides((current) => ({
      ...current,
      [provider.id]: {
        ...current[provider.id],
        state: "reachable",
        reachable: true,
        message: "Reachability check passed",
      },
    }));
  }

  function selectFallback(provider: ProviderStatus) {
    setOverrides((current) => ({
      ...current,
      [provider.id]: {
        ...current[provider.id],
        fallbackSelected: true,
        message: "Local fallback is selected",
      },
    }));
  }

  return (
    <div className="providers-stack">
      <section className="summary-grid" aria-label="Provider summary">
        <MetricTile
          label="Visible providers"
          value={`${visibleProviders.length} visible`}
        />
        <MetricTile
          label="Snapshot providers"
          value={providers.length.toString()}
        />
        <MetricTile
          label="Referenced by graphs"
          value={referencedProviderCount.toString()}
        />
        <MetricTile
          label="Warnings"
          value={providers
            .filter((provider) => provider.state !== "proven")
            .length.toString()}
        />
      </section>

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

      <section className="provider-card-grid" aria-label="Provider cards">
        {visibleProviders.map((provider) => (
          <article
            className={`provider-card ${provider.state}`}
            key={provider.id}
          >
            <div className="provider-card-header">
              <span className="provider-kind">
                {provider.kind.toUpperCase()}
              </span>
              <StatusPill
                label="Status"
                value={provider.state}
                tone={provider.state === "proven" ? "neutral" : "caution"}
              />
            </div>
            <h2>{provider.id}</h2>
            <p>{provider.message ?? "Provider has real turn proof"}</p>

            <div className="provider-facts">
              <div>
                <span>Configured</span>
                <strong>{provider.configured ? "yes" : "no"}</strong>
              </div>
              <div>
                <span>Reachable</span>
                <strong>{provider.reachable ? "yes" : "no"}</strong>
              </div>
              <div>
                <span>Pipelines</span>
                <strong>
                  {provider.affects_pipelines.join(", ") || "none"}
                </strong>
              </div>
            </div>

            <div className="provider-actions">
              <button
                className="secondary-action"
                type="button"
                onClick={() => testProvider(provider)}
              >
                <Play size={17} aria-hidden="true" />
                Test {provider.id}
              </button>
              <button
                className="secondary-action"
                type="button"
                onClick={() => selectFallback(provider)}
              >
                <RotateCcw size={17} aria-hidden="true" />
                Use fallback
              </button>
            </div>

            {overrides[provider.id]?.fallbackSelected ? (
              <p className="panel-notice">
                Fallback selected for {provider.id}
              </p>
            ) : null}
          </article>
        ))}
        {visibleProviders.length === 0 ? (
          <div className="overview-empty" role="status">
            <Boxes size={18} aria-hidden="true" />
            <span>No providers match this stage filter</span>
          </div>
        ) : null}
      </section>
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
  const [deploymentName, setDeploymentName] = useState("conduit-local");
  const [localOnly, setLocalOnly] = useState(true);
  const [defaultPipeline, setDefaultPipeline] = useState(
    pipelineViews[0]?.graph.name ?? access?.pipelines[0]?.name ?? "default",
  );
  const [retention, setRetention] = useState("30 d");
  const [logLevel, setLogLevel] = useState("info");
  const [resetOpen, setResetOpen] = useState(false);
  const [resetConfirmation, setResetConfirmation] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const retentionOptions = ["7 d", "30 d", "90 d", "forever"];

  function saveSettings() {
    setNotice(`Settings saved for ${deploymentName.trim() || "conduit-local"}`);
  }

  function resetLocalState() {
    if (resetConfirmation !== "RESET") {
      setNotice("Type RESET to confirm");
      return;
    }

    setDeploymentName("conduit-local");
    setLocalOnly(true);
    setDefaultPipeline(pipelineViews[0]?.graph.name ?? "default");
    setRetention("30 d");
    setLogLevel("info");
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
              onChange={(event) => setDeploymentName(event.target.value)}
            />
          </label>
          <label className="field">
            <span>Default pipeline</span>
            <select
              value={defaultPipeline}
              onChange={(event) => setDefaultPipeline(event.target.value)}
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
              onChange={(event) => setLocalOnly(event.target.checked)}
            />
          </label>
          <label className="field">
            <span>Log level</span>
            <select
              aria-label="Log level"
              value={logLevel}
              onChange={(event) => setLogLevel(event.target.value)}
            >
              {["debug", "info", "warn", "error"].map((level) => (
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
              onClick={() => setRetention(option)}
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
  eventPosture,
}: {
  events: readonly EventEnvelope[];
  eventPosture: EventStreamPosture;
}) {
  const [activeView, setActiveView] = useState<"story" | "raw">("story");
  const [filter, setFilter] = useState("");
  const turn = useMemo(() => reconstructTurn(events), [events]);
  const rawEvents = useMemo(
    () => filterRawEvents(events, filter),
    [events, filter],
  );
  const stale = eventPosture === "stale" || eventPosture === "disconnected";

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
              <p className="eyebrow">
                {turn.pipeline} / {turn.conversation}
              </p>
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
                      <span key={step.id}>{displayEventType(step.type)}</span>
                    ))}
                  </div>
                </article>
              ))}
            </div>
          </section>

          <ol className="event-story">
            {turn.steps.map((step) => (
              <li className="event-step" key={step.id}>
                <div className="event-meta">
                  <strong>{step.type}</strong>
                  <span>{step.component}</span>
                  <time dateTime={step.at}>{formatTime(step.at)}</time>
                </div>
                {step.detail ? <p>{step.detail}</p> : null}
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
  pipelineViews,
  readOnly,
  snapshot,
  onPipelineStored,
  onPipelineValidate,
}: {
  pipelineViews: readonly PipelineView[];
  readOnly: boolean;
  snapshot: OperatorStatusSnapshot | null;
  onPipelineStored: (graph: PipelineGraph, order: string[]) => void;
  onPipelineValidate: (graph: PipelineGraph) => PipelineValidationResult;
}) {
  const [selectedName, setSelectedName] = useState(
    pipelineViews[0]?.graph.name ?? "",
  );
  const selectedView =
    pipelineViews.find((view) => view.graph.name === selectedName) ??
    pipelineViews[0] ??
    null;
  const [draft, setDraft] = useState<PipelineGraph | null>(
    selectedView ? cloneGraph(selectedView.graph) : null,
  );
  const [history, setHistory] = useState<PipelineGraph[]>([]);
  const [validation, setValidation] = useState<PipelineValidationResult | null>(
    null,
  );
  const [notice, setNotice] = useState<string | null>(null);
  const selectedHealth = snapshot?.pipelines.find(
    (pipeline) => pipeline.name === selectedView?.graph.name,
  );

  function selectPipeline(view: PipelineView) {
    setSelectedName(view.graph.name);
    setDraft(cloneGraph(view.graph));
    setHistory([]);
    setValidation(null);
    setNotice(null);
  }

  function applyDraftEdit(edit: (graph: PipelineGraph) => PipelineGraph) {
    if (!draft) {
      return;
    }

    setHistory((current) => [...current, cloneGraph(draft)]);
    setDraft(edit(cloneGraph(draft)));
    setValidation(null);
    setNotice(null);
  }

  function addNodeAfter({
    id,
    kind,
    provider,
    from,
    to,
  }: {
    id: string;
    kind: NodeKind;
    provider: string;
    from: string;
    to?: string;
  }) {
    applyDraftEdit((graph) => {
      if (graph.nodes.some((node) => node.id === id)) {
        return graph;
      }

      const edges = graph.edges.filter(
        (edge) => !(to && edge.from === from && edge.to === to),
      );
      return {
        ...graph,
        nodes: [...graph.nodes, { id, kind, provider }],
        edges: [...edges, { from, to: id }, ...(to ? [{ from: id, to }] : [])],
      };
    });
  }

  function addToolNode() {
    addNodeAfter({
      id: "confirm",
      kind: "tool",
      provider: "builtin.confirm",
      from: "llm",
      to: "tts",
    });
  }

  function addMemoryNode() {
    addNodeAfter({
      id: "memory",
      kind: "memory",
      provider: "builtin.memory",
      from: "stt",
      to: "llm",
    });
  }

  function addFallbackTts() {
    addNodeAfter({
      id: "tts_fallback",
      kind: "tts",
      provider: "system-tts",
      from: "llm",
      to: "speaker",
    });
  }

  function undoLastEdit() {
    const previous = history.at(-1);
    if (!previous) {
      return;
    }

    setDraft(previous);
    setHistory((current) => current.slice(0, -1));
    setValidation(null);
    setNotice(null);
  }

  function validateDraft() {
    if (!draft) {
      return;
    }

    setValidation(onPipelineValidate(draft));
  }

  function saveDraft() {
    if (!draft || validation?.ok !== true) {
      return;
    }

    onPipelineStored(draft, validation.order);
    setHistory([]);
    setNotice(`Saved graph for ${draft.name}`);
  }

  function runTestTurn() {
    setNotice(`Test turn queued for ${draft?.name ?? selectedName}`);
  }

  if (!draft || !selectedView) {
    return (
      <div className="overview-empty" role="status">
        <Workflow size={18} aria-hidden="true" />
        <span>No stored pipeline graphs</span>
      </div>
    );
  }

  return (
    <div className="pipelines-stack">
      <section className="pipeline-toolbar" aria-label="Stored pipelines">
        <div>
          <p className="eyebrow">Advanced configuration</p>
          <h2>Graph Editor</h2>
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
        {history.length > 0 ? (
          <span className="edit-badge">
            {history.length} unsaved {history.length === 1 ? "edit" : "edits"}
          </span>
        ) : null}
      </section>

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
          <div className="graph-nodes">
            {draft.nodes.map((node, index) => (
              <article className="graph-node" key={node.id}>
                <span>{index + 1}</span>
                <strong>{node.id}</strong>
                <p>
                  {node.kind} / {node.provider}
                </p>
              </article>
            ))}
          </div>
          <div className="graph-edges" aria-label="Pipeline edges">
            {draft.edges.map((edge) => (
              <span key={`${edge.from}-${edge.to}-${edge.port ?? "default"}`}>
                {edge.from} -&gt; {edge.to}
              </span>
            ))}
          </div>
        </section>

        <aside className="pipeline-side-panel">
          <StatusPill
            label="Health"
            value={selectedHealth?.health.state ?? "unknown"}
            tone={
              selectedHealth?.health.state === "healthy" ? "neutral" : "caution"
            }
          />
          <p>
            {selectedHealth?.health.summary ?? "No snapshot health available"}
          </p>

          <div className="compact-list">
            {selectedHealth?.components.map((component) => (
              <div className="metric-row" key={component.kind}>
                <span>
                  {component.kind}
                  {component.provider ? ` / ${component.provider}` : ""}
                </span>
                <strong className={`state-text ${component.state}`}>
                  {component.state}
                </strong>
              </div>
            ))}
          </div>

          {!readOnly ? (
            <div className="graph-actions">
              <button
                className="secondary-action"
                type="button"
                disabled={history.length === 0}
                onClick={undoLastEdit}
              >
                <RotateCcw size={17} aria-hidden="true" />
                Undo last edit
              </button>
              <button
                className="secondary-action"
                type="button"
                onClick={addToolNode}
              >
                <Plus size={17} aria-hidden="true" />
                Add tool node
              </button>
              <button
                className="secondary-action"
                type="button"
                onClick={addMemoryNode}
              >
                <Plus size={17} aria-hidden="true" />
                Add memory node
              </button>
              <button
                className="secondary-action"
                type="button"
                onClick={addFallbackTts}
              >
                <Plus size={17} aria-hidden="true" />
                Add fallback TTS
              </button>
              <button
                className="secondary-action"
                type="button"
                onClick={runTestTurn}
              >
                <Play size={17} aria-hidden="true" />
                Run test turn
              </button>
              <button
                className="secondary-action"
                type="button"
                onClick={validateDraft}
              >
                <CircleCheck size={17} aria-hidden="true" />
                Validate Graph
              </button>
              <button
                className="primary-action"
                type="button"
                disabled={validation?.ok !== true}
                onClick={saveDraft}
              >
                <Save size={17} aria-hidden="true" />
                Save Graph
              </button>
            </div>
          ) : null}

          {notice ? <p className="panel-notice">{notice}</p> : null}

          {validation ? (
            <p className={validation.ok ? "validation-ok" : "form-error"}>
              {validation.ok ? "Validation passed" : validation.message}
            </p>
          ) : null}
        </aside>
      </div>
    </div>
  );
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

type TurnStatus = "completed" | "failed" | "cancelled" | "running";

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
  type: Event["type"];
  component: string;
  detail: string | null;
}

function reconstructTurn(events: readonly EventEnvelope[]): ReconstructedTurn {
  const ordered = [...events].sort((left, right) =>
    left.at.localeCompare(right.at),
  );
  const steps = ordered.map((envelope) => ({
    id: envelope.id,
    at: envelope.at,
    type: envelope.event.type,
    component: eventComponent(envelope.event),
    detail: eventDetail(envelope.event),
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
    groups.set(step.component, [...(groups.get(step.component) ?? []), step]);
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
    case "ToolStarted":
    case "ToolConfirmationRequested":
    case "ToolCompleted":
    case "ToolFailed":
      return "tools";
    case "TtsStarted":
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
    case "ToolStarted":
    case "ToolCompleted":
      return event.call;
    case "ToolConfirmationRequested":
      return event.prompt;
    case "ToolFailed":
    case "StageFailed":
      return event.error;
    case "ConversationStarted":
    case "ConversationCompleted":
    case "TtsStarted":
      return "boundary";
  }
}

function displayEventType(type: Event["type"]): string {
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
  onPipelineSaved: (graph: PipelineGraph) => void;
}) {
  const [pipelineName, setPipelineName] = useState("default");
  const [sttProvider, setSttProvider] = useState("whisper");
  const [llmProvider, setLlmProvider] = useState("openai");
  const [ttsProvider, setTtsProvider] = useState("piper");
  const [providerSettingsOpen, setProviderSettingsOpen] = useState(false);
  const [toolSetupSkipped, setToolSetupSkipped] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function save(event: FormEvent<HTMLFormElement>) {
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
    onPipelineSaved(
      buildMinimalVoiceLoopGraph({
        name,
        sttProvider: sttProvider.trim(),
        llmProvider: llmProvider.trim(),
        ttsProvider: ttsProvider.trim(),
      }),
    );
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

        <button className="primary-action" type="submit">
          <CircleCheck size={17} aria-hidden="true" />
          Validate and Save
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
          <div className="exception-list">
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

function validatePipelineGraph(graph: PipelineGraph): PipelineValidationResult {
  if (graph.nodes.length === 0) {
    return { ok: false, message: "graph has no nodes" };
  }

  const ids = new Set(graph.nodes.map((node) => node.id));
  const dangling = graph.edges.find(
    (edge) => !ids.has(edge.from) || !ids.has(edge.to),
  );
  if (dangling) {
    return {
      ok: false,
      message: `unknown node in edge ${dangling.from} -> ${dangling.to}`,
    };
  }

  return { ok: true, order: graph.nodes.map((node) => node.id) };
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

function promoteSavedPipeline(
  snapshot: OperatorStatusSnapshot | null,
  graph: PipelineGraph,
): OperatorStatusSnapshot | null {
  if (!snapshot) {
    return snapshot;
  }

  return {
    ...snapshot,
    runtime: {
      ...snapshot.runtime,
      launch_state: "operations_workspace",
    },
    pipelines: [
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
              graph.nodes.find((node) => node.id === "stt")?.provider ?? null,
            state: "unproven",
            detail: "pipeline saved",
            last_turn: null,
          },
          {
            kind: "reasoning",
            provider:
              graph.nodes.find((node) => node.id === "llm")?.provider ?? null,
            state: "unproven",
            detail: "pipeline saved",
            last_turn: null,
          },
          {
            kind: "synthesis",
            provider:
              graph.nodes.find((node) => node.id === "tts")?.provider ?? null,
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
    <article className="exception-item critical">
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
    <article className="exception-item">
      <Bell size={17} aria-hidden="true" />
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
    <article className="exception-item">
      <Boxes size={17} aria-hidden="true" />
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
