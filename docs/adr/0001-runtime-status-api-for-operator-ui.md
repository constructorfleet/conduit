# Runtime Status API For Operator UI

Conduit's operator UI will use a first-class runtime status API as the initial
source of truth for pipeline and satellite state, then apply live event updates
from the event stream. Relying only on `/v1/events` would make refreshes and
late subscribers lose connected-satellite and recent-activity context, while a
status API gives the UI a coherent snapshot before it becomes event-driven.

The shared response contract lives in `conduit_api::status` as
`OperatorStatusSnapshot`. The `/v1/status` route returns that shape rather than
inventing per-screen status objects. The snapshot groups runtime launch state,
Pipeline Health, Component Health, Provider Status, Satellite status, active
turns, recent failures, and the event stream rules that describe how live
events update snapshot resources.

The status API is a management route. Device tokens cannot read it, because it
contains the same kind of operator-level operational state as the event stream.
Anonymous access is allowed only when the deployment explicitly enables
anonymous mode.

Pipeline Health projection depends on runtime events carrying explicit pipeline
identity. `Envelope.pipeline` is optional for compatibility with non-runtime
publishers, but turns emitted by prepared pipelines set it from the graph name.
The status projection ignores unattributed events rather than guessing which
pipeline they belong to from node names.

Satellite status is also projected in memory. Connected Satellite means an
authenticated device currently holds a conversation WebSocket. Recently Active
Satellite means an attributed event from that device landed inside the
operator-facing recent activity window. These states are deliberately separate:
a connected socket can exist before meaningful activity, and recent activity
can remain after the socket closes. A process restart clears both projections
until new sockets or events appear.

Provider Status is projected from the runtime provider registry and stored
pipeline graph references until a durable Provider Settings store exists.
Registered providers are Configured. Their `Provider::health()` result is the
reachability check, so saved or registered settings do not imply Reachable.
Proven Provider status is recorded only when a later snapshot can tie a
successfully completed turn's invoked component back to the provider referenced
by the stored graph. This keeps Provider Status separate from Pipeline Health:
provider warnings can explain risk, but a pipeline is recovered only by a
successful turn through the pipeline.
