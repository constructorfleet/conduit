//! Building API server configuration from the environment.
//!
//! Provider Definitions are product runtime configuration. Environment parsing
//! here covers process concerns such as storage, authentication, and runtime
//! bounds.

use std::collections::HashMap;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use conduit_core::{Error, Result};
use conduit_openai::OpenAiConfig;
use conduit_provider::storage::{
    PipelineStore, ProviderDefinitionStore, SpeakerRosterStore, VoxLinkStore,
};
use conduit_runtime::{Providers, DEFAULT_IDLE_TIMEOUT};
use conduit_store::{FileStore, MemoryStore};

use crate::auth::{Access, Tokens, ALLOW_ANONYMOUS, TOKENS_FILE};
use crate::esphome::EsphomeDashboard;
use crate::turns::TurnHistoryRetention;

/// Base URL of an OpenAI-compatible server.
#[cfg(test)]
const BASE_URL: &str = "CONDUIT_OPENAI_BASE_URL";
/// Bearer token for that server. Local servers usually need none.
#[cfg(test)]
const API_KEY: &str = "CONDUIT_OPENAI_API_KEY";
/// Registry name, so two servers can be configured side by side.
#[cfg(test)]
const NAME: &str = "CONDUIT_OPENAI_NAME";
/// Transcription model, e.g. `whisper-1`. Enables the recognizer.
#[cfg(test)]
const STT_MODEL: &str = "CONDUIT_OPENAI_STT_MODEL";
/// Speech model, e.g. `tts-1`. Enables the synthesizer.
#[cfg(test)]
const TTS_MODEL: &str = "CONDUIT_OPENAI_TTS_MODEL";
/// How long the server may go silent mid-response, in seconds. `0` disables it.
const READ_TIMEOUT: &str = "CONDUIT_OPENAI_READ_TIMEOUT_SECS";

/// How long a turn may publish nothing before it is abandoned, in seconds.
/// `0` removes the bound.
const TURN_IDLE_TIMEOUT: &str = "CONDUIT_TURN_IDLE_TIMEOUT_SECS";
/// Maximum completed reconstructed turns retained in memory. `0` removes the bound.
const TURN_HISTORY_MAX_TURNS: &str = "CONDUIT_TURN_HISTORY_MAX_TURNS";
/// Maximum age for completed reconstructed turns, in seconds. `0` removes the bound.
const TURN_HISTORY_RETENTION: &str = "CONDUIT_TURN_HISTORY_RETENTION_SECS";

/// Base URL of an ESPHome dashboard to hand rendered fragments to. Unset means
/// no flash integration, which is the default: flashing is an opt-in
/// relationship with a service Conduit does not own.
const ESPHOME_URL: &str = "CONDUIT_ESPHOME_URL";
/// The `Authorization` header value that dashboard requires, if it requires one.
const ESPHOME_CREDENTIAL: &str = "CONDUIT_ESPHOME_CREDENTIAL";

/// Base directory for Conduit-managed local data.
const DATA_DIR: &str = "CONDUIT_DATA_DIR";
/// Directory to keep pipeline definitions in. Unset means the default data directory.
const PIPELINE_DIR: &str = "CONDUIT_PIPELINE_DIR";
/// Directory to keep provider definitions in. Unset means the default data directory.
const PROVIDER_DIR: &str = "CONDUIT_PROVIDER_DIR";
/// Directory to keep the speaker roster in. Unset means the default data directory.
const SPEAKER_DIR: &str = "CONDUIT_SPEAKER_DIR";
/// Directory to keep Conduit Vox link records in. Unset means the default data directory.
const VOX_LINK_DIR: &str = "CONDUIT_VOX_LINK_DIR";
/// Named only in the error a missing data directory raises: a wake definition
/// carries its own `models_dir`, so there is no variable to set instead.
const WAKE_MODELS_DIR: &str = "the definition's `models_dir`";
const VAD_MODEL_PATH: &str = "the definition's `model_path`";
/// Explicit value for a disposable in-memory pipeline store.
const MEMORY_PIPELINE_DIR: &str = ":memory:";
/// PostgreSQL connection URL. Takes precedence over a directory.
const DATABASE_URL: &str = "CONDUIT_DATABASE_URL";

