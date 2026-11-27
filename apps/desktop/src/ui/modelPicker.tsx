// The shared searchable model picker. The field takes the trigger's place
// in the parent focus order. Rows float in an overlay, but keyboard navigation
// highlights them without moving native focus into GTK's overlay focus order.

import { Overlay, Scroll, Storage, Text, View, announce, batch, cleanup, computed, effect, keymap, platform, screenReaderEnabled, signal } from "nsh";
import type { ViewRef } from "nsh";
import { Input } from "nsh/base";
import { connectedTo, me, sessionGuard } from "../store";
import { BrandMark, Icon } from "./chrome";
import type { Mark } from "./chrome";
import { createPickerMemory, pickerScopeKey } from "./pickerMemory";
import type { PickerGroupMemory, PickerKind } from "./pickerMemory";
import { cx, focusRingClass, pickerInputInkClass, popoverClass, popoverItemClass, popoverLabelClass, textClass, toneTextClass, transitionClass } from "./tokens";

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
  const anchor = signal<ViewRef | null>(null);
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
      menu.set({
        group: (provider) => (key ? memory : anonymous)(key, provider),
        valid: () => mounted && !props.disabled && scope() === key && props.context === context && sameSession(),
      });
      open.set(true);
    });
  };
  const close = (restoreFocus: boolean) => {
    open.set(false);
    if (restoreFocus && !props.disabled) anchor()?.focus();
  };
  effect(() => {
    if (open() && !menu()?.valid()) close(true);
  });
  effect(() => {
    if (!triggerFocused() || open() || props.disabled) return;
    return keymap.bind({ down: show, up: show }, { forFocus: true });
  });

  return (
    <View
      ref={anchor.set}
      class={open() ? "w-full" : cx(
        "flex-row items-center gap-(--space-2) min-h-[40px] w-full rounded-(--radius-sm) border border-input bg-background px-(--space-2) hover:bg-muted disabled:bg-muted",
        focusRingClass, transitionClass,
      )}
      role={open() ? undefined : "button"}
      disabled={props.disabled}
      focusable={!open() && !props.disabled}
      testID={props.testID}
      accessibilityLabel={open() ? undefined : `${props.accessibilityLabel ?? "Model"}: ${current()?.title ?? placeholder()}`}
      accessibilityState={open() ? undefined : { expanded: false, hasPopup: true }}
      onLayout={({ bounds }) => width.set(bounds.size.width)}
      onFocus={() => triggerFocused.set(true)}
      onBlur={() => triggerFocused.set(false)}
      onPress={open() ? undefined : show}
    >
      <If when={!open()} else={
        <Each of={menu() ? [menu()!] : []}>
          {(item) => {
            const state = item();
            return (
              <ModelPickerMenu
                picker={props}
                state={state}
                anchor={anchor()}
                width={width()}
                active={() => open() && menu() === state && state.valid()}
                onClose={close}
              />
            );
          }}
        </Each>
      }>
        <If when={current()?.provider} else={<Icon name="providers" color="muted" />}>
          {(p) => <BrandMark mark={p().mark} />}
        </If>
        <View class="flex-1 min-w-0 flex-col">
          <Text
            class={cx(textClass.sm, props.disabled ? "text-muted-foreground" : current() ? "font-medium text-foreground" : props.attention ? toneTextClass.warning : "text-muted-foreground")}
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
      </If>
    </View>
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
  anchor: ViewRef | null;
  width?: number;
  active: () => boolean;
  onClose: (restoreFocus: boolean) => void;
}) {
  const query = signal("");
  const needle = () => query().trim().toLowerCase();
  const searchFocused = signal(false);
  const highlighted = signal<string | null>(null);
  const available = signal<number | undefined>(undefined);
  const listRef = signal<ViewRef | null>(null);
  const refs = new Map<string, ViewRef>();
  let tabbingOut = false;
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
  // Default/None remain visible and directly pressable during any search.
  // They are not keyboard results unless the query actually matches them.
  const navigable = computed(() => {
    const q = needle();
    return rows().filter((row) => rowEnabled(row) && (!q || row.kind === "option" && matches(row.option, q)));
  });
  const changeGroup = (id: string, expand?: boolean) => {
    if (!props.active() || tabbingOut) return;
    const row = lookup(id);
    if (row?.kind !== "group" || !row.toggle) return;
    const next = expand ?? !row.expanded;
    props.state.group(row.provider.key).setCollapsed(!next);
    if (highlighted() === id && next !== row.expanded && screenReaderEnabled()) {
      announce(disclosureLabel(row.provider.name, row.count, next));
    }
  };
  const activate = (id: string) => {
    if (!props.active() || tabbingOut) return;
    const row = lookup(id);
    if (!row || !rowEnabled(row)) return;
    if (row.kind === "group") {
      changeGroup(id);
      return;
    }
    props.onClose(true);
    // Like Select, choosing the current value closes without resetting it
    // (in the composer that would also reset the chosen thinking level).
    if (row.option.value !== picker.value) picker.onChange(row.option.value);
  };
  const accept = () => {
    const results = navigable();
    const id = highlighted();
    const row = id !== null ? results.find((r) => r.id === id) : needle() ? results[0] : undefined;
    if (row) activate(row.id);
  };
  const goTo = (row?: Row) => {
    if (!props.active() || tabbingOut) return;
    highlighted.set(row?.id ?? null);
    if (!row) return;
    refs.get(row.id)?.reveal({ vertical: "nearest", animated: false });
    // As in NSH Command, announce the highlight on hosts that do not
    // follow active descendants (NSH 0.6.1 supports them on macOS and web).
    if (screenReaderEnabled() && platform.name !== "macos" && platform.name !== "web") {
      announce(row.kind === "option" ? row.option.title : disclosureLabel(row.provider.name, row.count, row.expanded));
    }
  };
  const move = (by: -1 | 1) => {
    const enabled = navigable();
    const at = enabled.findIndex((r) => r.id === highlighted());
    goTo(enabled[at === -1 && by === -1 ? enabled.length - 1 : at + by]);
  };
  const editQuery = (value: string) => batch(() => {
    highlighted.set(null);
    query.set(value);
  });
  const dismiss = () => { if (props.active() && !tabbingOut) props.onClose(true); };
  effect(() => {
    if (!searchFocused() || !props.active()) return;
    const at = highlighted();
    const navigating = { when: "always", forFocus: true } as const;
    const unbind = keymap.bind({
      down: () => move(1),
      up: () => move(-1),
      escape: dismiss,
      ...(at ? {
        home: () => goTo(navigable()[0]),
        end: () => goTo(navigable().at(-1)),
        space: accept,
      } : {}),
      ...(at?.startsWith("group:") ? {
        left: () => changeGroup(at, false),
        right: () => changeGroup(at, true),
      } : {}),
    }, navigating);
    // Let native traversal start at the inline field. Keep it mounted until
    // the key has advanced focus, then close without stealing that focus.
    const onTab = () => {
      if (tabbingOut) return;
      tabbingOut = true;
      requestAnimationFrame(() => { if (props.active()) props.onClose(false); });
    };
    const tabOut = keymap.bind({ tab: onTab, "shift+tab": onTab }, { ...navigating, passThrough: true });
    return () => { unbind(); tabOut(); };
  });

  const activeRow = () => {
    const id = highlighted();
    return id && navigable().some((row) => row.id === id) ? refs.get(id) : undefined;
  };
  return (
    <>
      <Input
        autoFocus
        submitBehavior="submit"
        value={query()}
        onChange={({ value }) => editQuery(value)}
        onSubmit={accept}
        onFocus={() => searchFocused.set(true)}
        onBlur={() => searchFocused.set(false)}
        placeholder={SEARCH_LABEL}
        accessibilityLabel={SEARCH_LABEL}
        accessibilityActiveDescendant={activeRow()}
        accessibilityControls={listRef()}
        accessibilityState={{ expanded: true, hasPopup: true }}
        testID={idOf("-search")}
        class={cx("min-h-[40px] w-full rounded-(--radius-sm) border border-input bg-background px-(--space-2)", pickerInputInkClass, textClass.sm, focusRingClass)}
      />
      <Overlay
        anchor={props.anchor}
        align="start"
        offset={4}
        margin={8}
        class={popoverClass}
        style={{ width: props.width, maxHeight: available() }}
        onPosition={(position) => available.set(position.available.height)}
        onPressOutside={() => { if (props.active()) props.onClose(false); }}
        onCancel={dismiss}
      >
        <Scroll
          ref={listRef.set}
          class="shrink min-h-0 max-h-[340px]"
          keyboardShouldPersistTaps="handled"
          role="group"
          accessibilityLabel={picker.accessibilityLabel ?? "Model choices"}
        >
          <Each of={rows()} key={(row) => row.id}>
            {(row) => {
              const { id } = row();
              cleanup(() => refs.delete(id));
              return (
                <PickerRow
                  row={row()}
                  selected={picker.value}
                  highlighted={highlighted() === id}
                  idOf={idOf}
                  rowRef={(node) => { refs.set(id, node); }}
                  onPress={() => activate(id)}
                />
              );
            }}
          </Each>
          <If when={!result().hasMatch}>
            <Text class={cx("px-(--space-2) py-(--space-2) text-muted-foreground", textClass.sm)}>No model matches.</Text>
          </If>
        </Scroll>
      </Overlay>
    </>
  );
}

