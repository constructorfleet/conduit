# Shadow Turns Capture Tool Intent And Never Invoke Tools

A **shadow turn** runs a candidate pipeline over real captured audio beside the
live pipeline, all the way through its reasoning core, and no satellite ever
hears its output. When that core requests a tool, the request is recorded with
its arguments and answered with a synthetic result. **No tool a shadow turn asks
for is ever invoked.**

**Why the whole pipeline, and not just recognition**

Comparing recognizers needs only the input path, and stopping there would have
been the cheaper decision with no side-effect story to get wrong. The reason to
go further is that the retained record is wanted as training data as well as as
evidence: what the model decided to do with a real utterance, and whether that
decision was right, is the part of a turn that no fixture produces. A comparison
that stops at the transcript cannot answer whether a different model reasons
better about the things actually said in this house.

That makes the tool boundary the whole risk. A shadowed turn's tool calls are, by
default, real calls — the same mail sent twice, the same light toggled twice, the
same purchase made twice — and a comparison feature is not worth a single one of
those.

**Why intent rather than a safety declaration**

The alternative considered was declaring tools read-only, per trait or per
definition, and letting a shadow turn invoke the ones declared safe. It is
rejected. It puts the guarantee in an annotation that a human or a third-party
MCP server has to get right, on a surface — `conduit-mcp` reaches tools over
stdio, streamable HTTP, and SSE, and the tools on the far end are not Conduit's —
where Conduit cannot verify the claim. One mis-declared tool and the failure is
external and irreversible.

Refusing to invoke anything is a guarantee rather than an audit. It also happens
to capture the thing wanted: for training, the valuable record is *what the model
chose to call and with what arguments*, which is the intent, not the result.

The cost is real and is accepted. A synthetic result means the candidate's
reasoning diverges from reality after the first tool round: round one's intent is
faithful, and everything the model concludes from a fabricated answer is
fabricated too. Multi-round tool reasoning is therefore not comparable from
shadow turns. Comparing that requires live capture, which is why capture is
enabled independently for live and shadow turns rather than being a shadow-only
feature — a live turn's tool results are real and its reasoning does not diverge,
so live turns are the better training data and shadow turns are the safer
comparison.

**Capture is opt-in, bounded, and visible**

Turn capture is off until an operator enables it, per pipeline, separately for
live and for shadow turns. Retention is the operator's choice by count or age,
with keeping everything available as an explicit selection rather than as what
happens when nobody chooses.

The bound is not administrative tidiness. An operator who enabled shadowing to
settle a question about recognizers did not ask for a permanent archive of
household speech, and a local-first appliance with no bound is how a disk fills
with recordings nobody decided to keep. Making the choice explicit at enable time
separates the two intents: a short bound to decide something, unbounded when a
corpus is the actual goal.

This is the same hazard the product already handles rather than a new one, and it
has to obey the existing rules or it contradicts them.
[ADR-0010](0010-server-owned-turn-reconstruction.md) established `Sensitive Tool
Evidence` — tool arguments and results kept for diagnostics but omitted from the
default operator view — and `Diagnostic Payload Access` as the higher-trust path
that reaches them, with redaction. A capture corpus is precisely that payload,
retained deliberately and for longer, so captured tool intents and their
arguments are sensitive tool evidence and reachable only through diagnostic
payload access. Capture changes the retention, not the classification.

Conduit is local-first, and that is what makes this defensible: the corpus never
has to leave the operator's hardware. Conduit's obligation is that capture is
never silent, that what is held is visible, and that it can be deleted.

**Consequences**

- A shadow turn resolves a plan whose tool bindings are non-invoking. This is a
  runtime execution mode, not a provider variant: a `Tool` provider is not
  configured differently for shadowing, it is not called.
- Shadow tool intents are recorded with arguments and classified as sensitive
  tool evidence per [ADR-0010](0010-server-owned-turn-reconstruction.md), so they
  are omitted from ordinary turn reconstruction views.
- A shadow turn's utterance is never rendered to a satellite. Synthesis may still
  run when synthesis latency is what is being compared, and its audio is
  discarded.
- Shadow turns cost real language model calls. A candidate pipeline pointed at a
  metered provider bills for every live utterance, which is an operator-facing
  cost that has to be stated where shadowing is enabled.
- Capture defaults to off, and configuring a shadow candidate does not enable it.
  A latency experiment must not silently begin recording speech, and the
  comparison in [ADR-0018](0018-comparison-judged-by-agreement.md) must be usable
  without retaining anything.
- The operator console gains a surface listing what capture holds, with deletion.
  A retention bound that cannot be inspected is not a bound.
- `Turn Status` is unchanged. A shadow turn reaches the same terminal outcomes;
  it is attributed to a candidate rather than modelled as its own status, which
  follows [ADR-0010](0010-server-owned-turn-reconstruction.md)'s handling of
  interruption as a reason rather than a status.
- Whoever is talking to the satellite is not necessarily the operator who
  enabled capture. Conduit surfaces the state and makes it deletable; the
  disclosure is the operator's, and Conduit does not pretend to make it.

**Open questions**

- Does a shadow turn's synthetic tool result claim success, claim failure, or say
  it is synthetic? Each teaches the candidate model something different, and a
  corpus built on one is not comparable with a corpus built on another.
- Does captured audio share storage with `Raw Event Evidence`, or is it its own
  store? Audio is much larger and its retention bound is the one that matters.
- Can a satellite be excluded from capture? A guest in the house is the case a
  per-pipeline switch does not express.
