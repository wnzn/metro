// Runner shell policy and shell approvals (Metro 0.6.3).
//
// - A registered runner has one default policy plus optional per-Space
//   overrides (null = inherit), read and changed only by whoever manages it.
// - A dedicated runner has one policy per Space, changed by the Space owner.
// - Every member reads the effective policy through GET …/runner (`shell`).
// - A `shell_approval` room event asks for a decision; `shell_approval_resolved`
//   says how it ended. Who may decide comes only from the server's pending
//   list (`can_decide`), never from the app's own guess.

import { ApiError } from "./client";
import type { RequestFn, RunnerVisibility } from "./client";

export type ShellMode = "off" | "approve" | "classify" | "allow";
export type ShellApprover = "owner" | "members";
export type ShellDowngrade = "built_in_multiple_spaces" | "classify_unavailable";
export type ShellPolicy = { mode: ShellMode; approver: ShellApprover };

export const SHELL_MODES: readonly ShellMode[] = ["off", "approve", "classify", "allow"];
export const SHELL_APPROVERS: readonly ShellApprover[] = ["owner", "members"];

export type RunnerSpaceShell = {
  spaceId: string;
  /** Null when the caller isn't a member of that Space. */
  spaceName: string | null;
  /** The override; null inherits the runner default. */
  mode: ShellMode | null;
  approver: ShellApprover | null;
  effectiveMode: ShellMode;
  effectiveApprover: ShellApprover;
  downgradedReason: ShellDowngrade | null;
};

export type RunnerShellPolicy = {
  runnerId: string;
  runnerName: string;
  visibility: RunnerVisibility;
  builtIn: boolean;
  default: ShellPolicy;
  spaces: RunnerSpaceShell[];
};

/** What a call does in one Space now, as every member reads it. */
export type SpaceShell = {
  mode: ShellMode;
  approver: ShellApprover;
  effectiveMode: ShellMode;
  downgradedReason: ShellDowngrade | null;
  runnerName: string;
  source: "dedicated" | "runner_default" | "space_override";
};

export type ShellPerson = { id: string; name: string };
export type ShellReasonCode = "policy" | "classify_unsafe" | "classify_unsure" | "classify_failed" | "built_in_multiple_spaces";

export type ShellApprovalRequest = {
  approvalId: string;
  runId: string;
  command: string;
  requestedBy: ShellPerson;
  runnerName: string;
  approver: ShellApprover;
  reasonCode: ShellReasonCode | null;
  reason: string;
  expiresAt: string | null;
};

export type ShellDecision = "approve" | "deny";
export type ShellResolution = { approvalId: string; decision: ShellDecision | "expired" | "cancelled"; by: ShellPerson | null };
export type PendingShellApproval = ShellApprovalRequest & { roomId: string; canDecide: boolean };

const object = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);
const text = (v: unknown, max: number): v is string => typeof v === "string" && v.length <= max;
const isMode = (v: unknown): v is ShellMode => typeof v === "string" && (SHELL_MODES as readonly string[]).includes(v);
const isApprover = (v: unknown): v is ShellApprover => v === "owner" || v === "members";
const downgrade = (v: unknown): ShellDowngrade | null =>
  v === "built_in_multiple_spaces" || v === "classify_unavailable" ? v : null;
const REASON_CODES: readonly string[] = ["policy", "classify_unsafe", "classify_unsure", "classify_failed", "built_in_multiple_spaces"];
const APPROVAL_ID = /^sa-[A-Za-z0-9_-]{1,80}$/;
const SPACE_ID = /^[a-z0-9][a-z0-9_-]{0,63}$/;
/** The longest command the server accepts, in UTF-8 bytes. */
export const MAX_SHELL_COMMAND_BYTES = 16_384;
const LONE_SURROGATE = /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/;
const utf8 = new TextEncoder();
/** A command as the contract allows it: 1..=16384 UTF-8 bytes, valid UTF-8, no NUL. */
export function isShellCommand(v: unknown): v is string {
  if (typeof v !== "string" || !v || v.includes("\u0000") || LONE_SURROGATE.test(v)) return false;
  // Cheap bound first: a string never has more UTF-16 units than UTF-8 bytes.
  return v.length <= MAX_SHELL_COMMAND_BYTES && utf8.encode(v).length <= MAX_SHELL_COMMAND_BYTES;
}
export const isApprovalId = (v: unknown): v is string => typeof v === "string" && APPROVAL_ID.test(v);

