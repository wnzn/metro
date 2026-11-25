// The shared searchable model picker. Popover owns anchoring, outside clicks
// and focus return. NSH 0.6.1 Command lacks a search testID and exposes every
// item as an option, with no expanded state. Use real disclosure buttons
// and keep the input's editing keys (Space, Left, Right) while it is focused.

import { Scroll, Storage, Text, View, batch, cleanup, computed, effect, keymap, reduceMotion, signal } from "nsh";
import type { InputRef, ViewRef } from "nsh";
import { Input, Popover } from "nsh/base";
import { connectedTo, me, sessionGuard } from "../store";
import { BrandMark, Icon } from "./chrome";
import type { Mark } from "./chrome";
import { createPickerMemory, pickerScopeKey } from "./pickerMemory";
import type { PickerGroupMemory, PickerKind } from "./pickerMemory";
import { cx, disabledClass, focusRingClass, popoverClass, popoverItemClass, popoverMotion, textClass, toneTextClass, transitionClass } from "./tokens";

type Provider = { key: string; name: string; mark: Mark };

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
  provider?: Provider;
  /** Right side of the row: the chosen level on the chosen row, else the levels it offers. */
  thinking?: string;
  /** Listed, but can't be chosen (say why in `detail`). */
  disabled?: boolean;
};

type PickerProps = {
  options: readonly ModelOption[];
  value: string | null;
  onChange: (value: string) => void;
  testID?: string;
  accessibilityLabel?: string;
  placeholder?: string;
  attention?: boolean;
  disabled?: boolean;
  rememberAs?: PickerKind;
  /** A changed catalog context closes the menu, but doesn't partition memory. */
  context?: string;
};

const memory = createPickerMemory(Storage);
type MenuState = {
  group: (provider: string) => PickerGroupMemory;
  valid: () => boolean;
};

export function ModelPicker(props: PickerProps) {
  const anonymous = createPickerMemory();
  const open = signal(false);
  const menu = signal<MenuState | null>(null);
  const width = signal<number | undefined>(undefined);
  const available = signal<number | undefined>(undefined);
  const triggerFocused = signal(false);
  let mounted = true;
  cleanup(() => { mounted = false; });

  const current = () => props.options.find((o) => o.value === props.value);
  const placeholder = () => props.placeholder ?? "Choose a model";
  const scope = () => pickerScopeKey(connectedTo(), me(), props.rememberAs ?? "default");
  const show = () => {
    if (props.disabled || open()) return;
    const key = scope(), context = props.context, sameSession = sessionGuard();
    batch(() => {
      available.set(undefined);
      menu.set({
        group: (provider) => (key ? memory : anonymous)(key, provider),
        valid: () => mounted && !props.disabled && scope() === key && props.context === context && sameSession(),
      });
      open.set(true);
    });
  };
  const close = () => open.set(false);
  effect(() => {
    if (open() && !menu()?.valid()) close();
  });
  effect(() => {
    if (!triggerFocused() || open() || props.disabled) return;
    return keymap.bind({ down: show, up: show }, { forFocus: true });
  });

  // No exit transition: in NSH 0.6.1 an exiting Popover still participates
  // in Tab order and can eat the Escape intended for the parent dialog.
  return (
    <Popover
      open={open()}
      onOpenChange={(event) => event.open ? show() : close()}
      align="start"
      offset={4}
      margin={8}
      popupClass={popoverClass}
      popupStyle={{ width: width(), maxHeight: available() }}
      onPosition={(position) => available.set(position.available.height)}
      popupStartingClass={reduceMotion() ? undefined : popoverMotion.starting}
      trigger={
        <View
          class={cx(
            "flex-row items-center gap-(--space-2) min-h-[40px] w-full rounded-(--radius-sm) border border-input bg-background px-(--space-2) hover:bg-muted",
            focusRingClass, transitionClass, disabledClass,
          )}
          role="button"
          disabled={props.disabled}
          focusable={!props.disabled}
          testID={props.testID}
          accessibilityLabel={`${props.accessibilityLabel ?? "Model"}: ${current()?.title ?? placeholder()}`}
          onLayout={({ bounds }) => width.set(bounds.size.width)}
          onFocus={() => triggerFocused.set(true)}
          onBlur={() => triggerFocused.set(false)}
        >
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
                <Text class={cx(textClass.xs, "text-muted-foreground")} style={{ numberOfLines: 1 }}>{line()}</Text>
              )}
            </If>
          </View>
          <Icon name="chevron-down" color="muted" size={14} />
        </View>
      }
    >
      <Each of={menu() ? [menu()!] : []}>
        {(item) => {
          const state = item();
          return <ModelPickerMenu picker={props} state={state} active={() => open() && menu() === state && state.valid()} onClose={close} />;
        }}
      </Each>
    </Popover>
  );
}

type OptionRow = { kind: "option"; id: string; option: ModelOption };
type GroupRow = { kind: "group"; id: string; provider: Provider; expanded: boolean; toggle: boolean; count: number };
type Row = OptionRow | GroupRow;

