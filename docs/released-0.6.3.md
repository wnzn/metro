# New in Metro 0.6.3

These changes are included in the published
[Metro 0.6.3 release](https://github.com/wnzn/metro/releases/tag/v0.6.3).
See [Acceptance](acceptance.md#published-metro-063-2026-11-27-github-release-v063)
for the release files and image digests, and [New in Metro 0.6.2](released-0.6.2.md)
for earlier features.

## Changes

- **Agents can write files.** A new runner tool, `workspace.write`,
  creates or replaces one UTF-8 text file (at most 1 MiB) in the Space's
  folder. It follows the same path rules as reading: no absolute paths,
  no `..`, no links. It never asks for approval. See
  [Tools](runner.md#tools).
- **Agents can run shell commands.** A new runner tool, `workspace.shell`,
  runs one command with `/bin/sh -c` (on Windows, `cmd.exe`) in the
  Space's folder, for up to 120 seconds by default and 600 at most. It
  works on dedicated and registered runners, standalone and built in. A
  stopped run kills its command. See
  [Shell commands](runner.md#shell-commands).
- **Both tools are in the default tool list.** New agents, including a new
  Space's default agent, get them unless their owner picks a list.
  Agents created before 0.6.3 keep their saved list; a Space owner adds the
  tools to them.
  Fixed in 0.6.4 (in the source, not released): a one-time startup migration adds the missing write and shell tools to saved lists that still hold all five earlier default runner tools; see [In the source, not released](next-source.md).
- **Shell policy.** Each runner has a shell mode: **Off**, **Ask every
  time** (the default), **Ask when System One flags it** or **Always
  allow**, and who approves: the runner's owner (the default; **Org
  admin** for an organization runner, **Space owner** for a dedicated one)
  or **Any Space member**. A registered runner's manager sets its default and can
  override it per Space; a dedicated runner's policy is set by the Space
  owner. On the desktop's built-in runner, **Always allow** applies only
  while it's attached to exactly one Space. Organization runners can't use
  **Ask when System One flags it**. See
  [Shell policy](runner.md#shell-policy).
- **Approval cards.** A command that needs approval shows a card in the
  room with **Approve** and **Deny** for eligible approvers. The first
  decision wins; it expires after 10 minutes, and is cancelled when the
  run stops or the Space's runner changes. Cards are run activity, never
  messages, agent history or search results. Waiting for approval doesn't
  use up the run's time limit. See [Approvals](runner.md#approvals).
- **System One checks commands.** With **Ask when System One flags it**,
  the runner owner's System One connection judges each command. Only a
  confident *safe* runs without asking. See
  [Shell command checks](system-one.md#shell-command-checks).
- **A folder per Space for this computer's runner.** Under **Settings →
  Runners → This computer's runner**, **Change** points an attached Space
  at an existing folder, such as a project, and **Use default** goes back.
  Metro refuses roots, the home folder itself, links, Metro's own data
  folder and folders that overlap another Space's. See
  [Folders and shell commands](install.md#folders-and-shell-commands).
- **`METRO_WORKSPACE` is optional.** A standalone runner without it uses
  the folder it was started in, and refuses `/`. See
  [Environment](runner.md#environment).
- **Shell in the Docker sandbox.** With `METRO_SANDBOX=docker`, shell
  commands run in the locked-down container with no network and only the
  Space's folder mounted, read-write. Check and MCP stay read-only and,
  on registered runners, sandbox only.

**Shell commands run as the runner's user**, with that user's access to
the machine, including other Spaces' folders there unless the Docker
sandbox is on. The built-in runner has no sandbox. Choose the approver
with that in mind; System One's answer is a model's judgement, not
containment.

Known limits: on Windows, a process started in the instant before it joins
the command's job object may survive a stop, and refusing a junction as a
chosen folder hasn't been checked on a real Windows machine.

For the API, see [Shell policy API](runners.md#shell-policy-api).

Later changes are [in the source, not released](next-source.md).
