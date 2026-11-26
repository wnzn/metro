// Sender-only POST metadata. These types deliberately don't belong to RoomEvent.
const decisionReasons = ["unreachable", "timeout", "invalid_answer", "no_classifier", "not_allowed"] as const;
const modelReasons = ["not_allowed", "connection_changed", "busy", "offline", "misconfigured", "internal"] as const;
const modelSources = ["override", "case", "routing_default", "default", "environment", "inherited", "agent_default", "space_default"] as const;
type DecisionReason = typeof decisionReasons[number];

export type AddressingNote =
  | { kind: "classification_failed"; reason: DecisionReason }
  | { kind: "generic_agent_unavailable"; reason: DecisionReason; message: string }
  | { kind: "reply_agent_unavailable"; message: string };
export type RoutingNote =
  | { kind: "routing_failed"; reason: DecisionReason; message: string }
  | { kind: "model_unavailable"; reason: typeof modelReasons[number]; source: typeof modelSources[number]; message: string };

export type SenderNoticeDetails = {
  eventId: number;
  senderId: string;
  createdAt: string;
  addressing: AddressingNote | null;
  routing: RoutingNote | null;
};
/** Echo evidence is held only while waiting for the poll to corroborate a POST. */
export type PostedSenderNotes = SenderNoticeDetails & { text: string; recipientsKey: string; replyTo?: number };

type SendIntent =
  | { kind: "chat"; text: string }
  | { kind: "space"; text: string; invoke_agent?: boolean; agent_id?: string;
      reply?: { eventId: number; askAgent: boolean; agentId?: string };
      mentions?: readonly ({ kind: "agent"; agent_id?: string } | { kind: "user"; id: string })[] };

export function canonicalEventTime(value: unknown): string | undefined {
  if (typeof value !== "string" || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/.test(value)) return;
  const time = Date.parse(value);
  // Metro's event serializer uses UTC milliseconds and reports pre-2000
  // legacy timestamps as unknown. Round-trip to reject normalized bad dates.
  return time >= 946_684_800_000 && new Date(time).toISOString() === value ? value : undefined;
}

export const canonicalSentText = (text: string) => text.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, "");

const isRecord = (v: unknown): v is Record<string, unknown> =>
  !!v && typeof v === "object" && !Array.isArray(v);
const isOneOf = <T extends string>(values: readonly T[], value: unknown): value is T =>
  typeof value === "string" && (values as readonly string[]).includes(value);

/** Never derive private identity by coercing the legacy display value. */
export function canonicalStableId(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= 256 && value === value.trim() &&
    !/[\u0000-\u001f\u007f-\u009f\u061c\u200e\u200f\u2028-\u202e\u2066-\u2069]/.test(value);
}

type AgentAddress = "classifier" | "schedule" | "reply" | "metro";
type Recipient = readonly ["user", string] | readonly ["agent", string | null, AgentAddress | null];
const identityOf = (r: Recipient) => JSON.stringify([r[0], r[1]]);
const recipientsKey = (recipients: readonly Recipient[]) => JSON.stringify(recipients);

/** Validate the complete modern wire address, without using legacy renderer normalization. */
export function canonicalAddressing(audience: unknown, raw: unknown): Recipient[] | null {
  if (!Array.isArray(raw) || raw.length > 32) return null;
  const recipients: Recipient[] = [];
  const seen = new Set<string>();
  for (const m of raw) {
    if (!isRecord(m)) return null;
    let r: Recipient;
    if (m.kind === "user" && canonicalStableId(m.id) && !("agent_id" in m) && !("addressed_by" in m)) {
      r = ["user", m.id];
    } else if (m.kind === "agent" && !("id" in m)) {
      const id = "agent_id" in m ? m.agent_id : null;
      const by = "addressed_by" in m ? m.addressed_by : null;
      if (("agent_id" in m && !canonicalStableId(id)) ||
        ("addressed_by" in m && (!(by === "classifier" || by === "schedule" || by === "reply" || by === "metro") || !canonicalStableId(id)))) return null;
      r = ["agent", id as string | null, by as AgentAddress | null];
    } else return null;
    const identity = identityOf(r);
    if (seen.has(identity)) return null;
    seen.add(identity);
    recipients.push(r);
  }
  const people = recipients.some(r => r[0] === "user");
  const agents = recipients.some(r => r[0] === "agent");
  return audience === (agents ? (people ? "mixed" : "agent") : (people ? "users" : "room")) ? recipients : null;
}

type RenderedRecipient = { kind: "user"; id: string } |
  { kind: "agent"; agentId?: string; addressedBy?: AgentAddress };

/** Only called for a row whose original wire fields passed canonicalAddressing. */
export function renderedRecipientsKey(mentions: readonly RenderedRecipient[] = []): string {
  return recipientsKey(mentions.map(m => m.kind === "user" ? ["user", m.id] :
    ["agent", m.agentId ?? null, m.addressedBy ?? null]));
}

function submittedRecipients(sent: SendIntent): Recipient[] {
  if (sent.kind === "chat") return [["agent", null, null]];
  const unique = new Map<string, Recipient>();
  const add = (r: Recipient) => unique.set(identityOf(r), r);
  for (const r of sent.mentions ?? []) add(r.kind === "user" ? ["user", r.id] : ["agent", r.agent_id ?? null, null]);
  if (sent.invoke_agent) add(["agent", sent.agent_id ?? null, null]);
  return [...unique.values()];
}

