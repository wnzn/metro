// Media primitives for previews: an image, a native player that can jump to
// a time, and a text section with the matching part marked. They take only
// semantic props; sources are `blob:` URLs the feature made from bytes it
// fetched with the user's credential (never a URL with a token in it), or a
// private local preview file where a platform's player can't play `blob:`.

import { effect, Image, signal, Text, untracked, Video, View } from "nsh";
import type { VideoRef } from "nsh";
import { Button, Slider } from "nsh/base";
import { MediaGlyphIcon } from "./library";
import { cx } from "./tokens";

export * from "./library";
export * from "./quickMedia";

type Testable = { testID?: string; accessibilityLabel?: string };

/** An image shown whole, fitted to the preview's width. */
export function MediaImage(props: Testable & { src: string; onError?: (message: string) => void }) {
  return (
    <View class="w-full h-[360px] items-center justify-center overflow-hidden rounded-(--radius-sm) border border-border bg-muted" testID={props.testID}>
      <Image
        src={props.src}
        class="w-full h-full"
        style={{ objectFit: "contain" }}
        accessibilityLabel={props.accessibilityLabel}
        onError={(e: { message?: string }) => props.onError?.(e?.message ?? "The image couldn't be shown.")}
      />
    </View>
  );
}

/**
 * A native player with the platform's controls. It jumps to `startSeconds`
 * once the media is loaded, and again whenever `seekToken` changes (a "play
 * from the match" press). `playing` starts it after a seek.
 */
export function MediaPlayer(
  props: Testable & {
    src: string;
    /** Audio files get a short player; the platform still draws its controls. */
    audioOnly?: boolean;
    startSeconds: number;
    seekToken: number;
    playing: boolean;
    onTime?: (seconds: number, duration: number) => void;
    onLoaded?: (duration: number) => void;
    onError?: (message: string) => void;
    onEnd?: () => void;
  },
) {
  let video: VideoRef | undefined;
  const loaded = signal(false);
  const seekTo = () => {
    if (video && props.startSeconds > 0) video.seek(props.startSeconds);
  };
  effect(() => {
    props.seekToken;
    if (untracked(loaded)) seekTo();
  });
  return (
    <View
      class={cx("w-full overflow-hidden rounded-(--radius-sm) border border-border bg-muted", props.audioOnly ? "h-[72px]" : "h-[320px]")}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
    >
      <Video
        ref={(node: VideoRef) => {
          video = node;
        }}
        src={props.src}
        controls
        paused={!props.playing}
        class="flex-1"
        style={{ objectFit: "contain" }}
        onLoad={(e: { duration: number }) => {
          loaded.set(true);
          seekTo();
          props.onLoaded?.(e?.duration ?? 0);
        }}
        onProgress={(e: { currentTime: number; duration: number }) => props.onTime?.(e.currentTime, e.duration)}
        onError={(e: { message?: string }) => props.onError?.(e?.message ?? "This file can't be played here.")}
        onEnd={() => props.onEnd?.()}
      />
    </View>
  );
}

