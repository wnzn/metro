// The one model picker: every place a model is chosen (the composer, Model
// routing) lists models the same way. Grouped by provider under its mark and
// name; each option shows the model's display name with its id muted beneath,
// then whose connection it is. Searchable and keyboard driven (arrows,
// typeahead, Return). Feature code passes only semantic props; every class
// lives here.

import { Text, View, reduceMotion } from "nsh";
import { Select } from "nsh/base";
import { BrandMark, Icon } from "./chrome";
import type { Mark } from "./chrome";
import { cx, disabledClass, focusRingClass, popoverClass, popoverItemClass, popoverMotion, textClass, toneTextClass, transitionClass } from "./tokens";

/** One choice. Without `provider` it is listed above every group (Default, None). */
export type ModelOption = {
  value: string;
  /** The model as the row and the closed trigger show it: its display name. */
  model: string;
  /** The model's id, muted under the name; left out when it equals the name. */
  modelId?: string;
  /** A muted line: the connection, whose it is, who can see the requests. */
  detail?: string;
  /** What search and screen readers get: name, id, connection, provider and levels. */
  title: string;
  provider?: { name: string; mark: Mark };
  /** Right side of the row: the chosen level on the chosen row, else the levels it offers. */
  thinking?: string;
  /** Listed, but can't be chosen (say why in `detail`). */
  disabled?: boolean;
};

/**
 * The grouped picker. Row testIDs are `${testID}:${value}`; group headers
 * `${testID}-group:${provider name}`; the search field `${testID}-search`.
 * With nothing chosen the trigger says `placeholder` ("Choose a model"), in
 * the warning ink when `attention` (a connection with no model at all).
 */
export function ModelPicker(props: {
  options: readonly ModelOption[];
  value: string | null;
  onChange: (value: string) => void;
  testID?: string;
  accessibilityLabel?: string;
  placeholder?: string;
  attention?: boolean;
  disabled?: boolean;
}) {
  const byValue = () => new Map(props.options.map((o) => [o.value, o]));
  const marks = () => new Map(props.options.flatMap((o) => (o.provider ? [[o.provider.name, o.provider.mark] as const] : [])));
  const current = () => (props.value === null ? undefined : byValue().get(props.value));
  const placeholder = () => props.placeholder ?? "Choose a model";
  return (
    <Select
      class={cx(
        "flex-row items-center gap-(--space-2) min-h-[40px] w-full rounded-(--radius-sm) border border-input bg-background px-(--space-2) hover:bg-muted",
        focusRingClass,
        transitionClass,
        disabledClass,
      )}
      items={props.options.map((o) => ({ value: o.value, title: o.title, group: o.provider?.name ?? null, disabled: o.disabled }))}
      value={props.value}
      onChange={({ value }) => {
        if (value !== null) props.onChange(value);
      }}
      placeholder={placeholder()}
      disabled={props.disabled}
      searchable
      searchPlaceholder="Search models or providers"
      searchAccessibilityLabel="Search models or providers"
      searchTestID={props.testID ? `${props.testID}-search` : undefined}
      accessibilityLabel={props.accessibilityLabel ?? "Model"}
      popupClass={popoverClass}
      popupStartingClass={reduceMotion() ? undefined : popoverMotion.starting}
      popupEndingClass={reduceMotion() ? undefined : popoverMotion.ending}
      listClass="max-h-[340px]"
      itemClass={cx("group", popoverItemClass, "selected:bg-muted", disabledClass)}
      groupClass="px-(--space-2) pt-(--space-2) pb-(--space-1)"
      searchClass={cx("mb-[4px] rounded-(--radius-sm) border border-input bg-background px-(--space-2) py-(--space-1) text-foreground", textClass.sm)}
      empty={<Text class={cx("px-(--space-2) py-(--space-2) text-muted-foreground", textClass.sm)}>No model matches.</Text>}
      testID={props.testID}
      renderGroup={(g) => (
        <View class="flex-row items-center gap-(--space-2)" testID={props.testID ? `${props.testID}-group:${g.title}` : undefined}>
          <If when={marks().get(g.title)}>{(m) => <BrandMark mark={m()} />}</If>
          <Text class={cx(textClass.xs, "font-semibold text-muted-foreground")} role="header">
            {g.title}
          </Text>
        </View>
      )}
      renderItem={(entry) => {
        const o = byValue().get(entry.value);
        return (
          <View class="flex-row items-center gap-(--space-2)" testID={props.testID ? `${props.testID}:${entry.value}` : undefined}>
            <View class="flex-1 min-w-0 flex-col gap-px">
              <Text class={cx(textClass.sm, "text-popover-foreground group-selected:font-semibold")} style={{ numberOfLines: 1 }}>
                {o?.model ?? entry.title}
              </Text>
              <If when={o?.modelId && o.modelId !== o.model ? o.modelId : null}>
                {(id) => (
                  <Text class={cx(textClass.xs, "font-mono text-muted-foreground")} style={{ numberOfLines: 1 }}>
                    {id()}
                  </Text>
                )}
              </If>
              <If when={o?.detail}>
                {(d) => (
                  <Text class={cx(textClass.xs, "text-muted-foreground")} style={{ numberOfLines: 2 }}>
                    {d()}
                  </Text>
                )}
              </If>
            </View>
            <If when={o?.thinking}>
              {(t) => (
                <Text class={cx(textClass.xs, "shrink-0 max-w-[45%] text-right text-muted-foreground group-selected:text-foreground")} style={{ numberOfLines: 3 }}>
                  {t()}
                </Text>
              )}
            </If>
          </View>
        );
      }}
      trigger={
        <View class="flex-1 flex-row items-center gap-(--space-2)">
          <If when={current()?.provider} else={<Icon name="providers" color="muted" />}>
            {(p) => <BrandMark mark={p().mark} />}
          </If>
          <View class="flex-1 min-w-0 flex-col">
            <Text
              class={cx(textClass.sm, current() ? "font-medium text-foreground" : props.attention ? toneTextClass.warning : "text-muted-foreground")}
              style={{ numberOfLines: 1 }}
              testID={props.testID ? `${props.testID}-value` : undefined}
            >
              {current()?.model ?? placeholder()}
            </Text>
            <If when={current() ? current()!.provider?.name ?? current()!.detail : null}>
              {(line) => (
                <Text class={cx(textClass.xs, "text-muted-foreground")} style={{ numberOfLines: 1 }}>
                  {line()}
                </Text>
              )}
            </If>
          </View>
          <Icon name="chevron-down" color="muted" size={14} />
        </View>
      }
    />
  );
}
