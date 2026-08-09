import {
  useState,
} from "react";
import {
  Plus,
  Save,
  Trash2,
  Play,
  X,
  Settings,
  CheckCircle,
  AlertCircle,
} from "lucide-react";

interface FormaRule {
  id: string;
  name: string;
  description: string;
  rule_type: "replace" | "remove" | "transform" | "insert" | "script";
  condition: "always" | "matches_pattern" | "contains" | "starts_with" | "ends_with" | "custom";
  action: {
    pattern?: string;
    replacement?: string;
    flags?: string;
    substring?: string;
    prefix?: string;
    suffix?: string;
    case_conversion?: "upper" | "lower" | "title" | "sentence";
    insert_text?: string;
    script?: string;
  };
  enabled: boolean;
  priority: number;
}

interface RuleSet {
  id: string;
  name: string;
  description: string;
  rules: FormaRule[];
}

export function FormaPanel() {
  const [ruleSets, setRuleSets] = useState<RuleSet[]>([
    {
      id: "default",
      name: "Default Rules",
      description: "Standard text transformations",
      rules: [
        {
          id: "emoji-removal",
          name: "Remove Emojis",
          description: "Strips emoji characters from text",
          rule_type: "remove",
          condition: "always",
          action: {
            pattern: "[\\p{Emoji_Presentation}\\p{Extended_Pictographic}]",
            flags: "gu",
          },
          enabled: true,
          priority: 10,
        },
        {
          id: "markdown-cleanup",
          name: "Markdown to Speech",
          description: "Converts markdown formatting to speech-friendly text",
          rule_type: "replace",
          condition: "always",
          action: {
            pattern: "\\*\\*([^*]+)\\*\\*",
            replacement: "$1",
            flags: "g",
          },
          enabled: true,
          priority: 5,
        },
      ],
    },
  ]);
  const [selectedRuleSet, setSelectedRuleSet] = useState<RuleSet | null>(ruleSets[0]);
  const [editingRule, setEditingRule] = useState<FormaRule | null>(null);
  const [testInput, setTestInput] = useState("Hello, World! 🌟 This is **bold** text.");
  const [testOutput, setTestOutput] = useState("");
  const [showTestResults, setShowTestResults] = useState(false);

  const handleAddRule = () => {
    if (!selectedRuleSet) return;

    const newRule: FormaRule = {
      id: `rule-${Date.now()}`,
      name: "New Rule",
      description: "",
      rule_type: "replace",
      condition: "always",
      action: {
        pattern: "",
        replacement: "",
        flags: "",
      },
      enabled: true,
      priority: 0,
    };

    setEditingRule(newRule);
  };

  const handleSaveRule = (rule: FormaRule) => {
    if (!selectedRuleSet) return;

    setRuleSets((current) =>
      current.map((set) =>
        set.id === selectedRuleSet.id
          ? {
              ...set,
              rules: set.rules.some((r) => r.id === rule.id)
                ? set.rules.map((r) => (r.id === rule.id ? rule : r))
                : [...set.rules, rule].sort((a, b) => b.priority - a.priority),
            }
          : set
      )
    );

    setSelectedRuleSet((current) =>
      current && current.id === selectedRuleSet.id
        ? {
            ...current,
            rules: current.rules.some((r) => r.id === rule.id)
              ? current.rules.map((r) => (r.id === rule.id ? rule : r))
              : [...current.rules, rule].sort((a, b) => b.priority - a.priority),
          }
        : current
    );

    setEditingRule(null);
  };

  const handleDeleteRule = (ruleId: string) => {
    if (!selectedRuleSet) return;

    setRuleSets((current) =>
      current.map((set) =>
        set.id === selectedRuleSet.id
          ? {
              ...set,
              rules: set.rules.filter((rule) => rule.id !== ruleId),
            }
          : set
      )
    );

    setSelectedRuleSet((current) =>
      current && current.id === selectedRuleSet.id
        ? {
            ...current,
            rules: current.rules.filter((rule) => rule.id !== ruleId),
          }
        : current
    );
  };

  const handleToggleRule = (ruleId: string) => {
    if (!selectedRuleSet) return;

    setRuleSets((current) =>
      current.map((set) =>
        set.id === selectedRuleSet.id
          ? {
              ...set,
              rules: set.rules.map((rule) =>
                rule.id === ruleId ? { ...rule, enabled: !rule.enabled } : rule
              ),
            }
          : set
      )
    );

    setSelectedRuleSet((current) =>
      current && current.id === selectedRuleSet.id
        ? {
            ...current,
            rules: current.rules.map((rule) =>
              rule.id === ruleId ? { ...rule, enabled: !rule.enabled } : rule
            ),
          }
        : current
    );
  };

  const handleTestRules = () => {
    if (!selectedRuleSet) return;

    let result = testInput;
    const sortedRules = [...selectedRuleSet.rules]
      .filter((rule) => rule.enabled)
      .sort((a, b) => b.priority - a.priority);

    for (const rule of sortedRules) {
      try {
        result = applyRule(result, rule);
      } catch (error) {
        console.error(`Rule ${rule.name} failed:`, error);
      }
    }

    setTestOutput(result);
    setShowTestResults(true);
  };

  const applyRule = (text: string, rule: FormaRule): string => {
    if (!rule.enabled) return text;

    try {
      let pattern = rule.action.pattern || "";
      let flags = rule.action.flags || "";
      let regex = new RegExp(pattern, flags);

      switch (rule.rule_type) {
        case "replace":
          if (rule.action.replacement) {
            return text.replace(regex, rule.action.replacement);
          }
          return text;

        case "remove":
          return text.replace(regex, "");

        case "transform":
          if (rule.action.case_conversion === "upper") {
            return text.toUpperCase();
          }
          if (rule.action.case_conversion === "lower") {
            return text.toLowerCase();
          }
          if (rule.action.case_conversion === "title") {
            return text.replace(/\b\w/g, (char) => char.toUpperCase());
          }
          if (rule.action.case_conversion === "sentence") {
            return text.replace(/(^\s*\w|[.!?]\s*\w)/g, (char) =>
              char.toUpperCase()
            );
          }
          return text;

        case "insert":
          if (rule.action.insert_text) {
            return text.replace(regex, (match) => {
              if (rule.rule_type === "insert") {
                return `${rule.action.insert_text}${match}`;
              }
              return match;
            });
          }
          return text;

        default:
          return text;
      }
    } catch (error) {
      console.error("Failed to apply rule:", error);
      return text;
    }
  };

  return (
    <section className="forma-panel surface" aria-label="Conduit Forma">
      <div className="forma-layout">
        <div className="forma-sidebar">
          <div className="panel-heading-row">
            <div>
              <h2>Rule Sets</h2>
              <p className="panel-caption">
                Collections of transformation rules
              </p>
            </div>
            <button
              className="secondary-action"
              type="button"
              onClick={() => {
                const newSet: RuleSet = {
                  id: `set-${Date.now()}`,
                  name: "New Rule Set",
                  description: "",
                  rules: [],
                };
                setRuleSets([...ruleSets, newSet]);
                setSelectedRuleSet(newSet);
              }}
            >
              <Plus size={17} aria-hidden="true" />
              New Set
            </button>
          </div>

          <div className="rule-sets-list">
            {ruleSets.map((set) => (
              <button
                key={set.id}
                type="button"
                className={`rule-set-item ${
                  selectedRuleSet?.id === set.id ? "selected" : ""
                }`}
                onClick={() => setSelectedRuleSet(set)}
              >
                <div className="rule-set-info">
                  <strong>{set.name}</strong>
                  <span>{set.rules.length} rules</span>
                </div>
                {selectedRuleSet?.id === set.id && (
                  <CheckCircle size={16} aria-hidden="true" />
                )}
              </button>
            ))}
          </div>
        </div>

        <div className="forma-main">
          {selectedRuleSet ? (
            <>
              <div className="panel-heading-row">
                <div>
                  <h2>{selectedRuleSet.name}</h2>
                  <p className="panel-caption">
                    {selectedRuleSet.description || "No description"}
                  </p>
                </div>
                <div className="rule-actions">
                  <button
                    className="primary-action"
                    type="button"
                    onClick={handleAddRule}
                  >
                    <Plus size={17} aria-hidden="true" />
                    Add Rule
                  </button>
                  <button
                    className="secondary-action"
                    type="button"
                    onClick={handleTestRules}
                  >
                    <Play size={17} aria-hidden="true" />
                    Test Rules
                  </button>
                </div>
              </div>

              <div className="test-area">
                <label className="field">
                  <span>Test Input</span>
                  <textarea
                    value={testInput}
                    onChange={(e) => setTestInput(e.target.value)}
                    placeholder="Enter text to test transformations..."
                    rows={3}
                  />
                </label>

                {showTestResults && (
                  <div className="test-results">
                    <label className="field">
                      <span>Test Output</span>
                      <textarea
                        value={testOutput}
                        readOnly
                        rows={3}
                      />
                    </label>
                  </div>
                )}
              </div>

              <div className="rules-list">
                {selectedRuleSet.rules
                  .sort((a, b) => b.priority - a.priority)
                  .map((rule) => (
                    <div
                      key={rule.id}
                      className={`rule-item ${
                        editingRule?.id === rule.id ? "editing" : ""
                      } ${!rule.enabled ? "disabled" : ""}`}
                    >
                      <div className="rule-header">
                        <div className="rule-info">
                          <strong>{rule.name}</strong>
                          <span className="rule-type">
                            {rule.rule_type.toUpperCase()}
                          </span>
                          <span className="rule-priority">
                            Priority: {rule.priority}
                          </span>
                        </div>
                        <div className="rule-actions">
                          <button
                            className="icon-action"
                            type="button"
                            aria-label={rule.enabled ? "Disable" : "Enable"}
                            onClick={() => handleToggleRule(rule.id)}
                          >
                            {rule.enabled ? (
                              <CheckCircle size={16} aria-hidden="true" />
                            ) : (
                              <AlertCircle size={16} aria-hidden="true" />
                            )}
                          </button>
                          <button
                            className="icon-action"
                            type="button"
                            aria-label="Edit rule"
                            onClick={() => setEditingRule(rule)}
                          >
                            <Settings size={16} aria-hidden="true" />
                          </button>
                          <button
                            className="icon-action danger"
                            type="button"
                            aria-label="Delete rule"
                            onClick={() => handleDeleteRule(rule.id)}
                          >
                            <Trash2 size={16} aria-hidden="true" />
                          </button>
                        </div>
                      </div>

                      <div className="rule-description">
                        {rule.description || "No description"}
                      </div>

                      <div className="rule-details">
                        <div className="rule-condition">
                          <span className="detail-label">Condition:</span>
                          <span className="detail-value">
                            {rule.condition}
                          </span>
                        </div>
                        <div className="rule-action">
                          <span className="detail-label">Action:</span>
                          <span className="detail-value">
                            {rule.action.pattern && (
                              <code>
                                {rule.rule_type === "replace"
                                  ? `s/${rule.action.pattern}/${rule.action.replacement}/`
                                  : `/${rule.action.pattern}/`}
                              </code>
                            )}
                            {rule.action.case_conversion && (
                              <span>Case: {rule.action.case_conversion}</span>
                            )}
                          </span>
                        </div>
                      </div>
                    </div>
                  ))}
              </div>
            </>
          ) : (
            <div className="empty-state">
              <Settings size={48} aria-hidden="true" />
              <p>Select a rule set to manage transformation rules</p>
            </div>
          )}
        </div>
      </div>

      {editingRule && (
        <RuleEditor
          rule={editingRule}
          onSave={handleSaveRule}
          onCancel={() => setEditingRule(null)}
        />
      )}
    </section>
  );
}

