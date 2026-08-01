# Snapshot Plus Events UI State

Conduit's operator UI will load a coherent snapshot for each major screen and then apply live updates from the event stream. The snapshot is the initial source of truth, while events keep the view current; event-stream-only screens are rejected because late subscribers and refreshed tabs would miss prior runtime and satellite state.
