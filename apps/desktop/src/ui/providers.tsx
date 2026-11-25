// Model providers primitives: the add-connection picker's frame, a
// provider's heading, one way to connect as a single pressable row, the
// one-line summary of per-capability defaults, and a saved connection's
// compact row, ⋯ menu, problem note, details heading and model rows. Like ./primitives, feature code
// passes only semantic props; every class lives here.

import { Text, View } from "nsh";
import type { JSX, ViewRef } from "nsh";
import { Button, Menu, MenuItem, MenuSeparator } from "nsh/base";
import { BrandMark, Icon } from "./chrome";
import type { Mark } from "./chrome";
import { Badge, StatusDot } from "./primitives";
import { cx, disabledClass, focusRingClass, popoverClass, popoverItemClass, popoverMotion, textClass, toneTextClass, tooltipClass, transitionClass } from "./tokens";
import type { Tone } from "./tokens";

/** The add-connection picker: a quiet card holding its search, filter and rows. */
export function PickerPanel(props: { testID?: string; accessibilityLabel: string; children?: JSX.Element }) {
  return (
    <View
      class="flex-col gap-(--space-3) rounded-(--radius) border border-border bg-card p-(--space-3)"
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
    >
      {props.children}
    </View>
  );
}

/**
 * A provider's heading in the picker: its mark and name. Pressing it narrows
 * the list to that provider (`selected` while narrowed).
 */
export function VendorHeading(props: { mark: Mark; name: string; selected: boolean; onPress: () => void; testID?: string; accessibilityLabel?: string }) {
  return (
    <Button
      class={cx(
        "flex-row items-center gap-(--space-2) self-start rounded-(--radius-sm) px-(--space-1) py-[3px] hover:bg-muted active:bg-muted",
        transitionClass,
        focusRingClass,
        props.selected && "bg-muted",
      )}
      selected={props.selected}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.name}
    >
      <BrandMark mark={props.mark} />
      <Text class={cx(textClass.sm, "font-semibold text-foreground")}>{props.name}</Text>
    </Button>
  );
}

/**
 * One way to connect, as one pressable row: its name, its capability badge,
 * and one short line on where it runs and who pays. Space or Return chooses it.
 */
export function MethodRow(props: {
  label: string;
  detail: string;
  capability: { label: string; tone: Tone };
  onPress: () => void;
  testID?: string;
  accessibilityLabel?: string;
}) {
  return (
    <Button
      class={cx(
        "flex-row items-center gap-(--space-3) rounded-(--radius-sm) px-(--space-3) py-(--space-2) hover:bg-muted active:bg-muted",
        transitionClass,
        focusRingClass,
      )}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
    >
      <View class="flex-1 min-w-0 flex-col gap-px">
        <Text class={cx(textClass.sm, "font-medium text-foreground")}>{props.label}</Text>
        <Text class={cx(textClass.xs, "text-muted-foreground")}>{props.detail}</Text>
      </View>
      <Badge tone={props.capability.tone} label={props.capability.label} />
    </Button>
  );
}

/** One entry of the defaults line: a capability and its default connection's name (or none). */
export type DefaultEntry = { key: string; label: string; tone: Tone; value: string | null };

/** "Chat · Work API   System One · none   Embeddings · Home": each capability's own default, on one line. */
export function DefaultsLine(props: { entries: readonly DefaultEntry[]; testID?: string }) {
  return (
    <View class="flex-row flex-wrap items-center gap-x-(--space-4) gap-y-(--space-1)" testID={props.testID}>
      <Text class={cx(textClass.xs, "font-medium text-muted-foreground")}>Defaults</Text>
      <Each of={props.entries} key={(e) => e.key}>
        {(e) => (
          <View class="flex-row items-center gap-(--space-1)" testID={props.testID ? `${props.testID}:${e().key}` : undefined}>
            <Badge tone={e().tone} label={e().label} />
            <Text class={cx(textClass.sm, e().value ? "text-foreground" : "text-muted-foreground")}>{e().value ?? "none"}</Text>
          </View>
        )}
      </Each>
    </View>
  );
}

// ------------------------------------------------------------------ saved connections

export type ConnectionTag = {
  label: string;
  tone?: Tone | "info";
  variant?: "soft" | "outline";
  testID?: string;
};

/**
 * A compact list row: a provider mark (or custom `leading`), name with tags,
 * and a status line. Supply `onPress` for a row that opens details; otherwise
 * only the trailing controls are interactive. `tags` describe badges beside
 * the name: each label does double duty as visible and spoken text. Empty
 * labels are skipped. In a pressable row the labels join the button name;
 * in a passive row they are read independently.
 * `children` go under the row.
 */
