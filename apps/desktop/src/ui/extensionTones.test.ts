// The colour families of file-extension badges (quick search): which
// extensions share one, and that every family's ink reads on its fill in
// both themes (theme/theme.json `ext-*`).

import { expect, test } from "nsh/test";
import theme from "../../theme/theme.json";
import { extensionTone } from "./chrome";
import type { ExtensionTone } from "./chrome";

type Palette = Record<string, string>;
const light = theme.cssVars.light as Palette;
const dark = theme.cssVars.dark as Palette;

function luminance(hex: string) {
  const [r, g, b] = [1, 3, 5].map((i) => {
    const c = parseInt(hex.slice(i, i + 2), 16) / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}
function contrast(a: string, b: string) {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
}

const FAMILIES: ExtensionTone[] = ["text", "markdown", "pdf", "word", "image", "audio", "video", "webm"];

test("extensions map to their family, case-insensitive, with or without the dot; aliases match; unknown is neutral", () => {
  const cases: [string, ExtensionTone][] = [
    [".txt", "text"],
    [".md", "markdown"],
    [".MD", "markdown"],
    [".pdf", "pdf"],
    [".docx", "word"],
    [".png", "image"],
    [".jpg", "image"],
    [".JPEG", "image"],
    ["jpeg", "image"],
    [".wav", "audio"],
    [".mp3", "audio"],
    [".mp4", "video"],
    [".webm", "webm"],
    [".zip", "neutral"],
    ["", "neutral"],
  ];
  for (const [ext, tone] of cases) expect(extensionTone(ext)).toBe(tone);
  expect(extensionTone(".jpg")).toBe(extensionTone(".jpeg"));
  expect(extensionTone(".mp4")).not.toBe(extensionTone(".webm")); // related tints, not one
});

test("every family's ink reads on its fill (4.5:1) in both themes, and the fills differ from each other and the popover", () => {
  for (const palette of [light, dark]) {
    const fills = new Set<string>();
    for (const f of FAMILIES) {
      const fill = palette[`ext-${f}`]!;
      const ink = palette[`ext-${f}-foreground`]!;
      expect(fill).toBeTruthy();
      expect(contrast(ink, fill)).toBeGreaterThanOrEqual(4.5);
      expect(fill).not.toBe(palette.popover ?? palette.card);
      fills.add(fill.toLowerCase());
    }
    expect(fills.size).toBe(FAMILIES.length);
  }
});
