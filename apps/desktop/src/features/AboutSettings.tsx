import { appInfo, cleanup, Clipboard, effect, openUrl, platform, signal } from "nsh";
import type { ViewRef } from "nsh";
import type { ServerUpdate } from "../api/serverUpdate";
import { apiClient, connectedTo, me, orgAdmin, sessionGuard, status } from "../store";
import { desktopUpdates, JOURNAL_UNCERTAIN, METRO_LICENSES, METRO_RELEASES, METRO_SOURCE, needsManualRecovery, newerVersion, officialReleaseUrl, outcomeKey, stableVersion } from "../updates";
import type { DesktopUpdates, UpdateOffer, UpdateState } from "../updates";
import { Action, ActionBar, Badge, Banner, Column, compactWidth, ConfirmInline, Disclosure, Label, PageHeader,
  PageLayout, ProgressLine, SettingRow, SettingsGroup, Toggle } from "../ui";
import { webActivityDisclosure, webDeviceSelection, webModelDisclosure, webProviderDisclosure } from "./webSearchPrivacy";

const buildName = { release: "Published release", preview: "Preview build", source: "Source build" } as const;
const installName = {
  "linux-installer": "Linux installer",
  "windows-script": "Windows script installation",
  "windows-msi": "Windows MSI installation",
  portable: "Portable copy",
  unsupported: "Unsupported",
} as const;
const platformName: Record<string, string> = { linux: "Linux", windows: "Windows", macos: "macOS", ios: "iOS", android: "Android", web: "Web" };
const checkDescription = "After startup and about once a day. Checks never download an update for you or send your account, device ID or usage data.";
const autoLabel = "Check for updates automatically";

function notInApp(s: UpdateState) {
  switch (s.current.install) {
    case "linux-installer": return platform.name === "linux" && s.reason === "manual_migration_required"
      ? "This Linux install uses an older layout. Run the current Metro Linux installer once to enable in-app updates."
      : "This install can check for updates, but can't apply them in Metro.";
    case "windows-msi": return "This copy can't install updates in Metro. Get the MSI from Metro Releases and run it yourself. The Windows MSI is unsigned and Windows may warn you.";
    case "windows-script": return s.reason === "manual_migration_required"
      ? "Run the Windows installer script once with -Action Update to turn on in-app updates."
      : "This copy can't install updates in Metro. Use the Windows installer script from Metro Releases with -Action Update.";
    case "portable": return "This is a portable copy. Get the package for your platform from Metro Releases and extract it yourself. Metro won't replace this folder.";
    default: return "In-app updates aren't supported on this platform.";
  }
}

function failureMessage(code: string | undefined, msiOutcome = false) {
  switch (code) {
    case "offline": return "Couldn't reach GitHub. Check your connection and try again.";
    case "rate_limited": return "GitHub asked Metro to wait. Try again later.";
    case "trust_root_stale": return "This copy of Metro can't verify the new release with its current verification data. Use the official releases page to update manually.";
    case "not_verified": return "The signature or checksum didn't pass verification. No update was installed.";
    case "archive": return "The package didn't match its file list. No update was installed.";
    case "inconsistent": return "The release files didn't match each other. No update was installed. Check again later.";
    case "redirect_refused": return "GitHub redirected the request to an untrusted address. Metro stopped.";
    case "too_large": return "A release file was larger than expected. Metro didn't use it.";
    case "manifest":
    case "not_offered": return "Metro couldn't confirm a complete, published release.";
    case "installer_busy": return msiOutcome
      ? "Another installation was running. Try again when it has finished."
      : "Another installation or update is using this Metro install. No update was installed. Try again when it finishes.";
    case "installer_cancelled": return "The update was cancelled. Metro wasn't changed.";
    case "installer_failed": return "Windows Installer couldn't update Metro. Metro wasn't changed.";
    case "stage_verification_failed": return "Metro couldn't verify the staged update. Use the official installer to update manually.";
    case JOURNAL_UNCERTAIN: return "Metro couldn't verify the update journal. Stop here and contact the Metro maintainers.";
    case "backup_failed": return "Metro couldn't back up the local workspace. The update wasn't started.";
    case "missing_libraries": return "This version needs system libraries that aren't installed. Install the required GTK, WebKitGTK and GStreamer packages before trying again.";
    case "not_latest": return "That version is no longer the current verified offer. Check again.";
    case "not_ready": return "That update isn't ready to install. Check again.";
    case "not_in_app": return "This copy can't install updates in the app. Check its installation type.";
    case "nothing_to_cancel": return "The download is no longer running.";
    case "busy": return "An update step is already running. Try again when it finishes.";
    case "http": return "GitHub returned an error. Try again later.";
    case "io": return "Metro couldn't save an update file. Check available space and install-folder permissions.";
    default: return "The updater couldn't finish this step. Check its status and try again.";
  }
}

