import type { ViewRef } from "nsh";
import { plainHttpWarning } from "../api/serverUrl";
import { Action, Banner, Column, Row } from "../ui";

/** Plain HTTP works everywhere; only hosts outside this computer and Tailscale ask first. */
export const SERVER_URL_HINT = "HTTPS, or HTTP on this computer, Tailscale or a network you trust.";

/**
 * The one-time question before a token goes over plain HTTP to a host that
 * is neither loopback nor Tailscale. `testID` prefixes the buttons:
 * `${testID}:accept` and `${testID}:cancel`.
 */
export function PlainHttpConsent(props: {
  baseUrl: string; testID: string; onAccept: () => void; onCancel: () => void; cancelRef?: (node: ViewRef) => void;
}) {
  return (
    <Column gap={2}>
      <Banner variant="inline" tone="warning" testID={props.testID} message={plainHttpWarning(props.baseUrl)} />
      <Row gap={2} justify="end">
        <Action testID={`${props.testID}:cancel`} label="Cancel" variant="secondary" onPress={props.onCancel} viewRef={(node) => props.cancelRef?.(node)} />
        <Action testID={`${props.testID}:accept`} label="Connect anyway" variant="primary" onPress={props.onAccept} />
      </Row>
    </Column>
  );
}
