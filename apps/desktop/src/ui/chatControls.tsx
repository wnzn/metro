// Conversation controls: the message toolbar and its popovers, the agent's
// glyph, recipient tags, the raised composer and the empty transcript. Like
// ./primitives, feature code passes only semantic props; every class and
// raw value lives here or in theme/theme.json.
//
// Keyboard: the message toolbar, the reaction picker and the mention list
// are `nsh/base` Toolbars (WAI-ARIA's toolbar pattern): one Tab stop each,
// arrows move between items, Home and End jump, Space and Return press. A
// focused Input owns its own keys (nsh.dev/docs/forms-and-keyboard), so
// while the caret is in the composer the mention list is driven by Return
// (the composer's submit) and Tab moves into the list for the arrows.

import { Image, Overlay, Text, View, signal, token } from "nsh";
import type { Bounds, CaretEvent, InputRef, JSX, ViewRef } from "nsh";
import { Button, Input, Toolbar } from "nsh/base";
import agentIcon from "./icons/agent.svg";
import atIcon from "./icons/at.svg";
import chatIcon from "./icons/chat.svg";
import checkIcon from "./icons/check.svg";
import copyIcon from "./icons/copy.svg";
import hashIcon from "./icons/hash.svg";
import reactionIcon from "./icons/reaction.svg";
import runnerIcon from "./icons/runner.svg";
import sendIcon from "./icons/send.svg";
import serverIcon from "./icons/server.svg";
import threadIcon from "./icons/thread.svg";
import { cx, toneSurfaceClass, toneTextClass } from "./tokens";
import type { Tone } from "./tokens";

type Testable = { testID?: string; accessibilityLabel?: string };

const focusRing = "focus-visible:ring-2 focus-visible:ring-ring";

// ---------------------------------------------------------------- glyphs

const GLYPHS = {
  agent: agentIcon,
  at: atIcon,
  chat: chatIcon,
  check: checkIcon,
  copy: copyIcon,
  hash: hashIcon,
  reaction: reactionIcon,
  runner: runnerIcon,
  send: sendIcon,
  server: serverIcon,
  thread: threadIcon,
} as const;

export type ChatGlyphName = keyof typeof GLYPHS;
type GlyphColor = "default" | "muted" | "on-primary" | Tone;

const GLYPH_TOKEN = {
  default: "foreground",
  muted: "muted-foreground",
  "on-primary": "primary-foreground",
  neutral: "muted-foreground",
  agent: "tone-agent-foreground",
  runner: "tone-runner-foreground",
  system: "tone-system-foreground",
  success: "tone-success-foreground",
  warning: "tone-warning-foreground",
  danger: "tone-danger-foreground",
} as const;

/** A decorative glyph of the conversation's family; say what it means in the control's label. */
export function ChatGlyph(props: { name: ChatGlyphName; color?: GlyphColor; size?: 14 | 16 | 20 }) {
  const size = () => props.size ?? 16;
  return (
    <Image
      src={GLYPHS[props.name]}
      fadeDuration={0}
      style={{ width: size(), height: size(), tintColor: token(GLYPH_TOKEN[props.color ?? "muted"]) }}
      accessibilityElementsHidden
    />
  );
}

/**
 * The round mark of a non-person author in the transcript gutter: the agent's
 * glyph (never initials, which could pass for a person's), a runner's or the
 * server's. As large as a person's avatar so the names line up.
 */
export function AuthorGlyph(props: Testable & { kind: "agent" | "runner" | "system"; tone: Tone; size?: "sm" | "md" }) {
  const small = () => props.size === "sm";
  return (
    <View
      class={cx("shrink-0 items-center justify-center rounded-full", small() ? "size-[20px]" : "size-[32px]", toneSurfaceClass[props.tone])}
      testID={props.testID}
      accessibilityElementsHidden
    >
      <ChatGlyph
        name={props.kind === "agent" ? "agent" : props.kind === "runner" ? "runner" : "server"}
        color={props.tone}
        size={small() ? 14 : props.kind === "agent" ? 20 : 16}
      />
    </View>
  );
}

