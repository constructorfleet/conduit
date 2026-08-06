//! Who is calling, and what they are allowed to do.
//!
//! Callers present a bearer token. Tokens come in two audiences and the split
//! is the point of this module: a **device** token may open conversation
//! sockets and nothing else, while a **management** token may read events and
//! manage pipelines. A token extracted from a satellite's firmware therefore
//! cannot read the household's transcripts.
//!
//! Tokens are declared in a JSON file, read once at startup. Parsing is
//! separated from loading — [`Tokens::parse`] takes a string and
//! [`Tokens::load`] takes a path and additionally refuses a file other users
//! can read — because that keeps every policy question testable without
//! touching a filesystem.

use std::collections::HashMap;
use std::path::Path;

use conduit_core::id::DeviceId;
use conduit_core::{Error, Result};
use serde::Deserialize;

/// The file listing every token this server accepts.
///
/// Unset means no token file, which the server refuses to start without —
/// see [`crate::config::tokens_from_env`].
pub const TOKENS_FILE: &str = "CONDUIT_TOKENS";

/// Set to `1` to serve the API with no authentication at all.
///
/// Exists so a development server is one variable away rather than a reason to
/// make "open" the default. Never appropriate on a network anyone else can
/// reach.
pub const ALLOW_ANONYMOUS: &str = "CONDUIT_ALLOW_ANONYMOUS";

/// How short a token may be before it is refused.
///
/// Nothing rate-limits authentication attempts yet, so entropy is the only
/// defence against guessing. A short token in a config file is a mistake worth
/// catching at startup rather than after someone finds it.
const MINIMUM_TOKEN_LENGTH: usize = 32;

/// What one authenticated caller is allowed to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// A satellite, permitted to hold conversations.
    Device(Device),
    /// An operator or UI, permitted to read events and manage pipelines.
    Management(Management),
}

impl Identity {
    /// Whether this caller may manage pipelines and read the event stream.
    #[must_use]
    pub const fn is_management(&self) -> bool {
        matches!(self, Self::Management(_))
    }

    /// The name to log this caller under. Never the token.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Device(device) => &device.name,
            Self::Management(management) => &management.name,
        }
    }
}

/// A satellite that may hold conversations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The name the operator gave this device, used in logs.
    pub name: String,
    /// The identifier its events are tagged with, so `/v1/events?device=`
    /// can select them.
    pub id: DeviceId,
    /// Pipelines this device may open. `None` means any of them.
    pipelines: Option<Vec<String>>,
}

impl Device {
    /// Whether this device may converse with `pipeline`.
    #[must_use]
    pub fn may_use(&self, pipeline: &str) -> bool {
        self.pipelines
            .as_ref()
            .is_none_or(|allowed| allowed.iter().any(|name| name == pipeline))
    }
}

/// An operator or UI that may manage pipelines and read events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Management {
    /// The name the operator gave this credential, used in logs.
    pub name: String,
}

/// Every token this server accepts, indexed for constant-time lookup.
///
/// Cheap to clone: the map is shared.
#[derive(Debug, Clone, Default)]
pub struct Tokens {
    by_token: std::sync::Arc<HashMap<String, Identity>>,
}

impl Tokens {
    /// Reads a token file from `text`.
    ///
    /// Takes a string rather than a path so that every rule below is testable
    /// without a temporary file. The permission check that needs a real file
    /// lives in [`Tokens::load`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if `text` is not the expected JSON, if any
    /// entry is missing a name or token, if a token is too short to be safe, if
    /// the same token appears twice, or if the file declares no tokens at all.
    pub fn parse(text: &str) -> Result<Self> {
        let file: TokenFile = serde_json::from_str(text)
            .map_err(|error| Error::Config(format!("cannot read the token file: {error}")))?;

        let mut by_token = HashMap::new();
        for entry in &file.devices {
            let identity = Identity::Device(Device {
                name: named(&entry.device, "a device entry")?,
                id: DeviceId::new(),
                pipelines: entry.pipelines.clone(),
            });
            insert(&mut by_token, &entry.token, identity)?;
        }
        for entry in &file.management {
            let identity = Identity::Management(Management {
                name: named(&entry.name, "a management entry")?,
            });
            insert(&mut by_token, &entry.token, identity)?;
        }

        if by_token.is_empty() {
            // A file that authenticates nobody locks the operator out of their
            // own server, which is not what anyone writing one meant.
            return Err(Error::Config(
                "the token file declares no tokens; add a `devices` or `management` entry"
                    .to_owned(),
            ));
        }

        Ok(Self { by_token: std::sync::Arc::new(by_token) })
    }

