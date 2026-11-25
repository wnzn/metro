import { effect } from "nsh";
import { Action, Column, Label, Modal } from "../../ui";
import { sidePanelFits } from "../../ui/activity";
import { closeForward, forwardSelection } from "./state";

/** L8 replaces the stub contents, using forwardSelection and closeForward. */
export function ForwardEntry() {
  effect(() => {
    const selection = forwardSelection();
    if (selection && !selection.current()) forwardSelection.set(null);
  });
  return (
    <Modal testID="forward-dialog" title="Forward message" open={!!forwardSelection()?.current()}
      onClose={closeForward} size={sidePanelFits() ? "compact" : "full-height"}>
      <Column pad={4} gap={3}>
        <Label variant="heading" heading>Forward message</Label>
        <Label color="muted">Forwarding isn’t available yet.</Label>
        <Action label="Cancel" onPress={closeForward} />
      </Column>
    </Modal>
  );
}