function OfficialLink(props: { label: string; url: string; testID?: string }) {
  const failed = signal(false);
  const launch = async () => {
    failed.set(false);
    try { failed.set(!(await openUrl(props.url)).opened); } catch { failed.set(true); }
  };
  return <Column gap={1} align="start">
    <Action label={props.label} accessibilityLabel={`${props.label} (opens in browser)`} testID={props.testID} variant="ghost" onPress={() => void launch()} />
    <If when={failed()}><Label variant="caption" color="danger">Couldn't open your browser.</Label></If>
  </Column>;
}

/** Read-only server notice. Updates for the computer are separate and need no Metro account. */
function serverInfo() {
  const answer = signal<{ valid: () => boolean; data: ServerUpdate } | null>(null);
  const adminOnline = () => orgAdmin() && status().phase === "online";
  let controller: AbortController | undefined;
  let activeClient: ReturnType<typeof apiClient> = null;
  cleanup(() => controller?.abort());
  effect(() => {
    const url = connectedTo();
    const user = me();
    const candidate = adminOnline() && url && user ? apiClient() : null;
    const client = candidate?.baseUrl === url ? candidate : null;
    if (activeClient === client) return;
    controller?.abort();
    answer.set(null);
    activeClient = client;
    if (!client) return;
    const request = controller = new AbortController();
    const sameSession = sessionGuard();
    const valid = () => !request.signal.aborted && sameSession() && apiClient() === client && adminOnline()
      && connectedTo() === url && me() === user;
    void client.serverUpdate(request.signal).then(data => {
      if (data && valid() && stableVersion(data.current)) answer.set({ valid, data });
    }).catch(() => {}); // 404 may be older server or access revoked; show no operator notice.
  });
  return () => {
    const result = answer();
    return result?.valid() ? result.data : null;
  };
}

function serverHost(url: string | null) {
  if (!url) return "Not connected";
  try { return new URL(url).host; } catch { return "Connected server"; }
}

function ServerNotice(props: { data: ServerUpdate }) {
  const latest = () => {
    const s = props.data;
    const l = s.latest;
    return s.available && l && newerVersion(l.version, s.current) ? l : undefined;
  };
  const releaseUrl = () => officialReleaseUrl(latest()?.version, latest()?.releaseUrl);
  return <SettingsGroup title="Server">
    <SettingRow label="Server updates" testID="about-server-update" stacked
      description={`This server runs ${props.data.current}.${latest() ? ` ${latest()!.version} is available.` : ""} The operator updates the server.`}>
      <ActionBar>
        <Badge label="Org admin" tone="neutral" />
        <If when={releaseUrl()}>
          {url => <OfficialLink label="Server release notes" url={url()} testID="about-server-release" />}
        </If>
      </ActionBar>
    </SettingRow>
  </SettingsGroup>;
}

function Privacy(props: { focusTitle?: boolean }) {
  return <SettingsGroup title="Privacy" testID="about-privacy" focusTitle={props.focusTitle}>
    <SettingRow label="Update checks"
      description="Metro contacts github.com and its release asset hosts (release-assets.githubusercontent.com or objects.githubusercontent.com). A newer release can also make a request to api.github.com for notes. If verification data is out of date, Metro can refresh it at tuf-repo-cdn.sigstore.dev. These requests don't send your account, device ID or usage data. If you're offline, the check can't confirm the latest release." />
    <SettingRow label="Web search" description={webDeviceSelection} />
    <SettingRow label="Web providers" description={webProviderDisclosure} />
    <SettingRow label="Web activity" description={webActivityDisclosure} />
    <SettingRow label="Web results" description={webModelDisclosure} />
    <SettingRow label="Model connections"
      description="What you send goes to the connection that answers. A shared connection's owner can see those requests and may pay for them." />
    <SettingRow label="Your Metro server"
      description="Your server stores your messages, files and settings. Its operator can read them." />
    <SettingRow label="Analytics and crash reports" description="Metro sends neither." />
    <SettingRow label="Monitoring"
      description="This computer's monitoring events aren't sent by update checks. A server also keeps its own monitoring events." />
  </SettingsGroup>;
}