/// The ESPHome dashboard rendered fragments are handed to, if one is configured.
///
/// Absent by default. A deployment that configured nothing has no ESPHome
/// integration rather than one pointing hopefully at localhost, because dialing
/// an address nobody named is how a convenience becomes a scan.
///
/// The URL is validated here, at startup, rather than at upload time: a typo an
/// operator finds when the server refuses to start is better than one they find
/// the first time they click flash.
///
/// # Errors
///
/// Returns [`Error::Config`] if the URL does not parse, names no host, or uses a
/// scheme other than `http` or `https`.
pub fn esphome_from_env() -> Result<Option<EsphomeDashboard>> {
    let Some(url) = std::env::var(ESPHOME_URL).ok().filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };

    EsphomeDashboard::new(&url, std::env::var(ESPHOME_CREDENTIAL).ok())
        .map(Some)
        // Names the variable rather than only what was wrong with the value,
        // and never echoes the credential — only the URL was ever at issue.
        .map_err(|invalid| Error::Config(format!("{ESPHOME_URL}: {invalid}")))
}

/// Decides who may call the service API.
///
/// There is no open default. A server that authenticated nobody unless
/// configured otherwise would mean every deployment that forgot the token file
/// is exposed and looks fine, which is the failure this exists to prevent. An
/// operator who genuinely wants an open server says so with
/// [`ALLOW_ANONYMOUS`], and gets a warning every time it starts.
///
/// # Errors
///
/// Returns [`Error::Config`] if neither variable is set, if both are, or if the
/// token file cannot be read, is readable by other users, or is invalid.
pub async fn access_from_env() -> Result<Access> {
    let file = std::env::var(TOKENS_FILE).ok().filter(|path| !path.trim().is_empty());
    let anonymous = std::env::var(ALLOW_ANONYMOUS)
        .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "yes"));

    match (file, anonymous) {
        // Both is a contradiction, and guessing which was meant could silently
        // discard the token file and serve the API open.
        (Some(_), true) => Err(Error::Config(format!(
            "{TOKENS_FILE} and {ALLOW_ANONYMOUS} are both set; pick one"
        ))),
        (Some(path), false) => {
            let tokens = Tokens::load(&path).await?;
            tracing::info!(
                path = %path,
                tokens = tokens.len(),
                "authenticating callers against the token file"
            );
            Ok(Access::Tokens(tokens))
        }
        (None, true) => {
            tracing::warn!(
                "{ALLOW_ANONYMOUS} is set: anyone who can reach this port can talk to \
                 the assistant, read transcripts off /v1/events, and delete pipelines"
            );
            Ok(Access::anonymous())
        }
        // Refusing to start is the whole point: an operator is never left
        // unsure whether their deployment is protected.
        (None, false) => Err(Error::Config(format!(
            "no authentication is configured; set {TOKENS_FILE} to a token file, or \
             {ALLOW_ANONYMOUS}=1 to serve the API to anyone who can reach the port"
        ))),
    }
}

/// Opens the pipeline store the environment asks for.
///
/// Defaults to memory, and says so: a server that silently forgot every
/// pipeline on restart would be a nasty surprise in production.
///
/// # Errors
///
/// Returns [`Error::Config`] if the configured directory cannot be used.
pub async fn store_from_env() -> Result<Arc<dyn PipelineStore>> {
    store_from_vars(&std::env::vars().collect()).await
}

/// Opens the provider definition store the environment asks for.
///
/// # Errors
///
/// Returns [`Error::Config`] if the configured directory cannot be used.
pub async fn provider_definition_store_from_env() -> Result<Arc<dyn ProviderDefinitionStore>> {
    provider_definition_store_from_vars(&std::env::vars().collect()).await
}

/// Opens the speaker roster store the environment asks for.
///
/// # Errors
///
/// Returns [`Error::Config`] if the configured directory cannot be used.
pub async fn speaker_roster_store_from_env() -> Result<Arc<dyn SpeakerRosterStore>> {
    speaker_roster_store_from_vars(&std::env::vars().collect()).await
}

