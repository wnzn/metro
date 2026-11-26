// The composer's model controls: the closed trigger beside Attach and the
// thinking levels shown as one row of choices. The model list itself is the
// shared grouped picker (./modelPicker). Feature code passes only semantic props; every class lives here.

import { Text } from "nsh";
import { Button, Radio, RadioGroup } from "nsh/base";
import { BrandMark, Icon } from "./chrome";
import type { Mark } from "./chrome";
import { cx, disabledClass, focusRingClass, textClass, transitionClass } from "./tokens";

/**
 * The closed trigger: the provider's mark, the model and the thinking level
 * ("gpt-5 · high", "gpt-5 · Provider default"). Long ids truncate the model,
 * never the level, so Send stays inside a narrow composer.
 */
export function ComposerModelTrigger(props: {
  /** The chosen model, or null for Default (normal routing). */
  model: string | null;
  /** The thinking level as it will be sent ("Provider default" when none is). */
  thinking?: string | null;
  mark?: Mark | null;
  accessibilityLabel: string;
  onPress: () => void;
}) {
  return (
    <Button class={cx("flex-row max-w-[260px] h-[28px] items-center gap-(--space-1) rounded-(--radius-sm) px-(--space-2) hover:bg-muted", focusRingClass, transitionClass)}
      testID="composer-model" accessibilityLabel={props.accessibilityLabel} onPress={props.onPress}>
      <If when={props.mark}>{m => <BrandMark mark={m()} />}</If>
      <Text class={cx("shrink text-(length:--text-xs)", props.model ? "text-foreground" : "text-muted-foreground")} style={{ numberOfLines: 1 }}
        testID="composer-model-name">{props.model ?? "Default model"}</Text>
      <If when={props.thinking}>
        {t => <Text class="shrink-0 text-(length:--text-xs) text-muted-foreground" style={{ numberOfLines: 1 }} testID="composer-model-thinking">· {t()}</Text>}
      </If>
      <Icon name="chevron-down" size={14} color="muted" testID="composer-model-chevron" />
    </Button>
  );
}

// The grouped model list is the shared picker (./modelPicker); these names stay for the composer.
export { ModelPicker as ModelOptionPicker } from "./modelPicker";
export type { ModelOption } from "./modelPicker";

/**
 * The thinking levels the chosen model advertises, all visible at once and
 * wrapping when there are many: a radio group (arrows move, Space checks).
 * Option testIDs are `${testID}:${value}`; "" is Provider default.
 */
export function ThinkingChoice(props: {
  options: readonly { value: string; label: string }[];
  value: string;
  onChange: (value: string) => void;
  testID?: string;
}) {
  return (
    <RadioGroup class="flex-row flex-wrap self-start gap-[4px]" accessibilityLabel="Thinking level" testID={props.testID}>
      <Each of={props.options} key={o => o.value}>
        {o => (
          <Radio
            class={cx(
              "group flex-row items-center justify-center min-h-[28px] rounded-(--radius-sm) border border-border px-(--space-3) checked:bg-primary checked:border-primary",
              props.value !== o().value && "hover:bg-muted",
              focusRingClass, transitionClass, disabledClass,
            )}
            checked={props.value === o().value}
            onChange={() => props.onChange(o().value)}
            accessibilityLabel={`Thinking: ${o().label}`}
            testID={props.testID ? `${props.testID}:${o().value}` : undefined}
          >
            <Text class={cx(textClass.sm, "font-medium text-muted-foreground group-checked:text-primary-foreground", props.value !== o().value && "group-hover:text-foreground")}>
              {o().label}
            </Text>
          </Radio>
        )}
      </Each>
    </RadioGroup>
  );
}
