//! Handing a rendered fragment to an ESPHome dashboard Conduit does not own.
//!
//! [ADR-0019][adr] decided this shape: Conduit uploads the fragment to an
//! ESPHome instance the operator already runs and links out to that instance's
//! own install and OTA affordances. Conduit never compiles, never stores a
//! compiled image, and never serves a binary — because an ESPHome build
//! substitutes `secrets.yaml` into generated C++, so a `.bin` carries a working
//! device token, and serving one would invert
//! [ADR-0015](../../../docs/adr/0015-render-the-conduit-part-of-the-firmware.md)'s
//! entire secrets posture.
//!
//! **Only the fragment is ever uploaded.** The board file is put in the ESPHome
//! config directory once, by hand, and `!include`s the fragment — the same
//! arrangement the checked-in boards already use. So reconfiguring a device is a
//! one-file write, Conduit still never sees a pin number, and a board Conduit
//! has never heard of still works.
//!
//! **The base URL is an SSRF surface**: an operator-supplied address this
//! server dials. It is parsed rather than concatenated, its scheme is
//! restricted to `http` and `https`, and a failure to reach it is reported as a
//! failure rather than retried in a way that scans. The credential for that
//! instance is a secret Conduit holds, so it is never logged and never returned
//! in a response — the rule `auth.rs` already follows for tokens.
//!
//! **A broken upload degrades to the download.** ESPHome's dashboard endpoints
//! are not versioned for third parties and can change between releases, so this
//! coupling is real. The console's download affordance is the fallback rather
//! than a stopgap, which is why an error here has to say what went wrong
//! specifically enough for an operator to apply the fragment by hand.
//!
//! [adr]: ../../../docs/adr/0019-flashing-through-an-esphome-instance-conduit-does-not-own.md

use std::time::Duration;

use crate::ApiError;

/// How long to wait for the dashboard's TCP and TLS handshake.
///
/// Short, because the common failure is a base URL pointing at nothing on the
/// LAN, and an operator waiting on a spinner learns less than one reading "no
/// route to host".
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the dashboard may go silent before an upload is abandoned.
///
/// Writing one small file is not a long operation. This bounds a dashboard that
/// accepted the connection and then stopped talking, which is otherwise
/// indistinguishable from a slow one forever.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The dashboard path a configuration file is written through.
///
/// ESPHome's dashboard, not a documented third-party API — see the module note
/// on why a failure here has to degrade rather than dead-end.
const EDIT_PATH: &str = "edit";

/// Where Conduit hands a fragment off, and how it authenticates there.
///
/// Absent unless an operator configured one. Flashing is an opt-in relationship
/// with a service Conduit does not own, so a deployment that configured nothing
/// has no ESPHome integration rather than a default one pointing at localhost.
#[derive(Clone)]
pub struct EsphomeDashboard {
    /// Parsed base URL, scheme already checked.
    base_url: reqwest::Url,
    /// Whatever the instance requires, as an `Authorization` header value.
    ///
    /// A secret Conduit holds. Never logged, never rendered, never returned.
    credential: Option<String>,
}