/// Opens the Vox link store the environment asks for.
///
/// # Errors
///
/// Returns [`Error::Config`] if the configured directory cannot be used.
pub async fn vox_link_store_from_env() -> Result<Arc<dyn VoxLinkStore>> {
    vox_link_store_from_vars(&std::env::vars().collect()).await
}

/// Opens the speaker roster store described by `vars`.
///
/// A shared database wins over a directory for the same reason it does for
/// pipelines, and more so: the roster is what turns an id into a person, so a
/// replica reading a different copy would answer to the wrong name.
///
/// # Errors
///
/// Returns [`Error::Config`] if the configured directory cannot be used.
pub async fn speaker_roster_store_from_vars(
    vars: &HashMap<String, String>,
) -> Result<Arc<dyn SpeakerRosterStore>> {
    #[cfg(feature = "postgres")]
    if let Some(url) = vars.get(DATABASE_URL) {
        if !url.trim().is_empty() {
            tracing::info!("storing the speaker roster in PostgreSQL");
            return Ok(Arc::new(conduit_store::PostgresStore::connect(url).await?));
        }
    }

    match vars.get(SPEAKER_DIR).map(|value| value.trim()).filter(|value| !value.is_empty()) {
        Some(MEMORY_PIPELINE_DIR) => {
            tracing::warn!(
                "{SPEAKER_DIR} is {MEMORY_PIPELINE_DIR}: the speaker roster is kept in memory \
                 and will be lost on restart"
            );
            Ok(Arc::new(MemoryStore::new()))
        }
        Some(directory) => {
            tracing::info!(%directory, "storing the speaker roster on disk");
            Ok(Arc::new(FileStore::open(directory).await?))
        }
        None => {
            let directory = default_speaker_dir(vars)?;
            tracing::info!(
                directory = %directory.display(),
                "storing the speaker roster in the default local data directory"
            );
            Ok(Arc::new(FileStore::open(directory).await?))
        }
    }
}

/// Opens the pipeline store described by `vars`.
///
/// # Errors
///
/// Returns [`Error::Config`] if the configured directory cannot be used.
pub async fn store_from_vars(vars: &HashMap<String, String>) -> Result<Arc<dyn PipelineStore>> {
    #[cfg(feature = "postgres")]
    if let Some(url) = vars.get(DATABASE_URL) {
        if !url.trim().is_empty() {
            // A shared database is what more than one replica needs, so it
            // wins over a directory only this process can see.
            tracing::info!("storing pipelines in PostgreSQL");
            return Ok(Arc::new(conduit_store::PostgresStore::connect(url).await?));
        }
    }

    #[cfg(not(feature = "postgres"))]
    if vars.get(DATABASE_URL).is_some_and(|url| !url.trim().is_empty()) {
        // Silently falling back to memory would lose pipelines a deployment
        // clearly meant to keep.
        return Err(Error::Config(format!(
            "{DATABASE_URL} is set but this build has no PostgreSQL support; \
             rebuild with --features postgres"
        )));
    }

    match vars.get(PIPELINE_DIR).map(|value| value.trim()).filter(|value| !value.is_empty()) {
        Some(MEMORY_PIPELINE_DIR) => {
            tracing::warn!(
                "{PIPELINE_DIR} is {MEMORY_PIPELINE_DIR}: pipelines are kept in memory and \
                 will be lost on restart"
            );
            Ok(Arc::new(MemoryStore::new()))
        }
        Some(directory) => {
            tracing::info!(%directory, "storing pipelines on disk");
            Ok(Arc::new(FileStore::open(directory).await?))
        }
        None => {
            let directory = default_pipeline_dir(vars)?;
            tracing::info!(
                directory = %directory.display(),
                "storing pipelines in the default local data directory"
            );
            Ok(Arc::new(FileStore::open(directory).await?))
        }
    }
}

