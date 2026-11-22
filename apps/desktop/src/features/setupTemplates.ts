// Copyable, non-secret setup text for a registered runner (the prebuilt
// standalone runner, or its Docker image) and for a device model connection
// on a headless computer (metro-connector). Pure functions of public facts
// only: none of them takes a token, so no token can end up in a command, a
// template or a screenshot. The user pastes the token into a private file
// (runner.env) or the connector's own prompt, never into a shell line.

/** The placeholder the user replaces with the token in runner.env. */
export const TOKEN_PLACEHOLDER = "<paste the runner token here>";

const LOOPBACK = /^(localhost|127(?:\.\d{1,3}){3}|\[::1\])$/i;

function hostOf(serverUrl: string): string {
  const m = /^[a-z]+:\/\/(\[[^\]]+\]|[^/:?#]+)/i.exec(serverUrl.trim());
  return m ? m[1] : "";
}

/** The server runs on this machine: a container reaches it only through the host network. */
export function isLoopbackServer(serverUrl: string): boolean {
  return LOOPBACK.test(hostOf(serverUrl));
}

/** A Docker-safe name part from a runner's display name ("Build box #2" → "build-box-2"). */
export function slug(name: string, fallback = "runner"): string {
  const s = name
    .toLowerCase()
    .normalize("NFKD")
    .replace(/[\u0300-\u036f]/g, "")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 40)
    .replace(/-+$/g, "");
  return s || fallback;
}

/** The lines of runner.env: the server URL, and where the token goes. */
export function runnerEnvTemplate(serverUrl: string): string {
  return [`METRO_SERVER_URL=${serverUrl.replace(/\/+$/, "")}`, `METRO_RUNNER_TOKEN=${TOKEN_PLACEHOLDER}`, ""].join("\n");
}

/** Creates an empty runner.env only its owner can read (chmod 600), ready to be filled in an editor. */
export const PRIVATE_ENV_FILE_COMMAND = "install -m 600 /dev/null runner.env";

/**
 * The runner image of this Metro release on GHCR, as the tagged release
 * workflow (.github/workflows/release.yml) names it: `ghcr.io/<owner>/metro-runner`,
 * tagged `v<version>` where the version is the desktop app's own (the
 * workflow refuses a tag that doesn't match apps/desktop/nsh.json).
 */
export const RUNNER_IMAGE_REPOSITORY = "ghcr.io/wnzn/metro-runner";
export const runnerImage = (appVersion: string) => `${RUNNER_IMAGE_REPOSITORY}:v${appVersion.replace(/^v/, "")}`;

// ---------------------------------------------------------------- prebuilt runner from Metro Releases

/** Metro's releases index: it lists every published release (and is reachable before any exists). */
export const RELEASES_URL = "https://github.com/wnzn/metro/releases";
const plain = (v: string) => v.replace(/^v/, "");
/** The release tag that matches this app's version (release.yml refuses any other). */
export const releaseTag = (appVersion: string) => `v${plain(appVersion)}`;
/** The standalone Linux runner archive of that release, as release.yml names it (with a `.sha256` beside it). */
export const runnerArchive = (appVersion: string) => `Metro-runner-${plain(appVersion)}-linux-x64.tar.gz`;
const runnerDir = (appVersion: string) => `Metro-runner-${plain(appVersion)}-linux-x64`;

/** Where the prebuilt runner comes from, in words that hold before and after the release is out. */
export const RUNNER_SOURCE_NOTE =
  "Get the matching prebuilt runner from Metro Releases, or pull this version's image when available. Use the files of the release whose tag matches this app; Metro never asks you to build the runner yourself.";

/**
 * Downloads this version's runner archive and its checksum from the matching
 * release tag, checks it, and unpacks it. `curl -f` fails plainly if that
 * release's files aren't available.
 */
export function runnerDownloadCommand(appVersion: string): string {
  const archive = runnerArchive(appVersion);
  const base = `${RELEASES_URL}/download/${releaseTag(appVersion)}`;
  return [
    `curl -fLO ${base}/${archive}`,
    `curl -fLO ${base}/${archive}.sha256`,
    `sha256sum -c ${archive}.sha256`,
    `tar -xzf ${archive}`,
  ].join("\n");
}

/** Creates the empty token file only its owner can read (the archive's run-runner refuses any other mode). */
export const RUNNER_TOKEN_FILE_COMMAND = "install -m 600 /dev/null runner.token";

