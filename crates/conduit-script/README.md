# conduit-script

Operator-written utterance transforms, run on a sandboxed Rhai interpreter.

| Provider | Trait | Built from |
| --- | --- | --- |
| `Script` | `UtteranceTransform` | a script, a name, and a deadline in milliseconds |

`conduit-transform` ships three fixed rules — strip emoji, flatten markdown,
collapse whitespace. They cover what almost every pipeline wants, and nothing
else, because each one is a Rust function somebody had to write and release.
This crate is the other half of that trade: an operator writes the rewrite
themselves, saves it, and it applies to the next utterance.

It is a separate crate deliberately. An interpreter is a large dependency, and
folding it into `conduit-transform` would charge every consumer of the three
builtin rules for a scripting engine none of them asked for. A deployment that
wants only `StripEmoji` never compiles Rhai.

## Writing a Script

A script is an expression, or a block ending in one. The incoming segment is
bound to `segment`, and whatever the script evaluates to is the text that gets
rendered:

```rhai
segment.to_upper()
```

Returning `""` drops the segment. That is a normal outcome rather than a
failure — a sentence that was nothing but an emoji has no spoken form:

```rhai
if segment.contains("[inaudible]") { "" } else { segment }
```

Returning anything that is not a string is an error. There is deliberately no
lenient stringification: silently stringifying a number would let a typo become
spoken output.

### The mutating-method trap

Rhai's `replace` and `trim` mutate the string in place and return unit. So the
natural-looking one-liner does not work — it evaluates to `()` and fails the
turn:

```rhai
segment.replace("cat", "dog")    // WRONG: evaluates to ()
segment.trim()                   // WRONG: evaluates to ()
```

Bind first, mutate, then name the variable as the final expression:

```rhai
let out = segment;
out.replace("cat", "dog");
out
```

Methods that return a new string instead of mutating — `to_upper`, `to_lower`,
`sub_string` — chain the way you would expect:

```rhai
segment.to_upper()
```

### Available language

Arithmetic, logic, basic and extended string operations, basic iterators, and
basic arrays. Object maps and time are deliberately absent: maps buy nothing for
rewriting a string, and `timestamp` is nondeterminism, which makes a transform
unreviewable — the same segment would rewrite differently depending on the clock.

## Validation

`Script::check(source)` is the save-time check, so an operator learns about a
mistake while the script is still in front of them rather than on the first
utterance of the first conversation that reaches it.

It compiles against a scope pre-seeded with `segment`, and that detail is
load-bearing. `strict_variables` is on, so a *bare* compile of any script
mentioning `segment` fails with "Undefined variable" — a check written that way
would reject every correct script ever saved. Validation and execution must seed
the same variable name or they disagree about what is valid.

**What validation catches:** syntax errors, and undefined variables.

**What it does not catch:** unknown functions, unknown methods, and a wrong
return type. Rhai resolves calls at runtime, so `no_such_function(segment)`
compiles cleanly and fails on the first segment it sees. An operator saving a
script with a misspelled method name gets a green save and a failed turn, and
that is worth knowing before it happens.

## Sandbox

Running operator-supplied code inside the turn loop is only acceptable because
the interpreter is boxed in. The asymmetry that shapes every decision here:

> **An error ends one turn cleanly. A hang ends every turn on that pipeline,
> forever.**

Failing fast is therefore the entire point. Each guard below exists because
something specific defeats the others without it, and the numbers are measured
on this crate's dependency versions rather than assumed.

### `Engine::new_raw()`, never `Engine::new()`

`Engine::new()` registers the standard package and a **filesystem** module
resolver. Subtracting them afterwards does not work, and the sharpest
demonstration is `sleep`:

```
Engine::new() + disable_symbol("sleep"), 10k operation cap, 100ms deadline
  → sleep(3) blocked for 3.0055s and returned Ok(())

Engine::new_raw(), package never registered
  → sleep(3) failed in 87µs: "Function not found: sleep (i64)"
```

`disable_symbol("sleep")` **does not work**, because `sleep` is a *registered
function*, not a keyword — `disable_symbol` only affects the tokenizer. And a
sleeping thread performs no operations, so neither the operation counter nor the
progress hook is ever consulted. Not registering the package is the only thing
that removes it. The test asserting this carries a comment recording the same,
so nobody "simplifies" it back.

`Engine::new()` also grants real filesystem reach through `import`, which is
what the sandbox test's positive control proves:

```
stock Engine::new():   import "<tmp>/secret" as m; m::LEAKED  → Ok("filesystem reached")
                       eval("1 + 1")                          → Ok(2)
sandboxed engine:      import ...  → Syntax error: 'import' is a reserved keyword
                       eval(...)   → Syntax error: reserved keyword 'eval' is disabled
```

Without that control the test would prove nothing — it would pass just as
happily against an engine that never had the capability to begin with.

### Two timeout layers are not enough; three limits are needed

`max_operations` plus a wall-clock deadline still leaves a hole, because a
script can spend nearly all of its time *inside a single operation*:

```
loop { s += s; }   with max_string_size(0):
  100ms deadline  → terminated after 131ms, having seen 91 operations
  1000ms deadline → terminated after 1554ms
  2000ms deadline → terminated after 2502ms, PEAK RSS 9.1 GB
```

The deadline does eventually fire, but each doubling is one operation, so it is
only noticed *after* an allocation twice the size of the last one. The overshoot
is unbounded in memory even where it is bounded in time. With the limit in
place the same script fails in 68µs with "Length of string too large", at 3.7 MB
resident.

