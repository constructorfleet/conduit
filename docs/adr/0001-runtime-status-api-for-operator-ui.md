# Runtime Status API For Operator UI

Conduit's operator UI will use a first-class runtime status API as the initial source of truth for pipeline and satellite state, then apply live event updates from the event stream. Relying only on `/v1/events` would make refreshes and late subscribers lose connected-satellite and recent-activity context, while a status API gives the UI a coherent snapshot before it becomes event-driven.
