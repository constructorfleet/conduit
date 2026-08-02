//! Contract tests for the operator status snapshot.
//!
//! These do not exercise a handler yet. The public seam for #37 is the typed
//! contract that later backend endpoints and frontend fixtures must share.

use chrono::{TimeZone, Utc};
use conduit_api::status::{
    ActiveTurnStatus, ComponentHealth, ComponentHealthState, ComponentKind,
    EventStreamContract, LaunchState, OperatorStatusSnapshot, PipelineHealth,
    PipelineHealthState, PipelineStatus, ProviderKind, ProviderStatus, ProviderStatusState,
    RecentlyActiveSatellite, RuntimeFailure, RuntimeState, SatelliteStatus,
    SnapshotEventBinding, SnapshotResource, StaleState, TurnOutcome,
};
use conduit_core::id::{ConversationId, DeviceId, TraceId, TurnId};
use uuid::Uuid;

fn uuid(value: &str) -> Uuid {
    Uuid::parse_str(value).expect("fixture uuid")
}

#[test]
fn operator_status_snapshot_serializes_the_contract_shape() {
    let device = DeviceId::from_uuid(uuid("00000000-0000-0000-0000-000000000001"));
    let conversation = ConversationId::from_uuid(uuid("00000000-0000-0000-0000-000000000002"));
    let turn = TurnId::from_uuid(uuid("00000000-0000-0000-0000-000000000003"));
    let trace = TraceId::from_uuid(uuid("00000000-0000-0000-0000-000000000004"));

    let snapshot = OperatorStatusSnapshot {
        generated_at: Utc.with_ymd_and_hms(2026, 8, 1, 1, 2, 3).unwrap(),
        runtime: RuntimeState {
            launch_state: LaunchState::OperationsWorkspace,
            stale_state: StaleState::Fresh,
        },
        pipelines: vec![PipelineStatus {
            name: "kitchen".to_owned(),
            usable: true,
            health: PipelineHealth {
                state: PipelineHealthState::Unhealthy,
                summary: "speech synthesis failed after the model completed".to_owned(),
                last_successful_turn: None,
                last_failed_turn: Some(turn),
            },
            components: vec![
                ComponentHealth {
                    kind: ComponentKind::Reasoning,
                    provider: Some("openai-primary".to_owned()),
                    state: ComponentHealthState::Healthy,
                    detail: Some("last invoked turn completed".to_owned()),
                    last_turn: Some(turn),
                },
                ComponentHealth {
                    kind: ComponentKind::Synthesis,
                    provider: Some("piper-local".to_owned()),
                    state: ComponentHealthState::Unhealthy,
                    detail: Some("connection refused".to_owned()),
                    last_turn: Some(turn),
                },
            ],
            affected_providers: vec!["piper-local".to_owned()],
        }],
        providers: vec![ProviderStatus {
            id: "piper-local".to_owned(),
            kind: ProviderKind::Tts,
            state: ProviderStatusState::Configured,
            configured: true,
            reachable: false,
            proven_by_turn: None,
            message: Some("no successful reachability check yet".to_owned()),
            affects_pipelines: vec!["kitchen".to_owned()],
            offers_tools: Vec::new(),
        }],
        satellites: SatelliteStatus {
            connected: vec![],
            recently_active: vec![RecentlyActiveSatellite {
                device,
                name: "Kitchen Satellite".to_owned(),
                last_seen_at: Utc.with_ymd_and_hms(2026, 8, 1, 1, 1, 58).unwrap(),
                last_event: "TtsStarted".to_owned(),
            }],
            recent_window_seconds: 300,
        },
        active_turns: vec![ActiveTurnStatus {
            pipeline: "kitchen".to_owned(),
            conversation,
            turn,
            trace,
            started_at: Utc.with_ymd_and_hms(2026, 8, 1, 1, 1, 59).unwrap(),
            invoked_components: vec![ComponentKind::Reasoning, ComponentKind::Synthesis],
        }],
        recent_failures: vec![RuntimeFailure {
            pipeline: "kitchen".to_owned(),
            turn: Some(turn),
            component: ComponentKind::Synthesis,
            provider: Some("piper-local".to_owned()),
            message: "connection refused".to_owned(),
            at: Utc.with_ymd_and_hms(2026, 8, 1, 1, 2, 1).unwrap(),
        }],
        event_stream: EventStreamContract {
            route: "/v1/events".to_owned(),
            stale_state_on_disconnect: StaleState::Stale,
            refresh_snapshot_after_reconnect: true,
            bindings: vec![
                SnapshotEventBinding {
                    resource: SnapshotResource::PipelineHealth,
                    events: vec![
                        "TurnStarted".to_owned(),
                        "StageFailed".to_owned(),
                        "ConversationCompleted".to_owned(),
                    ],
                },
                SnapshotEventBinding {
                    resource: SnapshotResource::SatelliteStatus,
                    events: vec![
                        "ConversationStarted".to_owned(),
                        "AudioStarted".to_owned(),
                        "ConversationCompleted".to_owned(),
                    ],
                },
            ],
        },
    };

    let json = serde_json::to_value(snapshot).expect("serializes");

    assert_eq!(
        json,
        serde_json::json!({
            "generated_at": "2026-08-01T01:02:03Z",
            "runtime": {
                "launch_state": "operations_workspace",
                "stale_state": "fresh"
            },
            "pipelines": [{
                "name": "kitchen",
                "usable": true,
                "health": {
                    "state": "unhealthy",
                    "summary": "speech synthesis failed after the model completed",
                    "last_successful_turn": null,
                    "last_failed_turn": "00000000-0000-0000-0000-000000000003"
                },
                "components": [{
                    "kind": "reasoning",
                    "provider": "openai-primary",
                    "state": "healthy",
                    "detail": "last invoked turn completed",
                    "last_turn": "00000000-0000-0000-0000-000000000003"
                }, {
                    "kind": "synthesis",
                    "provider": "piper-local",
                    "state": "unhealthy",
                    "detail": "connection refused",
                    "last_turn": "00000000-0000-0000-0000-000000000003"
                }],
                "affected_providers": ["piper-local"]
            }],
            "providers": [{
                "id": "piper-local",
                "kind": "tts",
                "state": "configured",
                "configured": true,
                "reachable": false,
                "proven_by_turn": null,
                "message": "no successful reachability check yet",
                "affects_pipelines": ["kitchen"]
            }],
            "satellites": {
                "connected": [],
                "recently_active": [{
                    "device": "00000000-0000-0000-0000-000000000001",
                    "name": "Kitchen Satellite",
                    "last_seen_at": "2026-08-01T01:01:58Z",
                    "last_event": "TtsStarted"
                }],
                "recent_window_seconds": 300
            },
            "active_turns": [{
                "pipeline": "kitchen",
                "conversation": "00000000-0000-0000-0000-000000000002",
                "turn": "00000000-0000-0000-0000-000000000003",
                "trace": "00000000-0000-0000-0000-000000000004",
                "started_at": "2026-08-01T01:01:59Z",
                "invoked_components": ["reasoning", "synthesis"]
            }],
            "recent_failures": [{
                "pipeline": "kitchen",
                "turn": "00000000-0000-0000-0000-000000000003",
                "component": "synthesis",
                "provider": "piper-local",
                "message": "connection refused",
                "at": "2026-08-01T01:02:01Z"
            }],
            "event_stream": {
                "route": "/v1/events",
                "stale_state_on_disconnect": "stale",
                "refresh_snapshot_after_reconnect": true,
                "bindings": [{
                    "resource": "pipeline_health",
                    "events": ["TurnStarted", "StageFailed", "ConversationCompleted"]
                }, {
                    "resource": "satellite_status",
                    "events": ["ConversationStarted", "AudioStarted", "ConversationCompleted"]
                }]
            }
        })
    );
}

#[test]
fn successful_turn_contract_ignores_components_not_invoked() {
    let outcome = TurnOutcome {
        turn: TurnId::from_uuid(uuid("00000000-0000-0000-0000-000000000005")),
        result: conduit_api::status::TurnResult::Successful,
        invoked_components: vec![ComponentKind::Transcription, ComponentKind::Reasoning],
        failed_components: vec![],
    };

    assert!(outcome.proves_recovery_for(ComponentKind::Reasoning));
    assert!(!outcome.proves_recovery_for(ComponentKind::Tools));

    let contradictory = TurnOutcome {
        result: conduit_api::status::TurnResult::Successful,
        failed_components: vec![ComponentKind::Reasoning],
        ..outcome
    };
    assert!(!contradictory.proves_recovery_for(ComponentKind::Reasoning));
}
