//! First-run scaffolding of the user config home (`~/.claw`).
//!
//! claw treats `~/.claw` (or `$CLAW_CONFIG_HOME`) as the single home for user
//! configuration: models (`providers.toml`), settings (`settings.*`), MCP
//! servers (`mcp.toml`), skills (`skills/`), hooks (`hooks/`) and agents
//! (`agents/`). To make that home useful out of the box, on first run we seed a
//! set of transparent, fully-local defaults:
//!
//! - **skills** — prompt-procedure skills (verify, grounding, plan, debug, …)
//!   that encode good practice a weak model would otherwise improvise;
//! - **hooks/verify.sh + settings.toml** — a local self-repair loop that runs
//!   the project's own check/lint after each edit and feeds errors back;
//! - **mcp.toml** — external MCP servers, *commented out* (claw defaults to no
//!   outbound traffic, for commercial / offline use).
//!
//! Everything seeded is local and offline — nothing calls out. Seeding is
//! idempotent and never overwrites anything the user already has.

use std::fs;
use std::path::Path;

/// A default skill shipped with claw, embedded at build time.
struct DefaultSkill {
    /// Directory name under `skills/` (also the invocation name).
    name: &'static str,
    /// Full `SKILL.md` contents.
    body: &'static str,
}

/// Transparent, fully-local skills seeded into `~/.claw/skills` on first run.
/// Each encodes a procedure (verification, grounding, a template) rather than
/// prose — the highest-value shape for a weak (qwen3-class) model. No skill here
/// performs any network/external call.
const DEFAULT_SKILLS: &[DefaultSkill] = &[
    // Git / review
    DefaultSkill { name: "commit", body: include_str!("defaults/skills/commit/SKILL.md") },
    DefaultSkill { name: "code-review", body: include_str!("defaults/skills/code-review/SKILL.md") },
    DefaultSkill { name: "pr-description", body: include_str!("defaults/skills/pr-description/SKILL.md") },
    DefaultSkill { name: "security-review", body: include_str!("defaults/skills/security-review/SKILL.md") },
    DefaultSkill { name: "changelog", body: include_str!("defaults/skills/changelog/SKILL.md") },
    // Weak-model amplifiers (Tier-1 techniques as procedures)
    DefaultSkill { name: "verify", body: include_str!("defaults/skills/verify/SKILL.md") },
    DefaultSkill { name: "grounding", body: include_str!("defaults/skills/grounding/SKILL.md") },
    DefaultSkill { name: "plan", body: include_str!("defaults/skills/plan/SKILL.md") },
    DefaultSkill { name: "consult", body: include_str!("defaults/skills/consult/SKILL.md") },
    DefaultSkill { name: "debug", body: include_str!("defaults/skills/debug/SKILL.md") },
    DefaultSkill { name: "test-first", body: include_str!("defaults/skills/test-first/SKILL.md") },
    DefaultSkill { name: "refactor", body: include_str!("defaults/skills/refactor/SKILL.md") },
    // Docs / architecture
    DefaultSkill { name: "adr", body: include_str!("defaults/skills/adr/SKILL.md") },
    DefaultSkill { name: "rfc", body: include_str!("defaults/skills/rfc/SKILL.md") },
    DefaultSkill { name: "mermaid", body: include_str!("defaults/skills/mermaid/SKILL.md") },
];

/// Default `mcp.toml` (external servers commented out) seeded on first run.
const DEFAULT_MCP_TOML: &str = include_str!("defaults/mcp.toml");

/// Default `settings.toml` wiring the local self-repair verify hook.
const DEFAULT_SETTINGS_TOML: &str = include_str!("defaults/settings.toml");

/// Default `plugins.toml` — git plugin sources (ships with the cozby-docs plugin).
const DEFAULT_PLUGINS_TOML: &str = include_str!("defaults/plugins.toml");

/// The local self-repair verify hook script.
const DEFAULT_VERIFY_HOOK: &str = include_str!("defaults/hooks/verify.sh");

/// Summary of what [`scaffold_config_home`] created, for logging and tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScaffoldReport {
    /// Names of skills newly written under `skills/`.
    pub created_skills: Vec<String>,
    /// Human-readable labels of other artifacts newly created, e.g.
    /// `"verify hook"`, `"settings.toml"`, `"mcp.toml"`, `"agents/"`.
    pub created_files: Vec<String>,
}

impl ScaffoldReport {
    /// True when nothing was created (config home was already populated).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.created_skills.is_empty() && self.created_files.is_empty()
    }

    /// True when an artifact with `label` was created this run (test helper).
    #[must_use]
    pub fn created(&self, label: &str) -> bool {
        self.created_files.iter().any(|entry| entry == label)
    }
}

