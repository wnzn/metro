import { cleanup, effect, signal } from "nsh";
import { attachCreatedSpaceRunner, installedRunnerChoice, snapshotInstalledRunner } from "../runnersState";
import { activeSpace, createSpace, creatingSpace, onSessionEnd, onTransientClear, openManage, sessionGuard } from "../store";
import { Action, Banner, Checkbox, Column, Field, Label, modalOpen } from "../ui";

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
  const offeredInSession = signal<(() => boolean) | null>(null);
  const wasOffered = () => !!offeredInSession()?.();
  const runnerId = () => props.visible !== false ? installedRunnerChoice()?.id ?? null : null;
  effect(() => {
    if (runnerId() && !modalOpen()) offeredInSession.set(sessionGuard());
  });
  const choiceMissing = () => checked() && wasOffered() && !runnerId();
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
    // A previously displayed checked choice can disappear (e.g. Reconnect).
    // Keep that intent for the outcome notice, never for an attachment.
    const wantsRunner = checked() && (!!id || wasOffered());
    const submitted = revision;
    const submittedModal = modalEpoch;
    const unchanged = () => revision === submitted && modalEpoch === submittedModal && !modalOpen();
    const created = await createSpace(props.name(), wantsRunner ? {
      formIsCurrent: () => mounted && unchanged(),
      async attachRunner(space, scope) {
        const stillConsented = () => unchanged() && scope.mayAttach() &&
          // Onboarding intentionally unmounts when the new Space appears.
          (props.onboarding || mounted);
        const result = choice ? await attachCreatedSpaceRunner(space, choice, stillConsented)
          : { kind: "not-attached" as const, reason: "This computer's runner wasn't available when you submitted." };
        if (!scope.mayShowOutcome()) return;
        if (result.kind !== "attached") {
          const detail = result.kind === "not-attached"
            ? `Metro didn't attach this computer's runner. ${result.reason} Open Runner to check this Space's runner settings.`
            : result.kind === "unconfirmed"
              ? "Metro couldn't confirm whether this computer's runner was attached. Open Runner to check this Space before retrying."
              : `${result.reason} Open Runner to check this Space's current runner.`;
          feedback.set({
            spaceId: space.id, failed: true, tone: "warning",
            message: `Created ${space.name}. ${detail}`,
          });
        } else {
          if (!scope.mayAttach()) return;
          const status = result.state === "online" ? ""
            : result.state === "revoked"
              ? " Its token is no longer accepted. Reconnect it in Settings › Runners. Jobs may time out until it reconnects."
              : " It was offline when last checked; jobs may time out before it comes back.";
          feedback.set({
            spaceId: space.id, failed: false, tone: result.state === "online" ? "success" : "warning",
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
      <If when={choiceMissing()}>
        <Label variant="caption" color="muted"
          testID={props.onboarding ? "onboarding-space-runner-unavailable" : "new-space-runner-unavailable"}>
          This computer's runner is unavailable. You can create the Space now and attach a runner later.
        </Label>
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
