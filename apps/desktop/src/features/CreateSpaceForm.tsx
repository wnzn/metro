import { cleanup, effect, signal } from "nsh";
import { attachCreatedSpaceRunner, installedRunnerChoice, snapshotInstalledRunner } from "../runnersState";
import { activeSpace, createSpace, creatingSpace, onSessionEnd, onTransientClear, openManage } from "../store";
import { Action, Banner, Checkbox, Column, Field, modalOpen } from "../ui";

type Feedback = { spaceId: string; tone: "warning" | "success"; message: string; failed: boolean };
const feedback = signal<Feedback | null>(null);
onTransientClear(() => feedback.set(null));
onSessionEnd(() => feedback.set(null));

// The source onboarding may unmount while verification is pending. Keep
// observing dialogs until that submission finishes, even after it unmounts.
let modalEpoch = 0;
effect(() => {
  modalOpen();
  modalEpoch++;
});

export function CreateSpaceForm(props: {
  onboarding?: boolean;
  visible?: boolean;
  name: () => string;
  setName: (name: string) => void;
  onCreated?: () => void;
}) {
  const checked = signal(true);
  const runnerId = () => props.visible !== false ? installedRunnerChoice()?.id ?? null : null;
  let mounted = true;
  let revision = 0;
  effect(() => {
    props.visible;
    revision++;
  });
  cleanup(() => { mounted = false; });

  const submit = async () => {
    if (!mounted || props.visible === false || modalOpen() || creatingSpace()) return;
    const id = runnerId();
    const choice = checked() && id ? snapshotInstalledRunner(id) : null;
    const submitted = revision;
    const submittedModal = modalEpoch;
    const unchanged = () => revision === submitted && modalEpoch === submittedModal && !modalOpen();
    const created = await createSpace(props.name(), choice ? {
      formIsCurrent: () => mounted && unchanged(),
      async attachRunner(space, fromOriginalForm, stillInCreatedSpace) {
        const stillConsented = () => fromOriginalForm && unchanged() && stillInCreatedSpace() &&
          // Onboarding intentionally unmounts when the new Space appears.
          (props.onboarding || mounted);
        const result = await attachCreatedSpaceRunner(space, choice, stillConsented);
        if (!stillInCreatedSpace()) return;
        if ("reason" in result) {
          feedback.set({
            spaceId: space.id, failed: true, tone: "warning",
            message: `Created ${space.name}, but this computer's runner couldn't be attached: ${result.reason}.`,
          });
        } else {
          const status = result.offline ? " It's offline now; jobs wait until it's back." : "";
          feedback.set({
            spaceId: space.id, failed: false, tone: result.offline ? "warning" : "success",
            message: `Created ${space.name} with this computer's runner.${status}`,
          });
        }
      },
    } : undefined);
    if (created && mounted) props.onCreated?.();
  };

  return (
    <Column gap={2}>
      <Field testID={props.onboarding ? "onboarding-space-name" : "new-space-name"}
        accessibilityLabel="New Space name" value={props.name()} onChange={props.setName}
        placeholder={props.onboarding ? "Name your first Space" : "Space name"}
        autoFocus={!props.onboarding} maxLength={80} onSubmit={() => void submit()} />
      <If when={runnerId()}>
        <Checkbox testID={props.onboarding ? "onboarding-space-use-runner" : "new-space-use-runner"}
          label="Use this computer's runner" hint="Agents in this Space can list and read files in its own folder on this computer."
          checked={checked()} onChange={(next) => { revision++; checked.set(next); }} />
      </If>
      <Action testID={props.onboarding ? "onboarding-create-space" : "create-space"}
        label="Create Space" busyLabel="Creating…" busy={creatingSpace()} variant="primary" onPress={() => void submit()} />
    </Column>
  );
}

export function NewSpaceRunnerFeedback() {
  return (
    <If when={feedback()}>
      {(result) => (
        <Banner testID={result().failed ? "new-space-runner-failed" : "new-space-runner-created"}
          tone={result().tone} message={result().message}
          action={result().failed && activeSpace()?.role === "owner" ? (
            <Action compact testID="new-space-open-runner" label="Open Runner" variant="ghost" onPress={() => {
              if (activeSpace()?.id !== result().spaceId || activeSpace()?.role !== "owner") return;
              // Until U9: this is the created Space's existing runner management.
              void openManage();
            }} />
          ) : undefined} />
      )}
    </If>
  );
}
