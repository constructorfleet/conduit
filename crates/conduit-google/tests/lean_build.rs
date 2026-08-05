//! What a build without the `google` feature does.
//!
//! The providers still exist and still register, so a lean build reports the
//! missing feature when a provider definition is saved rather than the first time
//! somebody speaks. These tests run in both configurations and assert opposite
//! things, because the behaviour they pin is the difference between them.

use conduit_google::{Credentials, GoogleConfig, GoogleStt, GoogleTts};

/// A configuration that has to discover credentials to get anywhere.
fn needing_adc() -> GoogleConfig {
    GoogleConfig { credentials: Credentials::Adc, ..GoogleConfig::default() }
}

#[cfg(not(feature = "google"))]
#[tokio::test]
async fn construction_refuses_and_names_the_missing_feature() {
    let tts = GoogleTts::new(&needing_adc()).await.err();
    let stt = GoogleStt::new(&needing_adc()).await.err();
    assert!(tts.is_some() && stt.is_some(), "both capabilities refuse");

    for error in [tts, stt].into_iter().flatten() {
        assert!(
            matches!(error, conduit_core::Error::Config(_)),
            "a missing feature is a configuration problem, not a provider failure: {error:?}"
        );
        let message = error.to_string();
        assert!(message.contains("google"), "the feature to rebuild with is named: {message}");
        assert!(
            message.contains("Application Default Credentials"),
            "and what it would have done: {message}"
        );
    }
}

#[cfg(not(feature = "google"))]
#[tokio::test]
async fn an_explicit_token_still_works_without_credential_discovery() {
    // Nothing about a token needs `gcp_auth`. A lean build that refused one would
    // be refusing more than it has to.
    let config = GoogleConfig {
        credentials: Credentials::Token("t0ken".to_owned()),
        ..GoogleConfig::default()
    };
    assert!(GoogleTts::new(&config).await.is_ok());
    assert!(GoogleStt::new(&config).await.is_ok());
}

#[cfg(feature = "google")]
#[tokio::test]
async fn with_the_feature_construction_does_not_refuse_for_want_of_it() {
    // Whether ADC finds credentials depends on the host, so this asserts only the
    // thing the feature controls: no refusal that names the feature itself.
    for message in [
        GoogleTts::new(&needing_adc()).await.err().map(|error| error.to_string()),
        GoogleStt::new(&needing_adc()).await.err().map(|error| error.to_string()),
    ]
    .into_iter()
    .flatten()
    {
        assert!(
            !message.contains("--features google"),
            "the feature is compiled in, so nothing may ask for it: {message}"
        );
    }
}

#[cfg(feature = "google")]
#[tokio::test]
async fn a_token_that_is_only_whitespace_is_refused_rather_than_sent() {
    // An empty bearer token reaches Google as `Authorization: Bearer `, which
    // comes back as an opaque 401 rather than as the configuration mistake it is.
    let config = GoogleConfig {
        credentials: Credentials::Token("   ".to_owned()),
        ..GoogleConfig::default()
    };
    let error = match GoogleTts::new(&config).await {
        Ok(_) => panic!("expected an empty token to be refused"),
        Err(error) => error,
    };
    assert!(matches!(error, conduit_core::Error::Config(_)), "{error:?}");
}

#[test]
fn a_credential_never_renders_its_secret() {
    let rendered = format!("{:?}", Credentials::Token("super-secret-token".to_owned()));
    assert!(!rendered.contains("super-secret-token"), "{rendered}");
    assert!(rendered.contains("redacted"), "{rendered}");
}