// ---------------------------------------------------------------- recipients

/**
 * One explicit recipient of a message ("@bob", "@you", "@agent", "agent ·
 * detected by System One"). `self` gives the signed-in user's accent. The
 * accessible label names the stable id where the shown name is a display
 * name.
 */
export function RecipientTag(props: Testable & { label: string; tone: Tone; self?: boolean }) {
  return (
    <View
      class={cx("rounded-(--radius-sm) px-[6px] py-px", props.self ? "bg-tone-warning" : toneSurfaceClass[props.tone])}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
    >
      <Text class={cx("text-(length:--text-xs) font-medium", props.self ? "text-tone-warning-foreground" : toneTextClass[props.tone])}>{props.label}</Text>
    </View>
  );
}

/** Quiet words in a message header ("to people", a kind of server event). */
export function HeaderNote(props: Testable & { text: string }) {
  return (
    <Text class="text-(length:--text-xs) text-muted-foreground" testID={props.testID} accessibilityLabel={props.accessibilityLabel}>
      {props.text}
    </Text>
  );
}

// ---------------------------------------------------------------- message toolbar

/**
 * The actions of one message, floating at its top right. Hidden until the
 * pointer is over the message or the keyboard focus is inside it, and always
 * shown on touch screens (`pointer-coarse`), where there is no hover; `open`
 * keeps it while one of its popovers is up. One Tab stop: arrows move
 * between its buttons.
 */
export function MessageToolbar(props: Testable & { open?: boolean; children?: JSX.Element }) {
  return (
    <View
      class={cx(
        "absolute top-[2px] right-(--space-2) opacity-0 group-hover/msg:opacity-100 group-focus-within/msg:opacity-100 pointer-coarse:opacity-100 transition-opacity",
        props.open && "opacity-100",
      )}
    >
      <Toolbar
        class="flex-row items-center gap-px rounded-(--radius-sm) border border-border bg-popover p-[2px] shadow-sm"
        accessibilityLabel={props.accessibilityLabel ?? "Message actions"}
        testID={props.testID}
      >
        {props.children}
      </Toolbar>
    </View>
  );
}

/** An icon button of a toolbar; its `accessibilityLabel` is all a screen reader hears, its `tooltip` what the pointer sees. */
export function ToolbarIconButton(
  props: Testable & {
    icon: ChatGlyphName;
    accessibilityLabel: string;
    tooltip?: string;
    onPress: () => void;
    disabled?: boolean;
    /** The popover it opens is up. */
    active?: boolean;
    ref?: (node: ViewRef) => void;
  },
) {
  return (
    <Button
      class={cx("items-center justify-center size-[28px] rounded-[5px] hover:bg-muted active:bg-muted disabled:opacity-50", focusRing, props.active && "bg-muted")}
      ref={props.ref}
      selected={props.active}
      disabled={props.disabled}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
      tooltip={props.tooltip ? <TooltipText text={props.tooltip} /> : undefined}
    >
      <ChatGlyph name={props.icon} color={props.active ? "default" : "muted"} />
    </Button>
  );
}

function TooltipText(props: { text: string }) {
  return <Text class="rounded-(--radius-sm) bg-popover border border-border px-(--space-2) py-[3px] text-(length:--text-xs) text-popover-foreground">{props.text}</Text>;
}

// ---------------------------------------------------------------- popovers

/**
 * A popup anchored to a control (a ref) or to a box in the window (an
 * input's caret). A press outside it and Escape call `onClose`; the app
 * decides. Its content is drawn above everything, whatever clips the anchor.
 */