const groupLabel = (name: string, count: number) => `${name}, ${count} ${count === 1 ? "model" : "models"}`;
const disclosureLabel = (name: string, count: number, expanded: boolean) =>
  `${groupLabel(name, count)}, ${expanded ? "expanded" : "collapsed"}`;

function PickerRow(props: {
  row: Row;
  selected: string | null;
  highlighted: boolean;
  idOf: (suffix: string) => string | undefined;
  rowRef: (node: ViewRef) => void;
  onPress: () => void;
}) {
  // Rows are reachable by arrows, pointer, and assistive technology, but
  // are not extra stops in the native Tab order.
  const rowClass = () => cx(popoverItemClass, props.highlighted ? "bg-accent" : "selected:bg-muted");
  if (props.row.kind === "option") {
    const option = () => (props.row as OptionRow).option;
    const selected = () => option().value === props.selected;
    return (
      <View
        ref={props.rowRef}
        class={rowClass()}
        role="button"
        focusable={false}
        onPress={props.onPress}
        disabled={option().disabled}
        accessibilityLabel={option().title}
        accessibilityDescription={selected() ? "Current selection" : undefined}
        accessibilityState={{ selected: selected(), disabled: !!option().disabled }}
        testID={props.idOf(`:${option().value}`)}
      >
        <ModelRow option={option()} />
      </View>
    );
  }
  const group = () => props.row as GroupRow;
  const groupID = () => props.idOf(`-group:${group().provider.name}`);
  return (
    <If when={group().toggle} else={
      <View
        class={cx(groupClass, "px-(--space-2) pt-(--space-2) pb-(--space-1)")}
        role="header"
        accessibilityLabel={groupLabel(group().provider.name, group().count)}
        testID={groupID()}
      >
        <GroupLabel provider={group().provider} count={group().count} />
      </View>
    }>
      <View
        ref={props.rowRef}
        class={cx(groupClass, rowClass())}
        role="button"
        focusable={false}
        onPress={props.onPress}
        accessibilityLabel={disclosureLabel(group().provider.name, group().count, group().expanded)}
        accessibilityState={{ expanded: group().expanded }}
        testID={props.idOf(`-group-toggle:${group().provider.name}`)}
      >
        <View class={groupClass} testID={groupID()}>
          <Icon name={group().expanded ? "chevron-down" : "chevron-right"} color="muted" size={14} />
          <GroupLabel provider={group().provider} count={group().count} />
        </View>
      </View>
    </If>
  );
}

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
        <Text class={cx(textClass.sm, popoverLabelClass, "group-selected:font-semibold")} style={{ numberOfLines: 1 }}>
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
          <Text class={cx(textClass.xs, "shrink-0 max-w-[45%] text-right text-muted-foreground", !props.option.disabled && "group-selected:text-foreground")} style={{ numberOfLines: 3 }}>
            {thinking()}
          </Text>
        )}
      </If>
    </View>
  );
}
