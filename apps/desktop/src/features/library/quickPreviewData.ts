// What a quick preview shows, computed from a file's own bytes: a short text
// snippet (plain text, Markdown, the text of a DOCX), and the amplitude of a
// WAV file's samples. Pure functions: no I/O, no UI, nothing sent anywhere.
// Anything they can't read returns null, so the caller says so honestly
// instead of drawing something made up.

import { inflateRawSync } from "zlib";

/** How many characters a snippet keeps by default (a short, readable excerpt). */
export const SNIPPET_CHARS = 600;
/** How many bars a waveform has. */
export const WAVE_BARS = 40;
/** Samples read for one waveform at most, so a long file costs the same as a short one. */
export const WAVE_SAMPLE_BUDGET = 200_000;
/** Peaks below this (about -60 dBFS) count as silence. */
export const SILENCE = 0.001;
/** Largest `word/document.xml` a DOCX snippet inflates. */
export const DOCX_XML_MAX = 4 * 1024 * 1024;

// ------------------------------------------------------------------ text

/** Whitespace folded, cut to `max` characters with an ellipsis when longer; null when nothing is left. */
function cut(text: string, max = SNIPPET_CHARS): string | null {
  const flat = text.replace(/\s+/g, " ").trim();
  if (!flat) return null;
  return flat.length > max ? `${flat.slice(0, max - 1).trimEnd()}…` : flat;
}

/** Markdown's own marks dropped, so the snippet reads as text. */
function plainMarkdown(md: string): string {
  return md
    .replace(/```[^\n]*\n?/g, " ")
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/^\s{0,3}(#{1,6}|>|[-*+]|\d+[.)])\s+/gm, "")
    .replace(/[*_`~]+/g, "");
}

/** Decodes UTF-8, allowing a character cut at the very end (a prefix of a file). Null when it isn't UTF-8. */
function utf8Prefix(bytes: Uint8Array): string | null {
  for (let trim = 0; trim <= 3 && trim < bytes.length; trim++) {
    try {
      return new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(0, bytes.length - trim));
    } catch {
      /* try without the last bytes: a cut character */
    }
  }
  return null;
}

/** The start of a text or Markdown file. Null when it isn't UTF-8 text or is empty. */
export function textSnippet(bytes: Uint8Array, markdown: boolean, max = SNIPPET_CHARS): string | null {
  const head = utf8Prefix(bytes.subarray(0, 16 * 1024));
  if (head === null || head.includes("\u0000")) return null;
  return cut(markdown ? plainMarkdown(head) : head, max);
}

// ------------------------------------------------------------------ docx

const u16 = (b: Uint8Array, o: number) => b[o]! | (b[o + 1]! << 8);
const u32 = (b: Uint8Array, o: number) => (b[o]! | (b[o + 1]! << 8) | (b[o + 2]! << 16) | (b[o + 3]! << 24)) >>> 0;

/** One file of a ZIP archive (a DOCX is one), read through its central directory. Null when absent or unreadable. */
export function zipEntry(zip: Uint8Array, name: string, maxBytes: number): Uint8Array | null {
  // End of central directory: the last 22 bytes, or before a comment of up to 65535.
  let eocd = -1;
  for (let o = zip.length - 22; o >= Math.max(0, zip.length - 22 - 65535); o--) {
    if (u32(zip, o) === 0x06054b50) {
      eocd = o;
      break;
    }
  }
  if (eocd < 0) return null;
  const count = u16(zip, eocd + 10);
  let o = u32(zip, eocd + 16);
  for (let i = 0; i < count && o + 46 <= zip.length; i++) {
    if (u32(zip, o) !== 0x02014b50) return null;
    const method = u16(zip, o + 10);
    const compressed = u32(zip, o + 20);
    const size = u32(zip, o + 24);
    const nameLen = u16(zip, o + 28);
    const extraLen = u16(zip, o + 30);
    const commentLen = u16(zip, o + 32);
    const local = u32(zip, o + 42);
    const entryName = new TextDecoder().decode(zip.subarray(o + 46, o + 46 + nameLen));
    if (entryName === name) {
      if (size > maxBytes || local + 30 > zip.length || u32(zip, local) !== 0x04034b50) return null;
      const start = local + 30 + u16(zip, local + 26) + u16(zip, local + 28);
      if (start + compressed > zip.length) return null;
      const data = zip.subarray(start, start + compressed);
      if (method === 0) return data;
      if (method !== 8) return null;
      try {
        const out = inflateRawSync(data) as unknown as Uint8Array;
        return out.length > maxBytes ? null : new Uint8Array(out);
      } catch {
        return null;
      }
    }
    o += 46 + nameLen + extraLen + commentLen;
  }
  return null;
}

const ENTITIES: Record<string, string> = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'" };

