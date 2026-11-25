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
  ...["self", "person", "agent"].map((t): [string, string] => [`mention-${t}-foreground`, `mention-${t}`]),
];

// Edges and indicators the user must find: 3:1 against what is next to them (WCAG 1.4.11).
const EDGE_PAIRS: [string, string][] = [
  ...["background", "card", "popover", "sidebar"].map((s): [string, string] => ["ring", s]),
  ...["background", "card", "popover"].map((s): [string, string] => ["control-edge", s]), // fields, pickers, a switch's off track
  ...["background", "card", "popover"].map((s): [string, string] => ["destructive", s]), // destructive outline
  // The selected row's accent bar and icon, on its pill and beside it.
  ["sidebar-indicator", "sidebar-accent"],
  ["sidebar-indicator", "sidebar"],
  ["sidebar-indicator", "sidebar-hover"], // update dot on a hovered tab or rail button
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

const openSettingsFixture = (width = 1280, height = 1000) =>
  open("./settings.fixture.tsx", { platform: "linux", width, height, reducedMotion: true });

test("Settings primitives: grouped navigation keeps update indicators named and flattens compact tabs", async () => {
  const app = await openSettingsFixture();
  await app.waitFor("id:fx-section-you");
  expect(app.shows("id:fx-section-metro")).toBe(true);
  expect(app.shows("id:fx-tab-about:dot")).toBe(true);
  expect(app.shows("label:About & updates, Update available")).toBe(true);
  expect(app.shows("id:fx-rail-dot")).toBe(true);
  expect(app.shows("label:Settings, Update available")).toBe(true);
  await app.press("id:fx-tab-about");
  expect(bg(app, "id:fx-tab-about")).toBe(rgba(dark["sidebar-accent"]));
  await app.waitFor("id:fx-header >> text:About");

  await app.setViewport(720, 900);
  expect(app.shows("id:fx-section-you")).toBe(false);
  expect(app.shows("id:fx-section-metro")).toBe(false);
  expect(app.shows("id:fx-tab-about:dot")).toBe(true);
  await app.press("id:fx-tab-profile");
  await app.waitFor("id:fx-header >> text:Profile page");
  await app.press("id:fx-update");
  expect(app.shows("id:fx-tab-about:dot")).toBe(false);
  expect(app.shows("label:About & updates")).toBe(true);
});

test("Settings primitives: the page header shows the fixed scope wording beside its usable action", async () => {
  const app = await openSettingsFixture(720, 900);
  for (const scope of [
    "Your account on metro.example", "This computer only", "This Space · only the owner changes it",
    "This Space · only you see these", "Organization · org admins manage it",
  ]) {
    await app.waitFor(`id:fx-header:scope >> text:${scope}`);
    expect(fg(app, `text:${scope}`)).toBe(rgba(dark["muted-foreground"]));
    await app.press("id:fx-next-scope");
  }
});

test("Settings primitives: exact inline confirmation is scoped, cancellable and not triggered by Return in the field", async () => {
  const app = await openSettingsFixture();
  await app.type("id:fx-before-checkbox", "x");
  await app.keyboard.press("tab");
  await app.keyboard.press("tab");
  await app.keyboard.press("enter");
  expect(bg(app, "id:fx-confirm:warning")).toBe(rgba(dark["tone-warning"]));
  expect(app.enabled("id:fx-confirm:confirm")).toBe(false);
  await app.type("id:fx-confirm:input", "exact Name");
  expect(app.enabled("id:fx-confirm:confirm")).toBe(false);
  await app.clear("id:fx-confirm:input");
  await app.type("id:fx-confirm:input", "Exact Name ");
  expect(app.enabled("id:fx-confirm:confirm")).toBe(false);
  await app.clear("id:fx-confirm:input");
  await app.type("id:fx-confirm:input", "Exact Name");
  expect(app.enabled("id:fx-confirm:confirm")).toBe(true);
  expect(bg(app, "id:fx-confirm:confirm")).toBe(rgba(dark.destructive));
  await app.keyboard.press("enter");
  expect(app.shows("text:Confirmations: 0")).toBe(true);
  await app.press("id:fx-retarget");
  expect(app.shows("id:fx-confirm:panel")).toBe(false);
  await app.press("id:fx-retarget");
  expect(app.shows("id:fx-confirm:panel")).toBe(false);
  await app.press("id:fx-confirm");
  expect(app.value("id:fx-confirm:input")).toBe("");
  await app.type("id:fx-confirm:input", "Exact Name");
  await app.keyboard.press("escape");
  expect(app.shows("id:fx-modal")).toBe(true);
  expect(app.shows("id:fx-confirm:panel")).toBe(false);
  await app.keyboard.press("enter"); // Escape restored focus to the trigger.
  expect(app.value("id:fx-confirm:input")).toBe("");
  await app.keyboard.press("tab"); // From the field to Cancel.
  await app.keyboard.press("escape");
  expect(app.shows("id:fx-modal")).toBe(true);
  expect(app.shows("id:fx-confirm:panel")).toBe(false);
  await app.press("id:fx-confirm");
  await app.press("id:fx-confirm:cancel");
  expect(app.shows("id:fx-confirm:panel")).toBe(false);
  await app.press("id:fx-confirm");
  await app.type("id:fx-confirm:input", "Exact Name");
  await app.press("id:fx-confirm:confirm");
  await app.waitFor("text:Confirmations: 1");
  expect(app.shows("id:fx-confirm:panel")).toBe(false);
});

test("Settings primitives: externally opened confirmation focuses Cancel, restores the opener, and handles Escape in the modal", async () => {
  const app = await openSettingsFixture();
  expect(app.shows("id:fx-external:panel")).toBe(false);
  await app.press("id:fx-external-open");
  expect(app.shows("id:fx-external:warning")).toBe(true);
  expect(app.shows("id:fx-external:input")).toBe(false);
  await app.keyboard.press("enter"); // The first focused action is Cancel.
  expect(app.shows("id:fx-external:panel")).toBe(false);
  expect(app.shows("id:fx-modal")).toBe(true);
  await app.keyboard.press("enter"); // Focus returned to the button that opened it.
  expect(app.shows("id:fx-external:panel")).toBe(true);
  await app.keyboard.press("tab"); // Cancel → Confirm
  await app.keyboard.press("escape");
  expect(app.shows("id:fx-external:panel")).toBe(false);
  expect(app.shows("id:fx-modal")).toBe(true);
  await app.keyboard.press("enter");
  expect(bg(app, "id:fx-external:confirm")).toBe(rgba(dark.primary));
  await app.keyboard.press("tab");
  await app.keyboard.press("enter");
  await app.waitFor("id:fx-external:busy >> text:Downloading…");
  expect(app.shows("label:Downloading…")).toBe(true);
  expect(app.shows("id:fx-external:confirm")).toBe(false);
  expect(app.enabled("id:fx-external:cancel")).toBe(false);
  await app.press("id:fx-reset-open"); // The caller closes after its action completes.
  expect(app.shows("id:fx-external:panel")).toBe(false);
  await app.keyboard.press("enter");
  expect(app.shows("id:fx-external:panel")).toBe(true);
});

test("Settings primitives: a no-text confirmation opened from a menu focuses Cancel and returns to the menu button", async () => {
  const app = await openSettingsFixture();
  await app.press("id:fx-external-menu");
  await app.press("id:fx-external-menu-item");
  await app.keyboard.press("enter");
  expect(app.shows("id:fx-external:panel")).toBe(false);
  expect(app.shows("id:fx-modal")).toBe(true);
  await app.keyboard.press("enter");
  await app.waitFor("id:fx-external-menu-item");
  await app.press("id:fx-external-menu-item");
  await app.keyboard.press("escape");
  expect(app.shows("id:fx-external:panel")).toBe(false);
  expect(app.shows("id:fx-modal")).toBe(true);
});

test("Settings primitives: Escape during keyboard-started work keeps the modal open without restarting or dismissing the action", async () => {
  const app = await open("./settings.fixture.tsx", { platform: "linux", width: 720, height: 900, reducedMotion: true, clock: {} });
  await app.press("id:fx-auto-settle");
  await app.press("id:fx-external-open");
  await app.keyboard.press("tab"); // Cancel → Confirm
  await app.keyboard.press("enter");
  expect(app.shows("id:fx-external:busy >> text:Downloading…")).toBe(true);
  await app.keyboard.press("escape");
  expect(app.shows("id:fx-modal")).toBe(true);
  expect(app.shows("id:fx-external:panel")).toBe(true);
  expect(app.enabled("id:fx-external:cancel")).toBe(false);
  expect(app.shows("id:fx-external:confirm")).toBe(false);
  await app.keyboard.press("enter");
  await app.keyboard.press("space");
  expect(app.shows("text:Actions started: 1")).toBe(true);
  await app.clock!.runFor(1000); // Settle without moving focus by clicking another control.
  expect(app.shows("id:fx-external:panel")).toBe(true);
  expect(app.shows("id:fx-external:busy")).toBe(false);
  await app.keyboard.press("enter"); // Focus settles on Cancel, not on the destructive action or somewhere else.
  expect(app.shows("id:fx-external:panel")).toBe(false);
  expect(app.shows("text:Actions started: 1")).toBe(true);
  await app.keyboard.press("enter"); // Cancel restores the original opener.
  expect(app.shows("id:fx-external:panel")).toBe(true);
  expect(app.shows("id:fx-modal")).toBe(true);
});

test("Settings primitives: a menu-opener handoff preserves focus and Escape in a typed confirmation", async () => {
  const app = await openSettingsFixture(720, 900);
  await app.press("id:fx-external-typed");
  await app.press("id:fx-external-menu");
  await app.press("id:fx-external-menu-item");
  await app.keyboard.press("x"); // The exact-text field, not the menu trigger, has focus.
  expect(app.value("id:fx-external:input")).toBe("x");
  expect(app.enabled("id:fx-external:confirm")).toBe(false);
  await app.keyboard.press("escape");
  expect(app.shows("id:fx-modal")).toBe(true);
  expect(app.shows("id:fx-external:panel")).toBe(false);
  await app.keyboard.press("enter");
  await app.waitFor("id:fx-external-menu-item"); // Returned to the menu button, not the nearby ordinary opener.
  await app.press("id:fx-external-menu-item");
  expect(app.value("id:fx-external:input")).toBe("");
  await app.keyboard.press("tab"); // Field → Cancel
  await app.keyboard.press("enter");
  expect(app.shows("id:fx-external:panel")).toBe(false);
  expect(app.shows("id:fx-modal")).toBe(true);
});

test("Settings primitives: retargeting a controlled confirmation invalidates a stuck-open request", async () => {
  const app = await openSettingsFixture();
  await app.press("id:fx-danger-external");
  await app.press("id:fx-external-open");
  expect(app.enabled("id:fx-external:confirm")).toBe(true);
  expect(bg(app, "id:fx-external:confirm")).toBe(rgba(dark.destructive));
  await app.press("id:fx-retarget");
  expect(app.shows("text:Parent open: true")).toBe(true);
  await expect(app.press("id:fx-external:confirm")).rejects.toMatchObject({ code: "ERR_NSH_TEST_NOT_FOUND" });
  await app.press("id:fx-retarget");
  await app.press("id:fx-external-open"); // Still true: even returning to the old target doesn't re-arm it.
  expect(app.shows("id:fx-external:panel")).toBe(false);
  expect(app.shows("text:Parent open: true")).toBe(true);
  await app.press("id:fx-reset-open");
  await app.press("id:fx-retarget");
  await app.press("id:fx-external-open");
  await app.press("id:fx-external:confirm");
  await app.waitFor("label:Downloading…");
});

test("Settings primitives: retargeting an uncontrolled confirmation without text closes it until reopened", async () => {
  const app = await openSettingsFixture();
  await app.press("id:fx-local");
  expect(app.enabled("id:fx-local:confirm")).toBe(true);
  await app.press("id:fx-retarget");
  await expect(app.press("id:fx-local:confirm")).rejects.toMatchObject({ code: "ERR_NSH_TEST_NOT_FOUND" });
  await app.press("id:fx-retarget");
  expect(app.shows("id:fx-local:panel")).toBe(false);
  expect(app.shows("text:Removed runner: -")).toBe(true);
  await app.press("id:fx-retarget");
  await app.press("id:fx-local");
  await app.press("id:fx-local:confirm");
  await app.waitFor("text:Removed runner: B");
});

test("Settings primitives: the whole checkbox toggles with keyboard or pointer, until disabled", async () => {
  const app = await openSettingsFixture();
  await app.waitFor("text:Runner selected: true");
  expect(app.shows("label:Use the runner. Only uses this computer.")).toBe(true);
  await app.press("id:fx-checkbox >> text:Only uses this computer.");
  await app.waitFor("text:Runner selected: false");
  await app.type("id:fx-before-checkbox", "x");
  await app.keyboard.press("tab");
  await app.keyboard.press("space");
  await app.waitFor("text:Runner selected: true");
  await app.press("id:fx-disabled");
  expect(app.enabled("id:fx-checkbox")).toBe(false);
  await expect(app.press("id:fx-checkbox")).rejects.toMatchObject({ code: "ERR_NSH_TEST_DISABLED" });
  expect(app.shows("text:Runner selected: true")).toBe(true);
});

test("Settings primitives: the same nonblank tags are visible and spoken, even with a custom button name", async () => {
  const app = await openSettingsFixture(720, 900);
  await app.waitFor("label:Test provider, default, Update 0.5.0, Online");
  expect(app.shows("id:fx-row:default")).toBe(true);
  expect(bg(app, "id:fx-update-tag")).toBe(rgba(dark.accent));
  expect(app.shows("id:fx-update-tag >> text:Update 0.5.0")).toBe(true);
  expect(app.shows("id:fx-blank-tag")).toBe(false);
  expect(app.shows("id:fx-problem")).toBe(true);
  expect(app.shows("id:fx-passive:open")).toBe(false);
  expect(app.shows("id:fx-passive >> label:Passive provider name")).toBe(true);
  expect(app.shows("id:fx-passive-tag >> text:Organization")).toBe(true);
  expect(app.shows("id:fx-passive >> text:Off")).toBe(true);
  await app.press("id:fx-row-action");
  await app.waitFor("text:Last row action: trailing");
  await app.press("id:fx-row:open");
  await app.waitFor("text:Last row action: open");
  await app.press("id:fx-custom-name");
  await app.waitFor("label:Open provider details, Update 0.5.0");
});

test("Settings primitives: reorder buttons report one step and disable impossible moves", async () => {
  const app = await openSettingsFixture();
  expect(app.shows("label:Move Test provider up")).toBe(true);
  expect(app.shows("label:Move Test provider down")).toBe(true);
  expect(app.enabled("id:fx-reorder:up")).toBe(false);
  await app.press("id:fx-reorder:down");
  await app.waitFor("text:Provider position: 2");
  expect(app.enabled("id:fx-reorder:down")).toBe(false);
  await app.press("id:fx-reorder:up");
  await app.waitFor("text:Provider position: 1");
  await app.press("id:fx-disabled");
  expect(app.enabled("id:fx-reorder:up")).toBe(false);
  expect(app.enabled("id:fx-reorder:down")).toBe(false);
});

test("Settings primitives: progress shows bounded percentages and never guesses an unknown total", async () => {
  const app = await openSettingsFixture();
  await app.waitFor("label:Downloading 0.5.0…");
  for (const [index, text] of ["Downloading 0.5.0… 42%", "Downloading 0.5.0… 100%", "Downloading 0.5.0…", "Downloading 0.5.0… 0%"].entries()) {
    await app.waitFor(`id:fx-progress >> text:${text}`);
    if (index < 3) await app.press("id:fx-advance");
  }
});
