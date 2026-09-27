---
name: ui-compare
description: Open and configure VibeStudio's live desktop UI comparison when making substantial visual changes or reviewing saved, uncommitted frontend edits against a pinned Git baseline. Supports separate working directories, worktrees, dev servers and existing preview URLs.
---

# Live UI comparison

Use this while developing a significant UI change so the user can review the
baseline and saved edits side by side. VibeStudio desktop must be running on the
machine that hosts the previews. Each UI diff is an artifact of an agent session,
with a title, review notes, exact host/session identity, and a pinned baseline.
The Sessions UI lists these artifacts. Each running preview owns its baseline
checkout and servers; the working directory stays in place.

Run `scripts/compare.py` relative to this skill directory. It discovers the
desktop's HTTP listener from VibeStudio's config directory; set
`VIBESTUDIO_COMPARISON_URL` or pass `--server` for an explicit listener (including
an agent's tunnel back to the desktop). Paths and commands are interpreted on
the desktop machine, independently of the workspace selected in VibeStudio.

1. Inspect the project's frontend command, dependencies and backend needs. Pick
   the actual working directory: it may be the main repo, another worktree, or
   an agent's checkout in a temporary directory. Preserve the user's choices.
2. Write a configuration file using [the configuration reference](references/configuration.md).
   Start with `python3 scripts/compare.py start --config /tmp/compare.json --title "Account layout" --description "Review navigation spacing" --wait`.
   The helper associates the invoking VibeStudio tmux terminal automatically and
   enriches its native conversation ID when available. Outside that terminal,
   supply `--session <terminal-id>` or an explicit `artifact.owner` in the config.
   For an SSH agent, also supply `--host` with the exact workspace identifier shown
   in VibeStudio. Never choose a session by matching its repository or folder.
   Retain the returned artifact ID. `--wait` reports startup errors; a timeout
   leaves the preview running for inspection or explicit stopping.
3. Edit the working component and save. Its own HMR updates the working preview;
   the baseline remains at the resolved commit. The user can set the matching
   viewport, route and scroll sync in the comparison window.
4. Use `status ID`, or `update ID --config /tmp/update.json`, to inspect or adjust
   an existing artifact. Keep it available while the user reviews. `close ID`
   closes only its window; `open ID --wait` opens or focuses it. The user can also
   use **Sessions → UI diffs** or the comparison window's session selector.
5. Use `stop ID --wait` to release preview servers and the temporary checkout.
   The artifact remains in the session's list. Reopening uses the same pinned
   commit, including after desktop restart; provide `open ID --config …` again
   if its environment values were omitted from saved metadata. To compare a new
   baseline or different source directories, create a new artifact.

Use separate artifacts for simultaneous agent branches. Do not reset, stash or
commit the working tree to prepare a preview. Do not kill existing servers;
passing an existing URL leaves its lifecycle with its owner. A URL baseline is
externally managed, so its content is not guaranteed to stay pinned.

Only browser-compatible frontends are supported; native-only toolkits are out
of scope. These are responsive web previews, not device simulators. Tauri/Electron
frontends that call native commands need an explicit backend adapter or preview
mocks. Arrange isolated backend fixtures when previewing state-changing screens.
