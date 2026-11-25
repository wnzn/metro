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

export type PostedSenderNotes = {
  eventId: number;
  senderId: string;
  createdAt?: string;
  addressing: AddressingNote | null;
  routing: RoutingNote | null;
};

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

/** Called only by the successful message POSTs. The envelope has no place field:
 * the caller binds it to the authenticated endpoint's original destination. */
export function readPostedNotes(
  data: unknown, submittedText: string, inSpace: boolean, readTime: (v: unknown) => string | undefined,
): PostedSenderNotes | null {
  if (!isRecord(data)) return null;
  const { id, kind, actor, actor_id, text } = data;
  // Mirror Rust str::trim (Unicode White_Space) for correlation only; never
  // change the POST body, whose mention spans index the original UTF-8 text.
  if (kind !== "message" || typeof id !== "number" || !Number.isSafeInteger(id) || id <= 0 ||
    typeof actor !== "string" || typeof actor_id !== "string" || !actor_id.trim() ||
    text !== submittedText.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, "")) return null;
  const addressing = inSpace ? addressingNote(data.addressing_note) : null;
  const routing = routingNote(data.routing_note);
  return addressing || routing
    ? { eventId: id, senderId: actor_id, createdAt: readTime(data.created_at), addressing, routing }
    : null;
}
