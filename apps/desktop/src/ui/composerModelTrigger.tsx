// The composer's model controls: the closed trigger beside Attach, the model
// list grouped by provider, and the thinking levels shown as one row of
// choices. Feature code passes only semantic props; every class lives here.

import { Text, View, reduceMotion } from "nsh";
import { Button, Radio, RadioGroup, Select } from "nsh/base";
import { BrandMark, Icon } from "./chrome";
import type { Mark } from "./chrome";
import { cx, disabledClass, focusRingClass, popoverClass, popoverItemClass, popoverMotion, textClass, transitionClass } from "./tokens";

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
      <Icon name="chevron-down" size={14} color="muted" />
    </Button>
  );
}

/** One choice in the model list. Without `provider` it is listed above every group (Default). */
export type ModelOption = {
  value: string;
  /** The model as the row shows it. */
  model: string;
  /** Second line: the connection and who can see the requests. */
  detail?: string;
  /** What search and screen readers get: model, connection, provider and levels. */
  title: string;
  provider?: { name: string; mark: Mark };
  /** Right side of the row: the chosen level on the chosen row, else the levels it offers. */
  thinking?: string;
};

/**
 * The model list, grouped by provider under its mark and name. Searchable and
 * keyboard driven (arrows, typeahead, Return); each row says its thinking
 * levels so they read without opening anything else (wrapping, never cut, in a narrow window). Row testIDs are
 * `${testID}:${value}`.
 */
export function ModelOptionPicker(props: {
  options: readonly ModelOption[];
  value: string | null;
  onChange: (value: string) => void;
  testID?: string;
}) {
  const byValue = () => new Map(props.options.map(o => [o.value, o]));
  const marks = () => new Map(props.options.flatMap(o => o.provider ? [[o.provider.name, o.provider.mark] as const] : []));
  const current = () => (props.value === null ? undefined : byValue().get(props.value));
  return (
    <Select
      class={cx("flex-row items-center gap-(--space-2) min-h-[40px] w-full rounded-(--radius-sm) border border-input bg-background px-(--space-2) hover:bg-muted", focusRingClass)}
      items={props.options.map(o => ({ value: o.value, title: o.title, group: o.provider?.name ?? null }))}
      value={props.value}
      onChange={({ value }) => { if (value !== null) props.onChange(value); }}
      placeholder="Choose a model"
      searchable
      searchPlaceholder="Search models or providers"
      searchAccessibilityLabel="Search models or providers"
      searchTestID={props.testID ? `${props.testID}-search` : undefined}
      accessibilityLabel="Model"
      popupClass={popoverClass}
      popupStartingClass={reduceMotion() ? undefined : popoverMotion.starting}
      popupEndingClass={reduceMotion() ? undefined : popoverMotion.ending}
      listClass="max-h-[340px]"
      itemClass={cx("group", popoverItemClass, "selected:bg-muted")}
      groupClass="px-(--space-2) pt-(--space-2) pb-(--space-1)"
      searchClass={cx("mb-[4px] rounded-(--radius-sm) border border-input bg-background px-(--space-2) py-(--space-1) text-foreground", textClass.sm)}
      empty={<Text class={cx("px-(--space-2) py-(--space-2) text-muted-foreground", textClass.sm)}>No model matches.</Text>}
      testID={props.testID}
      renderGroup={g => (
        <View class="flex-row items-center gap-(--space-2)" testID={props.testID ? `${props.testID}-group:${g.title}` : undefined}>
          <If when={marks().get(g.title)}>{m => <BrandMark mark={m()} />}</If>
          <Text class={cx(textClass.xs, "font-semibold text-muted-foreground")} role="header">{g.title}</Text>
        </View>
      )}
      renderItem={entry => {
        const o = byValue().get(entry.value);
        return (
          <View class="flex-row items-center gap-(--space-2)" testID={props.testID ? `${props.testID}:${entry.value}` : undefined}>
            <View class="flex-1 min-w-0 flex-col gap-px">
              <Text class={cx(textClass.sm, "text-popover-foreground group-selected:font-semibold")} style={{ numberOfLines: 1 }}>{o?.model ?? entry.title}</Text>
              <If when={o?.detail}>{d => <Text class={cx(textClass.xs, "text-muted-foreground")} style={{ numberOfLines: 2 }}>{d()}</Text>}</If>
            </View>
            <If when={o?.thinking}>{t => <Text class={cx(textClass.xs, "shrink-0 max-w-[45%] text-right text-muted-foreground group-selected:text-foreground")} style={{ numberOfLines: 3 }}>{t()}</Text>}</If>
          </View>
        );
      }}
      trigger={
        <View class="flex-1 flex-row items-center gap-(--space-2)">
          <If when={current()?.provider} else={<Icon name="providers" color="muted" />}>{p => <BrandMark mark={p().mark} />}</If>
          <View class="flex-1 min-w-0 flex-col">
            <Text class={cx(textClass.sm, current() ? "font-medium text-foreground" : "text-muted-foreground")} style={{ numberOfLines: 1 }}>
              {current()?.model ?? "Choose a model"}
            </Text>
            <If when={current()?.provider}>{p => <Text class={cx(textClass.xs, "text-muted-foreground")} style={{ numberOfLines: 1 }}>{p().name}</Text>}</If>
          </View>
          <Icon name="chevron-down" color="muted" size={14} />
        </View>
      }
    />
  );
}

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