function person(v: unknown): ShellPerson | null {
  if (!object(v) || !text(v.id, 200) || !v.id) return null;
  return { id: v.id, name: text(v.name, 200) && v.name.trim() ? v.name : v.id };
}

/** `shell` on GET …/runner, or null when absent or unreadable (never guessed). */
export function readSpaceShell(v: unknown): SpaceShell | null {
  if (!object(v) || !isMode(v.shell_mode) || !isApprover(v.approver) || !isMode(v.effective_mode)) return null;
  const source = v.source === "dedicated" || v.source === "runner_default" || v.source === "space_override" ? v.source : null;
  if (!source) return null;
  return {
    mode: v.shell_mode, approver: v.approver, effectiveMode: v.effective_mode,
    downgradedReason: downgrade(v.downgraded_reason),
    runnerName: text(v.runner_name, 200) ? v.runner_name : "",
    source,
  };
}

function readRunnerPolicy(v: unknown, what: string): RunnerShellPolicy {
  const d = object(v) ? v.default : undefined;
  if (!object(v) || !text(v.runner_id, 200) || !object(d) || !isMode(d.shell_mode) || !isApprover(d.approver) ||
      (v.visibility !== "personal" && v.visibility !== "organization") || !Array.isArray(v.spaces)) {
    throw new ApiError("protocol", `${what} did not return a shell policy.`);
  }
  const spaces: RunnerSpaceShell[] = [];
  for (const s of v.spaces) {
    if (!object(s) || typeof s.space_id !== "string" || !SPACE_ID.test(s.space_id) || !isMode(s.effective_mode) || !isApprover(s.effective_approver)) {
      throw new ApiError("protocol", `${what} returned a Space this app doesn't understand.`);
    }
    spaces.push({
      spaceId: s.space_id,
      spaceName: text(s.space_name, 200) ? s.space_name : null,
      mode: isMode(s.shell_mode) ? s.shell_mode : null,
      approver: isApprover(s.approver) ? s.approver : null,
      effectiveMode: s.effective_mode,
      effectiveApprover: s.effective_approver,
      downgradedReason: downgrade(s.downgraded_reason),
    });
  }
  return {
    runnerId: v.runner_id,
    runnerName: text(v.runner_name, 200) ? v.runner_name : "",
    visibility: v.visibility,
    builtIn: v.built_in === true,
    default: { mode: d.shell_mode, approver: d.approver },
    spaces,
  };
}

function readRequestFields(v: Record<string, unknown>): ShellApprovalRequest | null {
  const by = person(v.requested_by);
  if (!isApprovalId(v.approval_id) || !text(v.run_id, 200) || !isShellCommand(v.command) || !by || !isApprover(v.approver)) return null;
  return {
    approvalId: v.approval_id,
    runId: v.run_id,
    command: v.command,
    requestedBy: by,
    runnerName: text(v.runner_name, 200) ? v.runner_name : "",
    approver: v.approver,
    reasonCode: typeof v.reason_code === "string" && REASON_CODES.includes(v.reason_code) ? v.reason_code as ShellReasonCode : null,
    reason: text(v.reason, 1000) ? v.reason.trim() : "",
    expiresAt: typeof v.expires_at === "string" && Number.isFinite(Date.parse(v.expires_at)) ? v.expires_at : null,
  };
}