export function ConnectionItem(props: {
  mark?: Mark;
  leading?: JSX.Element;
  name: string;
  isDefault?: boolean;
  tags?: readonly ConnectionTag[];
  status: { tone: Tone; text: string };
  onPress?: () => void;
  trailing?: JSX.Element;
  children?: JSX.Element;
  testID?: string;
  accessibilityLabel?: string;
}) {
  const identityClass = "flex-1 min-w-0 flex-row items-center gap-(--space-3) px-(--space-2) py-(--space-2)";
  const tags = () => (props.tags ?? []).map((tag) => ({ ...tag, label: tag.label.trim() })).filter((tag) => tag.label);
  const buttonLabel = () => {
    const labels = tags().map((tag) => tag.label);
    return (props.accessibilityLabel !== undefined
      ? [props.accessibilityLabel, ...labels]
      : [props.name, props.isDefault ? "default" : undefined, ...labels, props.status.text]).filter(Boolean).join(", ");
  };
  const identity = (
    <>
      <If when={props.leading} else={<If when={props.mark}>{(mark) => <BrandMark mark={mark()} size="md" />}</If>}>
        <View class="shrink-0" accessibilityElementsHidden>{props.leading}</View>
      </If>
      <View class="flex-1 min-w-0 flex-col gap-px">
        <View class={cx("flex-row items-center gap-(--space-2)", tags().length > 0 && "flex-wrap gap-y-px")}>
          <Text class={cx(textClass.sm, "font-medium text-foreground", tags().length > 0 && "shrink min-w-0")} style={{ numberOfLines: 1 }}
            accessibilityLabel={props.onPress ? undefined : props.accessibilityLabel}>
            {props.name}
          </Text>
          <If when={props.isDefault}>
            <Badge tone="neutral" variant="outline" label="Default" testID={props.testID ? `${props.testID}:default` : undefined} />
          </If>
          <Each of={tags()} key="index">
            {(tag) => <Badge label={tag().label} tone={tag().tone ?? "neutral"} variant={tag().variant} testID={tag().testID} />}
          </Each>
        </View>
        <StatusLine tone={props.status.tone} text={props.status.text} testID={props.testID ? `${props.testID}:status` : undefined} />
      </View>
    </>
  );
  return (
    <View class="flex-col" testID={props.testID}>
      <View class="flex-row items-center gap-(--space-2) px-(--space-2) py-(--space-1)">
        <If when={!!props.onPress} else={<View class={identityClass}>{identity}</View>}>
          <Button
            class={cx(identityClass, "rounded-(--radius-sm) hover:bg-muted active:bg-muted", transitionClass, focusRingClass)}
            onPress={() => props.onPress?.()}
            accessibilityLabel={buttonLabel()}
            testID={props.testID ? `${props.testID}:open` : undefined}
          >
            {identity}
            <Icon name="chevron-right" color="muted" size={14} />
          </Button>
        </If>
        {props.trailing}
      </View>
      {props.children}
    </View>
  );
}

/** A coloured dot and one short line of status ("Online · this computer"). */
export function StatusLine(props: { tone: Tone; text: string; testID?: string }) {
  return (
    <View class="flex-row items-center gap-(--space-2)" testID={props.testID}>
      <StatusDot tone={props.tone} />
      <Text class={cx(textClass.xs, "text-muted-foreground")} style={{ numberOfLines: 1 }}>
        {props.text}
      </Text>
    </View>
  );
}

/**
 * A problem with a connection, as one plain sentence and at most one fix
 * (`action`), under its row or at the top of its details. Never technical text.
 */
export function ProblemNote(props: { text: string; tone?: Tone; action?: JSX.Element; testID?: string }) {
  const tone = () => props.tone ?? "warning";
  return (
    <View class="flex-row flex-wrap items-center gap-(--space-2) pl-[56px] pr-(--space-4) pb-(--space-2)" testID={props.testID} role="alert">
      <Text class={cx(textClass.xs, toneTextClass[tone()])}>{props.text}</Text>
      {props.action}
    </View>
  );
}

export type MoreMenuItem = {
  /** Unique among the items; also the item's testID. */
  id: string;
  label: string;
  onPress: () => void;
  disabled?: boolean;
  /** Removes or revokes something: danger ink, after a separator. */
  destructive?: boolean;
};

/**
 * A row's ⋯ menu: an icon-only trigger (`testID`, said as
 * `accessibilityLabel`) and its items in a popover, with a separator before
 * the first destructive item. Keyboard as NSH's Menu.
 */
