//! Classifying an `SdkError` the way [`conduit_http::Failure`] classifies a
//! reqwest one.
//!
//! Every other provider hands a `reqwest::Error` to `Failure::transport` and is
//! done. This one has an AWS SDK instead, so the same distinctions — was it
//! sent, did the server answer, is it worth trying again — have to be read off
//! the SDK's own error shape. What matters is that the *answer* comes out the
//! same, because a caller decides whether to retry from `Failure`, not from
//! which client produced it.

use aws_sdk_bedrockruntime::error::SdkError;
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use aws_smithy_runtime_api::client::result::ServiceError;
use aws_smithy_types::event_stream::RawMessage;
use conduit_http::Failure;

use aws_sdk_bedrockruntime::operation::converse_stream::ConverseStreamError;
use aws_sdk_bedrockruntime::operation::count_tokens::CountTokensError;
use aws_sdk_bedrockruntime::types::error::ConverseStreamOutputError;

/// Classifies an SDK error, given a way to classify the service's own answer.
///
/// The four non-service variants mean the same thing for every operation, so
/// they are read here; what a service error *is* depends on the operation, and
/// `service` is where the caller says.
fn classify<E, R>(
    error: &SdkError<E, R>,
    service: impl FnOnce(&ServiceError<E, R>) -> Failure,
) -> Failure
where
    // What `SdkError`'s own `std::error::Error` impl asks for, and reading the
    // cause chain is the whole reason [`detail`] exists.
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
                // A request the connector itself refused to send.
                Failure::unsendable(detail(error))
            } else {
                Failure::unreachable(detail(error))
            }
        }
        // Bytes arrived and did not mean what they claimed to, or the server
        // hung up partway. Asking again gets the same bytes.
        SdkError::ResponseError(_) => Failure::malformed(detail(error)),
        SdkError::ServiceError(context) => service(context),
        // The enum is non-exhaustive. A variant added after this build is one we
        // know nothing about except that the call did not succeed, and reporting
        // it as unreachable is the honest reading: it says try again, and the
        // SDK's own account of it comes along.
        _ => Failure::unreachable(detail(error)),
    }
}

/// The SDK's own account of a failure, cause chain included.
///
/// `SdkError`'s `Display` is a bare summary — "dispatch failure" — and the
/// useful part is always the source beneath it.
fn detail<E: std::error::Error + 'static, R: std::fmt::Debug>(
    error: &SdkError<E, R>,
) -> String {
    format!("{}", aws_smithy_types::error::display::DisplayErrorContext(error))
}

/// Classifies a failure the server answered with over HTTP.
///
/// Shared by every operation that gets a response back: the status and the
/// `Retry-After` header say everything about retryability, and reading them off
/// the response means an operation-specific error enum needs no arm here.
fn of_response<E: std::error::Error + 'static>(error: &SdkError<E, HttpResponse>) -> Failure {
    classify(error, |context| {
        let response = context.raw();
        Failure::status_failure(
            response.status().as_u16(),
            response.headers().get("retry-after"),
            &context.err().to_string(),
        )
    })
}

/// Classifies a failure to start a `ConverseStream`.
pub(crate) fn of_request(error: &SdkError<ConverseStreamError, HttpResponse>) -> Failure {
    of_response(error)
}

/// Classifies a failure of the health probe.
///
/// The probe is a `CountTokens` call, so its errors are that operation's — an
/// access denial, an unknown model, a throttle. Classified the same way, because
/// what an operator needs from a red provider is whether the credential, the
/// region, or the model id is the thing that is wrong.
pub(crate) fn of_count_tokens(error: &SdkError<CountTokensError, HttpResponse>) -> Failure {
    of_response(error)
}

