// Fetches each URL in PROBE_URLS with NSH's own fetch, in this process's
// environment (proxy variables and NO_PROXY), and shows what answered.
import { mount, signal, Text, View } from "nsh";
import proc from "process";

const out = signal("starting");

async function run() {
  const urls = (proc.env?.PROBE_URLS ?? "").split(" ").filter(Boolean);
  const res: string[] = [];
  for (const u of urls) {
    try {
      const r = await fetch(u);
      res.push(`${u} => ${(await r.text()).trim()}`);
    } catch (e) {
      res.push(`${u} => ERROR ${String(e)}`);
    }
  }
  out.set("DONE\n" + res.join("\n"));
}
void run();

function Probe() {
  return <View><Text testID="out">{out()}</Text></View>;
}
mount(Probe);
