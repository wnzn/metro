// Small previews for quick-search file rows, in two parts a row places
// apart: a leading TILE (72×44: an image or a video's own first frame fitted
// whole, a waveform of real sample peaks resized to fit, or the file type's
// glyph) and a one-line DETAIL under the file name (the start of its text,
// its audio tags, a sound's length). Each part takes semantic props only and
// is one element for screen readers, named by `accessibilityLabel`; the
// drawing inside is hidden from them. Sources are `blob:` URLs (or a private
// local file) the feature made from bytes it read with the user's credential.

import { Image, Text, Video, View } from "nsh";
import { MediaGlyphIcon } from "./library";
import type { FileKind } from "./library";
import { cx, textClass } from "./tokens";

type Named = { testID?: string; accessibilityLabel: string };

/** The leading tile every file row has, whatever fills it: same size, same edge, so rows line up. */
const tile = "w-[72px] h-[44px] shrink-0 items-center justify-center overflow-hidden rounded-(--radius-sm) border border-border bg-muted";

/** An image, fitted whole into the tile (never cropped). */
export function QuickImage(props: Named & { src: string; onError?: () => void }) {
  return (
    <View class={tile} testID={props.testID} accessibilityLabel={props.accessibilityLabel} role="image">
      <Image src={props.src} class="w-full h-full" style={{ objectFit: "contain" }} accessibilityElementsHidden onError={() => props.onError?.()} />
    </View>
  );
}

/**
 * A video's own first frame, fitted whole into the tile: the platform's
 * player, paused and muted, with no controls, so nothing plays. `onShown`
 * once the player loaded it.
 */
export function QuickVideoStill(props: Named & { src: string; onShown?: () => void; onError?: () => void }) {
  return (
    <View class={tile} testID={props.testID} accessibilityLabel={props.accessibilityLabel} role="image">
      <Video
        src={props.src}
        paused
        muted
        class="w-full h-full"
        style={{ objectFit: "contain" }}
        accessibilityElementsHidden
        onLoad={() => props.onShown?.()}
        onError={() => props.onError?.()}
      />
    </View>
  );
}

/**
 * An audio waveform in the tile: one bar per peak (0..1 of full scale), as
 * loud as the samples were, never scaled up; the whole graph is narrowed to
 * fit, none of it cut. Silence is a flat line. Its length goes in the row's
 * detail line.
 */
export function QuickWaveform(props: Named & { peaks: readonly number[] }) {
  return (
    <View class={tile} testID={props.testID} accessibilityLabel={props.accessibilityLabel} role="image">
      <View class={cx("w-[64px] h-[32px] flex-row items-center", props.peaks.length <= 24 && "gap-px")} accessibilityElementsHidden>
        <Each of={props.peaks} key="index">
          {(p, i) => (
            <View
              class={cx("flex-1 rounded-full", p() > 0 ? "bg-muted-foreground" : "bg-border")}
              style={{ height: `${Math.max(6, Math.round(p() * 100))}%` }}
              testID={props.testID ? `${props.testID}:bar:${i()}` : undefined}
            />
          )}
        </Each>
      </View>
    </View>
  );
}

/**
 * The file type's glyph in the tile: for files whose preview is words (text,
 * tags) or that have none. With an `accessibilityLabel` it says why there is
 * no picture; without, it is decorative.
 */
export function QuickTypeTile(props: { kind: FileKind; testID?: string; accessibilityLabel?: string }) {
  return (
    <View
      class={tile}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
      accessibilityElementsHidden={props.accessibilityLabel ? undefined : true}
      role={props.accessibilityLabel ? "image" : undefined}
    >
      <MediaGlyphIcon name={props.kind} size={20} />
    </View>
  );
}

/** While the preview loads: the empty tile. */
export function QuickPreviewLoading(props: Named) {
  return <View class={cx(tile, "opacity-60")} testID={props.testID} accessibilityLabel={props.accessibilityLabel} role="progressbar" />;
}

/** One quiet line under the file name: the start of its text, its tags, its length. The whole of it goes in `accessibilityLabel`. */
export function QuickDetailLine(props: Named & { text: string }) {
  return (
    <View class="flex-row items-center min-w-0" testID={props.testID} accessibilityLabel={props.accessibilityLabel} role="text">
      <Text class={cx("flex-1", textClass.xs, "text-muted-foreground")} style={{ numberOfLines: 1 }} accessibilityElementsHidden>
        {props.text}
      </Text>
    </View>
  );
}