/// Opens the provider definition store described by `vars`.
///
/// # Errors
///
/// Returns [`Error::Config`] if the configured directory cannot be used.
pub async fn provider_definition_store_from_vars(
    vars: &HashMap<String, String>,
) -> Result<Arc<dyn ProviderDefinitionStore>> {
    match vars.get(PROVIDER_DIR).map(|value| value.trim()).filter(|value| !value.is_empty()) {
        Some(MEMORY_PIPELINE_DIR) => {
            tracing::warn!(
                "{PROVIDER_DIR} is {MEMORY_PIPELINE_DIR}: provider definitions are kept in memory \
                 and will be lost on restart"
            );
            Ok(Arc::new(MemoryStore::new()))
        }
        Some(directory) => {
            tracing::info!(%directory, "storing provider definitions on disk");
            Ok(Arc::new(FileStore::open(directory).await?))
        }
        None => {
            let directory = default_provider_dir(vars)?;
            tracing::info!(
                directory = %directory.display(),
                "storing provider definitions in the default local data directory"
            );
            Ok(Arc::new(FileStore::open(directory).await?))
        }
    }
}

fn default_pipeline_dir(vars: &HashMap<String, String>) -> Result<PathBuf> {
    default_data_subdir(vars, PIPELINE_DIR, "pipelines")
}

fn default_speaker_dir(vars: &HashMap<String, String>) -> Result<PathBuf> {
    default_data_subdir(vars, SPEAKER_DIR, "speakers")
}

fn default_provider_dir(vars: &HashMap<String, String>) -> Result<PathBuf> {
    default_data_subdir(vars, PROVIDER_DIR, "providers")
}

fn default_vox_link_dir(vars: &HashMap<String, String>) -> Result<PathBuf> {
    default_data_subdir(vars, VOX_LINK_DIR, "vox-links")
}

/// Opens the Vox link store described by `vars`.
///
/// # Errors
///
/// Returns [`Error::Config`] if the configured directory cannot be used.
pub async fn vox_link_store_from_vars(
    vars: &HashMap<String, String>,
) -> Result<Arc<dyn VoxLinkStore>> {
    #[cfg(feature = "postgres")]
    if let Some(url) = vars.get(DATABASE_URL) {
        if !url.trim().is_empty() {
            tracing::info!("storing Vox links in PostgreSQL");
            return Ok(Arc::new(conduit_store::PostgresStore::connect(url).await?));
        }
    }

    match vars.get(VOX_LINK_DIR).map(|value| value.trim()).filter(|value| !value.is_empty()) {
        Some(MEMORY_PIPELINE_DIR) => {
            tracing::warn!(
                "{VOX_LINK_DIR} is {MEMORY_PIPELINE_DIR}: Vox links are kept in memory and \
                 will be lost on restart"
            );
            Ok(Arc::new(MemoryStore::new()))
        }
        Some(directory) => {
            tracing::info!(%directory, "storing Vox links on disk");
            Ok(Arc::new(FileStore::open(directory).await?))
        }
        None => {
            let directory = default_vox_link_dir(vars)?;
            tracing::info!(
                directory = %directory.display(),
                "storing Vox links in the default local data directory"
            );
            Ok(Arc::new(FileStore::open(directory).await?))
        }
    }
}

/// Where a wake definition reads models from when it names no directory.
///
/// The same data directory pipelines and provider definitions live under, which
/// is the volume the compose file mounts — so an operator who dropped models
/// where the documentation said gets a definition that needs no path at all.
///
/// # Errors
///
/// Returns [`Error::Config`] if there is no data directory to derive it from.
pub fn wake_models_dir_from_env() -> Result<PathBuf> {
    default_data_subdir(
        &std::env::vars().collect(),
        WAKE_MODELS_DIR,
        conduit_wake::DEFAULT_MODELS_DIR,
    )
}

/// Where a detector definition reads its model from when it names no path.
///
/// The same data directory the wake models live under, and the same volume the
/// compose file mounts — so an operator who dropped the model where the
/// documentation said gets a definition that needs no path at all.
///
/// # Errors
///
/// Returns [`Error::Config`] if there is no data directory to derive it from.
pub fn vad_model_path_from_env() -> Result<PathBuf> {
    Ok(default_data_subdir(
        &std::env::vars().collect(),
        VAD_MODEL_PATH,
        conduit_vad::DEFAULT_MODELS_DIR,
    )?
    .join(conduit_vad::DEFAULT_MODEL_FILE))
}

