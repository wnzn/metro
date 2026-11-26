# New in Metro 0.5.1

These [Pages](pages.md) changes are included in the published
[Metro 0.5.1 release](https://github.com/wnzn/metro/releases/tag/v0.5.1).
See [Acceptance](acceptance.md#published-metro-051-2026-11-26-github-release-v051)
for the release files and image digests, and [New in Metro 0.5.0](released-0.5.0.md)
for the features this release builds on.

- Resume private drafts from Pages while you still have access to the
  destination. Recovery and retries don't apply the draft.
- For the Space templates Daily brief, Release watch and Space overview,
  pick a top-level room. The template uses the Space's default agent.
  If that room or agent changes before compilation, the server refuses
  the draft instead of substituting another destination. Runs and Files
  don't ask for a room.
- The Page editor and action dialogs retain more context when dismissed
  or retried. Daily briefs can follow an in-progress run without sending
  it again.
- Draft review uses **Publish page** or **Apply changes**. **Refresh** is
  now **Refresh data**; a Space brief's **Keep this answer fresh…** is
  now **Schedule brief…**.
- Page layout and the accessibility of tables, links, diffs and headings
  have improved.

The [0.6.0 changes](next-source.md) are in the source, not released.