/// Classifies a failure that arrived mid-stream.
///
/// An event stream error carries no HTTP response to read a status off, so the
/// status is the one the exception stands for. It is not decoration: it is what
/// decides retryability, and the difference between a throttle and a validation
/// error is the difference between waiting and giving up.
pub(crate) fn of_stream(error: &SdkError<ConverseStreamOutputError, RawMessage>) -> Failure {
    classify(error, |context| {
        let (status, message) = match context.err() {
            ConverseStreamOutputError::ThrottlingException(exception) => {
                (429, exception.to_string())
            }
            ConverseStreamOutputError::ServiceUnavailableException(exception) => {
                (503, exception.to_string())
            }
            ConverseStreamOutputError::InternalServerException(exception) => {
                (500, exception.to_string())
            }
            ConverseStreamOutputError::ValidationException(exception) => {
                (400, exception.to_string())
            }
            // The server's own word for "the stream broke, try again", and it
            // reports the status the underlying failure had.
            ConverseStreamOutputError::ModelStreamErrorException(exception) => (
                exception
                    .original_status_code()
                    .and_then(|code| u16::try_from(code).ok())
                    .unwrap_or(500),
                exception.to_string(),
            ),
            // A variant this build has no mapping for. Reported as a server
            // error rather than guessed at, because the SDK grows these and a
            // turn should not fail over a name.
            other => (500, other.to_string()),
        };
        Failure::status_failure(status, None, &message)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_sdk_bedrockruntime::types::error::{
        ModelStreamErrorException, ThrottlingException, ValidationException,
    };
    use conduit_http::FailureKind;

    /// A `ServiceError` around `error`, as the event stream would deliver it.
    fn mid_stream(error: ConverseStreamOutputError) -> Failure {
        of_stream(&SdkError::service_error(error, RawMessage::invalid(None)))
    }

    #[test]
    fn a_request_that_was_never_built_is_not_worth_building_again() {
        let failure: Failure = classify(
            &SdkError::<ConverseStreamError, HttpResponse>::construction_failure(
                conduit_core::Error::Config("no region".to_owned()),
            ),
            |_| unreachable!("not a service error"),
        );

        assert_eq!(failure.kind(), FailureKind::Request);
        assert!(!failure.is_retryable());
    }

    #[test]
    fn a_timeout_is_retryable_and_a_lost_connection_is_too() {
        let timeout: Failure = classify(
            &SdkError::<ConverseStreamError, HttpResponse>::timeout_error(
                conduit_core::Error::Config("too slow".to_owned()),
            ),
            |_| unreachable!("not a service error"),
        );
        assert_eq!(timeout.kind(), FailureKind::Timeout);
        assert!(timeout.is_retryable());

        let dispatch: Failure = classify(
            &SdkError::<ConverseStreamError, HttpResponse>::dispatch_failure(
                aws_sdk_bedrockruntime::error::ConnectorError::io(Box::new(
                    std::io::Error::from(std::io::ErrorKind::ConnectionReset),
                )),
            ),
            |_| unreachable!("not a service error"),
        );
        assert_eq!(dispatch.kind(), FailureKind::Transport);
        assert!(dispatch.is_retryable());
    }

    #[test]
    fn a_throttle_mid_stream_is_waited_out_and_a_validation_error_is_not() {
        // The distinction the whole module exists for: both arrive as the same
        // Rust type on the same stream, and one is worth trying again.
        let throttled = mid_stream(ConverseStreamOutputError::ThrottlingException(
            ThrottlingException::builder().message("Too many requests").build(),
        ));
        assert!(throttled.is_retryable(), "{throttled}");
        assert_eq!(throttled.status(), Some(429));

        let invalid = mid_stream(ConverseStreamOutputError::ValidationException(
            ValidationException::builder()
                .message("model id is not an inference profile")
                .build(),
        ));
        assert!(!invalid.is_retryable(), "{invalid}");
        assert!(invalid.to_string().contains("inference profile"), "{invalid}");
    }

    #[test]
    fn a_broken_stream_reports_the_status_the_server_gave_it() {
        let failure = mid_stream(ConverseStreamOutputError::ModelStreamErrorException(
            ModelStreamErrorException::builder()
                .message("stream interrupted")
                .original_status_code(503)
                .build(),
        ));

        assert_eq!(failure.status(), Some(503));
        assert!(failure.is_retryable());
    }

    #[test]
    fn a_broken_stream_that_names_no_status_is_still_worth_retrying() {
        // The API documents this exception as "retry your request", so the
        // absence of a status must not turn into a permanent failure.
        let failure = mid_stream(ConverseStreamOutputError::ModelStreamErrorException(
            ModelStreamErrorException::builder().message("stream interrupted").build(),
        ));

        assert!(failure.is_retryable(), "{failure}");
    }
}
