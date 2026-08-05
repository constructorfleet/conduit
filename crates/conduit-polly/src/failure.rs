//! Classifying an `SdkError` the way [`conduit_http::Failure`] classifies a
//! reqwest one.
//!
//! The same job `conduit-bedrock::failure` does, and deliberately the same
//! answers: a caller decides whether to retry from a `Failure`, not from which
//! client produced it, so an AWS throttle has to read the same here as a 429 from
//! any HTTP provider. Not shared with that crate because sharing it would mean a
//! public dependency between two optional AWS features and a generic over two
//! unrelated operation-error enums, for forty lines that are only interesting
//! where they differ.

use aws_sdk_polly::error::SdkError;
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use aws_smithy_runtime_api::client::result::ServiceError;
use conduit_http::Failure;

/// Classifies an SDK error, given a way to classify the service's own answer.
fn classify<E, R>(
    error: &SdkError<E, R>,
    service: impl FnOnce(&ServiceError<E, R>) -> Failure,
) -> Failure
where
    E: std::error::Error + 'static,
    R: std::fmt::Debug,
{
    match error {
        // Nothing was sent, and the reason is on this side.
        SdkError::ConstructionFailure(_) => Failure::unsendable(detail(error)),
        SdkError::TimeoutError(_) => Failure::timeout(detail(error)),
        SdkError::DispatchFailure(dispatch) => {
            // The SDK folds a connect timeout into dispatch, and a timeout is
            // the one transport failure that says something about the server
            // rather than the network.
            if dispatch.as_connector_error().is_some_and(|connector| connector.is_timeout()) {
                Failure::timeout(detail(error))
            } else if dispatch.as_connector_error().is_some_and(|connector| connector.is_user())
            {
                Failure::unsendable(detail(error))
            } else {
                Failure::unreachable(detail(error))
            }
        }
        // Bytes arrived and did not mean what they claimed to. Asking again gets
        // the same bytes.
        SdkError::ResponseError(_) => Failure::malformed(detail(error)),
        SdkError::ServiceError(context) => service(context),
        // The enum is non-exhaustive. A variant added after this build is one we
        // know nothing about except that the call did not succeed; unreachable is
        // the honest reading, and it says try again.
        _ => Failure::unreachable(detail(error)),
    }
}

/// The SDK's own account of a failure, cause chain included.
///
/// `SdkError`'s `Display` is a bare summary — "dispatch failure" — and the useful
/// part is always the source beneath it.
fn detail<E: std::error::Error + 'static, R: std::fmt::Debug>(
    error: &SdkError<E, R>,
) -> String {
    format!("{}", aws_smithy_types::error::display::DisplayErrorContext(error))
}

/// Classifies a failure the server answered with over HTTP.
///
/// The status and `Retry-After` say everything about retryability, so reading them
/// off the response means an operation-specific error enum needs no arm here —
/// which is what keeps `SynthesizeSpeech` and `DescribeVoices` on one path.
pub(crate) fn of_response<E: std::error::Error + 'static>(
    error: &SdkError<E, HttpResponse>,
) -> Failure {
    classify(error, |context| {
        let response = context.raw();
        Failure::status_failure(
            response.status().as_u16(),
            response.headers().get("retry-after"),
            &context.err().to_string(),
        )
    })
}
