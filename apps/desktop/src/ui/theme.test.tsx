// Theme and appearance: the colors NSH shows (viewStyle / spanStyle), not
// just the stored preference. Headless; screenshots of the native window are
// taken separately (dev/screenshots).

import { expect, open, test } from "nsh/test";
import type { App, ColorScheme } from "nsh/test";
import theme from "../../theme/theme.json";
import { APPEARANCE_KEY } from "./appearance";

type Palette = Record<string, string>;
const light = theme.cssVars.light as Palette;
const dark = theme.cssVars.dark as Palette;
const rgba = (hex: string) => `${hex.toLowerCase()}ff`;

const FIXTURE = "./theme.fixture.tsx";
const openFixture = (storage: Record<string, string> = {}, colorScheme: ColorScheme = "light") =>
  open(FIXTURE, { platform: "linux", width: 1100, height: 760, storage, colorScheme });

const bg = (app: App, selector: string) => app.viewStyle(selector)?.backgroundColor ?? null;
const fg = (app: App, selector: string) => app.spanStyle(selector).color;

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

async function expectScheme(app: App, palette: Palette) {
  await app.waitUntil(() => bg(app, "id:fx-panel") === rgba(palette.background));
  expect(bg(app, "id:fx-app")).toBe(rgba(palette.background));
  expect(bg(app, "id:fx-sidebar")).toBe(rgba(palette.sidebar));
  expect(bg(app, "id:fx-card")).toBe(rgba(palette.card));
  expect(bg(app, "id:fx-nav")).toBe(rgba(palette["sidebar-accent"]));
  expect(bg(app, "id:fx-primary")).toBe(rgba(palette.primary));
  expect(fg(app, "text:Fixture body")).toBe(rgba(palette.foreground));
  expect(fg(app, "text:Fixture quiet page")).toBe(rgba(palette["muted-foreground"]));
  expect(fg(app, "id:fx-primary >> text:Primary")).toBe(rgba(palette["primary-foreground"]));
}

test("dark is the default, even when the system is light", async () => {
  const app = await openFixture({}, "light");
  await expectScheme(app, dark);
  expect(bg(app, "id:appearance:dark")).toBe(rgba(dark.primary)); // the checked option
  expect(app.shows("text:Dark")).toBe(true);
  expect(app.storage()[APPEARANCE_KEY]).toBeUndefined(); // nothing written until the user chooses
});

test("the real Connect screen opens dark on a light system", async () => {
  const app = await open("../../App.tsx", { platform: "linux", width: 1100, height: 760, colorScheme: "light" });
  await app.waitFor("id:connect-screen");
  await app.waitUntil(() => bg(app, "id:connect-screen") === rgba(dark.background));
  expect(fg(app, "text:Connect to Metro")).toBe(rgba(dark.foreground));
});

test("choosing Light repaints every surface and is saved", async () => {
  const app = await openFixture({}, "dark");
  await expectScheme(app, dark);
  await app.press("id:appearance:light");
  await expectScheme(app, light);
  expect(app.storage()[APPEARANCE_KEY]).toBe("light");
  // Still the primary fill (and readable) with the pointer left on it.
  await app.hover("id:appearance:light");
  expect(bg(app, "id:appearance:light")).toBe(rgba(light.primary));
  expect(fg(app, "id:appearance:light >> text:Light")).toBe(rgba(light["primary-foreground"]));
  expect(bg(app, "id:appearance:dark") ?? "transparent").not.toBe(rgba(light.primary));
  await app.press("id:appearance:dark");
  await expectScheme(app, dark);
  expect(app.storage()[APPEARANCE_KEY]).toBe("dark");
});

test("a saved Light preference survives a restart", async () => {
  const app = await openFixture({ [APPEARANCE_KEY]: "light" }, "dark");
  await expectScheme(app, light);
  expect(app.viewStyle("id:appearance:light")?.backgroundColor).toBe(rgba(light.primary));
});

test("System follows the operating system live", async () => {
  const app = await openFixture({ [APPEARANCE_KEY]: "system" }, "dark");
  await expectScheme(app, dark);
  await app.waitFor("text:Dark, from your system");
  await app.setColorScheme("light");
  await expectScheme(app, light);
  await app.waitFor("text:Light, from your system");
  // An explicit choice stops following it.
  await app.press("id:appearance:dark");
  await app.setColorScheme("light");
  await expectScheme(app, dark);
});

test("an unknown stored value falls back to dark", async () => {
  const app = await openFixture({ [APPEARANCE_KEY]: "neon" }, "light");
  await expectScheme(app, dark);
});

test("the shown text colors meet WCAG AA on their surfaces, in both schemes", async () => {
  for (const [scheme, palette] of [["dark", dark], ["light", light]] as const) {
    const app = await openFixture({ [APPEARANCE_KEY]: scheme });
    await expectScheme(app, palette);
    const pairs: [string, string, string][] = [
      ["body", fg(app, "text:Fixture body"), bg(app, "id:fx-panel")!],
      ["muted", fg(app, "text:Fixture quiet page"), bg(app, "id:fx-panel")!],
      ["card", fg(app, "text:Fixture card"), bg(app, "id:fx-card")!],
      ["muted on card", fg(app, "text:Fixture quiet card"), bg(app, "id:fx-card")!],
      ["primary button", fg(app, "id:fx-primary >> text:Primary"), bg(app, "id:fx-primary")!],
    ];
    for (const [name, text, surface] of pairs) {
      const ratio = contrast(text, surface);
      if (ratio < 4.5) throw new Error(`${scheme} ${name}: ${text} on ${surface} is ${ratio.toFixed(2)}:1`);
    }
  }
});

test("every token pair the primitives draw meets WCAG AA in both schemes", () => {
  const pairs = [
    ["foreground", "background"],
    ["muted-foreground", "background"],
    ["card-foreground", "card"],
    ["muted-foreground", "card"],
    ["muted-foreground", "muted"],
    ["popover-foreground", "popover"],
    ["primary-foreground", "primary"],
    ["secondary-foreground", "secondary"],
    ["accent-foreground", "accent"],
    ["destructive-foreground", "destructive"],
    ["sidebar-foreground", "sidebar"],
    ["sidebar-accent-foreground", "sidebar-accent"],
    ["muted-foreground", "sidebar"],
    ...["neutral", "agent", "runner", "system", "success", "warning", "danger"].map((t) => [`tone-${t}-foreground`, `tone-${t}`]),
  ];
  const failures: string[] = [];
  for (const [scheme, palette] of [["dark", dark], ["light", light]] as const) {
    for (const [text, surface] of pairs) {
      const ratio = contrast(palette[text], palette[surface]);
      if (ratio < 4.5) failures.push(`${scheme} ${text} on ${surface}: ${ratio.toFixed(2)}`);
    }
    // The focus ring must stand out from the page (WCAG 1.4.11, 3:1).
    const ring = contrast(palette.ring, palette.background);
    if (ring < 3) failures.push(`${scheme} ring on background: ${ring.toFixed(2)}`);
  }
  expect(failures).toEqual([]);
});

test("both schemes define the same tokens", () => {
  expect(Object.keys(light).sort()).toEqual(Object.keys(dark).sort());
});