export function AnchoredPopover(
  props: Testable & {
    anchor: ViewRef | Bounds | null | undefined;
    onClose: () => void;
    placement?: "top" | "bottom";
    align?: "start" | "center" | "end";
    role?: "menu" | "listbox" | "dialog";
    children?: JSX.Element;
  },
) {
  return (
    <Overlay
      anchor={props.anchor ?? null}
      placement={props.placement ?? "top"}
      align={props.align ?? "end"}
      offset={6}
      margin={8}
      onPressOutside={props.onClose}
      onCancel={props.onClose}
    >
      <View
        class="rounded-(--radius) border border-border bg-popover p-[4px] shadow-lg"
        role={props.role ?? "dialog"}
        testID={props.testID}
        accessibilityLabel={props.accessibilityLabel}
      >
        {props.children}
      </View>
    </Overlay>
  );
}

/** The row of emoji in the reaction popover: Left and Right move, Return or Space picks. */
export function EmojiRow(props: { accessibilityLabel: string; children?: JSX.Element }) {
  return (
    <Toolbar class="flex-row items-center gap-[2px]" accessibilityLabel={props.accessibilityLabel}>
      {props.children}
    </Toolbar>
  );
}

/** One emoji choice; `mine` marks the user's own reaction (picking it again removes it). */
export function EmojiChoice(
  props: Testable & { emoji: string; mine: boolean; onPress: () => void; ref?: (node: ViewRef) => void },
) {
  return (
    <Button
      class={cx(
        "items-center justify-center size-[34px] rounded-(--radius-sm) hover:bg-muted active:bg-muted",
        focusRing,
        props.mine && "bg-accent",
      )}
      ref={props.ref}
      selected={props.mine}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
    >
      <Text class="text-(length:--text-lg)">{props.emoji}</Text>
    </Button>
  );
}

/** A vertical list of suggestions (mentions): Up and Down move once the focus is in it, Return or Space picks. */
export function SuggestionList(props: Testable & { accessibilityLabel: string; children?: JSX.Element }) {
  return (
    <Toolbar axis="vertical" class="flex-col min-w-[260px] max-w-[360px] gap-px" accessibilityLabel={props.accessibilityLabel} testID={props.testID}>
      {props.children}
    </Toolbar>
  );
}

/**
 * One suggestion: a leading picture or glyph, a title (a display name), a
 * quiet detail (the stable id) and an optional note ("owner"). `active` is
 * the one Return picks while the caret is still in the composer.
 */
export function SuggestionOption(
  props: Testable & {
    title: string;
    detail?: string;
    note?: string;
    leading?: JSX.Element;
    active: boolean;
    onPress: () => void;
    ref?: (node: ViewRef) => void;
  },
) {
  return (
    <Button
      class={cx("flex-row items-center gap-(--space-2) rounded-(--radius-sm) px-(--space-2) py-[5px] hover:bg-muted", focusRing, props.active && "bg-accent")}
      ref={props.ref}
      role="option"
      selected={props.active}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
    >
      {props.leading}
      <View class="flex-1 min-w-0 flex-row items-baseline gap-(--space-2)">
        <Text class={cx("text-(length:--text-sm) font-medium", props.active ? "text-accent-foreground" : "text-popover-foreground")} style={{ numberOfLines: 1 }}>
          {props.title}
        </Text>
        <If when={props.detail}>
          <Text class="shrink text-(length:--text-xs) text-muted-foreground" style={{ numberOfLines: 1 }}>
            {props.detail}
          </Text>
        </If>
      </View>
      <If when={props.note}>
        <Text class="text-(length:--text-xs) text-muted-foreground">{props.note}</Text>
      </If>
    </Button>
  );
}

/** A small heading or note inside a popover ("Mention in #lobby"). */
export function PopoverNote(props: Testable & { text: string; tone?: "muted" | "danger" }) {
  return (
    <Text
      class={cx("px-(--space-2) py-[3px] text-(length:--text-xs)", props.tone === "danger" ? "text-destructive" : "text-muted-foreground")}
      testID={props.testID}
    >
      {props.text}
    </Text>
  );
}

// ---------------------------------------------------------------- composer

