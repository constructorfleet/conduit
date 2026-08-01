import {
  Activity,
  Bell,
  Boxes,
  CircleAlert,
  CircleCheck,
  KeyRound,
  Network,
  Radio,
  Settings,
  SlidersHorizontal,
  Workflow,
} from "lucide-react";
import { type FormEvent, type ReactNode, useMemo, useState } from "react";

import conduitLogo from "./assets/conduit-logo.png";
import "./App.css";
import { createSnapshotClient } from "./apiClient";
import type {
  OperatorStatusSnapshot,
  PipelineStatus,
  ProviderStatus,
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

function App() {
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
  onSectionChange,
  onClearAccess,
}: {
  access: OperatorAccess;
  activeSection: SectionId;
  onSectionChange: (section: SectionId) => void;
  onClearAccess: () => void;
}) {
  const snapshotClient = useMemo(
    () => createSnapshotClient({ baseUrl: window.location.origin, access }),
    [access],
  );
  const eventPlan = useMemo(() => initialEventStreamPlan(), []);
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
              {sections.find((section) => section.id === activeSection)?.label}
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
          snapshot={snapshotClient.snapshot}
          eventPosture={eventPlan.posture}
        />
      </section>
    </main>
  );
}

function SectionPanel({
  section,
  snapshot,
  eventPosture,
}: {
  section: SectionId;
  snapshot: OperatorStatusSnapshot | null;
  eventPosture: EventStreamPosture;
}) {
  if (section === "overview") {
    return <OverviewPanel snapshot={snapshot} eventPosture={eventPosture} />;
  }

  const content: Record<Exclude<SectionId, "overview">, string[]> = {
    pipelines: ["Guided Setup", "Graph Editor", "Validation"],
    providers: ["Provider Settings", "Reachability", "Real turn proof"],
    events: ["Turn Reconstruction", "Raw stream"],
    settings: ["Operator Access", "Deployment", "Snapshot plus events"],
  };

  return (
    <div className="section-band">
      {content[section].map((item) => (
        <div className="surface" key={item}>
          <SlidersHorizontal size={18} aria-hidden="true" />
          <span>{item}</span>
        </div>
      ))}
    </div>
  );
}

export function OverviewPanel({
  snapshot,
  eventPosture,
}: {
  snapshot: OperatorStatusSnapshot | null;
  eventPosture: EventStreamPosture;
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
              />
            ))}
            {unhealthyPipelines.map((pipeline) => (
              <PipelineException key={pipeline.name} pipeline={pipeline} />
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

function FailureItem({ failure }: { failure: RuntimeFailure }) {
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
      </div>
    </article>
  );
}

function PipelineException({ pipeline }: { pipeline: PipelineStatus }) {
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
