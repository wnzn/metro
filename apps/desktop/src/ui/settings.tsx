// Settings form primitives: a percentage field, short fact rows, and a
// "Learn more" disclosure for the explanation behind a one-sentence header. Feature
// code passes only semantic props; the styling lives here, like
// ./primitives.tsx.

import { signal, Text, View } from "nsh";
import type { JSX } from "nsh";
import { Input, Progress } from "nsh/base";
import { compactWidth, IconButton } from "./chrome";
import { Disclosure } from "./primitives";
import { cx, textClass } from "./tokens";

type Testable = { testID?: string; accessibilityLabel?: string };

const focusRing = "focus-visible:ring-2 focus-visible:ring-ring";

/**
 * A whole-number percentage, 0 to 100, typed beside a "%" sign. `value` and
 * `onChange` are the text as typed (digits only); the caller converts. An
 * `error` shows under it and marks the field invalid for screen readers.
 */
export function PercentField(
  props: Testable & {
    value: string;
    onChange: (value: string) => void;
    accessibilityLabel: string;
    error?: string | null;
    disabled?: boolean;
    placeholder?: string;
  },
) {
  return (
    <View class="flex-col gap-(--space-1)">
      <View class="flex-row items-center gap-(--space-2)">
        <Input
          class={cx(
            "w-[88px] rounded-(--radius-sm) border bg-background px-(--space-3) py-(--space-2) text-(length:--text-md) text-foreground placeholder:text-muted-foreground focus:ring-1 disabled:opacity-60",
            props.error ? "border-destructive focus:border-destructive focus:ring-destructive" : "border-input focus:border-ring focus:ring-ring",
            focusRing,
          )}
          value={props.value}
          onChange={({ value }) => props.onChange(value)}
          placeholder={props.placeholder}
          keyboardType="number-pad"
          maxLength={4}
          disabled={props.disabled}
          accessibilityLabel={`${props.accessibilityLabel}, percent`}
          accessibilityState={{ invalid: !!props.error }}
          accessibilityDescription={props.error ?? undefined}
          testID={props.testID}
        />
        <Text class="text-(length:--text-md) text-muted-foreground" accessibilityElementsHidden>
          %
        </Text>
      </View>
      <If when={props.error}>
        {(e) => (
          <Text class="text-(length:--text-xs) text-destructive" testID={props.testID ? `${props.testID}-error` : undefined}>
            {e()}
          </Text>
        )}
      </If>
    </View>
  );
}

export type Fact = { label: string; value: string };

/**
 * Short facts in label–value rows ("Sends", "To", "When"), each value a
 * sentence or a name. Narrow windows stack the label over its value.
 */
export function FactList(props: Testable & { facts: readonly Fact[] }) {
  return (
    <View class="flex-col gap-(--space-1)" testID={props.testID} accessibilityLabel={props.accessibilityLabel}>
      <Each of={props.facts.map((fact, i) => ({ ...fact, i }))} key={(f) => String(f.i)}>
        {(f) => (
          <View class={cx("gap-x-(--space-3)", compactWidth() ? "flex-col" : "flex-row items-start")}>
            <Text class="w-[96px] shrink-0 text-(length:--text-xs) font-medium text-muted-foreground">{f().label}</Text>
            <Text class="flex-1 min-w-0 text-(length:--text-xs) text-card-foreground">{f().value}</Text>
          </View>
        )}
      </Each>
    </View>
  );
}

/**
 * The longer explanation of a page or section, closed under its one-sentence
 * header. Only for background: never put a consent, a cost, an off-by-default
 * or who-sees-what fact only in here. Its toggle's testID is `${testID}:toggle`.
 */
export function LearnMore(props: { testID: string; children?: JSX.Element }) {
  const open = signal(false);
  return (
    <Disclosure testID={props.testID} title="Learn more" open={open()} onToggle={open.set}>
      {props.children}
    </Disclosure>
  );
}

/** Progress with a readable status line. Omit `value` when the total is unknown. */
export function ProgressLine(props: Testable & { label: string; value?: number }) {
  const value = () => props.value !== undefined && Number.isFinite(props.value) ? Math.min(100, Math.max(0, Math.round(props.value))) : undefined;
  return (
    <Progress
      class="min-w-0 flex-col gap-(--space-1)"
      trackClass="h-[6px] overflow-hidden rounded-full bg-muted"
      indicatorClass={cx("h-full rounded-full bg-primary", value() === undefined && "w-0")}
      value={value()}
      indeterminate={value() === undefined}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
      testID={props.testID}
    >
      <Text class={cx(textClass.xs, "tabular-nums text-muted-foreground")} accessibilityElementsHidden>
        {value() === undefined ? props.label : `${props.label} ${value()}%`}
      </Text>
    </Progress>
  );
}

/** Move an item one step at a time. The caller owns its order and list boundaries. */
export function ReorderButtons(props: {
  label: string;
  onMove: (step: -1 | 1) => void;
  canMoveUp: boolean;
  canMoveDown: boolean;
  disabled?: boolean;
  testID?: string;
  upTestID?: string;
  downTestID?: string;
}) {
  return (
    <View class="shrink-0 flex-row items-center gap-(--space-1)" testID={props.testID}>
      <IconButton icon="chevron-up" accessibilityLabel={`Move ${props.label} up`} tooltip="Move up"
        testID={props.upTestID ?? (props.testID ? `${props.testID}:up` : undefined)}
        disabled={props.disabled || !props.canMoveUp} onPress={() => props.onMove(-1)} />
      <IconButton icon="chevron-down" accessibilityLabel={`Move ${props.label} down`} tooltip="Move down"
        testID={props.downTestID ?? (props.testID ? `${props.testID}:down` : undefined)}
        disabled={props.disabled || !props.canMoveDown} onPress={() => props.onMove(1)} />
    </View>
  );
}