/**
 * The composer's raised box: what it is addressed to on top, the field, and
 * a bar of tools with the send controls at the bottom right. It lifts and
 * takes the focus ring while the field inside has the focus.
 */
export function ComposerBox(props: Testable & { children?: JSX.Element }) {
  return (
    <View
      class="flex-col rounded-(--radius-lg) border border-input bg-card shadow-md focus-within:border-ring focus-within:ring-1 focus-within:ring-ring"
      testID={props.testID}
    >
      {props.children}
    </View>
  );
}

/** The bottom bar of the composer: tools on the left, the send controls on the right. */
export function ComposerBar(props: { leading?: JSX.Element; trailing?: JSX.Element }) {
  return (
    <View class="flex-row items-center gap-(--space-2) px-(--space-2) pb-(--space-2)">
      <View class="flex-1 min-w-0 flex-row items-center gap-(--space-1)">{props.leading}</View>
      <View class="flex-row items-center gap-(--space-2)">{props.trailing}</View>
    </View>
  );
}

const FIELD_MIN = 20;
const FIELD_MAX = 200;
/** The field's vertical padding: pt-(--space-3) + pb-(--space-1). */
const FIELD_PAD = 16;

/**
 * The composer's text: several lines, growing with what is typed up to a
 * limit and scrolling past it (`onContentSizeChange`). Return submits,
 * Shift+Return (Alt+Return) adds a line. `onCaretMove` reports the caret's
 * box in the window, where an `@` mention's suggestions open.
 */
export function ComposerField(
  props: Testable & {
    value: string;
    onChange: (value: string) => void;
    onSubmit: () => void;
    placeholder?: string;
    disabled?: boolean;
    onFocusChange?: (focused: boolean) => void;
    onCaretMove?: (e: CaretEvent) => void;
    ref?: (node: InputRef) => void;
  },
) {
  // Grows with its text (one empty line's height while empty), then scrolls.
  const height = signal(FIELD_MIN);
  return (
    <Input
      class="w-full bg-transparent px-(--space-3) pt-(--space-3) pb-(--space-1) text-(length:--text-md) text-foreground placeholder:text-muted-foreground disabled:opacity-60"
      style={{ height: height() + FIELD_PAD }}
      ref={props.ref as never}
      value={props.value}
      onChange={({ value }) => props.onChange(value)}
      onSubmit={() => props.onSubmit()}
      submitBehavior="submit"
      multiline
      placeholder={props.placeholder}
      disabled={props.disabled}
      onFocus={() => props.onFocusChange?.(true)}
      onBlur={() => props.onFocusChange?.(false)}
      onCaretMove={props.onCaretMove}
      onContentSizeChange={({ size }) => height.set(Math.max(FIELD_MIN, Math.min(FIELD_MAX, Math.ceil(size.height))))}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.placeholder}
    />
  );
}

/** The one line of help under the composer, shown while it has the focus. */
export function ComposerHint(props: Testable & { text: string }) {
  return (
    <Text class="px-(--space-1) text-(length:--text-xs) text-muted-foreground" style={{ numberOfLines: 2 }} testID={props.testID}>
      {props.text}
    </Text>
  );
}

/** The quiet "To:" line of explicit recipients above the field. */
export function RecipientBar(props: Testable & { children?: JSX.Element }) {
  return (
    <View class="flex-row flex-wrap items-center gap-(--space-1) px-(--space-3) pt-(--space-2)" testID={props.testID}>
      <Text class="text-(length:--text-xs) text-muted-foreground pr-(--space-1)">To</Text>
      {props.children}
    </View>
  );
}

