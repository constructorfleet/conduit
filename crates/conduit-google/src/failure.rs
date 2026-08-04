//! Why a request to a Google Cloud speech API failed, in enough detail to
//! decide what to do about it.
//!
//! A caller facing a failure has three sensible responses: retry the same
//! provider, fail over to another, or give up. Telling them apart needs more
//! than a message, so every failure this crate produces carries a [`Failure`]
//! as the source of its [`conduit_core::Error::Provider`]. Recover it with
//! [`Failure::of`]:
//!
//! ```no_run
//! # use conduit_google::Failure;
//! # fn example(error: &conduit_core::Error) {
//! match Failure::of(error) {
//!     Some(failure) if failure.is_retryable() => { /* wait and try again */ }
//!     Some(_) => { /* the request itself is wrong; retrying cannot help */ }
//!     None => { /* not a failure this crate classified */ }
//! }
//! # }
//! ```

use std::time::Duration;

/// The shape of a failed request.
///
/// Deliberately coarse: a caller wants to know *what kind of thing went
/// wrong*, and the status code is there for anyone who needs more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureKind {
    /// The server accepted the request and did not answer in time, or stopped
    /// answering partway through.
    Timeout,
    /// The request never completed at the transport layer — connection
    /// refused, reset, DNS failure.
    Transport,
    /// The server answered with a status outside 2xx.
    Status,
    /// The server answered, but not in a shape this provider can read.
    Malformed,
    /// The request could not be assembled, so nothing was ever sent.
    Request,
}

/// A classified request failure.
///
/// Stored as the source of [`conduit_core::Error::Provider`], so the error's
/// message stays human-readable while the classification stays machine-usable.
#[derive(Debug, Clone)]
pub struct Failure {
    kind: FailureKind,
    status: Option<u16>,
    retry_after: Option<Duration>,
    detail: String,
}

impl Failure {
    /// How much of a response body is worth quoting in a message.
    ///
    /// Enough for a sentence of explanation, not enough for an HTML error page.
    const DETAIL_LIMIT: usize = 512;

    /// Recovers the classification from a [`conduit_core::Error`].
    ///
    /// Returns `None` for anything this crate did not produce — an error from
    /// another provider, or a non-provider error.
    #[must_use]
    pub fn of(error: &conduit_core::Error) -> Option<&Self> {
        match error {
            conduit_core::Error::Provider { source, .. } => source.downcast_ref::<Self>(),
            _ => None,
        }
    }

    /// Classifies a status the server returned.
    ///
    /// `retry_after` is the raw header value, if the server sent one.
    #[must_use]
    pub fn status_failure(status: u16, retry_after: Option<&str>, body: &str) -> Self {
        Self {
            kind: FailureKind::Status,
            status: Some(status),
            retry_after: retry_after.and_then(parse_retry_after),
            detail: google_message(body).chars().take(Self::DETAIL_LIMIT).collect(),
        }
    }

    /// Classifies a transport-level failure.
    ///
    /// A timeout is separated out because it is the one transport failure that
    /// says something about the *server* rather than the network: the request
    /// was accepted, and no answer came.
    #[must_use]
    pub fn transport(error: &reqwest::Error) -> Self {
        // Order matters: reqwest reports a timeout that happened while reading
        // a body as both a timeout and a body failure, and the timeout is the
        // more useful of the two.
        let kind = if error.is_timeout() {
            FailureKind::Timeout
        } else if error.is_builder() {
            // The request was never sent, because this crate could not build
            // it. That is a bug or a misconfiguration here, not upstream.
            FailureKind::Request
        } else if error.is_decode() {
            // The bytes arrived and did not mean what they claimed to. Asking
            // again gets the same bytes.
            FailureKind::Malformed
        } else {
            FailureKind::Transport
        };
        Self { kind, status: None, retry_after: None, detail: error.to_string() }
    }

