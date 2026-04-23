use std::fs;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use telemetry::{
    AnalyticsEvent, JsonlTelemetrySink, SessionTraceRecord, SessionTracer, TelemetryEvent,
};

fn temp_log_path() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("telemetry-roundtrip-{nanos}.jsonl"))
}

#[test]
fn telemetry_roundtrip_preserves_structured_jsonl_events() {
    let path = temp_log_path();
    let sink = Arc::new(JsonlTelemetrySink::new(&path).expect("sink should create file"));
    let tracer = SessionTracer::new("session-integration", sink);

    let mut request_attributes = Map::new();
    request_attributes.insert(
        "model".to_string(),
        Value::String("claude-sonnet".to_string()),
    );

    tracer.record_http_request_started(1, "POST", "/v1/messages", request_attributes.clone());
    tracer.record_http_request_succeeded(
        1,
        "POST",
        "/v1/messages",
        200,
        Some("req_123".to_string()),
        request_attributes,
    );
    tracer.record_analytics(
        AnalyticsEvent::new("cli", "prompt_sent").with_property("turn", Value::from(1)),
    );

    let events = fs::read_to_string(&path)
        .expect("telemetry log should be readable")
        .lines()
        .map(|line| serde_json::from_str::<TelemetryEvent>(line).expect("line should deserialize"))
        .collect::<Vec<_>>();

    assert_eq!(events.len(), 6);
    assert!(matches!(
        &events[0],
        TelemetryEvent::HttpRequestStarted {
            session_id,
            attempt: 1,
            method,
            path,
            ..
        } if session_id == "session-integration" && method == "POST" && path == "/v1/messages"
    ));
    assert!(matches!(
        &events[1],
        TelemetryEvent::SessionTrace(SessionTraceRecord { sequence: 0, name, .. })
            if name == "http_request_started"
    ));
    assert!(matches!(
        &events[2],
        TelemetryEvent::HttpRequestSucceeded {
            session_id,
            attempt: 1,
            method,
            path,
            status: 200,
            request_id,
            ..
        } if session_id == "session-integration"
            && method == "POST"
            && path == "/v1/messages"
            && request_id.as_deref() == Some("req_123")
    ));
    assert!(matches!(
        &events[3],
        TelemetryEvent::SessionTrace(SessionTraceRecord { sequence: 1, name, .. })
            if name == "http_request_succeeded"
    ));
    assert!(matches!(
        &events[4],
        TelemetryEvent::Analytics(analytics)
            if analytics.namespace == "cli" && analytics.action == "prompt_sent"
    ));
    assert!(matches!(
        &events[5],
        TelemetryEvent::SessionTrace(SessionTraceRecord { sequence: 2, name, .. })
            if name == "analytics"
    ));

    let _ = fs::remove_file(path);
}
