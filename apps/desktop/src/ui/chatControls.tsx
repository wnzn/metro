// Conversation controls: the message toolbar and its popovers, the agent's
// glyph, recipient tags, the raised composer and the empty transcript. Like
// ./primitives, feature code passes only semantic props; every class and
// raw value lives here or in theme/theme.json.
//
// Keyboard: the message toolbar, the reaction picker and the mention list
// are `nsh/base` Toolbars (WAI-ARIA's toolbar pattern): one Tab stop each,
// arrows move between items, Home and End jump, Space and Return press. A
// focused Input owns its own keys (nsh.dev/docs/forms-and-keyboard), so
// while the caret is in the composer and the mention list is open, the
// feature binds Up and Down (move the highlight, wrapping), Tab (insert it)
// with app keymaps that win over the field, and Return (the composer's
// submit) inserts instead of sending; Escape closes the list.

import { Image, Overlay, Scroll, Text, View, signal, token } from "nsh";
import type { Bounds, CaretEvent, InputRef, JSX, ViewRef } from "nsh";
import { Button, Input, Menu, MenuItem, MenuSeparator, Toolbar } from "nsh/base";
import { Icon } from "./chrome";
import agentIcon from "./icons/agent.svg";
import atIcon from "./icons/at.svg";
import attachIcon from "./icons/attach.svg";
import chatIcon from "./icons/chat.svg";
import checkIcon from "./icons/check.svg";
import copyIcon from "./icons/copy.svg";
import hashIcon from "./icons/hash.svg";
import reactionIcon from "./icons/reaction.svg";
import runnerIcon from "./icons/runner.svg";
import sendIcon from "./icons/send.svg";
import serverIcon from "./icons/server.svg";
import settingsIcon from "./icons/settings.svg";
import threadIcon from "./icons/thread.svg";
import { MediaGlyphIcon } from "./library";
import type { FileKind } from "./library";
import { cx, popoverClass, popoverItemClass, popoverMotion, toneSurfaceClass, toneTextClass } from "./tokens";
import type { Tone } from "./tokens";

type Testable = { testID?: string; accessibilityLabel?: string };

const focusRing = "focus-visible:ring-2 focus-visible:ring-ring";

// ---------------------------------------------------------------- glyphs

