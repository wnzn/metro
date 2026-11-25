// Chat and profile primitives: a person's picture (or initials), the
// transcript's message frame and day separator, and a timestamp. Like
// ./primitives, feature code passes only semantic props; every class and
// color decision lives here or in theme/theme.json.
//
// A picture's `src` is a `blob:` URL the feature made from bytes it fetched
// with the user's credential, never a remote URL.

import { Image, Text, View } from "nsh";
import type { JSX } from "nsh";
import { cx, toneSurfaceClass, toneTextClass } from "./tokens";
import type { Tone } from "./tokens";

type Testable = { testID?: string; accessibilityLabel?: string };

export type AvatarSize = "sm" | "md" | "lg";

const avatarBox: Record<AvatarSize, string> = {
  sm: "size-[20px]",
  md: "size-[32px]",
  lg: "size-[72px]",
};
const avatarText: Record<AvatarSize, string> = {
  sm: "text-[9px]",
  md: "text-(length:--text-xs)",
  lg: "text-(length:--text-xl)",
};

/**
 * A round picture, or the initials on a tone when there is none (or it
 * failed to load). Decorative unless it has an `accessibilityLabel`.
 */
export function Avatar(
  props: Testable & {
    initials: string;
    tone: Tone;
    src?: string | null;
    size?: AvatarSize;
    onImageError?: () => void;
  },
) {
  const size = () => props.size ?? "md";
  return (
    <View
      class={cx("shrink-0 items-center justify-center overflow-hidden rounded-full", avatarBox[size()], toneSurfaceClass[props.tone])}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
      accessibilityElementsHidden={props.accessibilityLabel ? undefined : true}
      role={props.accessibilityLabel ? "image" : undefined}
    >
      <If
        when={props.src}
        else={<Text class={cx("font-semibold", avatarText[size()], toneTextClass[props.tone])}>{props.initials}</Text>}
      >
        {(src) => (
          <Image
            src={src()}
            class="w-full h-full"
            style={{ objectFit: "cover" }}
            testID={props.testID ? `${props.testID}:image` : undefined}
            onError={() => props.onImageError?.()}
          />
        )}
      </If>
    </View>
  );
}

/**
 * One transcript entry: a narrow gutter (a picture, or a continuation's
 * time) and the content beside it. `highlight` marks a message addressed to
 * the signed-in user with an accent rail and a faint tint. `toolbar` floats
 * at the top right on hover or keyboard focus (always on touch screens; see
 * MessageToolbar). The row tints while the pointer is over it.
 */
export function MessageFrame(
  props: Testable & {
    gutter?: JSX.Element;
    /** The message's actions (a MessageToolbar). */
    toolbar?: JSX.Element;
    /** Small controls at the entry's top right, in the flow. */
    trailing?: JSX.Element;
    highlight?: boolean;
    /** Follows the previous entry by the same author: tighter spacing. */
    continued?: boolean;
    selectable?: boolean;
    children?: JSX.Element;
  },
) {
  return (
    <View
      class={cx(
        "group/msg relative flex-row gap-(--space-3) rounded-(--radius-sm) px-(--space-2) hover:bg-muted/50 focus-within:bg-muted/50",
        props.continued ? "py-px" : "pt-(--space-2) pb-px",
        props.highlight && "bg-tone-warning/45 hover:bg-tone-warning/70",
      )}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
    >
      <If when={props.highlight}>
        <View class="absolute left-0 top-[3px] bottom-[3px] w-[3px] rounded-full bg-tone-warning-foreground" accessibilityElementsHidden />
      </If>
      {/* The picture's top on the author line's; the time of a follow-up on its first line. */}
      <View class="w-[52px] shrink-0 self-start items-center">{props.gutter}</View>
      <View class="flex-1 min-w-0 flex-col gap-[2px]" selectable={props.selectable}>
        {props.children}
      </View>
      <If when={props.trailing}>
        <View class="shrink-0 items-end">{props.trailing}</View>
      </If>
      {props.toolbar}
    </View>
  );
}

/** The author line of a message: name, markers and time on one baseline. */
export function MessageHeader(props: Testable & { children?: JSX.Element; slim?: boolean }) {
  // `slim`: a follow-up's recipients alone, as high as the time in the gutter beside it.
  return (
    <View class={cx("flex-row flex-wrap items-center gap-x-(--space-2) gap-y-px min-w-0", props.slim && "min-h-[16px] pt-px")} testID={props.testID}>
      {props.children}
    </View>
  );
}

/**
 * An author's name in the transcript; `agent` gives the agent's tone.
 * `detail` (a person's stable id) shows on hover and is read after the name.
 */
export function AuthorName(props: Testable & { name: string; agent?: boolean; detail?: string }) {
  return (
    <Text
      class={cx("text-(length:--text-md) font-semibold", props.agent ? "text-tone-agent-foreground" : "text-foreground")}
      style={{ numberOfLines: 1 }}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? (props.detail ? `${props.name}, ${props.detail}` : undefined)}
      tooltip={props.detail ? <Text class="rounded-(--radius-sm) bg-popover border border-border px-(--space-2) py-[3px] text-(length:--text-xs) text-popover-foreground">{props.detail}</Text> : undefined}
    >
      {props.name}
    </Text>
  );
}

/** A quiet marker after an author's name ("you"): plain muted words, never a pill. */
export function AuthorTag(props: Testable & { label: string; tone?: Tone }) {
  return (
    <Text class="text-(length:--text-xs) text-muted-foreground" testID={props.testID} accessibilityLabel={props.accessibilityLabel}>
      {props.label}
    </Text>
  );
}

/** A time in the transcript: short on screen, the full date, time and zone for screen readers and on hover. */
export function Timestamp(props: Testable & { text: string; full: string; compact?: boolean }) {
  // A continuation's time (compact, in the gutter) stays quiet until the
  // pointer or the keyboard focus is on its message; touch screens show it.
  return (
    <Text
      class={cx(
        "text-muted-foreground",
        props.compact
          ? "text-[10px] pt-[3px] opacity-0 group-hover/msg:opacity-100 group-focus-within/msg:opacity-100 pointer-coarse:opacity-100 motion-safe:transition-opacity"
          : "text-(length:--text-xs)",
      )}
      style={{ numberOfLines: 1 }}
      accessibilityLabel={props.full}
      testID={props.testID}
      tooltip={<Text class="rounded-(--radius-sm) bg-popover border border-border px-(--space-2) py-[3px] text-(length:--text-xs) text-popover-foreground">{props.full}</Text>}
    >
      {props.text}
    </Text>
  );
}

/** A line across the transcript with the day's date in the middle. */
export function DaySeparator(props: Testable & { label: string }) {
  return (
    <View class="flex-row items-center gap-(--space-3) px-(--space-2) pt-(--space-3) pb-(--space-1)" testID={props.testID}>
      <View class="flex-1 h-px bg-border" />
      <Text class="text-(length:--text-xs) font-medium text-muted-foreground" role="header">
        {props.label}
      </Text>
      <View class="flex-1 h-px bg-border" />
    </View>
  );
}

export * from "./chatControls";
