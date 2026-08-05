//! One uniform description of a provider: what it is, what it can do, and
//! what it needs configured.
//!
//! A provider used to answer those three questions in three different ways.
//! Its identity was whatever string it happened to be registered under, its
//! capabilities were scattered across per-trait methods with different
//! defaulting rules — `models()` returning a slice, `supports_tools()`
//! returning a bool, `voices()` returning a future — and its provider-specific
//! settings were an untyped `serde_json::Value` on every request that nothing
//! validated and nothing could render.
//!
//! A [`Descriptor`] answers all three in one place, so an operator screen, the
//! status layer, and the runtime can each ask any provider the same question
//! and get a machine-readable answer, whatever capability it supplies.

use conduit_core::audio::Encoding;
use conduit_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::registry::Capability;
use crate::tts::Voice;
use crate::wake::WakePhrase;

/// What a provider is, what it can do, and what it needs configured.
///
/// Built once, when the provider is constructed, and returned by reference
/// from [`Provider::descriptor`](crate::Provider::descriptor): a provider has
/// exactly one of these, and everything that used to be asked of it
/// individually is read from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Descriptor {
    /// Stable identity of this provider, e.g. `"openai"` or `"ollama"`.
    ///
    /// Distinct from both the display name and the registry key. The key is
    /// the deployment's selector — what a pipeline graph names — and an
    /// operator may register the same implementation twice under two keys.
    /// This is what the provider calls itself: it appears in metric labels and
    /// in error messages, so it must not change between versions.
    pub id: String,
    /// Human-readable name for operator screens, e.g. `"OpenAI (hosted)"`.
    ///
    /// Free to change; nothing keys off it.
    pub label: String,
    /// Provider version, surfaced in diagnostics.
    pub version: String,
    /// The capability this provider supplies.
    pub capability: Capability,
    /// What this provider can do, in the one shape every capability shares.
    pub metadata: Metadata,
    /// The provider-specific settings a request may carry.
    pub settings: SettingsSchema,
}

impl Descriptor {
    /// A descriptor for `id` supplying `capability`, with the crate version,
    /// `id` as its label, no capability metadata, and no settings.
    ///
    /// Everything beyond identity is added with the `with_*` builders, so a
    /// provider declares exactly what it has and defaults are visible at the
    /// call site rather than buried in a trait method.
    #[must_use]
    pub fn new(id: impl Into<String>, capability: Capability) -> Self {
        let id = id.into();
        Self {
            label: id.clone(),
            id,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            capability,
            metadata: Metadata::default(),
            settings: SettingsSchema::none(),
        }
    }

    /// Sets the human-readable name shown on operator screens.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Sets the version reported in diagnostics.
    #[must_use]
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Sets what this provider can do.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Declares the provider-specific settings a request may carry.
    #[must_use]
    pub fn with_settings(mut self, settings: SettingsSchema) -> Self {
        self.settings = settings;
        self
    }

    /// Checks `values` against the declared settings schema.
    ///
    /// The only way a request's provider-specific settings are built, which is
    /// the point: a value that reaches a provider has been checked against
    /// what that provider said it accepts.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] naming the offending setting.
    pub fn validate_settings(&self, values: &Value) -> Result<Settings> {
        self.settings.validate(values).map_err(|detail| {
            Error::Config(format!("provider `{}` rejected its settings: {detail}", self.id))
        })
    }

    /// Checks `values` as an override of settings that are configured
    /// elsewhere: named settings are validated, absent ones are left absent.
    ///
    /// What a pipeline node's per-node settings are built from. A node names
    /// only what it wants to change about a Configured Provider it shares with
    /// other pipelines, so [`validate_settings`](Descriptor::validate_settings)
    /// is the wrong check twice over: filling in declared defaults would let a
    /// node that named one setting displace every stored default beside it, and
    /// enforcing `required` would demand a node repeat settings the Configured
    /// Provider already carries.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] naming the offending setting.
    pub fn validate_overrides(&self, values: &Value) -> Result<Settings> {
        self.settings.validate_overrides(values).map_err(|detail| {
            Error::Config(format!("provider `{}` rejected its settings: {detail}", self.id))
        })
    }
}

