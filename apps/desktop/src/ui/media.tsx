// Media primitives for previews: an image, a native player that can jump to
// a time, and a text section with the matching part marked. They take only
// semantic props; sources are `blob:` URLs the feature made from bytes it
// fetched with the user's credential (never a URL with a token in it), or a
// private local preview file where a platform's player can't play `blob:`.

import { effect, Image, signal, Text, untracked, Video, View } from "nsh";
import type { VideoRef } from "nsh";
import { cx } from "./tokens";

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