const SEARCH_LABEL = "Search models or providers";
const rowEnabled = (row: Row) => row.kind === "group" ? row.toggle : !row.option.disabled;
const optionRow = (option: ModelOption): Row => ({ kind: "option", id: `model:${option.value}`, option });
const groupClass = "flex-row items-center gap-(--space-2)";

function ModelPickerMenu(props: {
  picker: PickerProps;
  state: MenuState;
  active: () => boolean;
  onClose: () => void;
}) {
  const query = signal("");
  const needle = () => query().trim().toLowerCase();
  const focused = signal<string | null>(null);
  let searchRef: InputRef | undefined;
  const refs = new Map<string, ViewRef>();
  const picker = props.picker;
  const idOf = (suffix: string) => picker.testID ? `${picker.testID}${suffix}` : undefined;
  const matches = (o: ModelOption, q: string) =>
    [o.title, o.model, o.modelId, o.detail, o.provider?.name].some((v) => v?.toLowerCase().includes(q));
  const grouped = computed(() => {
    const standalone: ModelOption[] = [];
    const groups = new Map<string, { provider: Provider; options: ModelOption[] }>();
    for (const o of picker.options) {
      if (!o.provider) {
        standalone.push(o);
        continue;
      }
      const key = o.provider.key;
      const group = groups.get(key);
      if (group) group.options.push(o);
      else groups.set(key, { provider: o.provider, options: [o] });
    }
    return { standalone, groups };
  });
  const result = computed(() => {
    const { standalone, groups } = grouped();
    const q = needle();
    const rows = standalone.map(optionRow);
    let hasMatch = standalone.some((o) => matches(o, q));
    for (const [key, group] of groups) {
      const options = q ? group.options.filter((o) => matches(o, q)) : group.options;
      if (!options.length) continue;
      hasMatch = true;
      const toggle = !q && groups.size > 1 && !group.options.some((o) => o.value === picker.value);
      const expanded = !toggle || !props.state.group(key).collapsed();
      rows.push({ kind: "group", id: `group:${key}`, provider: group.provider, expanded, toggle, count: options.length });
      if (expanded) rows.push(...options.map(optionRow));
    }
    return { rows, hasMatch };
  });
  const rows = () => result().rows;
  const lookup = (id: string) => rows().find((row) => row.id === id);
  const changeGroup = (id: string, expand?: boolean) => {
    if (!props.active()) return;
    const row = lookup(id);
    if (row?.kind !== "group" || !row.toggle) return;
    props.state.group(row.provider.key).setCollapsed(expand === undefined ? row.expanded : !expand);
  };
  const activate = (id: string) => {
    if (!props.active()) return;
    const row = lookup(id);
    if (!row || !rowEnabled(row)) return;
    if (row.kind === "group") {
      changeGroup(id);
      return;
    }
    props.onClose();
    // Like Select, choosing the current value closes without resetting it
    // (in the composer that would also reset the chosen thinking level).
    if (row.option.value !== picker.value) picker.onChange(row.option.value);
  };
  const acceptSearch = () => {
    const q = needle();
    if (!q) return;
    const first = rows().find((row) => row.kind === "option" && !row.option.disabled && matches(row.option, q));
    if (first) activate(first.id);
  };
  const blur = (id: string) => {
    if (focused() === id) focused.set(null);
  };
  const goTo = (id?: string) => {
    if (!props.active()) return;
    if (!id) {
      searchRef?.focus();
      return;
    }
    const ref = refs.get(id);
    ref?.focus();
    ref?.reveal({ vertical: "nearest", animated: false });
  };
  const move = (by: -1 | 1) => {
    const enabled = rows().filter(rowEnabled);
    const at = enabled.findIndex((r) => r.id === focused());
    goTo(enabled[at === -1 && by === -1 ? enabled.length - 1 : at + by]?.id);
  };
  effect(() => {
    const at = focused();
    if (!at || !props.active()) return;
    const navigating = { when: "always", forFocus: true } as const;
    const unbind = keymap.bind({
      down: () => move(1),
      up: () => move(-1),
      escape: props.onClose,
      ...(at === "search" ? { enter: acceptSearch } : {
        home: () => goTo(rows().find(rowEnabled)?.id),
        end: () => goTo(rows().filter(rowEnabled).at(-1)?.id),
        ...(at.startsWith("group:") ? { left: () => changeGroup(at, false), right: () => changeGroup(at, true) } : {}),
      }),
    }, navigating);
    // Unlike Select's virtual highlighting, these are real focus stops.
    // Let Tab traverse them and dismiss only when it leaves the popover,
    // after the platform has moved focus. Do not pull it back to the trigger.
    const onTab = () => requestAnimationFrame(() => {
      if (props.active() && !focused()) props.onClose();
    });
    const tabOut = keymap.bind({ tab: onTab, "shift+tab": onTab }, { ...navigating, passThrough: true });
    return () => { unbind(); tabOut(); };
  });

  const rowClass = cx("group", popoverItemClass, "selected:bg-muted", focusRingClass);
  return (
    <View class="shrink min-h-0" role="group" accessibilityLabel={picker.accessibilityLabel ?? "Model choices"}>
      <Input
        ref={(node) => { searchRef = node; }}
        autoFocus
        submitBehavior="submit"
        value={query()}
        onChange={({ value }) => query.set(value)}
        onSubmit={acceptSearch}
        onFocus={() => focused.set("search")}
        onBlur={() => blur("search")}
        placeholder={SEARCH_LABEL}
        accessibilityLabel={SEARCH_LABEL}
        testID={idOf("-search")}
        class={cx("shrink-0 mb-[4px] rounded-(--radius-sm) border border-input bg-background px-(--space-2) py-(--space-1) text-foreground", textClass.sm)}
      />
      <Scroll class="shrink min-h-0 max-h-[340px]" keyboardShouldPersistTaps="handled">
        <Each of={rows()} key={(row) => row.id}>
          {(row) => {
            const { id, kind } = row(); // The keyed row keeps its kind and id.
            const focusProps = {
              ref: (node: ViewRef) => refs.set(id, node),
              onFocus: () => focused.set(id),
              onBlur: () => blur(id),
              onPress: () => activate(id),
            };
            cleanup(() => refs.delete(id));
            if (kind === "option") {
              const option = () => (row() as OptionRow).option;
              const selected = () => option().value === picker.value;
              return (
                <View
                  {...focusProps}
                  class={rowClass}
                  role="button"
                  focusable={!option().disabled}
                  disabled={option().disabled}
                  accessibilityLabel={option().title}
                  accessibilityDescription={selected() ? "Current selection" : undefined}
                  accessibilityState={{ selected: selected(), disabled: !!option().disabled }}
                  testID={idOf(`:${option().value}`)}
                >
                  <ModelRow option={option()} />
                </View>
              );
            }
            const group = () => row() as GroupRow;
            const title = () => groupLabel(group().provider.name, group().count);
            const groupID = () => idOf(`-group:${group().provider.name}`);
            return (
              <If when={group().toggle} else={
                <View class={cx(groupClass, "px-(--space-2) pt-(--space-2) pb-(--space-1)")} role="header"
                  accessibilityLabel={title()} testID={groupID()}>
                  <GroupLabel provider={group().provider} count={group().count} />
                </View>
              }>
                <View
                  {...focusProps}
                  class={cx(groupClass, rowClass)}
                  role="button"
                  focusable
                  accessibilityLabel={`${title()}, ${group().expanded ? "expanded" : "collapsed"}`}
                  accessibilityState={{ expanded: group().expanded }}
                  testID={idOf(`-group-toggle:${group().provider.name}`)}
                >
                  <View class={groupClass} testID={groupID()}>
                    <Icon name={group().expanded ? "chevron-down" : "chevron-right"} color="muted" size={14} />
                    <GroupLabel provider={group().provider} count={group().count} />
                  </View>
                </View>
              </If>
            );
          }}
        </Each>
        <If when={!result().hasMatch}>
          <Text class={cx("px-(--space-2) py-(--space-2) text-muted-foreground", textClass.sm)}>No model matches.</Text>
        </If>
      </Scroll>
    </View>
  );
}