/// Defines a [`Provider::descriptor`](crate::Provider::descriptor) method for
/// a provider whose whole description is an identity and a capability.
///
/// Written inside the `impl Provider` block, so a stand-in that also overrides
/// [`health`](crate::Provider::health) still writes one impl:
///
/// ```
/// # use conduit_provider::{Capability, Health, Provider};
/// struct NeverReachable;
///
/// #[async_trait::async_trait]
/// impl Provider for NeverReachable {
///     conduit_provider::stub_descriptor!("never-reachable", Capability::SpeakerId);
///
///     async fn health(&self) -> Health {
///         Health::Unhealthy { reason: "there is no service".to_owned() }
///     }
/// }
/// ```
///
/// Anything that serves models, speaks with voices, listens for phrases, or
/// accepts settings builds and stores a [`Descriptor`] of its own instead —
/// those are what a caller actually reads off it.
#[macro_export]
macro_rules! stub_descriptor {
    ($id:expr, $capability:expr) => {
        fn descriptor(&self) -> &$crate::Descriptor {
            static DESCRIPTOR: ::std::sync::OnceLock<$crate::Descriptor> =
                ::std::sync::OnceLock::new();
            DESCRIPTOR.get_or_init(|| $crate::Descriptor::new($id, $capability))
        }
    };
}

/// What a provider can do, in the one shape every capability shares.
///
/// A recognizer fills in `languages` and `encodings`, a model fills in `models`
/// and `tools`, a synthesizer fills in `voices` and `encodings`, a wake detector
/// fills in `phrases`, an activity detector fills in `sample_rates` — and
/// anything reading a provider generically reads the
/// same struct for all of them. An empty list means "unrestricted", not
/// "none": that is what an OpenAI-compatible local endpoint says when it
/// passes any model name straight through, and what a Wyoming server says when
/// it scores whatever phrase models it happens to have loaded.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    /// Models this provider serves. Empty means any model name passes through.
    pub models: Vec<String>,
    /// BCP-47 tags this provider handles. Empty means unrestricted.
    pub languages: Vec<String>,
    /// Voices this provider speaks with. Empty means it enumerates none, so an
    /// operator types a voice name instead of choosing from a catalogue.
    pub voices: Vec<Voice>,
    /// Wake phrases this provider has models for, with the thresholds it was
    /// configured with. Empty means it loads or trains phrases on demand.
    pub phrases: Vec<WakePhrase>,
    /// Audio encodings this provider accepts or produces. Empty means it
    /// decides at runtime and accepts whatever it is given.
    pub encodings: Vec<Encoding>,
    /// Sample rates this provider scores audio at, in hertz. Empty means it
    /// accepts any.
    ///
    /// Declared by the fixed-window providers, which is to say the activity
    /// detectors: a wrong rate does not degrade one, it makes its window the
    /// wrong length of sound, so the mismatch is refused at registration rather
    /// than resampled away. A served detector that adapts declares none.
    pub sample_rates: Vec<u32>,
    /// Whether this provider can execute tool calls.
    pub tools: bool,
}

impl Metadata {
    /// Sets the models this provider serves.
    #[must_use]
    pub fn with_models(mut self, models: Vec<String>) -> Self {
        self.models = models;
        self
    }

    /// Sets the BCP-47 tags this provider handles.
    #[must_use]
    pub fn with_languages(mut self, languages: Vec<String>) -> Self {
        self.languages = languages;
        self
    }

    /// Sets the voices this provider speaks with.
    #[must_use]
    pub fn with_voices(mut self, voices: Vec<Voice>) -> Self {
        self.voices = voices;
        self
    }

    /// Sets the wake phrases this provider has models for.
    #[must_use]
    pub fn with_phrases(mut self, phrases: Vec<WakePhrase>) -> Self {
        self.phrases = phrases;
        self
    }

