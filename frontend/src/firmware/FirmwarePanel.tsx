/// The console affordance for a satellite's rendered firmware fragment.
///
/// Track D of docs/specs/0003-firmware-fragment-rendering.md. An operator fills
/// in the ids their hand-written board file declares, renders, reads the YAML,
/// and saves it beside that board file.
///
/// The fragment is shown before it is saved rather than downloaded blind.
/// Applying one means reflashing a device, so the operator gets to see what they
/// are about to apply — and a rendered fragment is short enough to read.
///
/// Per [ADR-0019][adr], this download is permanent rather than a stopgap: when a
/// configured ESPHome instance is unreachable, the page degrades to "here is
/// your fragment, apply it yourself" rather than to a dead button.
///
/// Track E adds the hand-off to that instance: upload, then a link out to it for
/// the build and install. Deliberately not an embedded `<esp-web-install-button>`
/// — Conduit does not compile, does not hold an image, and has nothing to serve
/// one from, because an ESPHome build bakes `secrets.yaml` into the binary.
///
/// [adr]: ../../../docs/adr/0019-flashing-through-an-esphome-instance-conduit-does-not-own.md

import { Download, FileCode2, UploadCloud } from "lucide-react";
import { type FormEvent, useState } from "react";

import type {
  FirmwareFlashResult,
  FirmwareRenderRequest,
} from "../contracts/client";

/// Renders a fragment, or rejects with what the server said about why not.
export type FirmwareRenderer = (
  device: string,
  request: FirmwareRenderRequest,
) => Promise<string>;

/// Uploads a fragment to the configured ESPHome dashboard, or rejects saying
/// why — including that no dashboard is configured, which is a normal answer.
export type FirmwareFlasher = (
  device: string,
  request: FirmwareRenderRequest,
) => Promise<FirmwareFlashResult>;

/// The board ids and dial settings an operator types.
///
/// Every field is a string, including the numbers: an operator clearing a field
/// leaves it empty rather than zero, and turning that into "not asked for"
/// rather than `0` is what keeps a blank field from rendering a gain of nothing.
export interface FirmwareFormState {
  device: string;
  base_device: "" | "sat1" | "voicepe";
  pipeline: string;
  server: string;
  scheme: "ws" | "wss";
  max_utterance_ms: string;
  debug_udp_host: string;
  debug_udp_port: string;
}

/// No board ids are defaulted, deliberately.
///
/// The endpoint refuses a missing microphone rather than assuming one, because a
/// default would render a fragment that compiles cleanly against somebody
/// else's board. The console does not paper over that with a placeholder.
export const emptyFirmwareForm: FirmwareFormState = {
  device: "",
  base_device: "",
  pipeline: "",
  server: "",
  scheme: "ws",
  max_utterance_ms: "",
  debug_udp_host: "",
  debug_udp_port: "",
};

interface BaseDeviceProfile {
  label: string;
  microphone: string;
  speaker: string;
  mute_switch: string;
  gain_factor: number;
}

type BaseDevice = Exclude<FirmwareFormState["base_device"], "">;

const BASE_DEVICE_PROFILES = {
  sat1: {
    label: "Sat1",
    microphone: "sat1_mics",
    speaker: "announcement_resampling_speaker",
    mute_switch: "master_mute_switch",
    gain_factor: 6,
  },
  voicepe: {
    label: "VoicePE",
    microphone: "i2s_mics",
    speaker: "announcement_resampling_speaker",
    mute_switch: "master_mute_switch",
    gain_factor: 4,
  },
} satisfies Record<BaseDevice, BaseDeviceProfile>;

/// The fields the endpoint has no default for in the console itself.
const REQUIRED: readonly (keyof FirmwareFormState)[] = [
  "device",
  "base_device",
  "pipeline",
  "server",
];

/// The request a filled-in form describes.
///
/// Optional fields are omitted when blank rather than sent empty, so the
/// endpoint can tell "not asked for" from "asked for, blank" — which is how an
/// empty `debug_udp_host` stays the way both boards disable UDP debug.
export function firmwareRequestFrom(
  form: FirmwareFormState,
): FirmwareRenderRequest {
  const baseDevice = form.base_device
    ? BASE_DEVICE_PROFILES[form.base_device]
    : null;
  const request: FirmwareRenderRequest = {
    pipeline: form.pipeline.trim(),
    microphone: baseDevice?.microphone ?? "",
    speaker: baseDevice?.speaker ?? "",
    mute_switch: baseDevice?.mute_switch ?? "",
    gain_factor: baseDevice?.gain_factor ?? Number.NaN,
    server: form.server.trim(),
  };

  if (form.scheme !== "ws") {
    request.scheme = form.scheme;
  }
  if (form.max_utterance_ms.trim()) {
    request.max_utterance_ms = Number(form.max_utterance_ms);
  }
  // Sent whenever it was typed at all, including as the empty string an
  // operator uses to turn debug mirroring off.
  if (form.debug_udp_host !== "") {
    request.debug_udp_host = form.debug_udp_host.trim();
  }
  if (form.debug_udp_port.trim()) {
    request.debug_udp_port = Number(form.debug_udp_port);
  }

  return request;
}

