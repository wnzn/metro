# New in Metro 0.6.2

These changes are included in the published
[Metro 0.6.2 release](https://github.com/wnzn/metro/releases/tag/v0.6.2).
See [Acceptance](acceptance.md#published-metro-062-2026-11-27-github-release-v062)
for the release files and image digests, and [New in Metro 0.6.1](released-0.6.1.md)
for earlier features.

## Changes

- **A runner built into the desktop app.** **Settings → Runners → This
  computer's runner → Set up** makes the app's built-in runner your
  personal registered runner on whichever server you're signed in to,
  local or remote, with no separate install. It runs while Metro is open,
  updates with the app, lists and reads only, and is attached only where
  you choose. Deleting it there turns it off on this computer and keeps
  its folders. In 0.6.1 and earlier it worked only in the app's own local
  workspace, when the installer asked for it. See
  [the desktop's own runner](install.md#the-desktops-own-runner).
- **Runner install tabs.** When a runner token is shown, its setup steps
  are on tabs: **Linux** (the prebuilt binary archive), **Docker run**
  and **Docker Compose** (the prebuilt runner image, with a private env
  file and a named workspace volume). **Cloud** is shown as coming soon
  and can't be chosen. No step contains the token. See
  [A standalone runner](install.md#a-standalone-runner-operators).
- **Readable dropdowns.** Dropdown and menu text keeps its contrast in
  both the dark and the light theme.
- **More context for classification.** When you've turned on
  classification in a Space, System One also sees the Space, room and
  member display names, and up to eight earlier messages in that room or
  thread (400 characters each, 2,400 in total, with who wrote them and
  whom they addressed). In a reply thread, the message it started from
  comes first. *No agent* is now only for a message that talks to another
  person, is only conversation about someone, or says no agent should
  answer; a request to the room about someone still gets an agent. See
  [Classification in 0.6.2](system-one.md#classification-in-062).
- **Write while an agent replies.** The Chat composer stays usable while
  Metro replies: **Queue** (the default) sends the message after the
  current reply, **Steer** adds it to the running reply at its next step.
  In Space rooms and threads, your next message to an agent that is
  already working on your message there steers that run instead of
  starting a second one. See [While an agent replies](messages.md#while-an-agent-replies).
- **Chat threads.** Your personal Chat can have threads: start one with
  **New thread**, or choose **Reply** on a Chat message. Each thread is
  its own conversation with Metro, with its own history, summaries,
  **Stop** and Queue/Steer line, so a turn in one never waits for another.
  Rename or delete a thread from its menu; deleting is permanent and
  removes everything in it. See [Chat threads](messages.md#chat-threads).

For the API, see [Chat threads](messages.md#chat-threads-1) and
[Queue and steer](messages.md#queue-and-steer) under
[Messaging API](messages.md#messaging-api).

Later changes are [in the source, not released](next-source.md).