    /// Sets the audio encodings this provider handles.
    #[must_use]
    pub fn with_encodings(mut self, encodings: Vec<Encoding>) -> Self {
        self.encodings = encodings;
        self
    }

    /// Sets the sample rates this provider scores audio at.
    #[must_use]
    pub fn with_sample_rates(mut self, sample_rates: Vec<u32>) -> Self {
        self.sample_rates = sample_rates;
        self
    }

    /// Declares that this provider can execute tool calls.
    #[must_use]
    pub const fn with_tools(mut self) -> Self {
        self.tools = true;
        self
    }

    /// Whether this provider can handle audio in `encoding`.
    ///
    /// An empty [`Metadata::encodings`] accepts everything, which is what an
    /// adapter whose backend decides at runtime means by declaring nothing.
    #[must_use]
    pub fn supports_encoding(&self, encoding: Encoding) -> bool {
        self.encodings.is_empty() || self.encodings.contains(&encoding)
    }

    /// Whether this provider serves `model`.
    ///
    /// An empty [`Metadata::models`] serves everything, which is what an
    /// OpenAI-compatible endpoint means by advertising no catalogue.
    #[must_use]
    pub fn serves_model(&self, model: &str) -> bool {
        self.models.is_empty() || self.models.iter().any(|served| served == model)
    }
}

/// The settings a provider accepts, declared as JSON Schema.
///
/// The same shape a [`ToolSpec`](crate::llm::ToolSpec) already advertises to a
/// model, for the same reason: a machine-readable description of an object is
/// what lets a form be rendered and a value be checked without either side
/// knowing what the other is.
///
/// # Supported keywords
///
/// Validation covers the subset a provider setting is actually written in, and
/// nothing more — a whole JSON Schema implementation would be a dependency
/// carried for keywords no descriptor uses:
///
/// - `type` — `string`, `number`, `integer`, `boolean`, `array`, `object`,
///   `null`
/// - `enum` — the value must be one of the listed values
/// - `minimum` / `maximum` — inclusive bounds on numbers
/// - `required` — properties that must be present
/// - `default` — filled in for an absent property
/// - `additionalProperties` — `false` (the default here) rejects unknown
///   settings, which is what makes a typo an error rather than a value the
///   provider silently ignores
///
/// Anything else in the schema is carried through to whoever renders it and
/// ignored when validating.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SettingsSchema {
    schema: Value,
}

impl SettingsSchema {
    /// A provider that accepts no settings at all.
    #[must_use]
    pub fn none() -> Self {
        Self { schema: serde_json::json!({ "type": "object", "properties": {} }) }
    }

    /// A schema from a JSON Schema object.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] unless `schema` is a JSON object describing
    /// an object — a provider's settings are always named values.
    pub fn new(schema: Value) -> Result<Self> {
        let object = schema.as_object().ok_or_else(|| {
            Error::Config("a settings schema must be a JSON object".to_owned())
        })?;
        match object.get("type") {
            Some(Value::String(kind)) if kind == "object" => Ok(Self { schema }),
            _ => Err(Error::Config(
                "a settings schema must declare `\"type\": \"object\"`".to_owned(),
            )),
        }
    }

    /// The schema as JSON, for rendering a form or publishing an API contract.
    #[must_use]
    pub const fn as_json(&self) -> &Value {
        &self.schema
    }

