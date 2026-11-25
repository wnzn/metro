# Web search and privacy

> **Released in Metro 0.5.0.** Use matching desktop and server releases.

**Settings → Web search** shows whether your computer is ready, lets you
turn web search off, and lets you enable, reorder, test, or add keys to
providers. The switch and provider choices are local, not shared from the
server. The switch is on by default with a notice; **Got it** acknowledges
that notice, not a requirement to authorize each search.

For each permitted search or fetch, the Metro server picks the run
starter's most recently seen online web device on that server. That's
not necessarily the computer where you clicked Send or opened Settings.
The selected device uses its local provider order, switches, and keys
for your account. The model connection doesn't determine which web
device is chosen.

- **In a Space:** The Space owner enables `web_search` and `web_fetch`
  separately for each agent in its tool allowlist. Both are off by default
  there.
- **In Personal Chat:** **Allow web search in personal Chat** is a
  separate account preference on that server, **off by default**. It
  permits both search and fetch. Turning it on doesn't change the switch
  on this computer or any Space permissions. Clients can read or set it
  at `/v1/web-search/prefs` with `GET` or `PUT` (`{"personal_chat":true}`
  or `{"personal_chat":false}`).
- **Scheduled prompts and Pages:** A scheduled prompt uses its creator's
  web devices and the permissions of its destination: Personal Chat's
  opt-in or the Space's current default agent's allowlist. A Space Page
  button that asks a named agent starts a room run as the clicker, with
  that agent's permissions and the clicker's web devices. A button that
  runs a saved schedule follows the schedule's rules. A personal Page's
  agent run has no web tools, even if the owner opted in for Personal
  Chat. See [Daily briefs](pages.md#daily-briefs) for which replies a page
  can display.

If no device is paired, the agent is told web tools aren't available.
If there is a paired device but none is online when a tool is called,
the tool fails visibly in activity. Metro never sends the job to a
different account, a runner, or a server-side web provider. A run,
including a scheduled prompt, can still answer without fresh web data
if its model connection is usable. This does not make an offline model
connection usable.

## Providers and keys

This is the default try order. The modes below describe what the adapters
send; they are not a guarantee of live vendor access, free quotas, or plan
limits.

| Provider | Without a key | With a saved key |
| --- | --- | --- |
| Tavily | REST, `X-Tavily-Access-Mode: keyless` | `Authorization: Bearer`, no keyless header |
| Exa | Hosted MCP, direct IPv4 without a proxy | `X-API-Key` |
| Parallel | Hosted MCP, no key | `Authorization: Bearer` |
| Firecrawl | REST, no `Authorization` header | `Authorization: Bearer` |
| TinyFish | Skipped; an account key is required for search and fetch | `X-API-Key` |

Keys belong to the signed-in account on this computer. They're stored
in a permission-restricted file on Unix and sealed for the current OS
user on Windows. They are never shown again or sent to the Metro server.
A key goes only to its own provider. **Remove key** returns a provider
that supports keyless use to that mode. An empty key field doesn't save
a change. If a provider rejects a saved key, Metro doesn't retry it
without the key. Replace or remove the key, or use **Try again now** to
clear the recorded pause or rejection.

Metro tries the next eligible provider on a limit, exhausted credits,
rejected key, or outage, within the request's time limit. It records
rate-limit and outage pauses per provider and mode rather than guessing
quotas. Run activity shows the attempts and which provider answered.
**Test provider** contacts only the selected provider in its current
mode. If it reaches the search step, it sends the fixed query
“Metro web search test” and can consume credits.

## Who receives queries and results

- **The Metro server** relays the tool request to the selected web device
  and receives a bounded result and provider outcomes back. Search
  arguments can include the query, keywords, and a result limit; fetch
  arguments include page URLs and may include a focus. Web provider keys
  are never uploaded to Metro.
- **Each contacted provider** sees the applicable network and
  authentication information for requests it receives, including the
  connecting IP address and its own saved key if used. It may also
  receive search terms or fetch URLs and any extra fields its adapter
  sends. This applies to providers tried before the one that answers,
  even if they don't return usable results. A provider may reject
  session setup before Metro sends the query or URLs. Skipped providers
  aren't contacted. Using a key can identify or charge your account.
- **The answering model** can receive bounded snippets, fetched page
  text, or text-only results relayed as tool output for the current
  run only. Metro doesn't store that content as tool history or
  replay it in later runs. The model's saved reply can still quote or
  summarize it. Metro doesn't control what web or model providers log
  or retain.

## What Metro keeps in conversation activity

Metro records accepted arguments before dispatch, so a tool call in
activity isn't proof that a provider got them:

- **Accepted searches:** the validated query, any keywords and a
  non-default result limit (the default is five).
- **Accepted fetches:** normalized, deduplicated URLs with fragments
  removed, and any nonempty focus.
- **Results:** a summary, not the snippets or page text.
  It can include the answering provider and mode (keyless or keyed),
  search result counts, titles and URLs, or fetch counts, URLs,
  truncation and per-URL failure reasons.
- **Outcomes:** where available, attempt metadata records provider,
  mode, outcome and any cooldown expiry. A rejected-key attempt may
  say whether the provider was contacted. Failed calls record a
  Metro reason and failure code, not a raw vendor error. Refused
  calls don't copy rejected argument values into activity.

The activity text is subject to the server's event length limit.
Space members who can read the room can see the activity; Personal
Chat activity is private to that account in the app.

The desktop uses [neutral Web Activity
labels](released-0.5.0.md#page-consent-and-web-activity). **Saved key** and
**no key** describe how an attempt was made, not what the provider charges
or which limits apply.

## Which URLs Metro can fetch

Metro checks URL provenance: the normalized HTTP or HTTPS URL must
come from a person's message available to the run or from a structured
search result earlier in that same run. Links in fetched page text don't
qualify on their own. The server and device reject embedded usernames
or passwords, IP literals in ranges Metro blocks (such as private or
loopback addresses), and recognized local hostnames such as `localhost`,
single-label names and names ending in `.local` or `.internal`.

These are checks on the supplied URL, not proof the destination is
public. Metro doesn't resolve the target hostname to check where it
points, and doesn't control how the web provider resolves it or which
redirects the provider follows while fetching it. The device only
contacts supported vendor endpoints and doesn't follow redirects from
those APIs. Don't give the agent private links or URLs containing
credentials, such as signed links or tokens in paths or query strings:
an accepted URL can be stored in activity and sent to a provider.

**About & updates → Privacy** also explains update checks, model
connections and server storage. The server operator has access to the
stored data; a shared model connection's owner can see requests that go
through it and may pay for them. Metro doesn't send analytics or crash
reports; desktop monitoring isn't included in update checks.