    /// Classifies a response this provider could not interpret.
    #[must_use]
    pub fn malformed(detail: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Malformed,
            status: None,
            retry_after: None,
            detail: detail.into(),
        }
    }

    /// What kind of failure this was.
    #[must_use]
    pub const fn kind(&self) -> FailureKind {
        self.kind
    }

    /// The HTTP status, when the server sent one.
    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        self.status
    }

    /// Whether the server accepted the request and then failed to answer.
    #[must_use]
    pub const fn is_timeout(&self) -> bool {
        matches!(self.kind, FailureKind::Timeout)
    }

    /// How long the server asked the caller to wait, when it said so as a
    /// number of seconds.
    ///
    /// An HTTP-date `Retry-After` is reported as `None` rather than guessed
    /// at: honouring it needs a clock the caller has and this type does not.
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    /// Whether sending the same request again could plausibly succeed.
    ///
    /// Transport failures and timeouts are transient by nature. Among
    /// statuses, `408`, `425`, `429`, and the 5xx family are the server saying
    /// "not now"; `501` is it saying "not ever", and the remaining 4xx are the
    /// request itself being wrong.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self.kind {
            FailureKind::Timeout | FailureKind::Transport => true,
            FailureKind::Status => match self.status {
                Some(status) => is_transient_status(status),
                None => false,
            },
            FailureKind::Malformed | FailureKind::Request => false,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(formatter, "HTTP {status}: {}", self.detail),
            None => formatter.write_str(&self.detail),
        }
    }
}

impl std::error::Error for Failure {}

/// Whether `status` means "not now" rather than "not like that".
const fn is_transient_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429) || (status >= 500 && status != 501)
}

/// The sentence out of a Google error body, or the body itself.
///
/// Every Google API error is `{"error": {"code", "message", "status"}}`, and the
/// `message` is the whole of what an operator needs — "Invalid recognition
/// config: bad sample rate" rather than eight lines of JSON wrapping it. A body
/// that is not that shape is quoted as-is, because an unexpected body is
/// evidence and discarding it would leave nothing to diagnose.
fn google_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .as_ref()
        .and_then(|json| json.get("error")?.get("message")?.as_str())
        .map_or_else(|| body.to_owned(), str::to_owned)
}

/// Reads a `Retry-After` expressed as a whole number of seconds.
fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_limit_is_transient_and_a_bad_request_is_not() {
        assert!(is_transient_status(429));
        assert!(is_transient_status(503));
        assert!(!is_transient_status(400));
        assert!(!is_transient_status(404));
    }

    #[test]
    fn not_implemented_is_permanent_despite_being_a_5xx() {
        // A server that does not have the endpoint will not grow one between
        // two attempts.
        assert!(!is_transient_status(501));
    }

    #[test]
    fn retry_after_reads_seconds_and_ignores_dates() {
        assert_eq!(parse_retry_after("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after(" 30 "), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after(""), None);
    }

    #[test]
    fn a_long_body_is_truncated_rather_than_quoted_whole() {
        let failure = Failure::status_failure(500, None, &"x".repeat(4096));
        assert_eq!(failure.to_string().len(), "HTTP 500: ".len() + Failure::DETAIL_LIMIT);
    }

    #[test]
    fn a_google_error_body_is_reduced_to_its_message() {
        let body = serde_json::json!({
            "error": {
                "code": 400,
                "message": "Invalid recognition config: bad sample rate",
                "status": "INVALID_ARGUMENT",
            }
        })
        .to_string();

        let failure = Failure::status_failure(400, None, &body);
        assert_eq!(
            failure.to_string(),
            "HTTP 400: Invalid recognition config: bad sample rate"
        );
    }

    #[test]
    fn a_body_that_is_not_a_google_error_is_quoted_whole() {
        // An HTML error page from a proxy in front of the API is still the only
        // evidence there is.
        let failure = Failure::status_failure(502, None, "<html>bad gateway</html>");
        assert!(failure.to_string().contains("bad gateway"), "{failure}");
    }

    #[test]
    fn a_malformed_response_is_never_retryable() {
        assert!(!Failure::malformed("not json").is_retryable());
    }

    #[test]
    fn classification_survives_the_trip_through_a_core_error() {
        let error = conduit_core::Error::provider(
            "google",
            Failure::status_failure(429, Some("7"), "slow down"),
        );

        let failure = Failure::of(&error).expect("classified");
        assert_eq!(failure.status(), Some(429));
        assert_eq!(failure.retry_after(), Some(Duration::from_secs(7)));
        assert!(failure.is_retryable());
        assert!(error.to_string().contains("slow down"), "{error}");
    }

    #[test]
    fn an_unrelated_error_is_not_classified() {
        assert!(Failure::of(&conduit_core::Error::Config("nope".to_owned())).is_none());
    }
}
