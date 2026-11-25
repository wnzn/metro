import { announce, cleanup, computed, effect, lifecycle, openUrl, signal, untracked } from "nsh";
import type { InputRef, ViewRef } from "nsh";
import type { ActionKind, Confirmation, Page, PageData, PageDiagnostic } from "../../api/pages";
import { isPageStatus, pageFailure } from "../../api/pages";
import { goPages, hasSpace, pageGuard, pagesProblem, refreshPages, sameSpaceGrants, scopeVisible, spaceGrants } from "../../pagesState";
import type { PagesRoute } from "../../pagesState";
import { apiClient, me, openChat, openExactRoom, openPlace, sessionGuard } from "../../store";
import { Action, ActionBar, ActionsMenu, Banner, Column, compactWidth, Label, Modal, modalOpen, PageHeader, PageLayout, Row } from "../../ui";
import { httpsLink, PageEditPanel } from "../../ui/pageBlocks";
import { pageTaskPrefillIssue, prefillPageTask } from "../discovery/scheduleState";
import { openAsset } from "../library/libraryState";
import { profileName } from "../profile/profileState";
import { actionBody, checkedConfirmation } from "./actions";
import { answerState, linkedAnswerNames, terminalAnswer } from "./answerWatch";
import { Diagnostics, DraftPrompt, PagesError, scopeAudience, scopeName, showDraft } from "./controls";
import { PageRenderer } from "./PageRenderer";
import { PageHistory, PageSettings, SourceEditor } from "./PageTools";
import { capabilityOf, cell, MAX_DEPTH, otherApprovalButtons, own, str } from "./renderPlan";
import { intervalFor, pageReadLane, visiblePolling } from "./refresh";
import { answerScheduling } from "./scheduleAnswer";

type ViewRoute = Extract<PagesRoute, { kind: "page" }>;
type Sheet = "source" | "history" | "settings" | "delete" | null;
type AnswerWatch = { version: number; until: number; spaceId: string | null; roomId: string; runId: string;
  answers: { name: string; stamp?: string }[] };
export type PageToolProps = { page: Page; stillHere: () => boolean; close: () => void; saved: (page: Page) => void; lost: () => void };

