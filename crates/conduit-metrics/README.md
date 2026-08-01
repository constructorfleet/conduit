# conduit-metrics

Prometheus metrics derived from Conduit events.

This crate does not instrument the runtime directly. `Collector` subscribes to
the `EventBus`, observes events already published by the runtime, and updates a
`Metrics` registry that the API renders at `/metrics`.

## Exported Signals

Metrics cover:

- time to first synthesized audio
- total turn duration by outcome
- conversation counts and active conversations
- tool calls, requested calls, outcomes, and duration
- stage failures
- LLM token usage
- event volume by stage
- conversations forgotten from tracking
- events dropped by the metrics subscriber

## Drop Accounting

The event bus counts drops per subscription. The collector reads its own
subscription's drop count and exports `conduit_events_dropped_total` labelled
with `subscriber="metrics"`.

Other subscribers are responsible for exporting their own drops if they need
them visible in Prometheus.
