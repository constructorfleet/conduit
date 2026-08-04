//! Utterance transform provider interface.
//!
//! A model writes for a reader. It emphasises with asterisks, punctuates with
//! emoji, and links with brackets — and every one of those is noise once the
//! words are spoken aloud. Asking the model not to do it works until it does
//! not, which is the problem this interface exists to solve: what reaches a
//! synthesizer is decided by the pipeline rather than by the model's
//! willingness to follow an instruction.
//!
//! A transform sits on the utterance, before it is rendered. Which renderings
//! it applies to is a property of the graph: a `transform` node between a core
//! and a `tts` node changes what is spoken and leaves a text sink reading the
//! markdown the model actually wrote.

use conduit_core::Result;

use crate::Provider;

/// Rewrites what a model said on its way to being rendered.
///
/// # Contract
///
/// Implementations owe their callers all of this:
///
/// - **One segment in, one segment out.** A transform is called with a single
///   speakable unit — usually one sentence — because synthesis begins before
///   the model has finished writing. It must not wait for more input.
/// - **Removing everything is allowed.** A segment that was nothing but an
///   emoji has no spoken form, and `""` is how a transform says so. The caller
///   drops the segment rather than synthesizing silence.
/// - **No state between segments.** One provider serves every turn in every
///   pipeline that names it, concurrently. Anything remembered from the last
///   call belongs to somebody else's conversation.
/// - **Failure is not silent.** A transform that cannot do its job returns an
///   error rather than the text it was given; the runtime reports which node
///   failed. Passing the input through would deliver exactly what the operator
///   configured the transform to prevent.
///
/// The last two are what make transforms chainable: a graph may run several in
/// a row, and each sees the previous one's output with no shared context.
#[async_trait::async_trait]
pub trait UtteranceTransform: Provider {
    /// Rewrites one speakable segment.
    ///
    /// Returns the text to render, which may be empty when nothing is left to
    /// say.
    ///
    /// # Errors
    ///
    /// Returns an error if the segment cannot be transformed. The turn stops:
    /// delivering untransformed text is the outcome the transform was placed
    /// in the graph to rule out.
    async fn transform(&self, segment: &str) -> Result<String>;
}
