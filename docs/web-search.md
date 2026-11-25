# Web search and privacy

> **In the source, not released.** This describes [the source preview](next-source.md),
> not the Metro 0.4.0 app or server.

**Settings → Web search** shows whether your computer is ready, lets you
turn web search off, and lets you enable, reorder, test, or add keys to
providers. The switch and provider choices are local, not shared from the
server. The switch is on by default with a notice; **Got it** acknowledges
that notice, not a requirement to authorize each search.

- **In a Space:** The Space owner enables `web_search` and `web_fetch`
  separately for each agent in its tool allowlist. Both are off by default
  there. The tools use the computer and providers of the person who
  started the run, not the Space owner or the owner of a shared model
  connection.
- **In Personal Chat:** **Allow web search in personal Chat** is a
  separate account preference on that server, **off by default**. It
  permits both search and fetch. Turning it on doesn't change the switch
  on this computer or any Space permissions; the computer still needs to
  be ready. Clients can read or set it at `/v1/web-search/prefs` with
  `GET` or `PUT` (`{"personal_chat":true}` or `{"personal_chat":false}`).
- **When the computer is unavailable:** Web work fails visibly; Metro
  won't use another person's computer, the server, or a runner in its
  place. A scheduled prompt can still answer without fresh web data if
  its model connection is usable. Activity says why web access failed.

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
**Test provider** sends the fixed query “Metro web search test” to that
provider only, in the current mode; it can consume credits.

## What leaves your computer

Search queries and page addresses go to the provider that serves them.
Providers see the originating connection's IP address and, with a saved
key, can associate requests with your account or charge it. Exact queries
and fetch URLs from Space runs are visible in activity to current members
who can read the conversation. Activity records a result summary and
provider outcomes, not the fetched page text. The model can receive
bounded results to help answer the request.

Fetching is limited to public URLs a person wrote in the conversation or
a search earlier in the same run returned. Internal destinations are
refused. The device sends requests to the supported provider endpoints;
it isn't a general-purpose proxy that the Metro server can use to reach
other destinations.

**About & updates → Privacy** also explains update checks, model
connections and server storage. The server operator has access to the
stored data; a shared model connection's owner can see requests that go
through it and may pay for them. Metro doesn't send analytics or crash
reports; desktop monitoring isn't included in update checks.