/// Deliberately hand-written: the derived one would print `credential`.
///
/// This type is reachable from `AppState`, which is `Debug`, so a derive here is
/// one `tracing::debug!` away from a dashboard password in a log file.
impl std::fmt::Debug for EsphomeDashboard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EsphomeDashboard")
            .field("base_url", &self.base_url.as_str())
            .field("credential", &self.credential.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Why a configured dashboard could not be accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidDashboard {
    /// What was wrong, in terms an operator can act on.
    pub detail: String,
}

impl std::fmt::Display for InvalidDashboard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for InvalidDashboard {}

impl EsphomeDashboard {
    /// A dashboard at `base_url`, authenticating with `credential`.
    ///
    /// The URL is parsed here rather than at upload time so a typo is a startup
    /// failure rather than a surprise the first time an operator clicks flash.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidDashboard`] if the URL does not parse, if its scheme is
    /// anything but `http` or `https`, or if it names no host. The scheme check
    /// is the SSRF-relevant one: without it an operator-supplied `file://` or
    /// `unix://` turns a flash button into a local-filesystem read.
    pub fn new(base_url: &str, credential: Option<String>) -> Result<Self, InvalidDashboard> {
        let trimmed = base_url.trim();
        let mut parsed = reqwest::Url::parse(trimmed).map_err(|error| InvalidDashboard {
            detail: format!("`{trimmed}` is not a URL: {error}"),
        })?;

        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(InvalidDashboard {
                detail: format!(
                    "an ESPHome dashboard URL must be http or https, not `{}`",
                    parsed.scheme()
                ),
            });
        }

        if parsed.host_str().is_none_or(str::is_empty) {
            return Err(InvalidDashboard { detail: format!("`{trimmed}` names no host") });
        }

        // A base URL joins correctly only with a trailing slash: `Url::join`
        // against `http://host/esphome` replaces the last segment, which would
        // silently post to `http://host/edit` instead of under the prefix an
        // operator gave. Fixing it here rather than documenting it.
        if !parsed.path().ends_with('/') {
            let path = format!("{}/", parsed.path());
            parsed.set_path(&path);
        }

        Ok(Self {
            base_url: parsed,
            credential: credential
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
        })
    }

    /// The dashboard an operator would open, for the console to link to.
    ///
    /// Carries no credential: it is a link a browser follows, and a query
    /// parameter would put the secret in history and in every referrer.
    #[must_use]
    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    /// Writes `fragment` to the dashboard as `file_name`.
    ///
    /// Uploads the fragment and nothing else — no board file, no compiled
    /// artifact. What happens next is a build the operator triggers on that
    /// dashboard, which is ADR-0019's whole point: the toolchain that already
    /// holds these secrets is the one that uses them.
    ///
    /// # Errors
    ///
    /// Returns 502 when the dashboard cannot be reached or refuses the write,
    /// naming the dashboard and what it said, because the operator's next move
    /// is to apply the fragment by hand.
    pub async fn upload(&self, file_name: &str, fragment: &str) -> Result<(), ApiError> {
        let target = self.base_url.join(EDIT_PATH).map_err(|error| {
            ApiError::unavailable(format!("cannot build an upload URL: {error}"))
        })?;

        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(|error| {
                ApiError::unavailable(format!("cannot build an HTTP client: {error}"))
            })?;

        let mut request = client
            .post(target)
            .query(&[("configuration", file_name)])
            .header(reqwest::header::CONTENT_TYPE, "text/yaml")
            .body(fragment.to_owned());
        if let Some(credential) = &self.credential {
            request = request.header(reqwest::header::AUTHORIZATION, credential);
        }

        // No retry. An unreachable operator-supplied address retried in a loop
        // is a scan, and the fallback is right there: the console still offers
        // the fragment for download.
        let response = request.send().await.map_err(|error| self.unreachable(&error))?;

        let status = response.status();
        if status.is_success() {
            return Ok(());
        }

        // The dashboard's own words, which is the difference between "502" and
        // "502: the dashboard rejected it: no such configuration". Truncated
        // because a rejection can come back as a whole HTML error page.
        let said = response.text().await.unwrap_or_default();
        let said: String = said.trim().chars().take(200).collect();
        Err(ApiError::bad_gateway(if said.is_empty() {
            format!("the ESPHome dashboard at {} answered {status}", self.base_url)
        } else {
            format!("the ESPHome dashboard at {} answered {status}: {said}", self.base_url)
        }))
    }

    /// The error for a dashboard that could not be reached at all.
    ///
    /// Names the base URL and not the credential: `reqwest`'s own message
    /// includes the URL it dialed, which is why the message is built rather than
    /// forwarded — a URL with a credential in it would otherwise be echoed.
    fn unreachable(&self, error: &reqwest::Error) -> ApiError {
        let cause = if error.is_timeout() {
            "it did not answer in time".to_owned()
        } else if error.is_connect() {
            "the connection was refused".to_owned()
        } else {
            "the request could not be sent".to_owned()
        };
        ApiError::bad_gateway(format!(
            "cannot reach the ESPHome dashboard at {}: {cause}. The fragment is still \
             available for download — apply it to that instance by hand.",
            self.base_url
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scheme_that_is_not_http_is_refused_before_anything_is_dialed() {
        // The SSRF-relevant check. Without it an operator-supplied `file://`
        // turns a flash button into a local-filesystem read.
        for base in ["file:///etc/passwd", "unix:///var/run/docker.sock", "ftp://host/"] {
            let error = EsphomeDashboard::new(base, None)
                .expect_err("only http and https are dialable");
            assert!(error.detail.contains("http or https"), "for `{base}`: {error}");
        }
    }

    #[test]
    fn a_url_that_does_not_parse_is_refused_by_saying_so() {
        let error = EsphomeDashboard::new("not a url at all", None)
            .expect_err("a hostname is not a URL");

        assert!(error.detail.contains("is not a URL"), "{error}");
    }

    #[test]
    fn a_scheme_with_no_host_is_refused() {
        let error = EsphomeDashboard::new("http://", None).expect_err("nothing to dial");

        assert!(!error.detail.is_empty());
    }

    #[test]
    fn a_base_url_with_a_path_prefix_keeps_it() {
        // `Url::join` replaces the last path segment, so a dashboard behind a
        // reverse proxy at `/esphome` would otherwise receive uploads at `/edit`
        // — a 404 that looks like a broken integration rather than a bug here.
        let dashboard = EsphomeDashboard::new("http://homelab:6052/esphome", None)
            .expect("a prefixed dashboard");

        assert_eq!(dashboard.base_url(), "http://homelab:6052/esphome/");
    }

    #[test]
    fn the_credential_never_appears_in_the_debug_output() {
        // This type is reachable from `AppState`, which is `Debug`. A derive
        // here would put a dashboard password one `tracing::debug!` from a log.
        let dashboard =
            EsphomeDashboard::new("http://homelab:6052", Some("Bearer hunter2".to_owned()))
                .expect("a dashboard");

        let printed = format!("{dashboard:?}");
        assert!(!printed.contains("hunter2"), "the credential leaked: {printed}");
        assert!(printed.contains("redacted"), "and it says so: {printed}");
        assert!(printed.contains("homelab"), "the URL is still useful: {printed}");
    }

    #[test]
    fn a_blank_credential_is_no_credential() {
        // An operator who set the variable to an empty string configured no
        // credential, rather than a credential that is the empty string — which
        // would send `Authorization: ` and be refused confusingly.
        let dashboard = EsphomeDashboard::new("http://homelab:6052", Some("  ".to_owned()))
            .expect("a dashboard");

        assert!(dashboard.credential.is_none());
    }

    #[tokio::test]
    async fn an_unreachable_dashboard_says_so_and_points_at_the_download() {
        // ADR-0019: a broken upload degrades to "here is your fragment, apply it
        // yourself" rather than to a dead button, so the error has to say that.
        // Port 1 on localhost refuses rather than hanging.
        let dashboard = EsphomeDashboard::new("http://127.0.0.1:1", None).expect("a dashboard");

        let error = dashboard
            .upload("conduit-kitchen.conduit.yaml", "conduit_voice:\n")
            .await
            .expect_err("nothing is listening");

        let message = format!("{error:?}");
        assert!(message.contains("127.0.0.1:1"), "names the dashboard: {message}");
        assert!(message.contains("download"), "points at the fallback: {message}");
    }
}
