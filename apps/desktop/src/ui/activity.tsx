// Primitives for agent activity and the Chat: the live thinking indicator, a
// step's phase icon, the chevron disclosure, bounded code output, an empty
// state and suggested drafts. (Run actions use the shared `Action`: compact,
// `destructive` for Stop.) Like ./primitives, feature code passes only
// semantic props; every class, color and motion decision lives here.
//
// Motion is native (`createMotion`). When the user asks for less motion
// (`reduceMotion`) the dots are drawn still and no motion is created.

import { Image, Text, View, cleanup, createMotion, reduceMotion, token } from "nsh";
import type { ImageAsset, JSX, Tokens } from "nsh";
import { Button } from "nsh/base";
import chatIcon from "./icons/chat.svg";
import checkIcon from "./icons/check.svg";
import chevronDownIcon from "./icons/chevron-down.svg";
import chevronRightIcon from "./icons/chevron-right.svg";
import clockIcon from "./icons/clock.svg";
import closeIcon from "./icons/close.svg";
import copyIcon from "./icons/copy.svg";
import runnerIcon from "./icons/runner.svg";
import { cx } from "./tokens";
import type { Tone } from "./tokens";

const focusRing = "focus-visible:ring-2 focus-visible:ring-ring";

/** A tone's foreground as a theme token, for tinting icons. */
const toneToken = (tone: Tone): keyof Tokens => (tone === "neutral" ? "muted-foreground" : (`tone-${tone}-foreground` as keyof Tokens));

function Glyph(props: { src: ImageAsset; tint: keyof Tokens; size?: number }) {
  const size = () => props.size ?? 14;
  return <Image src={props.src} fadeDuration={0} style={{ width: size(), height: size(), tintColor: String(token(props.tint)) }} accessibilityElementsHidden />;
}

// ---------------------------------------------------------------- thinking

const dotClass = (tone: Tone) => cx("size-[6px] rounded-full", tone === "runner" ? "bg-tone-runner-foreground" : "bg-tone-agent-foreground");

function Dot(props: { delay: number; tone: Tone; testID?: string }) {
  const motion = createMotion();
  void motion.play({ keyframes: { opacity: [0.25, 1, 0.25], scale: [0.8, 1, 0.8] }, duration: 1200, delay: props.delay, iterations: "infinite", easing: "easeInOut" });
  cleanup(() => motion.stop());
  return <View class={dotClass(props.tone)} style={motion.style} testID={props.testID} />;
}

/**
 * Three dots that pulse while an agent or runner works. Decorative: the label
 * beside it ("Metro is thinking…") is what screen readers hear. When the user
 * asks for less motion the dots are drawn still (`<testID>:still`), and no
 * motion is created at all.
 */
export function ThinkingIndicator(props: { tone?: "agent" | "runner"; testID?: string }) {
  const tone = () => props.tone ?? "agent";
  return (
    <If
      when={!reduceMotion()}
      else={
        <View class="flex-row items-center gap-[3px] px-[2px]" accessibilityElementsHidden testID={props.testID ? `${props.testID}:still` : undefined}>
          <View class={cx(dotClass(tone()), "opacity-60")} />
          <View class={cx(dotClass(tone()), "opacity-80")} />
          <View class={dotClass(tone())} />
        </View>
      }
    >
      <View class="flex-row items-center gap-[3px] px-[2px]" accessibilityElementsHidden testID={props.testID ? `${props.testID}:moving` : undefined}>
        <Each of={[0, 1, 2]} key={(i) => String(i)}>
          {(i) => <Dot delay={i() * 160} tone={tone()} />}
        </Each>
      </View>
    </If>
  );
}

// ---------------------------------------------------------------- steps

/** Where a step stands: a tool called, finished or failed; a thought; progress; stopped. */
export type StepPhase = "called" | "complete" | "failed" | "thinking" | "progress" | "stopped";

const PHASE: Record<StepPhase, { icon: ImageAsset; tone: Tone }> = {
  called: { icon: runnerIcon, tone: "runner" },
  complete: { icon: checkIcon, tone: "success" },
  failed: { icon: closeIcon, tone: "danger" },
  thinking: { icon: chatIcon, tone: "agent" },
  progress: { icon: clockIcon, tone: "agent" },
  stopped: { icon: closeIcon, tone: "warning" },
};

/** A step's phase as a small tinted icon in a round tile. Decorative: say the phase in the step's text. */
export function StepIcon(props: { phase: StepPhase; testID?: string }) {
  const p = () => PHASE[props.phase];
  return (
    <View
      class={cx("size-[20px] items-center justify-center rounded-full", {
        neutral: "bg-tone-neutral",
        agent: "bg-tone-agent",
        runner: "bg-tone-runner",
        system: "bg-tone-system",
        success: "bg-tone-success",
        warning: "bg-tone-warning",
        danger: "bg-tone-danger",
      }[p().tone])}
      accessibilityElementsHidden
      testID={props.testID}
    >
      <Glyph src={p().icon} tint={toneToken(p().tone)} size={12} />
    </View>
  );
}

