// Theme and appearance: the colors NSH shows (viewStyle / spanStyle), not
// just the stored preference. Headless; screenshots of the native window are
// taken separately (dev/screenshots).

import { expect, open, test } from "nsh/test";
import type { App, ColorScheme } from "nsh/test";
import theme from "../../theme/theme.json";
import { APPEARANCE_KEY } from "./appearance";
import { spaceTileClass, spaceTileFor } from "./tokens";
import { launched } from "../../test/launched";

type Palette = Record<string, string>;
const light = theme.cssVars.light as Palette;
const dark = theme.cssVars.dark as Palette;
const rgba = (hex: string) => `${hex.toLowerCase()}ff`;

const FIXTURE = "./theme.fixture.tsx";
const openFixture = (storage: Record<string, string> = {}, colorScheme: ColorScheme = "light") =>
  open(FIXTURE, { platform: "linux", width: 1280, height: 1400, storage, colorScheme });

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
  const app = await open("../../App.tsx", { modules: launched(), platform: "linux", width: 1100, height: 760, colorScheme: "light" });
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

// Every text color the primitives draw, on every surface it is drawn on.
const TEXT_PAIRS: [string, string][] = [
  ["foreground", "background"],
  ["foreground", "card"],
  ["foreground", "muted"], // ghost Action label on its hover
  ["muted-foreground", "background"],
  ["muted-foreground", "card"],
  ["muted-foreground", "popover"],
  ["muted-foreground", "muted"], // disabled Actions, segmented track
  ["card-foreground", "card"],
  ["popover-foreground", "popover"],
  ["popover-foreground", "muted"], // menu item hover
  ["popover-foreground", "accent"], // menu item keyboard focus
  ["primary-foreground", "primary"],
  ["primary-foreground", "primary-hover"],
  ["secondary-foreground", "secondary"],
  ["secondary-foreground", "background"], // secondary Action
  ["secondary-foreground", "card"],
  ["accent-foreground", "accent"], // selected Chip
  ["accent-foreground", "background"],
  ["accent-foreground", "card"],
  ["destructive-foreground", "destructive"], // danger (confirm) Action
  ["destructive-foreground", "destructive-hover"],
  ["destructive", "background"], // Label color="danger", destructive Action, menu items
  ["destructive", "card"],
  ["destructive", "popover"],
  ["destructive", "tone-danger"], // destructive Action hover
  ["sidebar-foreground", "sidebar"],
  ["sidebar-foreground", "sidebar-hover"],
  ["sidebar-accent-foreground", "sidebar-accent"], // selected NavItem
  ["muted-foreground", "sidebar"],
  ["muted-foreground", "sidebar-hover"],
  ["muted-foreground", "sidebar-accent"], // NavItem trailing note when selected
  // The sidebar's gradient ends lighter: its text stays readable at the bottom, and on the account card.
  ["sidebar-foreground", "sidebar-gradient-end"],
  ["muted-foreground", "sidebar-gradient-end"],
  ["sidebar-foreground", "sidebar-card"],
  ["muted-foreground", "sidebar-card"],
  // A Space's initials on each tile color.
  ...[1, 2, 3, 4, 5, 6].map((n): [string, string] => ["space-tile-foreground", `space-tile-${n}`]),
  ...["neutral", "agent", "runner", "system", "success", "warning", "danger"].map((t): [string, string] => [`tone-${t}-foreground`, `tone-${t}`]),
];

// Edges and indicators the user must find: 3:1 against what is next to them (WCAG 1.4.11).
const EDGE_PAIRS: [string, string][] = [
  ...["background", "card", "popover", "sidebar"].map((s): [string, string] => ["ring", s]),
  ...["background", "card", "popover"].map((s): [string, string] => ["control-edge", s]), // fields, pickers, a switch's off track
  ...["background", "card", "popover"].map((s): [string, string] => ["destructive", s]), // destructive outline
  // The selected row's accent bar and icon, on its pill and beside it.
  ["sidebar-indicator", "sidebar-accent"],
  ["sidebar-indicator", "sidebar"],
  ["sidebar-indicator", "sidebar-gradient-end"],
  ["primary", "muted"], // the chosen segment on its track
  ["primary", "background"],
  ["primary", "card"],
];

