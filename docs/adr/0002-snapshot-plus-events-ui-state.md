# Snapshot Plus Events UI State

Conduit's operator UI will load a coherent snapshot for each major screen and
then apply live updates from the event stream. The snapshot is the initial
source of truth, while events keep the view current; event-stream-only screens
are rejected because late subscribers and refreshed tabs would miss prior
runtime and satellite state.

`OperatorStatusSnapshot.event_stream` records the live-update contract alongside
the snapshot:

- `route` names the SSE route the UI subscribes to.
- `stale_state_on_disconnect` tells the UI which Stale State to show when live
  updates stop.
- `refresh_snapshot_after_reconnect` requires the UI to reload the status
  snapshot before applying events after a reconnect.
- `bindings` maps snapshot resources such as Pipeline Health and Satellite
  status to `conduit_core::event::Event` variant names.

The event stream remains the live update channel, not a history store and not
the initial source of truth. The browser must preserve the last known view when
the stream disconnects, label it as stale, then reload the snapshot before it
trusts new live updates.
