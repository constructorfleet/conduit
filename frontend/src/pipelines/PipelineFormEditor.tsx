/// The pipeline editor as a form.
///
/// Each stage is a field the pipeline either has or does not, so every state
/// this component can reach describes a connected chain. There is no canvas
/// and no edge authoring: `graphFromForm` derives the graph on save.

import { Plus, Trash2 } from "lucide-react";

import { useEffect } from "react";

import type { ConfirmPolicy, MemoryMode } from "../contracts/client";
import { DEFAULT_MEMORY_LIMIT, nodeProvider, outputModality } from "./graph";
import { derivedSinkModality, graphFromForm } from "./form";
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
  transform: readonly ProviderOption[];
  wake: readonly ProviderOption[];
  speakerId: readonly ProviderOption[];
  vad: readonly ProviderOption[];
}

/// The voices the selected synthesizer offers.
///
/// `null` means nobody has asked yet, or the provider could not be reached —
/// either way the editor falls back to a typed voice, which is what an
/// operator had before providers could be asked. An empty list is the same
/// fallback for a different reason: the provider accepts any name.
export type VoiceCatalog = readonly ProviderOption[] | null;

const MEMORY_MODES: MemoryMode[] = ["read", "write", "read_write"];
const CONFIRM_POLICIES: ConfirmPolicy[] = ["never", "always"];

export function PipelineFormEditor({
  form,
  providers,
  voices,
  readOnly,
  onChange,
}: {
  form: PipelineForm;
  providers: ProviderOptions;
  /// Voices the selected synthesizer offers, when it was asked and answered.
  voices?: VoiceCatalog;
  readOnly: boolean;
  onChange: (form: PipelineForm) => void;
}) {
  function update(patch: Partial<PipelineForm>) {
    onChange({ ...form, ...patch });
  }

  function updateCore(patch: Partial<PipelineForm["core"]>) {
    onChange({ ...form, core: { ...form.core, ...patch } });
  }

  /// What the sink's modality must be so its incoming edge validates. The
  /// operator does not pick it: an audio stream cannot be rendered as text and
  /// a text stream cannot be rendered as speech, so what feeds the sink is
  /// what the sink says it accepts.
  const sinkModality = derivedSinkModality(form);
  useEffect(() => {
    if (form.sink.modality !== sinkModality) {
      onChange({ ...form, sink: { ...form.sink, modality: sinkModality } });
    }
  }, [form, sinkModality, onChange]);

  /// A row for a stage the pipeline may or may not run.
  ///
  /// A stage nothing can serve is not offered. Adding one would name a
  /// provider that does not exist, and a pipeline that names one is refused on
  /// save — so the row would be an invitation to write something unsaveable.
  /// Configure a provider of that capability and the stage appears.
  ///
  /// A stage the pipeline *already* runs is always shown, even with nothing to
  /// choose between: its provider may have been deleted since, and a pipeline
  /// should show what it says rather than quietly lose a stage.
  function optionalStage({
    label,
    stage,
    fallbackId,
    options,
    onStageChange,
  }: {
    label: string;
    stage: StageField | null;
    fallbackId: string;
    options: readonly ProviderOption[];
    onStageChange: (next: StageField | null) => void;
  }) {
    if (!stage) {
      const first = options[0];
      if (!first) {
        return null;
      }
      return (
        <div className="pipeline-form-row absent" key={label}>
          <span className="pipeline-form-label">{label}</span>
          <button
            className="secondary-action compact-action"
            type="button"
            aria-label={`Add ${label}`}
            disabled={readOnly}
            onClick={() =>
              onStageChange({ id: fallbackId, provider: first.id })
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
            data-kind={node.kind}
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
            className="compact"
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
          options: providers.wake,
          onStageChange: (wakeWord) => update({ wakeWord }),
        })}
        {optionalStage({
          label: "Voice activity",
          stage: form.vad,
          fallbackId: "vad",
          options: providers.vad,
          onStageChange: (vad) => update({ vad }),
        })}
        {optionalStage({
          label: "Speech to text",
          stage: form.stt,
          fallbackId: "stt",
          options: providers.stt,
          onStageChange: (stt) => update({ stt }),
        })}
        {optionalStage({
          label: "Speaker ID",
          stage: form.speakerId,
          fallbackId: "speaker_id",
          options: providers.speakerId,
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
            className="compact"
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
                  className="compact"
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
                  className="compact"
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
                  className="compact"
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

        {/* Between the model and what renders it: a model writes for a
            reader, and this is where a pipeline says what a listener should
            hear instead. */}
        {optionalStage({
          label: "Rewrite before output",
          stage: form.transform,
          fallbackId: "transform",
          options: providers.transform,
          onStageChange: (transform) => update({ transform }),
        })}

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
            {voices && voices.length > 0 ? (
              <select
                aria-label="Voice"
                value={form.tts.voice ?? ""}
                disabled={readOnly}
                onChange={(event) =>
                  update({
                    tts: {
                      ...form.tts!,
                      voice: event.target.value || undefined,
                    },
                  })
                }
              >
                <option value="">default voice</option>
                {/* A voice the pipeline names but the provider no longer
                    offers stays listed, for the same reason an unlisted
                    provider does: a pipeline should show what it says rather
                    than silently rebind to something else. */}
                {form.tts.voice &&
                !voices.some((voice) => voice.id === form.tts!.voice) ? (
                  <option value={form.tts.voice}>{form.tts.voice}</option>
                ) : null}
                {voices.map((voice) => (
                  <option key={voice.id} value={voice.id}>
                    {voice.label}
                  </option>
                ))}
              </select>
            ) : (
              /* No catalogue to choose from: the provider was not reachable,
                 or it accepts any name its backend knows. Either way a typed
                 voice is better than an empty menu. */
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
            )}
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
        ) : providers.tts[0] ? (
          <div className="pipeline-form-row absent">
            <span className="pipeline-form-label">Text to speech</span>
            <button
              className="secondary-action compact-action"
              type="button"
              aria-label="Add Text to speech"
              disabled={readOnly}
              onClick={() =>
                update({
                  tts: { id: "tts", provider: providers.tts[0].id },
                })
              }
            >
              <Plus size={16} aria-hidden="true" />
              Add
            </button>
          </div>
        ) : null}

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
          {/* Derived from what feeds the sink rather than picked: a mismatch
              refuses the pipeline on save, and there is exactly one right
              answer per upstream shape. Rendered as a disabled select so the
              value stays visible beside the source's modality picker. */}
          <select
            className="compact"
            aria-label="Sink modality"
            value={sinkModality}
            disabled
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
