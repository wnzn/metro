// Small previews for quick-search file rows: a text excerpt, an image
// thumbnail, a still of a video (its own first frame, paused and muted), an
// audio waveform drawn from real sample peaks, and a neutral line when
// there's nothing honest to draw. Each takes semantic props only and is one
// element for screen readers, named by `accessibilityLabel`; the drawing
// inside is hidden from them. Sources are `blob:` URLs (or a private local
// file) the feature made from bytes it read with the user's credential.

import { Image, Text, Video, View } from "nsh";
import { cx } from "./tokens";

type Named = { testID?: string; accessibilityLabel: string };

/** A short excerpt of a file's own text. The popup around it scrolls, so it isn't clipped to a line count. */
export function QuickTextExcerpt(props: Named & { text: string }) {
  return (
    <View class="w-full rounded-(--radius-sm) bg-muted px-(--space-2) py-(--space-1)" testID={props.testID} accessibilityLabel={props.accessibilityLabel} role="text" selectable>
      <Text class="text-(length:--text-xs) text-muted-foreground" accessibilityElementsHidden>
        {props.text}
      </Text>
    </View>
  );
}

const frame = "w-[72px] h-[48px] overflow-hidden rounded-(--radius-sm) border border-border bg-muted";

/** An image, fitted to a small frame. */
export function QuickImage(props: Named & { src: string; onError?: () => void }) {
  return (
    <View class={frame} testID={props.testID} accessibilityLabel={props.accessibilityLabel} role="image">
      <Image src={props.src} class="w-full h-full" style={{ objectFit: "cover" }} accessibilityElementsHidden onError={() => props.onError?.()} />
    </View>
  );
}

/**
 * A video's own first frame: the platform's player, paused and muted, with
 * no controls, so nothing plays. `onShown` once the player loaded it.
 */
export function QuickVideoStill(props: Named & { src: string; onShown?: () => void; onError?: () => void }) {
  return (
    <View class={frame} testID={props.testID} accessibilityLabel={props.accessibilityLabel} role="image">
      <Video
        src={props.src}
        paused
        muted
        class="w-full h-full"
        style={{ objectFit: "cover" }}
        accessibilityElementsHidden
        onLoad={() => props.onShown?.()}
        onError={() => props.onError?.()}
      />
    </View>
  );
}

/**
 * An audio waveform: one bar per peak (0..1 of full scale), as loud as the
 * samples were, never scaled up. Silence is a flat line. `caption` (a
 * length) follows it.
 */
export function QuickWaveform(props: Named & { peaks: readonly number[]; caption?: string }) {
  return (
    <View class="flex-row items-center gap-(--space-2)" testID={props.testID} accessibilityLabel={props.accessibilityLabel} role="image">
      <View class="w-[144px] h-[28px] flex-row items-center gap-px" accessibilityElementsHidden>
        <Each of={props.peaks} key="index">
          {(p, i) => (
            <View
              class={cx("flex-1 rounded-full", p() > 0 ? "bg-muted-foreground" : "bg-border")}
              style={{ height: `${Math.max(4, Math.round(p() * 100))}%` }}
              testID={props.testID ? `${props.testID}:bar:${i()}` : undefined}
            />
          )}
        </Each>
      </View>
      <If when={props.caption}>
        <Text class="text-(length:--text-xs) tabular-nums text-muted-foreground" accessibilityElementsHidden>
          {props.caption}
        </Text>
      </If>
    </View>
  );
}

/** Nothing to draw: a quiet line saying why (a type with no preview, a budget, lost access). */
export function QuickNoPreview(props: Named & { text: string }) {
  return (
    <View class="flex-row items-center" testID={props.testID} accessibilityLabel={props.accessibilityLabel} role="text">
      <Text class="text-(length:--text-xs) text-muted-foreground" accessibilityElementsHidden>
        {props.text}
      </Text>
    </View>
  );
}

/** While the preview loads: an empty frame of the size the preview will roughly take. */
export function QuickPreviewLoading(props: Named) {
  return <View class={cx(frame, "opacity-60")} testID={props.testID} accessibilityLabel={props.accessibilityLabel} role="progressbar" />;
}
