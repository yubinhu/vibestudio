# Agent integrations

VibeStudio integrates installed CLIs through the shared
[`AgentDef` registry](../server/skill-core/src/agents.rs). Detection runs on the
active server, including SSH hosts; install and authenticate the CLI on that host.
Restart the host after installing an agent so its executable inventory refreshes.

## Hermes and Pi

| Capability | Hermes | Pi |
| --- | --- | --- |
| Verified upstream | [v2026.9.14](https://github.com/NousResearch/hermes-agent/releases/tag/v2026.9.14) | [0.86.1](https://www.npmjs.com/package/@earendil-works/pi-coding-agent/v/0.86.1) |
| Terminal launch and mining | Interactive CLI; `chat -q` for submitted prompts | Interactive CLI; positional prompt |
| Native title | Exact per-TTY breadcrumb → SQLite session | Assigned session ID → JSONL session |
| Rename in VibeStudio | Persistent local override | Persistent local override |
| Status and assistant preview | Bundled detector; native message text | Bundled detector; active-branch message text |
| Continue a mining run | Unavailable: native latest-session lookup can cross workspaces | Cwd-scoped `--continue` |
| Personal skills | `~/.hermes/skills`, including nested categories | `~/.pi/agent/skills` plus shared `~/.agents/skills` |
| Project skills | `.hermes/skills`; Hermes can also read trusted `.agents/skills` | `.pi/skills` and `.agents/skills` |
| Managed MCP connector wiring | Unavailable | Unavailable |

Hermes honors `HERMES_HOME` and its active profile. Pi honors
`PI_CODING_AGENT_DIR`; session reads also honor `PI_CODING_AGENT_SESSION_DIR` and
native project/global `sessionDir` settings. Bundled skills use Hermes's active
profile and Pi's shared skills folder. The shared group does not claim global
Hermes access: that requires the user's explicit external-directory configuration.

Current Pi launches receive `--session-id`; older detected versions retain their
normal CLI arguments. Explicit continue/resume, custom session directories, and
nonpersistent sessions retain the user's arguments. Native titles can be absent
when the store cannot be correlated uniquely. Pi's explicit absolute `--session`
file is readable; arbitrary CLI storage overrides are not inferred. Hermes's
optional Node TUI uses a different identity channel; native titles currently
cover its standard Python CLI. Persistent VibeStudio names work in either mode.

Both agents can run skill-mining tasks; selectable history sources remain the
existing Claude Code/Codex inputs.

No model runs are needed to read titles. Readers use existing local stores and
never rewrite them; Reset restores the current native title or normal cwd label.

Upstream contracts: [Hermes CLI](https://hermes-agent.nousresearch.com/docs/reference/cli-commands),
[Hermes skills](https://hermes-agent.nousresearch.com/docs/user-guide/features/skills/),
[Hermes terminal identity](https://github.com/NousResearch/hermes-agent/blob/v2026.9.14/hermes_cli/terminal_breadcrumbs.py),
[Pi CLI and skills](https://github.com/earendil-works/pi/tree/main/packages/coding-agent),
[Pi session format](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/session-manager.ts).

## Adding an agent

1. Verify a released CLI's interactive launch, prompt submission, model flags,
   native stores and skill paths. Leave undocumented capabilities unset.
2. Add its registry entry. Executable discovery, skill groups, project scanning,
   bundled installs, sync destinations and generic session UI consume the entry.
3. Add any agent-specific home/profile resolution, native session reader and
   exact terminal identity mapping. Never substitute another session when a
   recorded ID is missing. Add a status detector only if one is not already bundled.
4. Add optional frontend color/path identity hints in `client/web/lib/agents.ts`.
   Existing HTTP terminal, mining, skill and rename routes serve the integration.
5. Validate isolated fixtures for concurrent sessions, renames, malformed stores,
   launch flags and skill locations. Exercise the picker, native title and local
   rename through HTTP/UI on private config and tmux state. Keep native stores
   read-only and do not run paid model requests as a test side effect.

MCP and resume are optional; they do not gate terminal launch. A fully custom
connector protocol or new API capability still follows the
[HTTP feature recipe](../design.md#adding-a-feature).
