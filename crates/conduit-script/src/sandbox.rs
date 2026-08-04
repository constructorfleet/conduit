//! The box the operator's script runs in.
//!
//! Every setting here is load-bearing. The comments say what each one stops,
//! because the obvious simplifications — start from [`rhai::Engine::new`] and
//! subtract, or trust a timeout to catch everything — were each measured and
//! each failed. `crates/conduit-script/README.md` carries the numbers.

use std::cell::Cell;
use std::time::Instant;

use rhai::module_resolvers::DummyModuleResolver;
use rhai::packages::{
    ArithmeticPackage, BasicArrayPackage, BasicIteratorPackage, BasicStringPackage,
    LogicPackage, MoreStringPackage, Package,
};
use rhai::{Dynamic, Engine, ParseError, Scope, AST};

/// The name the incoming segment is bound to inside a script.
///
/// The word the [`UtteranceTransform`](conduit_provider::transform::UtteranceTransform)
/// contract and the runtime's rewrite loop both use, so an operator reading the
/// trait docs and an operator reading a script see the same noun.
pub const INPUT_VARIABLE: &str = "segment";

/// How many interpreter operations one evaluation may perform.
///
/// The coarse guard: it stops a script that loops a great many cheap times.
/// It cannot stop a script that loops a few very expensive times, which is why
/// it is not the only limit.
pub const MAX_OPERATIONS: u64 = 500_000;

/// The largest string a script may build, in bytes.
///
/// Not an operator knob, deliberately. `loop { s += s; }` spends nearly all of
/// its time *inside* one operation, so the operation counter and the progress
/// hook both go unconsulted while it doubles; measured with this unset, a 2s
/// deadline was honoured only after the process reached 9.1 GB resident. Since
/// `spawn_blocking` cannot cancel the thread doing that allocating, the only
/// place to stop it is here — and an operator who could set it to zero could
/// reintroduce the whole failure.
///
/// 256 KiB is far above any plausible utterance and far below anything that
/// threatens the host.
pub const MAX_STRING_BYTES: usize = 256 * 1024;

/// The largest array a script may build.
///
/// The same argument as [`MAX_STRING_BYTES`], for the other growable type a
/// split-and-rejoin script naturally reaches for.
pub const MAX_ARRAY_ITEMS: usize = 8 * 1024;

/// How deep script function calls may nest.
///
/// Bounds recursion, which would otherwise exhaust the native stack — and a
/// stack overflow is a process abort, not a failed turn.
const MAX_CALL_LEVELS: usize = 32;

/// How deep an expression may nest, in top-level code and in functions.
///
/// Deeply nested expressions are a parser stack risk, and this is checked at
/// compile time, so a hostile script is refused before it ever runs.
const MAX_EXPR_DEPTH: usize = 64;

/// How often the progress hook samples the clock, in operations.
///
/// The hook fires once per operation, and reading a clock that often would cost
/// more than the script. Sampling means the deadline is enforced to within this
/// many operations, which is a bound on overshoot rather than a hole: the
/// interpreter cannot get stuck between two samples, only slowed.
const CLOCK_SAMPLE_INTERVAL: u64 = 512;