function journalNeedsSupport(updates: DesktopUpdates) {
  return updates.state()?.failure === JOURNAL_UNCERTAIN || updates.problem()?.code === JOURNAL_UNCERTAIN;
}

function UpdateOutcome(props: { updates: DesktopUpdates }) {
  const updates = props.updates;
  return <Each of={updates.outcome() ? [updates.outcome()!] : []} key={outcomeKey}>{last => {
    const copied = signal("");
    const label = () => ({ updated: "Update installed", install_failed: "Update not installed",
      not_confirmed: "Update not confirmed", rolled_back: "Previous version restored" })[last().outcome];
    const detail = () => {
      const { from, to, outcome, failureCode } = last();
      switch (outcome) {
        case "updated": return `Metro updated from ${from} to ${to}.`;
        case "rolled_back": return `Metro ${to} didn't start successfully. Metro went back to ${from}.`;
        case "not_confirmed": return `Metro ${to} didn't finish starting. Metro kept ${from}.`;
        case "install_failed": return `Metro didn't install ${to}. ${journalNeedsSupport(updates) ? failureMessage(JOURNAL_UNCERTAIN)
          : failureCode ? failureMessage(failureCode, updates.state()?.current.install === "windows-msi")
            : "Metro couldn't confirm the stage was safe to use. Use the official installer to update manually."}`;
      }
    };
    const copyDetails = async () => {
      const report = `${label()}: ${detail()}`;
      try { await Clipboard.writeText(report); copied.set("Details copied."); }
      catch { copied.set("Couldn't copy details."); }
    };
    return <SettingRow label={label()} description={detail()} testID="update-outcome" stacked>
      <ActionBar>
        <If when={last().outcome !== "updated"}>
          <Action label="Copy details" variant="ghost" testID="update-copy-details" onPress={() => void copyDetails()} />
        </If>
        <Action label="Dismiss" variant="ghost" testID="update-dismiss-outcome" onPress={() => { updates.dismissOutcome(last()); copied.set(""); }} />
      </ActionBar>
      <If when={copied()}><Label variant="caption" color="muted">{copied()}</Label></If>
    </SettingRow>;
  }}</Each>;
}

function ReleaseNotes(props: { release: NonNullable<UpdateState["latest"]> }) {
  const open = signal(false);
  return <Disclosure testID="update-notes" title={`What's new in ${props.release.version}`} open={open()} onToggle={open.set}>
    {/* Plain native text. Never parse markup, fetch images, or auto-link notes. */}
    <Column selectable>
      <Label testID="update-notes-text">{props.release.notes || "Release notes aren't available here."}</Label>
    </Column>
    <If when={props.release.notesTruncated}><Label variant="caption" color="muted">These notes were shortened.</Label></If>
    <If when={props.release.url}>
      {url => <OfficialLink label="Release notes on GitHub" url={url()} testID="update-release-link" />}
    </If>
  </Disclosure>;
}

