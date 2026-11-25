// The app's styled primitives. Feature code composes these and passes only
// semantic props (variant, tone, space); all styling decisions live here and
// in theme/theme.json. To swap the design system, replace this module while
// keeping its exports' props.

import { cleanup, effect, keymap, List, Scroll, signal, Text, untracked, View } from "nsh";
import type { InputRef, JSX, ListRef, ViewRef } from "nsh";
import { Button, Checkbox as BaseCheckbox, Collapsible, ContextMenu, Input, Menu, MenuItem, MenuSeparator, Radio, RadioGroup, Select, Switch } from "nsh/base";
import type { ButtonProps } from "nsh/base";
import { Icon, SpaceTile } from "./chrome";
import type { IconColor, IconName } from "./chrome";
import {
  actionDisabledClass,
  controlEdgeClass,
  controlHeightClass,
  cx,
  disabledClass,
  disabledTextClass,
  focusRingClass,
  gapClass,
  padClass,
  padXClass,
  padYClass,
  popoverClass,
  popoverItemClass,
  popoverMotion,
  textClass,
  toneBorderClass,
  toneSolidClass,
  toneSurfaceClass,
  toneTextClass,
  tooltipClass,
  transitionClass,
} from "./tokens";
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

// Pass layout overrides separately: spreading NSH props evaluates their lazy children.
function stackClass(props: StackProps, direction: "row" | "col", grow = props.grow) {
  return cx(
    direction === "row" ? "flex-row" : "flex-col",
    grow && "flex-1 min-w-0 min-h-0",
    props.wrap && "flex-wrap",
    props.align && alignClass[props.align],
    props.justify && justifyClass[props.justify],
    gapClass[props.gap ?? 0],
    padClass[props.pad ?? 0],
    padXClass[props.padX ?? 0],
    padYClass[props.padY ?? 0],
  );
}

