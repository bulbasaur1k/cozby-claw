---
name: grounding
description: Before editing, pull exact signatures, types and call sites from the local codebase so you don't hallucinate APIs. No LSP, no network.
---

# grounding

Weak models invent function signatures, field names and imports. Prevent it by
grounding every edit in facts you retrieve **locally** first — never guess an API
you have not seen. Uses only `grep`/`ripgrep`, file reads and `cargo doc`/`--help`;
**no LSP, no external services.**

## Before you edit a symbol

1. **Definition** — find where it is defined:
   `rg -n "fn <name>|struct <name>|class <name>|def <name>"` (or the language's form).
   Open the file and read the exact signature, parameters and return type.
2. **Types/fields** — for a struct/class/DTO you touch, read its real fields; do
   not assume names.
3. **Call sites** — see how it is already used: `rg -n "<name>\("`. Match the
   existing calling convention.
4. **External crates/libs** — read the real API from local sources only:
   - Rust: `cargo doc --no-deps` output, or read the dep source under the vendored/registry path; `cargo tree` for versions.
   - JS/TS: read `node_modules/<pkg>/dist/*.d.ts` for the actual types.
   - CLI tools: `<tool> --help`.
5. Only after you have the real signatures in context, write the edit.

## Rules

- If you cannot find the definition locally, say so and search wider — do not fabricate it.
- Prefer reusing an existing helper you found over inventing a parallel one.
- Keep retrieved snippets in context while editing so the types stay correct.
