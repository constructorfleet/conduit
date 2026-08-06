# Pipeline Comparison On Fixtures

Implementation spec for [ADR-0018](../adr/0018-comparison-judged-by-agreement.md).

Runs one input through several stored pipelines and reports where they differed
and what each cost, so a choice between two recognizers, two engines, two models,
or streaming and batch is settled by measurement.

This is the fixture stage only. Shadow turns, turn capture, remote voice activity
detection and transforms, and the component protocol are separate specs.

---

## Problem Statement

An operator choosing between two ways to recognize speech has no way to compare
them. The questions are ordinary — is streaming worth it, is this engine better
than that one, does this need a GPU, should this run on the device — and every one
of them is currently settled by argument, because nothing in Conduit runs the same
input through two configurations and shows the difference.

The pieces to make the change are already there. A `Provider Definition` makes
swapping an implementation a configuration edit, and a `Pipeline Test Turn` runs a
pipeline once against the current `Runtime Provider Registry Snapshot`. What is
missing is the second pipeline and the comparison: a test turn reports one
pipeline's reply in isolation, with no per-component timing surfaced and nothing to
compare it against.

Two smaller gaps make the current test turn unusable for this even manually. It
accepts a typed utterance rather than recorded audio, and the synthetic input it
builds is the UTF-8 bytes of that string — which works only because a fake
recognizer ignores its input, and tells an operator nothing about how a real
recognizer handles a real voice. And its result reports total audio bytes and the
reply, but not how long any component took, so even running two test turns by hand
answers "what did they say" and never "what did they cost".

## Solution

An operator names several stored pipelines and supplies one input, and Conduit
runs each pipeline against the same input and returns one report: what each
pipeline produced, what each component within it cost, and whether the pipelines
agreed.

The referee is agreement between the pipelines, not a reference transcript.
Nothing is labelled. When every pipeline produces the same reply, the report says
so, and the choice collapses to cost — the faster, cheaper, or more locally-hosted
candidate wins outright and the question is closed. When they differ, the report
shows the differences, and those are the few cases worth listening to.

Input is either a typed utterance or a real audio fixture. A typed utterance keeps
the existing cheap path for structural comparison; recorded audio is what makes
comparing recognizers meaningful, because a recognizer can only be judged on
speech.

The report is normalized before comparison — case, punctuation, filler, numerals —
and the normalization applied is part of the report rather than hidden inside the
comparer, because it determines the disagreement rate and therefore the
conclusion.

## User Stories

1. As an operator, I want to run one input through several stored pipelines in one
   request, so that I compare configurations without running and correlating
   separate test turns by hand.
2. As an operator, I want to supply a real recorded audio fixture, so that I judge
   a recognizer on speech rather than on bytes it was never trained to read.
3. As an operator, I want to keep supplying a typed utterance instead, so that
   comparing structural or text-pipeline behavior costs nothing to set up.
4. As an operator, I want each pipeline in a comparison to receive byte-identical
   input, so that a difference in output is attributable to the pipelines and not
   to the fixture.
5. As an operator, I want the report to say whether the pipelines agreed, so that
   I can stop thinking about the question when they did.
6. As an operator, I want to see the exact text each pipeline produced when they
   disagreed, so that I can judge which one was right by reading or listening.
7. As an operator, I want to see the normalization that was applied before
   comparison, so that I know whether a reported disagreement is substantive or
   punctuation.
8. As an operator, I want per-component timing for every pipeline in the
   comparison, so that I know which component is responsible for a latency
   difference.
9. As an operator, I want timing attributed to the component that spent it rather
   than only to the turn, so that a slow recognizer and a slow synthesizer are
   distinguishable.
10. As an operator, I want a remote component's timing reported the same way an
    in-process component's is, so that a comparison can referee the very question
    of where to run something.
11. As an operator, I want to compare two pipelines that differ only in one
    provider definition, so that I isolate the effect of one engine, model, or
    location.
12. As an operator, I want to compare a streaming recognizer against a batch one,
    so that I learn whether streaming is worth its complexity in my deployment.
13. As an operator, I want a comparison to use the current
    `Runtime Provider Registry Snapshot`, so that the result describes the
    providers I actually have configured.