    /// Reads the token file at `path`, refusing one other users can read.
    ///
    /// Tokens are stored in plaintext, so the file permissions *are* the
    /// protection. Checking them at startup turns a silent exposure into a
    /// failure someone fixes before it matters.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if the file cannot be read, if it is group- or
    /// world-readable, or if [`Tokens::parse`] rejects its contents.
    pub async fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = tokio::fs::read_to_string(path).await.map_err(|error| {
            Error::Config(format!("cannot read the token file `{}`: {error}", path.display()))
        })?;

        let metadata = tokio::fs::metadata(path).await.map_err(|error| {
            Error::Config(format!(
                "cannot inspect the token file `{}`: {error}",
                path.display()
            ))
        })?;
        check_permissions(path, &metadata)?;

        Self::parse(&text)
    }

    /// Resolves a presented token, or `None` if nobody holds it.
    #[must_use]
    pub fn identify(&self, token: &str) -> Option<&Identity> {
        self.by_token.get(token)
    }

    /// The declared device called `name`.
    ///
    /// A scan rather than a second index: a household has a handful of
    /// satellites, and a name lookup happens when an operator renders firmware,
    /// not on every request. A second map would be a second thing to keep
    /// consistent for no measured gain.
    #[must_use]
    pub fn device_named(&self, name: &str) -> Option<Device> {
        self.by_token.values().find_map(|identity| match identity {
            Identity::Device(device) if device.name == name => Some(device.clone()),
            _ => None,
        })
    }

    /// How many tokens were loaded. For a startup log, never the tokens.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_token.len()
    }

    /// Whether no tokens were loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_token.is_empty()
    }
}

/// Whether this server authenticates callers, and by what.
///
/// A variant rather than an `Option<Tokens>` because "no authentication" is a
/// deliberate configuration with an identity of its own, not the absence of
/// one: anonymous conversations still need a device to file their events under.
#[derive(Debug, Clone)]
pub enum Access {
    /// Every caller must present a token from this set.
    Tokens(Tokens),
    /// Nobody is asked for anything, and every caller shares one identity.
    ///
    /// Only reachable by setting [`ALLOW_ANONYMOUS`], because a server that
    /// silently defaulted to this is the vulnerability this module exists to
    /// close.
    Anonymous(Device),
}

impl Access {
    /// An open server, with one shared identity for every caller.
    #[must_use]
    pub fn anonymous() -> Self {
        Self::Anonymous(Device {
            name: "anonymous".to_owned(),
            id: DeviceId::new(),
            pipelines: None,
        })
    }

    /// Resolves the credential in `headers`, or `None` on an open server.
    ///
    /// `None` means nobody was asked, so the caller may do anything: an open
    /// server that refused management routes would be open and useless rather
    /// than open.
    ///
    /// # Errors
    ///
    /// Returns 401 if no usable bearer token was presented.
    fn identify(
        &self,
        headers: &axum::http::HeaderMap,
    ) -> Result<Option<Identity>, crate::ApiError> {
        let tokens = match self {
            Self::Anonymous(_) => return Ok(None),
            Self::Tokens(tokens) => tokens,
        };

        let presented = bearer(headers).ok_or_else(|| {
            tracing::warn!("rejected a request with no usable Authorization header");
            crate::ApiError::unauthorized()
        })?;

        tokens.identify(presented).cloned().map(Some).ok_or_else(|| {
            // No token in the log, and no hint to the caller that the token was
            // the part that was wrong rather than the header.
            tracing::warn!("rejected a request presenting an unrecognised token");
            crate::ApiError::unauthorized()
        })
    }