So `max_string_size` is set to a non-zero value and is **not an operator knob**.
`spawn_blocking` protects the reactor but cannot cancel the thread that is doing
the allocating, so once a script is inside one enormous operation there is
nothing left to stop it — the limit has to prevent the allocation rather than
interrupt it. An operator able to set this to zero could reintroduce the entire
failure. `max_array_size` is fixed for the same reason.

This is verified by mutation, not just by assertion: with `MAX_STRING_BYTES` set
to `0`, the string-bomb test's process is **SIGKILLed by the OS** — the outer
`tokio::time::timeout` wrapping the call cannot save it, which is precisely why
one timeout layer is not enough.

### Seed the input with `push`, never `push_constant`

The engine optimises at `Simple` level, which is `new_raw()`'s default. A
constant in the *compile-time* scope is const-folded into the AST, so every
later evaluation returns the validator's placeholder:

```
push_constant("segment", "placeholder") at compile, then real input
  → Ok("placeholder")        // wrong, and silent
push("segment", "placeholder") at compile, then real input
  → Ok("actual input")       // correct
```

This produces **no error at all** — just a transform that ignores its input from
then on. Flipping that one word in `sandbox::compile` turns seven behaviour tests
red while every sandbox test stays green, which is why statelessness is pinned by
tests rather than by a comment.

### The deadline is a thread-local

The deadline must be per *call*, but the engine is shared by every concurrent
call, so it cannot live on the engine — and the progress hook's signature is
fixed, so it cannot be passed in. `spawn_blocking` gives each evaluation a
thread to itself for its whole duration, which makes "the deadline for this
thread" and "the deadline for this call" the same thing.

It is installed through a guard whose `Drop` restores the previous value.
Blocking threads are pooled, so a deadline left set would kill whatever
evaluation next landed on that thread. A test runs four runaways and then eight
healthy calls to hold that line.

The hook fires once per operation, which is far too often to read a clock, so it
samples every 512 operations. That bounds overshoot rather than opening a hole:
the interpreter cannot get *stuck* between two samples, only briefly delayed.

### `sync` and `no_closure`

Both Rhai features are required, neither is a preference.

`sync` makes `Engine` and `AST` `Send + Sync`, without which one engine could
not be shared across turns at all.

`no_closure` removes shared closure captures. Under `sync` a captured variable
becomes `Arc<RwLock<Dynamic>>`, and self-reference through that lock is a hang
risk the deadline cannot break, because a thread blocked on a lock burns no
operations. Measured on a build *without* `no_closure`, the genuine
self-referential case is caught by Rhai's own detection —
`let f = (); f = |x| f.call(x); f.call(1)` fails in 60ms with "Data race
detected" rather than deadlocking — so the observed behaviour is better than
feared. `no_closure` is kept regardless: it removes the shared-capture machinery
outright instead of depending on that detection catching every shape, and closures
buy nothing for rewriting a string.

### Full posture

| Guard | Setting | Stops |
| --- | --- | --- |
| Raw engine | `Engine::new_raw()` | `sleep`, `exit`, filesystem module resolution |
| Curated packages | arithmetic, logic, string, iterator, array | everything else, by never registering it |
| No maps | `BasicMapPackage` omitted | nothing needed; reduces surface |
| No clock | `BasicTimePackage` omitted | nondeterministic, unreviewable transforms |
| Module resolver | `DummyModuleResolver` | `import` by path |
| Keywords | `disable_symbol("eval")`, `disable_symbol("import")` | runtime code generation no check can inspect |
| Module cap | `set_max_modules(0)` | a resolver reintroduced by mistake |
| Operation cap | 500,000 | scripts that loop many cheap times |
| String cap | 256 KiB, fixed | scripts that loop few expensive times |
| Array cap | 8,192 items, fixed | the same, for arrays |
| Call depth | 32 | recursion reaching the native stack, which aborts the process |
| Expression depth | 64 / 64 | parser stack exhaustion, refused at compile time |
| Strict variables | `set_strict_variables(true)` | misspelled variables, at save time |
| Output sinks | `on_print`, `on_debug` → discard | operator text forging log lines |
| Wall clock | per-call thread-local deadline | scripts that are slow without being busy |

## Errors

A script that fails takes the turn with it. It never falls back to passing the
input through, because passing it through would deliver exactly what the
operator configured the transform to prevent.

| Rhai failure | Conduit error | Logged at |
| --- | --- | --- |
| `ErrorTerminated` (deadline) | `Error::Timeout` | `warn` |
| `ErrorTooManyOperations` | `Error::Timeout` | `warn` |
| `ErrorRuntime` (`throw`) | `Error::Provider` | `error` |
| everything else | `Error::Provider` | `error` |

Both budget failures map to `Timeout` because they are the same fact to an
operator — the script ran away — and which guard noticed first is an
implementation detail. `EvalAltResult` is `Error + Send + Sync` under `sync`, so
it goes in as the error source unwrapped rather than stringified.

## Limits

- **Deadlines are 1ms to 5s.** A transform sits between a model finishing a
  sentence and a synthesizer starting to speak it, so its budget is a fraction of
  a turn. A longer cap would only widen the window in which a blocking thread is
  stuck.
- **`spawn_blocking` cannot be cancelled.** Returning an error to the caller
  does not stop the thread; the guards are what stop it. This is the whole
  reason `max_string_size` is fixed rather than configurable.
- **Compile-time checking is shallow.** Syntax and undefined variables only. An
  unknown function, an unknown method, or a wrong return type becomes a
  first-utterance turn failure.
- **One engine per provider.** Registering the same script twice builds two
  engines. They are cheap next to a conversation, and it keeps each provider's
  configuration independent.