14. As an operator, I want a comparison that names a pipeline that does not exist
    to be refused clearly, so that I do not read a partial report as a complete
    one.
15. As an operator, I want a comparison in which one pipeline fails to prepare to
    still report the pipelines that did prepare, so that one bad candidate does
    not waste the whole run.
16. As an operator, I want a pipeline that failed to be shown as failed rather
    than as disagreeing, so that I do not read a crash as a recognition
    difference.
17. As an operator, I want the reason a pipeline failed, so that I can fix the
    candidate and re-run rather than guess.
18. As an operator, I want to know which component failed, so that a broken
    candidate points at its own cause.
19. As an operator, I want to compare pipelines that render different modalities,
    so that a text pipeline and a voice pipeline can be compared on what they
    said.
20. As an operator, I want to hear what each voice pipeline synthesized, so that I
    can judge synthesis quality, which no automatic comparison can score.
21. As an operator, I want a comparison to be refused when it names fewer than two
    pipelines, so that I do not mistake a test turn for a comparison.
22. As an operator, I want a bound on how many pipelines one comparison may name,
    so that one request cannot exhaust the deployment.
23. As an operator, I want to know whether the pipelines ran concurrently or in
    sequence, so that I know whether the timings competed for the same hardware.
24. As an operator, I want comparison timings not to be distorted by other
    candidates in the same run, so that a reported latency is one I could expect
    in production.
25. As an operator, I want a comparison to require no retention of what was
    compared, so that answering a question about recognizers does not archive
    speech.
26. As an operator, I want the comparison to reach every pipeline through the same
    authenticated management surface as the rest of configuration, so that it does
    not widen access.
27. As an operator, I want a comparison to identify each pipeline's conversation,
    so that I can open the full `Turn Reconstruction` for any candidate that looks
    wrong.
28. As an operator, I want the comparison report to be a typed contract, so that
    the operator console renders it without inventing structure.
29. As an operator, I want to be told when two pipelines agreed only because
    normalization erased their difference, so that I do not conclude equivalence
    from a lenient comparer.
30. As an operator, I want to be warned that agreement cannot prove correctness,
    so that I do not read two identically-wrong recognizers as two good ones.
31. As an operator, I want the comparison to refuse to referee two pipelines whose
    reasoning cores differ, or to mark that verdict as unreliable, so that I do
    not read two phrasings of the same right answer as a disagreement.
32. As an operator, I want a comparison to run against pipelines I have already
    stored rather than definitions I supply inline, so that what I measured is what
    I can deploy.
33. As a maintainer, I want comparison to compose the existing test-turn path
    rather than duplicate it, so that a change to how a turn runs cannot make
    comparison quietly describe a different runtime.
34. As a maintainer, I want per-component timing derived from the existing event
    bus, so that comparison and `Turn Reconstruction` cannot disagree about what
    happened.
35. As a maintainer, I want normalization and agreement to be pure functions, so
    that their many edge cases are tested directly instead of through HTTP.

## Implementation Decisions

**Surface.** One new management route, `POST /v1/pipelines/compare`, on the
management router beside the existing pipeline routes, requiring the same
management caller as `test-turn`. Comparison is configuration work, not
conversation work, so it does not touch the conversation surface.

**Request.** Names of two or more stored pipelines, one input, and an audio
format. The input is either a typed utterance or a base64 audio fixture, exactly
one of the two. Naming fewer than two pipelines is refused as unprocessable, as is
naming a pipeline that is not stored. There is a bound on how many pipelines one
request may name, and a request body limit consistent with the existing pipeline
write limit.

**Real audio input.** `PipelineTestRequest`'s synthetic input path builds a chunk
from the utterance's UTF-8 bytes, which is not audio. Comparison accepts a real
fixture as base64 and feeds its decoded samples to each pipeline, advertising the
declared format. A fixture whose format does not match what a provider accepts is
refused at preparation rather than resampled — consistent with
[ADR-0014](../adr/0014-voice-activity-detection-as-two-decisions.md), which
established that a fixed-window model given the wrong frame size is invalidated
rather than degraded, and that the refusal belongs where an operator sees it.

The same fixture bytes go to every pipeline. Decoding happens once, before any
pipeline runs, so a decode failure is one error rather than N identical ones.

