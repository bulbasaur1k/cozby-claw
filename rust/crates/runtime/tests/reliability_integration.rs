use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use runtime::{
    apply_policy, attempt_recovery, check_freshness, recipe_for, BranchFreshness, DiffScope,
    FailureScenario, LaneBlocker, LaneContext, McpDegradedReport, McpFailedServer,
    McpLifecyclePhase, McpLifecycleValidator, PolicyAction, PolicyCondition, PolicyEngine,
    PolicyRule, RecoveryContext, RecoveryResult, RecoveryStep, ReviewStatus, StaleBranchAction,
    StaleBranchPolicy, WorkerFailureKind, WorkerRegistry, WorkerStatus,
};

fn temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{nanos}"))
}

fn run_git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|error| panic!("git {} failed to execute: {error}", args.join(" ")));
    assert!(
        status.success(),
        "git {} exited with {status}",
        args.join(" ")
    );
}

fn init_repo(path: &Path) {
    fs::create_dir_all(path).expect("create repo dir");
    run_git(path, &["init", "--quiet", "-b", "main"]);
    run_git(path, &["config", "user.email", "tests@example.com"]);
    run_git(path, &["config", "user.name", "Runtime Integration Tests"]);
    fs::write(path.join("init.txt"), "initial\n").expect("write init file");
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "initial commit", "--quiet"]);
}

fn commit_file(path: &Path, file: &str, contents: &str, message: &str) {
    fs::write(path.join(file), contents).expect("write file");
    run_git(path, &["add", file]);
    run_git(path, &["commit", "-m", message, "--quiet"]);
}

fn cwd_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct CurrentDirGuard {
    original: PathBuf,
}

impl CurrentDirGuard {
    fn change_to(path: &Path) -> Self {
        let original = env::current_dir().expect("read current dir");
        env::set_current_dir(path).expect("set current dir");
        Self { original }
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        env::set_current_dir(&self.original).expect("restore current dir");
    }
}

#[test]
fn branch_freshness_detection_surfaces_stale_fix_history() {
    let root = temp_dir("runtime-branch-freshness");
    init_repo(&root);

    run_git(&root, &["checkout", "-b", "topic"]);
    run_git(&root, &["checkout", "main"]);
    commit_file(&root, "fix1.txt", "timeout fix\n", "fix: resolve timeout");
    commit_file(&root, "fix2.txt", "hotpatch\n", "fix: apply hotpatch");

    let freshness = {
        let _cwd_guard = cwd_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _dir_guard = CurrentDirGuard::change_to(&root);
        check_freshness("topic", "main")
    };

    match &freshness {
        BranchFreshness::Stale {
            commits_behind,
            missing_fixes,
        } => {
            assert_eq!(*commits_behind, 2);
            assert_eq!(
                missing_fixes,
                &vec![
                    "fix: apply hotpatch".to_string(),
                    "fix: resolve timeout".to_string(),
                ]
            );
        }
        other => panic!("expected stale branch, got {other:?}"),
    }

    let action = apply_policy(&freshness, StaleBranchPolicy::Block);
    assert!(matches!(action, StaleBranchAction::Block { .. }));

    fs::remove_dir_all(&root).expect("cleanup temp repo");
}

#[test]
fn mcp_degraded_startup_reports_recoverable_timeout_and_missing_tools() {
    let mut validator = McpLifecycleValidator::new();
    for phase in [
        McpLifecyclePhase::ConfigLoad,
        McpLifecyclePhase::ServerRegistration,
        McpLifecyclePhase::SpawnConnect,
        McpLifecyclePhase::InitializeHandshake,
    ] {
        assert!(matches!(
            validator.run_phase(phase),
            runtime::McpPhaseResult::Success { .. }
        ));
    }

    let timeout = validator.record_timeout(
        McpLifecyclePhase::ToolDiscovery,
        Duration::from_secs(5),
        Some("demo".to_string()),
        BTreeMap::from([("transport".to_string(), "stdio".to_string())]),
    );

    let error = match timeout {
        runtime::McpPhaseResult::Timeout { phase, error, .. } => {
            assert_eq!(phase, McpLifecyclePhase::ToolDiscovery);
            assert!(error.recoverable);
            assert_eq!(error.server_name.as_deref(), Some("demo"));
            error
        }
        other => panic!("expected timeout result, got {other:?}"),
    };

    let degraded = McpDegradedReport::new(
        vec!["alpha".to_string()],
        vec![McpFailedServer {
            server_name: "demo".to_string(),
            phase: McpLifecyclePhase::ToolDiscovery,
            error,
        }],
        vec!["mcp__alpha__ping".to_string()],
        vec![
            "mcp__alpha__ping".to_string(),
            "mcp__demo__echo".to_string(),
        ],
    );

    assert_eq!(
        validator.state().current_phase(),
        Some(McpLifecyclePhase::ErrorSurfacing)
    );
    assert_eq!(degraded.working_servers, vec!["alpha".to_string()]);
    assert_eq!(
        degraded.available_tools,
        vec!["mcp__alpha__ping".to_string()]
    );
    assert_eq!(degraded.missing_tools, vec!["mcp__demo__echo".to_string()]);
    assert_eq!(degraded.failed_servers.len(), 1);
    assert_eq!(
        degraded.failed_servers[0].phase,
        McpLifecyclePhase::ToolDiscovery
    );
}

