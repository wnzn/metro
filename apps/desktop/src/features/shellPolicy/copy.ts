// Labels and help for runner shell policies. Short and literal; the help
// says what happens, nothing more. Domain mapping only, no UI.

import type { ShellApprover, ShellDowngrade, ShellMode } from "../../api/shell";
import type { PickerItem } from "../../ui";

export const MODE_LABEL: Record<ShellMode, string> = {
  off: "Off",
  approve: "Ask every time",
  classify: "Ask when System One flags it",
  allow: "Always allow",
};

export const MODE_HELP: Record<ShellMode, string> = {
  off: "Agents can't run commands.",
  approve: "Each command waits for an approver.",
  classify: "System One checks each command on the runner owner's connection. Flagged or unclear ones wait for an approver.",
  allow: "Commands run without asking.",
};

export const APPROVER_LABEL: Record<ShellApprover, string> = {
  owner: "Runner owner",
  members: "Any Space member",
};

/** For a dedicated runner the owner is the Space owner. */
export const DEDICATED_APPROVER_LABEL: Record<ShellApprover, string> = {
  owner: "Space owner",
  members: "Any Space member",
};

/** For an organization runner the owner is any org admin. */
export const ORG_APPROVER_LABEL: Record<ShellApprover, string> = {
  owner: "Org admin",
  members: "Any Space member",
};

export function downgradeText(reason: ShellDowngrade): string {
  return reason === "built_in_multiple_spaces"
    ? "A built-in runner allows commands only while it serves exactly one Space, so each command asks."
    : "Organization runners can't use System One checks, so each command asks.";
}

export const CLASSIFY_UNAVAILABLE = "Not available for organization runners.";

export function modeItems(organization: boolean, inherit?: ShellMode): PickerItem[] {
  const items: PickerItem[] = (["off", "approve", "classify", "allow"] as const).map((mode) => ({
    value: mode,
    label: MODE_LABEL[mode],
    detail: mode === "classify" && organization ? CLASSIFY_UNAVAILABLE : MODE_HELP[mode],
    disabled: mode === "classify" && organization,
  }));
  return inherit ? [{ value: "inherit", label: `Inherit (${MODE_LABEL[inherit]})`, detail: "Use the runner default." }, ...items] : items;
}

export function approverItems(labels: Record<ShellApprover, string>, inherit?: ShellApprover): PickerItem[] {
  const items: PickerItem[] = (["owner", "members"] as const).map((a) => ({ value: a, label: labels[a] }));
  return inherit ? [{ value: "inherit", label: `Inherit (${labels[inherit]})` }, ...items] : items;
}

/** "Ask every time · Runner owner approves". */
export function effectiveText(mode: ShellMode, approver: ShellApprover, labels: Record<ShellApprover, string> = APPROVER_LABEL): string {
  if (mode === "off" || mode === "allow") return MODE_LABEL[mode];
  return `${MODE_LABEL[mode]} · ${labels[approver]} approves`;
}