**Reusing the test turn.** Each pipeline runs through the existing test-turn path:
store lookup, `Runner::prepare` against the current
`Runtime Provider Registry Snapshot`, format application, idle timeout, then
`run` or `run_text` according to whether the prepared runner expects audio.
Comparison adds no runtime mode and does not change `Runner`, `Plan`, or any
provider trait. A text pipeline and a voice pipeline are therefore comparable
without special handling, because choosing the input shape per pipeline is already
what the test turn does.

**Execution order.** Pipelines run in sequence by default, not concurrently, and
the report states which happened. Concurrency would let candidates contend for the
same CPU or the same GPU and report latencies nobody could reproduce in
production, which defeats the purpose. Sequential execution is the honest default;
concurrency may be requested when throughput matters more than fidelity.

**Per-pipeline isolation of failure.** A pipeline that fails to prepare or fails
while running is recorded as failed, with its reason and the component that
failed, and the remaining pipelines still run. The overall request succeeds as
long as it was well-formed. A failed pipeline is excluded from the agreement
verdict rather than counted as disagreeing.

**Timing.** Per-component timing is derived by subscribing to the event bus for
each pipeline's conversation, which is the same source `Turn Reconstruction`
consumes per [ADR-0010](../adr/0010-server-owned-turn-reconstruction.md). No new
instrumentation, and no new events. Timings are attributed by the existing
`Stage` vocabulary, so a remote component's steps — which arrive as data and are
emitted by the runtime per
[ADR-0016](../adr/0016-component-location-as-a-definition-axis.md) — appear
identically to an in-process component's. Comparison must not show less about a
remote candidate than a local one.

**Normalization and agreement.** A pure normalization function over a reply's
text, and a pure agreement function over the normalized replies. Normalization
covers case, surrounding and repeated whitespace, terminal and internal
punctuation, and numeral spelling; the set of rules applied is named in the
report. Agreement is computed over normalized text and reports both the
normalized comparison and whether the raw texts were byte-identical, so an
operator can tell equivalence from lenience.

**Verdict reliability.** The verdict carries a reliability marker. Comparing
pipelines whose reasoning cores differ is marked unreliable, because two models
phrase the same correct answer differently and equality is not meaningful there —
this is stated in ADR-0018 and must be visible in the report rather than
implicit. Comparing pipelines that differ only ahead of the core is reliable.

**Report.** A typed result per [ADR-0006](../adr/0006-typed-ui-api-contract.md),
carrying, per pipeline: its name, its conversation id, its status, its reply text,
its synthesized audio as a playable container when it spoke, per-component
timings, and its failure reason when it failed. Plus, once: the agreement verdict,
its reliability, the normalization rules applied, and whether execution was
sequential. Synthesized audio is packaged and base64-encoded as the existing test
turn already does, because judging synthesis quality is something only a person
listening can do.

**Conversation ids are load-bearing.** Each pipeline's conversation id is in the
report so an operator can open the full `Turn Reconstruction` for a candidate that
looks wrong. The comparison report is a summary; the reconstruction is the detail,
and comparison does not duplicate it.

**No retention.** Comparison retains nothing beyond the response. `Turn Capture`
is a separate, opt-in feature, and a comparison must be usable with capture off.

## Testing Decisions

A good test here asserts what an operator can observe through the route: the
report's shape, the verdict, which pipelines ran, and what happened when one
failed. It does not assert how timings were collected, how many event subscribers
existed, or the internal structure of the comparer.

**Primary seam: the HTTP route,** tested in `crates/conduit-api/tests/pipelines.rs`
beside the existing test-turn tests. That file already has the harness this needs —
request builders, an authenticated call helper, provider-definition seeding, and
mock provider servers — so comparison tests are additions rather than new
scaffolding. Driving comparison through the router exercises store lookup,
snapshot resolution, preparation, execution, and reporting together, which is the
highest available seam and an existing one.