export function Row(props: StackProps & { stacked?: boolean }) {
  return (
    <View class={stackClass(props, props.stacked ? "col" : "row")} selectable={props.selectable} testID={props.testID} accessibilityLabel={props.accessibilityLabel}>
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
  | "rail"
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
  | "topbar"
  /** A floating surface one step above cards (a menu, a preview of one). */
  | "popover";

const surfaceClass: Record<SurfaceVariant, string> = {
  app: "flex-1 flex-row bg-background",
  // Navy deepening toward a lighter tone, ended by a hairline rather than a hard border.
  sidebar: "w-(--sidebar-width) bg-sidebar bg-linear-to-b from-sidebar to-sidebar-gradient-end border-r border-sidebar-border/70",
  /** A collapsed sidebar: one column of icons, the account at the bottom. */
  rail: "w-[56px] shrink-0 items-center bg-sidebar bg-linear-to-b from-sidebar to-sidebar-gradient-end border-r border-sidebar-border/70 py-(--space-3) gap-(--space-1)",
  panel: "flex-1 min-w-0 bg-background",
  // Cards are flat: a border, no shadow. Only popovers and dialogs float.
  card: "bg-card border border-border rounded-(--radius)",
  dialog: "w-full max-w-[460px] bg-card border border-border rounded-(--radius-xl) shadow-lg",
  toolbar: "flex-row items-center min-h-[52px] border-b border-border bg-background",
  footer: "border-t border-border bg-background",
  aside: "w-[340px] border-l border-border bg-sidebar",
  section: "border-b border-border",
  scroll: "flex-1 min-h-0",
  content: "w-full max-w-(--content-width) self-center",
  // One height with the sidebar's brand header and the rail's (--header-height), its divider inside it, so both dividers share one y.
  topbar: "flex-row items-center justify-center shrink-0 h-(--header-height) px-(--space-4) border-b border-border bg-background",
  popover: "bg-popover border border-border rounded-(--radius-lg) shadow-lg",
};

export function Surface(props: StackProps & {
  variant: SurfaceVariant; hidden?: boolean;
  /** A side panel's width the user chose (points), over the variant's own. */
  width?: number;
  /** Called after a nonempty layout; may fire again on resize. */
  onLaidOut?: () => void;
}) {
  return (
    <View
      class={cx(surfaceClass[props.variant], stackClass(props, props.variant === "toolbar" || props.variant === "topbar" || props.variant === "app" ? "row" : "col", false), props.grow && "flex-1 min-h-0", props.hidden && "hidden", props.width !== undefined && "shrink-0")}
      style={props.width !== undefined ? { width: props.width } : undefined}
      accessibilityElementsHidden={props.hidden}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
      onLayout={props.onLaidOut ? ({ bounds }) => {
        if (bounds.size.width > 0 && bounds.size.height > 0) props.onLaidOut?.();
      } : undefined}
    >
      {props.children}
    </View>
  );
}

export function Divider() {
  return <View class="h-px bg-border" />;
}

/**
 * The main pane's one header row, divided from the content under it by a
 * line. Alone, `children` (the search box) fills the row and stays on the
 * pane's centerline: the leading control's actual width is reserved on both
 * sides. With a `title` (a room's breadcrumb) and `trailing` actions the row
 * reads leading, title, search, actions, the search filling the middle.
 */
export function CenteredToolbar(props: Children & Testable & { leading: JSX.Element; title?: JSX.Element; trailing?: JSX.Element }) {
  const leadingWidth = signal(0);
  const split = () => !!(props.title || props.trailing);
  // One tree either way, so the search box keeps its focus as the place changes.
  return (
    <Surface variant="topbar" testID={props.testID} gap={2}>
      <View class="shrink-0" onLayout={e => leadingWidth.set(e.bounds.size.width)}>{props.leading}</View>
      <If when={split()}>
        {/* The title takes what it needs; when the row is tight the search box gives way first (to 180), then the title (to 120). */}
        <View class="shrink min-w-[120px] max-w-[40%] flex-row items-center overflow-hidden">{props.title}</View>
      </If>
      {/* The search box fills the space between the leading toggle (and a title) and the trailing actions, inset from both. */}
      <View class={cx("flex-1 flex-col items-stretch px-(--space-2)", split() ? "min-w-[180px]" : "min-w-0")}>{props.children}</View>
      <View class="shrink-0 flex-row items-center gap-(--space-2)" style={split() ? undefined : { width: leadingWidth() }} accessibilityElementsHidden={!split()}>
        {props.trailing}
      </View>
    </Surface>
  );
}

// ---------------------------------------------------------------- text

export type LabelVariant =
  | "display"
  | "title"
  | "heading"
  | "body"
  | "strong"
  | "caption"
  /** Metadata (times, counts, ids): small, tabular figures, muted unless a color is given. */
  | "meta"
  | "mono";
export type LabelColor = "default" | "muted" | "danger" | "accent" | "sidebar" | "sidebar-active" | Tone;

const labelVariantClass: Record<LabelVariant, string> = {
  display: cx(textClass["2xl"], "font-semibold tracking-tight"),
  title: cx(textClass.xl, "font-semibold tracking-tight"),
  heading: cx(textClass.lg, "font-semibold"),
  body: textClass.md,
  strong: cx(textClass.sm, "font-semibold"),
  caption: textClass.xs,
  meta: cx(textClass.xs, "tabular-nums"),
  mono: cx(textClass.sm, "font-mono"),
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
    tabular?: boolean;
    /** Screen readers treat it as a heading (page and section titles). */
    heading?: boolean;
    /** More words on hover (a description that has no room on screen). */
    tooltip?: string;
    children?: JSX.Element;
  },
) {
  return (
    <Text
      class={cx("shrink min-w-0", labelVariantClass[props.variant ?? "body"], props.tabular && "tabular-nums",
        colorClass(props.color ?? (props.variant === "meta" ? "muted" : "default")))}
      style={props.lines ? { numberOfLines: props.lines } : undefined}
      role={props.heading ? "header" : undefined}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
      tooltip={props.tooltip ? <Text class={tooltipClass}>{props.tooltip}</Text> : undefined}
    >
      {props.children}
    </Text>
  );
}

// ---------------------------------------------------------------- controls

/** The keyboard focus ring every interactive primitive shares. */
const focusRing = focusRingClass;

/** Only the focused control takes Escape, through the keymap before modal dismissal. */
function focusedEscape(handler: () => (() => void) | undefined) {
  const focused = signal(false);
  effect(() => {
    if (!focused() || !handler()) return;
    return keymap.bind("escape", () => handler()?.(), { when: "always", forFocus: true });
  });
  return { onFocus: () => focused.set(true), onBlur: () => focused.set(false) };
}

// NSH's base Button does not expose focus events. Keep its host for ordinary
// Actions; only controls with a focus-scoped Escape need the View host.
function EscapeActionButton(props: ButtonProps & { onEscape?: () => void }) {
  const escape = focusedEscape(() => props.onEscape);
  return (
    <View
      class={props.class}
      ref={props.ref}
      role="button"
      focusable={!props.disabled}
      disabled={props.disabled}
      accessibilityState={{ disabled: !!props.disabled }}
      accessibilityLabel={props.accessibilityLabel ?? undefined}
      testID={props.testID}
      onPress={() => props.onPress?.()}
      onFocus={escape.onFocus}
      onBlur={escape.onBlur}
    >
      {props.children}
    </View>
  );
}

/**
 * - `primary`: the one main action of a view.
 * - `secondary`: an outlined everyday action.
 * - `ghost`: a quiet action (Cancel, a disclosure, a link-like action).
 * - `destructive`: an outlined action that removes or revokes something and
 *   asks for confirmation next. Use it for the first press.
 * - `danger`: filled, only for the final confirm of a destructive action.
 */
export type ActionVariant = "primary" | "secondary" | "ghost" | "destructive" | "danger";

// Hover and press use opaque tokens (primary-hover, destructive-hover), so
// the label keeps AA on them. Disabled Actions of every variant show the
// same muted surface and muted text (actionDisabledClass), never a dimmed
// copy of the variant, so a disabled primary never reads as live.
const actionClass: Record<ActionVariant, string> = {
  primary: "bg-primary hover:bg-primary-hover active:bg-primary-hover",
  secondary: "bg-background border border-input hover:bg-muted active:bg-muted",
  ghost: "hover:bg-muted active:bg-muted",
  // The danger ink as text and a 3:1 edge on page, card and popover.
  destructive: "bg-background border border-destructive hover:bg-tone-danger active:bg-tone-danger",
  danger: "bg-destructive hover:bg-destructive-hover active:bg-destructive-hover",
};

const actionTextClass: Record<ActionVariant, string> = {
  primary: "text-primary-foreground",
  secondary: "text-secondary-foreground",
  ghost: "text-foreground",
  destructive: "text-destructive",
  danger: "text-destructive-foreground",
};

// Icons have no on-fill tint yet: filled variants show none (see Action's icon).
const actionIconColor: Record<ActionVariant, IconColor | null> = {
  primary: null,
  secondary: "default",
  ghost: "muted",
  destructive: "muted",
  danger: null,
};

export function Action(
  props: Testable & {
    label: string;
    onPress: () => void;
    variant?: ActionVariant;
    disabled?: boolean;
    busy?: boolean;
    busyLabel?: string;
    /** A disclosure: shows a chevron (right closed, down open) before the label. Say the state in `accessibilityLabel` too. */
    expanded?: boolean;
    /** 28 high instead of 36, tighter padding: in rows, lists and table cells. */
    compact?: boolean;
    /**
     * Ghost and destructive Actions only: pull the button out by its own
     * padding so its label lines up with the text edge above and below it
     * (the start edge; with `justify="end"` rows, put it last).
     */
    edgeAligned?: boolean;
    /** An icon of the app's family before the label (secondary, ghost and destructive only). */
    icon?: IconName;
    /** For focus handoffs to this action, such as returning from an inline confirmation. */
    viewRef?: (node: ViewRef) => void;
    /** Consume Escape while this action is focused (e.g. inside an inline confirmation). */
    onEscape?: () => void;
  },
) {
  const variant = () => props.variant ?? "secondary";
  const iconColor = () => actionIconColor[variant()];
  const quiet = () => variant() === "ghost" || variant() === "destructive";
  const Control = props.onEscape ? EscapeActionButton : Button;
  return (
    <Control
      ref={props.viewRef}
      onEscape={props.onEscape}
      class={cx(
        "group flex-row items-center justify-center",
        props.compact ? cx(controlHeightClass.compact, "rounded-(--radius-sm) gap-(--space-1) px-(--space-2)") : cx(controlHeightClass.default, "rounded-(--radius-md) gap-(--space-2) px-(--space-3)"),
        props.expanded !== undefined && "justify-start",
        props.edgeAligned && quiet() && (props.compact ? "-mx-2" : "-mx-3"),
        focusRing,
        transitionClass,
        actionClass[variant()],
        actionDisabledClass,
      )}
      disabled={props.disabled || props.busy}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
    >
      <If when={props.expanded !== undefined ? iconColor() : null}>
        {(color) => <Icon name={props.expanded ? "chevron-down" : "chevron-right"} color={color()} size={14} />}
      </If>
      <If when={props.expanded === undefined && props.icon ? iconColor() : null}>
        {(color) => <Icon name={props.icon!} color={color()} size={props.compact ? 14 : 16} />}
      </If>
      <Text class={cx(props.compact ? textClass.xs : textClass.sm, "font-medium", actionTextClass[variant()], disabledTextClass)}>
        {props.busy ? props.busyLabel ?? props.label : props.label}
      </Text>
    </Control>
  );
}

/** A row of Actions at the end of a card, form or footer (the primary one last). */
export function ActionBar(props: Testable & { children?: JSX.Element; justify?: "end" | "between" | "start" }) {
  return (
    <View
      class={cx("flex-row flex-wrap items-center gap-(--space-2)", justifyClass[props.justify ?? "end"])}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
    >
      {props.children}
    </View>
  );
}

// Nesting is inner padding, never a margin: a nested row's selected pill
// keeps the list's full width, and its glyph lines up under its parent's
// name (the parent's 20-point leading slot plus the 8-point gap: 28 more).
const indentClass = ["pl-(--space-3)", "pl-[40px]", "pl-[56px]", "pl-[72px]", "pl-[88px]"];

/** A selectable row in a navigation list (Chat, Spaces, rooms, threads). */
export function NavItem(
  props: Testable & {
    label: string;
    selected: boolean;
    onPress: () => void;
    /** A short glyph before the label ("#", "›"). Ignored when `icon` or `tile` is set. */
    prefix?: string;
    /** An icon of the app's family before the label, in place of `prefix`. */
    icon?: IconName;
    /** A rounded letter tile before the label (a Space), its color fixed by `seed`. */
    tile?: { initial: string; seed: string };
    /** A chevron at the end: the row opens a group (a Space's rooms). */
    expanded?: boolean;
    /** A quiet note after the label (a role, a count). */
    trailing?: string;
    /** A small accent dot at the end (something is working there); its words for screen readers. */
    dot?: string;
    /** Nesting level (threads under their parent room). */
    depth?: number;
    emphasis?: boolean;
    /** A quiet extra shown on hover (a role), never in the row. */
    tooltip?: string;
    /**
     * The row's own actions ("Space settings"): a ⋯ button at its end that
     * shows on hover and keyboard focus (always on touch screens), and the
     * same items on a secondary click of the row.
     */
    menu?: { items: readonly ActionsMenuItem[]; accessibilityLabel: string; testID?: string };
  },
) {
  const row = (
    <Button
      class={cx(
        // 40 high: a comfortable list row; the selected one is a soft pill with an accent bar.
        "relative flex-row items-center gap-(--space-2) min-h-[40px] rounded-(--radius-md) py-[8px]",
        props.menu ? "pr-[40px]" : "pr-(--space-3)",
        focusRing,
        transitionClass,
        indentClass[Math.min(props.depth ?? 0, indentClass.length - 1)],
        props.selected ? "bg-sidebar-accent" : "hover:bg-sidebar-hover active:bg-sidebar-hover",
      )}
      selected={props.selected}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={(props.accessibilityLabel ?? props.label) + (props.dot ? `, ${props.dot}` : "")}
      tooltip={props.tooltip ? <Text class={tooltipClass}>{props.tooltip}</Text> : undefined}
    >
      <If when={props.selected}>
        <View class="absolute left-0 top-[9px] bottom-[9px] w-[3px] rounded-full bg-sidebar-indicator" accessibilityElementsHidden />
      </If>
      <View class="w-[20px] shrink-0 items-center justify-center" accessibilityElementsHidden>
        <If
          when={props.tile}
          else={
            <If
              when={props.icon}
              else={
                <If when={props.prefix !== ""}>
                  <Text class={cx(textClass.sm, "text-muted-foreground")}>{props.prefix ?? "#"}</Text>
                </If>
              }
            >
              {(name) => <Icon name={name()} color={props.selected ? "sidebar-indicator" : "sidebar"} />}
            </If>
          }
        >
          {(t) => <SpaceTile initial={t().initial} seed={t().seed} />}
        </If>
      </View>
      <Text
        class={cx(
          "flex-1",
          textClass.sm,
          props.selected ? "font-semibold text-sidebar-accent-foreground" : props.emphasis ? "font-semibold text-sidebar-foreground" : "text-sidebar-foreground",
        )}
        style={{ numberOfLines: 1 }}
      >
        {props.label}
      </Text>
      <If when={props.trailing}>
        <Text class={cx(textClass.xs, "text-muted-foreground")}>{props.trailing}</Text>
      </If>
      <If when={props.dot}>
        <View class="size-[7px] rounded-full bg-sidebar-indicator" accessibilityElementsHidden />
      </If>
      <If when={props.expanded !== undefined}>
        <Icon name={props.expanded ? "chevron-down" : "chevron-right"} color="muted" size={12} />
      </If>
    </Button>
  );
  const items = (onSelect: (item: ActionsMenuItem) => void) => (
    <Each of={[...(props.menu?.items ?? [])]} key={(item) => item.id}>
      {(item) => (
        <MenuItem class={popoverItemClass} disabled={item().disabled} onSelect={() => onSelect(item())} testID={item().id}>
          <Text class={cx(textClass.sm, item().destructive ? "text-destructive" : "text-popover-foreground")}>{item().label}</Text>
        </MenuItem>
      )}
    </Each>
  );
  return (
    <If when={props.menu} else={row}>
      {(menu) => (
        <View class="group/nav relative">
          <ContextMenu
            popupClass={cx(popoverClass, "min-w-[180px]")}
            popupStartingClass={popoverMotion.starting}
            popupEndingClass={popoverMotion.ending}
            trigger={row}
          >
            {items((item) => item.onPress())}
          </ContextMenu>
          <View class="absolute right-[6px] top-0 bottom-0 justify-center">
            <Menu
              placement="bottom"
              align="end"
              offset={4}
              popupClass={cx(popoverClass, "min-w-[180px]")}
              popupStartingClass={popoverMotion.starting}
              popupEndingClass={popoverMotion.ending}
              trigger={
                <Button
                  class={cx(
                    "items-center justify-center size-[26px] rounded-(--radius-sm) hover:bg-sidebar-hover active:bg-sidebar-hover aria-expanded:bg-sidebar-hover aria-expanded:opacity-100",
                    "opacity-0 group-hover/nav:opacity-100 focus-visible:opacity-100 pointer-coarse:opacity-100",
                    focusRing,
                    transitionClass,
                  )}
                  testID={menu().testID}
                  accessibilityLabel={menu().accessibilityLabel}
                  tooltip={<Text class={tooltipClass}>More</Text>}
                >
                  <Icon name="more" color="sidebar" size={14} />
                </Button>
              }
            >
              {items((item) => item.onPress())}
            </Menu>
          </View>
        </View>
      )}
    </If>
  );
}

/**
 * Rows nested under a NavItem (a Space's rooms), joined by a thin vertical
 * guide under the parent's tile. The rows keep the list's full width; their
 * own padding indents them.
 */
export function NavGroup(props: Testable & { children?: JSX.Element }) {
  return (
    <View class="relative flex-col gap-(--space-1)" testID={props.testID} accessibilityLabel={props.accessibilityLabel}>
      <View class="absolute left-[21px] top-[4px] bottom-[4px] w-px bg-sidebar-guide" accessibilityElementsHidden />
      {props.children}
    </View>
  );
}

/**
 * A section's heading in a sidebar ("SPACES"): small, uppercase, wide
 * tracking, muted, with an optional action that shows on hover and keyboard
 * focus (always on touch screens, and while `actionShown`).
 */
export function SectionHeader(
  props: Testable & {
    label: string;
    action?: { icon: IconName; label: string; tooltip?: string; onPress: () => void; testID?: string; shown?: boolean };
  },
) {
  return (
    <View class="group/section flex-row items-center min-h-[30px] pl-(--space-3) pr-[5px]" testID={props.testID}>
      <Text class={cx("flex-1 uppercase tracking-wider font-semibold text-muted-foreground", textClass.xs)} role="header" style={{ numberOfLines: 1 }}>
        {props.label}
      </Text>
      <If when={props.action}>
        {(a) => (
          <Button
            class={cx(
              "items-center justify-center size-[26px] rounded-(--radius-sm) hover:bg-sidebar-hover active:bg-sidebar-hover",
              transitionClass,
              focusRing,
              a().shown ? "opacity-100" : "opacity-0 group-hover/section:opacity-100 focus-visible:opacity-100 pointer-coarse:opacity-100",
            )}
            onPress={a().onPress}
            testID={a().testID}
            accessibilityLabel={a().label}
            tooltip={<Text class={tooltipClass}>{a().tooltip ?? a().label}</Text>}
          >
            <Icon name={a().icon} color="sidebar" size={14} />
          </Button>
        )}
      </If>
    </View>
  );
}

/** The top of a sidebar: the logo, centred, in a band as tall as the main panel's header row, its divider on the same line. */
/** The top of the icon rail: the compact mark in a band as tall as the main panel's header row, its divider on the same line. Pulled up over the rail's top padding. */
export function RailBrand(props: { children?: JSX.Element }) {
  return <View class="self-stretch shrink-0 items-center justify-center h-(--header-height) mt-[-12px] mb-(--space-2) border-b border-sidebar-border/70">{props.children}</View>;
}

export function SidebarBrand(props: { children?: JSX.Element }) {
  return <View class="flex-row shrink-0 items-center justify-center h-(--header-height) px-(--space-4) border-b border-sidebar-border/70">{props.children}</View>;
}

/** A small selectable pill (targets, filters). */
export function Chip(props: Testable & { label: string; selected: boolean; onPress: () => void; disabled?: boolean }) {
  return (
    <Button
      class={cx(
        "flex-row items-center min-h-[28px] rounded-full border px-(--space-3)",
        focusRing,
        transitionClass,
        disabledClass,
        props.selected ? "border-ring bg-accent" : "border-border bg-background hover:bg-muted",
      )}
      selected={props.selected}
      disabled={props.disabled}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
    >
      <Text class={cx(textClass.xs, "font-medium", props.selected ? "text-accent-foreground" : "text-foreground")}>{props.label}</Text>
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
        "flex-row items-center gap-(--space-1) rounded-full border px-(--space-2) py-px",
        focusRing,
        transitionClass,
        disabledClass,
        props.mine ? "border-ring bg-accent" : props.agent && props.count === 0 ? "border-tone-agent-foreground/40 bg-tone-agent" : "border-border bg-background hover:bg-muted",
      )}
      selected={props.mine}
      disabled={props.busy}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
    >
      <Text class={cx(textClass.sm, "text-foreground")}>{props.emoji}</Text>
      <If when={props.count > 0}>
        <Text class={cx(textClass.xs, "tabular-nums", props.mine ? "font-semibold text-accent-foreground" : "text-muted-foreground")}>{String(props.count)}</Text>
      </If>
      <If when={props.agent}>
        <Text class={cx(textClass.xs, "font-medium text-tone-agent-foreground")}>agent</Text>
      </If>
    </Button>
  );
}

