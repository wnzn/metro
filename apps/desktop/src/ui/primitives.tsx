// The app's styled primitives. Feature code composes these and passes only
// semantic props (variant, tone, space); all styling decisions live here and
// in theme/theme.json. To swap the design system, replace this module while
// keeping its exports' props.

import { List, Scroll, Text, View } from "nsh";
import type { JSX, ListRef } from "nsh";
import { Button, Input, Radio, RadioGroup, Select, Switch } from "nsh/base";
import { Icon } from "./chrome";
import type { IconName } from "./chrome";
import { cx, gapClass, padClass, padXClass, padYClass, toneSolidClass, toneSurfaceClass, toneTextClass } from "./tokens";
import type { Space, Tone } from "./tokens";

type Children = { children?: JSX.Element };
type Testable = { testID?: string; accessibilityLabel?: string };

// ---------------------------------------------------------------- layout

type StackProps = Children &
  Testable & {
    gap?: Space;
    pad?: Space;
    padX?: Space;
    padY?: Space;
    grow?: boolean;
    align?: "start" | "center" | "end" | "stretch";
    justify?: "start" | "center" | "end" | "between";
    wrap?: boolean;
    /** Lets the user select and copy the texts inside. */
    selectable?: boolean;
  };

const alignClass = { start: "items-start", center: "items-center", end: "items-end", stretch: "items-stretch" };
const justifyClass = { start: "justify-start", center: "justify-center", end: "justify-end", between: "justify-between" };

function stackClass(props: StackProps, direction: "row" | "col") {
  return cx(
    direction === "row" ? "flex-row" : "flex-col",
    props.grow && "flex-1 min-w-0 min-h-0",
    props.wrap && "flex-wrap",
    props.align && alignClass[props.align],
    props.justify && justifyClass[props.justify],
    gapClass[props.gap ?? 0],
    padClass[props.pad ?? 0],
    padXClass[props.padX ?? 0],
    padYClass[props.padY ?? 0],
  );
}

export function Row(props: StackProps) {
  return (
    <View class={stackClass(props, "row")} selectable={props.selectable} testID={props.testID} accessibilityLabel={props.accessibilityLabel}>
      {props.children}
    </View>
  );
}

export function Column(props: StackProps) {
  return (
    <View class={stackClass(props, "col")} selectable={props.selectable} testID={props.testID} accessibilityLabel={props.accessibilityLabel}>
      {props.children}
    </View>
  );
}

/** Takes the remaining space along its parent's axis. */
export function Spacer() {
  return <View class="flex-1" />;
}

// ---------------------------------------------------------------- surfaces

export type SurfaceVariant =
  | "app"
  | "sidebar"
  | "panel"
  | "card"
  | "dialog"
  | "toolbar"
  | "footer"
  | "aside"
  | "section"
  | "scroll"
  /** A centred, width-limited column for settings and forms inside a panel. */
  | "content"
  /** The bar across the top of the main panel, its content centred (the search box). */
  | "topbar";

const surfaceClass: Record<SurfaceVariant, string> = {
  app: "flex-1 flex-row bg-background",
  sidebar: "w-(--sidebar-width) bg-sidebar border-r border-sidebar-border",
  panel: "flex-1 min-w-0 bg-background",
  card: "bg-card border border-border rounded-(--radius)",
  dialog: "w-full max-w-[460px] bg-card border border-border rounded-(--radius-lg) shadow-lg",
  toolbar: "flex-row items-center min-h-[52px] border-b border-border bg-background",
  footer: "border-t border-border bg-background",
  aside: "w-[340px] border-l border-border bg-sidebar",
  section: "border-b border-border",
  scroll: "flex-1 min-h-0",
  content: "w-full max-w-(--content-width) self-center",
  topbar: "flex-row items-center justify-center min-h-[52px] px-(--space-4) py-(--space-2) border-b border-border bg-background",
};