/// The file name the fragment is saved under.
///
/// Matches what the board files include — `conduit-sat1.yaml` includes
/// `conduit-sat1.conduit.yaml` — so a rendered file lands with the name the
/// board already names, and re-rendering overwrites rather than accumulating.
export function fragmentFileName(device: string): string {
  const slug = device
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-");
  return `conduit-${slug || "device"}.conduit.yaml`;
}

/// Which required fields are still blank.
function missingFields(form: FirmwareFormState): (keyof FirmwareFormState)[] {
  return REQUIRED.flatMap((field) => {
    const value = form[field];
    return typeof value === "string" && value.trim() ? [] : [field];
  });
}

function labelFor(field: keyof FirmwareFormState): string {
  switch (field) {
    case "base_device":
      return "base device";
    default:
      return field.replaceAll("_", " ");
  }
}

function messageOf(caught: unknown, fallback: string): string {
  return caught instanceof Error && caught.message ? caught.message : fallback;
}

export function FirmwarePanel({
  pipelineNames,
  onRender,
  onFlash,
}: {
  /// The stored pipelines, offered as a list because a fragment naming a
  /// pipeline that does not exist renders a device that cannot connect.
  pipelineNames: readonly string[];
  onRender: FirmwareRenderer;
  onFlash: FirmwareFlasher;
}) {
  const [form, setForm] = useState<FirmwareFormState>(emptyFirmwareForm);
  const [fragment, setFragment] = useState<string | null>(null);
  const [renderError, setRenderError] = useState<string | null>(null);
  const [rendering, setRendering] = useState(false);
  const [flashed, setFlashed] = useState<FirmwareFlashResult | null>(null);
  const [flashError, setFlashError] = useState<string | null>(null);
  const [flashing, setFlashing] = useState(false);

  function update(field: keyof FirmwareFormState, value: string) {
    setForm((current) => ({ ...current, [field]: value }));
    // A fragment on screen described the form as it was when it rendered.
    // Keeping it beside changed fields would invite saving one set of ids
    // having read another.
    setFragment(null);
    // And an upload confirmation describes a file that no longer matches the
    // form, which reads as "this device is configured" when it is not.
    setFlashed(null);
    setFlashError(null);
  }

  async function render(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const missing = missingFields(form);
    if (missing.length > 0) {
      setRenderError(
        `Fill in ${missing.map((field) => labelFor(field)).join(", ")} — the renderer has no default for a board choice or device name.`,
      );
      return;
    }

    setRendering(true);
    setRenderError(null);
    try {
      setFragment(
        await onRender(form.device.trim(), firmwareRequestFrom(form)),
      );
    } catch (caught) {
      setFragment(null);
      setRenderError(messageOf(caught, "Unable to render the fragment"));
    } finally {
      setRendering(false);
    }
  }

  /// Saves the fragment already on screen, from memory rather than by
  /// re-requesting it: what the operator read is what they get.
  function save() {
    if (!fragment) {
      return;
    }
    const blob = new Blob([fragment], { type: "application/yaml" });
    const href = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = href;
    link.download = fragmentFileName(form.device);
    link.click();
    URL.revokeObjectURL(href);
  }

  /// Hands the fragment to the configured ESPHome dashboard.
  ///
  /// A failure leaves the fragment and its save button exactly where they were:
  /// per ADR-0019 the download is the fallback, so an unreachable dashboard has
  /// to leave an operator somewhere other than a dead end.
  async function flash() {
    setFlashing(true);
    setFlashError(null);
    setFlashed(null);
    try {
      setFlashed(await onFlash(form.device.trim(), firmwareRequestFrom(form)));
    } catch (caught) {
      setFlashError(messageOf(caught, "Unable to upload the fragment"));
    } finally {
      setFlashing(false);
    }
  }

  return (
    <div className="providers-stack">
      <form className="settings-card" onSubmit={render}>
        <div className="section-heading">
          <div>
            <p className="eyebrow">ESPHome</p>
            <h2 id="firmware-title">Firmware fragment</h2>
          </div>
          <button className="primary-action" type="submit" disabled={rendering}>
            <FileCode2 size={17} aria-hidden="true" />
            {rendering ? "Rendering…" : "Render fragment"}
          </button>
        </div>

        <p className="panel-note">
          Conduit renders the <code>conduit_voice:</code> and{" "}
          <code>micro_wake_word:</code> half of a satellite&apos;s
          configuration. Save it beside your hand-written board file, which
          includes it as a package and stays yours. Credentials come out as{" "}
          <code>!secret</code> references, so the result is safe to commit.
        </p>

        <div className="settings-grid">
          <label className="field">
            <span>Device name</span>
            <input
              value={form.device}
              onChange={(event) => update("device", event.target.value)}
            />
          </label>
          <label className="field">
            <span>Pipeline</span>
            <input
              list="firmware-pipelines"
              value={form.pipeline}
              onChange={(event) => update("pipeline", event.target.value)}
            />
            <datalist id="firmware-pipelines">
              {pipelineNames.map((name) => (
                <option key={name} value={name} />
              ))}
            </datalist>
          </label>
          <label className="field">
            <span>Base device</span>
            <select
              value={form.base_device}
              onChange={(event) =>
                update(
                  "base_device",
                  event.target.value as FirmwareFormState["base_device"],
                )
              }
            >
              <option value="">Select a base device</option>
              {Object.entries(BASE_DEVICE_PROFILES).map(([value, profile]) => (
                <option key={value} value={value}>
                  {profile.label}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span>Server</span>
            <input
              placeholder="192.168.1.10:8080"
              value={form.server}
              onChange={(event) => update("server", event.target.value)}
            />
          </label>
          <label className="field">
            <span>Scheme</span>
            <select
              value={form.scheme}
              onChange={(event) =>
                update("scheme", event.target.value as "ws" | "wss")
              }
            >
              <option value="ws">ws</option>
              <option value="wss">wss</option>
            </select>
          </label>
          <label className="field">
            <span>Max utterance ms</span>
            <input
              type="number"
              placeholder="8000"
              value={form.max_utterance_ms}
              onChange={(event) =>
                update("max_utterance_ms", event.target.value)
              }
            />
          </label>
          <label className="field">
            <span>Debug UDP host</span>
            <input
              value={form.debug_udp_host}
              onChange={(event) => update("debug_udp_host", event.target.value)}
            />
          </label>
          <label className="field">
            <span>Debug UDP port</span>
            <input
              type="number"
              placeholder="6056"
              value={form.debug_udp_port}
              onChange={(event) => update("debug_udp_port", event.target.value)}
            />
          </label>
        </div>

        {form.base_device ? (
          <p className="panel-note">
            Using the checked-in {BASE_DEVICE_PROFILES[form.base_device].label}{" "}
            board defaults for microphone, speaker, mute switch, and gain.
          </p>
        ) : null}

        {renderError ? (
          <p className="form-error" role="alert">
            {renderError}
          </p>
        ) : null}
      </form>

      {fragment ? (
        <section className="settings-card" aria-labelledby="fragment-title">
          <div className="section-heading">
            <div>
              <p className="eyebrow">{fragmentFileName(form.device)}</p>
              <h2 id="fragment-title">Rendered fragment</h2>
            </div>
            <div className="section-actions">
              <button className="primary-action" type="button" onClick={save}>
                <Download size={17} aria-hidden="true" />
                Save fragment
              </button>
              <button
                className="secondary-action"
                type="button"
                onClick={flash}
                disabled={flashing}
              >
                <UploadCloud size={17} aria-hidden="true" />
                {flashing ? "Uploading…" : "Send to ESPHome"}
              </button>
            </div>
          </div>
          <pre className="fragment-preview" aria-label="Fragment YAML">
            {fragment}
          </pre>

          {flashed ? (
            <p className="panel-note" role="status">
              Uploaded as <code>{flashed.configuration}</code>. Build and
              install it from{" "}
              <a
                href={flashed.dashboard_url}
                target="_blank"
                // Conduit's own page is what the dashboard would otherwise be
                // able to reach through `window.opener`, and the dashboard is an
                // address an operator typed rather than one Conduit vouches for.
                rel="noreferrer noopener"
              >
                your ESPHome dashboard
              </a>
              . Conduit does not compile firmware — an ESPHome build bakes your{" "}
              <code>secrets.yaml</code> into the binary, so the toolchain that
              already holds those secrets is the one that uses them.
            </p>
          ) : null}

          {flashError ? (
            <p className="form-error" role="alert">
              {flashError} The fragment above is unchanged — save it and apply
              it to your ESPHome instance by hand.
            </p>
          ) : null}
        </section>
      ) : null}
    </div>
  );
}
