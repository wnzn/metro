# Metro 0.6.4 (in the source, not released)

- **Older agents get the write and shell tools.** In 0.6.3, agents
  created before that release kept their saved tool list without
  `workspace.write` and `workspace.shell`. When the server first starts
  on this source, a one-time migration adds whichever of the two is
  missing to every saved list that holds all five earlier default
  runner tools: `workspace.list`, `workspace.read`, `workspace.check`,
  `workspace.mcp_list` and `workspace.mcp`. Every existing entry is
  kept. A list missing any of the five was narrowed by its owner and is
  left untouched; the owner can still add the tools. If an owner later
  removes them, the migration doesn't run again. Runner, sandbox and
  [shell policy](runner.md#shell-policy)
  gating are unchanged, so shell commands still ask by default. See
  [Agents](spaces.md#agents) and [Tools](runner.md#tools).
