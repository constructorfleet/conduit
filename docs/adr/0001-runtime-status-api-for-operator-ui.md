# Runtime Status API For Operator UI

Conduit's operator UI will use a first-class runtime status API as the initial
source of truth for pipeline and satellite state, then apply live event updates
from the event stream. Relying only on `/v1/events` would make refreshes and
late subscribers lose connected-satellite and recent-activity context, while a
status API gives the UI a coherent snapshot before it becomes event-driven.

The shared response contract lives in `conduit_api::status` as
`OperatorStatusSnapshot`. The route implementation is a later slice, but it
must return that shape rather than inventing per-screen status objects. The
snapshot groups runtime launch state, Pipeline Health, Component Health,
Provider Status, Satellite status, active turns, recent failures, and the event
stream rules that describe how live events update snapshot resources.

The status API is a management route. Device tokens cannot read it, because it
contains the same kind of operator-level operational state as the event stream.
Anonymous access is allowed only when the deployment explicitly enables
anonymous mode.