interface RuleEditorProps {
  rule: FormaRule;
  onSave: (rule: FormaRule) => void;
  onCancel: () => void;
}

function RuleEditor({ rule, onSave, onCancel }: RuleEditorProps) {
  const [editedRule, setEditedRule] = useState<FormaRule>({ ...rule });

  return (
    <div className="rule-editor-overlay">
      <div className="rule-editor">
        <div className="rule-editor-header">
          <h3>Edit Rule</h3>
          <button
            className="icon-action"
            type="button"
            onClick={onCancel}
            aria-label="Close"
          >
            <X size={20} aria-hidden="true" />
          </button>
        </div>

        <div className="rule-editor-body">
          <label className="field">
            <span>Rule Name</span>
            <input
              type="text"
              value={editedRule.name}
              onChange={(e) =>
                setEditedRule({ ...editedRule, name: e.target.value })
              }
            />
          </label>

          <label className="field">
            <span>Description</span>
            <textarea
              value={editedRule.description}
              onChange={(e) =>
                setEditedRule({ ...editedRule, description: e.target.value })
              }
              rows={3}
            />
          </label>

          <div className="field-row">
            <label className="field">
              <span>Rule Type</span>
              <select
                value={editedRule.rule_type}
                onChange={(e) =>
                  setEditedRule({
                    ...editedRule,
                    rule_type: e.target.value as any,
                  })
                }
              >
                <option value="replace">Replace</option>
                <option value="remove">Remove</option>
                <option value="transform">Transform</option>
                <option value="insert">Insert</option>
                <option value="script">Script</option>
              </select>
            </label>

            <label className="field">
              <span>Priority</span>
              <input
                type="number"
                value={editedRule.priority}
                onChange={(e) =>
                  setEditedRule({
                    ...editedRule,
                    priority: parseInt(e.target.value) || 0,
                  })
                }
              />
            </label>
          </div>

          <label className="field">
            <span>Condition</span>
            <select
              value={editedRule.condition}
              onChange={(e) =>
                setEditedRule({
                  ...editedRule,
                  condition: e.target.value as any,
                })
              }
            >
              <option value="always">Always</option>
              <option value="matches_pattern">Matches Pattern</option>
              <option value="contains">Contains</option>
              <option value="starts_with">Starts With</option>
              <option value="ends_with">Ends With</option>
              <option value="custom">Custom</option>
            </select>
          </label>

          {editedRule.rule_type === "replace" && (
            <>
              <label className="field">
                <span>Pattern (Regex)</span>
                <input
                  type="text"
                  value={editedRule.action.pattern || ""}
                  onChange={(e) =>
                    setEditedRule({
                      ...editedRule,
                      action: {
                        ...editedRule.action,
                        pattern: e.target.value,
                      },
                    })
                  }
                />
              </label>

              <label className="field">
                <span>Replacement</span>
                <input
                  type="text"
                  value={editedRule.action.replacement || ""}
                  onChange={(e) =>
                    setEditedRule({
                      ...editedRule,
                      action: {
                        ...editedRule.action,
                        replacement: e.target.value,
                      },
                    })
                  }
                />
              </label>

              <label className="field">
                <span>Flags (e.g., 'gi' for global, case-insensitive)</span>
                <input
                  type="text"
                  value={editedRule.action.flags || ""}
                  onChange={(e) =>
                    setEditedRule({
                      ...editedRule,
                      action: {
                        ...editedRule.action,
                        flags: e.target.value,
                      },
                    })
                  }
                />
              </label>
            </>
          )}

          {editedRule.rule_type === "transform" && (
            <label className="field">
              <span>Case Conversion</span>
              <select
                value={editedRule.action.case_conversion || "lower"}
                onChange={(e) =>
                  setEditedRule({
                    ...editedRule,
                    action: {
                      ...editedRule.action,
                      case_conversion: e.target.value as any,
                    },
                  })
                }
              >
                <option value="upper">UPPERCASE</option>
                <option value="lower">lowercase</option>
                <option value="title">Title Case</option>
                <option value="sentence">Sentence case</option>
              </select>
            </label>
          )}

          <label className="check-row">
            <input
              type="checkbox"
              checked={editedRule.enabled}
              onChange={(e) =>
                setEditedRule({ ...editedRule, enabled: e.target.checked })
              }
            />
            <span>Enabled</span>
          </label>
        </div>

        <div className="rule-editor-footer">
          <button className="secondary-action" type="button" onClick={onCancel}>
            Cancel
          </button>
          <button
            className="primary-action"
            type="button"
            onClick={() => onSave(editedRule)}
          >
            <Save size={17} aria-hidden="true" />
            Save Rule
          </button>
        </div>
      </div>
    </div>
  );
}