export function Surface(props: StackProps & { variant: SurfaceVariant }) {
  return (
    <View
      class={cx(surfaceClass[props.variant], stackClass({ ...props, grow: false }, props.variant === "toolbar" || props.variant === "app" ? "row" : "col"), props.grow && "flex-1 min-h-0")}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
    >
      {props.children}
    </View>
  );
}

export function Divider() {
  return <View class="h-px bg-border" />;
}

// ---------------------------------------------------------------- text

export type LabelVariant = "display" | "title" | "heading" | "body" | "strong" | "caption" | "mono";
export type LabelColor = "default" | "muted" | "danger" | "accent" | "sidebar" | "sidebar-active" | Tone;

const labelVariantClass: Record<LabelVariant, string> = {
  display: "text-(length:--text-xl) font-semibold tracking-tight",
  title: "text-(length:--text-lg) font-semibold",
  heading: "text-(length:--text-md) font-semibold",
  body: "text-(length:--text-md)",
  strong: "text-(length:--text-sm) font-semibold",
  caption: "text-(length:--text-xs)",
  mono: "text-(length:--text-sm) font-mono",
};

const labelColorClass: Record<string, string> = {
  default: "text-foreground",
  muted: "text-muted-foreground",
  danger: "text-destructive",
  accent: "text-accent-foreground",
  sidebar: "text-sidebar-foreground",
  "sidebar-active": "text-sidebar-accent-foreground",
};

function colorClass(color: LabelColor) {
  return Object.prototype.hasOwnProperty.call(labelColorClass, color) ? labelColorClass[color] : toneTextClass[color as Tone];
}

export function Label(
  props: Testable & {
    variant?: LabelVariant;
    color?: LabelColor;
    lines?: number;
    /** Screen readers treat it as a heading (page and section titles). */
    heading?: boolean;
    children?: JSX.Element;
  },
) {
  return (
    <Text
      class={cx("shrink min-w-0", labelVariantClass[props.variant ?? "body"], colorClass(props.color ?? "default"))}
      style={props.lines ? { numberOfLines: props.lines } : undefined}
      role={props.heading ? "header" : undefined}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
    >
      {props.children}
    </Text>
  );
}

// ---------------------------------------------------------------- controls

/** The keyboard focus ring every interactive primitive shares. */
const focusRing = "focus-visible:ring-2 focus-visible:ring-ring";

export type ActionVariant = "primary" | "secondary" | "ghost" | "danger";

const actionClass: Record<ActionVariant, string> = {
  primary: "bg-primary active:bg-primary/85 hover:bg-primary/90 disabled:opacity-50",
  secondary: "bg-secondary border border-input active:bg-secondary/70 hover:bg-muted disabled:opacity-50",
  ghost: "active:bg-muted hover:bg-muted disabled:opacity-50",
  danger: "bg-destructive active:bg-destructive/85 hover:bg-destructive/90 disabled:opacity-50",
};

const actionTextClass: Record<ActionVariant, string> = {
  primary: "text-primary-foreground",
  secondary: "text-secondary-foreground",
  ghost: "text-foreground",
  danger: "text-destructive-foreground",
};

export function Action(
  props: Testable & {
    label: string;
    onPress: () => void;
    variant?: ActionVariant;
    disabled?: boolean;
    busy?: boolean;
    busyLabel?: string;
    /** A disclosure: shows a caret (▸ closed, ▾ open) before the label. Say the state in `accessibilityLabel` too. */
    expanded?: boolean;
  },
) {
  const variant = () => props.variant ?? "secondary";
  return (
    <Button
      class={cx(
        "flex-row items-center justify-center min-h-[32px] rounded-(--radius-sm) px-(--space-3) py-(--space-2)",
        props.expanded !== undefined && "justify-start gap-(--space-2)",
        focusRing,
        actionClass[variant()],
      )}
      disabled={props.disabled || props.busy}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
    >
      <If when={props.expanded !== undefined}>
        <Text class={cx("text-(length:--text-sm)", actionTextClass[variant()])}>{props.expanded ? "▾" : "▸"}</Text>
      </If>
      <Text class={cx("text-(length:--text-sm) font-medium", actionTextClass[variant()])}>
        {props.busy ? props.busyLabel ?? props.label : props.label}
      </Text>
    </Button>
  );
}

