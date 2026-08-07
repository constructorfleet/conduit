import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import {
  FirmwarePanel,
  emptyFirmwareForm,
  firmwareRequestFrom,
  fragmentFileName,
} from "./FirmwarePanel";

/// A form with every field the endpoint has no default for.
function filledForm() {
  return {
    ...emptyFirmwareForm,
    device: "kitchen",
    pipeline: "default",
    microphone: "sat1_mics",
    speaker: "announcement_resampling_speaker",
    mute_switch: "master_mute_switch",
    gain_factor: "6",
    server: "192.168.1.10:8080",
  };
}

/// Fills the required fields through the UI, as an operator would.
async function fillRequired(user: ReturnType<typeof userEvent.setup>) {
  const form = filledForm();
  for (const [label, value] of [
    ["Device name", form.device],
    ["Pipeline", form.pipeline],
    ["Microphone id", form.microphone],
    ["Speaker id", form.speaker],
    ["Mute switch id", form.mute_switch],
    ["Gain factor", form.gain_factor],
    ["Server", form.server],
  ] as const) {
    await user.type(screen.getByLabelText(label), value);
  }
}

/// A dashboard that accepted the upload.
function accepted() {
  return {
    configuration: "conduit-kitchen.conduit.yaml",
    dashboard_url: "http://homelab:6052/",
  };
}

/// Panels take a flasher too; most tests do not exercise it.
function panel(overrides: {
  onRender: () => Promise<string>;
  onFlash?: () => Promise<ReturnType<typeof accepted>>;
}) {
  return (
    <FirmwarePanel
      pipelineNames={["default"]}
      onRender={overrides.onRender}
      onFlash={overrides.onFlash ?? (async () => accepted())}
    />
  );
}

const FRAGMENT = [
  "# Rendered by Conduit. Edits are lost on the next render.",
  "conduit_voice:",
  "  token: !secret conduit_token",
  "",
].join("\n");

describe("the request a form describes", () => {
  it("omits an optional field left blank rather than sending it empty", () => {
    // The endpoint distinguishes "not asked for" from "asked for, blank", and
    // the console has to preserve that: a blank `max_utterance_ms` means the
    // server's default, not a zero-length utterance.
    const request = firmwareRequestFrom(filledForm());

    expect(request).not.toHaveProperty("max_utterance_ms");
    expect(request).not.toHaveProperty("debug_udp_port");
    expect(request).not.toHaveProperty("debug_udp_host");
    expect(request.gain_factor).toBe(6);
  });

  it("sends an explicitly emptied debug host, because that is how it is off", () => {
    // Both board files disable UDP debug with an empty host. That is a value an
    // operator chose, so it goes on the wire; only an untouched field does not.
    const request = firmwareRequestFrom({
      ...filledForm(),
      debug_udp_host: " ",
    });

    expect(request.debug_udp_host).toBe("");
  });

  it("keeps a scheme the operator changed and stays quiet about the default", () => {
    expect(firmwareRequestFrom(filledForm())).not.toHaveProperty("scheme");
    expect(firmwareRequestFrom({ ...filledForm(), scheme: "wss" }).scheme).toBe(
      "wss",
    );
  });

  it("names the file the board file already includes", () => {
    // A board file says `!include conduit-sat1.conduit.yaml`, so a re-render
    // has to land on that name rather than beside it.
    expect(fragmentFileName("sat1")).toBe("conduit-sat1.conduit.yaml");
    expect(fragmentFileName("Kitchen Satellite")).toBe(
      "conduit-kitchen-satellite.conduit.yaml",
    );
  });
});