/** A value the user must copy now and won't see again (a token). Selectable, never truncated. */
export function SecretValue(props: Testable & { value: string }) {
  return (
    <View class="rounded-(--radius-sm) border border-border bg-tone-warning px-(--space-3) py-(--space-2)" selectable testID={props.testID}>
      <Text class={cx(textClass.sm, "font-mono text-tone-warning-foreground")}>{props.value}</Text>
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
    /** For handing focus to the field when opening inline content. */
    inputRef?: (node: InputRef) => void;
    /** Keep the text exactly as typed (names and confirmation phrases). */
    verbatim?: boolean;
    grow?: boolean;
    /** Return submits (Shift+Return adds a line in multiline fields). */
    onSubmit?: () => void;
    keepFocusOnSubmit?: boolean;
    maxLength?: number;
    /** Escape in the field (an inline edit's cancel). */
    onCancel?: () => void;
  },
) {
  // Escape cancels while the field has the focus: an app binding that wins over the field's own keys.
  const focused = signal(false);
  effect(() => {
    if (!props.onCancel || !focused()) return;
    return keymap.bind("escape", () => props.onCancel?.(), { when: "always" });
  });
  return (
    <Input
      ref={props.inputRef}
      onFocus={() => focused.set(true)}
      onBlur={() => focused.set(false)}
      class={cx(
        "rounded-(--radius-md) border bg-background px-(--space-3) py-[7px] text-foreground placeholder:text-muted-foreground focus:border-ring focus:ring-1 focus:ring-ring",
        controlEdgeClass,
        textClass.md,
        controlHeightClass.default,
        disabledClass,
        props.multiline && "min-h-[72px] max-h-[160px]",
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
      autoCapitalize={props.masked || props.verbatim ? "none" : undefined}
      autoCorrect={props.masked || props.verbatim ? false : undefined}
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

/**
 * A quiet status pill. `soft` (the default) is tinted by its tone; `outline`
 * is quieter still, for metadata beside a name (a scope, a capability).
 */
export function Badge(props: { tone: Tone | "info"; label: string; testID?: string; variant?: "soft" | "outline" }) {
  const outline = () => props.variant === "outline";
  return (
    <View
      class={cx(
        "flex-row items-center self-center rounded-full px-[8px] py-px",
        outline() ? "border border-border" : props.tone === "info" ? "bg-accent" : toneSurfaceClass[props.tone],
      )}
      testID={props.testID}
    >
      <Text class={cx(textClass.xs, "font-medium", outline() ? "text-muted-foreground" : props.tone === "info" ? "text-accent-foreground" : toneTextClass[props.tone])} style={{ numberOfLines: 1 }}>
        {props.label}
      </Text>
    </View>
  );
}

export function StatusDot(props: { tone: Tone }) {
  return <View class={cx("size-2 rounded-full", toneSolidClass[props.tone])} />;
}

/**
 * Feedback in a tone: an error, a warning, a success, something in progress.
 *
 * - `bar` (the default): full width, square, flush with the edges of the
 *   panel or card it sits in (above a composer, under a toolbar).
 * - `inline`: a rounded, outlined box inside a form or card's flow.
 *
 * Give `title` and/or `message` for the standard look (a strong title, the
 * message under it, both in the tone's text color) and `action` for one
 * trailing control (Retry, Dismiss). `children` still work for anything else
 * and come after the message. Warnings and errors are announced at once.
 */
export function Banner(
  props: Testable & {
    tone: Tone;
    variant?: "bar" | "inline";
    title?: string;
    message?: string;
    action?: JSX.Element;
    children?: JSX.Element;
  },
) {
  const inline = () => props.variant === "inline";
  return (
    <View
      class={cx(
        "flex-row items-center gap-(--space-3)",
        inline() ? cx("rounded-(--radius) border px-(--space-3) py-(--space-2)", toneBorderClass[props.tone]) : "px-(--space-4) py-(--space-2)",
        toneSurfaceClass[props.tone],
      )}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
      role="alert"
    >
      <If
        when={props.title || props.message}
        else={props.children}
      >
        <View class="flex-1 min-w-0 flex-col gap-px">
          <If when={props.title}>
            <Text class={cx(textClass.sm, "font-semibold", toneTextClass[props.tone])}>{props.title}</Text>
          </If>
          <If when={props.message}>
            <Text class={cx(textClass.sm, toneTextClass[props.tone])}>{props.message}</Text>
          </If>
          {props.children}
        </View>
      </If>
      <If when={props.action}>
        <View class="shrink-0 flex-row items-center gap-(--space-2)">{props.action}</View>
      </If>
    </View>
  );
}

type ConfirmTrigger =
  | { triggerLabel: string; open?: boolean; onOpenChange?: (open: boolean) => void; returnFocus?: () => ViewRef | null | undefined }
  | { triggerLabel?: never; open: boolean; onOpenChange: (open: boolean) => void; returnFocus: () => ViewRef | null | undefined };

/** An inline result that can receive keyboard focus after its action goes away. */
export function StatusSummary(props: Testable & { focusable: boolean; viewRef: (node: ViewRef) => void; children?: JSX.Element }) {
  return (
    <View ref={props.viewRef} role={props.focusable ? "summary" : undefined} focusable={props.focusable}
      class={focusRing} accessibilityLabel={props.accessibilityLabel} testID={props.testID}>
      {props.children}
    </View>
  );
}

/**
 * A confirmation scoped to one row. `testID` names the trigger; the panel,
 * warning, input, Cancel, final action and busy status use `:panel`, `:warning`,
 * `:input`, `:cancel`, `:confirm` and `:busy`. For a menu trigger or a saving
 * state, control `open` and `onOpenChange`. For an external trigger, omit
 * `triggerLabel` and pass `returnFocus`, which captures the opener when
 * `open` turns true. A busy status takes focus in place of the final action.
 * Use the persistent menu button as the opener for a menu item that unmounts.
 * Clear controlled `open` when changing targets, and set it again only on
 * a new user request. Scope invalidation does not write to the parent's state.
 */
export function ConfirmInline(props: Testable & ConfirmTrigger & {
  confirmLabel: string;
  title?: string;
  message: string;
  /** The exact (case- and space-sensitive) text needed to enable the action. */
  typeToConfirm?: string;
  /** A changed target closes the panel. A controlled reopen needs `open` to go false, then true. */
  scopeKey?: string;
  onConfirm: () => void;
  /** Non-destructive actions (such as starting an update) use `primary`. */
  variant?: "destructive" | "primary";
  disabled?: boolean;
  busy?: boolean;
  busyLabel?: string;
  /** Idle close in the same scope: use this when the opener has become unavailable. */
  returnFocusOnClose?: () => ViewRef | null | undefined;
  /** Controlled close after confirming: fall back to the opener if this returns no result control. */
  returnFocusAfterBusy?: () => ViewRef | null | undefined;
}) {
  const localOpen = signal(false);
  const session = signal<{ key?: string; opener?: ViewRef | null } | null>(null);
  const typed = signal("");
  const open = () => {
    const active = session();
    return !!active && (props.open ?? localOpen()) && active.key === props.scopeKey;
  };
  const ready = () => !props.disabled && !props.busy && (props.typeToConfirm === undefined || typed() === props.typeToConfirm);
  const id = (part: string) => props.testID ? `${props.testID}:${part}` : undefined;
  let trigger: ViewRef | undefined;
  let cancelControl: ViewRef | undefined;
  let busyFocus: ReturnType<typeof session> = null;
  let submitted: ReturnType<typeof session> = null;
  let focusedWhileBusy: ReturnType<typeof session> = null;
  let mounted = true;
  cleanup(() => { mounted = false; });
  let wasRequested = false;
  const changeOpen = (next: boolean) => {
    if (props.open === undefined) localOpen.set(next);
    props.onOpenChange?.(next);
  };
  const restore = (active: NonNullable<ReturnType<typeof session>>) => {
    const opener = active.opener ?? trigger;
    const afterSubmit = submitted === active;
    const choose = afterSubmit ? props.returnFocusAfterBusy : props.returnFocusOnClose;
    if (!choose) {
      opener?.focus();
      return;
    }
    // Don't pull focus back from a different control the person moved to.
    if (props.busy || (focusedWhileBusy === active && busyFocus !== active)) return;
    const next = choose() ?? opener;
    if (!next) return;
    void next.measure().then((layout) => {
      if (layout && mounted && session() === null && props.scopeKey === active.key) next.focus();
    });
  };
  // A true prop is a request to open only on its rising edge. Retargeting
  // invalidates the session even if the parent keeps requesting it, or the
  // original target comes back before the parent clears its request.
  effect(() => {
    const requested = props.open ?? localOpen();
    const key = props.scopeKey;
    const opening = requested && !wasRequested;
    wasRequested = requested;
    const active = untracked(session);
    if (opening) {
      session.set({ key, opener: untracked(() => props.returnFocus?.() ?? trigger) });
    } else if (active && active.key !== key) {
      session.set(null);
      if (props.open === undefined) localOpen.set(false);
    } else if (active && !requested) {
      session.set(null);
      untracked(() => restore(active));
    }
  });
  effect(() => {
    session();
    props.typeToConfirm;
    typed.set("");
  });
  const close = () => {
    const active = session();
    session.set(null);
    changeOpen(false);
    if (active) restore(active);
  };
  const cancel = () => {
    if (!open() || props.busy) return;
    close();
  };
  const confirm = () => {
    if (!open() || !ready()) return;
    typed.set("");
    if (props.open === undefined) close();
    else submitted = session();
    props.onConfirm();
  };
  const variant = () => props.variant ?? "destructive";
  const unavailable = () => !!(props.disabled || props.busy);
  const focusInPanel = (node: ViewRef, busy: boolean) => {
    const active = session();
    // The menu may still be releasing focus during the mount. Focus the
    // new control after layout, and never if that open session has ended.
    void node.measure().then((layout) => {
      if (layout && active === session() && open() && !!props.busy === busy) node.focus();
    });
  };
  effect(() => {
    if (props.busy) return;
    // If the caller leaves the panel open after work settles (e.g. retry),
    // return busy focus to Cancel, not to the action that just ran.
    if (busyFocus && busyFocus === untracked(session) && cancelControl) focusInPanel(cancelControl, false);
    busyFocus = null;
  });
  const busyLabel = () => props.busyLabel ?? props.confirmLabel;
  const triggerEscape = focusedEscape(() => open() ? cancel : undefined);
  const busyEscape = focusedEscape(() => props.busy && open() ? cancel : undefined);
  return (
    <View class="min-w-0 flex-col">
      <If when={props.triggerLabel}>
        <View
          ref={(node) => (trigger = node)}
          class={cx("group self-start flex-row items-center justify-center rounded-(--radius-md) px-(--space-3)", controlHeightClass.default,
            focusRing, transitionClass, actionClass[variant() === "destructive" ? "destructive" : "secondary"], actionDisabledClass)}
          role="button"
          focusable={!unavailable()}
          disabled={unavailable()}
          accessibilityState={{ expanded: open(), disabled: unavailable() }}
          accessibilityLabel={props.accessibilityLabel ?? props.triggerLabel}
          testID={props.testID}
          onFocus={triggerEscape.onFocus}
          onBlur={triggerEscape.onBlur}
          onPress={() => {
            if (unavailable()) return;
            if (open()) cancel();
            else changeOpen(true);
          }}
        >
          <Text class={cx(textClass.sm, "font-medium", actionTextClass[variant() === "destructive" ? "destructive" : "secondary"], disabledTextClass)}>
            {props.triggerLabel}
          </Text>
        </View>
      </If>
      <If when={open()}>
        <View class={cx("min-w-0 flex-col gap-(--space-2)", props.triggerLabel && "mt-(--space-2)")} testID={id("panel")}>
          <Banner tone="warning" variant="inline" title={props.title} message={props.message} testID={id("warning")} />
          <If when={props.typeToConfirm !== undefined}>
            <FormRow label={`Type “${props.typeToConfirm}” to confirm`}>
              <Field
                value={typed()}
                onChange={typed.set}
                accessibilityLabel={`Type “${props.typeToConfirm}” to confirm`}
                testID={id("input")}
                disabled={unavailable()}
                inputRef={(node) => {
                  if (!unavailable()) focusInPanel(node, false);
                }}
                onCancel={cancel}
                // Return neither confirms nor takes the focus out of the field.
                onSubmit={() => {}}
                keepFocusOnSubmit
                verbatim
              />
            </FormRow>
          </If>
          <ActionBar>
            <Action label="Cancel" variant="ghost" testID={id("cancel")} onPress={cancel} onEscape={cancel} disabled={props.busy}
              viewRef={(node) => {
                cancelControl = node;
                if (!props.busy && (props.typeToConfirm === undefined || props.disabled)) focusInPanel(node, false);
              }} />
            <If when={props.busy} else={
              <Action label={props.confirmLabel} variant={variant() === "destructive" ? "danger" : "primary"} testID={id("confirm")}
                onPress={confirm} onEscape={cancel} disabled={!ready()} />
            }>
              <View class={cx("flex-row items-center rounded-(--radius-md) bg-muted px-(--space-3)", controlHeightClass.default, focusRing)}
                ref={(node) => focusInPanel(node, true)}
                role="summary" focusable captureKeys={["enter", "space"]}
                accessibilityLabel={busyLabel()} accessibilityState={{ busy: true }} testID={id("busy")}
                 onFocus={() => { focusedWhileBusy = busyFocus = session(); busyEscape.onFocus(); }}
                onBlur={() => { if (props.busy) busyFocus = null; busyEscape.onBlur(); }}>
                <Text class={cx(textClass.sm, "font-medium text-muted-foreground")} accessibilityElementsHidden>{busyLabel()}</Text>
              </View>
            </If>
          </ActionBar>
        </View>
      </If>
    </View>
  );
}

/** A tone-coloured rail beside a block of content (transcript entries). */
export function Rail(props: { tone: Tone }) {
  return <View class={cx("w-[3px] self-stretch rounded-full", toneSolidClass[props.tone])} />;
}

// ---------------------------------------------------------------- disclosure

/**
 * A titled section that opens and closes (advanced options, history,
 * details). The header row is the trigger: a chevron, the title, an optional
 * summary and trailing content. Its content opens with a short reveal, and
 * at once when the user asked for reduced motion (NSH's Collapsible).
 * The trigger's testID is `${testID}:toggle`.
 */
export function Disclosure(
  props: Testable & {
    title: string;
    open: boolean;
    onToggle: (open: boolean) => void;
    /** A quiet note after the title ("3 runs", "Optional"). */
    summary?: string;
    /** Content at the header's end (a badge). Keep it non-interactive. */
    trailing?: JSX.Element;
    disabled?: boolean;
    /** An authored section with a heading and a dividing rule. */
    lined?: boolean;
    children?: JSX.Element;
  },
) {
  return (
    <Collapsible
      open={props.open}
      reveal
      class="flex-col"
      testID={props.testID}
      content={<View class={cx("flex-col", props.lined ? "pt-(--space-3)" : "gap-(--space-2) pt-(--space-2)")}>{props.children}</View>}
    >
      <View
        class={cx(
          "group flex-row items-center gap-(--space-2) min-h-[32px] hover:bg-muted active:bg-muted",
          props.lined ? "border-b border-border py-(--space-1)" : "-mx-2 rounded-(--radius-sm) px-(--space-2)",
          focusRing,
          transitionClass,
          disabledClass,
        )}
        role="button"
        focusable={!props.disabled}
        disabled={props.disabled}
        onPress={() => props.onToggle(!props.open)}
        accessibilityState={{ expanded: props.open, disabled: !!props.disabled }}
        accessibilityLabel={props.accessibilityLabel ?? props.title}
        testID={props.testID ? `${props.testID}:toggle` : undefined}
      >
        <Icon name={props.open ? "chevron-down" : "chevron-right"} color="muted" size={14} />
        <Text class={cx(props.lined ? textClass.md : textClass.sm, "font-semibold text-foreground")} style={props.lined ? undefined : { numberOfLines: 1 }}>
          {props.title}
        </Text>
        <If when={props.summary}>
          <Text class={cx(textClass.xs, "text-muted-foreground")} style={{ numberOfLines: 1 }}>
            {props.summary}
          </Text>
        </If>
        <View class="flex-1" />
        {props.trailing}
      </View>
    </Collapsible>
  );
}

// ---------------------------------------------------------------- actions menu

export type ActionsMenuItem = {
  /** Unique among the items; also the item's testID, as given. */
  id: string;
  label: string;
  onPress: () => void;
  disabled?: boolean;
  /** Removes or revokes something: drawn in the danger ink, after a separator. Lead to a confirmation. */
  destructive?: boolean;
};

/**
 * Secondary actions folded into a menu ("More", "Actions"). The trigger is a
 * compact ghost button with the label and a chevron (`testID`); the items
 * open in a popover, in the order given, with a separator before a
 * destructive item that follows others. Keyboard: Return, Space or Down open
 * it on the first item, the arrows move, Escape closes (NSH's Menu).
 */
export function ActionsMenu(
  props: Testable & {
    label: string;
    items: readonly ActionsMenuItem[];
    disabled?: boolean;
  },
) {
  const rows = () =>
    props.items.map((item, i) => ({ item, separated: !!item.destructive && i > 0 && !props.items[i - 1].destructive }));
  return (
    <Menu
      placement="bottom"
      align="end"
      offset={4}
      popupClass={cx(popoverClass, "min-w-[180px]")}
      popupStartingClass={popoverMotion.starting}
      popupEndingClass={popoverMotion.ending}
      trigger={
        <Button
          class={cx(
            "group flex-row items-center gap-(--space-1) rounded-(--radius-sm) px-(--space-2) hover:bg-muted active:bg-muted aria-expanded:bg-muted",
            controlHeightClass.compact,
            focusRing,
            transitionClass,
            disabledClass,
          )}
          disabled={props.disabled}
          accessibilityLabel={props.accessibilityLabel ?? props.label}
          testID={props.testID}
        >
          <Text class={cx(textClass.xs, "font-medium text-foreground")}>{props.label}</Text>
          <Icon name="chevron-down" color="muted" size={14} />
        </Button>
      }
    >
      <Each of={rows()} key={(row) => row.item.id}>
        {(row) => (
          <>
            <If when={row().separated}>
              <MenuSeparator class="my-[4px] h-px bg-border" />
            </If>
            <MenuItem
              class={popoverItemClass}
              disabled={row().item.disabled}
              onSelect={() => row().item.onPress()}
              testID={row().item.id}
            >
              <Text class={cx(textClass.sm, row().item.destructive ? "text-destructive" : "text-popover-foreground")}>{row().item.label}</Text>
            </MenuItem>
          </>
        )}
      </Each>
    </Menu>
  );
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
 * A page that scrolls with a footer that doesn't: the footer (an ActionBar of
 * Save / Cancel) stays at the bottom of the page while the content scrolls
 * above it. Content and footer share the content width unless `wide`.
 * `inset` adds an outer gutter around the content column, and gives the
 * footer that same horizontal gutter. `pad` is inside the content column.
 * The scrolling region takes `testID`; the footer `${testID}:footer`.
 */
export function PageLayout(props: StackProps & { footer?: JSX.Element; wide?: boolean; inset?: Space }) {
  const content = <View class={cx(stackClass(props, "col", false), !props.wide && "w-full max-w-(--content-width) self-center")}>{props.children}</View>;
  return (
    <View class="flex-1 min-h-0 flex-col">
      <Scroll class="flex-1 min-h-0" testID={props.testID}>
        <If when={props.inset !== undefined} else={content}>
          <View class={cx("flex-col", padClass[props.inset ?? 0])}>{content}</View>
        </If>
      </Scroll>
      <If when={props.footer}>
        <View class={cx("border-t border-border bg-background py-(--space-3)", props.inset === undefined
          ? "px-(--space-8) max-md:px-(--space-4)" : padXClass[props.inset])} testID={props.testID ? `${props.testID}:footer` : undefined}>
          <View class={cx("w-full", !props.wide && "max-w-(--content-width) self-center")}>{props.footer}</View>
        </View>
      </If>
    </View>
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

export type ChoiceOption<T extends string> = { value: T; label: string; description?: string; disabled?: boolean; testID?: string };

/**
 * Pick exactly one of a few options (an appearance, a mode). A radio group
 * for screen readers and the keyboard (arrows move, Space checks). Each
 * option's testID is `${testID}:${value}`. 36 high, like every control; the
 * chosen option is the primary fill, the others muted text on the track.
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
              "group flex-row items-center justify-center min-w-[84px] min-h-[28px] rounded-[5px] px-(--space-3) checked:bg-primary",
              // Hover only on unchecked options: a hover background would
              // otherwise cover the checked one under the pointer.
              props.value !== option().value && "hover:bg-background/60",
              focusRing,
              transitionClass,
              disabledClass,
            )}
            checked={props.value === option().value}
            disabled={props.disabled || option().disabled}
            onChange={() => props.onChange(option().value)}
            accessibilityLabel={option().description ? `${option().label}. ${option().description}` : option().label}
            testID={option().testID ?? (props.testID ? `${props.testID}:${option().value}` : undefined)}
          >
            <Text
              class={cx(
                textClass.sm,
                "font-medium text-muted-foreground group-checked:text-primary-foreground",
                // As with the background: hover ink only on unchecked options.
                props.value !== option().value && "group-hover:text-foreground",
              )}
            >
              {option().label}
            </Text>
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
      class={cx("flex-row items-center gap-(--space-2) rounded-full", focusRing, disabledClass)}
      trackClass="w-[36px] h-[20px] rounded-full bg-control-edge p-[2px] checked:bg-primary transition-colors duration-150 ease-out motion-reduce:transition-none"
      thumbClass="size-[16px] rounded-full bg-primary-foreground checked:translate-x-4 transition-transform duration-150 ease-out motion-reduce:transition-none"
      checked={props.checked}
      disabled={props.disabled}
      onChange={({ checked }) => props.onChange(checked)}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
      testID={props.testID}
    >
      <If when={props.showLabel}>
        <Text class={cx(textClass.sm, "text-foreground")}>{props.label}</Text>
      </If>
    </Switch>
  );
}

/** A selectable option in a form, with one target for the box, label and optional hint. */
export function Checkbox(props: Testable & {
  label: string;
  hint?: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
}) {
  const label = () => props.accessibilityLabel ?? props.label;
  return (
    <BaseCheckbox
      class={cx("self-start max-w-full flex-row items-start gap-(--space-2) rounded-(--radius-sm) py-(--space-1)", controlHeightClass.compact, focusRing, disabledClass)}
      indicatorClass={cx("size-[18px] shrink-0 items-center justify-center rounded-[4px] border bg-background checked:bg-primary checked:border-primary", controlEdgeClass)}
      indicator={<If when={props.checked}><Icon name="check" color="on-primary" size={14} /></If>}
      checked={props.checked}
      disabled={props.disabled}
      onChange={({ checked }) => props.onChange(checked)}
      testID={props.testID}
      accessibilityLabel={props.hint ? `${label()}. ${props.hint}` : label()}
    >
      <View class="shrink min-w-0 flex-col" accessibilityElementsHidden>
        <Text class={cx(textClass.sm, "font-medium text-foreground")}>{props.label}</Text>
        <If when={props.hint}>
          <Text class={cx(textClass.xs, "text-muted-foreground")}>{props.hint}</Text>
        </If>
      </View>
    </BaseCheckbox>
  );
}

export type PickerItem = {
  value: string;
  label: string;
  disabled?: boolean;
  group?: string;
  /**
   * A second, muted line under the label in the list (what the option does).
   * Screen readers and search get it too; the trigger shows only the label.
   */
  detail?: string;
};

/** A drop-down choice among many (a provider, a model). Option rows' testIDs are `${testID}:${value}`. */
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
  const byValue = () => new Map(props.items.map((item) => [item.value, item]));
  const anyDetail = () => props.items.some((item) => item.detail);
  return (
    <Select
      class={cx(
        "flex-row items-center justify-between gap-(--space-2) min-w-[200px] self-start rounded-(--radius-sm) border bg-background px-(--space-3) hover:bg-muted",
        controlEdgeClass,
        controlHeightClass.default,
        focusRing,
        transitionClass,
        disabledClass,
      )}
      items={props.items.map((item) => ({
        value: item.value,
        // The title is what screen readers say and search matches: the
        // label, then the detail when there is one.
        title: item.detail ? `${item.label} — ${item.detail}` : item.label,
        disabled: item.disabled,
        group: item.group,
      }))}
      value={props.value}
      onChange={({ value }) => props.onChange(value)}
      placeholder={props.placeholder}
      searchable={props.searchable}
      disabled={props.disabled}
      accessibilityLabel={props.accessibilityLabel}
      popupClass={popoverClass}
      popupStartingClass={popoverMotion.starting}
      popupEndingClass={popoverMotion.ending}
      itemClass={cx(popoverItemClass, "selected:font-semibold")}
      groupClass="px-(--space-2) pt-(--space-2) pb-(--space-1)"
      searchClass={cx("mb-[4px] rounded-(--radius-sm) border border-input bg-background px-(--space-2) py-(--space-1) text-foreground", textClass.sm)}
      testID={props.testID}
      renderItem={
        anyDetail()
          ? (entry) => {
              const item = byValue().get(entry.value);
              return (
                <View class="flex-col gap-px" testID={props.testID ? `${props.testID}:${entry.value}` : undefined}>
                  <Text class={cx(textClass.sm, "text-popover-foreground")} style={{ numberOfLines: 1 }}>
                    {item?.label ?? entry.title}
                  </Text>
                  <If when={item?.detail}>
                    <Text class={cx(textClass.xs, "text-muted-foreground")}>{item?.detail}</Text>
                  </If>
                </View>
              );
            }
          : undefined
      }
      trigger={
        <View class="flex-1 flex-row items-center justify-between gap-(--space-2)">
          <Text class={cx(textClass.sm, current() ? "text-foreground" : "text-muted-foreground")} style={{ numberOfLines: 1 }}>
            {current() ?? props.placeholder ?? ""}
          </Text>
          <Icon name="chevron-down" color="muted" size={14} />
        </View>
      }
    />
  );
}

export type PageScope =
  | { kind: "account"; host: string }
  | { kind: "computer" }
  | { kind: "space"; personal?: boolean }
  | { kind: "organization" };

/** Whose settings these are, in fixed wording shared across pages. */
export function ScopeLine(props: Testable & { scope: PageScope }) {
  const text = () => {
    switch (props.scope.kind) {
      case "account": return `Your account on ${props.scope.host}`;
      case "computer": return "This computer only";
      case "space": return props.scope.personal ? "This Space · only you see these" : "This Space · only the owner changes it";
      case "organization": return "Organization · org admins manage it";
    }
  };
  const icons = { account: "person", computer: "device", space: "hash", organization: "server" } as const;
  return (
    <View class="min-w-0 flex-row items-start gap-(--space-1)" testID={props.testID}>
      <View class="shrink-0 pt-[2px]"><Icon name={icons[props.scope.kind]} color="muted" size={12} /></View>
      <Label variant="caption" color="muted" accessibilityLabel={props.accessibilityLabel}>{text()}</Label>
    </View>
  );
}

/** The title block at the top of a page (Settings, a Space's management). */
function useHeadingFocus(wanted: () => boolean | undefined) {
  const ref = signal<ViewRef | undefined>(undefined);
  effect(() => {
    if (!wanted()) return;
    const node = ref();
    if (!node) return;
    let live = true;
    void node.measure().then(layout => {
      if (!live || !layout || layout.bounds.size.height <= 0) return;
      node.reveal({ vertical: "start", animated: false });
      node.focus();
    });
    return () => { live = false; };
  });
  return ref.set;
}

export function PageHeader(props: Testable & { title: string; eyebrow?: string; subtitle?: string; scope?: PageScope; focusTitle?: boolean; children?: JSX.Element }) {
  const titleRef = useHeadingFocus(() => props.focusTitle);
  return (
    <View class="flex-row items-start gap-(--space-4) pb-(--space-2)" testID={props.testID}>
      <View class="flex-1 min-w-0 flex-col gap-(--space-1)">
        <If when={props.eyebrow}><Label variant="meta">{props.eyebrow}</Label></If>
        <Text class={cx(textClass["2xl"], "font-semibold tracking-tight text-foreground")} role="header"
          ref={props.focusTitle ? titleRef : undefined} focusable={props.focusTitle}>
          {props.title}
        </Text>
        <If when={props.subtitle}>
          <Text class={cx(textClass.sm, "text-muted-foreground")}>{props.subtitle}</Text>
        </If>
        <If when={props.scope}>
          {(scope) => <ScopeLine scope={scope()} testID={props.testID ? `${props.testID}:scope` : undefined} />}
        </If>
      </View>
      {props.children}
    </View>
  );
}

/**
 * A titled group of SettingRows on one card. The rows draw the lines between
 * them; the card clips the first one's. `focusTitle` is for navigation to a group.
 */
export function SettingsGroup(props: Testable & { title?: string; description?: string; focusTitle?: boolean; flat?: boolean; children?: JSX.Element }) {
  const titleRef = useHeadingFocus(() => props.focusTitle);
  return (
    <View class="flex-col gap-(--space-2)" testID={props.testID} accessibilityLabel={props.accessibilityLabel}>
      <If when={props.title || props.description}>
        <View class="flex-col gap-px px-(--space-1)">
          <If when={props.title}>
            <Text class={cx(props.flat ? textClass.md : textClass.sm, "font-semibold text-foreground")} role="header"
              ref={props.focusTitle ? titleRef : undefined} focusable={props.focusTitle}>
              {props.title}
            </Text>
          </If>
          <If when={props.description}>
            <Text class={cx(textClass.xs, "text-muted-foreground")}>{props.description}</Text>
          </If>
        </View>
      </If>
      <View class={cx("overflow-hidden", props.flat ? "border-y border-border" : "rounded-(--radius) border border-border bg-card")}>
        <View class="-mt-px flex-col">{props.children}</View>
      </View>
    </View>
  );
}

/** The name and the whole row are a single navigation target. */
export function SettingsLinkRow(props: Testable & { label: string; description?: string; mark?: string; onPress: () => void; trailing?: JSX.Element }) {
  return <Button class={cx("group flex-row items-center gap-(--space-3) min-h-[52px] border-t border-border px-(--space-2) py-(--space-2) hover:bg-muted",
    focusRing)} onPress={props.onPress} testID={props.testID} accessibilityLabel={props.accessibilityLabel ?? props.label}>
    <View class="size-[32px] shrink-0 items-center justify-center rounded-(--radius-sm) bg-muted" accessibilityElementsHidden>
      <If when={props.mark} else={<Icon name="file" size={16} color="muted" />}>
        <Text class={cx(textClass.sm, "text-foreground")} style={{ numberOfLines: 1 }}>{props.mark}</Text>
      </If>
    </View>
    <View class="flex-1 min-w-0 flex-col gap-px">
      <Text class={cx(textClass.sm, "font-medium text-foreground")} style={{ numberOfLines: 2 }}>{props.label}</Text>
      <If when={props.description}><Text class={cx(textClass.xs, "text-muted-foreground")}>{props.description}</Text></If>
    </View>
    {props.trailing}
    <Icon name="chevron-right" size={14} color="muted" />
  </Button>;
}

/**
 * One setting: its name and an optional explanation on the left, its control
 * on the right. `stacked` puts the control below (wide fields, long lists).
 */
export function SettingRow(props: Testable & { label: string; description?: string; stacked?: boolean; children?: JSX.Element }) {
  return (
    <View
      class={cx(
        "border-t border-border px-(--space-4) py-(--space-3) gap-(--space-3) min-h-[56px]",
        props.stacked ? "flex-col" : "flex-row items-center justify-between",
      )}
      testID={props.testID}
    >
      <View class="flex-1 min-w-0 flex-col gap-px">
        <Text class={cx(textClass.md, "font-medium text-card-foreground")} accessibilityLabel={props.accessibilityLabel}>
          {props.label}
        </Text>
        <If when={props.description}>
          <Text class={cx(textClass.xs, "text-muted-foreground")}>{props.description}</Text>
        </If>
      </View>
      {props.children}
    </View>
  );
}
