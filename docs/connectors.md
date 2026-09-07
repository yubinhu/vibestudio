# Connector discovery

Connectors brings together service connections and stored API keys/secrets used
by agents. Service availability is a list of observations from configuration or
an authenticated runtime. Secret storage keeps its own API and management
controls; both types contribute to the Connectors overview count.

## Presentation

- Services and API keys & secrets are visible together on the Connectors page,
  with equally prominent sections and their own counts. `/connectors#secrets`
  jumps to the secret store within that page. Home includes both in its overview
  total and shows their counts and management links in a quiet page footer.
- One service row, with prominent agent and status badges. CLI, desktop and IDE
  clients that share configuration are described together, not counted as three
  connections. Claude Desktop's additional local configuration has its own source.
- Filter by agent, search by name/host, or select a project on the active server.
  Expand a row for the precise configuration source, project and client scope.
- **Configured** means a definition or plugin app reference was found, or an
  account app is enabled. It does not imply a successful network connection,
  valid OAuth grant, trusted project or installed executable.
- **Connected** requires a runtime observation. **Sign-in needed** and **Error**
  can also come from a VibeStudio-managed connection's stored authentication
  status. **Disabled** comes from configuration or the runtime. Conflicting
  scopes remain visible in the expanded source list.
- Source coverage distinguishes a scan failure or unsupported runtime from an
  empty result. Catalog entries that merely advertise an available service are
  not counted as connected services.
- VibeStudio-managed connectors retain their reconnect/disconnect controls.
  Discovery does not transfer another client's credentials or rewrite its config.
- Refresh reads settings and retains dated results from the last agent check.
  Check agents replaces those runtime results. Managed connection changes
  invalidate cached evidence, including project inventories.

## Current coverage

| Agent | Configuration discovery | Explicit runtime check |
| --- | --- | --- |
| Claude Code | User and project MCP definitions, enabled plugin manifests | MCP status from an authenticated no-prompt runtime; cloud inventory can be incomplete |
| Claude Desktop | Local Chat MCP file, with its additional Desktop Code scope identified | Desktop extensions and cloud account inventory are not queried directly |
| Codex | Shared CLI/desktop/IDE TOML configuration, plugin MCP definitions and `.app.json` references, including packages with local remote-install records | Installed account apps and their enabled/callable state; MCP live status is not yet queried |
| OpenCode | Global and project JSON/JSONC, overrides, v1 and v2 MCP formats | Configuration only |
| Cursor / Gemini CLI | Registered user and project JSON MCP sources | Configuration only |

Runtime probes currently run on macOS and Linux. Other platforms report that
runtime coverage is unavailable and still perform configuration discovery. A
Claude runtime returning no MCP entries does not prove the account has no cloud
connectors; the inventory exposes that coverage limitation explicitly. ChatGPT
account connections are discovered only where the authenticated Codex runtime
exposes installed apps, not by inspecting browser sessions.

Codex app-only plugins do not need an MCP server definition or an entry in local
`config.toml`. Ordinary discovery reads their declared `.app.json` files when a
plugin is selected by configuration or has a valid local remote-install record.
Unmarked cache directories are not installation evidence. Remote-install records
can survive a failed operation or an account change, so these sources describe
local plugin files and leave account access unverified. App IDs, rather than
plugin names, join these references with runtime results, avoiding duplicate
Gmail rows across plugin and account observations. A fresh page load rediscovers
these references; it does not depend on a previous Check agents result.

## HTTP and extension points

`GET /api/connectors/discover?project=<absolute-directory>` passively reads
configuration on the active server. `POST /api/connectors/check` with optional
`{"project":"/absolute/directory"}` combines that inventory with explicit runtime
checks. Both are ordinary proxied routes, so remote/phone clients see the remote
host's inventory.

`skill-core::connectors` owns the common DTO and configuration readers;
`skill-core::connector_runtime` owns bounded runtime probes. `AgentDef` declares
`connector_discovery` and `connector_runtime` capabilities. A simple JSON-based
agent can use the declarative JSON adapter; a different format or protocol gets
an adapter implementation. The registry supplies labels and client variants to
the UI, so adding a new agent requires no frontend family-name branch.

The connector identity is an opaque hash of the canonical endpoint, or local
launch definition and its execution scope. Sources and project scopes are
retained when identities merge.
Hostnames are for display only, never a basis for assuming two accounts/endpoints
are identical. Managed gateway URLs are correlated with VibeStudio's connection
records; a historical list of configured agents is not proof that their current
files still contain the gateway.

The API projection is an allowlist. It omits full endpoint URLs, commands,
arguments, environment values, request headers and authentication material. Parse
errors use fixed messages rather than including a fragment of a secret-bearing
configuration file. Passive scans never execute configured commands. Explicit
runtime checks may initialize an agent and its enabled MCP servers; probes have
time/output limits and clean up the subprocesses they launch.

## Documentation used

Reviewed September 6, 2026. Installed versions can lag these interfaces; probe
capabilities and report unsupported versions instead of assuming current support.

- [Claude Code MCP](https://code.claude.com/docs/en/mcp): user/project scopes,
  plugin servers, precedence and Claude.ai connectors. Subscription-authenticated
  runtimes can load account connectors that do not exist in local MCP files.
- [Claude Desktop Code](https://code.claude.com/docs/en/desktop#mcp-servers-from-the-claude-desktop-chat-app):
  local Code sessions also read Desktop Chat MCP configuration. The standalone
  CLI does not automatically inherit that file.
- [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk/python#mcpstatusresponse):
  structured MCP status, including the output-only `claudeai-proxy` transport.
- [OpenAI MCP documentation](https://learn.chatgpt.com/docs/extend/mcp): shared
  host configuration for desktop, CLI and IDE, trusted project scope and plugins.
- [OpenAI App Server](https://learn.chatgpt.com/docs/app-server):
  `mcpServerStatus/list`, `app/list` and `app/installed`. Availability, enabled
  state and callable tools are distinct from a verified service authorization.
- [OpenAI plugin app references](https://learn.chatgpt.com/docs/enterprise/plugin-management#reference-an-existing-app-with-appjson):
  the `apps` manifest component points to `.app.json`, which maps app names to
  stable app IDs. References do not grant service permissions.
- [OpenCode configuration](https://opencode.ai/docs/config/) and
  [MCP](https://opencode.ai/docs/mcp-servers/): JSON/JSONC, global/project/config
  overrides and enablement. [V2](https://opencode.ai/v2/docs/mcp-servers) introduces
  `mcp.servers` and `disabled`, so readers must recognize both config shapes.

Configuration discovery is deliberately best effort. Dynamic plugin hooks,
organization-delivered defaults, desktop extensions and cloud-only accounts may
need runtime-specific adapters. Report the coverage limitation rather than
silently treating undiscovered services as disconnected.
