# In the source, not released

The Pages changes below are in source after `6a038f1`; they are **not** in
the published [Metro 0.5.0](https://github.com/wnzn/metro/releases/tag/v0.5.0).
For published behavior, see [Pages and Daily briefs](pages.md) and
[New in Metro 0.5.0](released-0.5.0.md).

## Pages 0.5.1 work

- Resume private drafts from Pages while you still have access to the
  destination. Draft recovery and retries preserve the intended edit
  without applying it to the live Page.
- When a Space template needs a destination (Daily brief, Release
  watch, Space overview), pick its top-level room; it uses the Space's
  default agent. The server refuses the page if that room or agent
  changed before it was compiled, rather than silently substituting a
  different destination. Runs and Files don't ask for a room.
- Page editor and action dialogs retain more context when dismissed
  or retried. Daily briefs can follow an in-progress run without sending
  it again.
- Some controls are renamed: draft review's **Apply** is **Publish page**
  for a new page and **Apply changes** for an edit; **Refresh** is
  **Refresh data**; a Space brief's **Keep this answer fresh…** is
  **Schedule brief…**.
- The desktop changes also improve Pages spacing and layout and the
  accessibility of tables, links, diffs and headings.
