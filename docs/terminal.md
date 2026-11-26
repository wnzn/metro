# Terminal client (`metro-tui`)

The terminal follows the
[0.5.0 server transport policy](released-0.5.0.md#http-tailscale-and-proxies).

> **Status: released in [Metro 0.3.2](https://github.com/wnzn/metro/releases/tag/v0.3.2).**
> `metro-tui` has been in the source since `142bc85`. CI builds it into
> every desktop download, including the unsigned
> [test builds](install.md#test-builds-from-github-actions). The
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

It's inside each desktop download, including the 0.6.0 release archives.

| Download | Where `metro-tui` is |
| --- | --- |
| `Metro-<version>-linux-x64.tar.gz` | `metro-tui`, next to the `Metro` launcher |
| `Metro-<version>-windows-x64.zip`, or the MSI's install folder | `metro-tui.exe`, next to `Metro.exe` |
| macOS: no download for now ([install.md](install.md#desktop-on-macos)); the packaging puts it | inside the app: `Metro.app/Contents/MacOS/metro-tui` |

Run it from a terminal in that folder, for example `./metro-tui --server …`
on Linux or `.\metro-tui.exe --server …` in Windows Terminal or PowerShell.

## Start it

```text
$ metro-tui --help
Metro conversations in an interactive terminal

Usage: metro-tui [OPTIONS] --server <ORIGIN>

Options:
      --server <ORIGIN>    Explicit Metro origin (HTTPS or HTTP; plain HTTP outside loopback and Tailscale prints a warning)
      --token-file <PATH>  Read a token from a private regular file instead of prompting without echo
  -h, --help               Print help
  -V, --version            Print version
```

- **`--server` is required.** Use the server's `https://` origin, such as
  `https://metro.example.com`, or an SSH forward. Plain HTTP outside
  loopback and recognized Tailscale hosts prints a warning. A path, query, fragment or
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
  `@agent`, an agent's `@handle` or `@member-id` (the server resolves them).
  A people-only message never starts an agent. **F2** always asks the
  generic agent (the Space's default agent, or your classifier's pick). An
  unaddressed **Send** follows your Metro settings for that Space.
- **Personal Chat.** **F2** explicitly asks, and the draft hint shows which
  key sends or saves a note. Unlike the desktop composer, the terminal
  uses the legacy `invoke_agent` field: a note does not start a run unless
  its text explicitly mentions `@agent`.
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