#[test]
fn policy_routing_distinguishes_startup_recovery_from_merge_ready_lanes() {
    let engine = PolicyEngine::new(vec![
        PolicyRule::new(
            "recover-startup",
            PolicyCondition::StartupBlocked,
            PolicyAction::Chain(vec![
                PolicyAction::RecoverOnce,
                PolicyAction::Notify {
                    channel: "#ops".to_string(),
                },
            ]),
            5,
        ),
        PolicyRule::new(
            "merge-ready",
            PolicyCondition::And(vec![
                PolicyCondition::GreenAt { level: 3 },
                PolicyCondition::ReviewPassed,
                PolicyCondition::ScopedDiff,
            ]),
            PolicyAction::MergeToDev,
            20,
        ),
    ]);

    let startup_blocked = LaneContext::new(
        "lane-blocked",
        3,
        Duration::from_secs(15 * 60),
        LaneBlocker::Startup,
        ReviewStatus::Pending,
        DiffScope::Scoped,
        false,
    );
    assert_eq!(
        engine.evaluate(&startup_blocked),
        vec![
            PolicyAction::RecoverOnce,
            PolicyAction::Notify {
                channel: "#ops".to_string(),
            },
        ]
    );

    let merge_ready = LaneContext::new(
        "lane-ready",
        3,
        Duration::from_secs(15 * 60),
        LaneBlocker::None,
        ReviewStatus::Approved,
        DiffScope::Scoped,
        false,
    );
    assert_eq!(
        engine.evaluate(&merge_ready),
        vec![PolicyAction::MergeToDev]
    );
}

#[test]
fn prompt_misdelivery_arms_replay_and_maps_to_recovery_recipe() {
    let root = temp_dir("runtime-prompt-misdelivery");
    fs::create_dir_all(&root).expect("create worker root");

    let registry = WorkerRegistry::new();
    let worker = registry.create(root.to_str().expect("utf8 path"), &[], true);

    let ready = registry
        .observe(&worker.worker_id, "Ready for your input\n>")
        .expect("worker should become ready");
    assert_eq!(ready.status, WorkerStatus::ReadyForPrompt);

    let running = registry
        .send_prompt(&worker.worker_id, Some("Investigate flaky boot"))
        .expect("prompt send should succeed");
    assert_eq!(running.status, WorkerStatus::Running);

    let recovered = registry
        .observe(
            &worker.worker_id,
            "% Investigate flaky boot\nzsh: command not found: Investigate",
        )
        .expect("misdelivery observe should succeed");
    assert_eq!(recovered.status, WorkerStatus::ReadyForPrompt);
    assert_eq!(
        recovered.replay_prompt.as_deref(),
        Some("Investigate flaky boot")
    );

    let failure = recovered
        .last_error
        .expect("worker should record a prompt delivery failure");
    assert_eq!(failure.kind, WorkerFailureKind::PromptDelivery);

    let scenario = FailureScenario::from_worker_failure_kind(failure.kind);
    assert_eq!(scenario, FailureScenario::PromptMisdelivery);
    assert_eq!(
        recipe_for(&scenario).steps,
        vec![RecoveryStep::RedirectPromptToAgent]
    );

    let mut context = RecoveryContext::new();
    let result = attempt_recovery(&scenario, &mut context);
    assert_eq!(result, RecoveryResult::Recovered { steps_taken: 1 });

    fs::remove_dir_all(&root).expect("cleanup worker root");
}