test("every text pair the primitives draw meets WCAG AA, and every essential edge 3:1, in both schemes", () => {
  const failures: string[] = [];
  for (const [scheme, palette] of [["dark", dark], ["light", light]] as const) {
    for (const [text, surface] of TEXT_PAIRS) {
      const ratio = contrast(palette[text], palette[surface]);
      if (ratio < 4.5) failures.push(`${scheme} ${text} on ${surface}: ${ratio.toFixed(2)}`);
    }
    for (const [edge, surface] of EDGE_PAIRS) {
      const ratio = contrast(palette[edge], palette[surface]);
      if (ratio < 3) failures.push(`${scheme} ${edge} against ${surface}: ${ratio.toFixed(2)}`);
    }
  }
  expect(failures).toEqual([]);
});

test("surfaces are distinct: cards on the page, the sidebar beside it, popovers above cards", () => {
  for (const palette of [dark, light]) {
    for (const [a, b] of [["card", "background"], ["sidebar", "background"]] as const) {
      expect(palette[a] === palette[b]).toBe(false);
    }
  }
  expect([light.background, light.sidebar, light.card]).toEqual(["#f8faff", "#f1f5fc", "#ffffff"]);
  expect([dark.background, dark.sidebar]).toEqual(["#0b0f19", "#081b3b"]);
  expect(dark.popover === dark.card).toBe(false);
});

test("both schemes define the same tokens", () => {
  expect(Object.keys(light).sort()).toEqual(Object.keys(dark).sort());
});

test("the shared scales retain compact controls and provide the complete brand type and radius scales", () => {
  const t = theme.cssVars.theme as Record<string, string>;
  expect(["sm", "md", "lg", "xl"].map(s => t[`radius-${s}`])).toEqual(["6px", "8px", "12px", "16px"]);
  expect([t["control-height"], t["control-height-sm"]]).toEqual(["36px", "28px"]);
  expect(t["text-2xl"]).toBe("24px");
  for (const size of ["xs", "sm", "md", "lg", "xl", "2xl", "3xl", "4xl", "display"]) {
    const px = (v: string) => Number(v.replace("px", ""));
    expect(px(t[`leading-${size}`]) > px(t[`text-${size}`])).toBe(true);
  }
});

test("Showing now appears only while System is chosen", async () => {
  const app = await openFixture({}, "light");
  await expectScheme(app, dark);
  expect(app.shows("id:appearance-shown")).toBe(false);
  await app.press("id:appearance:system");
  await app.waitFor("text:Light, from your system");
  await app.press("id:appearance:dark");
  await app.waitFor("id:appearance-shown", { state: "gone" });
});

test("the preview shows every kind of element, including a sample message", async () => {
  const app = await openFixture({ [APPEARANCE_KEY]: "dark" });
  await app.waitFor("id:appearance-preview");
  for (const label of [
    "Preview of a primary button",
    "Preview of a secondary button",
    "Preview of a quiet button",
    "Preview of a destructive button",
    "Preview of a confirm-delete button",
    "Preview of a disabled button",
    "Preview of a compact button",
    "Preview of a text field",
    "Preview of a switch",
  ]) {
    expect(app.shows(`label:${label}`)).toBe(true);
  }
  expect(app.enabled("label:Preview of a disabled button")).toBe(false);
  expect(app.shows("id:appearance-sample-message >> text:Leo Navarro")).toBe(true);
  expect(app.shows("text:Couldn't connect")).toBe(true);
});

test("Action variants: disabled turns neutral, destructive is an outline in the danger ink, danger is filled", async () => {
  for (const [scheme, palette] of [["dark", dark], ["light", light]] as const) {
    const app = await openFixture({ [APPEARANCE_KEY]: scheme });
    await expectScheme(app, palette);
    expect(bg(app, "id:fx-disabled")).toBe(rgba(palette.muted));
    expect(fg(app, "id:fx-disabled >> text:Fixture off")).toBe(rgba(palette["muted-foreground"]));
    expect(app.enabled("id:fx-disabled")).toBe(false);
    expect(bg(app, "id:fx-destructive")).toBe(rgba(palette.background));
    expect(fg(app, "id:fx-destructive >> text:Fixture remove")).toBe(rgba(palette.destructive));
    // Every disabled variant: the same muted surface and muted text.
    expect(bg(app, "id:fx-destructive-off")).toBe(rgba(palette.muted));
    expect(fg(app, "text:Fixture remove off")).toBe(rgba(palette["muted-foreground"]));
    expect(bg(app, "id:fx-ghost-off")).toBe(rgba(palette.muted));
    expect(fg(app, "text:Fixture ghost off")).toBe(rgba(palette["muted-foreground"]));
    expect(bg(app, "id:fx-danger")).toBe(rgba(palette.destructive));
    expect(fg(app, "text:Fixture delete")).toBe(rgba(palette["destructive-foreground"]));
    expect(bg(app, "id:fx-edge") ?? "transparent").toBe("transparent");
  }
});