Cases to cover: two pipelines agreeing; two disagreeing, with the differing texts
present; a request naming one pipeline refused; a request naming an unknown
pipeline refused; one candidate failing to prepare while the others still report;
one candidate failing mid-turn, reported as failed rather than disagreeing and
excluded from the verdict; a text pipeline compared against a voice pipeline; a
real audio fixture accepted; a fixture in a format a provider refuses rejected at
preparation; a differing-core comparison marked unreliable; per-component timings
present for every stage that ran; conversation ids present and resolvable against
the turns surface; the pipeline count bound enforced; the request body limit
enforced; and the route refused without a management caller, following the
existing pattern in `crates/conduit-api/tests/auth.rs`.

**Secondary seam: normalization and agreement as pure functions,** tested
directly. These carry the subtle correctness — every case difference, punctuation
variant, filler word, and numeral spelling is a separate assertion — and proving
them only through HTTP would make table-driven coverage painfully slow. This is
the one place a lower seam is accepted, and it is justified by the number of cases
rather than by convenience.

**Contract coverage.** The report is a typed contract, so it belongs in
`crates/conduit-api/tests/frontend_contract.rs` alongside the other generated
bindings. `IMPLEMENTATION_GAPS.md` records that a checked-in status fixture drifted
from the real response shape and undermined ADR-0006's guarantee; the comparison
fixture must be generated or asserted against the real shape rather than
hand-maintained, so it cannot drift the same way.

**Prior art.** `crates/conduit-runtime/tests/capture.rs` is the model for
event-derived assertions — it drains a subscription until the turn ends and asserts
over the whole sequence, which is the shape timing collection needs.
`crates/conduit-api/tests/turns.rs` covers the reconstruction surface the report's
conversation ids point into. `crates/conduit-runtime/tests/fakes` supplies the fake
recognizer, model, and synthesizer that make agreement and disagreement scriptable
without a real provider, including the failure injection the failed-candidate cases
need.

**One tripwire worth adding.** A test asserting that a failing candidate is
reported as failed and is *absent* from the agreement verdict. Counting a crash as
a disagreement is the failure mode that makes a comparison report actively
misleading, and it is the kind of thing a later refactor silently reintroduces —
the same reasoning that keeps `nothing_the_runtime_does_reports_barge_in` in the
runtime tests.

## Out of Scope

- **Shadow turns and real traffic.** ADR-0017's shadowing, tool-intent capture,
  and synthetic tool results are the second slice and their own spec.
- **Turn capture and retention.** Nothing is retained by this spec.
- **Labelling.** ADR-0018 offers manual labelling on disagreements; this spec
  reports disagreements and does not collect labels or store a corpus.
- **Word error rate and reference-judged scoring.** Deferred with labelling, not
  foreclosed by it.
- **Remote voice activity detection and transforms.** The two missing remote
  variants belong to ADR-0016's slice. Comparison must report remote candidates
  correctly, which it gets from the existing event vocabulary, but it does not add
  the variants.
- **The component protocol.** ADR-0016's plugin seam is separate work.
- **A Conduit transport protocol.** Deferred to measurement per ADR-0016, and this
  spec builds the instrument that would inform it.
- **Adding recognition engines.** Engine choice is already an environment variable
  on `services/wyoming-asr` and a requirements split on `services/speaker-id`;
  adding an engine is a change there, not in Rust.
- **Operator console rendering.** The typed contract is in scope; the comparison
  view is frontend work owned elsewhere.
- **Comparing reasoning cores as a supported verdict.** The report marks such a
  comparison unreliable rather than refereeing it.

## Further Notes

The reason to build this before anything else in the group is that three of the
four questions that prompted the work — streaming or batch, which engine, CPU or
GPU — are answerable entirely in process, with providers this repository already
has. Only "on the device or on a hypervisor" needs a remote seam, and distributing
an implementation before knowing which one won is work spent on a losing
candidate.

Agreement refereeing has a limit that the report should state rather than bury:
two recognizers that share a training corpus share its mistakes, and this method
reports those as agreement. When that matters, the answer is a labelled subset, and
the disagreement-labelling path deferred above is how one gets built.

`docs/api.md` does not currently document the turn-reconstruction surface, and
`docs/adr/README.md` still claims no ADRs are recorded — both noted in
`IMPLEMENTATION_GAPS.md`. Per `AGENTS.md`, documentation is part of the
implementation, so this route is documented in `docs/api.md` when it lands. Fixing
the two pre-existing gaps is not this spec's job, but the new route must not add a
third.
