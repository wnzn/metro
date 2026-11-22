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
 * player: the length once it loaded, the position from its progress events
 * or the user's own seek. Before it loaded the timeline is disabled and
 * shows no position. The match (`matchStart`..`matchEnd`, seconds) is drawn
 * under the timeline only once the length is known, so it is never placed on
 * a guessed scale.
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
  const seekTo = (seconds: number) => {
    if (!video) return;
    video.seek(seconds);
    position.set(seconds);
  };
  effect(() => {
    props.seekToken;
    if (untracked(loaded) && props.startSeconds > 0) seekTo(props.startSeconds);
  });
  const known = () => loaded() && duration() > 0 && Number.isFinite(duration());
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
          const d = e?.duration ?? 0;
          loaded.set(true);
          duration.set(d);
          position.set(0);
          if (props.startSeconds > 0) seekTo(props.startSeconds);
          props.onLoaded?.(d);
        }}
        onProgress={(e: { currentTime: number; duration: number }) => {
          position.set(e.currentTime);
          if (e.duration > 0 && Number.isFinite(e.duration)) duration.set(e.duration);
          props.onTime?.(e.currentTime, e.duration);
        }}
        onError={(e: { message?: string }) => props.onError?.(e?.message ?? "This file can't be played here.")}
        onEnd={() => props.onEnd?.()}
      />
      <View class="flex-row items-center gap-(--space-2)">
        <Button
          class="items-center justify-center size-[32px] rounded-full bg-primary hover:bg-primary/90 active:bg-primary/85 disabled:opacity-50 focus-visible:ring-2 focus-visible:ring-ring"
          onPress={() => props.onPlayingChange(!props.playing)}
          accessibilityLabel={`${props.playing ? "Pause" : "Play"} ${props.title}`}
          testID={props.testID ? `${props.testID}:toggle` : undefined}
        >
          <MediaGlyphIcon name={props.playing ? "pause" : "play"} tone="primary-foreground" />
        </Button>
        <Text class="min-w-[40px] text-(length:--text-xs) font-mono text-muted-foreground" testID={props.testID ? `${props.testID}:position` : undefined}>
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
            onChange={(e) => dragging.set(typeof e.value === "number" ? e.value : e.value[0])}
            onChangeEnd={(e) => {
              const v = typeof e.value === "number" ? e.value : e.value[0];
              dragging.set(null);
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
        <Text class="min-w-[40px] text-(length:--text-xs) font-mono text-muted-foreground" testID={props.testID ? `${props.testID}:duration` : undefined}>
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