/**
 * Starts the prebuilt runner with the archive's `run-runner`, which reads the
 * token from the mode-600 file, never from the command line. The workspace is
 * the base folder; each Space it serves gets its own `spaces/<space id>/`.
 */
export function standaloneRunCommand(opts: { serverUrl: string; runnerName: string; appVersion: string }): string {
  const workspace = `$HOME/metro-runner-${slug(opts.runnerName)}`;
  return [
    `mkdir -p "${workspace}"`,
    [
      `METRO_SERVER_URL=${opts.serverUrl.replace(/\/+$/, "")}`,
      `METRO_WORKSPACE="${workspace}"`,
      `METRO_RUNNER_TOKEN_FILE="$PWD/runner.token"`,
      `./${runnerDir(opts.appVersion)}/run-runner`,
    ].join(" \\\n  "),
  ].join("\n");
}

/**
 * Starts the runner from the image, with the token read from runner.env by
 * Docker (`--env-file`), never from the command line. Each Space it serves
 * gets its own `spaces/<space id>/` folder inside the named volume.
 */
export function dockerRunCommand(opts: { serverUrl: string; runnerName: string; appVersion: string }): string {
  const name = `metro-runner-${slug(opts.runnerName)}`;
  const lines = [
    "docker run -d",
    `--name ${name}`,
    "--restart unless-stopped",
    "--init",
    "--cap-drop ALL",
    "--security-opt no-new-privileges:true",
    ...(isLoopbackServer(opts.serverUrl) ? ["--network host"] : []),
    "--env-file ./runner.env",
    `-v ${name}-workspace:/workspace`,
    runnerImage(opts.appVersion),
  ];
  return lines.join(" \\\n  ");
}

/** What the runner may do as configured by the template, said plainly. */
export const RUNNER_TOOLS_NOTE =
  "As started here, a registered runner offers list and read only. Check and MCP commands need the Docker sandbox (METRO_SANDBOX=docker); see docs/runners.md.";

// ---------------------------------------------------------------- device connections

export type DeviceGuide = {
  /** What happens on the user's computer, in order. */
  steps: { text: string; command?: string }[];
  /** Plain words about consent and where credentials stay. */
  note: string;
};

/**
 * Operator steps for a device connection served by metro-connector on a
 * headless computer. Shown only by an app started without Metro's launcher
 * (a development or headless/operator setup); the packaged app sets
 * connections up in-app and never shows them. They assume an operator has
 * already put metro-connector on that computer: Metro installs nothing and
 * never asks anyone to build it (its interactive setup wizard). The wizard asks for the device token with hidden input, so the
 * commands never hold it; vendor sign-in and keys happen on the user's
 * computer, in the connector.
 */
export function deviceGuide(opts: { serverUrl: string; connectionName: string; providerId: string }): DeviceGuide {
  const server = opts.serverUrl.replace(/\/+$/, "");
  const profile = slug(opts.connectionName, "metro");
  const setup = `metro-connector setup --profile ${profile} --server-url ${server} --kind ${opts.providerId}`;
  const signIn: Record<string, { text: string; command?: string }> = {
    "device-openai-oauth": {
      text: "When setup offers it, sign in with ChatGPT and approve it in your browser on this computer (eligible local integrations; preview). Never paste an OAuth code or token into Metro. To sign in again later:",
      command: `metro-connector login --profile ${profile}`,
    },
    "device-claude-code": {
      text: "Setup uses your installed Claude Code CLI with your Anthropic API key, on this computer. That is API billing; Claude Pro/Max subscriptions are not used. To sign in again later:",
      command: `metro-connector login --profile ${profile}`,
    },
    "device-openai-compatible": {
      text: "Setup asks for your local or private endpoint's URL and, if it has one, its key (hidden input). They stay on this computer.",
    },
  };
  return {
    steps: [
      { text: "Run setup. It asks for the device token with hidden input: paste it there, never into the command. It also asks which models it may serve.", command: setup },
      signIn[opts.providerId] ?? { text: "Sign in to the provider where setup asks, on this computer." },
      { text: "Keep it running while you use this connection (Ctrl-C stops it):", command: `metro-connector run --profile ${profile}` },
      { text: "Back here, press Test connection. Models are listed once the connector is online. To check it locally:", command: `metro-connector status --profile ${profile}` },
    ],
    note: "Your vendor sign-in and keys stay with metro-connector on your computer, in files only you can read. Metro never receives them; it only relays requests to your connector. For headless computers and development builds only: this copy of Metro runs without its launcher, so it has no device helper. They assume an operator already installed metro-connector there. In the packaged Metro app, all of this happens in-app.",
  };
}
