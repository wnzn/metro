// A quick-search FILE row's preview, in two parts a row places apart from
// one acquisition (never two mounted previews of one row):
//
// - `useQuickPreview(asset)` reads it: the bytes are read, shared, bounded
//   and dropped by ./quickPreviewStore. Mounting reads (once per file),
//   unmounting the last row of a file frees it, and sign-out or losing the
//   file's Space frees everything at once.
// - `QuickPreviewTile` is the leading 72×44 tile: the image, a still of the
//   video (its own first frame, paused and muted), the waveform of a WAV
//   file's real samples, or the file type's glyph (text, tags, or no
//   preview; the reason is in its accessible name, not drawn as a paragraph).
// - `QuickPreviewDetail` is one quiet line under the file name: the start of
//   its text (plain text, Markdown, a Word document's own words), an MP3's
//   own title, artist and album tags, or a sound's length. Nothing otherwise.
//
// `<QuickAssetPreview asset={asset} />` is both, side by side.

import { cleanup, effect, signal, untracked } from "nsh";
import { QuickDetailLine, QuickImage, QuickPreviewLoading, QuickTypeTile, QuickVideoStill, QuickWaveform, Row } from "../../ui";
import { clock, fileKindOf } from "./libraryState";
import type { LibraryAsset } from "./libraryState";
import { acquirePreview, releasePreview, videoFailed, videoShown } from "./quickPreviewStore";
import type { QuickPreview } from "./quickPreviewStore";

export type { QuickPreview };

/** What a screen reader hears for a preview. */
export function previewLabel(name: string, p: QuickPreview): string {
  switch (p.status) {
    case "loading":
      return `Loading a preview of ${name}`;
    case "text":
      return `Preview of ${name}, text: ${p.text}`;
    case "image":
      return `Preview of ${name}: the image`;
    case "video":
      return `Preview of ${name}: a still from the video`;
    case "waveform": {
      const w = p.waveform;
      const part = w.coveredMs + 1 < w.durationMs ? `, its first ${clock(w.coveredMs)}` : "";
      return `Preview of ${name}: audio waveform${part}, ${clock(w.durationMs)} long${w.silent ? ", silent" : ""}`;
    }
    case "tags": {
      const t = p.tags;
      const parts = [t.title && `title ${t.title}`, t.artist && `artist ${t.artist}`, t.album && `album ${t.album}`].filter(Boolean).join(", ");
      return `Preview of ${name}, its tags: ${parts}`;
    }
    case "none":
      return `No preview of ${name}. ${p.reason}`;
  }
}

/** A sound's length, and how much of it is drawn when only the start was read. */
function waveCaption(p: Extract<QuickPreview, { status: "waveform" }>): string {
  const w = p.waveform;
  const len = clock(w.durationMs);
  if (w.silent) return `${len} · silent`;
  return w.coveredMs + 1 < w.durationMs ? `first ${clock(w.coveredMs)} of ${len}` : len;
}

/** The one-line detail a preview gives under the file name, or null (pictures say it in the tile). */
export function previewDetail(p: QuickPreview): string | null {
  if (p.status === "text") return p.text;
  if (p.status === "tags") return [p.tags.title ? `“${p.tags.title}”` : null, p.tags.artist, p.tags.album].filter((x): x is string => !!x).join(" · ");
  if (p.status === "waveform") return waveCaption(p);
  return null;
}

/** Reads the file's preview while the caller is mounted; a new file releases the old one. */
export function useQuickPreview(asset: () => LibraryAsset): () => QuickPreview {
  const view = signal<{ id: string; get: () => QuickPreview } | null>(null);
  let held: string | null = null;
  effect(() => {
    const a = asset();
    untracked(() => {
      if (held === a.id) return;
      if (held) releasePreview(held);
      held = a.id;
      view.set({ id: a.id, get: acquirePreview(a) });
    });
  });
  cleanup(() => {
    if (held) releasePreview(held);
    held = null;
  });
  return () => view()?.get() ?? ({ status: "loading" } as QuickPreview);
}

const as = <S extends QuickPreview["status"]>(p: QuickPreview, s: S) => (p.status === s ? (p as Extract<QuickPreview, { status: S }>) : null);

/** The leading tile: the picture, or the type's glyph. */
export function QuickPreviewTile(props: { asset: LibraryAsset; preview: () => QuickPreview }) {
  const p = props.preview;
  const id = () => props.asset.id;
  const name = () => props.asset.name;
  const kind = () => fileKindOf(props.asset.kind, props.asset.mimeType);
  return (
    <>
      <If when={p().status === "loading"}>
        <QuickPreviewLoading testID={`quick-preview-loading:${id()}`} accessibilityLabel={previewLabel(name(), p())} />
      </If>
      <If when={p().status === "text" || p().status === "tags"}>
        <QuickTypeTile kind={kind()} />
      </If>
      <If when={as(p(), "image")}>
        {(img) => <QuickImage testID={`quick-preview-image:${id()}`} accessibilityLabel={previewLabel(name(), img())} src={img().url} />}
      </If>
      <If when={as(p(), "video")}>
        {(v) => (
          <QuickVideoStill
            testID={`quick-preview-video:${id()}`}
            accessibilityLabel={previewLabel(name(), v())}
            src={v().url}
            onShown={() => videoShown(id())}
            onError={() => void videoFailed(id())}
          />
        )}
      </If>
      <If when={as(p(), "waveform")}>
        {(w) => <QuickWaveform testID={`quick-preview-wave:${id()}`} accessibilityLabel={previewLabel(name(), w())} peaks={w().waveform.peaks} />}
      </If>
      <If when={as(p(), "none")}>
        {(n) => <QuickTypeTile kind={kind()} testID={`quick-preview-none:${id()}`} accessibilityLabel={previewLabel(name(), n())} />}
      </If>
    </>
  );
}

/** The one quiet line under the file name, when the preview has words to give. */
export function QuickPreviewDetail(props: { asset: LibraryAsset; preview: () => QuickPreview }) {
  const p = props.preview;
  const id = () => props.asset.id;
  const name = () => props.asset.name;
  const testID = () => (p().status === "text" ? `quick-preview-text:${id()}` : p().status === "tags" ? `quick-preview-tags:${id()}` : `quick-preview-detail:${id()}`);
  return (
    <If when={previewDetail(p())}>
      {(text) => <QuickDetailLine testID={testID()} accessibilityLabel={previewLabel(name(), p())} text={text()} />}
    </If>
  );
}

/** Both parts side by side, from one acquisition. */
export function QuickAssetPreview(props: { asset: LibraryAsset }) {
  const preview = useQuickPreview(() => props.asset);
  return (
    <Row gap={2} align="center">
      <QuickPreviewTile asset={props.asset} preview={preview} />
      <QuickPreviewDetail asset={props.asset} preview={preview} />
    </Row>
  );
}
