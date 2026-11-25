// Space settings: a modal over the app, opened per Space
// (openSpaceSettings). Every member sees General, Members, Runner, Agents
// and Your preferences; only the owner changes shared settings.
// Closing returns to where the user was.
//
// Destructive steps follow the app's pattern: an outlined first press, then
// a filled confirmation that says what happens (Delete also needs the
// Space's exact name typed).

import { openSettings } from "../../store";
import {
  Action,
  ActionBar,
  AgentAvatar,
  Badge,
  Banner,
  ColorSwatches,
  Column,
  compactWidth,
  Field,
  IconButton,
  Label,
  Modal,
  ModalNav,
  ModalTab,
  ModalTabRow,
  ModelPicker,
  PageHeader,
  PageLayout,
  Row,
  SettingRow,
  SettingsGroup,
  Spacer,
  Surface,
  Toggle,
} from "../../ui";
import type { IconName } from "../../ui";
import { MAX_AGENT_DESCRIPTION, MAX_AGENT_HANDLE, MAX_AGENT_INSTRUCTIONS, MAX_AGENT_NAME, MAX_SPACE_DESCRIPTION, initialOf, suggestHandle } from "../../api/spaces";
import type { SpaceAgent } from "../../api/spaces";
import { agentList, spaceAgents, toolInfo } from "./agents";
import { SpaceAgentAvatar } from "./SpaceAgentAvatar";
import { MembersPage, RunnerPage } from "./SpaceManagementPages";
import { effortsOf, selectionOf, shownValue, spaceModelOptions, thinkingOptions, unlisted, withEffort } from "./modelOptions";
import { ThinkingChoice } from "../../ui/composerModelTrigger";
import {
  agentBusy,
  agentDraft,
  agentError,
  askDanger,
  canAddAgent,
  cancelAgentEdit,
  catalog,
  catalogError,
  chooseAgentPicture,
  closeSpaceSettings,
  confirmDeleteAgent,
  dangerBusy,
  dangerError,
  dangerOpen,
  deleteAgentNow,
  deleteConfirmText,
  deleteNameMatches,
  deleteSpaceNow,
  descriptionDraft,
  details,
  detailsError,
  draftDirty,
  draftHandleProblem,
  draftProblem,
  editedAgent,
  editing,
  generalDirty,
  generalProblem,
  isSettingsOwner,
  leaveSpaceNow,
  loadDetails,
  loadPreferences,
  makeDefaultAgent,
  nameDraft,
  pendingPicture,
  prefs,
  prefsBusy,
  prefsError,
  removeAgentPicture,
  saveAgent,
  saveGeneral,
  savingAgent,
  savingGeneral,
  setAgentModel,
  setClassifyUnaddressed,
  setSpaceDefaultModel,
  setSpaceRouting,
  settingsSpace,
  showSection,
  spaceNotice,
  spaceSettingsFor,
  spaceSettingsSection,
  startEditAgent,
  startNewAgent,
  updateDraft,
} from "./spaceSettingsState";
import type { SpaceSettingsSection } from "./spaceSettingsState";

const PAGES: readonly { section: SpaceSettingsSection; label: string; icon: IconName }[] = [
  { section: "general", label: "General", icon: "settings" },
  { section: "members", label: "Members", icon: "person" },
  { section: "runner", label: "Runner", icon: "server" },
  { section: "agents", label: "Agents", icon: "agent" },
  { section: "preferences", label: "Your preferences", icon: "routing" },
];

/** The Space settings modal. Mount it once (the Shell); it shows while a Space's settings are open. */
export function SpaceSettingsModal() {
  return (
    <Modal open={!!spaceSettingsFor()} title={`${settingsSpace()?.name ?? "Space"} settings`} onClose={closeSpaceSettings} testID="space-settings-dialog">
      {/* Unmounted while closed: half-typed drafts never outlive it. */}
      <If when={spaceSettingsFor()}>
        <If when={compactWidth()} else={<Wide />}>
          <Compact />
        </If>
      </If>
    </Modal>
  );
}