export function MoreMenu(props: {
  items: readonly MoreMenuItem[];
  accessibilityLabel: string;
  testID?: string;
  disabled?: boolean;
  /** The persistent opener to return to from an action's inline confirmation. */
  triggerRef?: (node: ViewRef) => void;
}) {
  const rows = () => props.items.map((item, i) => ({ item, separated: !!item.destructive && i > 0 && !props.items[i - 1].destructive }));
  return (
    <Menu
      placement="bottom"
      align="end"
      offset={4}
      popupClass={cx(popoverClass, "min-w-[220px]")}
      popupStartingClass={popoverMotion.starting}
      popupEndingClass={popoverMotion.ending}
      trigger={
        <Button
          ref={props.triggerRef}
          class={cx("items-center justify-center size-[32px] rounded-(--radius-sm) hover:bg-muted active:bg-muted aria-expanded:bg-muted", focusRingClass, transitionClass, disabledClass)}
          disabled={props.disabled}
          accessibilityLabel={props.accessibilityLabel}
          testID={props.testID}
        >
          <Icon name="more" color="muted" size={16} />
        </Button>
      }
    >
      <Each of={rows()} key={(row) => row.item.id}>
        {(row) => (
          <>
            <If when={row().separated}>
              <MenuSeparator class="my-[4px] h-px bg-border" />
            </If>
            <MenuItem class={popoverItemClass} disabled={row().item.disabled} onSelect={() => row().item.onPress()} testID={row().item.id}>
              <Text class={cx(textClass.sm, row().item.destructive ? "text-destructive" : "text-popover-foreground")}>{row().item.label}</Text>
            </MenuItem>
          </>
        )}
      </Each>
    </Menu>
  );
}

/** A details sub-view's way back: "← Model providers". */
export function BackLink(props: { label: string; onPress: () => void; testID?: string }) {
  return (
    <Button
      class={cx("flex-row items-center self-start gap-(--space-1) rounded-(--radius-sm) px-(--space-2) py-[3px] -ml-2 hover:bg-muted active:bg-muted", transitionClass, focusRingClass)}
      onPress={props.onPress}
      accessibilityLabel={`Back to ${props.label}`}
      testID={props.testID}
    >
      <Text class={cx(textClass.sm, "text-muted-foreground")}>← {props.label}</Text>
    </Button>
  );
}

/**
 * The heading of a connection's details: its mark, name and status line,
 * with `children` (its ⋯ menu or main action) at the end.
 */
export function ConnectionHeading(props: { mark: Mark; name: string; isDefault: boolean; status: { tone: Tone; text: string }; children?: JSX.Element; testID?: string }) {
  return (
    <View class="flex-row items-center gap-(--space-3)" testID={props.testID}>
      <BrandMark mark={props.mark} size="md" />
      <View class="flex-1 min-w-0 flex-col gap-px">
        <View class="flex-row items-center gap-(--space-2)">
          <Text class={cx(textClass.lg, "font-semibold text-foreground")} role="header" style={{ numberOfLines: 1 }}>
            {props.name}
          </Text>
          <If when={props.isDefault}>
            <Badge tone="neutral" variant="outline" label="Default" />
          </If>
        </View>
        <StatusLine tone={props.status.tone} text={props.status.text} testID={props.testID ? `${props.testID}:status` : undefined} />
      </View>
      {props.children}
    </View>
  );
}

/**
 * One model of a connection, as a row: its switch (`control`), its name
 * (`name`, an editable field) with the model id muted beneath, its tags, and
 * its actions at the end. Dimmed while switched off.
 */
export function ModelItem(props: {
  control: JSX.Element;
  name: JSX.Element;
  modelId: string;
  tags?: JSX.Element;
  actions?: JSX.Element;
  enabled: boolean;
  testID?: string;
}) {
  return (
    <View class="flex-row items-center gap-(--space-3) px-(--space-4) py-(--space-2)" testID={props.testID}>
      {props.control}
      <View class={cx("flex-1 min-w-0 flex-col gap-[2px]", !props.enabled && "opacity-60")}>
        {props.name}
        <View class="flex-row flex-wrap items-center gap-(--space-2)">
          <Text class={cx(textClass.xs, "font-mono text-muted-foreground")} style={{ numberOfLines: 1 }}>
            {props.modelId}
          </Text>
          {props.tags}
        </View>
      </View>
      <If when={props.actions}>
        <View class="shrink-0 flex-row items-center gap-(--space-1)">{props.actions}</View>
      </If>
    </View>
  );
}

/**
 * A model's display name as plain text with a small pencil: pressing either
 * (or Return or Space on it) starts editing it in place (`onEdit`). The
 * pencil shows on hover and keyboard focus, always on touch screens.
 */
export function EditableName(props: { name: string; accessibilityLabel: string; onEdit: () => void; disabled?: boolean; testID?: string }) {
  return (
    <Button
      class={cx("group self-start max-w-full flex-row items-center gap-(--space-1) rounded-(--radius-sm) -mx-[4px] px-[4px] py-[1px] hover:bg-muted", focusRingClass, transitionClass, disabledClass)}
      onPress={props.onEdit}
      disabled={props.disabled}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
      tooltip={<Text class={tooltipClass}>Rename</Text>}
    >
      <Text class={cx(textClass.sm, "font-medium text-foreground shrink")} style={{ numberOfLines: 1 }}>
        {props.name}
      </Text>
      <View class="opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100 pointer-coarse:opacity-100">
        <Icon name="pencil" color="muted" size={12} />
      </View>
    </Button>
  );
}
