# Pages and Daily briefs

> Use matching desktop and server releases. See
> [what changed in Metro 0.5.1](released-0.5.1.md).

**Pages** in the sidebar holds personal dashboards and dashboards for
Spaces you belong to. **New page** can start from a template or a prompt.
For an existing page, use **Edit with prompt** for the whole page or
**Select a block** to focus the change. A generated draft can be previewed
and compared with its base. It changes nothing until you select
**Publish page** or **Apply changes**. Changes outside a selected block
require additional confirmation, and a stale draft cannot overwrite a
newer version.

Pages use **Page TSX**: the server validates a restricted set of
components, data sources and actions, and Metro renders the result with
native components. It doesn't execute JavaScript from a page or give
the page arbitrary network, filesystem or token access. Editors can
edit the source and check it before saving. **Page history** lets you
review retained versions; restoring one creates a new version.

## Deleting Pages and Spaces

People who can manage an individual Page can restore it from
**Recently deleted** within 30 days, subject to the live Page limit.
Deleting the entire Space is different: it permanently removes the
Space's published Pages, their version history, any Pages already in
**Recently deleted**, and Space Page drafts. There is no Trash or
restore path after [deleting the Space](spaces.md#spaces-and-members).
Personal Pages aren't deleted with the Space.

## Who can see, edit, and run things

- A **personal page** is visible only to its owner, with no org-admin
  bypass. Its agent button runs as the owner and posts the result only
  to the page, never into Personal Chat or a Space room. It has no web
  tools, even if Personal Chat web search is on. Personal pages cannot
  run schedules from buttons.
- A **Space page** is visible only to current members of that Space. The
  creator (while a member) and the Space owner can manage and edit it.
  **Members can edit** grants members editing rights, not the right to
  delete the page or change that switch.
- Data is resolved as the viewer with current access, not as the page's
  author. Space pages cannot read other Spaces or personal Chat; personal
  pages can use the owner's own data and data from Spaces they can
  already read. Draft previews never execute actions.
- Actions run as the person clicking. A Space page can ask a named
  agent in one of its own top-level rooms or run one of the clicker's
  own scheduled tasks for such a room.
- If you didn't author the action and haven't confirmed that page
  version, Metro asks for confirmation. **Approve version and run**
  grants consent for the whole page version but runs only the clicked
  action. The confirmation shows that action's prompt, destination and
  model scope, plus up to four other available agent or schedule button
  labels and a count of any more. Review the page too: approval covers
  its agent and schedule buttons, not just the ones listed. Other buttons
  still need a click, but won't ask for approval again on that version.
  Approval doesn't carry over to a new version.
- Each click still checks the current page version, the clicker's
  access, and the action's destination. Running a schedule still
  requires the clicker to own it and pass the task's checks. The desktop
  submits the displayed task revision on schedule clicks; Metro
  requires the reviewed revision when confirming a schedule and checks
  any supplied revision even if the page version was already approved.

## Daily briefs

The **Daily brief** template collects an agent answer in a page block.
**Write today's brief** requests one now. The personal template shows
only the answer from its own page button. A Space brief follows the latest
answer from its selected room and agent, including replies from scheduled
prompts there. It is not limited to outputs of one particular task.
An answer block explicitly linked to a Page button is different: it
shows runs started from that button, not later scheduled room replies.

For a matching Space brief, choose **Schedule brief…**, then
**Open task form** to open a prefilled scheduled prompt. This works only
with a top-level room and the Space's default agent. Choose the
schedule and time zone and save the task yourself;
opening the form schedules nothing. The task runs as its creator.
The schedule asks the Space's default agent at run time; a **Run agent**
button asks the agent named in that button as the clicker. See
[web-search permissions](web-search.md) for what each run can use.
The page's regular data refresh reads the latest data while the page is
visible; it doesn't start agent runs.

## Drafts from a Space agent

A Space owner can enable `pages_list`, `page_read` and `page_propose` on
an individual agent; they're off by default. These tools use the
initiator's current permissions, access only that Space, and return
only Page data safe to use in its room. A proposal to create or edit
a page produces a **Page draft** card in the conversation. Other members
can see the card, not the private draft. Only the initiator can review the
preview and diff or apply it, and they must still have the necessary
rights when they apply. Drafts don't change the live page on their own.

## Live limits in the organization admin panel

An org admin can change **Page limits** on the server at `/admin`. The
panel uses the same token-authenticated `GET /v1/admin/settings` and
`PATCH /v1/admin/settings` API available to other operator clients. Use a
human bearer token whose stable ID is in `METRO_ORG_ADMINS`. Missing or
invalid authentication gets `401`; an authenticated non-admin gets `403`.

| Field | Default and range | Effect |
| --- | --- | --- |
| `pages_per_space` | 50 (1–500) | Live pages per Space, and separately per personal owner. Checked when creating or undeleting a page, including applying a new-page draft. |
| `versions_per_page` | 200 (2–1,000) | Keep version 1 plus the newest N versions; applied when a new version is written. |

GET and successful PATCH return both effective values. For example, a
PATCH body `{"versions_per_page":100}` changes just history retention.
Omit fields to leave them unchanged; nulls, unknown fields and values
outside the integer ranges are rejected. Saving takes effect without a
restart, but doesn't itself delete pages or prune history. If a new page
cap is below the current count, existing pages remain; creation and
undelete are blocked until there is room.

The limits API doesn't give admins access to users' pages. It is separate
from the admin panel's [local-account API](deploy.md#4-create-peoples-accounts).
