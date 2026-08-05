//! Tests for the script transform, weighted towards the sandbox.
//!
//! The sandbox tests are the ones that matter: a transform that rewrites text
//! correctly but can be made to hang is worse than no transform at all. Several
//! of them assert on the *kind* of failure rather than merely that one occurred,
//! because each kind names the specific guard that caught it — a test that only
//! checked for `Err` would keep passing while somebody removed a limit and left
//! a slower one to catch the fallout.

use std::time::Duration;

use super::*;

/// The wall-clock ceiling every runaway test wraps itself in.
///
/// A regression here would otherwise hang CI instead of failing it. Comfortably
/// above the script deadlines under test and comfortably below any CI timeout.
const OUTER_LIMIT: Duration = Duration::from_secs(5);

fn script(source: &str) -> Script {
    Script::new("rewrite", source, 100).expect("script compiles")
}

async fn rewritten(source: &str, segment: &str) -> Result<String> {
    script(source).transform(segment).await
}

// ---------------------------------------------------------------------------
// Behaviour
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_script_returning_its_input_leaves_the_segment_alone() {
    assert_eq!(rewritten("segment", "Hello there").await.expect("succeeds"), "Hello there");
}

#[tokio::test]
async fn a_script_rewrites_the_segment_it_is_given() {
    assert_eq!(rewritten("segment.to_upper()", "quiet").await.expect("succeeds"), "QUIET");
}

#[tokio::test]
async fn a_script_returning_an_empty_string_drops_the_segment() {
    // Empty is a legal, meaningful result, not a failure: it is how a transform
    // says the segment has no spoken form.
    let source = r#"if segment.contains("[inaudible]") { "" } else { segment }"#;
    assert_eq!(rewritten(source, "well [inaudible] yes").await.expect("succeeds"), "");
    assert_eq!(rewritten(source, "well yes").await.expect("succeeds"), "well yes");
}

