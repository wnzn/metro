// The quick preview decoders against real bytes built here: WAV files of a
// sine and of silence in every supported layout, and DOCX archives deflated
// with zlib. No network, no UI.

import { expect, test } from "nsh/test";
import { deflateRawSync } from "zlib";
import { docxSnippet, id3Tags, SILENCE, textSnippet, WAVE_BARS, wavWaveform, zipEntry } from "./quickPreviewData";

type Layout = { format: 1 | 3; bits: 8 | 16 | 24 | 32 | 64; channels: number; rate: number; extensible?: boolean };

/** A WAV file whose samples are `sample(i, channel)` (-1..1). */
function wav(layout: Layout, frames: number, sample: (i: number, c: number) => number): Uint8Array {
  const width = layout.bits / 8;
  const align = width * layout.channels;
  const fmtSize = layout.extensible ? 40 : 16;
  const out = new Uint8Array(12 + 8 + fmtSize + 8 + frames * align);
  const dv = new DataView(out.buffer);
  const tag = (o: number, s: string) => [...s].forEach((ch, i) => (out[o + i] = ch.charCodeAt(0)));
  tag(0, "RIFF");
  dv.setUint32(4, out.length - 8, true);
  tag(8, "WAVE");
  tag(12, "fmt ");
  dv.setUint32(16, fmtSize, true);
  dv.setUint16(20, layout.extensible ? 0xfffe : layout.format, true);
  dv.setUint16(22, layout.channels, true);
  dv.setUint32(24, layout.rate, true);
  dv.setUint32(28, layout.rate * align, true);
  dv.setUint16(32, align, true);
  dv.setUint16(34, layout.bits, true);
  if (layout.extensible) dv.setUint16(20 + 24, layout.format, true);
  const d = 20 + fmtSize;
  tag(d, "data");
  dv.setUint32(d + 4, frames * align, true);
  for (let i = 0; i < frames; i++) {
    for (let c = 0; c < layout.channels; c++) {
      const v = Math.max(-1, Math.min(1, sample(i, c)));
      const o = d + 8 + i * align + c * width;
      if (layout.format === 3) layout.bits === 32 ? dv.setFloat32(o, v, true) : dv.setFloat64(o, v, true);
      else if (layout.bits === 8) dv.setUint8(o, Math.round(v * 127) + 128);
      else if (layout.bits === 16) dv.setInt16(o, Math.round(v * 32767), true);
      else if (layout.bits === 24) {
        const n = Math.round(v * 8388607);
        dv.setUint8(o, n & 0xff);
        dv.setUint8(o + 1, (n >> 8) & 0xff);
        dv.setInt8(o + 2, n >> 16);
      } else dv.setInt32(o, Math.round(v * 2147483647), true);
    }
  }
  return out;
}

const sine = (amp: number, hz: number, rate: number) => (i: number) => amp * Math.sin((2 * Math.PI * hz * i) / rate);

test("a sine's waveform is its real amplitude across the file; silence is flat and says so", () => {
  const rate = 8000;
  const loud = wavWaveform(wav({ format: 1, bits: 16, channels: 1, rate }, 2000, sine(0.5, 440, rate)))!;
  expect(loud.peaks).toHaveLength(WAVE_BARS);
  expect(loud.durationMs).toBe(250);
  expect(loud.silent).toBe(false);
  for (const p of loud.peaks) {
    expect(p).toBeGreaterThan(0.45);
    expect(p).toBeLessThanOrEqual(0.51);
  }
  const quiet = wavWaveform(wav({ format: 1, bits: 16, channels: 1, rate }, 2000, () => 0))!;
  expect(quiet.silent).toBe(true);
  expect(quiet.peaks.every((p) => p < SILENCE)).toBe(true);
});

