# Forma - Custom Transformation Rules

Forma is a service for defining transformation rules for utterances prior to output/TTS. It extends beyond Conduit's built-in transforms by providing a flexible, user-configurable rule system.

## Features

- **Custom Transformation Rules**: Define your own text transformation patterns using regex
- **Multiple Rule Types**: Replace, remove, transform (case conversion), insert, and custom scripts
- **Conditional Application**: Apply rules based on conditions (always, contains, starts with, ends with, patterns)
- **Priority-based Execution**: Control the order in which rules are applied
- **Test Environment**: Preview transformations before applying them
- **Rule Sets**: Organize rules into collections for different use cases

## Architecture

Forma consists of several components:

1. **conduit-forma**: Rust crate providing the transformation engine and provider
2. **Frontend UI**: Operator console section for managing rules
3. **Storage**: In-memory and persistent storage backends for rules

## Rule Types

### Replace
Replaces text matching a regex pattern with new text.

Example: Replace "hello" with "hi"
- Pattern: `hello`
- Replacement: `hi`
- Flags: `g` (global)

### Remove
Removes text matching a regex pattern.

Example: Remove all emoji characters
- Pattern: `[\p{Emoji_Presentation}\p{Extended_Pictographic}]`
- Flags: `gu` (global, unicode)

### Transform
Applies case conversion to text.

Options:
- `upper`: Convert to UPPERCASE
- `lower`: Convert to lowercase  
- `title`: Convert to Title Case
- `sentence`: Convert to Sentence case

### Insert
Inserts text before or after matches.

Example: Add emphasis before important words
- Pattern: `important|critical|urgent`
- Insert text: `⚠️ `
- Position: Before

### Script
Custom script-based transformations (future feature).

## Rule Conditions

Rules can be applied conditionally:

- **Always**: Apply to all text
- **Contains**: Apply only if text contains a substring
- **Starts With**: Apply only if text starts with a substring
- **Ends With**: Apply only if text ends with a substring
- **Matches Pattern**: Apply only if text matches a regex pattern
- **Custom**: Apply only if a custom condition evaluates to true (future)

## Using Forma

### As a Transform Provider

Forma can be used as a transform provider in Conduit pipelines:

```rust
use conduit_forma::{FormaProvider, Engine, MemoryStore, FormaRule, RuleAction, RuleCondition, RuleType};

let store = Arc::new(MemoryStore::new());

let rule = FormaRule::new("clean-emoji", "Remove Emoji")
    .with_description("Strip emoji characters")
    .with_type(RuleType::Remove)
    .with_condition(RuleCondition::Always)
    .with_action(RuleAction::Remove {
        pattern: "[\\p{Emoji_Presentation}\\p{Extended_Pictographic}]".to_string(),
        flags: "gu".to_string(),
    })
    .with_enabled(true)
    .with_priority(10);

// Create a rule set and add the rule
let rule_set = RuleSet {
    id: "clean".to_string(),
    name: "Clean Rules".to_string(),
    description: "Text cleanup rules".to_string(),
    rules: vec![rule],
};

store.create_rule_set(rule_set).await.unwrap();

// Create the provider with the rule set
let provider = FormaProvider::with_storage("forma-transform", store)
    .with_rule_set("clean")
    .with_label("Forma Transformations");

// Use in a pipeline like any other transform provider
```

### Via the Operator Console

1. Navigate to the "Forma" section in the operator console
2. Create a new rule set or select an existing one
3. Add rules with the desired transformations
4. Test rules using the test area
5. Save and apply to pipelines

## Example Rule Sets

### Clean Text
- Remove emojis
- Convert markdown to speech-friendly text
- Collapse multiple spaces

### Accessibility
- Expand abbreviations (e.g., "ASAP" → "as soon as possible")
- Convert technical terms to plain language
- Add pauses before important announcements

### Localization
- Convert US English to UK English (color → colour)
- Handle measurement unit conversions
- Adapt cultural references

## API Reference

### FormaProvider

```rust
pub struct FormaProvider {
    // Provider implementation details
}
```

**Methods:**
- `new(name: impl Into<String>) -> Self`: Create a new provider with in-memory storage
- `with_storage(name: impl Into<String>, store: Arc<dyn FormaStore>) -> Self`: Create with custom storage
- `with_rule_set(rule_set_id: impl Into<String>) -> Self`: Set the rule set to use
- `with_label(label: impl Into<String>) -> Self`: Set human-readable label

### FormaRule

```rust
pub struct FormaRule {
    pub id: String,
    pub name: String,
    pub description: String,
    pub rule_type: RuleType,
    pub condition: RuleCondition,
    pub action: RuleAction,
    pub enabled: bool,
    pub priority: i32,
}
```

**Builder Methods:**
- `new(id: impl Into<String>, name: impl Into<String>) -> Self`
- `with_description(description: impl Into<String>) -> Self`
- `with_type(rule_type: RuleType) -> Self`
- `with_condition(condition: RuleCondition) -> Self`
- `with_action(action: RuleAction) -> Self`
- `with_enabled(enabled: bool) -> Self`
- `with_priority(priority: i32) -> Self`
- `validate(&self) -> Result<(), String>`: Validate rule configuration

### Engine

```rust
pub struct Engine {
    // Transformation engine
}
```

**Methods:**
- `new() -> Self`: Create a new engine
- `apply_rule(&self, text: &str, rule: &FormaRule) -> Result<String, FormaError>`
- `apply_rules(&self, text: &str, rules: &[FormaRule]) -> Result<String, FormaError>`

## Storage Backends

### MemoryStore
In-memory storage for testing and development.

```rust
use conduit_forma::MemoryStore;

let store = Arc::new(MemoryStore::new());
```

### Custom Storage
Implement the `FormaStore` trait for custom storage backends:

```rust
use conduit_forma::{FormaStore, RuleSet, FormaRule, FormaError};

#[async_trait::async_trait]
impl FormaStore for MyCustomStore {
    async fn create_rule_set(&self, rule_set: RuleSet) -> Result<(), FormaError> {
        // Implementation
    }
    // ... other trait methods
}
```

## Error Handling

Forma provides detailed error types:

```rust
pub enum FormaError {
    Validation(String),    // Rule validation failed
    Execution(String),     // Rule execution failed
    Storage(String),       // Storage operation failed
    InvalidRuleId(String), // Rule ID not found
    RuleSetNotFound(String), // Rule set not found
}
```

## Performance Considerations

- Rules are sorted by priority before execution
- Regex patterns are compiled and cached for performance
- In-memory storage is fast but not persistent
- For production use, implement a persistent storage backend

## Future Enhancements

- Custom script execution (JavaScript/Python)
- Rule templates and presets
- Import/export rule sets
- Version control for rule changes
- A/B testing for rule effectiveness
- Machine learning-based transformations

## Contributing

Forma follows Conduit's development guidelines. See `AGENTS.md` for details.

## License

MIT OR Apache-2.0 (same as Conduit)