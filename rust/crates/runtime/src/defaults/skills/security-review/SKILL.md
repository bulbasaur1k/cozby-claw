---
name: security-review
description: Review the current diff for security issues — injection, secrets, unsafe input, auth, deps. Local only.
---

# security-review

Review the pending changes for security problems with an imposed checklist. Report
concrete, located findings ranked by severity. Local and offline — read the diff,
do not exfiltrate code anywhere.

## Steps

1. Get the diff: `git diff` (working tree) or `git diff <base>...HEAD`.
2. Check each hunk against this checklist:
   - **Injection** — SQL/shell/command/path built from unsanitized input; format-string misuse.
   - **Input validation** — unchecked external input, missing bounds/limits, unsafe deserialization.
   - **Secrets** — hardcoded keys/tokens/passwords; secrets logged or committed.
   - **AuthN/AuthZ** — missing checks, privilege escalation, IDOR, broken session handling.
   - **Crypto** — weak/rolled-your-own crypto, static IV/salt, insecure randomness.
   - **Memory/concurrency** (systems code) — UB, unchecked `unsafe`, races, resource leaks.
   - **Dependencies** — new deps with known CVEs (`cargo audit`/`npm audit` if available, offline).
   - **Exposure** — overly broad file/network permissions, verbose errors leaking internals.
3. Report each finding as `file:line` · severity (Critical/High/Med/Low) · problem · fix.
4. State plainly when the diff is clean.

## Rules

- Only flag issues justifiable from the code; no vague "could be more secure".
- Prefer the fix that reuses existing validation/helpers in the codebase.
