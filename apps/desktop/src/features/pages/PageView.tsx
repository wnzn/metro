import { cleanup, effect, lifecycle, openUrl, signal, untracked } from "nsh";
import type { ActionKind, Confirmation, Page, PageData, PageDiagnostic } from "../../api/pages";
import { isPageStatus, pageFailure } from "../../api/pages";
import { goPages, hasSpace, pageGuard, pagesProblem, refreshPages, scopeVisible } from "../../pagesState";
import type { PagesRoute } from "../../pagesState";
import { apiClient, me, openChat, openExactRoom, openPlace, sessionGuard, spaces } from "../../store";
import { Action, ActionBar, ActionsMenu, Badge, Banner, Column, Label, Modal, modalOpen, PageHeader, PageLayout, Row } from "../../ui";
import { httpsLink } from "../../ui/pageBlocks";
import { prefillPageTask } from "../discovery/scheduleState";
import { openAsset } from "../library/libraryState";
import { profileName } from "../profile/profileState";
import { actionBody, checkedConfirmation } from "./actions";
import { Diagnostics, DraftPrompt, PagesError, scopeName, showDraft } from "./controls";
import { PageRenderer } from "./PageRenderer";
import { PageHistory, PageSettings, SourceEditor } from "./PageTools";
import { capabilityOf, cell, own, str } from "./renderPlan";
import { intervalFor, visiblePolling } from "./refresh";
import { answerSchedule } from "./scheduleAnswer";

type ViewRoute = Extract<PagesRoute, { kind: "page" }>;
type Sheet = "source" | "history" | "settings" | "delete" | null;
export type PageToolProps = { page: Page; stillHere: () => boolean; close: () => void; saved: (page: Page) => void; lost: () => void };