#[tokio::test]
async fn a_script_that_throws_fails_the_segment_rather_than_passing_it_through() {
    let error = rewritten(r#"throw "refusing this segment""#, "anything")
        .await
        .expect_err("a throw fails the turn");
    assert!(
        matches!(error, Error::Provider { .. }),
        "expected a provider error, got {error:?}"
    );
    // The point of failing: the input must not be what the caller receives.
    assert!(!error.to_string().is_empty());
}

#[tokio::test]
async fn a_script_returning_a_non_string_fails_rather_than_being_stringified() {
    // Lenient stringification would let a typo become spoken output: a script
    // that accidentally evaluates to a number would have that number spoken.
    for source in ["1 + 1", "()", "true", "[segment]"] {
        match rewritten(source, "hello").await {
            Ok(text) => {
                panic!("`{source}` must not produce a string, but it returned {text:?}")
            }
            Err(error) => assert!(
                matches!(error, Error::Provider { .. }),
                "`{source}` should be a provider error, got {error:?}"
            ),
        }
    }
}

#[tokio::test]
async fn a_transform_reports_the_name_it_was_registered_under() {
    assert_eq!(Provider::name(&script("segment")), "rewrite");
    assert_eq!(script("segment").with_label("Tidy up").descriptor().label, "Tidy up");
}

#[tokio::test]
async fn the_documented_mutating_idiom_is_the_one_that_works() {
    // Rhai's `replace` and `trim` mutate in place and return unit, so the
    // one-liner everybody writes first evaluates to `()` and fails. The README
    // spells out the binding form; this pins both halves of that claim.
    let naive = rewritten(r#"segment.replace("cat", "dog")"#, "one cat").await;
    assert!(naive.is_err(), "the in-place method returns unit, so this must fail");

    let working = r#"let out = segment; out.replace("cat", "dog"); out"#;
    assert_eq!(rewritten(working, "one cat").await.expect("succeeds"), "one dog");

    let trimming = "let out = segment; out.trim(); out";
    assert_eq!(rewritten(trimming, "  padded  ").await.expect("succeeds"), "padded");
}

#[tokio::test]
async fn identity_scripts_preserve_every_kind_of_segment_exactly() {
    let long = "word ".repeat(2_000);
    let cases = [
        "",
        "plain ascii",
        "unicode: naïve café résumé Ω≈ç√",
        "emoji: 👋🏽 🎉 👨‍👩‍👧‍👦 🇳🇿",
        r#"embedded "double" and 'single' quotes"#,
        "backslash \\ and newline\nand tab\t",
        long.as_str(),
    ];
    for segment in cases {
        assert_eq!(
            rewritten("segment", segment).await.expect("identity succeeds"),
            segment,
            "identity changed a segment"
        );
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

#[test]
fn save_time_validation_accepts_a_script_that_reads_the_input_variable() {
    // The regression this exists for: `strict_variables` makes a *bare* compile
    // of any script mentioning the input fail with "Undefined variable". A
    // `check` written that way would reject every correct script ever saved, so
    // it must seed the same variable the runtime seeds.
    Script::check("segment").expect("reading the input must validate");
    Script::check("segment.to_upper()").expect("a method on the input must validate");
    Script::check(r#"if segment == "" { "" } else { segment }"#)
        .expect("branching must validate");

    let error = Script::check("mystery").expect_err("an unknown variable must not validate");
    assert!(
        error.to_string().to_lowercase().contains("variable"),
        "the error should name the undefined variable, got: {error}"
    );
}

#[test]
fn save_time_validation_and_registration_agree_on_what_compiles() {
    for source in ["segment", "segment.to_upper()", r#"segment + "!""#] {
        assert!(Script::check(source).is_ok(), "`{source}` should validate");
        assert!(Script::new("t", source, 100).is_ok(), "`{source}` should register");
    }
    for source in ["segment +", "let = 1", "if { }"] {
        assert!(Script::check(source).is_err(), "`{source}` should not validate");
        assert!(Script::new("t", source, 100).is_err(), "`{source}` should not register");
    }
}

#[test]
fn validation_catches_syntax_errors_but_not_unknown_functions() {
    // An honest limit, documented in the README because it decides what an
    // operator sees: a call to something that does not exist compiles fine and
    // fails on the first utterance instead.
    Script::check("no_such_function(segment)").expect("an unknown function still compiles");
    Script::check("segment.no_such_method()").expect("an unknown method still compiles");
    Script::check("1 + 1").expect("a wrong return type still compiles");
}

#[tokio::test]
async fn an_unknown_function_fails_the_turn_on_the_first_segment() {
    let error = rewritten("no_such_function(segment)", "hello")
        .await
        .expect_err("an unknown function fails at runtime");
    assert!(
        matches!(error, Error::Provider { .. }),
        "expected a provider error, got {error:?}"
    );
}

#[test]
fn a_deadline_outside_the_supported_range_is_refused_at_registration() {
    assert!(Script::new("t", "segment", 0).is_err(), "a zero deadline must be refused");
    let too_long = u64::try_from(MAX_TIMEOUT.as_millis()).expect("fits") + 1;
    assert!(Script::new("t", "segment", too_long).is_err(), "an over-long deadline is refused");
    assert!(Script::new("t", "segment", 100).is_ok(), "a sane deadline is accepted");
}

// ---------------------------------------------------------------------------
// Sandbox: the required guarantees
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_runaway_script_fails_the_segment_rather_than_hanging() {
    let script = Script::new("runaway", "loop { }", 100).expect("compiles");

    let started = std::time::Instant::now();
    // The outer timeout is the difference between a regression that fails CI
    // and a regression that wedges it. It must not be what stops this script.
    let outcome = tokio::time::timeout(OUTER_LIMIT, script.transform("hello")).await;
    let elapsed = started.elapsed();

    let result =
        outcome.expect("the outer timeout must not fire: the sandbox should have stopped");
    let error = result.expect_err("a runaway script must fail the segment");
    assert!(
        matches!(error, Error::Timeout { .. }),
        "a runaway must be reported as a timeout, got {error:?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "the sandbox took too long to give up: {elapsed:?}"
    );
}

#[tokio::test]
async fn the_script_cannot_touch_the_filesystem() {
    // A real file for the script to reach for, so a passing sandbox is proving
    // it declined something that was actually there.
    let dir = std::env::temp_dir().join(format!("conduit-script-fs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let module = dir.join("secret.rhai");
    std::fs::write(&module, "export const LEAKED = \"filesystem reached\";\n").expect("write");
    let importable = module.with_extension("");

    let attempts = [
        format!(r#"import "{}" as m; m::LEAKED"#, importable.display()),
        r#"eval("1 + 1")"#.to_owned(),
        r#"open_file("/etc/passwd")"#.to_owned(),
        r#"read_file("/etc/passwd")"#.to_owned(),
    ];

    // POSITIVE CONTROL. Without this the test proves nothing: `import` is the
    // only one of these a stock engine actually grants, and if it did not, the
    // sandbox assertions below would pass against an engine that never had the
    // capability in the first place.
    let stock = rhai::Engine::new();
    let mut stock_scope = rhai::Scope::new();
    stock_scope.push(INPUT_VARIABLE, String::from("hello"));
    let control = stock.eval_with_scope::<rhai::Dynamic>(&mut stock_scope, &attempts[0]);
    assert_eq!(
        control.expect("a stock engine reads the filesystem").to_string(),
        "filesystem reached",
        "positive control failed: a stock engine must reach the file, or this test is vacuous"
    );
    let control_eval = stock.eval::<i64>(&attempts[1]);
    assert_eq!(
        control_eval.expect("a stock engine evaluates code"),
        2,
        "positive control failed"
    );

    for attempt in &attempts {
        let error = match Script::new("escape", attempt, 100) {
            // Refused at compile time, which is the better outcome.
            Err(error) => error,
            // Compiled, so it must fail when it runs.
            Ok(script) => match script.transform("hello").await {
                Ok(text) => panic!("the sandbox allowed `{attempt}`, returning {text:?}"),
                Err(error) => error,
            },
        };
        assert!(
            !error.to_string().contains("filesystem reached"),
            "`{attempt}` leaked file contents into the failure: {error}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_string_bomb_fails_with_a_data_size_error_rather_than_exhausting_memory() {
    // The companion that would catch someone making `max_string_size` a knob
    // and an operator setting it to zero. Each doubling is a single operation,
    // so the operation cap and the progress hook are both bystanders here;
    // measured without this limit, a 2s deadline was honoured only after the
    // process reached 9.1 GB resident.
    let script =
        Script::new("bomb", r#"let s = segment; loop { s += s; }"#, 100).expect("compiles");

    let outcome = tokio::time::timeout(OUTER_LIMIT, script.transform("x")).await;
    let error =
        outcome.expect("the outer timeout must not fire").expect_err("the bomb must fail");

    // Specifically the size limit, not the deadline: if this becomes a timeout,
    // the string cap has stopped doing its job and the memory is unbounded
    // again even though the test still sees an error.
    let detail = format!("{error:?} {error}").to_lowercase();
    assert!(
        detail.contains("too large"),
        "the string size limit should have caught this, got: {error}"
    );
}

#[tokio::test]
async fn unbounded_recursion_fails_with_a_stack_overflow_error() {
    // Native stack exhaustion aborts the process rather than failing a turn, so
    // the call-level cap has to catch this before the stack does.
    let script = Script::new("recurse", "fn f(n) { f(n + 1) } f(0)", 100).expect("compiles");

    let outcome = tokio::time::timeout(OUTER_LIMIT, script.transform("hello")).await;
    let error =
        outcome.expect("the outer timeout must not fire").expect_err("recursion must fail");

    let detail = format!("{error:?} {error}").to_lowercase();
    assert!(
        detail.contains("stack overflow"),
        "the call level limit should have caught this, got: {error}"
    );
}

#[tokio::test]
async fn sleep_is_absent_from_the_language_rather_than_merely_disabled() {
    // `disable_symbol("sleep")` DOES NOT achieve this. `sleep` is a registered
    // function, not a keyword, so disabling the symbol leaves it callable:
    // measured, `sleep(3)` on a stock engine with the symbol disabled blocked
    // for 3.0055s and returned Ok, with both a 10k operation cap and a 100ms
    // deadline in force. A sleeping thread performs no operations, so no guard
    // is ever consulted. Only never registering the package removes it, which
    // is what `Engine::new_raw()` plus a curated package set achieves.
    let script = Script::new("napper", "sleep(1); segment", 100).expect("compiles");

    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(OUTER_LIMIT, script.transform("hello")).await;
    let elapsed = started.elapsed();
    let error = outcome.expect("the outer timeout must not fire").expect_err("sleep must fail");

    let detail = format!("{error:?} {error}").to_lowercase();
    assert!(
        detail.contains("function not found"),
        "sleep should not exist at all, got: {error}"
    );
    assert!(elapsed < Duration::from_millis(500), "it slept before failing: {elapsed:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_runaway_script_does_not_stall_the_reactor() {
    // Two workers, so a runaway evaluated on the reactor would occupy half of
    // them and be plainly visible in the tick count. `spawn_blocking` is what
    // keeps the async side moving.
    let script = Script::new("runaway", "loop { }", 200).expect("compiles");
    let runaway = tokio::spawn(async move { script.transform("hello").await });

    let mut ticks = 0_u32;
    let mut ticker = tokio::time::interval(Duration::from_millis(10));
    ticker.tick().await; // The first tick is immediate.
    for _ in 0..20 {
        ticker.tick().await;
        ticks += 1;
    }

    assert_eq!(ticks, 20, "the reactor missed ticks while a script ran away");
    let error = runaway.await.expect("the task ran").expect_err("the runaway still failed");
    assert!(matches!(error, Error::Timeout { .. }), "expected a timeout, got {error:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_shared_script_serves_concurrent_calls_with_distinct_results() {
    // The statelessness guarantee under load: one provider serves every turn in
    // every pipeline at once, so a shared scope or a folded constant would show
    // up here as answers belonging to somebody else's conversation.
    let script = Arc::new(script(r#"segment + " ok""#));

    // Spawned up front so all 20 are in flight before any is awaited.
    let calls = (0..20)
        .map(|index| {
            let script = Arc::clone(&script);
            tokio::spawn(
                async move { (index, script.transform(&format!("call-{index}")).await) },
            )
        })
        .collect::<Vec<_>>();

    let mut answered = 0;
    for call in calls {
        let (index, result) = call.await.expect("the task ran");
        assert_eq!(
            result.expect("every concurrent call succeeds"),
            format!("call-{index} ok"),
            "call {index} got another call's answer"
        );
        answered += 1;
    }
    assert_eq!(answered, 20, "not every concurrent call was accounted for");
}

#[tokio::test]
async fn two_sequential_calls_on_one_script_are_independent() {
    // The scope-freshness guard. Two ways this breaks: a scope reused between
    // calls would let the first call's `let` leak into the second, and seeding
    // the input with `push_constant` instead of `push` would fold the first
    // value into the AST — producing no error at all, just a transform that
    // silently ignores its input from then on.
    let script = script("let seen = segment; seen");
    assert_eq!(script.transform("first").await.expect("succeeds"), "first");
    assert_eq!(script.transform("second").await.expect("succeeds"), "second");
    assert_eq!(script.transform("third").await.expect("succeeds"), "third");
}

#[tokio::test]
async fn a_thread_that_ran_a_runaway_script_still_serves_later_calls() {
    // The deadline lives in a thread-local on a pooled blocking thread. Left
    // set, it would kill whatever evaluation next landed on that thread; this
    // is what `Deadline`'s `Drop` is for.
    let runaway = Script::new("runaway", "loop { }", 50).expect("compiles");
    for _ in 0..4 {
        let error = tokio::time::timeout(OUTER_LIMIT, runaway.transform("hello"))
            .await
            .expect("the outer timeout must not fire")
            .expect_err("the runaway fails");
        assert!(matches!(error, Error::Timeout { .. }), "expected a timeout, got {error:?}");
    }

    let healthy = script("segment.to_upper()");
    for _ in 0..8 {
        assert_eq!(
            healthy.transform("still here").await.expect("a later call must still succeed"),
            "STILL HERE",
            "a stale deadline leaked onto a pooled thread"
        );
    }
}

#[tokio::test]
async fn the_operation_budget_stops_a_script_that_is_busy_rather_than_slow() {
    // A loop that finishes quickly per iteration but never in budget: this is
    // the guard that fires when the clock has not run out yet.
    let script =
        Script::new("busy", "let n = 0; while n < 100000000 { n += 1; } segment", 5_000)
            .expect("compiles");

    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(OUTER_LIMIT, script.transform("hello")).await;
    let error =
        outcome.expect("the outer timeout must not fire").expect_err("the budget runs out");

    assert!(matches!(error, Error::Timeout { .. }), "expected a timeout, got {error:?}");
    // The operation cap, not the 5s deadline, is what caught it.
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the operation cap should have fired long before the deadline: {:?}",
        started.elapsed()
    );
}