fn default_data_subdir(
    vars: &HashMap<String, String>,
    env_name: &str,
    child: &str,
) -> Result<PathBuf> {
    if let Some(directory) =
        vars.get(DATA_DIR).map(|value| value.trim()).filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(directory).join(child));
    }

    if let Some(directory) =
        vars.get("XDG_DATA_HOME").map(|value| value.trim()).filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(directory).join("conduit").join(child));
    }

    if let Some(home) =
        vars.get("HOME").map(|value| value.trim()).filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(home).join(format!(".local/share/conduit/{child}")));
    }

    Err(Error::Config(format!(
        "cannot choose a default storage directory; set {env_name}, {DATA_DIR}, \
         XDG_DATA_HOME, or HOME"
    )))
}

/// What a configuration registered, for logging and for deciding whether any
/// providers exist at all.
#[derive(Debug)]
pub struct Registered {
    /// Human-readable descriptions of what was registered.
    pub descriptions: Vec<String>,
    /// How long a turn may publish nothing before it is abandoned. `None` means
    /// an operator deliberately removed the bound.
    pub turn_idle_timeout: Option<Duration>,
}

/// Written by hand rather than derived, because a derived `Option<Duration>`
/// default is `None` — which here means "no timeout" and would hand an
/// unconfigured deployment the exact hang this bound exists to prevent.
impl Default for Registered {
    fn default() -> Self {
        Self { descriptions: Vec::new(), turn_idle_timeout: Some(DEFAULT_IDLE_TIMEOUT) }
    }
}

impl Registered {
    /// Whether nothing was registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.descriptions.is_empty()
    }
}

/// How long a provider may go silent mid-response, as configured.
///
/// Unset means the provider's own default. `0` means no bound, which is a
/// deliberate choice a deployment can make rather than an accident — a stalled
/// provider then hangs the turn until the client disconnects.
///
/// # Errors
///
/// Returns [`Error::Config`] if the value is not a whole number of seconds. A
/// misspelled duration silently falling back to the default is how a
/// deployment ends up with a timeout it did not ask for.
fn read_timeout(vars: &HashMap<String, String>) -> Result<Option<Duration>> {
    seconds_or(vars, READ_TIMEOUT, OpenAiConfig::default().read_timeout, || {
        tracing::warn!(
            "{READ_TIMEOUT} is 0, so a provider that stops responding will not be given up on"
        );
    })
}

/// How long a turn may publish nothing before it is abandoned, as configured.
///
/// Bounds the turn rather than one provider, so it catches a stage that stalls
/// somewhere the HTTP client's own read timeout cannot see it — including a
/// provider that is not an HTTP client at all.
///
/// # Errors
///
/// Returns [`Error::Config`] if the value is not a whole number of seconds.
fn turn_idle_timeout(vars: &HashMap<String, String>) -> Result<Option<Duration>> {
    seconds_or(vars, TURN_IDLE_TIMEOUT, Some(DEFAULT_IDLE_TIMEOUT), || {
        tracing::warn!(
            "{TURN_IDLE_TIMEOUT} is 0, so a turn whose provider stops answering will hold \
             the conversation until the client disconnects"
        );
    })
}

/// Reads turn-history retention bounds from `vars`.
///
/// Unset or empty values use bounded defaults. `0` removes one bound, and
/// invalid values fail startup rather than leaving retention to guess what was
/// meant.
///
/// # Errors
///
/// Returns [`Error::Config`] if either configured value is not a whole number.
pub fn turn_history_retention_from_vars(
    vars: &HashMap<String, String>,
) -> Result<TurnHistoryRetention> {
    let retention = TurnHistoryRetention {
        max_turns: count_or(
            vars,
            TURN_HISTORY_MAX_TURNS,
            TurnHistoryRetention::default().max_turns,
        )?,
        max_age: seconds_or(
            vars,
            TURN_HISTORY_RETENTION,
            TurnHistoryRetention::default().max_age,
            || {
                tracing::warn!(
                    "{TURN_HISTORY_RETENTION} is 0, so completed turn history has no age bound"
                );
            },
        )?,
    };
    if retention.max_turns.is_none() && retention.max_age.is_none() {
        tracing::warn!(
            "{TURN_HISTORY_MAX_TURNS} and {TURN_HISTORY_RETENTION} are both 0, so completed \
             turn history is unbounded"
        );
    }
    Ok(retention)
}