const indentClass = ["", "ml-(--space-4)", "ml-[32px]", "ml-[48px]", "ml-[64px]"];

/** A selectable row in a navigation list (Chat, Spaces, rooms, threads). */
export function NavItem(
  props: Testable & {
    label: string;
    selected: boolean;
    onPress: () => void;
    /** A short glyph before the label ("#", "›"). Ignored when `icon` is set. */
    prefix?: string;
    /** An icon of the app's family before the label, in place of `prefix`. */
    icon?: IconName;
    /** A quiet note after the label (a role, a count). */
    trailing?: string;
    /** Nesting level (threads under their parent room). */
    depth?: number;
    emphasis?: boolean;
  },
) {
  return (
    <Button
      class={cx(
        "flex-row items-center gap-(--space-2) rounded-(--radius-sm) px-(--space-2) py-[7px] hover:bg-sidebar-accent/60 active:bg-sidebar-accent",
        focusRing,
        indentClass[Math.min(props.depth ?? 0, indentClass.length - 1)],
        props.selected && "bg-sidebar-accent",
      )}
      selected={props.selected}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
    >
      <If
        when={props.icon}
        else={
          <If when={props.prefix !== ""}>
            <Text class="w-[12px] text-(length:--text-sm) text-muted-foreground">{props.prefix ?? "#"}</Text>
          </If>
        }
      >
        {(name) => <Icon name={name()} color={props.selected ? "sidebar-active" : "sidebar"} />}
      </If>
      <Text
        class={cx(
          "flex-1 text-(length:--text-sm)",
          props.selected || props.emphasis ? "font-semibold" : "",
          props.selected ? "text-sidebar-accent-foreground" : "text-sidebar-foreground",
        )}
        style={{ numberOfLines: 1 }}
      >
        {props.label}
      </Text>
      <If when={props.trailing}>
        <Text class="text-(length:--text-xs) text-muted-foreground">{props.trailing}</Text>
      </If>
    </Button>
  );
}

/** A small selectable pill (targets, filters). */
export function Chip(props: Testable & { label: string; selected: boolean; onPress: () => void; disabled?: boolean }) {
  return (
    <Button
      class={cx(
        "rounded-full border px-(--space-3) py-(--space-1) disabled:opacity-50",
        focusRing,
        props.selected ? "border-ring bg-accent" : "border-border bg-background hover:bg-muted",
      )}
      selected={props.selected}
      disabled={props.disabled}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
    >
      <Text class={cx("text-(length:--text-xs)", props.selected ? "font-semibold text-accent-foreground" : "text-foreground")}>{props.label}</Text>
    </Button>
  );
}

/**
 * A reaction pill: the emoji, how many people, and an agent marker. The
 * user's own reaction is highlighted; the agent's acknowledgement has its own
 * tone so it never reads as a person's.
 */
export function ReactionChip(
  props: Testable & { emoji: string; count: number; mine: boolean; agent: boolean; busy?: boolean; onPress: () => void },
) {
  return (
    <Button
      class={cx(
        "flex-row items-center gap-(--space-1) rounded-full border px-(--space-2) py-px disabled:opacity-60",
        focusRing,
        props.mine ? "border-ring bg-accent" : props.agent && props.count === 0 ? "border-tone-agent-foreground/40 bg-tone-agent" : "border-border bg-background hover:bg-muted",
      )}
      selected={props.mine}
      disabled={props.busy}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
    >
      <Text class="text-(length:--text-sm) text-foreground">{props.emoji}</Text>
      <If when={props.count > 0}>
        <Text class={cx("text-(length:--text-xs)", props.mine ? "font-semibold text-accent-foreground" : "text-muted-foreground")}>{String(props.count)}</Text>
      </If>
      <If when={props.agent}>
        <Text class="text-(length:--text-xs) font-medium text-tone-agent-foreground">agent</Text>
      </If>
    </Button>
  );
}