    /// The declared device called `name`, for a caller that knows a name rather
    /// than a token.
    ///
    /// Firmware rendering needs this: a rendered fragment is keyed on the name
    /// from the token file, because that is the one device identifier that
    /// survives a restart — [`DeviceId`] is minted per process.
    ///
    /// On an open server there are no declared names, so the name asked for is
    /// taken as a label and a device is returned for it. That is safe here
    /// precisely because rendering emits `!secret` references rather than
    /// credentials: the fragment is identical whatever it is labelled, so a name
    /// that names nobody reveals nothing. A server with a token file answers
    /// only for names it declares.
    #[must_use]
    pub fn device_named(&self, name: &str) -> Option<Device> {
        match self {
            Self::Anonymous(device) => Some(Device { name: name.to_owned(), ..device.clone() }),
            Self::Tokens(tokens) => tokens.device_named(name),
        }
    }

    /// The identity an unauthenticated caller conversing on an open server gets.
    fn anonymous_device(&self) -> Device {
        match self {
            Self::Anonymous(device) => device.clone(),
            // Unreachable: `identify` only returns `None` for `Anonymous`.
            Self::Tokens(_) => {
                Device { name: "anonymous".to_owned(), id: DeviceId::new(), pipelines: None }
            }
        }
    }
}

/// Extracts the token from an `Authorization: Bearer …` header.
///
/// Header only, never a query parameter: the firmware logs whole URLs and the
/// trace layer records request URIs into spans that may be exported, so a token
/// in a URL ends up in device logs and in observability infrastructure.
fn bearer(headers: &axum::http::HeaderMap) -> Option<&str> {
    let value = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then_some(token)
}

/// A caller permitted to manage pipelines and read the event stream.
///
/// Extracting this is what enforces the audience split: a handler asking for it
/// cannot be reached with a device token, whatever the route table says.
#[derive(Debug, Clone)]
pub struct ManagementCaller(pub Management);

/// A caller permitted to hold conversations.
#[derive(Debug, Clone)]
pub struct DeviceCaller(pub Device);

impl axum::extract::FromRequestParts<crate::AppState> for ManagementCaller {
    type Rejection = crate::ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &crate::AppState,
    ) -> Result<Self, Self::Rejection> {
        match state.access().identify(&parts.headers)? {
            // An open server asks for nothing, so there is nobody to refuse.
            None => Ok(Self(Management { name: "anonymous".to_owned() })),
            Some(Identity::Management(management)) => Ok(Self(management)),
            Some(Identity::Device(device)) => {
                // The whole point of two audiences: a token from a satellite
                // must not read the household's transcripts.
                tracing::warn!(
                    device = %device.name,
                    "rejected a device token presented to a management route"
                );
                Err(crate::ApiError::forbidden(
                    "this endpoint needs a management token; a device token cannot manage \
                     pipelines or read events",
                ))
            }
        }
    }
}

impl axum::extract::FromRequestParts<crate::AppState> for DeviceCaller {
    type Rejection = crate::ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &crate::AppState,
    ) -> Result<Self, Self::Rejection> {
        let access = state.access();
        match access.identify(&parts.headers)? {
            None => Ok(Self(access.anonymous_device())),
            Some(Identity::Device(device)) => Ok(Self(device)),
            // Deliberately allowed: an operator holding a management token is
            // already trusted with more than a conversation, and refusing them
            // would make the API impossible to try out by hand.
            Some(Identity::Management(management)) => {
                Ok(Self(Device { name: management.name, id: DeviceId::new(), pipelines: None }))
            }
        }
    }
}

/// Refuses a token file that anyone but its owner can read.
#[cfg(unix)]
fn check_permissions(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let mode = metadata.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(Error::Config(format!(
            "the token file `{}` is readable by other users (mode {mode:04o}); \
             run `chmod 600 {}`",
            path.display(),
            path.display()
        )));
    }
    Ok(())
}