function Wide() {
  return (
    <Row grow testID="space-settings">
      <ModalNav testID="space-settings-nav" accessibilityLabel="Space settings sections">
        <Row padX={3} padY={2}>
          <Label variant="heading" heading lines={1}>
            {settingsSpace()?.name ?? ""}
          </Label>
        </Row>
        <Tabs compact={false} />
      </ModalNav>
      <Column grow>
        <Row align="center" padX={3} padY={2}>
          <Spacer />
          <CloseButton />
        </Row>
        <Page />
      </Column>
    </Row>
  );
}

function Compact() {
  return (
    <Column grow testID="space-settings">
      <Row align="center" padX={4} padY={2} gap={2}>
        <Label variant="heading" heading lines={1}>
          {settingsSpace()?.name ?? ""}
        </Label>
        <Spacer />
        <CloseButton />
      </Row>
      <ModalTabRow testID="space-settings-nav">
        <Tabs compact />
      </ModalTabRow>
      <Page />
    </Column>
  );
}

function Tabs(props: { compact: boolean }) {
  return (
    <Each of={PAGES} key={(p) => p.section}>
      {(p) => (
        <ModalTab
          testID={`space-settings-tab:${p().section}`}
          label={p().label}
          icon={p().icon}
          compact={props.compact}
          selected={spaceSettingsSection() === p().section}
          onPress={() => showSection(p().section)}
        />
      )}
    </Each>
  );
}

function CloseButton() {
  return <IconButton testID="space-settings-close" icon="close" accessibilityLabel="Close Space settings" tooltip="Close (Esc)" onPress={closeSpaceSettings} />;
}

function Page() {
  return (
    <Surface variant="panel" grow testID="space-settings-page">
      <If when={spaceSettingsSection() === "general"}>
        <GeneralPage />
      </If>
      <If when={spaceSettingsSection() === "members"}>
        <MembersPage />
      </If>
      <If when={spaceSettingsSection() === "runner"}>
        <RunnerPage />
      </If>
      <If when={spaceSettingsSection() === "agents"}>
        <AgentsPage />
      </If>
      <If when={spaceSettingsSection() === "preferences"}>
        <PreferencesPage />
      </If>
    </Surface>
  );
}

/** What the last action did, in the fixed footer so it's seen. */
function Notice() {
  return (
    <If when={spaceNotice()}>
      {(n) => <Banner variant="inline" testID="space-settings-notice" tone={n().ok ? "success" : "danger"} message={n().message} />}
    </If>
  );
}

const pad = () => (compactWidth() ? 4 : 6);

// ---------------------------------------------------------------- General

function GeneralPage() {
  return (
    <PageLayout
      testID="space-general"
      pad={pad()}
      gap={6}
      footer={
        <Column gap={2}>
          <Notice />
          <If when={isSettingsOwner() && details()}>
            <ActionBar justify="between" testID="space-general-footer">
              <Column grow>
                <If
                  when={generalDirty() ? generalProblem() : null}
                  else={
                    <If when={generalDirty()}>
                      <Label variant="caption" color="muted">
                        Unsaved changes
                      </Label>
                    </If>
                  }
                >
                  {(problem) => (
                    <Label variant="caption" color="danger" testID="space-general-problem">
                      {problem()}
                    </Label>
                  )}
                </If>
              </Column>
              <Action
                testID="space-general-save"
                label="Save"
                busyLabel="Saving…"
                variant="primary"
                busy={savingGeneral()}
                disabled={!generalDirty() || !!generalProblem()}
                onPress={() => void saveGeneral()}
              />
            </ActionBar>
          </If>
        </Column>
      }
    >
      <PageHeader title="General" scope={{ kind: "space" }} subtitle={isSettingsOwner() ? "Every member of this Space sees its name and description." : "Only the owner can change this."} />
      <If when={detailsError()}>
        {(m) => (
          <Banner variant="inline" tone="danger" testID="space-details-error" message={m()} action={<Action label="Try again" onPress={() => void loadDetails()} />} />
        )}
      </If>
      <If
        when={details()}
        else={
          <If when={!detailsError()}>
            <Label color="muted" testID="space-details-loading">
              Loading this Space…
            </Label>
          </If>
        }
      >
        {(d) => (
          <SettingsGroup title="About this Space">
            <If
              when={isSettingsOwner()}
              else={
                <>
                  <SettingRow label="Name" stacked>
                    <Label testID="space-name-readonly">{d().name}</Label>
                  </SettingRow>
                  <SettingRow label="Description" stacked>
                    <Label color={d().description ? "default" : "muted"} testID="space-description-readonly">
                      {d().description || "No description."}
                    </Label>
                  </SettingRow>
                </>
              }
            >
              <SettingRow label="Name" stacked>
                <Field testID="space-name" accessibilityLabel="Space name" value={nameDraft()} onChange={nameDraft.set} placeholder="Space name" disabled={savingGeneral()} />
              </SettingRow>
              <SettingRow label="Description" description={`Optional, up to ${MAX_SPACE_DESCRIPTION} characters. What this Space is for.`} stacked>
                <Field
                  testID="space-description"
                  accessibilityLabel="Space description"
                  value={descriptionDraft()}
                  onChange={descriptionDraft.set}
                  placeholder="What this Space is for"
                  multiline
                  maxLength={MAX_SPACE_DESCRIPTION}
                  disabled={savingGeneral()}
                />
              </SettingRow>
            </If>
          </SettingsGroup>
        )}
      </If>
      <DangerZone />
    </PageLayout>
  );
}

