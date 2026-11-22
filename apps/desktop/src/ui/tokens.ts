// The design vocabulary feature code may use: semantic names only.
//
// Values live in theme/theme.json (light and dark). The class strings below are
// the single place that maps a semantic name to theme tokens; feature
// components never name colors, sizes or classes themselves. Replace this
// file and ./primitives.tsx (or just theme.json) to restyle the whole app.

export type Space = 0 | 1 | 2 | 3 | 4 | 6 | 8;

/** Semantic tones. Event kinds, statuses and banners pick one of these. */
export type Tone = "neutral" | "agent" | "runner" | "system" | "success" | "warning" | "danger";

export const gapClass: Record<Space, string> = {
  0: "",
  1: "gap-(--space-1)",
  2: "gap-(--space-2)",
  3: "gap-(--space-3)",
  4: "gap-(--space-4)",
  6: "gap-(--space-6)",
  8: "gap-(--space-8)",
};

export const padClass: Record<Space, string> = {
  0: "",
  1: "p-(--space-1)",
  2: "p-(--space-2)",
  3: "p-(--space-3)",
  4: "p-(--space-4)",
  6: "p-(--space-6)",
  8: "p-(--space-8)",
};

export const padXClass: Record<Space, string> = {
  0: "",
  1: "px-(--space-1)",
  2: "px-(--space-2)",
  3: "px-(--space-3)",
  4: "px-(--space-4)",
  6: "px-(--space-6)",
  8: "px-(--space-8)",
};

export const padYClass: Record<Space, string> = {
  0: "",
  1: "py-(--space-1)",
  2: "py-(--space-2)",
  3: "py-(--space-3)",
  4: "py-(--space-4)",
  6: "py-(--space-6)",
  8: "py-(--space-8)",
};

export const toneSurfaceClass: Record<Tone, string> = {
  neutral: "bg-tone-neutral",
  agent: "bg-tone-agent",
  runner: "bg-tone-runner",
  system: "bg-tone-system",
  success: "bg-tone-success",
  warning: "bg-tone-warning",
  danger: "bg-tone-danger",
};

export const toneTextClass: Record<Tone, string> = {
  neutral: "text-tone-neutral-foreground",
  agent: "text-tone-agent-foreground",
  runner: "text-tone-runner-foreground",
  system: "text-tone-system-foreground",
  success: "text-tone-success-foreground",
  warning: "text-tone-warning-foreground",
  danger: "text-tone-danger-foreground",
};

/** A tone's accent as a solid fill (status dots, rails). */
export const toneSolidClass: Record<Tone, string> = {
  neutral: "bg-muted-foreground",
  agent: "bg-tone-agent-foreground",
  runner: "bg-tone-runner-foreground",
  system: "bg-tone-system-foreground",
  success: "bg-tone-success-foreground",
  warning: "bg-tone-warning-foreground",
  danger: "bg-tone-danger-foreground",
};

/** A tone's quiet outline (inline banners). */
export const toneBorderClass: Record<Tone, string> = {
  neutral: "border-border",
  agent: "border-tone-agent-foreground/25",
  runner: "border-tone-runner-foreground/25",
  system: "border-border",
  success: "border-tone-success-foreground/25",
  warning: "border-tone-warning-foreground/25",
  danger: "border-tone-danger-foreground/25",
};

export const cx = (...parts: (string | false | null | undefined)[]) => parts.filter(Boolean).join(" ");

// ------------------------------------------------------------ shared scales
//
// The class strings every ui module (primitives, chrome, chat, media) shares,
// so controls look and behave alike. Values come from theme/theme.json.

/** Type scale: each size with its line height (`text-*` + `leading-*`). */
export const textClass = {
  xs: "text-(length:--text-xs) leading-(--leading-xs)",
  sm: "text-(length:--text-sm) leading-(--leading-sm)",
  md: "text-(length:--text-md) leading-(--leading-md)",
  lg: "text-(length:--text-lg) leading-(--leading-lg)",
  xl: "text-(length:--text-xl) leading-(--leading-xl)",
  "2xl": "text-(length:--text-2xl) leading-(--leading-2xl)",
} as const;

/** Radii: 8 controls, 12 cards, 16 popovers and dialogs; pills are full. */
export const radiusClass = {
  control: "rounded-(--radius-sm)",
  card: "rounded-(--radius)",
  popover: "rounded-(--radius-lg)",
  dialog: "rounded-(--radius-lg)",
  pill: "rounded-full",
} as const;

/** Control heights: 36 by default, 28 compact. */
export const controlHeightClass = {
  default: "min-h-(--control-height)",
  compact: "min-h-(--control-height-sm)",
} as const;

/**
 * Color changes on hover, press and selection: 150 ms, ease-out, none when
 * the user asked for reduced motion (NSH's `motion-reduce:` variant).
 */
export const transitionClass = "transition-colors duration-150 ease-out motion-reduce:transition-none";

/**
 * A popup's entrance and exit (menus, pickers): 150 ms ease-out, a fade with
 * a small 0.98 → 1 scale. `transition` animates opacity and transform
 * together. With reduced motion NSH skips the starting and ending looks
 * (nsh-base popup.rs), and `motion-reduce:transition-none` also stops any
 * transition, so the popup simply appears and goes.
 */
export const popoverMotion = {
  base: "transition duration-150 ease-out motion-reduce:transition-none",
  starting: "opacity-0 scale-[0.98]",
  ending: "opacity-0 scale-[0.98]",
} as const;

/**
 * The edge of a standalone control the user must find (a text field, a
 * picker, a switch's off track): at least 3:1 against the page, cards and
 * popovers (WCAG 1.4.11). `border` stays the soft separator between rows.
 */
export const controlEdgeClass = "border-control-edge";

/** The keyboard focus ring every interactive element shares. */
export const focusRingClass = "focus-visible:ring-2 focus-visible:ring-ring";

/**
 * Disabled, for controls without a fill of their own (fields, toggles,
 * segmented controls, chips, pickers, icon buttons): dimmed as a whole.
 * Filled and outlined Actions instead turn neutral (see primitives' Action).
 */
export const disabledClass = "disabled:opacity-50";

/**
 * Disabled, for Actions of every variant: the same muted surface and muted
 * text, never a dimmed copy of the variant (pair with disabledTextClass).
 */
export const actionDisabledClass = "disabled:bg-muted disabled:border-transparent";

/** A text inside a `group` control that turns muted while the control is disabled. */
export const disabledTextClass = "group-disabled:text-muted-foreground";

/** A floating surface one step above cards: menus, pickers, tooltips. */
export const popoverClass = cx("rounded-(--radius-lg) border border-border bg-popover p-[6px] shadow-lg", popoverMotion.base);

/** One row of a popover list (a menu item, a picker option). */
export const popoverItemClass =
  "rounded-(--radius-sm) px-(--space-2) py-[7px] text-(length:--text-sm) leading-(--leading-sm) text-popover-foreground hover:bg-muted focus:bg-accent disabled:opacity-50 transition-colors duration-150 ease-out motion-reduce:transition-none";

/** A tooltip's bubble (also a popover step). */
export const tooltipClass =
  "rounded-(--radius-sm) bg-popover border border-border px-(--space-2) py-[3px] text-(length:--text-xs) leading-(--leading-xs) text-popover-foreground shadow-md";
