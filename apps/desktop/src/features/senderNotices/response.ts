// Sender-only POST metadata. These types deliberately don't belong to RoomEvent.
const decisionReasons = ["unreachable", "timeout", "invalid_answer", "no_classifier", "not_allowed"] as const;
const modelReasons = ["not_allowed", "connection_changed", "busy", "offline", "misconfigured", "internal"] as const;
const modelSources = ["override", "case", "routing_default", "default", "environment", "inherited", "agent_default", "space_default"] as const;
type DecisionReason = typeof decisionReasons[number];

export type AddressingNote =
  | { kind: "classification_failed"; reason: DecisionReason }
  | { kind: "generic_agent_unavailable"; reason: DecisionReason; message: string };
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
/** Exact text is held only while waiting for the poll to corroborate a POST. */
export type PostedSenderNotes = SenderNoticeDetails & { text: string };

type SendIntent =
  | { kind: "chat"; text: string }
  | { kind: "space"; text: string; invoke_agent?: boolean; agent_id?: string;
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

// Plain text only; do not interpret links, markup, or control/directional characters.
function readMessage(value: unknown) {
  // Includes room for a use-case label and model id in the longest server refusal.
  if (typeof value !== "string" || value.length > 1_200) return null;
  return value.replace(/[\u0000-\u001f\u007f-\u009f\u061c\u200e\u200f\u2028-\u202e\u2066-\u2069]/g, " ").trim() || null;
}

function addressingNote(value: unknown): AddressingNote | null {
  if (!isRecord(value) || !isOneOf(decisionReasons, value.reason)) return null;
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
  if (note.kind === "generic_agent_unavailable") return note.message;
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
    typeof actor !== "string" || typeof actor_id !== "string" || !actor_id.trim() || !createdAt ||
    typeof text !== "string" || text.length > 16_000 || text !== canonicalSentText(sent.text)) return null;
  const addressing = sent.kind === "space" ? addressingNote(data.addressing_note) : null;
  const routing = routingNote(data.routing_note);
  if (data.routing_note != null && !routing) return null;
  if (sent.kind === "space") {
    if (data.addressing_note != null && !addressing) return null;
    const recipients = sent.mentions ?? [];
    const unaddressed = !sent.invoke_agent && !recipients.length;
    const generic = (sent.invoke_agent && !sent.agent_id) || recipients.some(m => m.kind === "agent" && !m.agent_id);
    const named = (sent.invoke_agent && !!sent.agent_id) || recipients.some(m => m.kind === "agent" && !!m.agent_id);
    if (addressing?.kind === "classification_failed" &&
      (!unaddressed || !text || routing || (Array.isArray(data.mentions) && data.mentions.length > 0))) return null;
    if (addressing?.kind === "generic_agent_unavailable" && (!generic || (routing && !named))) return null;
    if (routing && !generic && !named) {
      // An unaddressed message can reach routing only if classification
      // succeeded, in which case the stored event names the classifier's pick.
      if (!unaddressed || !text || !Array.isArray(data.mentions) || data.mentions.length !== 1) return null;
      const [pick] = data.mentions;
      if (!isRecord(pick) || pick.kind !== "agent" || pick.addressed_by !== "classifier" ||
        typeof pick.agent_id !== "string" || !pick.agent_id.trim()) return null;
    }
  }
  return addressing || routing ? { eventId: id, senderId: actor_id, createdAt, text, addressing, routing } : null;
}
