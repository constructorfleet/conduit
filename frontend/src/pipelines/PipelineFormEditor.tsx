/// The pipeline editor as a form.
///
/// Each stage is a field the pipeline either has or does not, so every state
/// this component can reach describes a connected chain. There is no canvas
/// and no edge authoring: `graphFromForm` derives the graph on save.

import { Plus, Trash2 } from "lucide-react";

import type { ConfirmPolicy, MemoryMode } from "../contracts/client";
import { DEFAULT_MEMORY_LIMIT, nodeProvider, outputModality } from "./graph";
import { graphFromForm } from "./form";
import type { PipelineForm, StageField } from "./form";

/// The single store the console binds memory to. Memory is not a provider
/// capability the catalog reports, so there is nothing to choose between.
export const BUILTIN_MEMORY_PROVIDER = "builtin.memory";

/// The tool bound when no tool provider is configured. A pipeline can ask the
/// operator to confirm an action without an MCP server being set up, so the
/// absence of a catalog is not a reason to refuse a binding.
export const BUILTIN_CONFIRM_PROVIDER = "builtin.confirm";

export interface ProviderOption {
  id: string;
  label: string;
}

/// The providers an operator can bind, by what they do.
export interface ProviderOptions {
  stt: readonly ProviderOption[];
  llm: readonly ProviderOption[];
  tts: readonly ProviderOption[];
  tool: readonly ProviderOption[];
}

const MEMORY_MODES: MemoryMode[] = ["read", "write", "read_write"];
const CONFIRM_POLICIES: ConfirmPolicy[] = ["never", "always"];

