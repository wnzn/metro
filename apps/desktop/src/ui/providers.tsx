// Model providers primitives: the add-connection picker's frame, a
// provider's heading, one way to connect as a single pressable row, and the
// one-line summary of per-capability defaults. Like ./primitives, feature code
// passes only semantic props; every class lives here.

import { Text, View } from "nsh";
import type { JSX } from "nsh";
import { Button } from "nsh/base";
import { BrandMark } from "./chrome";
import type { Mark } from "./chrome";
import { Badge } from "./primitives";
import { cx, focusRingClass, textClass, transitionClass } from "./tokens";
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
