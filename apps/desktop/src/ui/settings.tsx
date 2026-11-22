// Settings form primitives: a percentage field and short fact rows. Feature
// code passes only semantic props; the styling lives here, like
// ./primitives.tsx.

import { Text, View } from "nsh";
import { Input } from "nsh/base";
import { compactWidth } from "./chrome";
import { cx } from "./tokens";

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