// ---------------------------------------------------------------- disclosure

/**
 * A press target that opens and closes details: a chevron (right closed, down
 * open) and the label. Space and Return press it. Say the state in
 * `accessibilityLabel` ("Show details: …" / "Hide details: …").
 */
export function DisclosureToggle(props: { label: string; expanded: boolean; onPress: () => void; accessibilityLabel?: string; testID?: string; children?: JSX.Element }) {
  return (
    <Button
      class={cx("flex-row items-center gap-(--space-2) min-h-[28px] rounded-(--radius-sm) pl-(--space-1) pr-(--space-2) hover:bg-muted active:bg-muted", focusRing)}
      onPress={props.onPress}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
      testID={props.testID}
    >
      <Glyph src={props.expanded ? chevronDownIcon : chevronRightIcon} tint="muted-foreground" />
      <Text class="text-(length:--text-sm) font-medium text-foreground">{props.label}</Text>
      {props.children}
    </Button>
  );
}

// ---------------------------------------------------------------- code output

/**
 * Monospace output in a quiet box: folded to a few lines with "Show all N
 * lines", and a Copy control. The feature decides what is shown and copies;
 * this only draws it. `notice` explains a bounded view (e.g. only the first
 * lines are drawn).
 */
export function CodeOutput(props: {
  text: string;
  /** Folded to this many lines; unset shows every line given. */
  lines?: number;
  /** The fold control's label, or null when there is nothing to fold. */
  toggleLabel: string | null;
  onToggle: () => void;
  copyLabel: string;
  onCopy: () => void;
  notice?: string | null;
  testID?: string;
}) {
  return (
    <View class="flex-col overflow-hidden rounded-(--radius-sm) border border-border bg-muted/50" testID={props.testID}>
      <View class="px-(--space-3) py-(--space-2)" selectable>
        <Text class="text-(length:--text-sm) font-mono text-foreground" style={props.lines ? { numberOfLines: props.lines } : undefined}>
          {props.text}
        </Text>
      </View>
      <View class="flex-row items-center gap-(--space-2) border-t border-border px-(--space-1) py-[2px]">
        <If when={props.toggleLabel}>
          {(label) => (
            <Button class={cx("rounded-(--radius-sm) px-(--space-2) py-[3px] hover:bg-muted active:bg-muted", focusRing)} onPress={props.onToggle} accessibilityLabel={label()} testID={props.testID ? `${props.testID}:fold` : undefined}>
              <Text class="text-(length:--text-xs) font-medium text-foreground">{label()}</Text>
            </Button>
          )}
        </If>
        <If when={props.notice}>
          {(notice) => <Text class="flex-1 min-w-0 text-(length:--text-xs) text-muted-foreground">{notice()}</Text>}
        </If>
        <View class="flex-1" />
        <Button
          class={cx("flex-row items-center gap-(--space-1) rounded-(--radius-sm) px-(--space-2) py-[3px] hover:bg-muted active:bg-muted", focusRing)}
          onPress={props.onCopy}
          accessibilityLabel={props.copyLabel}
          testID={props.testID ? `${props.testID}:copy` : undefined}
        >
          <Glyph src={copyIcon} tint="muted-foreground" size={12} />
          <Text class="text-(length:--text-xs) text-muted-foreground">{props.copyLabel}</Text>
        </Button>
      </View>
    </View>
  );
}

// ---------------------------------------------------------------- empty state

/** A centred empty state: an icon, a title, a short line, then any actions. */
export function EmptyState(props: { title: string; text?: string; testID?: string; children?: JSX.Element }) {
  return (
    <View class="flex-1 flex-col items-center justify-center gap-(--space-3) px-(--space-6) py-(--space-6)" testID={props.testID}>
      <View class="size-[44px] items-center justify-center rounded-full bg-tone-agent" accessibilityElementsHidden>
        <Glyph src={chatIcon} tint="tone-agent-foreground" size={20} />
      </View>
      <View class="flex-col items-center gap-(--space-1) max-w-[420px]">
        <Text class="text-(length:--text-md) font-semibold text-foreground" role="header">
          {props.title}
        </Text>
        <If when={props.text}>
          <Text class="text-(length:--text-sm) text-muted-foreground text-center">{props.text}</Text>
        </If>
      </View>
      {props.children}
    </View>
  );
}

/** A suggested draft: pressing it only fills the composer, never sends. */
export function SuggestionChip(props: { label: string; onPress: () => void; disabled?: boolean; testID?: string; accessibilityLabel?: string }) {
  return (
    <Button
      class={cx("rounded-(--radius) border border-border bg-background px-(--space-3) py-(--space-2) hover:bg-muted active:bg-muted disabled:opacity-50", focusRing)}
      disabled={props.disabled}
      onPress={props.onPress}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
      testID={props.testID}
    >
      <Text class="text-(length:--text-sm) text-foreground">{props.label}</Text>
    </Button>
  );
}