/** A value the user must copy now and won't see again (a token). Selectable, never truncated. */
export function SecretValue(props: Testable & { value: string }) {
  return (
    <View class="rounded-(--radius-sm) border border-border bg-tone-warning px-(--space-3) py-(--space-2)" selectable testID={props.testID}>
      <Text class="text-(length:--text-sm) font-mono text-tone-warning-foreground">{props.value}</Text>
    </View>
  );
}

export function Field(
  props: Testable & {
    value: string;
    onChange: (value: string) => void;
    placeholder?: string;
    masked?: boolean;
    multiline?: boolean;
    disabled?: boolean;
    autoFocus?: boolean;
    grow?: boolean;
    /** Return submits (Shift+Return adds a line in multiline fields). */
    onSubmit?: () => void;
    keepFocusOnSubmit?: boolean;
    maxLength?: number;
  },
) {
  return (
    <Input
      class={cx(
        "rounded-(--radius-sm) border border-input bg-background px-(--space-3) py-(--space-2) text-(length:--text-md) text-foreground placeholder:text-muted-foreground focus:border-ring focus:ring-1 focus:ring-ring disabled:opacity-60",
        props.multiline && "min-h-[64px] max-h-[160px]",
        props.grow && "flex-1",
      )}
      value={props.value}
      onChange={({ value }) => props.onChange(value)}
      onSubmit={() => props.onSubmit?.()}
      submitBehavior={props.onSubmit ? (props.keepFocusOnSubmit || props.multiline ? "submit" : "blurAndSubmit") : undefined}
      placeholder={props.placeholder}
      masked={props.masked}
      multiline={props.multiline}
      disabled={props.disabled}
      autoFocus={props.autoFocus}
      maxLength={props.maxLength}
      autoCapitalize={props.masked ? "none" : undefined}
      autoCorrect={props.masked ? false : undefined}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.placeholder}
    />
  );
}

/** A labelled field for forms. */
export function FormRow(props: { label: string; hint?: string; children?: JSX.Element }) {
  return (
    <Column gap={1}>
      <Label variant="strong">{props.label}</Label>
      {props.children}
      <If when={props.hint}>
        <Label variant="caption" color="muted">
          {props.hint}
        </Label>
      </If>
    </Column>
  );
}

// ---------------------------------------------------------------- status

export function Badge(props: { tone: Tone; label: string; testID?: string }) {
  return (
    <View class={cx("rounded-(--radius-sm) px-(--space-2) py-px", toneSurfaceClass[props.tone])} testID={props.testID}>
      <Text class={cx("text-(length:--text-xs) font-medium", toneTextClass[props.tone])}>{props.label}</Text>
    </View>
  );
}

export function StatusDot(props: { tone: Tone }) {
  return <View class={cx("size-2 rounded-full", toneSolidClass[props.tone])} />;
}

export function Banner(props: Testable & { tone: Tone; children?: JSX.Element }) {
  return (
    <View
      class={cx("flex-row items-center gap-(--space-3) px-(--space-4) py-(--space-2)", toneSurfaceClass[props.tone])}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
      role="alert"
    >
      {props.children}
    </View>
  );
}

/** A tone-coloured rail beside a block of content (transcript entries). */
export function Rail(props: { tone: Tone }) {
  return <View class={cx("w-[3px] self-stretch rounded-full", toneSolidClass[props.tone])} />;
}

// ---------------------------------------------------------------- lists

/** A vertically scrolling region that takes the remaining space. */
export function ScrollArea(props: StackProps) {
  return (
    <Scroll class="flex-1 min-h-0" testID={props.testID}>
      <View class={stackClass({ ...props, grow: false }, "col")}>{props.children}</View>
    </Scroll>
  );
}

/**
 * A virtualized chat-style list: rests at the bottom and follows new items
 * while the user is at (or near) the end, without yanking them when they
 * scrolled up to read.
 */