function DangerZone() {
  const name = () => details()?.name ?? settingsSpace()?.name ?? "";
  return (
    <SettingsGroup title="Danger zone" testID="space-danger">
      <If
        when={isSettingsOwner()}
        else={
          <SettingRow
            label="Leave this Space"
            description="You stop seeing its rooms, messages and files, and runs you started here stop. The owner can add you again."
            stacked={dangerOpen() === "leave"}
          >
            <If
              when={dangerOpen() === "leave"}
              else={<Action variant="destructive" testID="leave-space" label="Leave Space…" onPress={() => askDanger("leave")} />}
            >
              <Column gap={2}>
                <If when={dangerError()}>
                  {(m) => (
                    <Label variant="caption" color="danger" testID="danger-error">
                      {m()}
                    </Label>
                  )}
                </If>
                <ActionBar>
                  <Action label="Cancel" variant="ghost" testID="leave-cancel" onPress={() => askDanger(null)} />
                  <Action testID="leave-confirm" variant="danger" label={`Leave ${name()}`} busyLabel="Leaving…" busy={dangerBusy()} onPress={() => void leaveSpaceNow()} />
                </ActionBar>
              </Column>
            </If>
          </SettingRow>
        }
      >
        <SettingRow
          label="Delete this Space"
          description="Permanently deletes its rooms, threads, messages, library files, scheduled prompts and Space Pages, including page history and drafts. It detaches its runner. You can’t recover this Space from Trash."
          stacked={dangerOpen() === "delete"}
        >
          <If
            when={dangerOpen() === "delete"}
            else={<Action variant="destructive" testID="delete-space" label="Delete Space…" onPress={() => askDanger("delete")} />}
          >
            <Column gap={2} testID="delete-space-confirm-panel">
              <Label variant="caption">{`To confirm, type the Space's name: ${name()}`}</Label>
              <Field
                testID="delete-space-name"
                accessibilityLabel={`Type ${name()} to confirm`}
                value={deleteConfirmText()}
                onChange={deleteConfirmText.set}
                placeholder={name()}
                disabled={dangerBusy()}
                autoFocus
              />
              <If when={dangerError()}>
                {(m) => (
                  <Label variant="caption" color="danger" testID="danger-error">
                    {m()}
                  </Label>
                )}
              </If>
              <ActionBar>
                <Action label="Cancel" variant="ghost" testID="delete-cancel" onPress={() => askDanger(null)} />
                <Action
                  testID="delete-confirm"
                  variant="danger"
                  label="Delete this Space for good"
                  busyLabel="Deleting…"
                  busy={dangerBusy()}
                  disabled={!deleteNameMatches()}
                  onPress={() => void deleteSpaceNow()}
                />
              </ActionBar>
            </Column>
          </If>
        </SettingRow>
      </If>
    </SettingsGroup>
  );
}