// Plain text only; do not interpret links, markup, or control/directional characters.
function readMessage(value: unknown) {
  // Includes room for a use-case label and model id in the longest server refusal.
  if (typeof value !== "string" || value.length > 1_200) return null;
  return value.replace(/[\u0000-\u001f\u007f-\u009f\u061c\u200e\u200f\u2028-\u202e\u2066-\u2069]/g, " ").trim() || null;
}

function addressingNote(value: unknown): AddressingNote | null {
  if (!isRecord(value)) return null;
  if (value.kind === "reply_agent_unavailable") {
    const message = readMessage(value.message);
    return message ? { kind: value.kind, message } : null;
  }
  if (!isOneOf(decisionReasons, value.reason)) return null;
  const { kind, reason } = value;
  if (kind === "classification_failed") return { kind, reason };
  if (kind !== "generic_agent_unavailable") return null;
  const message = readMessage(value.message);
  return message ? { kind, reason, message } : null;
}

function routingNote(value: unknown): RoutingNote | null {
  if (!isRecord(value)) return null;
  const { kind, reason, source } = value;
  const message = readMessage(value.message);
  if (!message) return null;
  if (kind === "routing_failed" && isOneOf(decisionReasons, reason)) return { kind, reason, message };
  if (kind === "model_unavailable" && isOneOf(modelReasons, reason) && isOneOf(modelSources, source)) {
    return { kind, reason, source, message };
  }
  return null;
}

export function addressingText(note: AddressingNote) {
  if (note.kind !== "classification_failed") return note.message;
  const reason: Record<DecisionReason, string> = {
    unreachable: "System One didn't answer",
    timeout: "it took too long",
    invalid_answer: "its answer wasn't valid",
    no_classifier: "your System One connection isn't usable here",
    not_allowed: "your System One connection isn't usable here",
  };
  return `Your classifier couldn't decide whether this was for an agent (${reason[note.reason]}). Nothing was started.`;
}

export function routingText(note: RoutingNote, inSpace: boolean) {
  if (!inSpace) return note.message;
  // These are phrases from the server's refusal templates. A message can
  // start other turns; this note describes only the first refusal.
  return note.message
    .replace(/^Your model routing could not choose a model for this message:/, "Your model routing could not choose a model for this turn:")
    .replace(/No model was used; check your System One connection in Settings\.$/, "No model was used for this turn; check your System One connection in Settings.")
    .replace(/No other model was used\.$/, "No other model was used for this turn.");
}

/** Called only by the successful message POSTs. The envelope has no place field:
 * the caller binds it to the authenticated endpoint's original destination. */
export function readPostedNotes(data: unknown, sent: SendIntent): PostedSenderNotes | null {
  if (!isRecord(data)) return null;
  const { id, kind, actor, actor_id, text } = data;
  const createdAt = canonicalEventTime(data.created_at);
  // Mirror Rust str::trim (Unicode White_Space) for correlation only; never
  // change the POST body, whose mention spans index the original UTF-8 text.
  if (kind !== "message" || typeof id !== "number" || !Number.isSafeInteger(id) || id <= 0 ||
    typeof actor !== "string" || !canonicalStableId(actor_id) || !createdAt ||
    typeof text !== "string" || text.length > 16_000 || text !== canonicalSentText(sent.text)) return null;
  const addressing = sent.kind === "space" ? addressingNote(data.addressing_note) : null;
  const routing = routingNote(data.routing_note);
  if ((!addressing && !routing) || (data.routing_note != null && !routing)) return null;
  const echoed = canonicalAddressing(data.audience, data.mentions);
  if (!echoed) return null;
  const reply = sent.kind === "space" ? sent.reply : undefined;
  if ((data.reply_to ?? undefined) !== reply?.eventId) return null;
  const expected = submittedRecipients(sent);
  const key = recipientsKey(echoed);
  const explicitMatch = recipientsKey(expected) === key;
  if (sent.kind === "space") {
    if (data.addressing_note != null && !addressing) return null;
    const unaddressed = expected.length === 0;
    const generic = expected.some(m => m[0] === "agent" && m[1] === null);
    const replyRecipient = echoed.filter(m => m[0] === "agent" && m[2] === "reply");
    const replied = reply?.askAgent && replyRecipient.length === 1 &&
      (!reply.agentId || replyRecipient[0]![1] === reply.agentId) &&
      recipientsKey(echoed.filter(m => m[0] !== "agent" || m[2] !== "reply")) === recipientsKey(expected);
    const named = expected.some(m => m[0] === "agent" && m[1] !== null) || replied;
    if (addressing?.kind === "reply_agent_unavailable" && (!reply?.askAgent || !explicitMatch)) return null;
    if (addressing?.kind === "classification_failed" &&
      (!unaddressed || !text || routing || !explicitMatch)) return null;
    if (addressing?.kind === "generic_agent_unavailable" && (!generic || (routing && !named))) return null;
    // A decision made after the message was stored never edits its
    // recipients; its POST says whether the classifier picked an agent.
    const classified = unaddressed && !!text && (
      (echoed.length === 1 && echoed[0]?.[0] === "agent" && echoed[0][2] === "classifier") ||
      (echoed.length === 0 && data.classified === true));
    if (!explicitMatch && !replied && !(routing && !addressing && classified)) return null;
    if (routing && !generic && !named && !classified) return null;
  } else if (!explicitMatch) return null;
  return { eventId: id, senderId: actor_id, createdAt, text, recipientsKey: key, addressing, routing, ...(reply ? { replyTo: reply.eventId } : {}) };
}