    /// Whether this provider declares any settings at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.properties().is_none_or(Map::is_empty)
    }

    fn properties(&self) -> Option<&Map<String, Value>> {
        self.schema.get("properties")?.as_object()
    }

    fn required(&self) -> impl Iterator<Item = &str> {
        self.schema
            .get("required")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(Value::as_str)
    }

    /// Whether settings this schema does not name are accepted.
    fn allows_unknown(&self) -> bool {
        self.schema.get("additionalProperties") == Some(&Value::Bool(true))
    }

    /// Checks `values` against this schema, filling in declared defaults.
    ///
    /// # Errors
    ///
    /// Returns a message naming the offending setting.
    fn validate(&self, values: &Value) -> std::result::Result<Settings, String> {
        let given = self.named(values)?;
        let properties = self.properties().cloned().unwrap_or_default();

        let mut checked = Map::new();
        for (name, declared) in &properties {
            match given.get(name) {
                Some(value) => {
                    check_value(name, value, declared)?;
                    checked.insert(name.clone(), value.clone());
                }
                None => {
                    if let Some(default) = declared.get("default") {
                        checked.insert(name.clone(), default.clone());
                    }
                }
            }
        }
        for name in self.required() {
            if !checked.contains_key(name) {
                return Err(format!("missing required setting `{name}`"));
            }
        }
        // Unknown settings only reach here when the schema opted into them, so
        // carrying them through is what the schema asked for.
        for (name, value) in given {
            checked.entry(name).or_insert(value);
        }

        Ok(Settings { values: checked })
    }

    /// Checks `values` as an override: what it names is validated, what it
    /// omits stays omitted.
    ///
    /// See [`Descriptor::validate_overrides`] for why neither declared defaults
    /// nor `required` apply to an override.
    ///
    /// # Errors
    ///
    /// Returns a message naming the offending setting.
    fn validate_overrides(&self, values: &Value) -> std::result::Result<Settings, String> {
        let given = self.named(values)?;
        let properties = self.properties().cloned().unwrap_or_default();

        let mut checked = Map::new();
        for (name, value) in given {
            if let Some(declared) = properties.get(&name) {
                check_value(&name, &value, declared)?;
            }
            checked.insert(name, value);
        }
        Ok(Settings { values: checked })
    }

    /// The settings `values` names, rejecting any this schema does not declare.
    ///
    /// Absent settings and `{}` are the same request — the caller named nothing
    /// — so both answer with an empty map rather than one of them being an
    /// error.
    fn named(&self, values: &Value) -> std::result::Result<Map<String, Value>, String> {
        let given = match values {
            Value::Null => Map::new(),
            Value::Object(given) => given.clone(),
            other => return Err(format!("settings must be an object, got {}", kind_of(other))),
        };
        if !self.allows_unknown() {
            let properties = self.properties();
            for name in given.keys() {
                if !properties.is_some_and(|properties| properties.contains_key(name)) {
                    return Err(format!("unknown setting `{name}`"));
                }
            }
        }
        Ok(given)
    }
}

/// Checks one value against one property schema.
fn check_value(name: &str, value: &Value, declared: &Value) -> std::result::Result<(), String> {
    if let Some(Value::String(expected)) = declared.get("type") {
        if !matches_type(value, expected) {
            return Err(format!("setting `{name}` must be {expected}, got {}", kind_of(value)));
        }
    }
    if let Some(Value::Array(allowed)) = declared.get("enum") {
        if !allowed.contains(value) {
            let allowed =
                allowed.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
            return Err(format!("setting `{name}` must be one of [{allowed}], got {value}"));
        }
    }
    if let (Some(number), Some(minimum)) =
        (value.as_f64(), declared.get("minimum").and_then(Value::as_f64))
    {
        if number < minimum {
            return Err(format!("setting `{name}` must be at least {minimum}, got {number}"));
        }
    }
    if let (Some(number), Some(maximum)) =
        (value.as_f64(), declared.get("maximum").and_then(Value::as_f64))
    {
        if number > maximum {
            return Err(format!("setting `{name}` must be at most {maximum}, got {number}"));
        }
    }
    Ok(())
}

fn matches_type(value: &Value, expected: &str) -> bool {
    match expected {
        "string" => value.is_string(),
        // An integer is a number, and a number that happens to be whole is
        // still accepted where an integer is asked for: JSON has one numeric
        // type and a form that posts `1` should not depend on whether the
        // sender wrote a decimal point.
        "number" => value.is_number(),
        "integer" => value.as_f64().is_some_and(|number| number.fract() == 0.0),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        // A type this validator predates is one it cannot contradict.
        _ => true,
    }
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Provider-specific settings that have been checked against a provider's
/// declared schema.
///
/// A request carries one of these rather than a free-form `serde_json::Value`,
/// so a provider reading a setting knows the value is one it said it accepts
/// and an operator who mistyped one is told so rather than having it silently
/// dropped.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Settings {
    values: Map<String, Value>,
}