function RestartUpdate(props: { target: UpdateOffer; updates: DesktopUpdates }) {
  const updates = props.updates;
  const confirmation = signal<"msi" | "interrupt" | null>(null);
  const isMsi = () => updates.state()?.current.install === "windows-msi";
  const deferred = signal(false);
  let restartButton: ViewRef | undefined;
  let alive = true;
  cleanup(() => { alive = false; });
  const busyText = () => {
    const busy = updates.state()?.busy;
    const count = busy?.servingConnections ?? 0;
    const jobs = [count ? `${count} device ${count === 1 ? "connection" : "connections"}` : "",
      busy?.localWorkspace ? "the local workspace" : ""].filter(Boolean).join(" and ");
    return jobs ? `Restarting now stops work in ${jobs}. Restart anyway?` : "Metro may have on-device work in progress. Restarting stops that work. Restart anyway?";
  };
  const restart = async (confirmed: boolean) => {
    const target = props.target;
    if (confirmed && confirmation() !== "interrupt") return;
    const refused = await updates.restart(target, confirmed);
    if (alive && updates.state()?.phase === "ready" && updates.offer()?.identity === target.identity) {
      confirmation.set(refused === "would_interrupt" ? "interrupt" : null);
    }
  };
  return <Column gap={2}>
    <If when={deferred()} else={<ActionBar>
      <Action label="Restart now" variant="primary" testID="update-restart" viewRef={node => { restartButton = node; }}
        busyLabel="Checking work…" busy={updates.pending() === "restart" && !confirmation()}
        disabled={!!updates.pending() || !!confirmation()} onPress={() => {
          if (isMsi()) confirmation.set("msi");
          else void restart(false);
        }} />
      <Action label="Later" variant="ghost" testID="update-later" disabled={!!updates.pending()}
        onPress={() => { confirmation.set(null); deferred.set(true); }} />
    </ActionBar>}>
      <Action label="Show restart options" variant="ghost" testID="update-show-restart" onPress={() => deferred.set(false)} />
    </If>
    <ConfirmInline open={!!confirmation()} onOpenChange={open => { if (!open) confirmation.set(null); }}
      returnFocus={() => restartButton} scopeKey={props.target.identity}
      testID={confirmation() === "msi" ? "update-msi-restart" : "update-interrupt"}
      title={confirmation() === "msi" ? undefined : "Work in progress"}
      message={confirmation() === "msi" ? "Metro closes, Windows Installer updates it, then Metro starts again." : busyText()}
      confirmLabel={confirmation() === "msi" ? "Restart now" : "Restart anyway"} variant="primary"
      busy={updates.pending() === "restart"} busyLabel="Restarting…" disabled={!!updates.pending()}
      onConfirm={() => void restart(confirmation() === "interrupt")} />
  </Column>;
}

