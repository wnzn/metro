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
/**
 * Largest `word/document.xml` a DOCX snippet inflates. Enforced while
 * inflating (zlib's `maxOutputLength`, which NSH's zlib applies during
 * decompression and fails past), never taken from the archive's own sizes.
 */
export const DOCX_XML_MAX = 1024 * 1024;

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

/**
 * One file of a ZIP archive (a DOCX is one), read through its central
 * directory, at most `maxBytes` of it. The sizes an archive declares are
 * never trusted as limits: a stored entry's real length is checked, and a
 * deflated one is inflated with a hard output limit, so a small archive that
 * expands enormously fails while inflating instead of allocating. Null when
 * absent, unreadable or over the limit.
 */
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
      // A declared size over the limit is refused early; one under it proves nothing.
      if (size > maxBytes || local + 30 > zip.length || u32(zip, local) !== 0x04034b50) return null;
      const start = local + 30 + u16(zip, local + 26) + u16(zip, local + 28);
      if (start + compressed > zip.length) return null;
      const data = zip.subarray(start, start + compressed);
      if (method === 0) return data.length <= maxBytes ? data : null;
      if (method !== 8) return null;
      try {
        const out = inflateRawSync(data, { maxOutputLength: maxBytes }) as unknown as Uint8Array;
        return out.length <= maxBytes ? new Uint8Array(out) : null;
      } catch {
        return null; // past the limit (a RangeError) or not valid deflate data
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
      // Only real Unicode scalar values: no surrogates, nothing past U+10FFFF, no NUL.
      return Number.isInteger(n) && n > 0 && n <= 0x10ffff && !(n >= 0xd800 && n <= 0xdfff) ? String.fromCodePoint(n) : "\ufffd";
    }
    return ENTITIES[e.toLowerCase()] ?? all;
  });
  return cut(joined, max);
}

// ------------------------------------------------------------------ mp3 tags

/** How much of an MP3's start the tag reader looks at. */
export const ID3_READ_BYTES = 64 * 1024;

export type AudioTags = { title: string | null; artist: string | null; album: string | null };

function id3Text(frame: Uint8Array): string | null {
  if (frame.length < 2) return null;
  const encoding = frame[0]!;
  let body = frame.subarray(1);
  let text: string;
  try {
    if (encoding === 0) text = Array.from(body, (b) => String.fromCharCode(b)).join("");
    else if (encoding === 3) text = new TextDecoder("utf-8", { fatal: true }).decode(body);
    else if (encoding === 1 || encoding === 2) {
      let le = encoding === 1;
      if (encoding === 1 && body.length >= 2) {
        if (body[0] === 0xff && body[1] === 0xfe) body = body.subarray(2);
        else if (body[0] === 0xfe && body[1] === 0xff) {
          le = false;
          body = body.subarray(2);
        }
      }
      const units: number[] = [];
      for (let i = 0; i + 1 < body.length; i += 2) units.push(le ? body[i]! | (body[i + 1]! << 8) : (body[i]! << 8) | body[i + 1]!);
      text = String.fromCharCode(...units.slice(0, 1024));
    } else return null;
  } catch {
    return null;
  }
  // A text frame may hold several values split by NUL: the first one is shown.
  const first = text.split("\u0000")[0]!.replace(/[\u0000-\u001f\u007f]/g, " ").replace(/\s+/g, " ").trim();
  return first ? first.slice(0, 120) : null;
}

/**
 * The title, artist and album from an MP3's ID3v2 tag (v2.2, v2.3, v2.4),
 * read from at most the first ID3_READ_BYTES of the file. Frames past that,
 * compressed or encrypted frames, and pictures are skipped. Null when there's
 * no ID3v2 tag or it names none of the three: the file's sound is never
 * guessed at.
 */
export function id3Tags(head: Uint8Array): AudioTags | null {
  if (head.length < 10 || head[0] !== 0x49 || head[1] !== 0x44 || head[2] !== 0x33) return null;
  const version = head[3]!;
  if (version < 2 || version > 4) return null;
  const flags = head[5]!;
  const syncsafe = (o: number) => ((head[o]! & 0x7f) << 21) | ((head[o + 1]! & 0x7f) << 14) | ((head[o + 2]! & 0x7f) << 7) | (head[o + 3]! & 0x7f);
  const tagSize = syncsafe(6);
  let tag = head.subarray(10, Math.min(head.length, 10 + tagSize));
  if (flags & 0x80) {
    // Unsynchronisation: every 0xFF 0x00 stands for 0xFF.
    const out: number[] = [];
    for (let i = 0; i < tag.length; i++) {
      out.push(tag[i]!);
      if (tag[i] === 0xff && tag[i + 1] === 0x00) i++;
    }
    tag = new Uint8Array(out);
  }
  let o = 0;
  if (flags & 0x40 && version >= 3 && tag.length >= 4) {
    const ext = version === 4 ? ((tag[0]! & 0x7f) << 21) | ((tag[1]! & 0x7f) << 14) | ((tag[2]! & 0x7f) << 7) | (tag[3]! & 0x7f) : ((tag[0]! << 24) | (tag[1]! << 16) | (tag[2]! << 8) | tag[3]!) + 4;
    o = ext;
  }
  const ids = version === 2 ? { title: "TT2", artist: "TP1", album: "TAL" } : { title: "TIT2", artist: "TPE1", album: "TALB" };
  const found: AudioTags = { title: null, artist: null, album: null };
  const idLen = version === 2 ? 3 : 4;
  const headLen = version === 2 ? 6 : 10;
  for (let n = 0; n < 256 && o + headLen <= tag.length; n++) {
    const id = String.fromCharCode(...tag.subarray(o, o + idLen));
    if (!/^[A-Z0-9]+$/.test(id)) break; // padding or the end
    const size =
      version === 2
        ? (tag[o + 3]! << 16) | (tag[o + 4]! << 8) | tag[o + 5]!
        : version === 4
          ? ((tag[o + 4]! & 0x7f) << 21) | ((tag[o + 5]! & 0x7f) << 14) | ((tag[o + 6]! & 0x7f) << 7) | (tag[o + 7]! & 0x7f)
          : ((tag[o + 4]! << 24) | (tag[o + 5]! << 16) | (tag[o + 6]! << 8) | tag[o + 7]!) >>> 0;
    const body = o + headLen;
    if (size <= 0 || body + size > tag.length) break;
    const frameFlags = version === 2 ? 0 : tag[o + 9]!;
    // v2.3: 0x80 compressed, 0x40 encrypted; v2.4: 0x08 compressed, 0x04 encrypted, 0x02 unsynchronised, 0x01 length added.
    const skip = version === 3 ? (frameFlags & 0xc0) !== 0 : version === 4 ? (frameFlags & 0x0f) !== 0 : false;
    if (!skip) {
      for (const key of ["title", "artist", "album"] as const) {
        if (id === ids[key] && found[key] === null) found[key] = id3Text(tag.subarray(body, body + size));
      }
    }
    o = body + size;
  }
  return found.title || found.artist || found.album ? found : null;
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
