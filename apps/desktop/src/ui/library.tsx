// Primitives for file lists and search results (Library, Search): a file-type
// icon, a file row, a result card the whole of which opens the result, text
// with marked parts, and a small authenticated thumbnail. Row menus use the
// shared ActionsMenu (./primitives).
// Feature code passes only semantic props; every class decision lives here.

import { Image, Text, View, effect, reduceMotion, signal, token, untracked } from "nsh";
import type { JSX, ViewRef } from "nsh";
import { Button, Menu, MenuItem } from "nsh/base";
import fileAudioIcon from "./icons/file-audio.svg";
import fileDocumentIcon from "./icons/file-document.svg";
import fileImageIcon from "./icons/file-image.svg";
import fileVideoIcon from "./icons/file-video.svg";
import pauseIcon from "./icons/pause.svg";
import playIcon from "./icons/play.svg";
import uploadIcon from "./icons/upload.svg";
import { cx, disabledTextClass, popoverClass, popoverItemClass } from "./tokens";

type Testable = { testID?: string; accessibilityLabel?: string };

const focusRing = "focus-visible:ring-2 focus-visible:ring-ring";

// ---------------------------------------------------------------- glyphs

const GLYPHS = {
  document: fileDocumentIcon,
  image: fileImageIcon,
  audio: fileAudioIcon,
  video: fileVideoIcon,
  pause: pauseIcon,
  play: playIcon,
  upload: uploadIcon,
} as const;

export type MediaGlyph = keyof typeof GLYPHS;
/** What a file is, for its icon. */
export type FileKind = "document" | "image" | "audio" | "video";

/** A decorative glyph of the media family; say what it means in the control's label. */
export function MediaGlyphIcon(props: { name: MediaGlyph; tone?: "default" | "muted" | "primary-foreground"; size?: 16 | 20 }) {
  const tint = () => (props.tone === "default" ? "foreground" : props.tone === "primary-foreground" ? "primary-foreground" : "muted-foreground");
  return (
    <Image
      src={GLYPHS[props.name]}
      fadeDuration={0}
      style={{ width: props.size ?? 16, height: props.size ?? 16, tintColor: token(tint()) }}
      accessibilityElementsHidden
    />
  );
}

/** A file's type as a tile: decorative, the row's text says the type. */
export function FileIcon(props: { kind: FileKind; testID?: string }) {
  return (
    <View class="size-[36px] items-center justify-center rounded-(--radius-sm) bg-muted" accessibilityElementsHidden testID={props.testID}>
      <MediaGlyphIcon name={props.kind} size={20} />
    </View>
  );
}

/** A primary-looking button with a glyph before its label (Upload). */
export function GlyphAction(
  props: Testable & { glyph: MediaGlyph; label: string; busyLabel?: string; busy?: boolean; disabled?: boolean; onPress: () => void },
) {
  return (
    <Button
      class={cx("flex-row items-center gap-(--space-2) min-h-[32px] rounded-(--radius-sm) px-(--space-3) py-(--space-2) bg-primary hover:bg-primary/90 active:bg-primary/85 disabled:opacity-50", focusRing)}
      disabled={props.disabled || props.busy}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
    >
      <MediaGlyphIcon name={props.glyph} tone="primary-foreground" />
      <Text class="text-(length:--text-sm) font-medium text-primary-foreground">{props.busy ? props.busyLabel ?? props.label : props.label}</Text>
    </Button>
  );
}

// ---------------------------------------------------------------- lists

/** A list on one card; its rows draw the lines between them. */
export function FileList(props: Testable & { children?: JSX.Element }) {
  return (
    <View class="overflow-hidden rounded-(--radius) border border-border bg-card" testID={props.testID} accessibilityLabel={props.accessibilityLabel}>
      <View class="-mt-px flex-col">{props.children}</View>
    </View>
  );
}

/**
 * One file: its icon (or thumbnail), its name and a quiet line about it, then
 * its actions. `below` holds a status line or a confirmation under the row.
 */
export function FileRow(
  props: Testable & { leading: JSX.Element; title: string; meta: string; trailing?: JSX.Element; below?: JSX.Element },
) {
  return (
    <View class="flex-col border-t border-border" testID={props.testID}>
      <View class="flex-row items-center gap-(--space-3) px-(--space-4) py-(--space-3)">
        {props.leading}
        <View class="flex-1 min-w-0 flex-col gap-px">
          <Text class="text-(length:--text-md) font-medium text-card-foreground" style={{ numberOfLines: 1 }}>
            {props.title}
          </Text>
          <Text class="text-(length:--text-xs) text-muted-foreground" style={{ numberOfLines: 1 }}>
            {props.meta}
          </Text>
        </View>
        <View class="flex-row items-center gap-(--space-2)">{props.trailing}</View>
      </View>
      {props.below}
    </View>
  );
}

/** Indented content under a FileRow (a status, a confirmation). */
export function RowDetail(props: { children?: JSX.Element }) {
  return <View class="flex-col gap-(--space-1) pl-[64px] pr-(--space-4) pb-(--space-3)">{props.children}</View>;
}

// ---------------------------------------------------------------- results