/// Non-Unix platforms have no mode bits to check.
///
/// Says so rather than pretending the file was verified, because an operator
/// who read the documentation is entitled to know the check did not run.
#[cfg(not(unix))]
fn check_permissions(path: &Path, _metadata: &std::fs::Metadata) -> Result<()> {
    tracing::warn!(
        path = %path.display(),
        "cannot check token file permissions on this platform; protect it yourself"
    );
    Ok(())
}

/// Adds one entry, refusing a token that already means someone else.
fn insert(
    by_token: &mut HashMap<String, Identity>,
    token: &str,
    identity: Identity,
) -> Result<()> {
    if token.len() < MINIMUM_TOKEN_LENGTH {
        // Naming the entry rather than the token keeps the secret out of logs
        // and crash output.
        return Err(Error::Config(format!(
            "the token for `{}` is {} characters; use at least {MINIMUM_TOKEN_LENGTH} \
             random ones, because nothing rate-limits guesses yet",
            identity.name(),
            token.len()
        )));
    }

    if let Some(existing) = by_token.get(token) {
        // A token meaning two things has no correct interpretation, and
        // guessing which was meant is how a device ends up with an operator's
        // permissions.
        return Err(Error::Config(format!(
            "the same token is used by `{}` and `{}`; every token must be distinct",
            existing.name(),
            identity.name()
        )));
    }

    by_token.insert(token.to_owned(), identity);
    Ok(())
}

/// Requires a non-empty name, since a nameless entry cannot be logged usefully.
fn named(name: &str, what: &str) -> Result<String> {
    if name.trim().is_empty() {
        return Err(Error::Config(format!(
            "{what} has no name; every entry needs one for logs"
        )));
    }
    Ok(name.to_owned())
}

/// The on-disk shape. Both lists are optional so a file may declare only one
/// audience.
#[derive(Debug, Deserialize)]
struct TokenFile {
    #[serde(default)]
    devices: Vec<DeviceEntry>,
    #[serde(default)]
    management: Vec<ManagementEntry>,
}

