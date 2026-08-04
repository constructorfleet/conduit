//! Operator-written utterance transforms, run on a sandboxed Rhai interpreter.
//!
//! [`conduit_transform`][1] ships three fixed rules. They cover what almost
//! every pipeline wants and nothing else, because each one is a Rust function
//! somebody had to write and release. This crate is the other half of that
//! trade: an operator writes the rewrite themselves, saves it, and it runs on
//! the next utterance — no build, no deploy.
//!
//! It is a separate crate for one reason. An interpreter is a large dependency,
//! and folding it into `conduit-transform` would charge every consumer of the
//! three builtin rules for a scripting engine none of them asked for. A
//! deployment that wants only `StripEmoji` never compiles Rhai.
//!
//! # The bargain
//!
//! Running operator-supplied code inside the turn loop is only acceptable
//! because the interpreter is boxed in. A transform error ends one turn
//! cleanly; a transform *hang* ends every turn on that pipeline, forever.
//! Failing fast is therefore the entire purpose of the sandbox, and every guard
//! in [`sandbox`] exists because something specific defeats the guards without
//! it. `crates/conduit-script/README.md` records which attack each one stops
//! and the measurements behind it.
//!
//! # Writing a script
//!
//! The script is an expression, or a block ending in one. The incoming segment
//! is bound to a variable named [`INPUT_VARIABLE`], and the value the script
//! evaluates to is the text that gets rendered:
//!
//! ```rhai
//! segment.to_upper()
//! ```
//!
//! Returning `""` drops the segment, which is a normal outcome rather than a
//! failure — a sentence that was nothing but an emoji has no spoken form.
//! Returning anything that is not a string is an error: silently stringifying a
//! number would let a typo become spoken output.
//!
//! [1]: https://docs.rs/conduit-transform

pub mod sandbox;

use std::sync::Arc;
use std::time::Duration;

use conduit_core::{Error, Result};
use conduit_provider::transform::UtteranceTransform;
use conduit_provider::{Capability, Descriptor, Provider};
use rhai::{Scope, AST};

pub use sandbox::{INPUT_VARIABLE, MAX_OPERATIONS, MAX_STRING_BYTES};

/// The shortest deadline a script may be given.
///
/// A deadline below this is almost certainly a units mistake — seconds typed
/// where milliseconds were meant — and it would fail every utterance.
pub const MIN_TIMEOUT: Duration = Duration::from_millis(1);

/// The longest deadline a script may be given.
///
/// A transform sits between a model finishing a sentence and a synthesizer
/// starting to speak it, so its budget is a fraction of a conversational turn.
/// Nothing legitimate needs longer, and a longer cap only widens the window in
/// which a blocking thread is stuck.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(5);

/// An utterance transform defined by an operator-supplied script.
///
/// Holds a compiled program and the engine that runs it, both shared and both
/// immutable. Every turn that names this provider evaluates the same [`AST`] on
/// the same [`rhai::Engine`] concurrently; the per-call state that makes that
/// safe is a fresh [`Scope`] and a thread-local deadline, neither of which
/// outlives one call.
#[derive(Debug, Clone)]
pub struct Script {
    descriptor: Descriptor,
    engine: Arc<rhai::Engine>,
    program: Arc<AST>,
    source: Arc<str>,
    timeout: Duration,
}

impl Script {
    /// Compiles `source` into a transform registered as `name`, giving each
    /// evaluation `timeout_ms` of wall-clock time.
    ///
    /// Compilation happens here so a script that cannot parse is refused at
    /// registration rather than on the first utterance of the first
    /// conversation that reaches it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if `timeout_ms` is outside [`MIN_TIMEOUT`]
    /// ..=[`MAX_TIMEOUT`], or if `source` does not compile. Compilation catches
    /// syntax errors and undefined variables; it does *not* catch unknown
    /// functions, unknown methods, or a wrong return type, so those still
    /// surface as a failed turn on the first segment.
    pub fn new(
        name: impl Into<String>,
        source: impl Into<String>,
        timeout_ms: u64,
    ) -> Result<Self> {
        let name = name.into();
        let timeout = Duration::from_millis(timeout_ms);
        if timeout < MIN_TIMEOUT || timeout > MAX_TIMEOUT {
            return Err(Error::Config(format!(
                "script `{name}` asked for a {timeout_ms}ms deadline, outside the supported {}ms..={}ms",
                MIN_TIMEOUT.as_millis(),
                MAX_TIMEOUT.as_millis(),
            )));
        }

        let source = source.into();
        let engine = sandbox::engine();
        let program = sandbox::compile(&engine, &source).map_err(|detail| {
            Error::Config(format!("script `{name}` did not compile: {detail}"))
        })?;

        Ok(Self {
            descriptor: Descriptor::new(name, Capability::Transform),
            engine: Arc::new(engine),
            program: Arc::new(program),
            source: source.into(),
            timeout,
        })
    }

