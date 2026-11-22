#!/usr/bin/env node
// A throwaway in-memory server for UI work without the Rust server. It serves
// the same test double the headless tests use (test/fakeServer.ts), so it is
// NOT the real server: no persistence, no real agent or runner; "Ask agent"
// and "Run demo" answer with canned events. Never use it for acceptance
// evidence or screenshots.
//
//   node dev/mock-server.mjs            # Node >= 22.18 (strips TypeScript types)
//   PORT=8787 HOST=127.0.0.1 (defaults)
//
// Users: alice (owns "Acme", runner paired; bob is a member), bob (owns "Side
// project"; alice is a member), carol (no Spaces). Their fixed fake tokens are
// in test/fakeServer.ts (TOKENS); they only work against this mock.

import { createServer } from "node:http";
import { world } from "../test/fakeServer.ts";

const port = Number(process.env.PORT ?? 8787);
const host = process.env.HOST ?? "127.0.0.1";
const fake = world();
const route = fake.network[`GET http://127.0.0.1:8787/v1/*`];

createServer(async (req, res) => {
  let body = "";
  for await (const chunk of req) body += chunk;
  // The double was written for BASE http://127.0.0.1:8787; only the path matters.
  const answer = await route({ method: req.method, url: `http://127.0.0.1:8787${req.url}`, headers: req.headers, body: body || null });
  if (answer.error) {
    res.destroy();
    return;
  }
  const data = answer.body === undefined ? "" : JSON.stringify(answer.body);
  res.writeHead(answer.status ?? 200, { "content-type": "application/json" });
  res.end(data);
}).listen(port, host, () => console.log(`mock Metro (test double) on http://${host}:${port} — users alice, bob, carol`));
