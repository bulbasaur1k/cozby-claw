# SECURITY — Air-Gap & Local-LLM Deployment Guide

This document is the result of a full InfoSec audit of every outbound network
call `cozby-claw` can make. If you are deploying inside a closed corporate
network with a local LLM, read this end-to-end before running the binary.

## TL;DR — what it does not do

- No built-in telemetry ever transmits data to a remote endpoint. The
  `telemetry` crate writes to a local JSONL file only.
- No auto-update / crash-reporting / version-check / analytics calls exist.
- No background threads, cron jobs, or scheduled tasks make network calls.
- No git submodules or build-time downloads. All Rust dependencies come from
  the standard `crates.io` index.
- No hard dependency on any Anthropic / OpenAI / GitHub host — every endpoint
  is redirectable via environment variables or config files.

## TL;DR — what it can do if misconfigured

- Call whatever LLM endpoint `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL` /
  `XAI_BASE_URL` points to. **Defaults are public provider endpoints.**
- Call arbitrary URLs if the user invokes the built-in `WebFetch`,
  `WebSearch`, or `RemoteTrigger` tools.
- Perform OAuth token exchange against whatever `authorize_url` /
  `token_url` is loaded from the config file.
- Spawn external MCP server processes or connect to MCP servers over
  stdio / SSE / HTTP / WebSocket as configured.

## Complete list of outbound call sites

| # | Trigger | Default URL | Override env var | Source |
|---|---|---|---|---|
| 1 | Anthropic messages | `https://api.anthropic.com` | `ANTHROPIC_BASE_URL` | [rust/crates/api/src/providers/anthropic.rs](rust/crates/api/src/providers/anthropic.rs) |
| 2 | OpenAI chat completions | `https://api.openai.com/v1` | `OPENAI_BASE_URL` | [rust/crates/api/src/providers/openai_compat.rs](rust/crates/api/src/providers/openai_compat.rs) |
| 3 | xAI chat completions | `https://api.x.ai/v1` | `XAI_BASE_URL` | [rust/crates/api/src/providers/openai_compat.rs](rust/crates/api/src/providers/openai_compat.rs) |
| 4 | OAuth token exchange & refresh | `config.token_url` from settings | settings file | [rust/crates/api/src/providers/anthropic.rs](rust/crates/api/src/providers/anthropic.rs) |
| 5 | `WebFetch` tool | user-supplied URL | n/a — tool input | [rust/crates/tools/src/lib.rs](rust/crates/tools/src/lib.rs) |
| 6 | `WebSearch` tool | `https://html.duckduckgo.com/html/` | `CLAWD_WEB_SEARCH_BASE_URL` | [rust/crates/tools/src/lib.rs](rust/crates/tools/src/lib.rs) |
| 7 | `RemoteTrigger` tool | user-supplied URL (any verb) | n/a — tool input | [rust/crates/tools/src/lib.rs](rust/crates/tools/src/lib.rs) |
| 8 | MCP transports | per-server URL/command from settings | settings file | [rust/crates/runtime/src/mcp_stdio.rs](rust/crates/runtime/src/mcp_stdio.rs), [mcp_client.rs](rust/crates/runtime/src/mcp_client.rs) |

No other `reqwest`, `hyper`, `ureq`, `TcpStream`, `UdpSocket`, or WebSocket
callers exist in the workspace.

## Recommended air-gapped configuration

### 1. Environment

Point every LLM endpoint at your internal inference server. Any
OpenAI-compatible gateway (llama.cpp server, vLLM, Ollama OpenAI-compat, TGI,
a corporate proxy that re-forwards to an on-prem model) will work:

```bash
export OPENAI_BASE_URL="http://inference.internal:8080/v1"
export OPENAI_API_KEY="dummy"        # any non-empty value
# OR, for Anthropic-compatible proxies:
export ANTHROPIC_BASE_URL="http://inference.internal:8443"
export ANTHROPIC_API_KEY="dummy"

# Explicit egress controls
export HTTPS_PROXY="http://egress-proxy.internal:3128"
export NO_PROXY="localhost,127.0.0.1,.internal"
export SSL_CERT_FILE="/etc/ssl/certs/internal-ca.crt"

# Disable DuckDuckGo default for web search (point at an internal search or
# leave the tool disabled — see below)
export CLAWD_WEB_SEARCH_BASE_URL="http://search.internal:9000"
```

### 2. Disable the three "open-world" tools

For the strongest isolation, disable `WebFetch`, `WebSearch`, and
`RemoteTrigger` via the permission system. In `.claw/settings.json`:

```json
{
  "permissions": {
    "deny": ["WebFetch", "WebSearch", "RemoteTrigger"]
  }
}
```

Or run with a restrictive permission mode:

```bash
./target/debug/claw --permission-mode read-only prompt "..."
```

### 3. OAuth — only use an internal IdP (or skip entirely)

If you use OAuth, make sure `authorize_url` and `token_url` in the settings
point **only** at your internal IdP:

```json
{
  "oauth": {
    "clientId": "cozby-claw-internal",
    "authorizeUrl": "https://sso.internal/oauth/authorize",
    "tokenUrl":     "https://sso.internal/oauth/token",
    "scopes": ["openid"]
  }
}
```

If you do not need OAuth, omit the `oauth` block entirely and the code path
is never exercised.

### 4. MCP — keep transports local

`cozby-claw` speaks MCP over `stdio`, `sse`, `http`, `ws`. Only register MCP
servers whose transport stays on the loopback interface or your trusted
internal network. For example:

```json
{
  "mcpServers": {
    "cozby-local-tools": {
      "type": "stdio",
      "command": "./target/debug/cozby-mcp",
      "args": ["--root", "/srv/project"]
    },
    "internal-search": {
      "type": "http",
      "url": "http://mcp.internal/search"
    }
  }
}
```

### 5. Verification checklist

Before trusting a deployment, verify:

```bash
# 1. No Anthropic/OpenAI/xAI default sneaking through
env | grep -E 'ANTHROPIC_BASE_URL|OPENAI_BASE_URL|XAI_BASE_URL'

# 2. Proxy / CA bundle in place
env | grep -E 'HTTPS_PROXY|NO_PROXY|SSL_CERT_FILE'

# 3. Tool denylist is active
./target/debug/claw --print-config | jq '.permissions.deny'

# 4. DNS/egress check — watch for surprise hosts during a session
sudo tcpdump -ni any 'port 443 or port 80' &
./target/debug/claw prompt "list files"
```

## Data that leaves the machine (once configured)

Assuming the recommended configuration above:

- Conversation content and tool results are sent only to the URL in
  `OPENAI_BASE_URL` / `ANTHROPIC_BASE_URL` (your local inference server).
- Nothing else. No session metadata, no error reports, no usage counters.

## Reporting an issue

Open an issue on the repository. Do not email the maintainers outside the
repository; there is no private channel.