// ---------------------------------------------------------------- Agents

function AgentsPage() {
  const sid = () => spaceSettingsFor() ?? "";
  const list = () => agentList(sid());
  return (
    <PageLayout
      testID="space-agents"
      pad={pad()}
      gap={6}
      footer={
        // Only when there is something to say or save: an empty footer would draw a bare bar.
        spaceNotice() || editing() ? (
          <Column gap={2}>
            <Notice />
            <If when={editing()}>
              <EditorFooter />
            </If>
          </Column>
        ) : undefined
      }
    >
      <If
        when={editing()}
        else={
          <>
            <PageHeader
              title="Agents"
              scope={{ kind: "space" }}
              subtitle={
                isSettingsOwner()
                  ? "Members address an agent with its @handle. @agent and Ask agent start the default one, or the agent a member's own classifier picks when they turned it on."
                  : "Members address an agent with its @handle. @agent and Ask agent start the default one, or your classifier's pick when you turn it on. Only the owner can change agents."
              }
            >
              <If when={isSettingsOwner()}>
                <Action testID="add-agent" label="Add agent" variant="primary" icon="plus" disabled={!canAddAgent()} onPress={startNewAgent} />
              </If>
            </PageHeader>
            <If when={list()?.error}>
              {(m) => <Banner variant="inline" tone="danger" testID="agents-error" message={m()} />}
            </If>
            <If when={!list() || list()!.status === "loading"}>
              <Label color="muted" testID="agents-loading">
                Loading agents…
              </Label>
            </If>
            <Column gap={3} testID="agent-list">
              <Each of={[...spaceAgents(sid())].sort((a, b) => Number(b.isDefault) - Number(a.isDefault))} key={(a) => a.id}>
                {(a) => <AgentCard agent={a()} />}
              </Each>
            </Column>
          </>
        }
      >
        <AgentEditor />
      </If>
    </PageLayout>
  );
}

function AgentCard(props: { agent: SpaceAgent }) {
  const a = () => props.agent;
  const sid = () => spaceSettingsFor() ?? "";
  const asking = () => confirmDeleteAgent() === a().id;
  return (
    <Surface variant="card" pad={4} gap={3} testID={`agent:${a().id}`}>
      <Row gap={3} align="start">
        <SpaceAgentAvatar spaceId={sid()} agent={a()} size="md" testID={`agent-avatar:${a().id}`} />
        <Column grow gap={1}>
          <Row gap={2} align="center" wrap>
            <Label variant="strong" testID={`agent-name:${a().id}`}>
              {a().name}
            </Label>
            <Label variant="mono" color="muted">{`@${a().handle}`}</Label>
            <If when={a().isDefault}>
              <Badge tone="agent" label="Default" testID={`agent-default:${a().id}`} />
            </If>
          </Row>
          <Label variant="caption" color="muted" testID={`agent-description:${a().id}`}>
            {a().description}
          </Label>
          <If when={a().tools.length} else={<Label variant="caption" color="muted">No optional tools</Label>}>
            <Row align="center" gap={1} wrap>
              <Label variant="caption" color="muted">Tools:</Label>
              <Each of={a().tools} key={(t) => t}>
                {(t, index) => <Label variant={toolInfo(t()) ? "caption" : "mono"} color="muted">{`${toolInfo(t())?.label ?? t()}${index() < a().tools.length - 1 ? "," : ""}`}</Label>}
              </Each>
            </Row>
          </If>
        </Column>
      </Row>
      <If when={isSettingsOwner()}>
        <Row gap={2} align="center" wrap>
          <Action compact testID={`edit-agent:${a().id}`} label="Edit" accessibilityLabel={`Edit ${a().name}`} onPress={() => startEditAgent(a().id)} />
          <If when={!a().isDefault}>
            <Action
              compact
              variant="ghost"
              testID={`default-agent:${a().id}`}
              label="Make default"
              accessibilityLabel={`Make ${a().name} the default agent`}
              busyLabel="Saving…"
              busy={agentBusy() === `default:${a().id}`}
              disabled={!!agentBusy()}
              onPress={() => void makeDefaultAgent(a().id)}
            />
          </If>
          <Spacer />
          <If
            when={asking()}
            else={
              <Action
                compact
                variant="destructive"
                testID={`delete-agent:${a().id}`}
                label="Delete…"
                accessibilityLabel={`Delete ${a().name}`}
                disabled={!!agentBusy() || spaceAgents(sid()).length <= 1}
                onPress={() => confirmDeleteAgent.set(a().id)}
              />
            }
          >
            <Action compact variant="ghost" label="Cancel" testID={`delete-agent-cancel:${a().id}`} onPress={() => confirmDeleteAgent.set(null)} />
            <Action
              compact
              variant="danger"
              testID={`delete-agent-confirm:${a().id}`}
              label={a().isDefault ? `Delete ${a().name}: another agent becomes the default` : `Delete ${a().name}`}
              busyLabel="Deleting…"
              busy={agentBusy() === `delete:${a().id}`}
              onPress={() => void deleteAgentNow(a().id)}
            />
          </If>
        </Row>
      </If>
    </Surface>
  );
}

