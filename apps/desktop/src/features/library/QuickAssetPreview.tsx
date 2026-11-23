// A quick-search FILE row's preview: `<QuickAssetPreview asset={asset} />`.
//
// It shows what the file itself holds: the start of its text (plain text,
// Markdown, a Word document's own words), the image, a still of the video
// (its own first frame, paused and muted), or the waveform of a WAV file's
// real samples. Where nothing honest can be drawn (PDF, MP3, an unreadable
// or too-large file, a budget reached, lost access) it says so in a quiet
// line. The bytes are read, shared, bounded and dropped by
// ./quickPreviewStore: mounting reads (once per file), unmounting the last
// row of a file frees it, and sign-out or losing the file's Space frees
// everything at once.

import { cleanup, effect, signal, untracked } from "nsh";
import { QuickImage, QuickNoPreview, QuickPreviewLoading, QuickTextExcerpt, QuickVideoStill, QuickWaveform } from "../../ui";
import { clock } from "./libraryState";
import type { LibraryAsset } from "./libraryState";
import { acquirePreview, releasePreview, videoFailed, videoShown } from "./quickPreviewStore";
import type { QuickPreview } from "./quickPreviewStore";

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
    case "none":
      return `No preview of ${name}. ${p.reason}`;
  }
}

/** The caption beside a waveform: its length, and how much of it is drawn when only the start was read. */
function waveCaption(p: Extract<QuickPreview, { status: "waveform" }>): string {
  const w = p.waveform;
  const len = clock(w.durationMs);
  if (w.silent) return `${len} · silent`;
  return w.coveredMs + 1 < w.durationMs ? `first ${clock(w.coveredMs)} of ${len}` : len;
}

export function QuickAssetPreview(props: { asset: LibraryAsset }) {
  // The preview of the file this row shows now; a new file releases the old one.
  const view = signal<{ id: string; get: () => QuickPreview } | null>(null);
  let held: string | null = null;
  effect(() => {
    const asset = props.asset;
    untracked(() => {
      if (held === asset.id) return;
      if (held) releasePreview(held);
      held = asset.id;
      view.set({ id: asset.id, get: acquirePreview(asset) });
    });
  });
  cleanup(() => {
    if (held) releasePreview(held);
    held = null;
  });
  const p = () => view()?.get() ?? ({ status: "loading" } as QuickPreview);
  const id = () => props.asset.id;
  const name = () => props.asset.name;
  return (
    <>
      <If when={p().status === "loading"}>
        <QuickPreviewLoading testID={`quick-preview-loading:${id()}`} accessibilityLabel={previewLabel(name(), p())} />
      </If>
      <If when={p().status === "text" ? (p() as Extract<QuickPreview, { status: "text" }>) : null}>
        {(t) => <QuickTextExcerpt testID={`quick-preview-text:${id()}`} accessibilityLabel={previewLabel(name(), t())} text={t().text} />}
      </If>
      <If when={p().status === "image" ? (p() as Extract<QuickPreview, { status: "image" }>) : null}>
        {(img) => <QuickImage testID={`quick-preview-image:${id()}`} accessibilityLabel={previewLabel(name(), img())} src={img().url} />}
      </If>
      <If when={p().status === "video" ? (p() as Extract<QuickPreview, { status: "video" }>) : null}>
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
      <If when={p().status === "waveform" ? (p() as Extract<QuickPreview, { status: "waveform" }>) : null}>
        {(w) => <QuickWaveform testID={`quick-preview-wave:${id()}`} accessibilityLabel={previewLabel(name(), w())} peaks={w().waveform.peaks} caption={waveCaption(w())} />}
      </If>
      <If when={p().status === "none" ? (p() as Extract<QuickPreview, { status: "none" }>) : null}>
        {(n) => <QuickNoPreview testID={`quick-preview-none:${id()}`} accessibilityLabel={previewLabel(name(), n())} text={n().reason} />}
      </If>
    </>
  );
}
