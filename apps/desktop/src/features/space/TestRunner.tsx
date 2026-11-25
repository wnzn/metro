// Test runner: the deterministic `workspace.list` on a Space's runner
// (POST /v1/spaces/{sid}/rooms/{rid}/demo), a check that the runner answers.
// No model is used. Its listing is posted in the room the user has open in
// that Space, as one collapsed Runner activity group. Offered in Space settings › Runner.

import { signal } from "nsh";
import { activeRoom, activeSpaceId, actionError, demoPending, mainPlace, runDemo, status } from "../../store";
import { Action, Column, Label } from "../../ui";

export const TEST_RUNNER_LABEL = "Test runner";
export const TEST_RUNNER_DESCRIPTION = "Lists the workspace on this Space's runner; no model is used";

/** Why the runner of Space `sid` can't be tested now, or null when it can. */
function testRunnerBlocked(sid: string | null, paired: boolean): string | null {
  if (!sid) return "No Space is open.";
  if (!paired) return "Attach or pair a runner first.";
  if (status().phase !== "online") return "Metro is offline.";
  if (mainPlace() !== "space" || activeSpaceId() !== sid || !activeRoom()) return "Open a room of this Space first: the listing is posted there.";
  return null;
}

/** The Test runner button, with what happened under it (requested, or why it can't). */
export function TestRunnerAction(props: { spaceId: string | null; paired: boolean }) {
  const result = signal<{ ok: boolean; text: string } | null>(null);
  const blocked = () => testRunnerBlocked(props.spaceId, props.paired);
  const run = async () => {
    if (blocked() || demoPending()) return;
    const room = activeRoom()?.name ?? "this room";
    result.set(null);
    const ok = await runDemo();
    result.set(ok
      ? { ok, text: `Requested. The listing appears in # ${room} as Runner activity.` }
      : { ok, text: actionError() ?? "Could not test the runner." });
  };
  return (
    <Column gap={1} align="end">
      <Action
        testID="test-runner"
        label={TEST_RUNNER_LABEL}
        busyLabel="Requesting…"
        accessibilityLabel={blocked() ? `${TEST_RUNNER_LABEL} (${blocked()})` : `${TEST_RUNNER_LABEL}: ${TEST_RUNNER_DESCRIPTION}`}
        busy={demoPending()}
        disabled={!!blocked()}
        onPress={() => void run()}
      />
      <If when={blocked() ?? result()?.text}>
        {(text) => (
          <Label variant="caption" color={!blocked() && result()?.ok === false ? "danger" : "muted"} testID="test-runner-note">
            {text()}
          </Label>
        )}
      </If>
    </Column>
  );
}
