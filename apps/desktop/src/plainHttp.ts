// Consent to send a token over plain HTTP to a host that is neither loopback
// nor Tailscale (see api/serverUrl.ts). The person confirms once per exact
// server address; the store refuses to send a token to such an address until
// they have.
//
// Where it is kept: in this run's memory, and for a saved server also in its
// saved record (savedWorkspaces.ts writes `plainHttp: true` beside its name
// and address, and drops it with Forget). An address that isn't saved asks
// again after a restart.

import { signal, untracked } from "nsh";
import { parseServerUrl } from "./api/serverUrl";

const accepted = signal<ReadonlySet<string>>(new Set());

/** Whether the person already agreed to use this exact address over plain HTTP. */
export function plainHttpAccepted(baseUrl: string): boolean {
  return accepted().has(baseUrl);
}

export function acceptPlainHttp(baseUrl: string): void {
  const now = untracked(accepted);
  if (now.has(baseUrl)) return;
  accepted.set(new Set([...now, baseUrl]));
}

export function forgetPlainHttp(baseUrl: string): void {
  const now = untracked(accepted);
  if (!now.has(baseUrl)) return;
  const next = new Set(now);
  next.delete(baseUrl);
  accepted.set(next);
}

/**
 * The canonical address that needs the person's confirmation before a token
 * may go to it, or null when it needs none (https, loopback, Tailscale, an
 * address already confirmed, or input that isn't a valid address at all —
 * that error is reported where the address is used).
 */
export function pendingPlainHttp(urlInput: string): string | null {
  const parsed = parseServerUrl(urlInput);
  if (!parsed.ok || !parsed.needsConsent) return null;
  return untracked(accepted).has(parsed.baseUrl) ? null : parsed.baseUrl;
}