thread_local! {
    /// When the evaluation running on this thread must stop.
    ///
    /// The deadline has to be per *call*, but the engine is shared by every
    /// concurrent call, so it cannot live on the engine. It also cannot be
    /// passed in, because the progress hook's signature is fixed. A
    /// thread-local is the seam: `spawn_blocking` gives each evaluation a
    /// thread to itself for its whole duration, so "the deadline for this
    /// thread" and "the deadline for this call" are the same thing.
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// The current thread's evaluation deadline, for the lifetime of one call.
///
/// Clears the deadline on drop, so a pooled blocking thread cannot carry a
/// stale deadline into the next evaluation and kill it instantly. Restores the
/// previous value rather than clearing outright, which keeps nesting honest.
#[derive(Debug)]
pub struct Deadline {
    previous: Option<Instant>,
}

impl Deadline {
    /// Makes `at` the deadline for evaluations on this thread.
    #[must_use]
    pub fn install(at: Instant) -> Self {
        Self { previous: DEADLINE.replace(Some(at)) }
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        DEADLINE.set(self.previous);
    }
}

/// Whether the current thread's deadline has passed.
///
/// No deadline installed means nothing to enforce, which is the case for a
/// bare compile.
fn past_deadline() -> bool {
    DEADLINE.get().is_some_and(|deadline| Instant::now() > deadline)
}

/// Builds the sandboxed interpreter.
///
/// Every provider gets its own engine — they are cheap next to a conversation —
/// but one engine serves all of that provider's concurrent evaluations, which is
/// why nothing call-specific is configured here.
#[must_use]
pub fn engine() -> Engine {
    // `Engine::new_raw()`, never `Engine::new()`. This is the single most
    // important line in the crate. `new()` registers the standard package and a
    // *filesystem* module resolver, and subtracting them afterwards does not
    // work: `disable_symbol("sleep")` has no effect on `sleep`, because `sleep`
    // is a registered function rather than a keyword. Measured, a `sleep(3)`
    // blocked for 3.0055 seconds and returned `Ok` with both a 10k operation
    // cap and a 100ms deadline in force — a sleeping thread performs no
    // operations, so neither guard is ever consulted. Not registering the
    // package is the only thing that removes it.
    let mut engine = Engine::new_raw();

    // A curated language: enough to rewrite strings, and nothing that reaches
    // outside the process.
    ArithmeticPackage::new().register_into_engine(&mut engine);
    LogicPackage::new().register_into_engine(&mut engine);
    BasicStringPackage::new().register_into_engine(&mut engine);
    MoreStringPackage::new().register_into_engine(&mut engine);
    BasicIteratorPackage::new().register_into_engine(&mut engine);
    BasicArrayPackage::new().register_into_engine(&mut engine);
    // Deliberately absent: `BasicMapPackage`, because object maps buy nothing
    // for rewriting a string; and `BasicTimePackage`, because `timestamp` is
    // nondeterminism, and a transform that rewrites differently depending on
    // the clock is not reviewable. `LanguageCorePackage` is absent too, which
    // is what keeps `sleep` and `exit` out.

    // Belt and braces around modules. The dummy resolver refuses every import
    // by path, `disable_symbol` makes the keyword itself a syntax error, and
    // the module cap means a resolver reintroduced by mistake still loads
    // nothing. `eval` goes with them: a script that builds code at runtime is a
    // script no save-time check can inspect.
    engine.set_module_resolver(DummyModuleResolver::new());
    engine.disable_symbol("eval");
    engine.disable_symbol("import");
    engine.set_max_modules(0);

    engine.set_max_operations(MAX_OPERATIONS);
    engine.set_max_string_size(MAX_STRING_BYTES);
    engine.set_max_array_size(MAX_ARRAY_ITEMS);
    engine.set_max_call_levels(MAX_CALL_LEVELS);
    // Plural, and it takes two depths: one for top-level expressions and one
    // for expressions inside script functions. There is no singular form.
    engine.set_max_expr_depths(MAX_EXPR_DEPTH, MAX_EXPR_DEPTH);

    // Turns a misspelled variable into a compile error, which is what makes
    // `Script::check` worth calling. Note that it does *not* check function or
    // method names.
    engine.set_strict_variables(true);

    // A script cannot write to the operator's logs. `print` and `debug` are
    // left callable so a script using them still runs; their output goes
    // nowhere, because operator-supplied text on a shared log is a way to forge
    // log lines.
    engine.on_print(|_| {});
    engine.on_debug(|_, _, _| {});

    // The wall-clock guard, and the reason the operation cap is not enough on
    // its own: a script can be slow without being busy. Returning `Some`
    // terminates the evaluation with `ErrorTerminated`.
    engine.on_progress(|operations| {
        if operations % CLOCK_SAMPLE_INTERVAL == 0 && past_deadline() {
            Some(Dynamic::UNIT)
        } else {
            None
        }
    });

    engine
}

/// Compiles `source` against a scope shaped like the one evaluation will use.
///
/// The scope matters. `strict_variables` is on, so a bare compile of any script
/// that mentions the input variable fails with "Undefined variable" — a check
/// built that way would reject every correct script. Seeding the same name here
/// that [`transform`](conduit_provider::transform::UtteranceTransform::transform)
/// seeds is what keeps validation and execution agreeing.
///
/// `push`, never `push_constant`, and *this* is the call site where it decides
/// the outcome: the engine optimises at `Simple` level, so a constant in the
/// compile-time scope is folded into the AST here and every later evaluation
/// returns the placeholder seeded below instead of the real segment. There is no
/// error — just a transform that silently ignores its input. Measured, flipping
/// this one word turns seven behaviour tests red while every sandbox test stays
/// green, which is why the guarantee is pinned by tests rather than by a comment.
///
/// # Errors
///
/// Returns the parse error, which carries a line and column.
pub fn compile(engine: &Engine, source: &str) -> Result<AST, ParseError> {
    let mut scope = Scope::new();
    scope.push(INPUT_VARIABLE, String::new());
    engine.compile_with_scope(&scope, source)
}
