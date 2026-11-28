# New in Metro 0.6.4

This change is included in the published
[Metro 0.6.4 release](https://github.com/wnzn/metro/releases/tag/v0.6.4).
See [Acceptance](acceptance.md#published-metro-064-2026-11-28-github-release-v064)
for the release files and image digests, and [New in Metro 0.6.3](released-0.6.3.md)
for earlier features.

## Changes

- **Older agents get the write and shell tools.** In 0.6.3, agents
  created before that release kept their saved tool list without
  `workspace.write` and `workspace.shell`. When the server first starts
  on 0.6.4, a one-time migration adds whichever of the two is
  missing to every saved list that holds all five earlier default
  runner tools: `workspace.list`, `workspace.read`, `workspace.check`,
  `workspace.mcp_list` and `workspace.mcp`. Every existing entry is
  kept. A list missing any of the five was narrowed by its owner and is
  left untouched; the owner can still add the tools. If an owner later
  removes them, the migration doesn't run again. Runner, sandbox and
  [shell policy](runner.md#shell-policy)
  gating are unchanged, so shell commands still ask by default. See
  [Agents](spaces.md#agents) and [Tools](runner.md#tools).

Later changes are [in the source, not released](next-source.md).