function EditorFooter() {
  return (
    <ActionBar justify="between" testID="agent-editor-footer">
      <Column grow>
        <If when={agentError()} else={<If when={draftDirty() ? draftProblem() : null}>{(p) => <Label variant="caption" color="danger" testID="agent-problem">{p()}</Label>}</If>}>
          {(m) => (
            <Label variant="caption" color="danger" testID="agent-error">
              {m()}
            </Label>
          )}
        </If>
      </Column>
      <Action testID="agent-cancel" label="Cancel" variant="ghost" onPress={cancelAgentEdit} />
      <Action
        testID="agent-save"
        label={editing()?.mode === "new" ? "Add agent" : "Save"}
        busyLabel="Saving…"
        variant="primary"
        busy={savingAgent()}
        disabled={!!draftProblem() || !draftDirty()}
        onPress={() => void saveAgent()}
      />
    </ActionBar>
  );
}

function AgentEditor() {
  const d = () => agentDraft()!;
  const sid = () => spaceSettingsFor() ?? "";
  const isNew = () => editing()?.mode === "new";
  const current = () => editedAgent();
  const busyPicture = () => !!agentBusy()?.startsWith("picture:");
  const hasPicture = () => (isNew() ? !!pendingPicture() : !!current()?.avatarVersion);
  return (
    <If when={agentDraft()}>
      <Column gap={6} testID="agent-editor">
        <PageHeader title={isNew() ? "New agent" : `Edit ${current()?.name ?? "agent"}`} scope={{ kind: "space" }} subtitle="Members see everything on this page, including the instructions." />

        <SettingsGroup title="Picture" description="PNG or JPEG, up to 1 MB. Without one, the initial shows on the colour below.">
          <Row gap={4} align="center" padX={4} padY={4}>
            <If
              when={isNew() ? null : current()}
              else={<AgentAvatar testID="agent-editor-avatar" size="lg" initial={initialOf(d().name)} color={d().color} accessibilityLabel="Agent picture" />}
            >
              {(a) => <SpaceAgentAvatar testID="agent-editor-avatar" size="lg" spaceId={sid()} agent={{ ...a(), color: d().color, initial: initialOf(d().name || a().name) }} accessibilityLabel="Agent picture" />}
            </If>
            <Column gap={1}>
              <Row gap={2} wrap>
                <Action
                  testID="agent-picture-choose"
                  label={hasPicture() ? "Change picture…" : "Choose picture…"}
                  busyLabel="Uploading…"
                  busy={busyPicture()}
                  disabled={!!agentBusy()}
                  onPress={() => void chooseAgentPicture()}
                />
                <If when={hasPicture()}>
                  <Action testID="agent-picture-remove" label="Remove" variant="ghost" disabled={!!agentBusy()} onPress={() => void removeAgentPicture()} />
                </If>
              </Row>
              <If when={isNew() && pendingPicture()}>
                <Label variant="caption" color="muted" testID="agent-picture-pending">
                  The picture is sent when you add the agent.
                </Label>
              </If>
            </Column>
          </Row>
          <SettingRow label="Colour" description="Shown behind the initial when there's no picture." stacked>
            <ColorSwatches testID="agent-color" accessibilityLabel="Agent colour" value={d().color} onChange={(color) => updateDraft({ color })} />
          </SettingRow>
        </SettingsGroup>

        <SettingsGroup title="Identity">
          <SettingRow label="Name" stacked>
            <Field
              testID="agent-name"
              accessibilityLabel="Agent name"
              value={d().name}
              maxLength={MAX_AGENT_NAME}
              placeholder="Code reviewer"
              onChange={(name) => updateDraft(d().handleTouched ? { name } : { name, handle: suggestHandle(name) })}
            />
          </SettingRow>
          <SettingRow label="Handle" description={`How members mention it: @handle. Lowercase letters, digits, - and _, up to ${MAX_AGENT_HANDLE}.`} stacked>
            <Field
              testID="agent-handle"
              accessibilityLabel="Agent handle"
              value={d().handle}
              maxLength={MAX_AGENT_HANDLE}
              placeholder="reviewer"
              onChange={(handle) => updateDraft({ handle: handle.trim().replace(/^@/, ""), handleTouched: true })}
            />
            <If when={d().handle ? draftHandleProblem() : null}>
              {(p) => (
                <Label variant="caption" color="danger" testID="agent-handle-problem">
                  {p()}
                </Label>
              )}
            </If>
          </SettingRow>
          <SettingRow label="Description" description={`One line, up to ${MAX_AGENT_DESCRIPTION} characters. The classifier uses this to choose the agent.`} stacked>
            <Field
              testID="agent-description"
              accessibilityLabel="Agent description"
              value={d().description}
              maxLength={MAX_AGENT_DESCRIPTION}
              placeholder="Reviews pull requests and points out bugs"
              onChange={(description) => updateDraft({ description: description.replace(/[\r\n]+/g, " ") })}
            />
          </SettingRow>
        </SettingsGroup>

        <SettingsGroup title="Instructions" description="Added after Metro's built-in instructions. Every member of this Space can read them.">
          <SettingRow label="Instructions" description={`Optional, up to ${MAX_AGENT_INSTRUCTIONS} characters.`} stacked>
            <Field
              testID="agent-instructions"
              accessibilityLabel="Agent instructions"
              value={d().instructions}
              maxLength={MAX_AGENT_INSTRUCTIONS}
              placeholder="Be brief. Always cite the file and line."
              multiline
              onChange={(instructions) => updateDraft({ instructions })}
            />
          </SettingRow>
        </SettingsGroup>

        <AgentToolGroups />
      </Column>
    </If>
  );
}