/// Idempotently seed default skills, hooks, `settings.toml`, `mcp.toml`, and the
/// reserved `agents/` directory into `config_home`. Existing files and skill
/// directories are left untouched, so this is safe to call on every startup.
///
/// # Errors
/// Propagates any filesystem error from creating directories or writing files.
pub fn scaffold_config_home(config_home: &Path) -> std::io::Result<ScaffoldReport> {
    let mut report = ScaffoldReport::default();

    // Default skills — one directory per skill, skipped if it already exists.
    let skills_root = config_home.join("skills");
    for skill in DEFAULT_SKILLS {
        let dir = skills_root.join(skill.name);
        if dir.exists() {
            continue;
        }
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("SKILL.md"), skill.body)?;
        report.created_skills.push(skill.name.to_string());
    }

    // Local self-repair verify hook script.
    let hooks_dir = config_home.join("hooks");
    let verify_path = hooks_dir.join("verify.sh");
    if !verify_path.exists() {
        fs::create_dir_all(&hooks_dir)?;
        fs::write(&verify_path, DEFAULT_VERIFY_HOOK)?;
        set_executable(&verify_path);
        report.created_files.push("verify hook".to_string());
    }

    // Default settings.toml — wires the verify hook. Only when the user has no
    // settings.toml yet, so we never clobber hand-written settings.
    let settings_path = config_home.join("settings.toml");
    if !settings_path.exists() {
        fs::create_dir_all(config_home)?;
        fs::write(&settings_path, DEFAULT_SETTINGS_TOML)?;
        report.created_files.push("settings.toml".to_string());
    }

    // Default MCP servers (external, commented out) — only when absent.
    let mcp_path = config_home.join("mcp.toml");
    if !mcp_path.exists() {
        fs::create_dir_all(config_home)?;
        fs::write(&mcp_path, DEFAULT_MCP_TOML)?;
        report.created_files.push("mcp.toml".to_string());
    }

    // Default plugin sources (ships the cozby-docs plugin) — only when absent.
    let plugins_path = config_home.join("plugins.toml");
    if !plugins_path.exists() {
        fs::create_dir_all(config_home)?;
        fs::write(&plugins_path, DEFAULT_PLUGINS_TOML)?;
        report.created_files.push("plugins.toml".to_string());
    }

    // Reserved home for user agent definitions.
    let agents_dir = config_home.join("agents");
    if !agents_dir.exists() {
        fs::create_dir_all(&agents_dir)?;
        report.created_files.push("agents/".to_string());
    }

    Ok(report)
}

/// Best-effort: mark a shipped script executable on unix. A failure is harmless
/// because the hook is invoked as `sh <path>`, which does not require the bit.
fn set_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::{scaffold_config_home, DEFAULT_SKILLS};
    use std::fs;

    fn temp_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("claw-defaults-{}", crate::test_unique_suffix()))
    }

    #[test]
    fn seeds_defaults_into_empty_home() {
        let home = temp_dir();
        let report = scaffold_config_home(&home).expect("scaffold");

        assert_eq!(report.created_skills.len(), DEFAULT_SKILLS.len());
        assert!(report.created("mcp.toml"));
        assert!(report.created("plugins.toml"));
        assert!(report.created("settings.toml"));
        assert!(report.created("verify hook"));
        assert!(report.created("agents/"));
        assert!(fs::read_to_string(home.join("plugins.toml"))
            .expect("read plugins")
            .contains("cozby-docs"));
        assert!(home.join("skills").join("verify").join("SKILL.md").is_file());
        assert!(home.join("skills").join("grounding").join("SKILL.md").is_file());
        assert!(home.join("skills").join("adr").join("SKILL.md").is_file());
        assert!(home.join("hooks").join("verify.sh").is_file());
        assert!(home.join("settings.toml").is_file());
        assert!(home.join("mcp.toml").is_file());
        assert!(home.join("agents").is_dir());
        // settings wires the verify hook; mcp defaults to no active servers.
        assert!(fs::read_to_string(home.join("settings.toml"))
            .expect("read settings")
            .contains("verify.sh"));
        assert!(fs::read_to_string(home.join("mcp.toml"))
            .expect("read mcp")
            .contains("ЗАКОММЕНТИРОВАН"));

        fs::remove_dir_all(&home).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn verify_hook_is_executable() {
        use std::os::unix::fs::PermissionsExt;
        let home = temp_dir();
        scaffold_config_home(&home).expect("scaffold");
        let mode = fs::metadata(home.join("hooks").join("verify.sh"))
            .expect("hook metadata")
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "verify.sh should be executable");
        fs::remove_dir_all(&home).expect("cleanup");
    }

    #[test]
    fn does_not_overwrite_existing_files() {
        let home = temp_dir();
        // Pre-existing files must survive untouched.
        fs::create_dir_all(home.join("skills").join("verify")).expect("skill dir");
        fs::write(home.join("skills").join("verify").join("SKILL.md"), "custom")
            .expect("write skill");
        fs::write(home.join("mcp.toml"), "# mine\n").expect("write mcp");
        fs::write(home.join("settings.toml"), "# my settings\n").expect("write settings");

        let report = scaffold_config_home(&home).expect("scaffold");

        assert!(!report.created("mcp.toml"));
        assert!(!report.created("settings.toml"));
        assert!(!report.created_skills.contains(&"verify".to_string()));
        assert_eq!(
            fs::read_to_string(home.join("skills").join("verify").join("SKILL.md"))
                .expect("read skill"),
            "custom"
        );
        assert_eq!(
            fs::read_to_string(home.join("settings.toml")).expect("read settings"),
            "# my settings\n"
        );

        // Second run is a no-op.
        let again = scaffold_config_home(&home).expect("scaffold again");
        assert!(again.is_empty());

        fs::remove_dir_all(&home).expect("cleanup");
    }
}
