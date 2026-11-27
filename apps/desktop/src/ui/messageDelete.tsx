// Deleting a message: the toolbar's Delete control, its confirmation and
// what remains of a deleted message that anchors a thread. Feature code
// passes only semantic props; classes live here and in theme/theme.json.

import { Image, Text, View, token } from "nsh";
import type { ViewRef } from "nsh";
import { Button } from "nsh/base";
import trashIcon from "./icons/trash.svg";
import { cx } from "./tokens";
import { Action } from "./primitives";

type Testable = { testID?: string; accessibilityLabel?: string };

/** The message toolbar's Delete button; it looks like the toolbar's other icon buttons. */
export function DeleteMessageButton(props: Testable & {
  accessibilityLabel: string;
  tooltip?: string;
  onPress: () => void;
  disabled?: boolean;
  active?: boolean;
  ref?: (node: ViewRef) => void;
}) {
  return (
    <Button
      class={cx("items-center justify-center size-[28px] rounded-[5px] hover:bg-muted active:bg-muted disabled:opacity-50",
        "focus-visible:ring-2 focus-visible:ring-ring", props.active && "bg-muted")}
      ref={props.ref}
      selected={props.active}
      disabled={props.disabled}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
      tooltip={props.tooltip
        ? <Text class="rounded-(--radius-sm) bg-popover border border-border px-(--space-2) py-[3px] text-(length:--text-xs) text-popover-foreground">{props.tooltip}</Text>
        : undefined}
    >
      <Image src={trashIcon} fadeDuration={0} accessibilityElementsHidden
        style={{ width: 16, height: 16, tintColor: token(props.active ? "tone-danger-foreground" : "muted-foreground") }} />
    </Button>
  );
}

/**
 * The confirmation inside the Delete popover: what happens, then Delete and
 * Cancel. Delete takes the focus first; Escape (the popover's) cancels.
 */
export function DeleteConfirmation(props: Testable & {
  title: string;
  detail: string;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <View class="w-[260px] gap-(--space-2) p-(--space-2)" testID={props.testID}>
      <Text class="text-(length:--text-sm) font-semibold text-popover-foreground">{props.title}</Text>
      <Text class="text-(length:--text-xs) text-muted-foreground">{props.detail}</Text>
      <View class="flex-row justify-end gap-(--space-2)">
        <Action compact variant="ghost" label="Cancel" testID={props.testID ? `${props.testID}:cancel` : undefined} onPress={props.onCancel} />
        <Action compact variant="destructive" label="Delete" busy={props.busy} busyLabel="Deleting…"
          testID={props.testID ? `${props.testID}:confirm` : undefined}
          viewRef={node => queueMicrotask(() => node.focus())} onPress={props.onConfirm} />
      </View>
    </View>
  );
}

/** A deleted message kept because its thread has replies: one quiet line. */
export function DeletedMessage(props: Testable) {
  return (
    <Text class="text-(length:--text-sm) italic text-muted-foreground" testID={props.testID}>Message deleted</Text>
  );
}