fn count_or(
    vars: &HashMap<String, String>,
    name: &str,
    fallback: Option<usize>,
) -> Result<Option<usize>> {
    let Some(value) =
        vars.get(name).map(|value| value.trim()).filter(|value| !value.is_empty())
    else {
        return Ok(fallback);
    };

    let count: usize = value
        .parse()
        .map_err(|_| Error::Config(format!("{name} must be a whole number, got `{value}`")))?;
    if count == 0 {
        tracing::warn!("{name} is 0, so completed turn history has no count bound");
        return Ok(None);
    }
    Ok(Some(count))
}

/// Reads a duration in whole seconds from `vars`, or `fallback` if unset.
///
/// `0` means no bound, and calls `on_removed` so a deployment that removed one
/// says so in its logs rather than discovering it during an incident.
///
/// # Errors
///
/// Returns [`Error::Config`] if the value is not a whole number of seconds. A
/// misspelled duration silently falling back to the default is how a deployment
/// ends up with a timeout it did not ask for.
fn seconds_or(
    vars: &HashMap<String, String>,
    name: &str,
    fallback: Option<Duration>,
    on_removed: impl FnOnce(),
) -> Result<Option<Duration>> {
    let Some(value) =
        vars.get(name).map(|value| value.trim()).filter(|value| !value.is_empty())
    else {
        return Ok(fallback);
    };

    let seconds: u64 = value.parse().map_err(|_| {
        Error::Config(format!("{name} must be a whole number of seconds, got `{value}`"))
    })?;
    if seconds == 0 {
        on_removed();
        return Ok(None);
    }
    Ok(Some(Duration::from_secs(seconds)))
}

/// Reads runtime configuration from the process environment.
///
/// # Errors
///
/// Returns [`Error::Config`] if a runtime setting is invalid.
pub fn from_env() -> Result<(Providers, Registered)> {
    from_vars(&std::env::vars().collect())
}