/** A chosen recipient in the composer; pressing it removes it. */
export function RecipientChip(props: Testable & { label: string; agent?: boolean; onRemove: () => void }) {
  return (
    <Button
      class={cx(
        "flex-row items-center gap-[4px] rounded-full px-(--space-2) py-px hover:opacity-80",
        focusRing,
        props.agent ? "bg-tone-agent" : "bg-accent",
      )}
      onPress={props.onRemove}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? `Remove ${props.label}`}
    >
      <Text class={cx("text-(length:--text-xs) font-medium", props.agent ? "text-tone-agent-foreground" : "text-accent-foreground")}>{props.label}</Text>
      <Text class={cx("text-(length:--text-xs)", props.agent ? "text-tone-agent-foreground" : "text-accent-foreground")}>×</Text>
    </Button>
  );
}

/**
 * "Ask agent": sends and explicitly asks the agent to answer. Outlined in
 * the agent's tone with its glyph, so it never reads as the plain send.
 */
export function AskAgentButton(props: Testable & { label: string; busyLabel: string; busy: boolean; disabled: boolean; onPress: () => void }) {
  return (
    <Button
      class={cx(
        "flex-row items-center gap-(--space-1) min-h-[32px] rounded-(--radius-sm) border border-tone-agent-foreground/40 px-(--space-2) hover:bg-tone-agent active:bg-tone-agent disabled:opacity-50",
        focusRing,
      )}
      disabled={props.disabled || props.busy}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
    >
      <ChatGlyph name="agent" color="agent" />
      <Text class="text-(length:--text-sm) font-medium text-tone-agent-foreground">{props.busy ? props.busyLabel : props.label}</Text>
    </Button>
  );
}

/** The plain send, at the bottom right: an arrow on the primary color. */
export function SendButton(props: Testable & { busy: boolean; disabled: boolean; onPress: () => void; tooltip?: string }) {
  return (
    <Button
      class={cx("items-center justify-center size-[32px] rounded-(--radius-sm) bg-primary hover:bg-primary/90 active:bg-primary/85 disabled:opacity-40", focusRing, props.busy && "opacity-60")}
      disabled={props.disabled || props.busy}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.busy ? "Sending…" : props.accessibilityLabel ?? "Send"}
      tooltip={props.tooltip ? <TooltipText text={props.tooltip} /> : undefined}
    >
      <ChatGlyph name="send" color="on-primary" />
    </Button>
  );
}

// ---------------------------------------------------------------- empty transcript

/** A conversation with nothing in it yet: a glyph, a line or two, and suggestions that fill the draft. */
export function EmptyConversation(props: Testable & { glyph: ChatGlyphName; title: string; text?: string; children?: JSX.Element }) {
  return (
    <View class="flex-1 flex-col items-center justify-center gap-(--space-3) p-(--space-6)" testID={props.testID}>
      <View class="size-[44px] items-center justify-center rounded-full bg-muted">
        <ChatGlyph name={props.glyph} color="muted" size={20} />
      </View>
      <Text class="text-(length:--text-md) font-semibold text-foreground" role="header">
        {props.title}
      </Text>
      <If when={props.text}>
        <Text class="max-w-[420px] text-center text-(length:--text-sm) text-muted-foreground">{props.text}</Text>
      </If>
      <View class="flex-row flex-wrap justify-center gap-(--space-2)">{props.children}</View>
    </View>
  );
}

/** A suggestion of the empty conversation: fills the draft, never sends. */
export function DraftSuggestion(props: Testable & { label: string; onPress: () => void }) {
  return (
    <Button
      class={cx("rounded-full border border-border bg-background px-(--space-3) py-[5px] hover:bg-muted active:bg-muted", focusRing)}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? `${props.label} (fills the message box)`}
    >
      <Text class="text-(length:--text-sm) text-foreground">{props.label}</Text>
    </Button>
  );
}

// ---------------------------------------------------------------- header

/** A link-sized button that sits on a caption's baseline ("Thread from # lobby"). */
export function InlineLink(props: Testable & { label: string; onPress: () => void }) {
  return (
    <Button
      class={cx("rounded-[3px] px-[2px] hover:bg-muted", focusRing)}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
    >
      <Text class="text-(length:--text-xs) font-medium text-accent-foreground">{props.label}</Text>
    </Button>
  );
}