export function FeedList<T>(props: {
  each: readonly T[];
  keyOf: (item: T) => string;
  empty?: JSX.Element;
  testID?: string;
  ref?: (node: ListRef) => void;
  children: (item: () => T) => JSX.Element;
}) {
  return (
    <List
      each={props.each}
      keyExtractor={(item) => props.keyOf(item)}
      alignment="bottom"
      initialScrollAtEnd
      maintainVisibleContentPosition={{ autoscrollToBottomThreshold: 80 }}
      estimatedItemSize={56}
      class="flex-1"
      contentClass="px-(--space-4) py-(--space-3) gap-(--space-2)"
      empty={props.empty}
      testID={props.testID}
      ref={props.ref}
    >
      {(item) => props.children(item)}
    </List>
  );
}

// ---------------------------------------------------------------- settings

export type ChoiceOption<T extends string> = { value: T; label: string; description?: string; disabled?: boolean };

/**
 * Pick exactly one of a few options (an appearance, a mode). A radio group
 * for screen readers and the keyboard (arrows move, Space checks). Each
 * option's testID is `${testID}:${value}`.
 */
export function SegmentedControl<T extends string>(
  props: {
    /** What the choice is about, said by screen readers ("Appearance"). */
    accessibilityLabel: string;
    options: readonly ChoiceOption<T>[];
    value: T;
    onChange: (value: T) => void;
    disabled?: boolean;
    testID?: string;
  },
) {
  return (
    <RadioGroup
      class="flex-row self-start gap-[2px] rounded-(--radius-sm) border border-border bg-muted p-[3px]"
      accessibilityLabel={props.accessibilityLabel}
      testID={props.testID}
    >
      <Each of={props.options} key={(option) => option.value}>
        {(option) => (
          <Radio
            class={cx(
              "group flex-row items-center justify-center min-w-[84px] rounded-[5px] px-(--space-3) py-[6px] checked:bg-primary disabled:opacity-50",
              // Hover only on unchecked options: a hover background would
              // otherwise cover the checked one under the pointer.
              props.value !== option().value && "hover:bg-background/60",
              focusRing,
            )}
            checked={props.value === option().value}
            disabled={props.disabled || option().disabled}
            onChange={() => props.onChange(option().value)}
            accessibilityLabel={option().description ? `${option().label}. ${option().description}` : option().label}
            testID={props.testID ? `${props.testID}:${option().value}` : undefined}
          >
            <Text class="text-(length:--text-sm) font-medium text-muted-foreground group-checked:text-primary-foreground">{option().label}</Text>
          </Radio>
        )}
      </Each>
    </RadioGroup>
  );
}

/** An on/off setting that takes effect at once. */
export function Toggle(
  props: Testable & {
    /** What it turns on, said by screen readers (and shown when `showLabel`). */
    label: string;
    checked: boolean;
    onChange: (checked: boolean) => void;
    disabled?: boolean;
    /** Show the label beside the track (off when a SettingRow already shows it). */
    showLabel?: boolean;
  },
) {
  return (
    <Switch
      class={cx("flex-row items-center gap-(--space-2) rounded-full disabled:opacity-50", focusRing)}
      trackClass="w-[36px] h-[20px] rounded-full bg-input p-[2px] checked:bg-primary transition-colors"
      thumbClass="size-[16px] rounded-full bg-primary-foreground checked:translate-x-4 transition-transform"
      checked={props.checked}
      disabled={props.disabled}
      onChange={({ checked }) => props.onChange(checked)}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
      testID={props.testID}
    >
      <If when={props.showLabel}>
        <Text class="text-(length:--text-sm) text-foreground">{props.label}</Text>
      </If>
    </Switch>
  );
}

export type PickerItem = { value: string; label: string; disabled?: boolean; group?: string };