export function PipelineFormEditor({
  form,
  providers,
  readOnly,
  onChange,
}: {
  form: PipelineForm;
  providers: ProviderOptions;
  readOnly: boolean;
  onChange: (form: PipelineForm) => void;
}) {
  function update(patch: Partial<PipelineForm>) {
    onChange({ ...form, ...patch });
  }

  function updateCore(patch: Partial<PipelineForm["core"]>) {
    onChange({ ...form, core: { ...form.core, ...patch } });
  }

  /// A row for a stage the pipeline may or may not run.
  function optionalStage({
    label,
    stage,
    fallbackId,
    options,
    fallbackProvider,
    onStageChange,
  }: {
    label: string;
    stage: StageField | null;
    fallbackId: string;
    options: readonly ProviderOption[];
    fallbackProvider: string;
    onStageChange: (next: StageField | null) => void;
  }) {
    if (!stage) {
      return (
        <div className="pipeline-form-row absent" key={label}>
          <span className="pipeline-form-label">{label}</span>
          <button
            className="secondary-action compact-action"
            type="button"
            aria-label={`Add ${label}`}
            disabled={readOnly}
            onClick={() =>
              onStageChange({
                id: fallbackId,
                provider: options[0]?.id ?? fallbackProvider,
              })
            }
          >
            <Plus size={16} aria-hidden="true" />
            Add
          </button>
        </div>
      );
    }

    return (
      <div className="pipeline-form-row" key={label}>
        <label className="pipeline-form-label" htmlFor={`stage-${stage.id}`}>
          {label}
        </label>
        <select
          id={`stage-${stage.id}`}
          aria-label={`${label} provider`}
          value={stage.provider}
          disabled={readOnly}
          onChange={(event) =>
            onStageChange({ ...stage, provider: event.target.value })
          }
        >
          {providerChoices(options, stage.provider)}
        </select>
        <button
          className="icon-action danger subtle-danger"
          type="button"
          aria-label={`Remove ${label}`}
          disabled={readOnly}
          onClick={() => onStageChange(null)}
        >
          <Trash2 size={17} aria-hidden="true" />
        </button>
      </div>
    );
  }

  return (
    <div className="pipeline-form">
      <ol className="pipeline-chain" aria-label="Pipeline stages">
        {graphFromForm(form).nodes.map((node) => (
          <li
            key={node.id}
            className="pipeline-chain-stage"
            aria-label={`${node.id} stage`}
            /// What this stage hands the next one, so a miswiring reads off
            /// the chain rather than off a save the backend refuses.
            data-modality={outputModality(node) ?? ""}
          >
            <strong>{node.id}</strong>
            <span className="node-provider-label">
              {node.kind} / {nodeProvider(node)}
            </span>
          </li>
        ))}
      </ol>

      <section className="pipeline-form-section" aria-label="Input">
        <header>
          <p className="eyebrow">Stage 1</p>
          <h3>Input</h3>
        </header>

        <div className="pipeline-form-row">
          <label className="pipeline-form-label" htmlFor="source-provider">
            Source
          </label>
          <select
            id="source-provider"
            aria-label="Source transport"
            value={form.source.provider}
            disabled={readOnly}
            onChange={(event) =>
              update({
                source: { ...form.source, provider: event.target.value },
              })
            }
          >
            <option value="websocket">websocket</option>
          </select>
          <select
            aria-label="Source modality"
            value={form.source.modality}
            disabled={readOnly}
            onChange={(event) =>
              update({
                source: {
                  ...form.source,
                  modality: event.target
                    .value as PipelineForm["source"]["modality"],
                },
              })
            }
          >
            <option value="audio">audio</option>
            <option value="text">text</option>
          </select>
        </div>

        {optionalStage({
          label: "Wake word",
          stage: form.wakeWord,
          fallbackId: "wake_word",
          options: [],
          fallbackProvider: "porcupine",
          onStageChange: (wakeWord) => update({ wakeWord }),
        })}
        {optionalStage({
          label: "Speech to text",
          stage: form.stt,
          fallbackId: "stt",
          options: providers.stt,
          fallbackProvider: "whisper",
          onStageChange: (stt) => update({ stt }),
        })}
        {optionalStage({
          label: "Speaker ID",
          stage: form.speakerId,
          fallbackId: "speaker_id",
          options: [],
          fallbackProvider: "resemblyzer",
          onStageChange: (speakerId) => update({ speakerId }),
        })}
      </section>

      <section className="pipeline-form-section" aria-label="Reasoning">
        <header>
          <p className="eyebrow">Stage 2</p>
          <h3>Reasoning</h3>
        </header>

        <div className="pipeline-form-row">
          <label className="pipeline-form-label" htmlFor="core-model-provider">
            Model provider
          </label>
          <select
            id="core-model-provider"
            aria-label="Model provider"
            value={form.core.model.provider}
            disabled={readOnly}
            onChange={(event) =>
              updateCore({
                model: { ...form.core.model, provider: event.target.value },
              })
            }
          >
            {providerChoices(providers.llm, form.core.model.provider)}
          </select>
        </div>

        <div className="pipeline-form-row">
          <label className="pipeline-form-label" htmlFor="core-model">
            Model
          </label>
          <input
            id="core-model"
            aria-label="Model"
            value={form.core.model.model ?? ""}
            disabled={readOnly}
            placeholder="the provider's default"
            onChange={(event) =>
              updateCore({
                model: {
                  ...form.core.model,
                  model: event.target.value || undefined,
                },
              })
            }
          />
        </div>

        <div className="pipeline-form-row wide">
          <label className="pipeline-form-label" htmlFor="core-system">
            System prompt
          </label>
          <textarea
            id="core-system"
            aria-label="System prompt"
            rows={4}
            value={form.core.system ?? ""}
            disabled={readOnly}
            placeholder="the provider definition's prompt"
            onChange={(event) =>
              updateCore({ system: event.target.value || undefined })
            }
          />
        </div>

        <div className="pipeline-form-row">
          <label className="pipeline-form-label" htmlFor="core-max-rounds">
            Max rounds
          </label>
          <input
            id="core-max-rounds"
            aria-label="Max rounds"
            type="number"
            min={1}
            value={form.core.maxRounds}
            disabled={readOnly}
            onChange={(event) =>
              updateCore({
                maxRounds: Number(event.target.value) || form.core.maxRounds,
              })
            }
          />
        </div>

        <div className="pipeline-form-list" aria-label="Tools">
          <div className="pipeline-form-list-head">
            <span className="pipeline-form-label">Tools</span>
            <button
              className="secondary-action compact-action"
              type="button"
              aria-label="Add tool"
              disabled={readOnly}
              onClick={() =>
                updateCore({
                  tools: [
                    ...form.core.tools,
                    {
                      provider:
                        providers.tool[0]?.id ?? BUILTIN_CONFIRM_PROVIDER,
                      confirm: "never",
                    },
                  ],
                })
              }
            >
              <Plus size={16} aria-hidden="true" />
              Add
            </button>
          </div>
          {form.core.tools.length === 0 ? (
            <p className="pipeline-form-empty">No tools bound</p>
          ) : (
            form.core.tools.map((tool, index) => (
              <div className="pipeline-form-row" key={`tool-${index}`}>
                <select
                  aria-label={`Tool ${index + 1} provider`}
                  value={tool.provider}
                  disabled={readOnly}
                  onChange={(event) =>
                    updateCore({
                      tools: replaceAt(form.core.tools, index, {
                        ...tool,
                        provider: event.target.value,
                      }),
                    })
                  }
                >
                  {providerChoices(providers.tool, tool.provider)}
                </select>
                <select
                  aria-label={`Tool ${index + 1} confirmation`}
                  value={tool.confirm}
                  disabled={readOnly}
                  onChange={(event) =>
                    updateCore({
                      tools: replaceAt(form.core.tools, index, {
                        ...tool,
                        confirm: event.target.value as ConfirmPolicy,
                      }),
                    })
                  }
                >
                  {CONFIRM_POLICIES.map((policy) => (
                    <option key={policy} value={policy}>
                      confirm {policy}
                    </option>
                  ))}
                </select>
                <button
                  className="icon-action danger subtle-danger"
                  type="button"
                  aria-label={`Remove tool ${index + 1}`}
                  disabled={readOnly}
                  onClick={() =>
                    updateCore({ tools: removeAt(form.core.tools, index) })
                  }
                >
                  <Trash2 size={17} aria-hidden="true" />
                </button>
              </div>
            ))
          )}
        </div>

        <div className="pipeline-form-list" aria-label="Memory">
          <div className="pipeline-form-list-head">
            <span className="pipeline-form-label">Memory</span>
            <button
              className="secondary-action compact-action"
              type="button"
              aria-label="Add memory"
              disabled={readOnly}
              onClick={() =>
                updateCore({
                  memory: [
                    ...form.core.memory,
                    {
                      provider: BUILTIN_MEMORY_PROVIDER,
                      mode: "read_write",
                      limit: DEFAULT_MEMORY_LIMIT,
                    },
                  ],
                })
              }
            >
              <Plus size={16} aria-hidden="true" />
              Add
            </button>
          </div>
          {form.core.memory.length === 0 ? (
            <p className="pipeline-form-empty">No memory bound</p>
          ) : (
            form.core.memory.map((store, index) => (
              <div className="pipeline-form-row" key={`memory-${index}`}>
                <span className="node-provider-label">{store.provider}</span>
                <select
                  aria-label={`Memory ${index + 1} mode`}
                  value={store.mode}
                  disabled={readOnly}
                  onChange={(event) =>
                    updateCore({
                      memory: replaceAt(form.core.memory, index, {
                        ...store,
                        mode: event.target.value as MemoryMode,
                      }),
                    })
                  }
                >
                  {MEMORY_MODES.map((mode) => (
                    <option key={mode} value={mode}>
                      {mode}
                    </option>
                  ))}
                </select>
                <input
                  aria-label={`Memory ${index + 1} limit`}
                  type="number"
                  min={1}
                  value={store.limit}
                  disabled={readOnly}
                  onChange={(event) =>
                    updateCore({
                      memory: replaceAt(form.core.memory, index, {
                        ...store,
                        limit: Number(event.target.value) || store.limit,
                      }),
                    })
                  }
                />
                <button
                  className="icon-action danger subtle-danger"
                  type="button"
                  aria-label={`Remove memory ${index + 1}`}
                  disabled={readOnly}
                  onClick={() =>
                    updateCore({ memory: removeAt(form.core.memory, index) })
                  }
                >
                  <Trash2 size={17} aria-hidden="true" />
                </button>
              </div>
            ))
          )}
        </div>
      </section>

      <section className="pipeline-form-section" aria-label="Output">
        <header>
          <p className="eyebrow">Stage 3</p>
          <h3>Output</h3>
        </header>

        {form.tts ? (
          <div className="pipeline-form-row">
            <label className="pipeline-form-label" htmlFor="tts-provider">
              Text to speech
            </label>
            <select
              id="tts-provider"
              aria-label="Text to speech provider"
              value={form.tts.provider}
              disabled={readOnly}
              onChange={(event) =>
                update({
                  tts: { ...form.tts!, provider: event.target.value },
                })
              }
            >
              {providerChoices(providers.tts, form.tts.provider)}
            </select>
            <input
              aria-label="Voice"
              value={form.tts.voice ?? ""}
              disabled={readOnly}
              placeholder="default voice"
              onChange={(event) =>
                update({
                  tts: {
                    ...form.tts!,
                    voice: event.target.value || undefined,
                  },
                })
              }
            />
            <button
              className="icon-action danger subtle-danger"
              type="button"
              aria-label="Remove Text to speech"
              disabled={readOnly}
              onClick={() => update({ tts: null })}
            >
              <Trash2 size={17} aria-hidden="true" />
            </button>
          </div>
        ) : (
          <div className="pipeline-form-row absent">
            <span className="pipeline-form-label">Text to speech</span>
            <button
              className="secondary-action compact-action"
              type="button"
              aria-label="Add Text to speech"
              disabled={readOnly}
              onClick={() =>
                update({
                  tts: {
                    id: "tts",
                    provider: providers.tts[0]?.id ?? "piper",
                  },
                })
              }
            >
              <Plus size={16} aria-hidden="true" />
              Add
            </button>
          </div>
        )}

        <div className="pipeline-form-row">
          <label className="pipeline-form-label" htmlFor="sink-provider">
            Sink
          </label>
          <select
            id="sink-provider"
            aria-label="Sink transport"
            value={form.sink.provider}
            disabled={readOnly}
            onChange={(event) =>
              update({ sink: { ...form.sink, provider: event.target.value } })
            }
          >
            <option value="websocket">websocket</option>
          </select>
          <select
            aria-label="Sink modality"
            value={form.sink.modality}
            disabled={readOnly}
            onChange={(event) =>
              update({
                sink: {
                  ...form.sink,
                  modality: event.target
                    .value as PipelineForm["sink"]["modality"],
                },
              })
            }
          >
            <option value="audio">audio</option>
            <option value="text">text</option>
          </select>
        </div>
      </section>
    </div>
  );
}

/// Options for a select, keeping the bound provider listed even when the
/// catalog does not report it: a pipeline naming a provider nobody configured
/// should show what it names rather than silently rebind to something else.
function providerChoices(options: readonly ProviderOption[], selected: string) {
  const listed = options.some((option) => option.id === selected);
  return (
    <>
      {!listed && selected ? (
        <option value={selected}>{selected}</option>
      ) : null}
      {options.map((option) => (
        <option key={option.id} value={option.id}>
          {option.label}
        </option>
      ))}
    </>
  );
}

function replaceAt<T>(items: readonly T[], index: number, next: T): T[] {
  return items.map((item, position) => (position === index ? next : item));
}

function removeAt<T>(items: readonly T[], index: number): T[] {
  return items.filter((_, position) => position !== index);
}