function PublishedUpdates(props: { updates: DesktopUpdates }) {
  const updates = props.updates;
  const s = () => updates.state()!;
  const install = () => s().current.install;
  const diagnostic = () => {
    if (install() === "windows-msi") return " Keep the staged MSI. Share Metro's recovery message, not private files.";
    return (install() === "windows-script" ? " Use the verified Windows installer script with -Action Status for read-only diagnosis."
      : " Use the verified Linux installer's status command for read-only diagnosis.") + " Share the diagnostic, not private files.";
  };
  const recoveryInstaller = () => install() === "windows-msi" ? "the Windows MSI"
    : install() === "windows-script" ? "the Windows installer script" : "the Linux installer";
  const offer = updates.offer;
  const phase = () => s().phase;
  const actionBusy = () => !!updates.pending();
  const checking = () => phase() === "checking" || updates.pending() === "check";
  const transfer = () => phase() === "downloading" || phase() === "verifying" || phase() === "staging";
  const ready = () => s().canUpdateInApp && phase() === "ready" ? offer() : undefined;
  const journalBlocked = () => journalNeedsSupport(updates);
  const blocked = () => {
    const last = s().lastUpdate;
    return s().canUpdateInApp && last?.from === s().current.version && needsManualRecovery(last) ? last.to : undefined;
  };
  const error = () => {
    const v = s();
    if (v.anomaly === "older_release_served") return "GitHub returned an older release than Metro already verified. Metro won't use it. Check again later.";
    if (v.anomaly) return "Release files changed after Metro verified them. Metro won't use them. Check again later.";
    if (v.persistenceError === "status_io") return "Metro can't read or save its update record. It won't install an update without that record.";
    if (v.persistenceError === "settings_io") return "Metro can't read or save this computer's update settings.";
    if (journalBlocked()) return failureMessage(JOURNAL_UNCERTAIN);
    if (v.phase === "failed") return failureMessage(v.failure);
    if (v.error || v.result === "error") return failureMessage(v.error);
    if (blocked() && v.latest?.version === blocked()) return `Metro won't retry the staged update to ${blocked()} in the app.`;
    if (v.result === "available" && !offer() && !updates.problem()) return "Check again for a current verified offer.";
    return "";
  };
  const statusText = () => {
    if (phase() === "applying") return "Metro is restarting to finish the update…";
    if (transfer()) return "No update has been installed.";
    if (checking()) return "Checking for updates…";
    if (ready()) return `Metro ${ready()!.version} is verified and ready. Restart to finish the update.`;
    if (error()) return error();
    if (offer()) return `Metro ${offer()!.version} is available. You have ${s().current.version}.`;
    if (s().result === "up_to_date") return `Metro ${s().current.version} is up to date.`;
    return "Not checked yet.";
  };
  const progressText = () => {
    if (updates.cancelling()) return "Cancelling…";
    if (phase() === "verifying") return "Checking the signature and checksum…";
    if (phase() === "staging") return "Preparing the verified update…";
    return `Downloading ${s().latest ? `Metro ${s().latest!.version}` : "the update"}…`;
  };
  const lastChecked = () => {
    const at = s().lastCheckedAt;
    return at === null ? "Never" : new Date(at).toLocaleString();
  };
  return <>
    <SettingRow label="Status" description={statusText()} testID="update-status" stacked={transfer() || !!ready()}>
      <If when={transfer()}>
        <ProgressLine testID="update-progress" label={progressText()}
          value={phase() === "downloading" && !updates.cancelling() ? s().progress : undefined} />
        <If when={phase() === "downloading" || phase() === "verifying"}>
          <Action label="Cancel download" variant="ghost" testID="update-cancel" disabled={actionBusy() || updates.cancelling()}
            onPress={() => void updates.cancel()} />
        </If>
      </If>
      <Each of={ready() ? [ready()!] : []} key={target => target.identity}>{target => <Column gap={2} align="start">
        <Badge tone="success" label="Ready" />
        <RestartUpdate target={target()} updates={updates} />
      </Column>}</Each>
      <If when={!checking() && phase() === "failed"}><Badge tone="danger" label="Failed" /></If>
      <If when={phase() === "idle" && !checking() && !error() && !ready()}>
        <If when={offer()} else={<If when={s().result === "up_to_date"}><Badge tone="success" label="Up to date" /></If>}>
          <Badge tone="info" label="Update available" />
        </If>
      </If>
    </SettingRow>
    <If when={!s().canUpdateInApp}>
      <SettingRow label="Manual updates" description={notInApp(s())} testID="update-manual" stacked>
        <OfficialLink label="Open Metro releases" url={offer()?.url ?? METRO_RELEASES} testID="update-download-page" />
      </SettingRow>
    </If>
    <If when={journalBlocked()}>
      <SettingRow label="Update recovery" testID="update-journal-recovery" stacked
        description={`Stop Metro, its bundled server and any installer. Keep the installation, update records and both database copies. Do not remove locks or records, change the current version, or run install, update or uninstall.${diagnostic()}`}>
        <OfficialLink label="Contact Metro maintainers" url={`${METRO_SOURCE}/issues`} testID="update-recovery-support" />
      </SettingRow>
    </If>
    <If when={!journalBlocked() && blocked()}>
      {version => <SettingRow label="Manual recovery" testID="update-manual-recovery" stacked
        description={`The staged update to ${version()} wasn't used. To update manually, get ${recoveryInstaller()} from Metro's official releases.`}>
        <OfficialLink label="Open Metro releases" url={METRO_RELEASES} testID="update-recovery-release" />
      </SettingRow>}
    </If>
    <If when={updates.problem()}>{problem => {
      const message = () => {
        const { command, code } = problem();
        if (command === "settings") return "Metro couldn't save the automatic-check setting.";
        if (command === "restart" && code === "unreachable") return "Metro didn't confirm the restart. Don't assume the update was installed.";
        return failureMessage(code);
      };
      return <SettingRow label="Update action" testID="update-action-error" stacked><Banner tone="warning" variant="inline" message={message()} /></SettingRow>;
    }}</If>
    <If when={updates.notice()}>{message => <SettingRow label="Download" description={message()} testID="update-notice" />}</If>
    <SettingRow label={autoLabel} description={checkDescription}>
      <Toggle label={autoLabel} testID="update-auto" checked={s().autoCheck}
        disabled={actionBusy() || phase() === "applying"} onChange={value => void updates.setAutoCheck(value)} />
    </SettingRow>
    <SettingRow label="Last checked" description={lastChecked()} testID="update-last-checked">
      <Action label={error() || updates.problem() ? "Check again" : "Check now"} busyLabel="Checking…" busy={checking()}
        disabled={actionBusy() || transfer() || phase() === "applying"} testID="update-check" onPress={() => void updates.check()} />
    </SettingRow>
    <Each of={offer() ? [offer()!] : []} key={release => release.identity}>{release => <SettingRow label={`Metro ${release().version}`} stacked>
      <ReleaseNotes release={release()} />
      <If when={s().canUpdateInApp && phase() === "idle" && !checking()}>
        <ConfirmInline testID="update-start" triggerLabel={`Update to ${release().version}…`} confirmLabel="Download and verify"
          message={`Metro downloads ${release().version} from GitHub and verifies its signature and checksum. It won't install anything until you choose to restart.`}
          scopeKey={release().identity} variant="primary" disabled={actionBusy()}
          onConfirm={() => void updates.download(release())} />
      </If>
    </SettingRow>}</Each>
  </>;
}

