---
name: verify
description: Run the project's local build/type-check/lint/test loop and fix errors until it passes. Fully offline.
---

# verify

Close the self-repair loop: after making changes, run the project's own
deterministic checks, read the located errors, fix them, and repeat until clean.
This is the single biggest quality lever — a weak model self-corrects from exact
compiler/linter output. Everything here is **local, offline, no network**.

## Loop

1. Detect the stack from files in the repo and run the FAST checks first:
   - **Rust** (`Cargo.toml`): `cargo check` → `cargo clippy -- -D warnings` → `cargo test`
   - **JS/TS** (`tsconfig.json`): `tsc --noEmit` (strict) → `biome check` (or `eslint`) → test runner (vitest/jest)
   - **C#/.NET** (`*.sln`/`*.csproj`): `dotnet build` → `dotnet test`
   - **Dockerfile**: `hadolint <file>` → `docker build` (if cheap)
   - **Shell** (`*.sh`): `shellcheck <file>`
2. Read the FIRST located error (file:line + message). Fix exactly that.
3. Re-run the same check. Do not move on while it still fails.
4. When the fast checks pass, run the tests. Fix failures the same way.
5. Stop when build + lint + tests are all green. Report what you ran and the result.

## Rules

- Only run tools that exist (`command -v`); skip missing ones silently.
- Fix the root cause the error points to — do not suppress warnings or add `any`/`#[allow]` to silence them.
- If the same error survives two fix attempts, re-plan instead of retrying blindly.
- Never weaken checks (no `--no-verify`, no disabling clippy/strict) to get green.
