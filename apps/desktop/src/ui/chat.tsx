// Chat and profile primitives: a person's picture (or initials), the
// transcript's message frame and day separator, and a timestamp. Like
// ./primitives, feature code passes only semantic props; every class and
// color decision lives here or in theme/theme.json.
//
// A picture's `src` is a `blob:` URL the feature made from bytes it fetched
// with the user's credential, never a remote URL.

import { announce, computed, effect, Image, openUrl, signal, Text, View } from "nsh";
import { Button, TextView } from "nsh/base";
import type { TextViewCustomNode } from "nsh/base";
import type { JSX, ViewRef } from "nsh";
import { messageMarkdown } from "./messageMarkdown";
import { httpsLink } from "./pageMarkdown";
import { Modal } from "./chrome";
import { Action, ActionBar, Column, Label } from "./primitives";
import { cx, focusRingClass, toneSurfaceClass, toneTextClass } from "./tokens";
import type { Tone } from "./tokens";

type Testable = { testID?: string; accessibilityLabel?: string };

/** The bounded, selectable Markdown body of a person or agent message. */
export function MessageBody(props: {
  text: string;
  testID?: string;
  scopeKey?: string;
  mentions?: Parameters<typeof messageMarkdown>[1];
  renderMention?: (ordinal: number) => JSX.Element;
}) {
  const rich = computed(() => messageMarkdown(props.text, props.mentions));
  const pending = signal<{ url: string; scope?: string } | null>(null);
  const external = () => {
    const link = pending();
    return link?.scope === props.scopeKey ? link : null;
  };
  const close = () => pending.set(null);
  effect(() => { if (pending() && !external()) close(); });
  const renderCustom = (node: TextViewCustomNode) =>
    node.name === "mention" && props.renderMention ? props.renderMention(Number(node.attributes?.ordinal)) : <Text>{node.text}</Text>;
  return <>
    <TextView
      class="min-w-0 max-w-full text-(length:--text-md) text-foreground"
      classes={{
        heading1: "text-(length:--text-lg)", heading2: "text-(length:--text-md)", heading3: "text-(length:--text-md)",
        listItemContent: "min-w-0", table: "min-w-0 max-w-full", codeBlock: "min-w-0 max-w-full",
      }}
      testID={props.testID}
      document={rich().document}
      selectable
      links="report"
      renderCustom={renderCustom}
      onLinkPress={e => {
        const url = httpsLink(e.url);
        if (url) pending.set({ url, scope: props.scopeKey });
      }}
    />
    <If when={rich().limited}><Label variant="caption" color="muted">Showing part of this message.</Label></If>
    <If when={external()}>{link =>
      <Modal open size="compact" title="Open external link" onClose={close}>
        <Column pad={4} gap={3}>
          <Label heading variant="title">Open in your browser?</Label>
          <Label>{link().url}</Label>
          <ActionBar>
            <Action label="Cancel" variant="ghost" onPress={close} />
            <Action label="Open link" variant="primary" onPress={() => {
              const url = external()?.url;
              const safe = url && httpsLink(url);
              close();
              if (!safe) return;
              const failed = () => announce("Could not open the link in your browser.");
              void openUrl(safe).then(result => { if (!result.opened) failed(); }, failed);
            }} />
          </ActionBar>
        </Column>
      </Modal>
    }</If>
  </>;
}

/** An inline attachment, focusable only for a verified identity. */
export function MentionToken(props: Testable & {
  label: string; tone: "self" | "person" | "agent" | "unavailable";
  onPress?: () => void; ref?: (node: ViewRef) => void;
}) {
  const colors = () => props.tone === "self" ? "bg-tone-warning text-tone-warning-foreground font-semibold"
    : props.tone === "person" ? "bg-accent text-accent-foreground"
    : props.tone === "agent" ? "bg-tone-agent text-tone-agent-foreground" : "bg-muted text-muted-foreground";
  const shape = "max-w-[175px] rounded-full px-(--space-1)";
  return <View class="flex-row items-center align-middle">
    <If when={props.onPress} else={
      <View class={cx(shape, colors())}
        testID={props.testID} accessibilityLabel={props.accessibilityLabel}>
        <Text class={colors()} style={{ numberOfLines: 1 }}>{props.label}</Text>
      </View>
    }>
      <Button role="link" testID={props.testID} ref={props.ref} accessibilityLabel={props.accessibilityLabel}
        onPress={props.onPress} class={cx(shape, "hover:opacity-80", colors(), focusRingClass)}>
        <Text class={colors()} testID={props.tone === "self" ? "mention-token-me" : undefined}
          style={{ numberOfLines: 1 }}>{props.label}</Text>
      </Button>
    </If>
  </View>;
}

export function IdentityCardFrame(props: { children?: JSX.Element }) {
  return <View class="flex-col w-[256px] max-w-full gap-(--space-2) p-(--space-2)">{props.children}</View>;
}

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