const groupLabel = (name: string, count: number) => `${name}, ${count} ${count === 1 ? "model" : "models"}`;

function GroupLabel(props: { provider: Provider; count: number }) {
  return (
    <>
      <BrandMark mark={props.provider.mark} />
      <Text class={cx(textClass.xs, "font-semibold text-muted-foreground")}>
        {`${props.provider.name} · ${props.count}`}
      </Text>
    </>
  );
}

function ModelRow(props: { option: ModelOption }) {
  return (
    <View class="flex-row items-center gap-(--space-2)">
      <View class="flex-1 min-w-0 flex-col gap-px">
        <Text class={cx(textClass.sm, "text-popover-foreground group-selected:font-semibold")} style={{ numberOfLines: 1 }}>
          {props.option.model}
        </Text>
        <If when={props.option.modelId !== props.option.model ? props.option.modelId : null}>
          {(id) => (
            <Text class={cx(textClass.xs, "font-mono text-muted-foreground")} style={{ numberOfLines: 1 }}>{id()}</Text>
          )}
        </If>
        <If when={props.option.detail}>
          {(detail) => (
            <Text class={cx(textClass.xs, "text-muted-foreground")} style={{ numberOfLines: 2 }}>{detail()}</Text>
          )}
        </If>
      </View>
      <If when={props.option.thinking}>
        {(thinking) => (
          <Text class={cx(textClass.xs, "shrink-0 max-w-[45%] text-right text-muted-foreground group-selected:text-foreground")} style={{ numberOfLines: 3 }}>
            {thinking()}
          </Text>
        )}
      </If>
    </View>
  );
}