/** A drop-down choice among many (a provider, a model). */
export function Picker(
  props: {
    accessibilityLabel: string;
    items: readonly PickerItem[];
    value: string | null;
    onChange: (value: string | null) => void;
    placeholder?: string;
    disabled?: boolean;
    searchable?: boolean;
    testID?: string;
  },
) {
  const current = () => props.items.find((item) => item.value === props.value)?.label;
  return (
    <Select
      class={cx(
        "flex-row items-center justify-between gap-(--space-2) min-h-[32px] min-w-[200px] self-start rounded-(--radius-sm) border border-input bg-background px-(--space-3) hover:bg-muted disabled:opacity-50",
        focusRing,
      )}
      items={props.items.map((item) => ({ value: item.value, title: item.label, disabled: item.disabled, group: item.group }))}
      value={props.value}
      onChange={({ value }) => props.onChange(value)}
      placeholder={props.placeholder}
      searchable={props.searchable}
      disabled={props.disabled}
      accessibilityLabel={props.accessibilityLabel}
      popupClass="rounded-(--radius) border border-border bg-popover p-[4px] shadow-lg"
      itemClass="rounded-(--radius-sm) px-(--space-2) py-[6px] text-(length:--text-sm) text-popover-foreground focus:bg-accent selected:font-semibold disabled:opacity-50"
      groupClass="px-(--space-2) pt-(--space-2) pb-(--space-1)"
      searchClass="rounded-(--radius-sm) border border-input bg-background px-(--space-2) py-(--space-1) text-(length:--text-sm) text-foreground"
      testID={props.testID}
      trigger={
        <View class="flex-1 flex-row items-center justify-between gap-(--space-2)">
          <Text class={cx("text-(length:--text-sm)", current() ? "text-foreground" : "text-muted-foreground")} style={{ numberOfLines: 1 }}>
            {current() ?? props.placeholder ?? ""}
          </Text>
          <Text class="text-(length:--text-xs) text-muted-foreground">▾</Text>
        </View>
      }
    />
  );
}

/** The title block at the top of a page (Settings, a Space's management). */
export function PageHeader(props: Testable & { title: string; subtitle?: string; children?: JSX.Element }) {
  return (
    <View class="flex-row items-start gap-(--space-4) pb-(--space-2)" testID={props.testID}>
      <View class="flex-1 min-w-0 flex-col gap-(--space-1)">
        <Text class="text-(length:--text-xl) font-semibold tracking-tight text-foreground" role="header">
          {props.title}
        </Text>
        <If when={props.subtitle}>
          <Text class="text-(length:--text-sm) text-muted-foreground">{props.subtitle}</Text>
        </If>
      </View>
      {props.children}
    </View>
  );
}

/**
 * A titled group of SettingRows on one card. The rows draw the lines between
 * them; the card clips the first one's.
 */
export function SettingsGroup(props: Testable & { title?: string; description?: string; children?: JSX.Element }) {
  return (
    <View class="flex-col gap-(--space-2)" testID={props.testID} accessibilityLabel={props.accessibilityLabel}>
      <If when={props.title || props.description}>
        <View class="flex-col gap-px px-(--space-1)">
          <If when={props.title}>
            <Text class="text-(length:--text-sm) font-semibold text-foreground" role="header">
              {props.title}
            </Text>
          </If>
          <If when={props.description}>
            <Text class="text-(length:--text-xs) text-muted-foreground">{props.description}</Text>
          </If>
        </View>
      </If>
      <View class="overflow-hidden rounded-(--radius) border border-border bg-card">
        <View class="-mt-px flex-col">{props.children}</View>
      </View>
    </View>
  );
}

/**
 * One setting: its name and an optional explanation on the left, its control
 * on the right. `stacked` puts the control below (wide fields, long lists).
 */
export function SettingRow(props: Testable & { label: string; description?: string; stacked?: boolean; children?: JSX.Element }) {
  return (
    <View
      class={cx(
        "border-t border-border px-(--space-4) py-(--space-3) gap-(--space-3)",
        props.stacked ? "flex-col" : "flex-row items-center justify-between",
      )}
      testID={props.testID}
    >
      <View class="flex-1 min-w-0 flex-col gap-px">
        <Text class="text-(length:--text-md) font-medium text-card-foreground" accessibilityLabel={props.accessibilityLabel}>
          {props.label}
        </Text>
        <If when={props.description}>
          <Text class="text-(length:--text-xs) text-muted-foreground">{props.description}</Text>
        </If>
      </View>
      {props.children}
    </View>
  );
}