/** `m:ss` (or `h:mm:ss`) of whole seconds. */
function clockOf(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

/**
 * A compact audio player: play or pause, the position, a timeline the user
 * can drag or step with the arrow keys, and the length. The sound plays
 * through the platform's player (the documented `Video` element, which plays
 * audio files too), kept out of sight. Every time it shows comes from that
 * player: the length from its load event, the position only from its
 * progress events. A seek (the jump to the match, a drag of the timeline) is
 * asked of the player but never shown as done until the player reports where
 * it is; while dragging, the thumb previews the value under the pointer and
 * then returns to the reported position. Until the first report the position
 * reads "–:––". The match (`matchStart`..`matchEnd`, seconds) is drawn under
 * the timeline once the length is known, whether or not playback was
 * confirmed, and never on a guessed scale. A new `src` starts over.
 *
 * Its one play button starts at the match the first time (it asks the player
 * to seek to `startSeconds`, then plays), and pauses and resumes from where
 * the player is after that. Once the user moved the timeline themselves,
 * nothing jumps back to the match.
 */
export function CompactAudioPlayer(
  props: Testable & {
    src: string;
    /** Where the match starts; the player goes there once loaded and on each `seekToken` change. */
    startSeconds: number;
    matchStart?: number | null;
    matchEnd?: number | null;
    seekToken: number;
    playing: boolean;
    /** The play/pause button asks for this; the feature owns `playing`. */
    onPlayingChange: (playing: boolean) => void;
    /** What the play button and the timeline say to screen readers ("standup.wav"). */
    title: string;
    onTime?: (seconds: number, duration: number) => void;
    onLoaded?: (duration: number) => void;
    onError?: (message: string) => void;
    onEnd?: () => void;
  },
) {
  let video: VideoRef | undefined;
  const loaded = signal(false);
  const duration = signal(0);
  const position = signal<number | null>(null);
  const dragging = signal<number | null>(null);
  /** The first play still starts at the match (no play yet, no manual seek). */
  const atMatchFirst = signal(true);
  let seekOnLoad = false;
  /** A finite length in seconds, else 0 (unknown). */
  const lengthOf = (d: unknown) => (typeof d === "number" && Number.isFinite(d) && d > 0 ? d : 0);
  /** `seconds` held to 0..length, or null when it isn't a finite number. */
  const within = (seconds: unknown, length: number): number | null =>
    typeof seconds === "number" && Number.isFinite(seconds) ? Math.min(Math.max(seconds, 0), length > 0 ? length : Math.max(seconds, 0)) : null;
  /** Asks the player to move; the position changes only when the player reports it. */
  const seekTo = (seconds: number) => {
    const target = within(seconds, untracked(duration));
    if (!video || target === null) return;
    video.seek(target);
  };
  // A new source: nothing of the previous one's length or position remains.
  effect(() => {
    props.src;
    untracked(() => {
      loaded.set(false);
      duration.set(0);
      position.set(null);
      dragging.set(null);
      atMatchFirst.set(true);
      seekOnLoad = false;
    });
  });
  effect(() => {
    props.seekToken;
    if (untracked(loaded) && props.startSeconds > 0) seekTo(props.startSeconds);
  });
  const known = () => loaded() && duration() > 0;
  const shown = () => dragging() ?? position();
  const band = () => {
    if (!known() || props.matchStart == null) return null;
    const d = duration();
    const a = Math.min(Math.max(props.matchStart, 0), d);
    const b = Math.min(Math.max(props.matchEnd ?? a, a), d);
    return { before: a, match: Math.max(b - a, d / 200), after: Math.max(d - b, 0) };
  };
  return (
    <View class="w-full flex-col gap-(--space-1) rounded-(--radius-sm) border border-border bg-card px-(--space-3) py-(--space-2)" testID={props.testID} accessibilityLabel={props.accessibilityLabel}>
      <Video
        ref={(node: VideoRef) => {
          video = node;
        }}
        src={props.src}
        paused={!props.playing}
        class="absolute h-px w-px opacity-0"
        accessibilityElementsHidden
        onLoad={(e: { duration: number }) => {
          const d = lengthOf(e?.duration);
          loaded.set(true);
          duration.set(d);
          if (seekOnLoad && props.startSeconds > 0) seekTo(props.startSeconds);
          seekOnLoad = false;
          props.onLoaded?.(d);
        }}
        onProgress={(e: { currentTime: number; duration: number }) => {
          const d = lengthOf(e?.duration) || untracked(duration);
          const at = within(e?.currentTime, d);
          if (at === null) return;
          if (d !== untracked(duration)) duration.set(d);
          position.set(at);
          props.onTime?.(at, d);
        }}
        onError={(e: { message?: string }) => props.onError?.(e?.message ?? "This file can't be played here.")}
        onEnd={() => props.onEnd?.()}
      />
      <View class="flex-row items-center gap-(--space-2)">
        <Button
          class="items-center justify-center size-[32px] rounded-full bg-primary hover:bg-primary/90 active:bg-primary/85 disabled:opacity-50 focus-visible:ring-2 focus-visible:ring-ring"
          onPress={() => {
            if (props.playing) return props.onPlayingChange(false);
            if (atMatchFirst() && props.startSeconds > 0) {
              if (loaded()) seekTo(props.startSeconds);
              else seekOnLoad = true;
            }
            atMatchFirst.set(false);
            props.onPlayingChange(true);
          }}
          accessibilityLabel={
            props.playing
              ? `Pause ${props.title}`
              : atMatchFirst() && props.startSeconds > 0
                ? `Play ${props.title} from the match at ${clockOf(props.startSeconds)}`
                : `Play ${props.title}`
          }
          testID={props.testID ? `${props.testID}:toggle` : undefined}
        >
          <MediaGlyphIcon name={props.playing ? "pause" : "play"} tone="primary-foreground" />
        </Button>
        <Text class="min-w-[40px] text-(length:--text-xs) tabular-nums text-muted-foreground" testID={props.testID ? `${props.testID}:position` : undefined}>
          {known() && shown() !== null ? clockOf(shown()!) : "–:––"}
        </Text>
        <View class="flex-1 min-w-0 flex-col gap-[2px]">
          <Slider
            class="w-full"
            value={known() ? Math.min(shown() ?? 0, duration()) : 0}
            min={0}
            max={known() ? duration() : 1}
            step={0}
            disabled={!known()}
            accessibilityLabel={`Position in ${props.title}`}
            trackClass="h-[20px] justify-center"
            indicatorClass="h-[4px] rounded-full bg-muted"
            rangeClass="rounded-full bg-primary"
            thumbClass="size-[12px] rounded-full bg-primary-foreground border border-primary focus-visible:ring-2 focus-visible:ring-ring"
            onChange={(e) => dragging.set(within(typeof e.value === "number" ? e.value : e.value[0], duration()))}
            onChangeEnd={(e) => {
              const v = typeof e.value === "number" ? e.value : e.value[0];
              dragging.set(null);
              if (typeof v !== "number") return;
              atMatchFirst.set(false); // the user chose a place: never jump back to the match
              seekOnLoad = false;
              seekTo(v);
            }}
            testID={props.testID ? `${props.testID}:timeline` : undefined}
          />
          <If when={band()}>
            {(b) => (
              <View class="w-full h-[3px] flex-row" accessibilityElementsHidden testID={props.testID ? `${props.testID}:match` : undefined}>
                <View style={{ flex: b().before }} />
                <View class="rounded-full bg-tone-warning-foreground" style={{ flex: b().match }} />
                <View style={{ flex: b().after }} />
              </View>
            )}
          </If>
        </View>
        <Text class="min-w-[40px] text-(length:--text-xs) tabular-nums text-muted-foreground" testID={props.testID ? `${props.testID}:duration` : undefined}>
          {known() ? clockOf(duration()) : "–:––"}
        </Text>
      </View>
    </View>
  );
}

/**
 * A section of a text document with the match marked. `before`, `match`
 * and `after` are already cut by the feature (a bounded window), so a long
 * file never renders whole.
 */
export function TextSection(props: Testable & { before: string; match: string; after: string; mono?: boolean }) {
  return (
    <View class="w-full rounded-(--radius-sm) border border-border bg-card px-(--space-4) py-(--space-3)" selectable testID={props.testID}>
      <Text class={cx("text-(length:--text-sm) text-card-foreground", props.mono && "font-mono")} accessibilityLabel={props.accessibilityLabel}>
        {props.before}
        <Text class="bg-tone-warning text-tone-warning-foreground font-semibold" testID={props.testID ? `${props.testID}:match` : undefined}>
          {props.match}
        </Text>
        {props.after}
      </Text>
    </View>
  );
}
