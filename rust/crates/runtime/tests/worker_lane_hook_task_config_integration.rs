use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use runtime::task_registry::{TaskRegistry, TaskStatus};
use runtime::{
    validate_packet, ConfigLoader, HookRunner, LaneEvent, LaneEventBlocker, LaneFailureClass,
    RuntimeHookConfig, TaskPacket, WorkerEventKind, WorkerFailureKind, WorkerRegistry,
    WorkerStatus,
};
use serde_json::json;

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(prefix: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{unique}"));
        fs::create_dir_all(&path).expect("temp dir should be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[test]
fn worker_boot_state_progresses_from_spawning_to_ready_snapshot() {
    let registry = WorkerRegistry::new();
    let worker = registry.create("/tmp/runtime-integration-worker", &[], true);

    assert_eq!(worker.status, WorkerStatus::Spawning);
    assert_eq!(worker.events.len(), 1);
    assert_eq!(worker.events[0].kind, WorkerEventKind::Spawning);

    let ready = registry
        .observe(&worker.worker_id, "Ready for your input\n>")
        .expect("ready observe should succeed");

    assert_eq!(ready.status, WorkerStatus::ReadyForPrompt);
    assert!(ready.last_error.is_none());
    assert_eq!(
        ready.events.last().map(|event| event.kind),
        Some(WorkerEventKind::ReadyForPrompt)
    );

    let snapshot = registry
        .await_ready(&worker.worker_id)
        .expect("ready snapshot should succeed");
    assert_eq!(snapshot.worker_id, worker.worker_id);
    assert!(snapshot.ready);
    assert!(!snapshot.blocked);
    assert!(!snapshot.replay_prompt_ready);
    assert!(snapshot.last_error.is_none());
}

#[test]
fn lane_event_emission_serializes_worker_prompt_delivery_failure() {
    let registry = WorkerRegistry::new();
    let worker = registry.create("/tmp/runtime-integration-lane", &[], true);
    registry
        .observe(&worker.worker_id, "Ready for input\n>")
        .expect("ready observe should succeed");
    registry
        .send_prompt(&worker.worker_id, Some("Run lane event emission test"))
        .expect("prompt send should succeed");

    let failed = registry
        .observe(
            &worker.worker_id,
            "% Run lane event emission test\nzsh: command not found: Run",
        )
        .expect("misdelivery observe should succeed");

    let error = failed
        .last_error
        .clone()
        .expect("prompt delivery failure should be recorded");
    assert_eq!(error.kind, WorkerFailureKind::PromptDelivery);

    let blocker = LaneEventBlocker {
        failure_class: LaneFailureClass::PromptDelivery,
        detail: error.message,
    };
    let lane_event = LaneEvent::blocked("2026-04-04T00:00:00Z", &blocker).with_data(json!({
        "worker_id": failed.worker_id,
        "worker_status": failed.status,
        "worker_event_kinds": failed
            .events
            .iter()
            .map(|event| format!("{:?}", event.kind))
            .collect::<Vec<_>>()
    }));

    let emitted = serde_json::to_value(&lane_event).expect("lane event should serialize");
    assert_eq!(emitted["event"], json!("lane.blocked"));
    assert_eq!(emitted["status"], json!("blocked"));
    assert_eq!(emitted["failureClass"], json!("prompt_delivery"));
    assert!(emitted["detail"]
        .as_str()
        .expect("detail should be a string")
        .contains("worker prompt landed in shell"));
    assert_eq!(emitted["data"]["worker_status"], json!("ready_for_prompt"));
    assert!(emitted["data"]["worker_event_kinds"]
        .as_array()
        .expect("worker event kinds should be an array")
        .iter()
        .any(|value| value == "PromptMisdelivery"));
}

#[test]
fn hook_merge_runs_loaded_config_hooks_and_overlay_once_each() {
    let temp = TestDir::new("runtime-hooks-integration");
    let cwd = temp.path().join("project");
    let home = temp.path().join("home").join(".claw");
    fs::create_dir_all(cwd.join(".claw")).expect("project config dir should exist");
    fs::create_dir_all(&home).expect("home config dir should exist");

    fs::write(
        home.join("settings.json"),
        r#"{
          "hooks": {
            "PreToolUse": ["printf 'config pre'"]
          }
        }"#,
    )
    .expect("home settings should be written");
    fs::write(
        cwd.join(".claw").join("settings.local.json"),
        r#"{
          "hooks": {
            "PostToolUse": ["printf 'config post'"]
          }
        }"#,
    )
    .expect("project settings should be written");

    let loaded = ConfigLoader::new(&cwd, &home)
        .load()
        .expect("config should load");
    let overlay = RuntimeHookConfig::new(
        vec![
            "printf 'config pre'".to_string(),
            "printf 'overlay pre'".to_string(),
        ],
        vec![],
        vec![],
    );

    let runner = HookRunner::new(loaded.hooks().merged(&overlay));
    let result = runner.run_pre_tool_use("Read", r#"{"path":"README.md"}"#);

    assert_eq!(
        result.messages(),
        &["config pre".to_string(), "overlay pre".to_string()]
    );
    assert!(!result.is_failed());
    assert!(!result.is_denied());
}

#[test]
fn task_packet_roundtrip_validates_and_creates_registry_task() {
    let packet = TaskPacket {
        objective: "Ship runtime integration coverage".to_string(),
        scope: "runtime/tests".to_string(),
        repo: "cozby-claw".to_string(),
        branch_policy: "origin/main only".to_string(),
        acceptance_tests: vec!["cargo test --workspace".to_string()],
        commit_policy: "single verified commit".to_string(),
        reporting_contract: "print verification summary and sha".to_string(),
        escalation_policy: "escalate only on destructive ambiguity".to_string(),
    };

    let serialized = serde_json::to_string(&packet).expect("packet should serialize");
    let roundtrip: TaskPacket =
        serde_json::from_str(&serialized).expect("packet should deserialize");
    let validated = validate_packet(roundtrip.clone()).expect("packet should validate");

    let registry = TaskRegistry::new();
    let task = registry
        .create_from_packet(validated.into_inner())
        .expect("task should be created from packet");
    registry
        .set_status(&task.task_id, TaskStatus::Running)
        .expect("status should update");

    let stored = registry.get(&task.task_id).expect("task should be stored");
    assert_eq!(stored.prompt, packet.objective);
    assert_eq!(stored.description.as_deref(), Some("runtime/tests"));
    assert_eq!(stored.task_packet, Some(packet));
    assert_eq!(stored.status, TaskStatus::Running);
}

#[test]
fn config_validation_rejects_invalid_hook_entries_before_merge() {
    let temp = TestDir::new("runtime-config-validation");
    let cwd = temp.path().join("project");
    let home = temp.path().join("home").join(".claw");
    let project_settings = cwd.join(".claw").join("settings.json");
    fs::create_dir_all(cwd.join(".claw")).expect("project config dir should exist");
    fs::create_dir_all(&home).expect("home config dir should exist");

    fs::write(
        home.join("settings.json"),
        r#"{"hooks":{"PreToolUse":["printf 'base'"]}}"#,
    )
    .expect("home settings should be written");
    fs::write(
        &project_settings,
        r#"{"hooks":{"PreToolUse":["printf 'project'",42]}}"#,
    )
    .expect("project settings should be written");

    let error = ConfigLoader::new(&cwd, &home)
        .load()
        .expect_err("invalid hooks should fail validation");
    let rendered = error.to_string();

    assert!(rendered.contains(&format!(
        "{}: hooks: field PreToolUse must contain only strings",
        project_settings.display()
    )));
    assert!(!rendered.contains("merged settings.hooks"));
}
