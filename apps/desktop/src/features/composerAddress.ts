// Mirror the server's text boundaries in crates/server/src/mentions.rs.
// Personal Chat has no member list: a handle that looks like a person's is
// enough to keep Return from explicitly asking on their behalf.
export function composerAddress(text: string, memberIds: readonly string[] = []) {
  if (!text.includes("@")) return { agent: false, people: false };
  const fenced: [number, number][] = [];
  let fence: { run: string; start: number } | undefined;
  for (const m of text.matchAll(/^ {0,3}(`{3,}|~{3,})([^\n]*)(?:\n|$)/gm)) {
    const run = m[1]!;
    if (!fence) fence = { run, start: m.index! };
    else if (run.startsWith(fence.run) && !m[2]!.trim()) {
      fenced.push([fence.start, m.index! + m[0].length]);
      fence = undefined;
    }
  }
  if (fence) fenced.push([fence.start, text.length]);
  const ticks = [...text.matchAll(/`+/g)];
  const next = new Map<number, number>();
  const endAt = ticks.map(() => -1);
  for (let i = ticks.length - 1; i >= 0; i--) {
    const n = ticks[i]![0].length;
    endAt[i] = next.get(n) ?? -1;
    next.set(n, i);
  }
  const spans: [number, number][] = [];
  let f = 0;
  for (let i = 0; i < ticks.length; i++) {
    const start = ticks[i]!.index!;
    while (f < fenced.length && fenced[f]![1] <= start) f++;
    if (fenced[f] && fenced[f]![0] <= start) continue;
    if (endAt[i]! < 0) continue;
    i = endAt[i]!;
    spans.push([start, ticks[i]!.index! + ticks[i]![0].length]);
  }
  let agent = false, people = false, s = 0;
  f = 0;
  // Periods can belong to an id or be trailing punctuation. Consume them
  // with the id first; don't backtrack through a long run of periods.
  for (const m of text.matchAll(/(?:^|[\s([{<"',;:!?*])@([a-z0-9][a-z0-9._-]*)(?:[,;:!?)\]}>"'*][.,;:!?)\]}>"'*]*)?(?=$|\s)/gi)) {
    const at = m.index! + m[0].indexOf("@");
    while (f < fenced.length && fenced[f]![1] <= at) f++;
    while (s < spans.length && spans[s]![1] <= at) s++;
    if ((fenced[f] && fenced[f]![0] <= at) || (spans[s] && spans[s]![0] <= at)) continue;
    const raw = m[1]!;
    const handle = raw.replace(/\.+$/, "");
    if (handle.toLowerCase() === "agent" && !memberIds.some(id => id.length > 5 && id.length <= 64 && raw.startsWith(id) && /^\.*$/.test(raw.slice(id.length)))) agent = true;
    else people = true;
  }
  return { agent, people };
}
