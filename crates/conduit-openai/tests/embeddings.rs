//! The embeddings adapter driven against a stand-in for a compatible server.
//!
//! Two things are worth pinning here. The request has to be the shape every
//! compatible server expects, because this one adapter is meant to reach the
//! hosted API, Ollama, vLLM, and `text-embeddings-inference` without a variant
//! per vendor. And a reply with no embedding in it has to be an error rather
//! than an empty vector: a caller that stored a zero-length vector would get a
//! record that matches nothing, forever, with nothing having gone wrong.

mod server;

use std::time::Duration;

use conduit_openai::{Failure, OpenAiConfig, OpenAiEmbeddings};
use server::MockServer;

fn config(server: &MockServer) -> OpenAiConfig {
    OpenAiConfig {
        base_url: server.url(),
        api_key: Some("test-key".to_owned()),
        ..OpenAiConfig::default()
    }
}

fn embedder(server: &MockServer) -> OpenAiEmbeddings {
    OpenAiEmbeddings::new(&config(server), "text-embedding-3-small").expect("embedder builds")
}

/// One embedding, in the vendor's reply shape.
fn reply(vector: &[f32]) -> String {
    serde_json::json!({
        "object": "list",
        "data": [{ "object": "embedding", "index": 0, "embedding": vector }],
        "model": "text-embedding-3-small",
    })
    .to_string()
}

/// The failure this crate recorded, so a caller can classify it.
fn failure(error: &conduit_core::Error) -> &Failure {
    Failure::of(error).unwrap_or_else(|| panic!("not a classified provider failure: {error}"))
}

#[tokio::test]
async fn one_text_becomes_one_vector() {
    let server = MockServer::start(reply(&[0.5, -0.25, 0.125])).await;

    let vector =
        embedder(&server).embed("the recycling goes out on tuesday").await.expect("embeds");

    assert_eq!(vector, [0.5, -0.25, 0.125]);
}

#[tokio::test]
async fn the_request_carries_the_model_and_the_text_as_a_list() {
    let server = MockServer::start(reply(&[1.0])).await;

    embedder(&server).embed("mabel is the cat").await.expect("embeds");

    let body = server.last_body().await.expect("a JSON body");
    assert_eq!(body["model"], "text-embedding-3-small");
    // A list even for one text: `input` is typed as an array, and a server is
    // entitled to reject a bare string.
    assert_eq!(body["input"], serde_json::json!(["mabel is the cat"]));
}

#[tokio::test]
async fn a_configured_key_authenticates_the_request() {
    let server = MockServer::start(reply(&[1.0])).await;

    embedder(&server).embed("mabel is the cat").await.expect("embeds");

    assert_eq!(server.last_authorization().await.as_deref(), Some("Bearer test-key"));
}

#[tokio::test]
async fn a_local_server_needing_no_key_is_not_sent_one() {
    let server = MockServer::start(reply(&[1.0])).await;
    let embedder = OpenAiEmbeddings::new(
        &OpenAiConfig { base_url: server.url(), ..OpenAiConfig::default() },
        "nomic-embed-text",
    )
    .expect("embedder builds");

    embedder.embed("mabel is the cat").await.expect("embeds");

    assert_eq!(server.last_authorization().await, None, "no key was configured");
}

#[tokio::test]
async fn only_the_first_embedding_is_taken_when_a_server_answers_with_several() {
    let body = serde_json::json!({
        "data": [
            { "index": 0, "embedding": [1.0, 0.0] },
            { "index": 1, "embedding": [0.0, 1.0] },
        ],
    })
    .to_string();
    let server = MockServer::start(body).await;

    let vector = embedder(&server).embed("mabel is the cat").await.expect("embeds");

    assert_eq!(vector, [1.0, 0.0], "one text was sent, so the first reply is that text's");
}

#[tokio::test]
async fn a_reply_with_no_embedding_is_an_error_rather_than_an_empty_vector() {
    let server = MockServer::start(serde_json::json!({ "data": [] }).to_string()).await;

    let error = embedder(&server).embed("mabel is the cat").await.expect_err("no embedding");

    assert!(
        !failure(&error).is_retryable(),
        "an empty reply will not fill in on retry: {error}"
    );
    assert!(
        error.to_string().contains("no embedding"),
        "the message says what was missing: {error}"
    );
}

#[tokio::test]
async fn a_reply_that_will_not_parse_is_reported_as_malformed() {
    let server = MockServer::start("not json at all".to_owned()).await;

    let error = embedder(&server).embed("mabel is the cat").await.expect_err("unreadable");

    assert!(
        !failure(&error).is_retryable(),
        "sending nonsense again returns nonsense: {error}"
    );
    assert!(error.to_string().contains("embedding"), "the message names the body: {error}");
}

#[tokio::test]
async fn a_rejected_request_carries_the_status_so_a_caller_can_decide_to_retry() {
    let server = MockServer::start_status(429, "slow down").await;

    let error = embedder(&server).embed("mabel is the cat").await.expect_err("rejected");
    let failure = failure(&error);

    assert_eq!(failure.status(), Some(429));
    assert!(failure.is_retryable(), "a rate limit passes: {error}");
}

#[tokio::test]
async fn an_unknown_model_is_permanent_rather_than_worth_retrying() {
    let server = MockServer::start_status(400, "unknown model `text-embedding-3-tiny`").await;

    let error = embedder(&server).embed("mabel is the cat").await.expect_err("rejected");

    assert!(!failure(&error).is_retryable(), "asking again will not invent the model: {error}");
}

#[tokio::test]
async fn a_server_that_never_answers_ends_the_request_instead_of_holding_the_turn() {
    // An embedding is awaited on the critical path of a turn, so a stall here
    // is a person waiting in silence. `connect_timeout` cannot bound it: the
    // handshake completed.
    let server = MockServer::start_stalled().await;
    let embedder = OpenAiEmbeddings::new(
        &OpenAiConfig {
            base_url: server.url(),
            read_timeout: Some(Duration::from_millis(150)),
            ..OpenAiConfig::default()
        },
        "text-embedding-3-small",
    )
    .expect("embedder builds");

    let error = embedder.embed("mabel is the cat").await.expect_err("stalled");

    assert!(failure(&error).is_timeout(), "a stall is a timeout: {error}");
}

#[tokio::test]
async fn the_configured_identity_is_what_errors_are_reported_against() {
    let server = MockServer::start_status(500, "broken").await;
    let embedder = OpenAiEmbeddings::new(
        &OpenAiConfig {
            base_url: server.url(),
            name: "embeddings-box".to_owned(),
            ..OpenAiConfig::default()
        },
        "nomic-embed-text",
    )
    .expect("embedder builds");

    assert_eq!(embedder.name(), "embeddings-box");
    assert_eq!(embedder.model(), "nomic-embed-text");

    let error = embedder.embed("mabel is the cat").await.expect_err("rejected");
    assert!(
        error.to_string().contains("embeddings-box"),
        "an operator running two servers needs to know which one: {error}"
    );
}