impl Settings {
    /// No settings — every declared default, for a provider that declares
    /// none.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// The value of one setting, if it is present.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }

    /// One setting as a string.
    #[must_use]
    pub fn string(&self, name: &str) -> Option<&str> {
        self.get(name)?.as_str()
    }

    /// One setting as a number.
    #[must_use]
    pub fn number(&self, name: &str) -> Option<f64> {
        self.get(name)?.as_f64()
    }

    /// One setting as a boolean.
    #[must_use]
    pub fn boolean(&self, name: &str) -> Option<bool> {
        self.get(name)?.as_bool()
    }

    /// Whether nothing is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The settings as JSON, for a provider that forwards them to its backend.
    #[must_use]
    pub const fn as_map(&self) -> &Map<String, Value> {
        &self.values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> SettingsSchema {
        SettingsSchema::new(serde_json::json!({
            "type": "object",
            "properties": {
                "reasoning_effort": { "type": "string", "enum": ["low", "high"] },
                "top_p": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                "seed": { "type": "integer" },
                "stream": { "type": "boolean", "default": true },
            },
            "required": ["reasoning_effort"],
        }))
        .expect("an object schema")
    }

    fn descriptor() -> Descriptor {
        Descriptor::new("openai", Capability::Llm).with_settings(schema())
    }

    #[test]
    fn a_descriptor_labels_itself_with_its_id_until_it_is_given_a_name() {
        let descriptor = Descriptor::new("openai", Capability::Llm);
        assert_eq!(descriptor.label, "openai");
        assert_eq!(descriptor.with_label("OpenAI (hosted)").label, "OpenAI (hosted)");
    }

    #[test]
    fn a_settings_schema_must_describe_an_object() {
        assert!(SettingsSchema::new(serde_json::json!({ "type": "string" })).is_err());
        assert!(SettingsSchema::new(serde_json::json!(["nope"])).is_err());
        assert!(SettingsSchema::new(serde_json::json!({ "type": "object" })).is_ok());
    }

    #[test]
    fn declaring_no_settings_is_an_empty_schema() {
        assert!(SettingsSchema::none().is_empty());
        assert!(!schema().is_empty());
    }

    #[test]
    fn declared_settings_pass_and_defaults_are_filled_in() {
        let settings = descriptor()
            .validate_settings(&serde_json::json!({ "reasoning_effort": "high", "seed": 7 }))
            .expect("valid settings");

        assert_eq!(settings.string("reasoning_effort"), Some("high"));
        assert_eq!(settings.number("seed"), Some(7.0));
        assert_eq!(settings.boolean("stream"), Some(true), "the declared default is filled in");
    }

    #[test]
    fn a_mistyped_setting_is_named_rather_than_dropped() {
        // The whole point of declaring a schema: a setting the provider never
        // had used to travel all the way to the backend and be ignored there.
        let error = descriptor()
            .validate_settings(&serde_json::json!({
                "reasoning_effort": "high",
                "reasoning_efort": "high"
            }))
            .expect_err("unknown setting");
        assert!(error.to_string().contains("reasoning_efort"), "{error}");
    }

    #[test]
    fn a_setting_of_the_wrong_type_is_refused() {
        let error = descriptor()
            .validate_settings(&serde_json::json!({ "reasoning_effort": 3 }))
            .expect_err("wrong type");
        assert!(error.to_string().contains("reasoning_effort"), "{error}");
        assert!(error.to_string().contains("string"), "{error}");
    }

    #[test]
    fn a_setting_outside_its_declared_enum_is_refused() {
        let error = descriptor()
            .validate_settings(&serde_json::json!({ "reasoning_effort": "medium" }))
            .expect_err("not an allowed value");
        assert!(error.to_string().contains("medium"), "{error}");
    }

    #[test]
    fn a_number_outside_its_declared_bounds_is_refused() {
        for value in [-0.5, 1.5] {
            let error = descriptor()
                .validate_settings(
                    &serde_json::json!({ "reasoning_effort": "low", "top_p": value }),
                )
                .expect_err("out of bounds");
            assert!(error.to_string().contains("top_p"), "{error}");
        }
    }

    #[test]
    fn a_missing_required_setting_is_refused() {
        let error = descriptor()
            .validate_settings(&serde_json::json!({ "top_p": 0.5 }))
            .expect_err("missing required");
        assert!(error.to_string().contains("reasoning_effort"), "{error}");
    }

    #[test]
    fn naming_no_settings_is_the_same_as_naming_an_empty_object() {
        let none = SettingsSchema::none();
        let descriptor = Descriptor::new("piper", Capability::Tts).with_settings(none);
        assert!(descriptor.validate_settings(&Value::Null).expect("valid").is_empty());
        assert!(descriptor
            .validate_settings(&serde_json::json!({}))
            .expect("valid")
            .is_empty());
    }

    #[test]
    fn a_non_object_settings_value_is_refused() {
        let error = Descriptor::new("piper", Capability::Tts)
            .validate_settings(&serde_json::json!("fast"))
            .expect_err("not an object");
        assert!(error.to_string().contains("object"), "{error}");
    }

    #[test]
    fn a_schema_that_opts_into_unknown_settings_carries_them_through() {
        // A passthrough adapter forwards whatever it is handed; saying so in
        // the schema is how it opts out of the check rather than out of the
        // descriptor.
        let descriptor = Descriptor::new("passthrough", Capability::Llm).with_settings(
            SettingsSchema::new(serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": true,
            }))
            .expect("an object schema"),
        );
        let settings = descriptor
            .validate_settings(&serde_json::json!({ "anything": "goes" }))
            .expect("valid settings");
        assert_eq!(settings.string("anything"), Some("goes"));
    }

    #[test]
    fn an_override_names_only_what_it_changes() {
        // A pipeline node overriding one setting must not silently acquire
        // every declared default beside it: those defaults would then displace
        // whatever the Configured Provider was configured with.
        let overrides = descriptor()
            .validate_overrides(&serde_json::json!({ "top_p": 0.2 }))
            .expect("valid");

        assert_eq!(overrides.number("top_p"), Some(0.2));
        assert_eq!(overrides.get("stream"), None, "a declared default is not filled in");
        assert_eq!(overrides.get("reasoning_effort"), None, "nor a required setting");
    }

    #[test]
    fn an_override_is_still_checked_against_the_schema() {
        // Naming fewer settings is the only latitude an override gets; the ones
        // it does name are checked exactly as a full settings object is.
        for value in [
            serde_json::json!({ "top_p": 5.0 }),
            serde_json::json!({ "top_p": "high" }),
            serde_json::json!({ "reasoning_effort": "medium" }),
            serde_json::json!({ "top-p": 0.2 }),
        ] {
            assert!(
                descriptor().validate_overrides(&value).is_err(),
                "{value} should be refused"
            );
        }
    }

    #[test]
    fn overriding_nothing_is_no_settings_at_all() {
        // What a node that names no overrides resolves to, and the reason this
        // is not `validate_settings`: the empty answer leaves the Configured
        // Provider's stored settings entirely in charge.
        for value in [Value::Null, serde_json::json!({})] {
            assert!(descriptor().validate_overrides(&value).expect("valid").is_empty());
        }
    }

    #[test]
    fn an_empty_metadata_list_means_unrestricted() {
        let metadata = Metadata::default();
        assert!(metadata.supports_encoding(Encoding::Opus));
        assert!(metadata.serves_model("llama3.1:8b"));

        let restricted = Metadata::default()
            .with_encodings(vec![Encoding::PcmS16Le])
            .with_models(vec!["gpt-4o".to_owned()]);
        assert!(restricted.supports_encoding(Encoding::PcmS16Le));
        assert!(!restricted.supports_encoding(Encoding::Opus));
        assert!(restricted.serves_model("gpt-4o"));
        assert!(!restricted.serves_model("llama3.1:8b"));
    }
}
