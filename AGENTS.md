# AGENTS.md

Guidance for people and coding agents who work on Metro.

## Authorship policy (build process)

This is how the work is split while building Metro. It is **not** runtime
behaviour. Metro does not route requests to these models; at runtime it uses
whatever model `METRO_MODEL` names.

| Role | Owner |
| --- | --- |
| Project manager and orchestrator: decides architecture, scope and trade-offs; assigns and directs subagents; accepts results only once they are verified | **Astra 6.2** |
| Writing (README, AGENTS.md, docs/), frontend (`apps/desktop`), complex code | **Opus 5.5**, only through OpenChamber subagents |
| Mid-level code (assigned to either, at Astra's discretion) | **Sol 6.2** or **Sonnet 5.5**, both through OpenChamber subagents |

Opus 5.5 never edits directly in the root session, and nobody calls the
Claude CLI directly. Opus contributes only as an OpenChamber subagent. These
are build assignments only. They don't affect which model Metro calls at
runtime.

## Layout

- `crates/`: Rust server (Axum, SQLite, Rig agent), auth, and outbound runner
- `tests/e2e/`: `smoke.py` and `threads.py`, run against the real binaries
  with a local scripted model
- `apps/desktop`: TSX desktop app on [Native Shell (NSH)](https://nsh.dev).
  Feature components use only semantic props. Styling lives in
  `theme/theme.json` and `src/ui/`, which must stay replaceable.
- `docs/`: user and operator docs
- `compose.yaml`, `Dockerfile`, `.github/workflows/ci.yml`: local stack, CI,
  and tagged GHCR releases

## Rules

- Stay inside the files you were assigned. Don't edit another lane's files;
  report what needs to change instead.
- Never commit secrets. Generate tokens locally, and keep `.env` out of git.
- Access is per **Space**. Only members see a Space or its rooms. Only the
  owner manages members and the runner token. Outsiders and unknown ids get
  404, non-owners get 403, and nobody can add themselves. Users are identified
  by their stable identity-provider id (`sub`, or the dev username), never by
  display name.
- Runner tokens are per Space: issued by the owner, shown once, stored only as
  a hash, rotated on reissue and revocable. Don't add a server-wide runner
  token, a fallback Space, or anything that lets one Space's runner serve
  another Space.
- Never print, log or screenshot a token. Docs and scripts write tokens to
  mode-600 files and read them from there.
- Mentioning a person (`@bob`) never starts agent work. Only `@agent` or Ask
  agent does. The agent's history shows each message's author and
  recipients.
- Reactions never start agent work and never imply that work is complete.
- Threads stay one level deep and bounded, and inherit their Space's members
  and runner. The personal Chat agent's only tool is the scoped
  `react_to_message` acknowledgment. It has no runner, workspace or thread
  tools, and can't read Space transcripts. Cross-Space run controls are for
  the human, and they check membership.
- The database is pinned to one identity namespace. Never assign legacy or
  foreign history to anyone.
- *Demo* must stay deterministic and must never call a model.
- Sandboxing is opt-in, and `docker` mode fails closed. Check and MCP commands
  are operator-defined JSON argv and are never run through a shell.
- The docs must not claim features that aren't implemented, or results that
  nobody recorded. When code and docs disagree, fix the docs. Record each
  verification run in `docs/acceptance.md`.