const TOOL_GROUPS = [
  { key: "workspace", title: "Workspace and threads", description: "Workspace tools need a runner that offers them. Threads stay in this Space." },
  { key: "web", title: "Web", description: "Uses only the computer, internet address and providers of the person who started the run. If that computer is offline, no web data is fetched." },
  { key: "pages", title: "Pages", description: "Works only in this Space and with the starter's access. The room sees the draft card; only the starter can preview and apply it." },
  { key: "other", title: "Other tools", description: "Additional tools reported by this server." },
] as const;

function AgentToolGroups() {
  const tools = () => agentList(spaceSettingsFor())?.tools ?? [];
  return (
    <Column gap={3} testID="agent-tool-groups">
      <Label variant="heading" heading>Tools</Label>
      <Label variant="caption" color="muted">These permissions limit what the agent can use. They don’t add tools to a run.</Label>
      <Each of={TOOL_GROUPS} key={(g) => g.key}>
        {(group) => {
          const ids = () => tools().filter((id) => (toolInfo(id)?.group ?? "other") === group().key);
          return (
            <If when={ids().length}>
              <SettingsGroup title={group().title} description={group().description} testID={`agent-tools:${group().key}`}>
                <If when={group().key === "web"}>
                  <SettingRow label="Queries are visible to this Space" description="Members can see exact queries in run activity. Providers see queries or addresses; keys stay on the starter's computer." />
                </If>
                <Each of={ids()} key={(id) => id}>
                  {(id) => {
                    const tool = () => toolInfo(id());
                    const label = () => tool()?.label ?? id();
                    return (
                      <SettingRow label={tool() ? label() : "Allow tool"} description={tool()?.description}>
                        <If when={!tool()}><Label variant="mono">{id()}</Label></If>
                        <Toggle testID={`agent-tool:${id()}`} label={`Allow ${label()}`}
                          checked={!!agentDraft()?.tools.includes(id())} disabled={savingAgent()}
                          onChange={(on) => {
                            const draft = agentDraft();
                            const toolId = id();
                            if (!draft || !isSettingsOwner() || savingAgent() || !tools().includes(toolId)) return;
                            const kept = draft.tools.filter((x) => x !== toolId);
                            updateDraft({ tools: on ? [...kept, toolId] : kept });
                          }} />
                      </SettingRow>
                    );
                  }}
                </Each>
              </SettingsGroup>
            </If>
          );
        }}
      </Each>
    </Column>
  );
}