function UpdatePanel(props: { updates: DesktopUpdates }) {
  const updates = props.updates;
  const problem = updates.readProblem;
  const s = updates.state;
  return <SettingsGroup title="Updates">
    <If when={problem()}>{code => {
      const detail = () => ({
        no_launcher: "This copy isn't running through the desktop launcher. It can't check for or install updates here.",
        old_launcher: "This desktop launcher doesn't report update status. Use Metro Releases to update manually.",
        update_unavailable: "Updates are handled by the first Metro window.",
      } as Record<string, string>)[code()] ?? "Metro couldn't read the updater's status on this computer.";
      return <SettingRow label="Updates unavailable" description={detail()} testID="update-unavailable">
        <If when={code() !== "no_launcher"}>
          <Action label="Refresh status" variant="ghost" testID="update-refresh" onPress={() => void updates.refresh()} />
        </If>
      </SettingRow>;
    }}</If>
    <If when={!problem() && !s()}>
      <SettingRow label="Status" description="Reading this computer's update status…" testID="update-loading" />
    </If>
    <If when={s() && !problem()}>
      <If when={s()!.canCheck} else={
        <SettingRow label="Updates unavailable"
          testID={s()?.current.channel === "source" ? "update-source-build" : "update-unsupported"}
          description={s()?.current.channel === "source" ? "This is a source build. It doesn't check for or install updates."
            : s()?.current.channel === "preview" ? "Preview builds don't check for or install updates."
            : "Metro can't check for or install updates on this platform."} />
      }>
        <UpdateOutcome updates={updates} />
        <PublishedUpdates updates={updates} />
      </If>
    </If>
  </SettingsGroup>;
}

/** Settings page `about`; navigation and entry-point indicators belong to the Settings shell. */
export function AboutSettings(props: { updates?: DesktopUpdates; focusPrivacy?: boolean } = {}) {
  const updates = props.updates ?? desktopUpdates;
  cleanup(updates.watch());
  const server = serverInfo();
  const ui = appInfo();
  const info = () => updates.state()?.current;
  const version = () => info()?.version ?? ui.version ?? "Unknown";
  const build = () => [ui.build !== null ? `Build ${ui.build}` : null, info() ? installName[info()!.install] : "Launcher details unavailable"]
    .filter(Boolean).join(" · ");
  const currentRelease = () => info()?.channel === "release" ? `${METRO_RELEASES}/tag/v${info()!.version}` : METRO_RELEASES;
  return <PageLayout testID="about-settings" pad={compactWidth() ? 4 : 6} gap={6}>
    <PageHeader title="About & updates" scope={{ kind: "computer" }} />
    <SettingsGroup title="Metro">
      <SettingRow label="Version" description={version()} testID="about-version">
        <If when={info()}>{current => <Badge label={buildName[current().channel]} tone="neutral" />}</If>
      </SettingRow>
      <SettingRow label="Build" description={build()} testID="about-build" />
      <SettingRow label="Platform" description={platformName[platform.name] ?? platform.name} testID="about-platform" />
      <SettingRow label="Connected server" testID="about-server-version"
        description={connectedTo() ? `${serverHost(connectedTo())} · ${server() ? `server ${server()!.current}` : "version unknown"}` : "Not connected"} />
      <SettingRow label="Links" stacked>
        <ActionBar>
          <OfficialLink label="Release notes" url={currentRelease()} testID="about-releases" />
          <OfficialLink label="Source (GitHub)" url={METRO_SOURCE} testID="about-source" />
          <OfficialLink label="Third-party license sources" url={METRO_LICENSES} testID="about-licenses" />
        </ActionBar>
        <Label variant="caption" color="muted">Packaged copies include their license notices in the licenses folder.</Label>
      </SettingRow>
    </SettingsGroup>
    <UpdatePanel updates={updates} />
    <If when={server()}>{data => <ServerNotice data={data()} />}</If>
    <Privacy focusTitle={props.focusPrivacy} />
  </PageLayout>;
}
