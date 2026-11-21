// The app's styled primitives. Feature code composes these and passes only
// semantic props (variant, tone, space); all styling decisions live here and
// in theme/theme.json. To swap the design system, replace this module while
// keeping its exports' props.

import { List, Scroll, Text, View } from "nsh";
import type { JSX, ListRef } from "nsh";
import { Button, Input } from "nsh/base";
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

export type SurfaceVariant = "app" | "sidebar" | "panel" | "card" | "dialog" | "toolbar" | "footer" | "aside" | "section" | "scroll";

const surfaceClass: Record<SurfaceVariant, string> = {
  app: "flex-1 flex-row bg-background",
  sidebar: "w-(--sidebar-width) bg-sidebar border-r border-sidebar-border",
  panel: "flex-1 min-w-0 bg-background",
  card: "bg-card border border-border rounded-(--radius)",
  dialog: "w-full max-w-[460px] bg-card border border-border rounded-(--radius)",
  toolbar: "flex-row items-center border-b border-border bg-background",
  footer: "border-t border-border bg-background",
  aside: "w-[340px] border-l border-border bg-sidebar",
  section: "border-b border-border",
  scroll: "flex-1 min-h-0",
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

export type LabelVariant = "title" | "heading" | "body" | "strong" | "caption" | "mono";
export type LabelColor = "default" | "muted" | "danger" | "sidebar" | "sidebar-active" | Tone;

const labelVariantClass: Record<LabelVariant, string> = {
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
  sidebar: "text-sidebar-foreground",
  "sidebar-active": "text-sidebar-accent-foreground",
};

function colorClass(color: LabelColor) {
  return Object.prototype.hasOwnProperty.call(labelColorClass, color) ? labelColorClass[color] : toneTextClass[color as Tone];
}

export function Label(props: Testable & { variant?: LabelVariant; color?: LabelColor; lines?: number; children?: JSX.Element }) {
  return (
    <Text
      class={cx("shrink min-w-0", labelVariantClass[props.variant ?? "body"], colorClass(props.color ?? "default"))}
      style={props.lines ? { numberOfLines: props.lines } : undefined}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
    >
      {props.children}
    </Text>
  );
}

// ---------------------------------------------------------------- controls

export type ActionVariant = "primary" | "secondary" | "ghost" | "danger";

const actionClass: Record<ActionVariant, string> = {
  primary: "bg-primary active:bg-primary/85 hover:bg-primary/90 disabled:opacity-50",
  secondary: "bg-secondary border border-border active:bg-secondary/70 hover:bg-muted disabled:opacity-50",
  ghost: "active:bg-muted hover:bg-muted disabled:opacity-50",
  danger: "bg-destructive active:bg-destructive/85 disabled:opacity-50",
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
  },
) {
  const variant = () => props.variant ?? "secondary";
  return (
    <Button
      class={cx(
        "flex-row items-center justify-center rounded-(--radius-sm) px-(--space-3) py-(--space-2) focus-visible:ring-2 focus-visible:ring-ring",
        actionClass[variant()],
      )}
      disabled={props.disabled || props.busy}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
    >
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
    /** A short glyph before the label ("#", "›"). */
    prefix?: string;
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
        "flex-row items-center gap-(--space-2) rounded-(--radius-sm) px-(--space-2) py-(--space-2) hover:bg-sidebar-accent/60 active:bg-sidebar-accent",
        indentClass[Math.min(props.depth ?? 0, indentClass.length - 1)],
        props.selected && "bg-sidebar-accent",
      )}
      selected={props.selected}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
    >
      <If when={props.prefix !== ""}>
        <Text class="w-[12px] text-(length:--text-sm) text-muted-foreground">{props.prefix ?? "#"}</Text>
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
        "rounded-(--radius-sm) border border-input bg-background px-(--space-3) py-(--space-2) text-(length:--text-md) text-foreground placeholder:text-muted-foreground focus:border-ring",
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