test("ActionsMenu: opens from its trigger, items by id, destructive ink, disabled items stay put", async () => {
  const app = await openFixture({ [APPEARANCE_KEY]: "dark" });
  await expectScheme(app, dark);
  expect(app.shows("id:fx-menu-rename")).toBe(false);
  await app.press("id:fx-menu");
  await app.waitFor("id:fx-menu-rename");
  expect(fg(app, "text:Fixture delete item")).toBe(rgba(dark.destructive));
  expect(fg(app, "text:Fixture rename")).toBe(rgba(dark["popover-foreground"]));
  expect(app.enabled("id:fx-menu-off")).toBe(false);
  await app.press("id:fx-menu-delete");
  await app.waitFor("text:Last: delete");
  await app.waitFor("id:fx-menu-rename", { state: "gone" });
  await app.press("id:fx-menu");
  await app.press("id:fx-menu-rename");
  await app.waitFor("text:Last: rename");
});

test("Banner: the inline form shows its title and message in the tone's ink on the tone", async () => {
  const app = await openFixture({ [APPEARANCE_KEY]: "light" });
  await expectScheme(app, light);
  expect(bg(app, "id:fx-banner")).toBe(rgba(light["tone-danger"]));
  expect(fg(app, "text:Fixture failed")).toBe(rgba(light["tone-danger-foreground"]));
  expect(fg(app, "text:Fixture banner message")).toBe(rgba(light["tone-danger-foreground"]));
});

test("Disclosure: the header toggles its content", async () => {
  const app = await openFixture({ [APPEARANCE_KEY]: "dark" });
  await expectScheme(app, dark);
  expect(app.shows("text:Fixture hidden content")).toBe(false);
  await app.press("id:fx-disclosure:toggle");
  await app.waitFor("text:Fixture hidden content");
  await app.press("id:fx-disclosure:toggle");
  await app.waitFor("text:Fixture hidden content", { state: "gone" });
});

test("Picker: a detail shows as a second line, and the trigger shows only the label", async () => {
  const app = await openFixture({ [APPEARANCE_KEY]: "dark" });
  await expectScheme(app, dark);
  await app.press("id:fx-picker");
  await app.waitFor("id:fx-picker:a");
  expect(fg(app, "text:Fixture alpha detail")).toBe(rgba(dark["muted-foreground"]));
  await app.press("id:fx-picker:a");
  await app.waitFor("id:fx-picker:a", { state: "gone" });
  expect(app.shows("text:Fixture alpha detail")).toBe(false);
  expect(app.shows("id:fx-picker >> text:Fixture alpha")).toBe(true);
});

test("menus open and close with and without reduced motion (fade + 0.98 scale, none when reduced)", async () => {
  for (const reducedMotion of [false, true]) {
    const app = await open(FIXTURE, { platform: "linux", width: 1280, height: 1400, storage: { [APPEARANCE_KEY]: "dark" }, reducedMotion });
    await expectScheme(app, dark);
    await app.press("id:fx-menu");
    await app.waitFor("id:fx-menu-rename");
    // Its end look is fully opaque, whatever the entrance.
    await app.waitUntil(() => app.viewStyle("id:fx-menu-rename")?.effectiveOpacity === 1);
    await app.press("id:fx-menu-rename");
    await app.waitFor("id:fx-menu-rename", { state: "gone" });
    await app.waitFor("text:Last: rename");
  }
});

test("a Space's tile color is fixed by its id and drawn from the six tile tones", () => {
  expect(spaceTileFor("acme")).toBe(spaceTileFor("acme"));
  const seen = new Set(Array.from({ length: 60 }, (_, i) => spaceTileFor(`s${i}`)));
  expect([...seen].every((c) => (spaceTileClass as readonly string[]).includes(c))).toBe(true);
  expect(seen.size).toBe(6);
});
