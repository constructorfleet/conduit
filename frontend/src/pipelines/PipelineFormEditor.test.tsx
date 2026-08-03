import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { buildMinimalVoiceLoopGraph } from "./graph";
import { formFromGraph, graphFromForm } from "./form";
import type { PipelineForm } from "./form";
import {
  BUILTIN_CONFIRM_PROVIDER,
  PipelineFormEditor,
  type ProviderOptions,
  type VoiceCatalog,
} from "./PipelineFormEditor";

function voiceForm(): PipelineForm {
  return formFromGraph(
    buildMinimalVoiceLoopGraph({
      name: "kitchen",
      sttProvider: "whisper",
      llmProvider: "openai",
      ttsProvider: "piper",
    }),
  );
}

const providers: ProviderOptions = {
  stt: [{ id: "whisper", label: "whisper" }],
  llm: [
    { id: "openai", label: "openai" },
    { id: "anthropic", label: "anthropic" },
  ],
  tts: [{ id: "piper", label: "piper" }],
  tool: [{ id: "search", label: "search" }],
  wake: [
    { id: "openwakeword", label: "openWakeWord" },
    { id: "okay-nabu", label: "On-device (okay nabu)" },
  ],
  speakerId: [{ id: "voices", label: "SpeechBrain" }],
};

function renderEditor(
  form: PipelineForm,
  readOnly = false,
  voices: VoiceCatalog = null,
) {
  const onChange = vi.fn();
  render(
    <PipelineFormEditor
      form={form}
      providers={providers}
      voices={voices}
      readOnly={readOnly}
      onChange={onChange}
    />,
  );
  return onChange;
}

