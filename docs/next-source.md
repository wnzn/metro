# In the source, not released

The Pages changes below are in source after `6a038f1`; they are **not** in
the published [Metro 0.5.0](https://github.com/wnzn/metro/releases/tag/v0.5.0).
For published behavior, see [Pages and Daily briefs](pages.md) and
[New in Metro 0.5.0](released-0.5.0.md).

## Pages 0.5.1 work

- Resume private drafts from Pages while you still have access to the
  destination. Draft recovery and retries preserve the intended edit
  without applying it to the live Page.
- When a Space template needs a destination, pick and review its room
  and agent. The server checks the reviewed room and agent rather than
  silently substituting a different destination.
- Page editor and action dialogs retain more context when dismissed
  or retried. Daily briefs can follow an in-progress run without sending
  it again.
- The desktop changes also improve Pages spacing and the accessibility
  of tables, links, diffs and headings.
