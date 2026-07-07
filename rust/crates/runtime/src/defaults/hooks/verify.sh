#!/bin/sh
# claw verify hook — local self-repair signal for weak models.
#
# Runs the project's own fast check/lint after a file edit and feeds any located
# errors back to the model as advisory context (it does NOT block the edit).
# The model then self-corrects from exact compiler/linter output.
#
# 100% LOCAL / OFFLINE: only invokes tools already on PATH (cargo, tsc, biome,
# dotnet, hadolint, shellcheck). No network, no telemetry, nothing leaves the
# machine — safe for commercial / air-gapped projects.
#
# Wired as a PostToolUse hook (see ~/.claw/settings.toml). Env provided by claw:
#   HOOK_TOOL_NAME   — the tool that just ran
#   HOOK_TOOL_INPUT  — its JSON input (contains the edited file path)
#
# Disable entirely with:  CLAW_VERIFY=0
# Add a full clippy sweep (slow on big workspaces) with:  CLAW_VERIFY_FULL=1
# claw kills any hook after CLAW_HOOK_TIMEOUT_SECS (default 120s; 0 disables).

# Drain the JSON payload claw pipes on stdin (we only use env vars): a hook
# that never reads stdin would leave the parent blocked on a full pipe.
cat >/dev/null 2>&1 || :

[ "$CLAW_VERIFY" = "0" ] && exit 0

# Only react to edits/writes — not Read/Grep/Bash/etc.
case "$HOOK_TOOL_NAME" in
    Edit | Write | MultiEdit | edit_file | write_file) : ;;
    *) exit 0 ;;
esac

# Best-effort: pull the edited file path out of the tool input JSON.
path=$(printf '%s' "$HOOK_TOOL_INPUT" \
    | grep -oE '"(file_path|path)"[[:space:]]*:[[:space:]]*"[^"]+"' \
    | head -1 | sed -E 's/.*"([^"]+)"$/\1/')
ext=${path##*.}
base=$(basename "$path" 2>/dev/null)

have() { command -v "$1" >/dev/null 2>&1; }

# Emit bounded feedback and exit 0 (advisory, non-blocking).
report() {
    label=$1
    body=$2
    printf 'verify: %s reported problems — fix before finishing:\n%s\n' \
        "$label" "$(printf '%s\n' "$body" | head -n 40)"
    exit 0
}

# --- Rust ---------------------------------------------------------------------
if [ "$ext" = "rs" ] && [ -f Cargo.toml ] && have cargo; then
    out=$(cargo check --quiet 2>&1) || report "cargo check" "$out"
    # A clippy --all-targets sweep recompiles tests/benches and can take minutes
    # per edit on a workspace, freezing the turn — opt in explicitly.
    if [ "$CLAW_VERIFY_FULL" = "1" ] && [ "$CLAW_VERIFY_FAST" != "1" ]; then
        out=$(cargo clippy --quiet --all-targets -- -D warnings 2>&1) \
            || report "cargo clippy -D warnings" "$out"
    fi
    exit 0
fi

# --- JS / TS ------------------------------------------------------------------
case "$ext" in
    ts | tsx | js | jsx | mjs | cts | mts)
        if [ -f tsconfig.json ] && have npx; then
            out=$(npx --no-install tsc --noEmit 2>&1) || report "tsc --noEmit" "$out"
        fi
        if have biome; then
            out=$(biome check "$path" 2>&1) || report "biome check" "$out"
        elif [ -f biome.json ] && have npx; then
            out=$(npx --no-install biome check "$path" 2>&1) || report "biome check" "$out"
        fi
        exit 0
        ;;
esac

# --- C# / .NET ----------------------------------------------------------------
case "$ext" in
    cs | csproj | fs | fsproj)
        if have dotnet; then
            out=$(dotnet build --nologo -v q 2>&1) || report "dotnet build" "$out"
        fi
        exit 0
        ;;
esac

# --- Dockerfile ---------------------------------------------------------------
case "$base" in
    Dockerfile | Dockerfile.*)
        if have hadolint; then
            out=$(hadolint "$path" 2>&1) || report "hadolint" "$out"
        fi
        exit 0
        ;;
esac

# --- Shell --------------------------------------------------------------------
if [ "$ext" = "sh" ] || [ "$ext" = "bash" ]; then
    if have shellcheck; then
        out=$(shellcheck "$path" 2>&1) || report "shellcheck" "$out"
    fi
    exit 0
fi

exit 0
