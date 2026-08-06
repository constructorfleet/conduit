# Comparison Judged By Agreement, Labelled Only On Disagreement

**Pipeline comparison** runs one input through several pipelines and reports
where they differed and what each cost. The referee is **agreement between
candidates**, not a ground truth: no transcript is labelled by default, and
manual labelling is offered only for the inputs where the candidates disagreed.

This is the feature the rest of this work exists to serve. Provider definitions
already make swapping an implementation a configuration edit, and
[ADR-0016](0016-component-location-as-a-definition-axis.md) makes location one
more field on that edit — but nothing in the repository runs one input through two
configurations and shows the difference, so every choice between them is currently
settled by argument. `Pipeline Test Turn` runs *a* turn through *a* pipeline
successfully; it has no notion of a second pipeline, no diff, and no per-component
timing surfaced for the comparison a person is actually making.

**Agreement rather than truth**

Scoring against a reference transcript is the obvious design and needs something
this deployment does not have: labelled audio in the right voices on the right
hardware. Two ways to get one were considered.

Labelling by hand is real ground truth and is the reason most private corpora
stop at twenty samples. Using a large reference model as judge — a cloud API, or
`whisper-large` offline — automates it and buys a third opinion that is also
unverifiable, while quietly biasing every result toward whichever candidate most
resembles the reference. Neither is worth paying for up front.

Agreement needs no labels and answers the question that was actually asked. If
two recognizers produce the same transcript for 95% of real household utterances,
then the cheaper, faster, or more locally-hosted one wins outright and the
question is closed — no truth required to conclude that. All the information is in
the 5%, which is small enough to listen to. Labelling is therefore offered
exactly there, where each label is decisive rather than one sample among
hundreds.

The limitation is stated rather than hidden: agreement cannot detect that both
candidates are wrong the same way. Two recognizers sharing a training corpus will
share its mistakes, and this method will report them as agreement. When that
matters, a labelled subset is the answer, and the disagreement-labelling path is
already the mechanism for building one.

**Fixtures first, real traffic second**

Comparison lands in two stages, and the first needs no distribution and no
capture at all: one recorded sample, several stored pipelines, a report of
per-component latency and where the outputs differed. Every question that
prompted this work — streaming or batch, one engine or another, CPU or GPU — is
answerable there, in process, with providers this repository already has. Only
"on the device or on a hypervisor" needs a remote seam, and that is the question
most safely deferred, because distributing an implementation before knowing which
implementation won is work spent on a losing candidate.

Real traffic follows, as the shadow turns of
[ADR-0017](0017-shadow-turns-capture-tool-intent.md). Shadowing is the better
signal — real utterances, real room, real microphone — and it is also what
*produces* the corpus that labelling and fine-tuning need, which is why the two
are one design rather than two features. It carries the retention and consent
surfaces, so it is the larger and riskier slice and goes second.

**Consequences**

- Comparison is built on `Pipeline Test Turn` rather than beside it. The
  machinery for running a synthetic turn against the current
  `Runtime Provider Registry Snapshot` exists; what is added is running several
  and diffing them.
- The report needs per-component timing, which means comparison consumes the same
  runtime events as `Turn Reconstruction` rather than instrumenting separately.
  A remote component's steps arrive as data per
  [ADR-0016](0016-component-location-as-a-definition-axis.md) and appear in the
  report the same way an in-process component's do — a comparison must not show
  less about a remote candidate than a local one, or it cannot referee the very
  question it exists for.
- Diffing transcripts requires a normalization policy — case, punctuation,
  filler, numerals — and that policy determines the disagreement rate. It is a
  visible part of the report, not an implementation detail buried in a comparer.
- A comparison of pipelines whose reasoning cores differ is not decidable by
  agreement: two models phrase the same correct answer differently. Agreement
  refereeing applies to recognition and to transforms, where equality is
  meaningful. Comparing cores needs the labelled path, or a human reading the
  pair.
- Labelling produces the labelled corpus that a reference-judged or WER-scored
  comparison would need later. This decision defers that scoring; it does not
  foreclose it.
- Comparison must be usable with capture off. Answering a question about
  recognizers cannot require retaining household speech.

**Open questions**

- Is one sample per comparison enough to decide, or does the fixture stage need a
  small fixed set before its results are trustworthy? A single utterance can make
  two equivalent recognizers look different.
- What counts as agreement for a transform, whose output is a rewrite of a rewrite?
  Equality is defensible there and may be too strict.
- Does a comparison report persist, or is it read once and discarded? Persisting
  it is how a decision made six months ago stays explainable, and it is also
  retained output derived from possibly-unretained input.
