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
};

function renderEditor(form: PipelineForm, readOnly = false) {
  const onChange = vi.fn();
  render(
    <PipelineFormEditor
      form={form}
      providers={providers}
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
});