function parseJson(raw: string): unknown {
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

/** A `shell_approval` event's text, or null when it isn't one this app can read. */
export function readShellApproval(raw: string): ShellApprovalRequest | null {
  const v = parseJson(raw);
  return object(v) ? readRequestFields(v) : null;
}

/** A `shell_approval_resolved` event's text, or null. */
export function readShellResolution(raw: string): ShellResolution | null {
  const v = parseJson(raw);
  if (!object(v) || !isApprovalId(v.approval_id)) return null;
  const d = v.decision;
  if (d !== "approve" && d !== "deny" && d !== "expired" && d !== "cancelled") return null;
  return { approvalId: v.approval_id, decision: d, by: d === "approve" || d === "deny" ? person(v.by) : null };
}

/**
 * A command as it may be drawn: bidi controls, invisible format characters,
 * line and paragraph separators and other control characters (except real
 * line breaks and tabs) become visible `⟨U+XXXX⟩` marks. Display only: the
 * raw command is what is compared, approved and run.
 */
export function visibleCommand(raw: string): string {
  // Cc/Cf cover C0, DEL, C1, bidi, zero-width and tag characters; also the
  // separators, variation selectors, the grapheme joiner and Hangul fillers.
  return raw.replace(/[\p{Cc}\p{Cf}\p{Zl}\p{Zp}\u034F\u115F\u1160\u3164\uFFA0\uFE00-\uFE0F\u{E0100}-\u{E01EF}]/gu, (ch) => {
    if (ch === "\n" || ch === "\t") return ch;
    return `⟨U+${ch.codePointAt(0)!.toString(16).toUpperCase().padStart(4, "0")}⟩`;
  });
}

export function shellApi(request: RequestFn) {
  const runner = (id: string) => `/v1/runners/${encodeURIComponent(id)}`;
  const sp = (sid: string) => `/v1/spaces/${encodeURIComponent(sid)}`;
  return {
    async getRunnerShellPolicy(runnerId: string): Promise<RunnerShellPolicy> {
      return readRunnerPolicy(await request("GET", `${runner(runnerId)}/shell-policy`), "GET …/shell-policy");
    },
    async setRunnerShellPolicy(runnerId: string, policy: ShellPolicy): Promise<RunnerShellPolicy> {
      const data = await request("PUT", `${runner(runnerId)}/shell-policy`, { shell_mode: policy.mode, approver: policy.approver });
      return readRunnerPolicy(data, "PUT …/shell-policy");
    },
    /** Both null: inherit the runner default. */
    async setRunnerSpaceShellPolicy(runnerId: string, spaceId: string, policy: { mode: ShellMode | null; approver: ShellApprover | null }): Promise<RunnerShellPolicy> {
      const data = await request("PUT", `${runner(runnerId)}/spaces/${encodeURIComponent(spaceId)}/shell-policy`,
        { shell_mode: policy.mode, approver: policy.approver });
      return readRunnerPolicy(data, "PUT …/spaces/…/shell-policy");
    },
    /** A dedicated runner's policy for one Space (Space owner only). */
    async setSpaceShellPolicy(spaceId: string, policy: ShellPolicy): Promise<SpaceShell> {
      const data = await request("PUT", `${sp(spaceId)}/shell-policy`, { shell_mode: policy.mode, approver: policy.approver });
      const shell = object(data) ? readSpaceShell(data.shell) : null;
      if (!shell) throw new ApiError("protocol", "PUT …/shell-policy did not return { shell }.");
      return shell;
    },
    async listShellApprovals(spaceId: string): Promise<PendingShellApproval[]> {
      const data = await request("GET", `${sp(spaceId)}/shell-approvals`);
      if (!object(data) || !Array.isArray(data.approvals)) throw new ApiError("protocol", "GET …/shell-approvals did not return { approvals }.");
      const out: PendingShellApproval[] = [];
      for (const a of data.approvals) {
        const fields = object(a) ? readRequestFields(a) : null;
        if (!fields || !object(a) || typeof a.room_id !== "string") continue;
        out.push({ ...fields, roomId: a.room_id, canDecide: a.can_decide === true });
      }
      return out;
    },
    async decideShellApproval(spaceId: string, approvalId: string, decision: ShellDecision): Promise<ShellResolution> {
      if (!isApprovalId(approvalId)) throw new ApiError("protocol", "Not a shell approval id.");
      const data = await request("POST", `${sp(spaceId)}/shell-approvals/${encodeURIComponent(approvalId)}`, { decision });
      const by = object(data) ? person(data.by) : null;
      if (!object(data) || data.approval_id !== approvalId || data.decision !== decision || !by) {
        throw new ApiError("protocol", "The server did not confirm the decision.");
      }
      return { approvalId, decision, by };
    },
  };
}