    /// Checks that `source` would compile, without building a provider.
    ///
    /// This is the save-time check an editor calls so an operator learns about
    /// a typo while the script is still in front of them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] describing the parse failure, positioned at
    /// the offending line and column.
    pub fn check(source: &str) -> Result<()> {
        let engine = sandbox::engine();
        sandbox::compile(&engine, source)
            .map(|_| ())
            .map_err(|detail| Error::Config(format!("script did not compile: {detail}")))
    }

    /// Sets the human-readable name operator screens show.
    ///
    /// Separate from the identity this provider was built with: the identity is
    /// what a pipeline selects and what appears in metric labels, and this is
    /// only what a person reads.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.descriptor = self.descriptor.with_label(label);
        self
    }

    /// The script this transform runs, as the operator wrote it.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// How long one evaluation may run before it is abandoned.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Turns a failed evaluation into the error the runtime reports.
    ///
    /// Two failures mean "this script does not finish": the wall-clock deadline
    /// firing, and the operation budget running out. Both are reported as
    /// [`Error::Timeout`] because they are the same fact to an operator — the
    /// script ran away — and because the distinction between them is an
    /// implementation detail of which guard noticed first.
    fn failure(&self, error: Box<rhai::EvalAltResult>, elapsed: Duration) -> Error {
        let provider = self.name();
        let detail = error.to_string();
        match *error {
            rhai::EvalAltResult::ErrorTerminated(..)
            | rhai::EvalAltResult::ErrorTooManyOperations(..) => {
                tracing::warn!(
                    provider,
                    elapsed_ms = elapsed.as_millis(),
                    timeout_ms = self.timeout.as_millis(),
                    max_operations = MAX_OPERATIONS,
                    error = detail,
                    "script transform exceeded its budget; failing the segment"
                );
                Error::Timeout { operation: format!("script transform {provider}"), elapsed }
            }
            other => {
                tracing::error!(
                    provider,
                    elapsed_ms = elapsed.as_millis(),
                    error = detail,
                    "script transform failed; failing the segment"
                );
                Error::provider(provider, other)
            }
        }
    }
}

#[async_trait::async_trait]
impl Provider for Script {
    fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
}

#[async_trait::async_trait]
impl UtteranceTransform for Script {
    async fn transform(&self, segment: &str) -> Result<String> {
        let engine = Arc::clone(&self.engine);
        let program = Arc::clone(&self.program);
        let timeout = self.timeout;
        let segment = segment.to_owned();

        // Rhai is synchronous and CPU-bound, and a runaway script is CPU-bound
        // on purpose. Evaluating it on the reactor would stall every other
        // conversation sharing that worker until a guard fired.
        let finished = tokio::task::spawn_blocking(move || {
            let started = std::time::Instant::now();
            // Installed per call rather than baked into the engine, which is
            // what lets one shared engine enforce a different deadline for
            // every concurrent evaluation.
            let _deadline = sandbox::Deadline::install(started + timeout);

            // A fresh scope per call, seeded with `push` and never
            // `push_constant`: the engine optimises at `Simple` level, and a
            // constant would be folded into the AST at compile time so every
            // later evaluation returned the first call's input.
            let mut scope = Scope::new();
            scope.push(INPUT_VARIABLE, segment);

            engine
                .eval_ast_with_scope::<String>(&mut scope, &program)
                .map_err(|error| (error, started.elapsed()))
        })
        .await;

        match finished {
            Ok(Ok(text)) => Ok(text),
            Ok(Err((error, elapsed))) => Err(self.failure(error, elapsed)),
            // The interpreter panicked, or the runtime is shutting down.
            // Neither is something the segment can be recovered from, and
            // passing the input through would deliver what the operator wrote
            // this transform to prevent.
            Err(join) => {
                tracing::error!(
                    provider = self.name(),
                    error = %join,
                    "script transform did not run to completion; failing the segment"
                );
                Err(Error::provider(self.name(), join))
            }
        }
    }
}

#[cfg(test)]
mod tests;
