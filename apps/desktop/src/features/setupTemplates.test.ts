import { expect, test } from "nsh/test";
import manifest from "../../nsh.json";
import {
  deviceGuide,
  dockerRunCommand,
  isLoopbackServer,
  releaseTag,
  RELEASES_URL,
  runnerArchive,
  runnerDownloadCommand,
  RUNNER_IMAGE_REPOSITORY,
  RUNNER_SOURCE_NOTE,
  RUNNER_TOKEN_FILE_COMMAND,
  runnerEnvTemplate,
  runnerImage,
  standaloneRunCommand,
  slug,
  TOKEN_PLACEHOLDER,
} from "./setupTemplates";

test("the docker template runs the release's GHCR image, reads the token from runner.env and keeps Spaces in a named volume", () => {
  const remote = dockerRunCommand({ serverUrl: "https://metro.example.com", runnerName: "Build box #2", appVersion: "0.2.0" });
  expect(remote).toContain("--env-file ./runner.env");
  expect(remote).toContain("-v metro-runner-build-box-2-workspace:/workspace");
  expect(remote).toContain("--cap-drop ALL");
  expect(remote).toContain("--security-opt no-new-privileges:true");
  expect(remote.endsWith("ghcr.io/wnzn/metro-runner:v0.2.0")).toBe(true);
  expect(remote.includes("--network host")).toBe(false);
  expect(remote.includes("METRO_RUNNER_TOKEN")).toBe(false);
  // A server on this machine is reached through the host network.
  expect(dockerRunCommand({ serverUrl: "http://127.0.0.1:8787", runnerName: "x", appVersion: "0.2.0" })).toContain("--network host");
});

test("the image is the release workflow's name, tagged with this app's version, never built from source", () => {
  // release.yml: ghcr.io/<owner>/metro-<component>:<tag>, and the tag must equal v<nsh.json version>.
  expect(RUNNER_IMAGE_REPOSITORY).toBe("ghcr.io/wnzn/metro-runner");
  expect(runnerImage("v0.2.0")).toBe("ghcr.io/wnzn/metro-runner:v0.2.0");
  expect(runnerImage((manifest as { version: string }).version)).toBe(`ghcr.io/wnzn/metro-runner:v${(manifest as { version: string }).version}`);
  const all = dockerRunCommand({ serverUrl: "https://m", runnerName: "r", appVersion: "0.2.0" });
  for (const banned of ["docker build", "cargo", "git clone", "--target runner"]) expect(all.includes(banned)).toBe(false);
  // Accurate before and after the release is out: never claims it's published or unpublished.
  expect(RUNNER_SOURCE_NOTE).toBe(
    "Get the matching prebuilt runner from Metro Releases, or pull this version's image when available. Use the files of the release whose tag matches this app; Metro never asks you to build the runner yourself.",
  );
  for (const claim of ["published", "not yet", "isn't"]) expect(RUNNER_SOURCE_NOTE.includes(claim)).toBe(false);
});

test("the standalone runner is this version's release archive, checked, with the token read from a mode-600 file", () => {
  expect(RELEASES_URL).toBe("https://github.com/wnzn/metro/releases");
  expect(releaseTag("0.2.0")).toBe("v0.2.0");
  expect(releaseTag("v0.2.0")).toBe("v0.2.0");
  expect(runnerArchive("0.2.0")).toBe("Metro-runner-0.2.0-linux-x64.tar.gz");
  expect(runnerDownloadCommand("0.2.0").split("\n")).toEqual([
    "curl -fLO https://github.com/wnzn/metro/releases/download/v0.2.0/Metro-runner-0.2.0-linux-x64.tar.gz",
    "curl -fLO https://github.com/wnzn/metro/releases/download/v0.2.0/Metro-runner-0.2.0-linux-x64.tar.gz.sha256",
    "sha256sum -c Metro-runner-0.2.0-linux-x64.tar.gz.sha256",
    "tar -xzf Metro-runner-0.2.0-linux-x64.tar.gz",
  ]);
  expect(RUNNER_TOKEN_FILE_COMMAND).toBe("install -m 600 /dev/null runner.token");
  const run = standaloneRunCommand({ serverUrl: "https://metro.example.com/", runnerName: "Build box #2", appVersion: "0.2.0" });
  expect(run).toBe(
    'mkdir -p "$HOME/metro-runner-build-box-2"\nMETRO_SERVER_URL=https://metro.example.com \\\n  METRO_WORKSPACE="$HOME/metro-runner-build-box-2" \\\n  METRO_RUNNER_TOKEN_FILE="$PWD/runner.token" \\\n  ./Metro-runner-0.2.0-linux-x64/run-runner',
  );
  expect(run.includes("METRO_RUNNER_TOKEN=")).toBe(false);
  // The archive name and tag follow the app's own version.
  const v = (manifest as { version: string }).version;
  expect(runnerDownloadCommand(v)).toContain(`/download/v${v}/Metro-runner-${v}-linux-x64.tar.gz`);
  for (const banned of ["cargo", "docker build", "git clone", "make "]) {
    expect(runnerDownloadCommand(v).includes(banned)).toBe(false);
    expect(run.includes(banned)).toBe(false);
  }
});

test("runner.env holds the URL and a placeholder, never a token", () => {
  expect(runnerEnvTemplate("http://127.0.0.1:8787/")).toBe(`METRO_SERVER_URL=http://127.0.0.1:8787\nMETRO_RUNNER_TOKEN=${TOKEN_PLACEHOLDER}\n`);
});

test("names become safe Docker and profile names", () => {
  expect(slug("  Ünïcode Box!! ")).toBe("unicode-box");
  expect(slug("***")).toBe("runner");
  expect(slug("***", "metro")).toBe("metro");
  expect(slug("a".repeat(60)).length).toBe(40);
});

test("loopback detection", () => {
  for (const url of ["http://127.0.0.1:8787", "http://localhost", "http://[::1]:8787"]) expect(isLoopbackServer(url)).toBe(true);
  for (const url of ["https://metro.example.com", "http://192.168.1.2:8787", "http://127.0.0.1.example.com"]) expect(isLoopbackServer(url)).toBe(false);
});

test("manual connector steps are for headless computers and development builds, never install from source and never carry a token", () => {
  const guide = deviceGuide({ serverUrl: "https://metro.example.com/", connectionName: "Home GPU", providerId: "device-openai-compatible" });
  const commands = guide.steps.map((s) => s.command).filter(Boolean);
  expect(commands).toEqual([
    "metro-connector setup --profile home-gpu --server-url https://metro.example.com --kind device-openai-compatible",
    "metro-connector run --profile home-gpu",
    "metro-connector status --profile home-gpu",
  ]);
  expect(JSON.stringify(guide)).not.toContain("cargo");
  expect(guide.note).toContain("For headless computers and development builds only");
  expect(guide.note).toContain("assume an operator already installed metro-connector");
  for (const banned of ["cargo", "install --", "git clone", "build it"]) expect(JSON.stringify(guide).includes(banned)).toBe(false);
  const oauth = deviceGuide({ serverUrl: "https://m", connectionName: "Work", providerId: "device-openai-oauth" });
  expect(oauth.steps.some((s) => s.command === "metro-connector login --profile work")).toBe(true);
  expect(JSON.stringify(oauth)).toContain("Never paste an OAuth code or token into Metro");
});