/// Builds runtime configuration from `vars`.
///
/// Taking a map rather than reading the environment directly keeps this
/// testable: process environment is global, and tests that mutate it race.
///
/// # Errors
///
/// Returns [`Error::Config`] if a runtime setting is invalid.
pub fn from_vars(vars: &HashMap<String, String>) -> Result<(Providers, Registered)> {
    let _ = turn_history_retention_from_vars(vars)?;
    let _ = read_timeout(vars)?;
    let registered =
        Registered { turn_idle_timeout: turn_idle_timeout(vars)?, ..Registered::default() };

    Ok((Providers::new(), registered))
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::graph::PipelineGraph;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect()
    }

    fn temp_data_dir(name: &str) -> String {
        let directory = std::env::temp_dir()
            .join(format!("conduit-config-test-{name}-{}", uuid::Uuid::new_v4()));
        directory.display().to_string()
    }

    #[test]
    fn an_empty_environment_registers_nothing() {
        let (providers, registered) = from_vars(&vars(&[])).expect("builds");
        assert!(registered.is_empty());
        assert!(providers.llm().is_empty());
    }

    #[tokio::test]
    async fn pipeline_storage_defaults_to_a_persistent_data_directory() {
        let data_dir = temp_data_dir("default-store");
        let vars = vars(&[("CONDUIT_DATA_DIR", &data_dir)]);
        let before = store_from_vars(&vars).await.expect("opens");
        before.put("kitchen", PipelineGraph::new("kitchen")).await.expect("stores");

        let after = store_from_vars(&vars).await.expect("reopens");

        assert_eq!(after.list().await.expect("lists"), ["kitchen"]);
        tokio::fs::remove_dir_all(&data_dir).await.expect("cleans up");
    }

    #[tokio::test]
    async fn pipeline_storage_uses_memory_only_when_explicitly_requested() {
        let vars = vars(&[(PIPELINE_DIR, ":memory:")]);
        let before = store_from_vars(&vars).await.expect("opens");
        before.put("kitchen", PipelineGraph::new("kitchen")).await.expect("stores");

        let after = store_from_vars(&vars).await.expect("reopens");

        assert!(after.list().await.expect("lists").is_empty());
    }

    #[test]
    fn openai_environment_variables_do_not_register_product_providers() {
        let map = vars(&[
            (BASE_URL, "http://localhost:8000/v1"),
            (API_KEY, "sk-test"),
            (NAME, "local"),
            (STT_MODEL, "whisper-1"),
            (TTS_MODEL, "tts-1"),
        ]);
        let (providers, registered) = from_vars(&map).expect("builds");

        assert!(providers.stt().is_empty());
        assert!(providers.llm().is_empty());
        assert!(providers.tts().is_empty());
        assert!(registered.is_empty());
    }

    #[test]
    fn speech_model_environment_variables_are_ignored_without_provider_definitions() {
        let (providers, registered) =
            from_vars(&vars(&[(STT_MODEL, "whisper-1"), (TTS_MODEL, "tts-1")]))
                .expect("builds");

        assert!(providers.stt().is_empty());
        assert!(providers.llm().is_empty());
        assert!(providers.tts().is_empty());
        assert!(registered.is_empty());
    }

    #[test]
    fn the_read_timeout_defaults_to_the_providers_own() {
        assert_eq!(
            read_timeout(&vars(&[])).expect("builds"),
            OpenAiConfig::default().read_timeout
        );
        assert!(
            read_timeout(&vars(&[])).expect("builds").is_some(),
            "a deployment that configures nothing must still bound a stalled provider"
        );
    }

    #[test]
    fn the_read_timeout_can_be_set_in_seconds() {
        let timeout = read_timeout(&vars(&[(READ_TIMEOUT, "5")])).expect("builds");
        assert_eq!(timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn a_zero_read_timeout_removes_the_bound() {
        assert_eq!(read_timeout(&vars(&[(READ_TIMEOUT, "0")])).expect("builds"), None);
    }

    #[test]
    fn an_unreadable_read_timeout_is_refused_rather_than_ignored() {
        // Falling back to the default would give a deployment a timeout it did
        // not ask for and no indication of why.
        let error = read_timeout(&vars(&[(READ_TIMEOUT, "30s")])).expect_err("not a number");
        assert!(error.to_string().contains(READ_TIMEOUT), "{error}");
    }

    #[test]
    fn a_turn_is_bounded_by_default() {
        // The deployment that configures nothing must not be the one that hangs.
        let (_, registered) = from_vars(&vars(&[])).expect("builds");
        assert_eq!(registered.turn_idle_timeout, Some(DEFAULT_IDLE_TIMEOUT));
    }

    #[test]
    fn a_default_registered_is_still_bounded() {
        // `Default` is what a caller that skipped `from_vars` gets, and an
        // `Option<Duration>` defaulting to `None` would mean no timeout at all.
        assert_eq!(Registered::default().turn_idle_timeout, Some(DEFAULT_IDLE_TIMEOUT));
    }

    #[test]
    fn the_turn_idle_timeout_can_be_set_in_seconds() {
        let (_, registered) = from_vars(&vars(&[(TURN_IDLE_TIMEOUT, "5")])).expect("builds");
        assert_eq!(registered.turn_idle_timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn a_zero_turn_idle_timeout_removes_the_bound() {
        // Expressible on purpose, for a deployment whose own layer above the
        // runtime already bounds a turn.
        let (_, registered) = from_vars(&vars(&[(TURN_IDLE_TIMEOUT, "0")])).expect("builds");
        assert_eq!(registered.turn_idle_timeout, None);
    }

    #[test]
    fn an_unreadable_turn_idle_timeout_is_refused_rather_than_ignored() {
        let error =
            from_vars(&vars(&[(TURN_IDLE_TIMEOUT, "a minute")])).expect_err("not a number");
        assert!(error.to_string().contains(TURN_IDLE_TIMEOUT), "{error}");
    }

    #[test]
    fn obsolete_provider_environment_keeps_read_timeout_validation() {
        let error =
            from_vars(&vars(&[(BASE_URL, "http://localhost:8000/v1"), (READ_TIMEOUT, "30s")]))
                .expect_err("invalid timeout is still rejected");
        assert!(error.to_string().contains(READ_TIMEOUT), "{error}");
    }
}