test("a sound that stops halfway shows loud bars, then flat ones (not scaled to the loudest)", () => {
  const rate = 8000;
  const w = wavWaveform(wav({ format: 1, bits: 16, channels: 1, rate }, 4000, (i) => (i < 2000 ? sine(0.25, 300, rate)(i) : 0)))!;
  expect(w.peaks.slice(0, 19).every((p) => p > 0.2 && p < 0.26)).toBe(true);
  expect(w.peaks.slice(21).every((p) => p === 0)).toBe(true);
});

test("a prefix of a WAV draws only what it holds and says how much of the whole that is", () => {
  const rate = 8000;
  const full = wav({ format: 1, bits: 16, channels: 1, rate }, 8000, sine(0.5, 440, rate)); // 1 s
  const w = wavWaveform(full.subarray(0, 44 + 4000))!; // the header and 2000 frames
  expect(w.durationMs).toBe(1000);
  expect(w.coveredMs).toBe(250);
  expect(w.peaks.every((p) => p > 0.45)).toBe(true);
});

test("8-, 24- and 32-bit PCM, 32- and 64-bit float, extensible headers and stereo read the same amplitude", () => {
  const rate = 4000;
  const layouts: Layout[] = [
    { format: 1, bits: 8, channels: 1, rate },
    { format: 1, bits: 24, channels: 1, rate },
    { format: 1, bits: 32, channels: 1, rate },
    { format: 3, bits: 32, channels: 1, rate },
    { format: 3, bits: 64, channels: 1, rate },
    { format: 1, bits: 16, channels: 1, rate, extensible: true },
  ];
  for (const l of layouts) {
    const w = wavWaveform(wav(l, 1000, sine(0.8, 200, rate)))!;
    expect(w).toBeDefined();
    expect(Math.max(...w.peaks)).toBeGreaterThan(0.75);
    expect(Math.max(...w.peaks)).toBeLessThanOrEqual(0.81);
  }
  // Stereo: only the right channel sounds; the peak is still found.
  const st = wavWaveform(wav({ format: 1, bits: 16, channels: 2, rate }, 1000, (i, c) => (c === 1 ? sine(0.6, 200, rate)(i) : 0)))!;
  expect(Math.max(...st.peaks)).toBeGreaterThan(0.55);
});

test("bytes that aren't a readable WAV give no waveform", () => {
  expect(wavWaveform(new TextEncoder().encode("ID3 not a wav at all"))).toBeNull();
  expect(wavWaveform(new Uint8Array(0))).toBeNull();
  const ok = wav({ format: 1, bits: 16, channels: 1, rate: 8000 }, 100, () => 0.1);
  expect(wavWaveform(ok.subarray(0, 30))).toBeNull(); // cut before the data chunk
  expect(wavWaveform(ok.subarray(0, 44))).toBeNull(); // a data chunk with no sample in hand
  const adpcm = ok.slice();
  new DataView(adpcm.buffer).setUint16(20, 2, true); // MS ADPCM: not decoded here
  expect(wavWaveform(adpcm)).toBeNull();
});

