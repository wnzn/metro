// The design vocabulary feature code may use: semantic names only.
//
// Values live in theme/theme.json (light and dark). The class strings below are
// the single place that maps a semantic name to theme tokens; feature
// components never name colors, sizes or classes themselves. Replace this
// file and ./primitives.tsx (or just theme.json) to restyle the whole app.

export type Space = 0 | 1 | 2 | 3 | 4 | 6;

/** Semantic tones. Event kinds, statuses and banners pick one of these. */
export type Tone = "neutral" | "agent" | "runner" | "system" | "success" | "warning" | "danger";

export const gapClass: Record<Space, string> = {
  0: "",
  1: "gap-(--space-1)",
  2: "gap-(--space-2)",
  3: "gap-(--space-3)",
  4: "gap-(--space-4)",
  6: "gap-(--space-6)",
};

export const padClass: Record<Space, string> = {
  0: "",
  1: "p-(--space-1)",
  2: "p-(--space-2)",
  3: "p-(--space-3)",
  4: "p-(--space-4)",
  6: "p-(--space-6)",
};

export const padXClass: Record<Space, string> = {
  0: "",
  1: "px-(--space-1)",
  2: "px-(--space-2)",
  3: "px-(--space-3)",
  4: "px-(--space-4)",
  6: "px-(--space-6)",
};

export const padYClass: Record<Space, string> = {
  0: "",
  1: "py-(--space-1)",
  2: "py-(--space-2)",
  3: "py-(--space-3)",
  4: "py-(--space-4)",
  6: "py-(--space-6)",
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

export const cx = (...parts: (string | false | null | undefined)[]) => parts.filter(Boolean).join(" ");