#[derive(Debug, Deserialize)]
struct DeviceEntry {
    token: String,
    device: String,
    /// Omitted means any pipeline, which is the common case.
    #[serde(default)]
    pipelines: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ManagementEntry {
    token: String,
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Long enough to pass the entropy floor; the value itself is irrelevant.
    fn token(label: &str) -> String {
        format!("{label}-{}", "0".repeat(MINIMUM_TOKEN_LENGTH))
    }

    fn file(body: &str) -> Result<Tokens> {
        Tokens::parse(body)
    }

    #[test]
    fn a_device_token_resolves_to_its_device() {
        let tokens = file(&format!(
            r#"{{"devices":[{{"token":"{}","device":"kitchen"}}]}}"#,
            token("d")
        ))
        .expect("parses");

        let identity = tokens.identify(&token("d")).expect("the device is known");
        assert_eq!(identity.name(), "kitchen");
        assert!(!identity.is_management(), "a device must not be management");
    }

    #[test]
    fn a_management_token_resolves_to_management() {
        let tokens =
            file(&format!(r#"{{"management":[{{"token":"{}","name":"ui"}}]}}"#, token("m")))
                .expect("parses");

        let identity = tokens.identify(&token("m")).expect("the credential is known");
        assert_eq!(identity.name(), "ui");
        assert!(identity.is_management());
    }

    #[test]
    fn an_unknown_token_resolves_to_nobody() {
        let tokens =
            file(&format!(r#"{{"management":[{{"token":"{}","name":"ui"}}]}}"#, token("m")))
                .expect("parses");
        assert!(tokens.identify(&token("other")).is_none());
    }

    #[test]
    fn a_device_without_a_pipeline_list_may_use_any() {
        // The common case must need no configuration.
        let tokens = file(&format!(
            r#"{{"devices":[{{"token":"{}","device":"kitchen"}}]}}"#,
            token("d")
        ))
        .expect("parses");

        let Some(Identity::Device(device)) = tokens.identify(&token("d")) else {
            panic!("expected a device");
        };
        assert!(device.may_use("anything"));
    }

    #[test]
    fn a_restricted_device_may_use_only_its_pipelines() {
        let tokens = file(&format!(
            r#"{{"devices":[{{"token":"{}","device":"guest","pipelines":["guest-room"]}}]}}"#,
            token("d")
        ))
        .expect("parses");

        let Some(Identity::Device(device)) = tokens.identify(&token("d")) else {
            panic!("expected a device");
        };
        assert!(device.may_use("guest-room"));
        assert!(!device.may_use("front-door"), "a restriction that admits everything is none");
    }

    #[test]
    fn an_empty_pipeline_list_permits_nothing() {
        // Distinct from omitting the key: an operator who wrote `[]` asked for
        // a device that can converse with nothing, and reading that as "all"
        // would be the opposite of what they wrote.
        let tokens = file(&format!(
            r#"{{"devices":[{{"token":"{}","device":"shelf","pipelines":[]}}]}}"#,
            token("d")
        ))
        .expect("parses");

        let Some(Identity::Device(device)) = tokens.identify(&token("d")) else {
            panic!("expected a device");
        };
        assert!(!device.may_use("kitchen"));
    }

    #[test]
    fn two_entries_sharing_a_token_is_an_error() {
        let shared = token("shared");
        let error = file(&format!(
            r#"{{"devices":[{{"token":"{shared}","device":"kitchen"}}],
                 "management":[{{"token":"{shared}","name":"ui"}}]}}"#
        ))
        .expect_err("a token cannot mean two identities");

        let message = error.to_string();
        assert!(message.contains("kitchen"), "{message}");
        assert!(message.contains("ui"), "{message}");
        assert!(!message.contains(&shared), "the token must not be in the error: {message}");
    }

    #[test]
    fn a_short_token_is_refused() {
        let error = file(r#"{"devices":[{"token":"hunter2","device":"kitchen"}]}"#)
            .expect_err("a guessable token is a configuration error");
        assert!(error.to_string().contains("kitchen"), "{error}");
    }

    #[test]
    fn an_error_never_quotes_the_token() {
        // Startup errors reach logs and terminals, so they must not leak what
        // they are complaining about.
        let secret = "s3cret";
        let error =
            file(&format!(r#"{{"devices":[{{"token":"{secret}","device":"kitchen"}}]}}"#))
                .expect_err("too short");
        assert!(!error.to_string().contains(secret), "{error}");
    }

    #[test]
    fn a_file_declaring_no_tokens_is_an_error() {
        // Otherwise a typo in the key names produces a server nobody can use,
        // reported as 401s rather than as the misconfiguration it is.
        let error = file("{}").expect_err("a file that authenticates nobody is a mistake");
        assert!(error.to_string().contains("no tokens"), "{error}");
    }

    #[test]
    fn a_nameless_entry_is_an_error() {
        let error =
            file(&format!(r#"{{"devices":[{{"token":"{}","device":""}}]}}"#, token("d")))
                .expect_err("an unnamed device cannot be logged");
        assert!(error.to_string().contains("no name"), "{error}");
    }

    #[test]
    fn malformed_json_is_an_error() {
        let error = file("not json at all").expect_err("unreadable");
        assert!(error.to_string().contains("cannot read the token file"), "{error}");
    }

    #[test]
    fn every_device_gets_its_own_identifier() {
        // Two satellites sharing a device id would make the event stream's
        // device filter select both.
        let tokens = file(&format!(
            r#"{{"devices":[{{"token":"{}","device":"kitchen"}},
                            {{"token":"{}","device":"office"}}]}}"#,
            token("a"),
            token("b")
        ))
        .expect("parses");

        let Some(Identity::Device(first)) = tokens.identify(&token("a")) else {
            panic!("expected a device");
        };
        let Some(Identity::Device(second)) = tokens.identify(&token("b")) else {
            panic!("expected a device");
        };
        assert_ne!(first.id, second.id);
    }
}