const GLYPHS = {
  agent: agentIcon,
  at: atIcon,
  attach: attachIcon,
  chat: chatIcon,
  check: checkIcon,
  copy: copyIcon,
  hash: hashIcon,
  reaction: reactionIcon,
  runner: runnerIcon,
  send: sendIcon,
  server: serverIcon,
  settings: settingsIcon,
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
export function RecipientTag(props: Testable & { label: string; tone: Tone; self?: boolean; slim?: boolean }) {
  // `slim`: plain words in the tone's ink (a follow-up message's quiet header).
  return (
    <View
      class={cx(props.slim ? "" : "rounded-(--radius-sm) px-[6px] py-px", props.slim ? "" : props.self ? "bg-tone-warning" : toneSurfaceClass[props.tone])}
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
        "absolute top-[2px] right-(--space-2) opacity-0 group-hover/msg:opacity-100 group-focus-within/msg:opacity-100 pointer-coarse:opacity-100 motion-safe:transition-opacity",
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
    bounded?: boolean;
    children?: JSX.Element;
  },
) {
  const available = signal(192);
  return (
    <Overlay
      anchor={props.anchor ?? null}
      placement={props.placement ?? "top"}
      align={props.align ?? "end"}
      offset={6}
      margin={8}
      onPosition={props.bounded ? (position) => available.set(position.available.height) : undefined}
      onPressOutside={props.onClose}
      onCancel={props.onClose}
    >
      <View
        class="min-h-0 rounded-(--radius) border border-border bg-popover p-[4px] shadow-lg"
        style={props.bounded ? { maxHeight: Math.min(248, available()) } : undefined}
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

/** A vertical list of suggestions (mentions): Up and Down move once the focus is in it, Return or Space picks; the composer drives it while the caret stays there. */
export function SuggestionList(props: Testable & { accessibilityLabel: string; children?: JSX.Element }) {
  return (
    <Scroll class="shrink min-h-0 max-h-[192px] w-[280px] max-w-full" testID={props.testID}>
      <Toolbar axis="vertical" class="self-stretch flex-col gap-px" accessibilityLabel={props.accessibilityLabel}>
        {props.children}
      </Toolbar>
    </Scroll>
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
    focusable?: boolean;
    ref?: (node: ViewRef) => void;
  },
) {
  const rowClass = () => cx("flex-row items-center gap-(--space-2) rounded-(--radius-sm) px-(--space-2) py-[5px] hover:bg-muted", focusRing, props.active && "bg-accent");
  const content = () => (
    <>
      {props.leading}
      <View class="flex-1 min-w-0 flex-col">
        <Text class={cx("text-(length:--text-sm) font-medium", props.active ? "text-accent-foreground" : "text-popover-foreground")} style={{ numberOfLines: 1 }}>
          {props.title}
        </Text>
        <If when={props.detail}>
          <Text class="shrink text-(length:--text-xs) text-muted-foreground" style={{ numberOfLines: 2 }}
            testID={props.testID ? `${props.testID}:detail` : undefined}>
            {props.detail}
          </Text>
        </If>
      </View>
      <If when={props.note}>
        <Text class="text-(length:--text-xs) text-muted-foreground">{props.note}</Text>
      </If>
    </>
  );
  return (
    <If when={props.focusable === false} else={
      <Button class={rowClass()} ref={props.ref} role="option" selected={props.active} onPress={props.onPress}
        testID={props.testID} accessibilityLabel={props.accessibilityLabel}>{content()}</Button>
    }>
      <View class={rowClass()} ref={props.ref} role="option" focusable={false} onPress={props.onPress}
        accessibilityState={{ selected: props.active }} testID={props.testID} accessibilityLabel={props.accessibilityLabel}>{content()}</View>
    </If>
  );
}

/** A small heading or note inside a popover ("Mention in #lobby"). */
export function PopoverNote(props: Testable & { text: string; tone?: "muted" | "danger"; lines?: number }) {
  return (
    <Text
      class={cx("px-(--space-2) py-[3px] text-(length:--text-xs)", props.tone === "danger" ? "text-destructive" : "text-muted-foreground")}
      style={{ numberOfLines: props.lines }}
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
      <View class="shrink-0 flex-row items-center gap-(--space-2)">{props.trailing}</View>
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
    onSelectionChange?: (selection: { start: number; end: number }) => void;
    ref?: (node: InputRef) => void;
  },
) {
  // Grows with its text (one empty line's height while empty), then scrolls.
  const height = signal(FIELD_MIN);
  const focused = signal(false);
  return (
    <>
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
        onFocus={() => { focused.set(true); props.onFocusChange?.(true); }}
        onBlur={() => { focused.set(false); props.onFocusChange?.(false); }}
        onCaretMove={props.onCaretMove}
        onSelectionChange={props.onSelectionChange}
        onContentSizeChange={({ size }) => height.set(Math.max(FIELD_MIN, Math.min(FIELD_MAX, Math.ceil(size.height))))}
        testID={props.testID}
        accessibilityLabel={props.accessibilityLabel ?? props.placeholder}
      />
      <If when={focused()}>
        <Text class="px-(--space-3) pb-(--space-1) text-(length:--text-xs) text-muted-foreground" testID="composer-markdown-hint">
          Markdown supported
        </Text>
      </If>
    </>
  );
}

/** The one line of help under the composer, shown while it has the focus. */
export function ComposerHint(props: Testable & { text: string }) {
  return (
    <Text
      class="flex-1 shrink min-w-0 px-(--space-1) text-(length:--text-xs) text-muted-foreground"
      style={{ numberOfLines: 2 }}
      accessibilityLabel={props.text}
      testID={props.testID}
    >
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
 * The send controls share one look while they can't act (disabled or busy):
 * a muted surface with muted glyph and text, never a dimmed brand fill, in
 * both themes. 36px tall, with the shared focus ring (ring on background is
 * checked for 3:1 in theme.test).
 */
const sendInactive = "bg-muted border border-border";

/**
 * "Ask agent": a rounded chip in the agent's tone with its glyph. Pressing it
 * sends and explicitly asks the agent; it never reads as the plain send.
 */
export function AskAgentButton(
  props: Testable & {
    primary?: boolean;
    label: string;
    busyLabel: string;
    busy: boolean;
    disabled: boolean;
    onPress: () => void;
    tooltip?: string;
    /** An agent chooser joined to its end (AskAgentChooser): the two read as one split button. */
    chooser?: JSX.Element;
    /** The chosen agent's picture in place of the agent glyph. */
    picture?: JSX.Element;
  },
) {
  const inactive = () => props.disabled || props.busy;
  const button = (
    <Button
      class={cx(
        "flex-row items-center gap-(--space-1) h-[36px] px-(--space-3)",
        props.chooser ? "rounded-l-full pr-(--space-2)" : "rounded-full",
        focusRing,
        inactive() ? sendInactive : props.primary ? "bg-primary hover:bg-primary/90" : "bg-tone-agent border border-tone-agent-foreground/40 hover:border-tone-agent-foreground active:opacity-90",
      )}
      disabled={inactive()}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
      tooltip={props.tooltip ? <TooltipText text={props.tooltip} /> : undefined}
    >
      <If when={props.picture} else={<ChatGlyph name="agent" color={inactive() ? "muted" : props.primary ? "on-primary" : "agent"} />}>
        {props.picture}
      </If>
      <Text class={cx("text-(length:--text-sm) font-medium", inactive() ? "text-muted-foreground" : props.primary ? "text-primary-foreground" : "text-tone-agent-foreground")} style={{ numberOfLines: 1 }}>
        {props.busy ? props.busyLabel : props.label}
      </Text>
    </Button>
  );
  return (
    <If when={props.chooser} else={button}>
      <View class="flex-row items-center">
        {button}
        {props.chooser}
      </View>
    </If>
  );
}

/** One choice of the Ask-agent chooser: an agent (its picture in `leading`), or "Let the classifier choose". */
export type AskAgentChoice = { id: string; title: string; detail?: string; leading?: JSX.Element; checked: boolean; accessibilityLabel: string; onSelect: () => void; separated?: boolean };

/**
 * The chevron end of the Ask-agent split button: a menu of the Space's
 * agents (one checked) and, when the person may use it, "Let the classifier
 * choose". Keyboard as NSH's Menu: Return, Space or Down open it on the
 * first item, arrows move, typeahead by name, Escape closes. Item testIDs
 * are `${testID}:${id}`.
 */
export function AskAgentChooser(props: Testable & { choices: readonly AskAgentChoice[]; primary?: boolean; disabled?: boolean; heading?: string }) {
  return (
    <Menu
      placement="top"
      align="end"
      offset={6}
      popupClass={cx(popoverClass, "min-w-[240px] max-w-[340px]")}
      popupStartingClass={popoverMotion.starting}
      popupEndingClass={popoverMotion.ending}
      trigger={
        <Button
          class={cx(
            "items-center justify-center h-[36px] w-[30px] rounded-r-full border-l",
            focusRing,
            props.disabled
              ? "bg-muted border border-border"
              : props.primary
                ? "bg-primary border-primary-foreground/30 hover:bg-primary/90"
                : "bg-tone-agent border border-tone-agent-foreground/40 hover:border-tone-agent-foreground aria-expanded:border-tone-agent-foreground",
          )}
          disabled={props.disabled}
          testID={props.testID}
          accessibilityLabel={props.accessibilityLabel ?? "Choose which agent to ask"}
          tooltip={<TooltipText text="Choose the agent" />}
        >
          <Icon name="chevron-down" size={14} color={props.disabled ? "muted" : props.primary ? "on-primary" : "default"} />
        </Button>
      }
    >
      <If when={props.heading}>
        <Text class="px-(--space-2) pt-[3px] pb-[5px] text-(length:--text-xs) text-muted-foreground">{props.heading}</Text>
      </If>
      <Each of={[...props.choices]} key={(c) => c.id}>
        {(c) => (
          <>
            <If when={c().separated}>
              <MenuSeparator class="my-[4px] h-px bg-border" />
            </If>
            <MenuItem
              class={cx(popoverItemClass, "group flex-row items-center gap-(--space-2)")}
              role="menuitemradio"
              checked={c().checked}
              onSelect={c().onSelect}
              testID={props.testID ? `${props.testID}:${c().id}` : undefined}
              accessibilityLabel={c().accessibilityLabel}
            >
              {c().leading}
              <View class="flex-1 min-w-0 flex-col gap-px">
                <Text class="text-(length:--text-sm) font-medium text-popover-foreground" style={{ numberOfLines: 1 }}>
                  {c().title}
                </Text>
                <If when={c().detail}>
                  <Text class="text-(length:--text-xs) text-muted-foreground" style={{ numberOfLines: 1 }}>
                    {c().detail}
                  </Text>
                </If>
              </View>
              <View class={cx("size-[16px] items-center justify-center", c().checked ? "opacity-100" : "opacity-0")}>
                <ChatGlyph name="check" size={14} color="agent" />
              </View>
            </MenuItem>
          </>
        )}
      </Each>
    </Menu>
  );
}

/** The plain send, at the bottom right: an arrow on the primary color while it can send, muted otherwise. */
export function SendButton(props: Testable & { note?: boolean; busy: boolean; disabled: boolean; onPress: () => void; tooltip?: string }) {
  const inactive = () => props.disabled || props.busy;
  return (
    <Button
      class={cx(
        "items-center justify-center rounded-full", props.note ? "h-[36px] px-(--space-2)" : "size-[36px]",
        focusRing,
        inactive() ? sendInactive : props.note ? "bg-muted hover:bg-accent" : "bg-primary hover:bg-primary/90 active:bg-primary/85",
      )}
      disabled={inactive()}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.busy ? "Sending…" : props.accessibilityLabel ?? "Send"}
      tooltip={props.tooltip ? <TooltipText text={props.tooltip} /> : undefined}
    >
      <If when={props.note} else={<ChatGlyph name="send" color={inactive() ? "muted" : "on-primary"} />}>
        <Text class="text-(length:--text-sm) text-muted-foreground">Save note</Text>
      </If>
    </Button>
  );
}

// ---------------------------------------------------------------- attachments

/**
 * The files staged in the composer, above the field: their chips, one short
 * line on where they go and who sees them, and what a pick left out.
 */
export function AttachmentTray(props: Testable & { cue: string; note?: string | null; children?: JSX.Element }) {
  return (
    <View class="flex-col gap-(--space-1) px-(--space-3) pt-(--space-2)" testID={props.testID}>
      <View class="flex-row flex-wrap gap-(--space-2)">{props.children}</View>
      <Text class="text-(length:--text-xs) text-muted-foreground" testID={props.testID ? `${props.testID}:cue` : undefined}>
        {props.cue}
      </Text>
      <If when={props.note}>
        <Text class="text-(length:--text-xs) text-tone-warning-foreground" role="alert" testID={props.testID ? `${props.testID}:note` : undefined}>
          {props.note}
        </Text>
      </If>
    </View>
  );
}

/** A note about attaching files, outside the tray (no files staged yet). */
export function AttachmentNote(props: Testable & { text: string }) {
  return (
    <Text class="px-(--space-3) pt-(--space-2) text-(length:--text-xs) text-tone-warning-foreground" role="alert" testID={props.testID}>
      {props.text}
    </Text>
  );
}

function FileThumb(props: { kind: FileKind; src?: string | null }) {
  return (
    <View class="size-[28px] shrink-0 items-center justify-center overflow-hidden rounded-[5px] bg-muted" accessibilityElementsHidden>
      <If when={props.src} else={<MediaGlyphIcon name={props.kind} size={16} />}>
        {(src) => <Image src={src()} class="w-full h-full" style={{ objectFit: "cover" }} fadeDuration={0} />}
      </If>
    </View>
  );
}

/** One staged file: a small picture or its type's icon, its name and size, and Remove. */
export function StagedFileChip(
  props: Testable & { name: string; detail: string; kind: FileKind; previewSrc?: string | null; busy?: boolean; onRemove: () => void },
) {
  return (
    <View
      class="flex-row items-center gap-(--space-2) max-w-[260px] rounded-(--radius-sm) border border-border bg-background py-[3px] pl-[3px] pr-[2px]"
      testID={props.testID}
      accessibilityLabel={`${props.name}, ${props.detail}`}
    >
      <FileThumb kind={props.kind} src={props.previewSrc} />
      <View class="flex-1 min-w-0 flex-col">
        <Text class="text-(length:--text-xs) font-medium text-foreground" style={{ numberOfLines: 1 }}>
          {props.name}
        </Text>
        <Text class="text-[11px] text-muted-foreground" style={{ numberOfLines: 1 }}>
          {props.detail}
        </Text>
      </View>
      <Button
        class={cx("items-center justify-center size-[24px] rounded-[5px] hover:bg-muted active:bg-muted disabled:opacity-50", focusRing)}
        disabled={props.busy}
        onPress={props.onRemove}
        testID={props.testID ? `${props.testID}:remove` : undefined}
        accessibilityLabel={`Remove ${props.name}`}
      >
        <Text class="text-(length:--text-sm) text-muted-foreground">×</Text>
      </Button>
    </View>
  );
}

/** A message's files, below its text. */
export function MessageFiles(props: Testable & { children?: JSX.Element }) {
  return (
    <View class="flex-row flex-wrap gap-(--space-2) pt-[2px]" testID={props.testID}>
      {props.children}
    </View>
  );
}

/** A file a message carries: its type's icon, name and size; pressing it opens it in the Library. */
export function MessageFileCard(props: Testable & { name: string; detail: string; kind: FileKind; onOpen: () => void }) {
  return (
    <Button
      class={cx(
        "flex-row items-center gap-(--space-2) min-w-[180px] max-w-[320px] rounded-(--radius) border border-border bg-card px-(--space-2) py-(--space-1) hover:bg-muted active:bg-muted",
        focusRing,
      )}
      onPress={props.onOpen}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? `Open ${props.name} in the Library`}
      tooltip={<TooltipText text="Open in the Library" />}
    >
      <FileThumb kind={props.kind} />
      <View class="flex-1 min-w-0 flex-col">
        <Text class="text-(length:--text-sm) font-medium text-card-foreground" style={{ numberOfLines: 1 }}>
          {props.name}
        </Text>
        <Text class="text-(length:--text-xs) text-muted-foreground" style={{ numberOfLines: 1 }}>
          {props.detail}
        </Text>
      </View>
    </Button>
  );
}

/** A file deleted from the Library since the message was sent: neutral, no name, nothing to open. */
export function RemovedFileCard(props: Testable) {
  return (
    <View
      class="flex-row items-center gap-(--space-2) min-w-[180px] rounded-(--radius) border border-dashed border-border px-(--space-2) py-(--space-1)"
      testID={props.testID}
      accessibilityLabel="File removed"
    >
      <FileThumb kind="document" />
      <Text class="text-(length:--text-sm) text-muted-foreground">File removed</Text>
    </View>
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