/** A ZIP archive of `files`, each deflated (or stored); `declare` overrides the uncompressed size it claims. */
function zip(files: Record<string, string>, store = false, declare?: number): Uint8Array {
  const enc = new TextEncoder();
  const locals: Uint8Array[] = [];
  const centrals: Uint8Array[] = [];
  let offset = 0;
  for (const [name, text] of Object.entries(files)) {
    const raw = enc.encode(text);
    const data = store ? raw : new Uint8Array(deflateRawSync(raw) as unknown as Uint8Array);
    const n = enc.encode(name);
    const local = new Uint8Array(30 + n.length + data.length);
    const lv = new DataView(local.buffer);
    lv.setUint32(0, 0x04034b50, true);
    lv.setUint16(8, store ? 0 : 8, true);
    lv.setUint32(18, data.length, true);
    lv.setUint32(22, declare ?? raw.length, true);
    lv.setUint16(26, n.length, true);
    local.set(n, 30);
    local.set(data, 30 + n.length);
    const central = new Uint8Array(46 + n.length);
    const cv = new DataView(central.buffer);
    cv.setUint32(0, 0x02014b50, true);
    cv.setUint16(10, store ? 0 : 8, true);
    cv.setUint32(20, data.length, true);
    cv.setUint32(24, declare ?? raw.length, true);
    cv.setUint16(28, n.length, true);
    cv.setUint32(42, offset, true);
    central.set(n, 46);
    locals.push(local);
    centrals.push(central);
    offset += local.length;
  }
  const cdSize = centrals.reduce((s, c) => s + c.length, 0);
  const end = new Uint8Array(22);
  const ev = new DataView(end.buffer);
  ev.setUint32(0, 0x06054b50, true);
  ev.setUint16(8, centrals.length, true);
  ev.setUint16(10, centrals.length, true);
  ev.setUint32(12, cdSize, true);
  ev.setUint32(16, offset, true);
  const out = new Uint8Array(offset + cdSize + 22);
  let at = 0;
  for (const part of [...locals, ...centrals, end]) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

const DOC = `<?xml version="1.0"?><w:document xmlns:w="x"><w:body><w:p><w:r><w:t>Termination</w:t></w:r><w:r><w:t xml:space="preserve"> clause &amp; notice</w:t></w:r></w:p><w:p><w:r><w:t>Either party may end it.</w:t></w:r></w:p></w:body></w:document>`;

test("a DOCX snippet is the document's own text, deflated or stored; anything else gives none", () => {
  const files = { "[Content_Types].xml": "<Types/>", "word/document.xml": DOC };
  expect(docxSnippet(zip(files))).toBe("Termination clause & notice Either party may end it.");
  expect(docxSnippet(zip(files, true))).toBe("Termination clause & notice Either party may end it.");
  expect(docxSnippet(zip({ "word/other.xml": DOC }))).toBeNull();
  expect(docxSnippet(new TextEncoder().encode("PK not really a zip"))).toBeNull();
  expect(zipEntry(zip(files), "word/document.xml", 10)).toBeNull(); // over the inflate budget
});

test("text snippets fold whitespace, drop Markdown marks, cut long text, and refuse non-text", () => {
  const enc = new TextEncoder();
  expect(textSnippet(enc.encode("# Rollback plan\n\n- revert the **tag**\n- see [docs](http://x)"), true)).toBe("Rollback plan revert the tag see docs");
  expect(textSnippet(enc.encode("a\tb\n\nc"), false)).toBe("a b c");
  const long = textSnippet(enc.encode("word ".repeat(300)), false)!;
  expect(long.length).toBeLessThanOrEqual(600);
  expect(long.endsWith("…")).toBe(true);
  expect(textSnippet(new Uint8Array([0xff, 0xfe, 0x00, 0x41]), false)).toBeNull();
  expect(textSnippet(enc.encode("   \n "), false)).toBeNull();
  // A multi-byte character cut by the read limit is dropped, not refused.
  const cutChar = enc.encode("café").subarray(0, 4);
  expect(textSnippet(cutChar, false)).toBe("caf");
});

test("inflating is bounded while it runs, whatever sizes the archive claims (small, benign thresholds)", () => {
  const big = { "word/document.xml": `<w:t>${"a".repeat(4096)}</w:t>` }; // 4 KB that deflates to a few bytes
  // Honest sizes over the limit: refused.
  expect(zipEntry(zip(big), "word/document.xml", 1024)).toBeNull();
  // The archive claims 10 bytes: inflating still stops at the limit instead of trusting it.
  expect(zipEntry(zip(big, false, 10), "word/document.xml", 1024)).toBeNull();
  // A stored entry claiming 10 bytes: its real length is checked.
  expect(zipEntry(zip(big, true, 10), "word/document.xml", 1024)).toBeNull();
  // Under the limit, both read.
  expect(zipEntry(zip(big, false, 10), "word/document.xml", 8192)!.length).toBe(4096 + 11);
  expect(zipEntry(zip(big, true), "word/document.xml", 8192)!.length).toBe(4096 + 11);
  // Not deflate data at all: none, no throw.
  const broken = zip(big);
  broken.fill(0x55, 30 + "word/document.xml".length, 30 + "word/document.xml".length + 4);
  expect(zipEntry(broken, "word/document.xml", 8192)).toBeNull();
});

test("numeric entities outside Unicode scalar values never throw; they read as U+FFFD", () => {
  const doc = `<w:document><w:p><w:t>a&#xD800;b&#x110000;c&#0;d&#99999999999;e&#x41;</w:t></w:p></w:document>`;
  expect(docxSnippet(zip({ "word/document.xml": doc }))).toBe("a\ufffdb\ufffdc\ufffdd\ufffdeA");
});

/** An ID3v2 tag of `frames` (id, encoding byte, text bytes). */
function id3(version: 2 | 3 | 4, frames: [string, number, Uint8Array][], flags = 0): Uint8Array {
  const parts: number[] = [];
  for (const [id, enc, text] of frames) {
    const size = text.length + 1;
    parts.push(...[...id].map((c) => c.charCodeAt(0)));
    if (version === 2) parts.push((size >> 16) & 0xff, (size >> 8) & 0xff, size & 0xff);
    else if (version === 4) parts.push((size >> 21) & 0x7f, (size >> 14) & 0x7f, (size >> 7) & 0x7f, size & 0x7f, 0, 0);
    else parts.push((size >>> 24) & 0xff, (size >> 16) & 0xff, (size >> 8) & 0xff, size & 0xff, 0, flags);
    parts.push(enc, ...text);
  }
  const n = parts.length;
  return new Uint8Array([0x49, 0x44, 0x33, version, 0, 0, (n >> 21) & 0x7f, (n >> 14) & 0x7f, (n >> 7) & 0x7f, n & 0x7f, ...parts, 0xff, 0xfb, 0x90, 0x00]);
}
const latin1 = (t: string) => new Uint8Array([...t].map((c) => c.charCodeAt(0)));
const utf16le = (t: string) => new Uint8Array([0xff, 0xfe, ...[...t].flatMap((c) => [c.charCodeAt(0) & 0xff, c.charCodeAt(0) >> 8])]);

test("an MP3's ID3 title, artist and album are read in each tag version and text encoding", () => {
  expect(id3Tags(id3(3, [["TIT2", 0, latin1("Harbor Song")], ["TPE1", 1, utf16le("Ana Reyes")], ["TALB", 0, latin1("Dockside")]]))).toEqual({ title: "Harbor Song", artist: "Ana Reyes", album: "Dockside" });
  expect(id3Tags(id3(4, [["TIT2", 3, new TextEncoder().encode("Café Night")]]))).toEqual({ title: "Café Night", artist: null, album: null });
  expect(id3Tags(id3(2, [["TT2", 0, latin1("Old Tag")], ["TP1", 0, latin1("Band")]]))).toEqual({ title: "Old Tag", artist: "Band", album: null });
});

test("no tag, an empty tag, or only compressed frames give no tags; a frame past the bytes read is skipped", () => {
  expect(id3Tags(new Uint8Array([0xff, 0xfb, 0x90, 0x00, 1, 2, 3, 4, 5, 6, 7]))).toBeNull(); // plain MPEG frames
  expect(id3Tags(id3(3, [["TXXX", 0, latin1("other")]]))).toBeNull();
  expect(id3Tags(id3(3, [["TIT2", 0, latin1("Hidden")]], 0x80))).toBeNull(); // compressed frame: not read
  const cut = id3(3, [["TIT2", 0, latin1("Long title")]]);
  expect(id3Tags(cut.subarray(0, 16))).toBeNull();
});
