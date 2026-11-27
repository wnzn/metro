// A `shell_approval` room or thread event: the command an agent run asked to
// run on the Space's runner, who asked, which runner, and why it needs a
// person. Approve and Deny appear only when the server's pending list says
// this person can decide it, and only until it is decided, expired or
// cancelled. The outcome comes from `shell_approval_resolved` (or this
// person's own confirmed decision until that event arrives).
//
// Run activity, never a message: no reply, reaction or forward.

import { cleanup, effect, signal, untracked } from "nsh";
import type { RoomEvent } from "../../api/client";
import { readShellApproval, visibleCommand } from "../../api/shell";
import type { ShellApprovalRequest, ShellResolution } from "../../api/shell";
import { activeSpaceId, me } from "../../store";
import { Action, Badge, Column, Label, Row, Surface } from "../../ui";
import type { Tone } from "../../ui";
import { CommandBlock } from "../../ui/activity";
import { isSystemEvent } from "./group";
import {
  canDecideShell, decideShell, decidingApproval, loadShellApprovals, ownShellDecision, shellApprovalClosed, shellApprovalError,
} from "./shellApprovals";

const REFRESH_MS = 10_000;

export function resolutionText(r: ShellResolution, myId: string | null): string {
  const who = r.by ? (r.by.id === myId ? "you" : r.by.name) : null;
  switch (r.decision) {
    case "approve": return who ? `Approved by ${who}.` : "Approved.";
    case "deny": return who ? `Denied by ${who}.` : "Denied.";
    case "expired": return "Expired. Nobody decided within 10 minutes.";
    default: return "Cancelled. The run stopped or the Space's runner changed.";
  }
}

function resolutionTone(r: ShellResolution): Tone {
  return r.decision === "approve" ? "success" : r.decision === "deny" ? "danger" : "neutral";
}

const approverText = (a: ShellApprovalRequest["approver"]) =>
  a === "members" ? "Waiting for a member of this Space." : "Waiting for the runner's owner.";

/**
 * `roomId`: the room or thread the transcript shows. Only a system event in
 * a known room can offer a decision.
 */
export function ShellApprovalCard(props: { event: RoomEvent; resolution: ShellResolution | null; roomId: string | null }) {
  const request = () => readShellApproval(props.event.text);
  const fromSystem = () => isSystemEvent(props.event);
  return (
    <If when={request()} else={
      <Surface variant="card" gap={2} pad={3} testID={`shell-approval-unreadable:${props.event.id}`}>
        <Badge tone="warning" label="Command approval" />
        <Label variant="caption" color="muted">This approval request can't be shown.</Label>
      </Surface>
    }>
      {(r) => <Card request={r()} resolution={props.resolution} roomId={fromSystem() ? props.roomId : null} />}
    </If>
  );
}

function Card(props: { request: ShellApprovalRequest; resolution: ShellResolution | null; roomId: string | null }) {
  const sid = activeSpaceId();
  const id = () => props.request.approvalId;
  const expiresMs = () => (props.request.expiresAt ? Date.parse(props.request.expiresAt) : NaN);
  const expired = signal(false);
  const outcome = () => props.resolution ?? ownShellDecision(id());
  const closed = () => shellApprovalClosed(id());
  const open = () => !outcome() && !closed() && !expired();
  const shown = () => (props.roomId ? { request: props.request, roomId: props.roomId } : null);
  const canDecide = () => !!sid && activeSpaceId() === sid && open() && !!shown() && canDecideShell(sid, shown()!);
  const display = () => visibleCommand(props.request.command);
  const decide = (decision: "approve" | "deny") => {
    const target = shown();
    if (sid && target) void decideShell(sid, target, decision);
  };
  const busy = () => decidingApproval() === id();

  effect(() => {
    const at = expiresMs();
    if (!Number.isFinite(at) || outcome()) return;
    const left = at - Date.now();
    if (left <= 0) {
      expired.set(true);
      return;
    }
    const timer = setTimeout(() => expired.set(true), Math.min(left, 2_147_000_000));
    cleanup(() => clearTimeout(timer));
  });

  // Only an open request needs to know who may decide it.
  effect(() => {
    if (!sid || !open() || !props.roomId) return;
    untracked(() => void loadShellApprovals(sid));
    const timer = setInterval(() => { if (untracked(open)) void loadShellApprovals(sid); }, REFRESH_MS);
    cleanup(() => clearInterval(timer));
  });

  const status = (): { tone: Tone | "info"; label: string } => {
    const r = outcome();
    if (r) return { tone: resolutionTone(r), label: r.decision === "approve" ? "Approved" : r.decision === "deny" ? "Denied" : r.decision === "expired" ? "Expired" : "Cancelled" };
    if (closed()) return { tone: "neutral", label: "Closed" };
    if (expired()) return { tone: "neutral", label: "Expired" };
    return { tone: "warning", label: "Waiting for approval" };
  };

  return (
    <Surface variant="card" gap={2} pad={3} testID={`shell-approval:${id()}`}>
      <Row gap={2} align="center" wrap>
        <Badge tone="runner" label="Run command" />
        <Badge testID={`shell-approval-status:${id()}`} tone={status().tone} label={status().label} />
      </Row>
      <CommandBlock text={display()} testID={`shell-approval-command:${id()}`}
        accessibilityLabel={`Command: ${display()}`} />
      <Column gap={1}>
        <Label variant="caption" testID={`shell-approval-requester:${id()}`}>
          {`Requested by ${props.request.requestedBy.id === me() ? "you" : props.request.requestedBy.name}${props.request.runnerName ? ` · Runner: ${props.request.runnerName}` : ""}`}
        </Label>
        <If when={props.request.reason}>
          {(reason) => <Label variant="caption" color="muted" testID={`shell-approval-reason:${id()}`}>{reason()}</Label>}
        </If>
      </Column>
      <If when={outcome()}>
        {(r) => <Label variant="caption" testID={`shell-approval-result:${id()}`}>{resolutionText(r(), me())}</Label>}
      </If>
      <If when={!outcome() && expired() && !closed()}>
        <Label variant="caption" color="muted" testID={`shell-approval-result:${id()}`}>Expired. Nobody decided within 10 minutes.</Label>
      </If>
      <If when={canDecide() || busy()} else={
        <If when={open()}>
          <Label variant="caption" color="muted" testID={`shell-approval-waiting:${id()}`}>{approverText(props.request.approver)}</Label>
        </If>
      }>
        <Column gap={1}>
          <Label variant="caption" color="muted">Approving runs the command once on the runner, in this Space's folder.</Label>
          <Row gap={2} wrap>
            <Action compact variant="primary" testID={`shell-approve:${id()}`} label="Approve" busyLabel="Sending…"
              busy={busy()} disabled={!!decidingApproval()} accessibilityLabel={`Approve command: ${display()}`}
              onPress={() => decide("approve")} />
            <Action compact variant="destructive" testID={`shell-deny:${id()}`} label="Deny"
              disabled={!!decidingApproval()} accessibilityLabel={`Deny command: ${display()}`}
              onPress={() => decide("deny")} />
          </Row>
        </Column>
      </If>
      <If when={shellApprovalError(id())}>
        {(message) => <Label variant="caption" color="danger" testID={`shell-approval-error:${id()}`}>{message()}</Label>}
      </If>
    </Surface>
  );
}
