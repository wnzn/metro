import { expect, open, test } from "nsh/test";
import inter from "./fonts/InterVariable.ttf";
import theme from "../../theme/theme.json";
import { launched } from "../../test/launched";

test("the bundled face matches the first family of the desktop theme", () => {
  expect(inter).toBe("Inter");
  expect(theme.cssVars.theme["font-sans"].split(",")[0]).toBe(inter);
});

test("the connect form opens on Windows with the bundled face", async () => {
  const app = await open("../../App.tsx", { modules: launched(), platform: "windows", width: 1100, height: 720 });
  await app.waitFor("id:connect");
  expect(app.shows("id:server-url")).toBe(true);
  expect(app.shows("id:metro-mark")).toBe(true);
});