/** The start of a Word document's own text (word/document.xml). Null when it isn't a readable DOCX or has no text. */
export function docxSnippet(bytes: Uint8Array, max = SNIPPET_CHARS): string | null {
  const xml = zipEntry(bytes, "word/document.xml", DOCX_XML_MAX);
  if (!xml) return null;
  const text = new TextDecoder().decode(xml);
  const runs: string[] = [];
  const re = /<w:t(?:\s[^>]*)?>([^<]*)<\/w:t>|<w:tab\/>|<\/w:p>/g;
  for (const m of text.matchAll(re)) {
    runs.push(m[1] !== undefined ? m[1] : " ");
    if (runs.length > 2000) break;
  }
  const joined = runs.join("").replace(/&(#x[0-9a-f]+|#\d+|\w+);/gi, (all, e: string) => {
    if (e[0] === "#") {
      const n = e[1] === "x" || e[1] === "X" ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10);
      return Number.isFinite(n) ? String.fromCodePoint(n) : all;
    }
    return ENTITIES[e.toLowerCase()] ?? all;
  });
  return cut(joined, max);
}

// ------------------------------------------------------------------ wav

export type Waveform = {
  /** Each bar's loudest sample, 0..1 of full scale (not scaled to the loudest bar), across `coveredMs`. */
  peaks: number[];
  /** The whole recording's length, from its header. */
  durationMs: number;
  /** How much of it the bytes held (a prefix of the file reads only its start). */
  coveredMs: number;
  /** Every sample is below SILENCE. */
  silent: boolean;
};

type Fmt = { format: number; channels: number; rate: number; align: number; bits: number };

/** A sample reader for one PCM layout, giving -1..1; null when the layout isn't supported. */
function reader(fmt: Fmt, dv: DataView): ((offset: number) => number) | null {
  const { format, bits } = fmt;
  if (format === 1) {
    if (bits === 8) return (o) => (dv.getUint8(o) - 128) / 128;
    if (bits === 16) return (o) => dv.getInt16(o, true) / 32768;
    if (bits === 24) return (o) => ((dv.getUint8(o) | (dv.getUint8(o + 1) << 8) | (dv.getInt8(o + 2) << 16)) / 8388608);
    if (bits === 32) return (o) => dv.getInt32(o, true) / 2147483648;
  }
  if (format === 3) {
    if (bits === 32) return (o) => dv.getFloat32(o, true);
    if (bits === 64) return (o) => dv.getFloat64(o, true);
  }
  return null;
}

/**
 * The amplitude of a WAV file's samples (PCM 8/16/24/32-bit, float 32/64,
 * WAVE_FORMAT_EXTENSIBLE of those), as `bars` peaks across its length.
 * Reads at most WAVE_SAMPLE_BUDGET samples, evenly spread. `bytes` may be a
 * prefix of the file: the bars then cover only the samples it holds
 * (`coveredMs`). Null when the bytes aren't a WAV file this can read.
 */
export function wavWaveform(bytes: Uint8Array, bars = WAVE_BARS): Waveform | null {
  if (bytes.length < 12) return null;
  const tag = (o: number) => String.fromCharCode(bytes[o]!, bytes[o + 1]!, bytes[o + 2]!, bytes[o + 3]!);
  if (tag(0) !== "RIFF" || tag(8) !== "WAVE") return null;
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let fmt: Fmt | null = null;
  let data: { start: number; length: number; declared: number } | null = null;
  for (let o = 12; o + 8 <= bytes.length && !(fmt && data); ) {
    const id = tag(o);
    const size = dv.getUint32(o + 4, true);
    const body = o + 8;
    if (id === "fmt " && size >= 16 && body + 16 <= bytes.length) {
      let format = dv.getUint16(body, true);
      if (format === 0xfffe && size >= 40 && body + 26 <= bytes.length) format = dv.getUint16(body + 24, true);
      fmt = { format, channels: dv.getUint16(body + 2, true), rate: dv.getUint32(body + 4, true), align: dv.getUint16(body + 12, true), bits: dv.getUint16(body + 14, true) };
    } else if (id === "data") {
      data = { start: body, length: Math.max(0, Math.min(size, bytes.length - body)), declared: size };
    }
    o = body + size + (size & 1);
  }
  if (!fmt || !data) return null;
  const { channels, rate, align, bits } = fmt;
  if (channels < 1 || channels > 8 || rate <= 0 || bits % 8 !== 0 || align !== (channels * bits) / 8) return null;
  const read = reader(fmt, dv);
  if (!read) return null;
  const frames = Math.floor(data.length / align);
  const step = Math.max(1, Math.ceil((frames * channels) / WAVE_SAMPLE_BUDGET));
  const width = bits / 8;
  const peaks: number[] = [];
  let loudest = 0;
  for (let i = 0; i < bars; i++) {
    const from = Math.floor((i * frames) / bars);
    const to = Math.floor(((i + 1) * frames) / bars);
    let peak = 0;
    for (let f = from; f < to; f += step) {
      const base = data.start + f * align;
      for (let c = 0; c < channels; c++) {
        const v = Math.abs(read(base + c * width));
        if (Number.isFinite(v) && v > peak) peak = v;
      }
    }
    peak = Math.min(1, peak);
    peaks.push(peak);
    if (peak > loudest) loudest = peak;
  }
  if (frames === 0) return null;
  return { peaks, durationMs: (Math.floor(data.declared / align) / rate) * 1000, coveredMs: (frames / rate) * 1000, silent: loudest < SILENCE };
}