describe("PipelineFormEditor", () => {
  it("shows each stage the pipeline runs", () => {
    renderEditor(voiceForm());

    expect(screen.getByLabelText("Speech to text provider")).toHaveValue(
      "whisper",
    );
    expect(screen.getByLabelText("Model provider")).toHaveValue("openai");
    expect(screen.getByLabelText("Text to speech provider")).toHaveValue(
      "piper",
    );
  });

  it("offers to add a stage the pipeline does not run", () => {
    renderEditor(voiceForm());

    expect(screen.getByLabelText("Add Wake word")).toBeInTheDocument();
    expect(screen.getByLabelText("Add Speaker ID")).toBeInTheDocument();
  });

  it("removes a stage without leaving the chain broken", async () => {
    const form = voiceForm();
    const onChange = renderEditor(form);

    await userEvent.click(screen.getByLabelText("Remove Speech to text"));

    const next = onChange.mock.calls[0][0] as PipelineForm;
    expect(next.stt).toBeNull();

    const graph = graphFromForm(next);
    const ids = new Set(graph.nodes.map((node) => node.id));
    expect(
      graph.edges
        .flatMap((edge) => [edge.from, edge.to])
        .filter((id) => !ids.has(id)),
    ).toEqual([]);
  });

  it("rebinds the model provider", async () => {
    const onChange = renderEditor(voiceForm());

    await userEvent.selectOptions(
      screen.getByLabelText("Model provider"),
      "anthropic",
    );

    const next = onChange.mock.calls[0][0] as PipelineForm;
    expect(next.core.model.provider).toBe("anthropic");
  });

  it("edits the system prompt onto the core", async () => {
    const onChange = renderEditor(voiceForm());

    await userEvent.type(screen.getByLabelText("System prompt"), "x");

    const next = onChange.mock.calls[0][0] as PipelineForm;
    expect(next.core.system).toBe("x");
  });

  it("binds a tool as a core binding rather than a node", async () => {
    const onChange = renderEditor(voiceForm());

    await userEvent.click(screen.getByLabelText("Add tool"));

    const next = onChange.mock.calls[0][0] as PipelineForm;
    expect(next.core.tools).toEqual([{ provider: "search", confirm: "never" }]);
    expect(
      graphFromForm(next).nodes.filter((node) => node.kind !== "core"),
    ).toHaveLength(4);
  });

  it("binds the builtin confirm tool when no tool provider is configured", async () => {
    const onChange = vi.fn();
    render(
      <PipelineFormEditor
        form={voiceForm()}
        providers={{ ...providers, tool: [] }}
        readOnly={false}
        onChange={onChange}
      />,
    );

    await userEvent.click(screen.getByLabelText("Add tool"));

    expect((onChange.mock.calls[0][0] as PipelineForm).core.tools).toEqual([
      { provider: BUILTIN_CONFIRM_PROVIDER, confirm: "never" },
    ]);
  });

  it("keeps a provider the catalog does not report listed", () => {
    const form = voiceForm();
    form.core.model.provider = "retired-provider";
    renderEditor(form);

    expect(screen.getByLabelText("Model provider")).toHaveValue(
      "retired-provider",
    );
  });

  it("disables every control when read only", () => {
    renderEditor(voiceForm(), true);

    expect(screen.getByLabelText("Model provider")).toBeDisabled();
    expect(screen.getByLabelText("System prompt")).toBeDisabled();
    expect(screen.getByLabelText("Add tool")).toBeDisabled();
    expect(screen.getByLabelText("Remove Speech to text")).toBeDisabled();
  });

  it("binds a configured detector when the wake stage is added", async () => {
    // The editor used to offer no detectors at all, so adding the stage wrote
    // a hardcoded provider name that no deployment had.
    const onChange = renderEditor(voiceForm());

    await userEvent.click(screen.getByLabelText("Add Wake word"));

    const next = onChange.mock.calls[0]?.[0] as PipelineForm;
    expect(next.wakeWord?.provider).toBe("openwakeword");
    expect(
      graphFromForm(next).nodes.some((node) => node.kind === "wake_word"),
    ).toBe(true);
  });

  it("binds a configured identifier when the speaker stage is added", async () => {
    const onChange = renderEditor(voiceForm());

    await userEvent.click(screen.getByLabelText("Add Speaker ID"));

    const next = onChange.mock.calls[0]?.[0] as PipelineForm;
    expect(next.speakerId?.provider).toBe("voices");
  });

  it("offers the voices the synthesizer reported", async () => {
    // The point of asking the provider: an operator picks a voice that exists
    // instead of typing one and finding out at the first reply.
    const onChange = renderEditor(voiceForm(), false, [
      { id: "en_US-amy", label: "Amy (en_US-amy)" },
      { id: "en_GB-alba", label: "Alba (en_GB-alba)" },
    ]);

    const voice = screen.getByLabelText("Voice");
    expect(voice.tagName).toBe("SELECT");
    await userEvent.selectOptions(voice, "en_GB-alba");

    const next = onChange.mock.calls[0]?.[0] as PipelineForm;
    expect(next.tts?.voice).toBe("en_GB-alba");
  });

  it("falls back to a typed voice when the provider offers no catalogue", async () => {
    // A Wyoming synthesizer enumerates nothing and accepts any name, and an
    // unreachable provider cannot be asked at all. Neither is a reason to stop
    // an operator naming a voice.
    const onChange = renderEditor(voiceForm(), false, []);

    const voice = screen.getByLabelText("Voice");
    expect(voice.tagName).toBe("INPUT");
    await userEvent.type(voice, "a");

    const next = onChange.mock.calls[0]?.[0] as PipelineForm;
    expect(next.tts?.voice).toBe("a");
  });

  it("keeps a voice the pipeline names but the provider no longer offers", () => {
    // The same rule an unlisted provider follows: show what the pipeline says
    // rather than silently rebinding it to something else.
    const form = voiceForm();
    form.tts = { ...form.tts!, voice: "en_US-retired" };
    renderEditor(form, false, [{ id: "en_US-amy", label: "Amy (en_US-amy)" }]);

    expect(screen.getByLabelText("Voice")).toHaveValue("en_US-retired");
  });
});