export function PageView(props: { route: ViewRoute }) {
  const page = signal<Page | null>(null), data = signal<PageData | null>(null);
  const loading = signal(true), refreshing = signal(false), busy = signal(false);
  const error = signal(""), notice = signal("");
  const diagnostics = signal<PageDiagnostic[]>([]);
  const sheet = signal<Sheet>(null), confirm = signal<Confirmation | null>(null), external = signal<string | null>(null);
  const schedule = signal<ReturnType<typeof answerSchedule>>(undefined);
  const editing = signal(false), select = signal(false), block = signal<string | undefined>(undefined);
  const client = apiClient(), route = props.route;
  const guard = pageGuard(route);
  let alive = true, ticket = 0;
  let scopeGuard = sessionGuard(route.spaceId);
  cleanup(() => { alive = false; });
  const valid = () => alive && guard() && scopeGuard() && hasSpace(page()?.space_id);
  const live = () => valid() && lifecycle() === "active";
  const grants = () => new Map(spaces().map(s => [s.id, sessionGuard(s.id)]));
  const lost = () => {
    ++ticket;
    page.set(null); data.set(null); confirm.set(null); external.set(null); schedule.set(undefined); sheet.set(null);
    refreshing.set(false); loading.set(false);
    error.set("This page is no longer available to you.");
    void refreshPages();
  };
  function accept(next: Page, before: ReturnType<typeof grants>) {
    const previous = page();
    if (next.id !== route.id || !scopeVisible(next) || next.deleted_at
      || (route.spaceId && next.space_id !== route.spaceId)
      || (previous && next.space_id !== previous.space_id)
      || (next.space_id && !before.get(next.space_id)?.())) { lost(); return false; }
    const changed = previous && previous.head_version !== next.head_version;
    scopeGuard = sessionGuard(next.space_id ?? undefined);
    page.set(next);
    if (changed) {
      confirm.set(null); external.set(null); schedule.set(undefined); data.set(null);
      editing.set(false); select.set(false); block.set(undefined);
    }
    return true;
  }
  async function refresh(loadHead = false) {
    if (!client || !valid()) return;
    const index = ++ticket, before = grants();
    const allowed = () => valid() && index === ticket && [...before.values()].every(ok => ok());
    refreshing.set(true);
    error.set("");
    try {
      if (loadHead || !page()) {
        const head = await client.getPage(route.id);
        if (!allowed() || !accept(head, before)) return;
      }
      let answer = await client.pageData(route.id);
      if (!allowed()) return;
      if (answer.version !== page()?.head_version) {
        data.set(null); confirm.set(null);
        const head = await client.getPage(route.id);
        if (!allowed() || !accept(head, before)) return;
        answer = await client.pageData(route.id);
      }
      if (!allowed()) return;
      if (answer.version !== page()?.head_version) {
        error.set("The page changed while it was loading. Refresh to see the latest version.");
        data.set(null);
        return;
      }
      data.set(answer);
    } catch (e) {
      if (!allowed()) return;
      if (isPageStatus(e, 404) || isPageStatus(e, 403)) lost();
      else { data.set(null); confirm.set(null); error.set(pagesProblem(e)); }
    } finally {
      if (valid() && index === ticket) { loading.set(false); refreshing.set(false); }
    }
  }
  // A personal page can reference other Spaces. Clear its data and consent
  // as soon as the list of accessible Spaces changes.
  let membership: string | undefined, wasVisible = false, recheck = true;
  effect(() => {
    const now = JSON.stringify(spaces().map(s => s.id).sort());
    const visible = live() && !modalOpen();
    const changed = membership !== undefined && now !== membership;
    membership = now;
    untracked(() => {
      if (changed) { ++ticket; data.set(null); confirm.set(null); external.set(null); schedule.set(undefined); refreshing.set(false); recheck = true; }
      if (visible && (!wasVisible || recheck)) { recheck = false; void refresh(true); }
      wasVisible = visible;
    });
  });
  visiblePolling(() => live() && !modalOpen() && !!page(), () => intervalFor(page()?.spec?.refresh), () => {
    if (!busy() && !refreshing()) void refresh();
  });

  const saved = (next: Page) => {
    if (!valid() || !accept(next, grants())) return;
    sheet.set(null); notice.set("Page saved.");
    void refresh();
    void refreshPages();
  };
  const toolProps = (head: Page): PageToolProps => ({ page: head, stillHere: valid, close: () => sheet.set(null), saved, lost });
  async function run(key: string, kind: ActionKind, consent?: Confirmation) {
    const head = page(), c = client, displayed = data();
    if (!head || !c || !live() || busy() || displayed?.version !== head.head_version) return;
    if (consent && (confirm() !== consent || !checkedConfirmation(consent, head, key, kind, me()))) return;
    const capability = capabilityOf(displayed, key);
    if (capability?.enabled !== true) return;
    if (kind === "refresh") { void refresh(true); return; }
    busy.set(true); error.set(""); notice.set("");
    try {
      if (kind === "openRoom") {
        if (capability.chat === true && head.scope === "personal" && !capability.space_id && !capability.room_id) { openChat(); return; }
        const sid = str(capability.space_id), rid = str(capability.room_id);
        if (!sid || !rid || (head.space_id && head.space_id !== sid)
          || !(await openExactRoom(sid, rid, () => live() && page() === head && data() === displayed))) {
          if (valid()) error.set("That room is no longer available.");
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
      if (!body) { error.set("The action could not be checked. Refresh the page and try again."); return; }
      if (consent) confirm.set(null);
      const outcome = await c.pageAction(head.id, body);
      if (!valid()) return;
      if (outcome.kind === "runAgent") {
        if (outcome.started) notice.set(head.scope === "personal" ? "Agent started. Its answer will stay on this page." : "Agent started in the Space room.");
        else error.set(str(outcome.error) || "The agent did not start.");
      } else if (outcome.run.status === "failed" || outcome.run.status === "unknown") {
        error.set(str(outcome.run.detail) || "The scheduled task did not finish.");
      } else notice.set("Scheduled task started.");
      // Preserve an action failure while refreshing its answer.
      const failure = error();
      await refresh();
      if (valid() && failure && !error()) error.set(failure);
    } catch (e) {
      if (!valid()) return;
      confirm.set(null);
      if (isPageStatus(e, 428)) {
        const check = checkedConfirmation(pageFailure(e).confirmation, head, key, kind, me());
        if (check && live() && page() === head && data() === displayed) confirm.set(check);
        else error.set("Metro could not verify what this action would do. Refresh the page and try again.");
      } else if (isPageStatus(e, 409)) {
        await refresh(true);
        if (valid() && page()) error.set("The page or scheduled task changed. Review it and click again; no action ran.");
      } else {
        const problem = pagesProblem(e);
        if (isPageStatus(e, 404) || isPageStatus(e, 403)) await refresh(true);
        if (valid() && page()) error.set(problem);
      }
    } finally { if (valid()) busy.set(false); }
  }
  async function file(id: string) {
    const head = page();
    if (!client || !head || !live() || busy()) return;
    const before = grants();
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
    const chosen = answerSchedule(head.spec, block);
    if (!chosen || capabilityOf(data(), chosen.key)?.enabled !== true) return;
    busy.set(true); error.set("");
    try {
      const [agents, rooms, latest] = await Promise.all([client.spaceAgents(sid), client.listRooms(sid), client.getPage(head.id)]);
      if (!live() || page() !== head) return;
      if (latest.id !== head.id || latest.space_id !== sid || latest.head_version !== head.head_version) {
        await refresh(true);
        if (valid()) error.set("The page changed. Review it before setting up a scheduled task.");
      } else if (!agents.agents.some(a => a.id === chosen.agent && a.isDefault) || !rooms.some(r => r.id === chosen.roomId && !r.parentRoomId)) {
        error.set("Scheduled prompts use the Space’s default agent and a top-level room. This answer uses a different destination.");
      } else schedule.set(chosen);
    } catch (e) { if (valid()) error.set(pagesProblem(e)); }
    finally { if (valid()) busy.set(false); }
  }
  async function remove() {
    const head = page();
    if (!client || !head?.can_delete || !live() || sheet() !== "delete" || busy()) return;
    busy.set(true);
    try {
      await client.deletePage(head.id);
      if (valid()) { goPages({ kind: "home", spaceId: head.space_id ?? undefined }); void refreshPages(); }
    } catch (e) { if (valid()) error.set(pagesProblem(e)); }
    finally { if (valid()) { busy.set(false); sheet.set(null); } }
  }
  async function generate(prompt: string, choice?: import("../../api/client").TurnModelChoice) {
    const head = page();
    if (!client || !head?.can_edit || !live() || busy()) return;
    busy.set(true); error.set(""); diagnostics.set([]);
    try {
      const draft = await client.createPageDraft({ page_id: head.id, block_key: block() }, prompt, choice);
      if (valid() && scopeVisible(draft) && draft.page_id === head.id && draft.space_id === head.space_id) showDraft(draft);
    } catch (e) { if (valid()) { error.set(pagesProblem(e)); diagnostics.set(pageFailure(e).diagnostics ?? []); } }
    finally { if (valid()) busy.set(false); }
  }
  return <PageLayout wide gap={3} testID="page-view">
    <Row gap={2} wrap>
      <Action compact label="‹ Pages" testID="page-back" variant="ghost" onPress={() => goPages({ kind: "home", spaceId: page()?.space_id ?? route.spaceId })} />
    </Row>
    <PagesError message={error()} onRetry={() => void refresh(true)} />
    <If when={notice()}>{message => <Banner tone="success" variant="inline" message={message()} />}</If>
    <If when={loading() && !page()}><Label color="muted">Loading page…</Label></If>
    <If when={valid() ? page() : null}>{head => <>
      <PageHeader title={[head().icon, head().title].filter(Boolean).join(" ")} subtitle={`Version ${head().head_version} · Edited by ${profileName(head().version.author_id) ?? head().version.author_id}`}>
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
      </PageHeader>
      <Row gap={2} align="center" wrap>
        <Badge tone="system" label={scopeName(head().space_id)} />
        <Label variant="meta">{refreshing() ? "Refreshing…" : data() ? `Updated ${cell(data()?.resolved_at, "relative").label.toLowerCase()}` : "Data not loaded"}</Label>
        <Action compact label="Refresh" testID="page-refresh" disabled={refreshing() || busy()} onPress={() => void refresh(true)} />
        <If when={head().can_edit}>
          <Action compact label={editing() ? "Close edit prompt" : "Edit with prompt"} testID="page-edit" onPress={() => {
            editing.set(!editing()); if (!editing()) { select.set(false); block.set(undefined); }
          }} />
          <Action compact label={select() ? "Stop selecting" : "Select a block"} testID="page-select-block" variant="ghost" onPress={() => {
            select.set(!select()); if (select()) editing.set(true);
          }} />
        </If>
      </Row>
      <PageRenderer spec={head().spec} data={data()?.version === head().head_version ? data() : null} pending={refreshing()} busy={busy()} onAction={(key, kind) => void run(key, kind)}
        onLink={url => { if (live()) external.set(httpsLink(url)); }} onFile={id => void file(id)}
        canSchedule={key => !!head().space_id && !!answerSchedule(head().spec, key)} onSchedule={key => void prepareSchedule(key)}
        onEdit={head().can_edit && select() ? key => { block.set(key); editing.set(true); } : undefined} />
      <If when={editing() && head().can_edit}>
        <Column gap={2} testID="page-edit-prompt">
          <Row gap={2} align="center" wrap>
            <Label variant="strong">{block() ? `Editing block: ${block()}` : "Edit page with a prompt"}</Label>
            <If when={block()}><Action compact label="Edit whole page" variant="ghost" onPress={() => block.set(undefined)} /></If>
          </Row>
          <DraftPrompt spaceId={head().space_id} label="Describe what to change" busy={busy()} onSubmit={(prompt, choice) => void generate(prompt, choice)} />
          <Diagnostics items={diagnostics()} />
        </Column>
      </If>
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
    <If when={confirm()}>{choice => <Modal open title="Confirm page action" testID="page-action-confirm" onClose={() => confirm.set(null)}>
      <PageLayout gap={3}>
        <PageHeader title={choice().action.kind === "runAgent" ? "Run agent?" : "Run scheduled task now?"} />
        <Label>Runs as you, with your model settings.</Label>
        <Label variant="meta">Last edited by {profileName(choice().edited_by) ?? choice().edited_by} · {cell(choice().edited_at, "datetime").label}</Label>
        <Label variant="strong">{destination(choice())}</Label>
        <Column selectable><Label>{choice().action.prompt}</Label></Column>
        <Label variant="caption" color="muted">Metro remembers this confirmation for this page version.</Label>
        <ActionBar><Action label="Cancel" testID="page-action-cancel" variant="ghost" onPress={() => confirm.set(null)} />
          <Action label="Run now" testID="page-action-run" variant="primary" busy={busy()} onPress={() => void run(choice().block_key, choice().action.kind, choice())} /></ActionBar>
      </PageLayout>
    </Modal>}</If>
    <If when={schedule()}>{task => <Modal open size="compact" title="Set up scheduled refresh" onClose={() => schedule.set(undefined)}>
      <Column pad={4} gap={3}>
        <Label heading variant="title">Set up a scheduled prompt?</Label>
        <Label>The scheduled task runs as you and posts to this Space room. It won’t run until you save the task.</Label>
        <If when={task().linked}><Label color="warning">This block shows only runs from its page button. To show scheduled answers here, remove the data source’s block link in the Page TSX.</Label></If>
        <ActionBar><Action label="Cancel" variant="ghost" onPress={() => schedule.set(undefined)} />
          <Action label="Open task form" variant="primary" testID="page-open-schedule-form" onPress={() => {
            const chosen = task(), sid = page()?.space_id;
            schedule.set(undefined);
            if (!sid || !live()) return;
            if (prefillPageTask(sid, chosen.roomId, chosen.prompt)) openPlace("schedules");
            else error.set("Close any open scheduled task draft or free a task slot first.");
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

function destination(confirmation: Confirmation) {
  const action = confirmation.action;
  if (action.kind === "runSchedule") return action.schedule;
  if (action.chat) return "Your private page agent · answer stays on this page";
  return `${action.agent ? `@${action.agent}` : "Agent"} in ${scopeName(action.space_id)} › ${action.room ?? "Room"}`;
}
