# Terminal client (`metro-tui`)

> **Status: no release yet.** `metro-tui` has been in the source since
> `142bc85`. CI now builds it into every desktop download, including the
> unsigned [test builds](install.md#test-builds-from-github-actions), but no
> public release containing it exists yet. The
> [57409bb setup cut](acceptance.md#staged-installers-and-setup-at-57409bb-2026-11-23-project-manager-local)
> doesn't include it. Its test runs the real Linux binary in a
> pseudo-terminal against a real Metro server with a scripted model
> ([acceptance](acceptance.md#source-only-terminal-client-at-142bc85-2026-11-23-project-manager-local)).
> It hasn't been run on native Windows or macOS, or against a real vendor.

`metro-tui` lets you read and write your personal Chat and your Spaces' rooms
from a terminal, including over SSH. It talks to a Metro server over its
HTTP API with your own token, like the desktop app does. It doesn't need
GTK, the desktop app or a display, and it never reads Metro's database.

## Where it is

It's inside each desktop download. A release will have it in the same
places; no release exists yet.

| Download | Where `metro-tui` is |
| --- | --- |
| `Metro-<version>-linux-x64.tar.gz` | `metro-tui`, next to the `Metro` launcher |
| `Metro-<version>-windows-x64.zip`, or the MSI's install folder | `metro-tui.exe`, next to `Metro.exe` |
| `Metro-<version>-macos-arm64.tar.gz` / `-macos-x64.tar.gz` | inside the app: `Metro.app/Contents/MacOS/metro-tui` |

Run it from a terminal in that folder, for example `./metro-tui --server …`
on Linux or `.\metro-tui.exe --server …` in Windows Terminal or PowerShell.

## Start it

```text
$ metro-tui --help
Metro conversations in an interactive terminal

Usage: metro-tui [OPTIONS] --server <ORIGIN>

Options:
      --server <ORIGIN>    Explicit Metro origin (HTTPS, or HTTP with a literal loopback IP)
      --token-file <PATH>  Read a token from a private regular file instead of prompting without echo
  -h, --help               Print help
  -V, --version            Print version
```

- **`--server` is required.** Use the server's `https://` origin, such as
  `https://metro.example.com`. Plain `http://` is accepted only for a
  literal loopback IP with its port, such as `http://127.0.0.1:8787` or
  `http://[::1]:8787`; not `localhost`. A path, query, fragment or
  credentials in the address are refused. Redirects aren't followed, and
  proxy environment variables aren't used.
- **Your token.** Without `--token-file`, it asks for `Metro token:` and
  doesn't echo what you type or paste. With `--token-file PATH`, the file
  must be a regular file. On Linux and macOS it must also belong to you and
  give no access to group or others (`chmod 600`), and symbolic links are
  refused; otherwise it refuses to start. On Windows it checks only that
  the path is a regular file, not its permissions, so keep the file in a
  folder only you can read. Never put a token on the command line.
- It needs an interactive terminal. Over SSH, allocate one (`ssh -t`).
- If the server refuses the token, it says so and exits. A mistaken
  argument is never echoed back, in case it held a credential.

## Keys

Press **F1** in the app for this list.

| Key | Does |
| --- | --- |
| **F1** / **Esc** | Open or close help |
| **F10** / **Ctrl-C** | Quit |
| **Tab** | Switch between the conversation list and the draft |
| **↑ / ↓** | Move through Spaces, rooms or models |
| **Enter** | Expand a Space, open a conversation, or choose a model. In the draft, the action shown below it |
| **F2** | Ask agent explicitly (adds `@agent` to any recipients) |
| **F3** | Send, or **Save note** in personal Chat; typed mentions and server routing still apply |
| **F4** | Pick a model and an advertised thinking level |
| **F5** | Refresh Spaces, rooms, the transcript and available models |
| **F6** | Insert a newline in the draft |
| **F7** | Hide or show the sidebar in a wide terminal |
| **PgUp / PgDn** | Scroll the transcript while the draft has focus |
| **← / →**, **Home / End** | Move the caret; jump to the draft's start or end |

The line under the draft names what **Enter** and **F3** do right now, for
example `Enter Ask agent · F2 Ask · F3 Save note`.

## What it does, and doesn't

- **Who and what you see.** You're the person the token belongs to. You see
  your personal Chat and only the Spaces you're a member of now, with their
  rooms. Other Spaces never appear. Every request is checked by the server
  as usual.
- **Addressing is the same as in the app.** In a room, type standalone
  `@agent` or `@member-id`. A people-only message never starts the agent.
  **F2** always asks the agent. An unaddressed **Send** follows your Metro
  settings.
- **Personal Chat.** When a chat model is ready for you and you haven't set
  up System One, **Enter** on a typed question is **Ask agent**, and **F3**
  is **Save note**, which saves it without calling any model. Otherwise
  **Enter** sends, and **F2** asks.
- **Model and thinking level (F4).** It lists only the models the server
  offers you in that place, and only the levels it advertises for each. The
  server checks your choice when you send. If it's no longer available, the
  draft stays and you're asked to choose again with F4; it never falls back
  to another model.
- **Drafts** are kept per conversation in this process only, never written
  to disk. If a draft contains your own sign-in token, it refuses to send it.
- Untrusted text from messages is shown as text; terminal control sequences
  in it aren't run.
- It has no attachments, Library, search, settings, runners or monitoring.
  Use the desktop app for those.

## Help

Never send anyone your token, token file or raw terminal recordings that
might show it. See the
[desktop troubleshooting](../apps/desktop/README.md#troubleshooting).