/**
 * A search result the whole of which opens it: one button, so it is one
 * focus stop and one press target, named by `accessibilityLabel`.
 */
export function ResultCard(props: Testable & { accessibilityLabel: string; onPress: () => void; leading?: JSX.Element; children?: JSX.Element }) {
  return (
    <Button
      class={cx("flex-row items-start gap-(--space-3) rounded-(--radius) border border-border bg-card px-(--space-4) py-(--space-3) hover:bg-muted/60 active:bg-muted", focusRing)}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
    >
      {props.leading}
      <View class="flex-1 min-w-0 flex-col gap-(--space-1)">{props.children}</View>
    </Button>
  );
}

/** A part of a text, marked or not. */
export type Marked = { text: string; mark: boolean };

/** Text with its marked parts (the words that matched) emphasised. */
export function MarkedText(props: Testable & { parts: readonly Marked[]; lines?: number; muted?: boolean }) {
  return (
    <Text
      class={cx("shrink min-w-0 text-(length:--text-sm)", props.muted ? "text-muted-foreground" : "text-card-foreground")}
      style={props.lines ? { numberOfLines: props.lines } : undefined}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
    >
      <Each of={props.parts} key="index">
        {(p) => (
          <Text class={p().mark ? "font-semibold text-tone-warning-foreground bg-tone-warning" : undefined}>{p().text}</Text>
        )}
      </Each>
    </Text>
  );
}

/** A small preview of an image the feature fetched with the user's credential (a `blob:` URL). */
export function Thumbnail(props: Testable & { src: string }) {
  return (
    <View class="size-[48px] overflow-hidden rounded-(--radius-sm) border border-border bg-muted" testID={props.testID}>
      <Image src={props.src} class="w-full h-full" style={{ objectFit: "cover" }} accessibilityLabel={props.accessibilityLabel} />
    </View>
  );
}

// ---------------------------------------------------------------- upload

export type UploadDestination = { id: string; label: string; detail: string };

/**
 * The primary Upload button as a menu of destinations: each item names where
 * the files go and who will see them, and choosing one is the whole step (the
 * feature then opens the system's file picker). `note` (the accepted types
 * and size) heads the menu. Items' testIDs are `${testID}:${id}`.
 */
export function UploadMenu(
  props: Testable & { label: string; busyLabel?: string; busy?: boolean; note?: string; destinations: readonly UploadDestination[]; onChoose: (id: string) => void },
) {
  return (
    <Menu
      placement="bottom"
      align="end"
      offset={4}
      popupClass={cx(popoverClass, "min-w-[260px] max-w-[360px]")}
      trigger={
        <Button
          class={cx("group flex-row items-center gap-(--space-2) min-h-[32px] rounded-(--radius-sm) px-(--space-3) py-(--space-2)", props.busy ? "bg-muted" : "bg-primary hover:bg-primary-hover active:bg-primary-hover", focusRing)}
          disabled={props.busy}
          testID={props.testID}
          accessibilityLabel={props.accessibilityLabel ?? props.label}
        >
          <MediaGlyphIcon name="upload" tone={props.busy ? "muted" : "primary-foreground"} />
          <Text class={cx("text-(length:--text-sm) font-medium text-primary-foreground", disabledTextClass)}>{props.busy ? props.busyLabel ?? props.label : props.label}</Text>
          <Text class={cx("text-(length:--text-xs) text-primary-foreground", disabledTextClass)}>▾</Text>
        </Button>
      }
    >
      <If when={props.note}>
        <Text class="px-(--space-2) pt-(--space-1) pb-(--space-2) text-(length:--text-xs) text-muted-foreground">{props.note}</Text>
      </If>
      <Each of={props.destinations} key={(d) => d.id}>
        {(d) => (
          <MenuItem class={popoverItemClass} onSelect={() => props.onChoose(d().id)} testID={props.testID ? `${props.testID}:${d().id}` : undefined} accessibilityLabel={`${d().label}. ${d().detail}`}>
            <View class="flex-col gap-px">
              <Text class="text-(length:--text-sm) text-popover-foreground">{d().label}</Text>
              <Text class="text-(length:--text-xs) text-muted-foreground">{d().detail}</Text>
            </View>
          </MenuItem>
        )}
      </Each>
    </Menu>
  );
}

/** Content that scrolls itself into view when `active` turns true (a "take me there" from another page), then calls `onShown`. */
export function RevealAnchor(props: { active: boolean; onShown?: () => void; children?: JSX.Element; testID?: string }) {
  let node: ViewRef | undefined;
  const laidOut = signal(false);
  effect(() => {
    if (!props.active || !laidOut() || !node) return;
    const target = node;
    let live = true;
    void target.measure().then((m) => {
      if (!live || !m || m.bounds.size.height === 0) return;
      target.reveal({ vertical: "start", animated: !untracked(reduceMotion) });
      props.onShown?.();
    });
    return () => { live = false; };
  });
  return (
    <View ref={(n: ViewRef) => (node = n)} onLayout={({ bounds }) => laidOut.set(bounds.size.height > 0)} class="flex-col" testID={props.testID}>
      {props.children}
    </View>
  );
}