// ---------------------------------------------------------------- Your preferences

function PreferencesPage() {
  void loadPreferences();
  const sid = () => spaceSettingsFor() ?? "";
  const p = () => prefs();
  const fallbackDetail = () => (p()?.routing ? "Your model routing, if you turned it on, else your default chat connection" : "Your default chat connection");
  const spaceOptions = () => spaceModelOptions(catalog(), fallbackDetail());
  const agentOptions = () => spaceModelOptions(catalog(), "Your model for this Space, or the rule after it");
  return (
    <PageLayout testID="space-preferences" pad={pad()} gap={6} footer={spaceNotice() ? <Notice /> : undefined}>
      <PageHeader title="Your preferences" scope={{ kind: "space", personal: true }} subtitle="These choices apply to your own messages in this Space." />
      <If when={prefsError()}>
        {(m) => <Banner variant="inline" tone="danger" testID="prefs-error" message={m()} action={<Action label="Try again" onPress={() => void loadPreferences()} />} />}
      </If>
      <If
        when={p()}
        else={
          <If when={!prefsError()}>
            <Label color="muted" testID="prefs-loading">
              Loading your preferences…
            </Label>
          </If>
        }
      >
        {(v) => (
          <Column gap={6}>
            <SettingsGroup title="Agent pickup">
              <SettingRow
                testID="classify-row"
                label="Classify my unaddressed messages"
                description="System One can decide whether your unaddressed messages here are for an agent, and pick which one. Off by default." stacked
              >
                <Toggle
                  testID="classify-unaddressed"
                  label="Classify my unaddressed messages"
                  checked={v().classifyUnaddressed}
                  disabled={prefsBusy() !== null || (!v().classifierAvailable && !v().classifyUnaddressed)}
                  onChange={setClassifyUnaddressed}
                />
                <Label variant="caption" color="muted">
                  With this on, System One can also pick the agent for a generic @agent or Ask agent request. It never classifies messages to a person only or to a named agent. Never other people's messages. Personal Chat starts its agent directly, without classification.
                </Label>
              </SettingRow>
              <If when={!v().classifierAvailable}>
                <Row padX={4} padY={3} gap={3} align="center" testID="classifier-missing">
                  <Column grow>
                    <Label variant="caption" color="muted">
                      You need a System One connection you can use in this Space first. Without one, messages that mention nobody stay ordinary conversation.
                    </Label>
                  </Column>
                  <Action
                    compact
                    testID="open-model-providers"
                    label="Open Model connections"
                    onPress={() => {
                      closeSpaceSettings();
                      openSettings("providers");
                    }}
                  />
                </Row>
              </If>
            </SettingsGroup>

            <SettingsGroup title="Models" description="Which of your chat connections answers when you start an agent here. A model you pick for one turn always wins.">
              <If when={catalogError()}>
                {(m) => (
                  <Row padX={4} padY={3}>
                    <Label variant="caption" color="danger" testID="catalog-error">
                      {m()}
                    </Label>
                  </Row>
                )}
              </If>
              <SettingRow label="Default model in this Space" description="Used for every agent here unless you pick one for that agent below." stacked>
                <ModelPicker
                  testID="space-default-model"
                  rememberAs="space-prefs"
                  context={sid()}
                  accessibilityLabel="Your default model in this Space"
                  options={spaceOptions()}
                  value={shownValue(spaceOptions(), v().defaultModel)}
                  placeholder={v().defaultModel ? "Saved model not listed" : "No preference"}
                  disabled={prefsBusy() !== null || !catalog()}
                  onChange={(value) => setSpaceDefaultModel(selectionOf(value))}
                />
                <If when={v().defaultModel && effortsOf(catalog(), v().defaultModel).length ? v().defaultModel : null}>
                  {(sel) => (
                    <Column gap={1}>
                      <Label variant="caption" color="muted">
                        Thinking level
                      </Label>
                      <ThinkingChoice
                        testID="space-default-thinking"
                        options={thinkingOptions(effortsOf(catalog(), sel()))}
                        value={sel().reasoningEffort ?? ""}
                        onChange={(effort) => setSpaceDefaultModel(withEffort(sel(), effort))}
                      />
                    </Column>
                  )}
                </If>
                <If when={catalog() && unlisted(spaceOptions(), v().defaultModel)}>
                  <Label variant="caption" color="warning" testID="space-default-unlisted">
                    {v().defaultModel?.modelId
                      ? "Your saved model isn't one you can use here now. Turns that rely on it fail until you pick another."
                      : "Your saved choice uses that connection's default model."}
                  </Label>
                </If>
              </SettingRow>
              <SettingRow
                label="Use model routing in this Space"
                description="Let your use-case routing pick the answering model here when you haven't chosen one for the turn, agent or Space."
              >
                <Toggle testID="space-routing" label="Use model routing in this Space" checked={v().routing} disabled={prefsBusy() !== null} onChange={setSpaceRouting} />
              </SettingRow>
            </SettingsGroup>

            <SettingsGroup title="Per agent" description="Your model for one agent here; it overrides your default model in this Space.">
              <Each of={[...spaceAgents(sid())]} key={(a) => a.id}>
                {(a) => (
                  <SettingRow label={`${a().name} (@${a().handle})`} stacked testID={`agent-model-row:${a().id}`}>
                    <Row gap={3} align="center">
                      <SpaceAgentAvatar spaceId={sid()} agent={a()} size="sm" />
                      <Column grow>
                        <ModelPicker
                          testID={`agent-model:${a().id}`}
                          rememberAs="space-prefs"
                          context={sid()}
                          accessibilityLabel={`Your model for ${a().name}`}
                          options={agentOptions()}
                          value={shownValue(agentOptions(), v().agentModels[a().id])}
                          placeholder={v().agentModels[a().id] ? "Saved model not listed" : "No preference"}
                          disabled={prefsBusy() !== null || !catalog()}
                          onChange={(value) => setAgentModel(a().id, selectionOf(value))}
                        />
                        <If when={v().agentModels[a().id] && effortsOf(catalog(), v().agentModels[a().id]).length ? v().agentModels[a().id] : null}>
                          {(sel) => (
                            <Column gap={1} padY={1}>
                              <Label variant="caption" color="muted">
                                Thinking level
                              </Label>
                              <ThinkingChoice
                                testID={`agent-thinking:${a().id}`}
                                options={thinkingOptions(effortsOf(catalog(), sel()))}
                                value={sel().reasoningEffort ?? ""}
                                onChange={(effort) => setAgentModel(a().id, withEffort(sel(), effort))}
                              />
                            </Column>
                          )}
                        </If>
                      </Column>
                    </Row>
                  </SettingRow>
                )}
              </Each>
            </SettingsGroup>
          </Column>
        )}
      </If>
    </PageLayout>
  );
}
