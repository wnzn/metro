import { Clipboard, openUrl, signal, Storage } from "nsh";
import type { JSX } from "nsh";
import {
  dockerComposeFile, dockerRunCommand, PRIVATE_ENV_FILE_COMMAND, releaseTag, RELEASES_URL,
  runnerArchive, runnerDownloadCommand, runnerEnvTemplate, runnerImage, RUNNER_SOURCE_NOTE,
  RUNNER_TOKEN_FILE_COMMAND, RUNNER_TOOLS_NOTE, standaloneRunCommand,
} from "../features/setupTemplates";
import { Action, Column, Label, Row, SegmentedControl, SettingRow, SettingsGroup, Surface } from "./primitives";

type Method = "linux" | "docker-run" | "docker-compose";
const options = [
  { value: "linux", label: "Linux" },
  { value: "docker-run", label: "Docker run" },
  { value: "docker-compose", label: "Docker Compose" },
  { value: "cloud", label: "Cloud · Coming soon", disabled: true },
] as const;

const key = "metro.runner-install.method";
const method = signal<Method>("linux");
let edited = false;
let writes = Promise.resolve();
void Storage.getItem(key).then((saved) => {
  if (!edited && (saved === "linux" || saved === "docker-run" || saved === "docker-compose")) method.set(saved);
}).catch(() => {});

function select(next: Method | "cloud") {
  if (next === "cloud") return;
  edited = true;
  method.set(next);
  writes = writes.then(() => Storage.setItem(key, next)).catch(() => {});
}

/** Copyable, non-secret text (a command or a file's lines). */
export function CopyBlock(props: { text: string; testID: string; label: string }) {
  const copied = signal(false);
  return (
    <Column gap={1}>
      <Surface variant="card" padX={3} padY={2} testID={props.testID} accessibilityLabel={props.label}>
        <Column selectable>
          <Label variant="mono">{props.text}</Label>
        </Column>
      </Surface>
      <Row>
        <Action
          testID={`${props.testID}-copy`}
          label={copied() ? "Copied" : "Copy"}
          accessibilityLabel={`Copy ${props.label}`}
          variant="ghost"
          onPress={() => {
            Clipboard.writeText(props.text)
              .then(() => copied.set(true))
              .catch(() => {});
          }}
        />
      </Row>
    </Column>
  );
}

function Step(props: { n: number; text: string; children?: JSX.Element }) {
  return <SettingRow label={`${props.n}. ${props.text}`} stacked>{props.children}</SettingRow>;
}

type RunnerInstallProps = { serverUrl: string; runnerName: string; appVersion: string; dedicated?: boolean };

/** Prebuilt runner setup. No token is passed in or included in the copyable steps. */
export function RunnerInstall(props: RunnerInstallProps) {
  const workspace = () => props.dedicated
    ? "The workspace is this Space's folder."
    : "Each attached Space has its own folder under spaces/<space id>/ in the workspace.";
  const afterStart = () => props.dedicated ? "Return to this Space's Runner settings to test it."
    : "Space owners can attach it from Space settings › Runner.";
  return (
    <Column gap={4}>
      <SegmentedControl testID="runner-install-method" accessibilityLabel="Runner installation method"
        options={options} value={method()} onChange={select} wrap />
      <If when={method() === "linux"} else={<DockerSteps {...props} workspace={workspace()} afterStart={afterStart()} />}>
        <SettingsGroup
          title="Run the prebuilt runner"
          description={`Linux x64, kernel 5.6 or newer. ${RUNNER_SOURCE_NOTE}`}
          testID="setup-standalone"
        >
          <SettingRow label={`Metro Releases · ${releaseTag(props.appVersion)}`} description={`${runnerArchive(props.appVersion)} and its .sha256`}>
            <Action testID="setup-releases" label="Open Metro Releases" variant="ghost" onPress={() => void openUrl(RELEASES_URL).catch(() => {})} />
          </SettingRow>
          <Step n={1} text="In a new setup folder, download, verify and unpack the runner:">
            <CopyBlock testID="setup-download" label="the download commands" text={runnerDownloadCommand(props.appVersion)} />
          </Step>
          <Step n={2} text="In the same folder, create a private runner.token file. Open it in an editor and paste the token as its only line:">
            <CopyBlock testID="setup-tokenfile" label="the command that creates runner.token" text={RUNNER_TOKEN_FILE_COMMAND} />
          </Step>
          <Step n={3} text={`Start the runner from the same folder. ${workspace()}`}>
            <CopyBlock testID="setup-standalone-run" label="the command that starts the runner" text={standaloneRunCommand(props)} />
          </Step>
          <Step n={4} text={afterStart()} />
          <If when={!props.dedicated}><SettingRow label="What it can do" description={RUNNER_TOOLS_NOTE} /></If>
        </SettingsGroup>
      </If>
    </Column>
  );
}

function DockerSteps(props: RunnerInstallProps & { workspace: string; afterStart: string }) {
  const compose = () => method() === "docker-compose";
  return (
    <SettingsGroup
      title={compose() ? "Run with Docker Compose" : "Run with Docker"}
      description={`Image ${runnerImage(props.appVersion)}, when available. Docker admins can read the container's environment.`}
      testID="setup-docker-steps"
    >
      <Step n={1} text="In a new setup folder, create a private runner.env file:">
        <CopyBlock testID="setup-envfile" label="the command that creates runner.env" text={PRIVATE_ENV_FILE_COMMAND} />
      </Step>
      <Step n={2} text="Open runner.env in an editor, add these lines and replace the placeholder with the token:">
        <CopyBlock testID="setup-env" label="the runner.env lines" text={runnerEnvTemplate(props.serverUrl)} />
      </Step>
      <If when={compose()} else={
        <>
          <Step n={3} text={`Start the runner in the same folder, using a named volume for its workspace. ${props.workspace}`}>
            <CopyBlock testID="setup-docker" label="the docker run command" text={dockerRunCommand(props)} />
          </Step>
          <Step n={4} text={props.afterStart} />
        </>
      }>
        <Step n={3} text={`Save this as compose.yaml beside runner.env. It uses a named volume for its workspace. ${props.workspace}`}>
          <CopyBlock testID="setup-compose" label="the Compose file" text={dockerComposeFile(props)} />
        </Step>
        <Step n={4} text="Start the runner from the same folder:">
          <CopyBlock testID="setup-compose-up" label="the docker compose command" text="docker compose up -d" />
        </Step>
        <Step n={5} text={props.afterStart} />
      </If>
    </SettingsGroup>
  );
}