export function PageView(props: { route: ViewRoute }) {
  const page = signal<Page | null>(null), data = signal<PageData | null>(null);
  const loading = signal(true), refreshing = signal(false), busy = signal(false);
  const error = signal(""), refreshError = signal(""), notice = signal("");
  const actionError = signal(""), actionNotice = signal("");
  const waiting = signal<AnswerWatch | null>(null);
  const diagnostics = signal<PageDiagnostic[]>([]);
  const sheet = signal<Sheet>(null), confirm = signal<Confirmation | null>(null), external = signal<string | null>(null);
  type ScheduleButton = NonNullable<NonNullable<ReturnType<typeof answerScheduling>>["button"]>;
  const schedule = signal<(ScheduleButton & { room: string; agentName: string }) | undefined>(undefined);
  const covered = computed(() => {
    const choice = confirm(), head = page(), displayed = data();
    if (!choice || !head || !displayed || head.head_version !== choice.version || displayed.version !== choice.version) return;
    return otherApprovalButtons(head.spec, displayed, choice.block_key);
  });
  const editing = signal(false), select = signal(false), block = signal<string | undefined>(undefined);
  const editVersion = signal<number | undefined>(undefined), reselect = signal(false);
  const blockName = signal("");
  let editTicket = 0;
  let promptField: InputRef | undefined, editControl: ViewRef | undefined;
  function closePrompt() {
    if (busy()) return;
    editTicket++;
    promptField = undefined;
    editing.set(false); select.set(false); block.set(undefined); reselect.set(false);
    editControl?.focus();
  }
  const client = apiClient(), route = props.route;
  const unavailableSpace = signal(false);
  const verifiedSpace = signal<string | null>(route.spaceId ?? null);
  const guard = pageGuard(route);
  const reads = pageReadLane<boolean>();
  const runReads = pageReadLane<AnswerWatch>();
  let alive = true;
  let scopeGuard = sessionGuard(route.spaceId);
  cleanup(() => { alive = false; });
  const valid = () => alive && guard() && !unavailableSpace() && scopeGuard() && hasSpace(verifiedSpace());
  const live = () => valid() && lifecycle() === "active";
  const visible = () => live() && !modalOpen();
  const cancelRead = () => { reads.cancel(); runReads.cancel(); refreshing.set(false); };
  function actionStatus(message: string) {
    actionNotice.set(message);
    if (message && live()) announce(message);
  }
  const lost = (space = false) => {
    if (space) unavailableSpace.set(true);
    cancelRead();
    page.set(null); data.set(null); confirm.set(null); external.set(null); schedule.set(undefined); sheet.set(null);
    refreshing.set(false); loading.set(false);
    refreshError.set("");
    notice.set(""); waiting.set(null); actionNotice.set(""); actionError.set("");
    editing.set(false); select.set(false); block.set(undefined); diagnostics.set([]);
    error.set(space ? "This Space is no longer available." : "This page is no longer available to you.");
    void refreshPages();
  };
  function accept(next: Page, before: ReturnType<typeof spaceGrants>) {
    const previous = page();
    if (next.id !== route.id || !scopeVisible(next) || next.deleted_at
      || (route.spaceId && next.space_id !== route.spaceId)
      || (verifiedSpace() && next.space_id !== verifiedSpace())
      || (next.space_id && !before.get(next.space_id)?.())) { lost(); return false; }
    const changed = previous && previous.head_version !== next.head_version;
    scopeGuard = sessionGuard(next.space_id ?? undefined);
    verifiedSpace.set(next.space_id);
    page.set(next);
    if (changed) {
      confirm.set(null); external.set(null); schedule.set(undefined); data.set(null);
      runReads.cancel(); waiting.set(null); actionNotice.set(""); actionError.set("");
      const selected = block();
      if (editing() && selected && !hasPageBlock(next.spec.blocks, selected)) {
        reselect.set(true); select.set(true);
      }
    }
    return true;
  }
  function refresh(loadHead = false, replace = false): Promise<void> {
    if (!client || !visible()) return Promise.resolve();
    const needsHead = loadHead || !page();
    const before = spaceGrants();
    return reads.run(needsHead, async (signal, current) => {
      const allowed = () => current() && visible() && [...before.values()].every(ok => ok());
      if (!allowed()) return;
      refreshing.set(true);
      try {
        if (needsHead) {
          const head = await client.getPage(route.id, signal);
          if (!allowed() || !accept(head, before)) return;
        }
        if (!allowed()) return;
        let answer = await client.pageData(route.id, signal);
        if (!allowed()) return;
        if (answer.version !== page()?.head_version) {
          data.set(null); confirm.set(null);
          const head = await client.getPage(route.id, signal);
          if (!allowed() || !accept(head, before) || !allowed()) return;
          answer = await client.pageData(route.id, signal);
        }
        if (!allowed()) return;
        if (answer.version !== page()?.head_version) {
          refreshError.set("The page changed while it was loading. Refresh to see the latest version.");
          data.set(null);
          return;
        }
        data.set(answer);
        refreshError.set("");
      } catch (e) {
        if (!allowed()) return;
        if (isPageStatus(e, 404) || isPageStatus(e, 403)) lost();
        else { data.set(null); confirm.set(null); refreshError.set(pagesProblem(e)); }
      } finally {
        if (allowed()) { loading.set(false); refreshing.set(false); }
      }
    }, existingHead => !replace && (!needsHead || existingHead));
  }
  // A personal page can reference other Spaces. Clear its data and consent
  // as soon as the list of accessible Spaces changes.
  let membership: ReturnType<typeof spaceGrants> | undefined, wasVisible = false;
  effect(() => {
    const now = spaceGrants();
    const sid = verifiedSpace();
    const spaceLost = !unavailableSpace() && !!sid && (!now.has(sid) || !scopeGuard());
    const shown = visible();
    const changed = membership !== undefined && !sameSpaceGrants(membership, now);
    const justShown = shown && !wasVisible;
    membership = now;
    wasVisible = shown;
    untracked(() => {
      if (spaceLost) { lost(true); return; }
      if (!shown || changed) cancelRead();
      if (changed) {
        data.set(null); confirm.set(null); external.set(null); schedule.set(undefined);
        waiting.set(null); actionNotice.set(""); actionError.set("");
      }
      if (shown && (justShown || changed)) void refresh(true);
    });
  });
  visiblePolling(() => visible() && !!waiting(), () => 1_500, () => { void pollAnswer(); });
  function pollAnswer() {
    const watch = waiting();
    if (!watch || !client || !visible()) return;
    if (Date.now() >= watch.until) {
      waiting.set(null);
      runReads.cancel();
      actionStatus(watch.spaceId ? "Request sent. Check the room for the run." : "Request sent. Refresh data to check for an answer.");
      return;
    }
    if (busy() || refreshing()) return;
    return runReads.run(watch, async (signal, current) => {
      if (!current() || !visible() || waiting() !== watch || page()?.head_version !== watch.version) return;
      const statusRead = async () => {
        if (!watch.spaceId || !watch.roomId || !watch.runId) return;
        try {
          const run = await client.getRun(watch.runId, signal);
          if (run.actor === me() && run.spaceId === watch.spaceId && run.roomId === watch.roomId) return run.status;
        } catch { /* Status may be transiently unavailable. Keep the bounded data refresh. */ }
      };
      const [status] = await Promise.all([statusRead(), refresh()]);
      if (!current() || !visible() || waiting() !== watch || page()?.head_version !== watch.version) return;
      if (status === "completed" || status === "failed" || status === "cancelled") {
        waiting.set(null);
        if (status === "completed") {
          // The run marker may arrive after the concurrent data read.
          actionStatus("Agent run finished in the room.");
          void refresh();
        } else {
          actionNotice.set("");
          actionError.set(status === "failed" ? "The agent run failed. See the room for activity." : "The agent run was cancelled.");
        }
      } else if (!watch.spaceId && watch.answers.some(({ name, stamp }) => {
        const latest = answerState(data(), name);
        return latest && latest.stamp !== stamp && terminalAnswer(latest.status);
      })) {
        waiting.set(null);
        actionStatus("Page answer updated.");
      }
    });
  }
  visiblePolling(() => visible() && !!page(), () => intervalFor(page()?.spec?.refresh), () => {
    if (!busy() && !refreshing()) void refresh();
  });

  const saved = (next: Page) => {
    if (!valid() || !accept(next, spaceGrants())) return;
    sheet.set(null); notice.set("Page saved.");
    void refresh(false, true);
    void refreshPages();
  };
  const toolProps = (head: Page): PageToolProps => ({ page: head, stillHere: valid, close: () => sheet.set(null), saved, lost: () => lost() });
  async function run(key: string, kind: ActionKind, consent?: Confirmation) {
    const head = page(), c = client, displayed = data();
    if (!head || !c || !live() || busy() || displayed?.version !== head.head_version || (kind === "runAgent" && waiting())) return;
    if (consent && (confirm() !== consent || !checkedConfirmation(consent, head, key, kind, me()))) return;
    const capability = capabilityOf(displayed, key);
    if (capability?.enabled !== true) return;
    if (kind === "refresh") { void refresh(true); return; }
    busy.set(true); actionError.set(""); notice.set("");
    if (!waiting()) actionNotice.set("");
    try {
      if (kind === "openRoom") {
        if (capability.chat === true && head.scope === "personal" && !capability.space_id && !capability.room_id) { openChat(); return; }
        const sid = str(capability.space_id), rid = str(capability.room_id);
        if (!sid || !rid || (head.space_id && head.space_id !== sid)
          || !(await openExactRoom(sid, rid, () => live() && page() === head && data() === displayed))) {
          if (valid()) actionError.set("That room is no longer available.");
        }
        return;
      }
      if (kind === "openPage") {
        const id = str(capability.page_id);
        if (id) goPages({ kind: "page", id });
        return;
      }
      // If asking for consent, /data can be older than the challenge. On
      // confirmation send the *reviewed* revision, never a replacement.
      const sr = consent?.action.kind === "runSchedule" ? consent.action.schedule_revision : capability.schedule_revision;
      const body = actionBody(head, key, kind, sr, consent);
      if (!body) { actionError.set("The action could not be checked. Refresh the page and try again."); return; }
      if (consent) confirm.set(null);
      const outcome = await c.pageAction(head.id, body);
      if (!valid() || page()?.head_version !== head.head_version) return;
      if (outcome.kind === "runAgent") {
        if (outcome.started) {
          waiting.set({ version: head.head_version, until: Date.now() + 45_000, spaceId: head.space_id,
            roomId: str(outcome.room_id), runId: str(outcome.run_id),
            answers: linkedAnswerNames(head.spec, key).map(name => ({ name, stamp: answerState(displayed, name)?.stamp })) });
          actionStatus(head.space_id ? "Agent started in the room. Waiting for the run…" : "Agent started. Waiting for a page answer…");
        } else actionError.set(str(outcome.error) || "The agent did not start.");
      } else if (outcome.run.status === "failed" || outcome.run.status === "unknown") {
        actionError.set(str(outcome.run.detail) || "The scheduled task did not finish.");
      } else actionStatus("Scheduled task started.");
      await refresh(false, true);
    } catch (e) {
      if (!valid()) return;
      confirm.set(null);
      if (isPageStatus(e, 428)) {
        const check = checkedConfirmation(pageFailure(e).confirmation, head, key, kind, me());
        if (check && live() && page() === head && data() === displayed) confirm.set(check);
        else actionError.set("Metro could not verify what this action would do. Refresh the page and try again.");
      } else if (isPageStatus(e, 409)) {
        await refresh(true, true);
        if (valid() && page()) actionError.set("The page or scheduled task changed. Review it and click again; no action ran.");
      } else {
        const problem = pagesProblem(e);
        if (isPageStatus(e, 404) || isPageStatus(e, 403)) await refresh(true);
        if (valid() && page()) actionError.set(problem);
      }
    } finally { if (valid()) busy.set(false); }
  }
  async function file(id: string) {
    const head = page();
    if (!client || !head || !live() || busy()) return;
    const before = spaceGrants();
    try {
      const asset = await client.getAsset(id);
      if (!live() || page() !== head || asset.id !== id || (head.space_id && head.space_id !== asset.spaceId)
        || (asset.spaceId && (!before.get(asset.spaceId)?.() || !hasSpace(asset.spaceId)))) return;
      openPlace("library");
      openAsset(asset);
    } catch (e) { if (valid()) error.set(pagesProblem(e)); }
  }
  async function prepareSchedule(block: string) {
    const head = page(), sid = head?.space_id;
    if (!client || !head || !sid || busy() || !live()) return;
    const chosen = answerScheduling(head.spec, block)?.button;
    if (!chosen) return;
    const capability = capabilityOf(data(), chosen.key);
    actionError.set("");
    const issue = pageTaskPrefillIssue(sid);
    if (issue) { actionError.set(issue); return; }
    if (capability?.enabled !== true) {
      actionError.set(capability?.reason || "Scheduling is unavailable until the page data and destination can be checked.");
      return;
    }
    busy.set(true);
    try {
      const [agents, rooms, latest] = await Promise.all([client.spaceAgents(sid), client.listRooms(sid), client.getPage(head.id)]);
      if (!live() || page()?.head_version !== head.head_version) return;
      const agent = agents.agents.find(a => a.id === chosen.agent && a.isDefault);
      const room = rooms.find(r => r.id === chosen.roomId && !r.parentRoomId);
      if (latest.id !== head.id || latest.space_id !== sid || latest.head_version !== head.head_version) {
        await refresh(true);
        if (valid()) actionError.set("The page changed. Review it before setting up a scheduled task.");
      } else if (!agent || !room) {
        actionError.set(!room ? "Scheduling needs a top-level room that you can access." : "Scheduling uses the Space’s default agent. This answer follows a different agent.");
      } else schedule.set({ ...chosen, room: room.name, agentName: agent.name });
    } catch (e) { if (valid()) actionError.set(pagesProblem(e)); }
    finally { if (valid()) busy.set(false); }
  }
  async function remove() {
    const head = page();
    if (!client || !head?.can_delete || !live() || sheet() !== "delete" || busy()) return;
    busy.set(true);
    try {
      await client.deletePage(head.id);
      if (valid()) { goPages({ kind: "home", spaceId: head.space_id ?? undefined, afterDelete: true }); void refreshPages(); }
    } catch (e) { if (valid()) error.set(pagesProblem(e)); }
    finally { if (valid()) { busy.set(false); sheet.set(null); } }
  }
  function prepareGenerate() {
    const head = page();
    const selected = block(), ticket = editTicket;
    return async (prompt: string, choice?: import("../../api/client").TurnModelChoice) => {
      if (!client || !head?.can_edit || !live() || busy() || !editing()) return;
      if (ticket !== editTicket || reselect() || selected !== block() || page()?.head_version !== head.head_version || !page()?.can_edit) {
        error.set("The edit target changed. Review the selection and generate again.");
        return;
      }
      busy.set(true); error.set(""); diagnostics.set([]);
      try {
        const draft = await client.createPageDraft({ page_id: head.id, block_key: selected }, prompt, choice);
        if (valid() && scopeVisible(draft) && draft.page_id === head.id && draft.space_id === head.space_id) showDraft(draft, choice);
      } catch (e) { if (valid()) { error.set(pagesProblem(e)); diagnostics.set(pageFailure(e).diagnostics ?? []); } }
      finally { if (valid()) busy.set(false); }
    };
  }
  function openPrompt() {
    if (!editing()) editVersion.set(page()?.head_version);
    editing.set(true);
  }
  const shownPage = () => valid() ? page() : null;
  return <PageLayout inset={6} gap={4} testID="page-view">
    <Column gap={2}>
      <Row gap={2} wrap>
        <Action compact label={unavailableSpace() ? "‹ All pages" : "‹ Pages"} testID="page-back" variant="ghost" onPress={() => {
          const sid = verifiedSpace() ?? undefined;
          goPages({ kind: "home", spaceId: !unavailableSpace() && hasSpace(sid) ? sid : undefined });
        }} />
      </Row>
      <PagesError message={refreshError() || error()} onRetry={unavailableSpace() ? undefined : () => { error.set(""); void refresh(true); }} />
      <PagesError message={actionError()} testID="page-action-error" />
      <If when={actionNotice()}>{message => <Label testID="page-action-status">{message()}</Label>}</If>
      <If when={notice()}>{message => <Banner tone="success" variant="inline" message={message()} />}</If>
      <If when={loading() && !page()}><Label color="muted">Loading page…</Label></If>
    </Column>
    <If when={shownPage()}>{head => <>
      <Column gap={2}>
        <PageHeader title={[head().icon, head().title].filter(Boolean).join(" ")} subtitle={scopeAudience(head().space_id)} />
        <Label variant="meta">{`Version ${head().head_version} · Edited by ${profileName(head().version.author_id) ?? head().version.author_id} · ${
          refreshing() ? "Refreshing data…" : data() ? `Data updated ${cell(data()?.resolved_at, "relative").label.toLowerCase()}` : "Data not loaded"}`}</Label>
        <Row gap={2} align="center" wrap>
          <If when={head().can_edit || editing()}>
            <Action compact label={editing() ? "Close edit prompt" : "Edit with prompt"} testID="page-edit" disabled={busy()}
              viewRef={node => { editControl = node; }} onPress={() => {
                if (!editing()) openPrompt();
                else closePrompt();
              }} />
          </If>
          <If when={head().can_edit}>
            <Action compact label={select() ? "Stop selecting" : "Select a block"} testID="page-select-block" variant="ghost" onPress={() => {
              select.set(!select()); if (select()) openPrompt();
            }} />
          </If>
          <Action compact label="Refresh data" testID="page-refresh" variant="ghost" disabled={refreshing() || busy()} onPress={() => void refresh(true)} />
          <ActionsMenu testID="page-menu" label="Page options" disabled={busy()} items={[
            { id: "page-pin", label: head().pinned ? "Unpin" : "Pin", onPress: () => {
              const current = head();
              if (!client || !live() || busy()) return;
              busy.set(true);
              void client.pinPage(current.id, !current.pinned).then(prefs => {
                if (valid() && page() === current) page.set({ ...current, ...prefs });
                if (valid()) void refreshPages();
              }, e => { if (valid()) error.set(pagesProblem(e)); }).finally(() => { if (valid()) busy.set(false); });
            } },
            ...(head().can_edit ? [{ id: "page-source", label: "Edit source", onPress: () => sheet.set("source") }] : []),
            { id: "page-history", label: "History", onPress: () => sheet.set("history") },
            ...(head().can_edit || head().can_manage ? [{ id: "page-settings", label: "Page settings", onPress: () => sheet.set("settings") }] : []),
            ...(head().can_delete ? [{ id: "page-delete", label: "Delete…", destructive: true, onPress: () => sheet.set("delete") }] : []),
          ]} />
        </Row>
      </Column>
      <If when={editing()}>
        <PageEditPanel testID="page-edit-prompt">
          <Row gap={2} align="center" wrap>
            <Label heading variant="strong">{block() ? `Editing ${blockName()}` : "Edit page with a prompt"}</Label>
            <If when={block() && !reselect()}><Action compact label="Edit whole page" variant="ghost" onPress={() => {
              editTicket++; block.set(undefined); promptField?.focus();
            }} /></If>
          </Row>
          <If when={select() && !block()}><Label variant="meta">Choose an Edit block… button below to target part of the page.</Label></If>
          <If when={editVersion() !== head().head_version}>
            <Banner tone="warning" variant="inline" title="New version published" message={reselect()
              ? "Your prompt is still here. The selected block is gone. Select a block to continue."
              : `Your prompt is still here. The next draft will use version ${head().head_version}.`} />
          </If>
          <If when={!head().can_edit}><Label color="warning">You can no longer edit this page. You can still copy your prompt.</Label></If>
          <DraftPrompt spaceId={head().space_id} label={block() ? `Describe what to change in ${blockName()}` : "Describe what to change"} busy={busy()} blocked={reselect() || !head().can_edit}
            compact focus inputRef={node => { promptField = node; }} onCancel={closePrompt} prepareSubmit={prepareGenerate} />
          <Diagnostics items={diagnostics()} />
        </PageEditPanel>
      </If>
      <PageRenderer spec={head().spec} data={data()?.version === head().head_version ? data() : null} pending={refreshing()} busy={busy()} onAction={(key, kind) => void run(key, kind)}
        waitingForAgent={!!waiting()}
        onLink={url => { if (live()) external.set(httpsLink(url)); }} onFile={id => void file(id)}
        scheduling={key => {
          const match = head().space_id ? answerScheduling(head().spec, key) : undefined;
          return match ? match.button ? "available" : "no-prompt" : undefined;
        }}
        onSchedule={key => void prepareSchedule(key)}
        onEdit={head().can_edit && select() ? (key, label) => {
          if (busy()) return;
          editTicket++; block.set(key); blockName.set(label); reselect.set(false); openPrompt();
          promptField?.reveal({ vertical: "center", animated: false });
          promptField?.focus();
        } : undefined} />
      <If when={sheet() === "source"}><SourceEditor {...toolProps(head())} /></If>
      <If when={sheet() === "history"}><PageHistory {...toolProps(head())} /></If>
      <If when={sheet() === "settings"}><PageSettings {...toolProps(head())} /></If>
      <If when={sheet() === "delete"}><Modal open size="compact" title="Delete page" testID="page-delete-confirm" onClose={() => { if (!busy()) sheet.set(null); }}>
        <Column pad={4} gap={3}>
          <Label heading variant="title">Delete {head().title}?</Label>
          <Label>{head().space_id ? "This removes the page for everyone in this Space." : "This removes the page from your list."} You can restore it for 30 days.</Label>
          <ActionBar><Action label="Cancel" variant="ghost" disabled={busy()} onPress={() => sheet.set(null)} />
            <Action testID="page-delete-now" label="Delete page" variant="danger" busy={busy()} onPress={() => void remove()} /></ActionBar>
        </Column>
      </Modal></If>
    </>}</If>
    <If when={confirm()}>{choice => <Modal open title="Approve page version" testID="page-action-confirm" onClose={() => confirm.set(null)}>
      <PageLayout gap={compactWidth() ? 2 : 4} inset={compactWidth() ? 4 : 6} testID="page-action-review" footer={
        <ActionBar><Action label="Cancel" testID="page-action-cancel" variant="ghost" onPress={() => confirm.set(null)} />
          <Action label="Approve version and run" testID="page-action-run" variant="primary" busy={busy()} onPress={() => void run(choice().block_key, choice().action.kind, choice())} /></ActionBar>
      }>
        <PageHeader title={`Approve version ${choice().version} and ${choice().action.kind === "runAgent" ? "run the agent?" : "run the scheduled task?"}`} />
        <Label variant="strong">{`Approves every agent and schedule button in version ${choice().version}. Only this button runs now.`}</Label>
        <Column gap={1}>
          <Label variant="strong">{destination(choice())}</Label>
          <Label variant="caption">Runs as you, with your model settings.</Label>
          <If when={taskRevision(choice().action)}>{revision => <Label variant="meta">Task revision {revision()}</Label>}</If>
        </Column>
        <If when={covered()?.labels.length ? covered() : undefined}>{buttons =>
          <Column gap={1} testID="page-action-covered">
            <Label variant="caption">Other available buttons this approval covers</Label>
            <Each of={buttons().labels} key={button => button.key}>
              {button => <Label lines={compactWidth() ? 1 : 2} variant="caption">• {button().label}</Label>}
            </Each>
            <If when={buttons().more > 0}><Label variant="caption">{`And ${buttons().more} more`}</Label></If>
          </Column>
        }</If>
        <Label variant="caption">Other buttons still need a click. Permission and schedule checks still apply.</Label>
        <Label variant="meta">Last edited by {profileName(choice().edited_by) ?? choice().edited_by} · {cell(choice().edited_at, "datetime").label}</Label>
        <Column selectable><Label>{choice().action.prompt}</Label></Column>
      </PageLayout>
    </Modal>}</If>
    <If when={schedule()}>{task => <Modal open size="compact" title="Schedule brief" onClose={() => schedule.set(undefined)}>
      <Column pad={4} gap={3}>
        <Label heading variant="title">Schedule this brief?</Label>
        <Label variant="strong">{`${scopeName(page()?.space_id)} › ${task().room} · ${task().agentName} (default agent)`}</Label>
        <Label>The task runs as you. This block shows the latest room answer from that agent, including scheduled answers. Saving the task starts the schedule; opening the form does not.</Label>
        <ActionBar><Action label="Cancel" variant="ghost" onPress={() => schedule.set(undefined)} />
          <Action label="Open task form" variant="primary" testID="page-open-schedule-form" onPress={() => {
            const chosen = task(), sid = page()?.space_id;
            schedule.set(undefined);
            if (!sid || !live()) return;
            if (prefillPageTask(sid, chosen.roomId, chosen.prompt)) openPlace("schedules");
            else actionError.set(pageTaskPrefillIssue(sid) ?? "Could not open the scheduled task form.");
          }} /></ActionBar>
      </Column>
    </Modal>}</If>
    <If when={external()}>{url => <Modal open size="compact" title="Open external link" onClose={() => external.set(null)}>
      <Column pad={4} gap={3}>
        <Label heading variant="title">Open in your browser?</Label>
        <Label>{url()}</Label>
        <ActionBar><Action label="Cancel" variant="ghost" onPress={() => external.set(null)} />
          <Action label="Open link" variant="primary" onPress={() => {
            const safe = httpsLink(url());
            external.set(null);
            if (live() && safe) void openUrl(safe).then(result => {
              if (valid() && !result.opened) error.set("Could not open the link in your browser.");
            }, () => { if (valid()) error.set("Could not open the link in your browser."); });
          }} /></ActionBar>
      </Column>
    </Modal>}</If>
  </PageLayout>;
}

function hasPageBlock(blocks: unknown, key: string) {
  let remaining = 600; // The Page TSX element limit.
  const find = (items: unknown, depth: number): boolean => {
    if (!Array.isArray(items) || depth > MAX_DEPTH) return false;
    for (const item of items) {
      if (--remaining < 0) break;
      if (own(item, "key") === key || find(own(item, "children"), depth + 1)) return true;
    }
    return false;
  };
  return find(blocks, 0);
}

function destination(confirmation: Confirmation) {
  const action = confirmation.action;
  if (action.kind === "runSchedule") return `${action.schedule} · Space default agent in ${scopeName(action.space_id)} › ${action.room}`;
  if (action.chat) return "Your private page agent · answer stays on this page";
  const handle = action.agent ? `@${action.agent}` : "Agent";
  const name = str(action.agent_name);
  return `${name ? `${name} (${handle})` : handle} in ${scopeName(action.space_id)} › ${action.room ?? "Room"}`;
}

function taskRevision(action: Confirmation["action"]) {
  return action.kind === "runSchedule" ? action.schedule_revision : undefined;
}
