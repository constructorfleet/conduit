//! Where a Google access token comes from.
//!
//! Nobody types a Google key. Every other credential in a Google deployment is
//! discovered — from the metadata server on a GCE instance or GKE pod, from the
//! service-account JSON that `GOOGLE_APPLICATION_CREDENTIALS` points at, or
//! from whatever `gcloud auth application-default login` last wrote. That chain
//! is Application Default Credentials, and asking an operator to paste a bearer
//! token instead would be asking them to do by hand the one thing the platform
//! does for them.
//!
//! So [`Credentials::Adc`] is the shape a real deployment uses.
//! [`Credentials::Token`] exists for the two cases ADC cannot serve: a test
//! pointing at a stand-in server, and a deployment that mints its own tokens
//! out of band.

use std::sync::Arc;

use conduit_core::{Error, Result};

/// The OAuth scope both speech APIs are authorized under.
///
/// Google publishes no narrower scope for either service; `cloud-platform` is
/// what the REST reference names for `text:synthesize` and `speech:recognize`
/// alike.
pub const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// How a provider proves who it is.
#[derive(Clone, Default)]
pub enum Credentials {
    /// Application Default Credentials, discovered at construction.
    ///
    /// The default, because it is what a real deployment uses. Requires the
    /// `google` feature; without it, constructing a provider fails with an
    /// [`Error::Config`] naming the feature.
    #[default]
    Adc,
    /// A bearer token supplied directly.
    ///
    /// The escape hatch: a caller that already has a token, or a test whose
    /// stand-in server does not check one.
    Token(String),
}

// A manual impl so no `Debug` derived anywhere upstream can print a token: the
// only way to keep a credential out of a log is for the type never to render
// it.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Adc => formatter.write_str("Adc"),
            Self::Token(_) => formatter.write_str("Token(<redacted>)"),
        }
    }
}

/// A resolved source of bearer tokens.
///
/// Cloned by every capability built from one configuration, so the token cache
/// underneath is shared rather than duplicated per provider.
#[derive(Clone)]
pub enum Tokens {
    /// A token provider that refreshes on demand.
    #[cfg(feature = "google")]
    Adc(Arc<dyn gcp_auth::TokenProvider>),
    /// A fixed token, used until it stops working.
    Fixed(Arc<str>),
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "google")]
            Self::Adc(_) => formatter.write_str("Adc"),
            Self::Fixed(_) => formatter.write_str("Fixed(<redacted>)"),
        }
    }
}

impl Tokens {
    /// Resolves `credentials` into a token source.
    ///
    /// Called once when a provider is constructed, which is deliberate: an
    /// operator who saves a Google provider definition on a host with no
    /// credentials is told so while they are looking at the form, not when
    /// someone speaks to it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if [`Credentials::Adc`] is asked for in a build
    /// compiled without the `google` feature, or if no Application Default
    /// Credentials can be discovered.
    pub async fn resolve(provider: &str, credentials: &Credentials) -> Result<Self> {
        match credentials {
            Credentials::Token(token) if token.trim().is_empty() => Err(Error::Config(
                format!("provider `{provider}` was given an empty access token"),
            )),
            Credentials::Token(token) => Ok(Self::Fixed(Arc::from(token.as_str()))),
            Credentials::Adc => Self::adc(provider).await,
        }
    }

    /// A bearer token good for the next request.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Provider`] if the credential cannot be refreshed — an
    /// expired refresh token, a metadata server that has gone away, a revoked
    /// service account.
    pub async fn bearer(&self, provider: &str) -> Result<Arc<str>> {
        // Without credential discovery there is nothing to refresh, so `provider`
        // has no error to name.
        let _ = provider;
        match self {
            Self::Fixed(token) => Ok(Arc::clone(token)),
            #[cfg(feature = "google")]
            Self::Adc(tokens) => {
                let token = tokens.token(&[SCOPE]).await.map_err(|error| {
                    // `error` describes the *failure to obtain* a token and
                    // never contains one; `gcp_auth` keeps token bodies out of
                    // its error text.
                    Error::provider(
                        provider,
                        crate::failure::Failure::malformed(format!(
                            "cannot obtain a Google access token: {error}"
                        )),
                    )
                })?;
                Ok(Arc::from(token.as_str()))
            }
        }
    }

    /// Discovers Application Default Credentials.
    #[cfg(feature = "google")]
    async fn adc(provider: &str) -> Result<Self> {
        let tokens = gcp_auth::provider().await.map_err(|error| {
            Error::Config(format!(
                "provider `{provider}` found no Application Default Credentials: {error}. Set \
                 GOOGLE_APPLICATION_CREDENTIALS to a service-account key, run `gcloud auth \
                 application-default login`, or run on a host with a metadata server"
            ))
        })?;
        // The identity is worth a log line and the token never is. `project_id`
        // is not fatal: neither speech endpoint is project-scoped in its URL, so
        // a credential that cannot name a project still synthesizes and still
        // transcribes.
        match tokens.project_id().await {
            Ok(project) => {
                tracing::info!(provider, project = %project, "resolved Google credentials");
            }
            Err(error) => {
                tracing::info!(
                    provider,
                    %error,
                    "resolved Google credentials that do not name a project"
                );
            }
        }
        Ok(Self::Adc(tokens))
    }

    /// The refusal a build without credential discovery gives.
    #[cfg(not(feature = "google"))]
    #[allow(clippy::unused_async)]
    async fn adc(provider: &str) -> Result<Self> {
        Err(Error::Config(format!(
            "provider `{provider}` authenticates with Application Default Credentials, which \
             this build cannot discover: it was compiled without the `google` feature. Supply an \
             access token directly, or rebuild with `--features google`"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_credential_never_renders_itself() {
        // The only reliable way to keep a token out of a log: a `Debug` that
        // does not have it to print.
        let secret = "ya29.a0AfB_byC_this_must_not_appear";
        assert!(!format!("{:?}", Credentials::Token(secret.to_owned())).contains(secret));
        assert!(!format!("{:?}", Tokens::Fixed(Arc::from(secret))).contains(secret));
    }

    #[tokio::test]
    async fn a_supplied_token_is_used_verbatim() {
        let tokens = Tokens::resolve("google", &Credentials::Token("t0ken".to_owned()))
            .await
            .expect("a token needs no discovery");
        assert_eq!(&*tokens.bearer("google").await.expect("fixed"), "t0ken");
    }

    #[tokio::test]
    async fn an_empty_token_is_refused_rather_than_sent() {
        // An empty `Authorization: Bearer ` header earns a 401 that reads like
        // a permissions problem. Naming it here says what it is.
        let error = Tokens::resolve("google", &Credentials::Token("   ".to_owned()))
            .await
            .expect_err("empty");
        assert!(error.to_string().contains("empty access token"), "{error}");
    }

    #[cfg(not(feature = "google"))]
    #[tokio::test]
    async fn without_the_feature_adc_names_the_feature_it_needs() {
        let error =
            Tokens::resolve("google", &Credentials::Adc).await.expect_err("no discovery");
        assert!(error.to_string().contains("`google` feature"), "{error}");
    }
}