describe("Firmware panel", () => {
  it("shows the fragment it rendered, before offering to save it", async () => {
    // Applying a fragment means reflashing a device, so the operator reads it
    // first. There is nothing to save until there is something to read.
    const user = userEvent.setup();
    const onRender = vi.fn(async () => FRAGMENT);
    render(panel({ onRender }));

    expect(
      screen.queryByRole("button", { name: "Save fragment" }),
    ).not.toBeInTheDocument();

    await fillRequired(user);
    await user.click(screen.getByRole("button", { name: "Render fragment" }));

    expect(screen.getByLabelText("Fragment YAML")).toHaveTextContent(
      "token: !secret conduit_token",
    );
    expect(
      screen.getByRole("button", { name: "Save fragment" }),
    ).toBeInTheDocument();
    expect(onRender).toHaveBeenCalledWith("kitchen", {
      pipeline: "default",
      microphone: "sat1_mics",
      speaker: "announcement_resampling_speaker",
      mute_switch: "master_mute_switch",
      gain_factor: 6,
      server: "192.168.1.10:8080",
    });
  });

  it("refuses a missing board id itself, naming what is blank", async () => {
    // The endpoint has no default for a board id either. Asking the server
    // would get the same refusal a round trip later and less specifically.
    const user = userEvent.setup();
    const onRender = vi.fn(async () => FRAGMENT);
    render(panel({ onRender }));

    await user.type(screen.getByLabelText("Device name"), "kitchen");
    await user.click(screen.getByRole("button", { name: "Render fragment" }));

    expect(screen.getByRole("alert")).toHaveTextContent("microphone");
    expect(onRender).not.toHaveBeenCalled();
  });

  it("surfaces what the server said when it refused", async () => {
    // The refusals worth reading are the server's own — an unknown phrase, a
    // pipeline that does not wake on the device — so they are shown verbatim.
    const user = userEvent.setup();
    const onRender = vi.fn(async () => {
      throw new Error("no microWakeWord model is known for the phrase `stop`");
    });
    render(panel({ onRender }));

    await fillRequired(user);
    await user.click(screen.getByRole("button", { name: "Render fragment" }));

    expect(screen.getByRole("alert")).toHaveTextContent(
      "no microWakeWord model is known",
    );
    expect(
      screen.queryByRole("button", { name: "Save fragment" }),
    ).not.toBeInTheDocument();
  });

  it("drops a rendered fragment when a field changes under it", async () => {
    // Otherwise an operator edits the microphone id, saves what is on screen,
    // and flashes a fragment naming the id they just replaced.
    const user = userEvent.setup();
    const onRender = vi.fn(async () => FRAGMENT);
    render(panel({ onRender }));

    await fillRequired(user);
    await user.click(screen.getByRole("button", { name: "Render fragment" }));
    expect(screen.getByLabelText("Fragment YAML")).toBeInTheDocument();

    await user.type(screen.getByLabelText("Microphone id"), "-2");

    expect(screen.queryByLabelText("Fragment YAML")).not.toBeInTheDocument();
  });
});

describe("handing a fragment to ESPHome", () => {
  it("uploads what is on screen and links out for the build", async () => {
    // ADR-0019: Conduit uploads and then gets out of the way. There is no
    // embedded installer, because Conduit never holds a compiled image.
    const user = userEvent.setup();
    const onFlash = vi.fn(async () => accepted());
    render(panel({ onRender: async () => FRAGMENT, onFlash }));

    await fillRequired(user);
    await user.click(screen.getByRole("button", { name: "Render fragment" }));
    await user.click(screen.getByRole("button", { name: "Send to ESPHome" }));

    expect(onFlash).toHaveBeenCalledWith("kitchen", {
      pipeline: "default",
      microphone: "sat1_mics",
      speaker: "announcement_resampling_speaker",
      mute_switch: "master_mute_switch",
      gain_factor: 6,
      server: "192.168.1.10:8080",
    });
    expect(screen.getByRole("status")).toHaveTextContent(
      "conduit-kitchen.conduit.yaml",
    );
    expect(
      screen.getByRole("link", { name: "your ESPHome dashboard" }),
    ).toHaveAttribute("href", "http://homelab:6052/");
  });

  it("leaves the fragment and its save button when the upload fails", async () => {
    // The whole point of ADR-0019's degradation rule: a broken hand-off ends at
    // "here is your fragment, apply it yourself", not at a dead button.
    const user = userEvent.setup();
    const onFlash = vi.fn(async () => {
      throw new Error(
        "cannot reach the ESPHome dashboard at http://homelab:6052/",
      );
    });
    render(panel({ onRender: async () => FRAGMENT, onFlash }));

    await fillRequired(user);
    await user.click(screen.getByRole("button", { name: "Render fragment" }));
    await user.click(screen.getByRole("button", { name: "Send to ESPHome" }));

    expect(screen.getByRole("alert")).toHaveTextContent(
      "cannot reach the ESPHome dashboard",
    );
    expect(screen.getByRole("alert")).toHaveTextContent("by hand");
    expect(screen.getByLabelText("Fragment YAML")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Save fragment" }),
    ).toBeInTheDocument();
  });

  it("clears an upload confirmation when a field changes under it", async () => {
    // A confirmation naming a file that no longer matches the form reads as
    // "this device is configured" when it is not.
    const user = userEvent.setup();
    render(panel({ onRender: async () => FRAGMENT }));

    await fillRequired(user);
    await user.click(screen.getByRole("button", { name: "Render fragment" }));
    await user.click(screen.getByRole("button", { name: "Send to ESPHome" }));
    expect(screen.getByRole("status")).toBeInTheDocument();

    await user.type(screen.getByLabelText("Server"), "1");

    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });
});
