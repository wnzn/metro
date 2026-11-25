import { cleanup, effect, signal, untracked } from "nsh";
import type { DraftMeta, PageMeta, PageTarget, Template } from "../../api/pages";
import { isPageStatus, pageFailure } from "../../api/pages";
import { forgetPageDraft, goPages, hasSpace, pageDrafts, pageDraftsError, pageGuard, pages, pagesError, pagesLoading, pagesProblem, refreshPageDrafts, refreshPages, scopeVisible, spaceGrants, watchPageScope } from "../../pagesState";
import type { PagesRoute } from "../../pagesState";
import { apiClient, sessionGuard, spaces } from "../../store";
import { Action, ActionBar, Badge, Banner, Column, compactWidth, Label, Modal, PageHeader, PageLayout, Picker, Row, SegmentedControl, SettingRow, SettingsGroup, SettingsLinkRow } from "../../ui";
import { profileName } from "../profile/profileState";
import { Diagnostics, DraftPrompt, PagesError, scopeAudience, scopeName, showDraft, showPage } from "./controls";
import { cell } from "./renderPlan";

type Home = Extract<PagesRoute, { kind: "home" }>;
type StartMode = "prompt" | "template";

export function PagesHome(props: { route: Home }) {
  const creating = signal<StartMode | null>(null), deleted = signal(false);
  const scopeId = () => props.route.spaceId;
  const inSpace = () => !!scopeId();
  const valid = pageGuard(props.route);
  const personal = () => pages().filter(p => p.scope === "personal");
  const spaceGroups = () => spaces().filter(s => (!scopeId() || s.id === scopeId()) && pages().some(p => p.space_id === s.id))
    .slice().sort((a, b) => a.name.localeCompare(b.name));
  const empty = () => !pagesLoading() && !pagesError() && (inSpace()
    ? !pages().some(p => p.space_id === scopeId()) : !pages().length);
  const templates = signal<Template[]>([]), templateError = signal(""), loadingTemplates = signal(false);
  let alive = true;
  cleanup(() => { alive = false; });
  async function templatesLoad() {
    const client = apiClient();
    if (!client || !valid() || loadingTemplates()) return;
    templateError.set(""); loadingTemplates.set(true);
    try {
      const list = await client.pageTemplates();
      if (alive && valid()) templates.set(list);
    } catch (e) { if (alive && valid()) templateError.set(pagesProblem(e)); }
    finally { if (alive && valid()) loadingTemplates.set(false); }
  }
  let requestedTemplates = false;
  effect(() => {
    if (empty() || creating() === "template") untracked(() => {
      if (requestedTemplates) return;
      requestedTemplates = true;
      void templatesLoad();
    });
  });
  let lastSpaces: string | undefined;
  effect(() => {
    const key = JSON.stringify(spaces().map(s => s.id).sort());
    if (key !== lastSpaces) {
      lastSpaces = key;
      untracked(() => void refreshPageDrafts(valid));
    }
  });
  return <PageLayout testID="pages-home" inset={6} gap={4}>
    <Column gap={2}>
      <If when={inSpace()}><Row><Action compact variant="ghost" label="‹ All pages" onPress={() => goPages()} /></Row></If>
      <PageHeader title={inSpace() ? `${scopeName(scopeId())} pages` : "Pages"}
        subtitle={inSpace() ? scopeAudience(scopeId()) : "Private and shared dashboards or briefs."}>
        <Action testID="new-page" label="New page" variant="primary" onPress={() => creating.set("prompt")} />
      </PageHeader>
    </Column>
    <If when={props.route.afterDelete}>
      <Banner tone="success" variant="inline" message="Page deleted. You can restore it for 30 days."
        action={<Action compact label="Recently deleted" testID="page-deleted-recovery" onPress={() => deleted.set(true)} />} />
    </If>
    <PagesError message={pagesError()} onRetry={() => void refreshPages()} />
    <If when={pagesLoading() && !pages().length}><Label color="muted">Loading pages…</Label></If>
    <If when={empty()}>
      <Column gap={3} testID="pages-empty">
        <Column gap={1}>
          <Label heading variant="heading">No pages yet</Label>
          <Label>Start with a ready-made page, or describe what you want and review a draft before publishing.</Label>
        </Column>
        <Row gap={2} wrap>
          <Action testID="pages-start-template" label="Use a template" onPress={() => creating.set("template")} />
          <Action testID="pages-start-prompt" label="Describe a page" variant="ghost" onPress={() => creating.set("prompt")} />
        </Row>
        <If when={loadingTemplates()}><Label variant="meta">Loading templates…</Label></If>
        <PagesError message={templateError()} onRetry={() => void templatesLoad()} />
        <If when={templates().some(t => !inSpace() || t.scopes.includes("space"))}>
          <SettingsGroup flat title="Templates">
            <Each of={templates().filter(t => !inSpace() || t.scopes.includes("space"))} key={t => t.id}>
              {t => <SettingRow label={t().title} description={t().description} />}
            </Each>
          </SettingsGroup>
        </If>
      </Column>
    </If>
    <If when={!inSpace() && personal().length}><PageGroup title="Only you" entries={personal()} /></If>
    <Each of={spaceGroups()} key={s => s.id}>
      {space => <PageGroup title={`Members of ${space().name}`} entries={pages().filter(p => p.space_id === space().id)} />}
    </Each>
    <DraftGroup route={props.route} />
    <Row gap={2} wrap>
      <Action compact testID="pages-reload" label="Refresh list" variant="ghost" onPress={() => {
        void refreshPages(); void refreshPageDrafts(valid);
      }} />
      <Action compact testID="pages-deleted" label="Recently deleted" variant="ghost" onPress={() => deleted.set(true)} />
    </Row>
    <If when={creating()}>{mode => <NewPageSheet route={props.route} initialMode={mode()} close={() => creating.set(null)}
      templates={templates()} templatesError={templateError()} loadingTemplates={loadingTemplates()} loadTemplates={() => { requestedTemplates = true; void templatesLoad(); }} />}</If>
    <If when={deleted()}><DeletedPages route={props.route} close={() => deleted.set(false)} /></If>
  </PageLayout>;
}

function DraftGroup(props: { route: Home }) {
  const rows = () => pageDrafts().filter(d => !props.route.spaceId || d.space_id === props.route.spaceId);
  const selected = signal<DraftMeta | null>(null), busy = signal(false), error = signal("");
  const valid = pageGuard(props.route);
  async function discard(draft: DraftMeta) {
    const client = apiClient();
    if (!client || !valid() || busy() || !rows().some(d => d.id === draft.id)) return;
    const grant = sessionGuard(draft.space_id ?? undefined);
    busy.set(true); error.set("");
    try {
      await client.discardPageDraft(draft.id);
      if (valid() && grant()) forgetPageDraft(draft.id);
    } catch (e) {
      if (valid() && grant()) {
        if (isPageStatus(e, 404)) forgetPageDraft(draft.id);
        else error.set(pagesProblem(e));
      }
    } finally {
      busy.set(false); selected.set(null);
    }
  }
  const title = (draft: DraftMeta) => draft.page_title ?? "New page";
  return <Column gap={2}>
    <PagesError message={pageDraftsError()} onRetry={() => void refreshPageDrafts(valid)} />
    <PagesError message={error()} />
    <If when={rows().length}>
      <SettingsGroup flat title="Drafts" description="Only you can review or apply your drafts.">
        <Each of={rows()} key={d => d.id}>{draft => <SettingRow label={`${title(draft())} · ${draft().space_name ?? "Personal"}`}
          description={`${({ generating: "Building", ready: "Ready", failed: "Failed" })[draft().status]} · Updated ${cell(draft().updated_at, "relative").label.toLowerCase()}`}>
          <Row gap={1} wrap>
            <Action compact label="Resume" accessibilityLabel={`Resume draft: ${title(draft())}`} testID={`continue-page-draft:${draft().id}`}
              onPress={() => goPages({ kind: "draft", id: draft().id, spaceId: draft().space_id ?? undefined, returnPage: draft().page_id ?? undefined })} />
            <Action compact variant="ghost" label="Discard…" accessibilityLabel={`Discard draft: ${title(draft())}`} testID={`discard-listed-draft:${draft().id}`}
              onPress={() => { error.set(""); selected.set(draft()); }} />
          </Row>
        </SettingRow>}</Each>
      </SettingsGroup>
    </If>
    <If when={selected() && rows().some(d => d.id === selected()?.id)}>
      <Modal open title="Discard draft?" onClose={() => { if (!busy()) selected.set(null); }}>
        <Column pad={4} gap={3}>
          <Label>The published page won’t change.</Label>
          <ActionBar>
            <Action label="Cancel" variant="ghost" disabled={busy()} onPress={() => selected.set(null)} />
            <Action label="Discard draft" testID="discard-listed-draft-confirm" variant="danger" busy={busy()} onPress={() => {
              const draft = selected(); if (draft) void discard(draft);
            }} />
          </ActionBar>
        </Column>
      </Modal>
    </If>
  </Column>;
}

function PageGroup(props: { title: string; entries: PageMeta[] }) {
  return <SettingsGroup flat title={props.title}>
    <Each of={props.entries.slice().sort((a, b) => a.title.localeCompare(b.title))} key={p => p.id}>{p =>
      <SettingsLinkRow testID={`open-page:${p().id}`} label={p().title} mark={p().icon ?? undefined}
        accessibilityLabel={p().pinned ? `${p().title}, pinned` : p().title}
        description={`Created by ${profileName(p().creator_id) ?? p().creator_id} · Updated ${cell(p().updated_at, "relative").label.toLowerCase()}`}
        trailing={<If when={p().pinned}><Badge label="Pinned" tone="neutral" /></If>} onPress={() => {
          if (scopeVisible(p())) goPages({ kind: "page", id: p().id, spaceId: p().space_id ?? undefined });
        }} />
    }</Each>
  </SettingsGroup>;
}

function NewPageSheet(props: { route: Home; initialMode: StartMode; close: () => void;
  templates: Template[]; templatesError: string; loadingTemplates: boolean; loadTemplates: () => void }) {
  const scope = signal<string | null>(props.route.spaceId ?? null);
  const mode = signal<StartMode>(props.initialMode);
  const template = signal<string | null>(null), room = signal<string | null>(null);
  const rooms = signal<{ name: string; id: string }[]>([]);
  const error = signal(""), busy = signal<object | null>(null);
  const diagnostics = signal<ReturnType<typeof pageFailure>["diagnostics"]>([]);
  let alive = true, roomTicket = 0;
  cleanup(() => { alive = false; });
  const client = apiClient();
  const valid = pageGuard(props.route);
  const options = () => props.templates.filter(t => t.scopes.includes(scope() ? "space" : "personal"));
  const pickedTemplate = () => options().find(t => t.id === template());
  const needsRoom = () => !!scope() && !!pickedTemplate() && pickedTemplate()?.requires_space_destination !== false;
  const pickedRoom = () => room() === null ? rooms()[0] : rooms().find(r => r.id === room());
  const target = (): PageTarget => scope() ? { scope: "space", space_id: scope()! } : { scope: "personal" };
  watchPageScope(scope, (sid, visible) => {
    const ticket = ++roomTicket;
    room.set(null); template.set(null); rooms.set([]); error.set(""); diagnostics.set([]);
    if (!sid || !visible || !client) return;
    const grant = sessionGuard(sid);
    void client.listRooms(sid).then(list => {
      if (alive && valid() && grant() && ticket === roomTicket) rooms.set(list.filter(r => !r.parentRoomId));
    }, () => { /* A destination-dependent template cannot be submitted without a room. */ });
  });
  async function create(run: (c: NonNullable<typeof client>, dest: PageTarget, current: () => boolean) => Promise<void>) {
    if (!client || !valid() || busy() || !hasSpace(scope())) return;
    const dest = target(), request = {};
    const grant = sessionGuard(dest.space_id);
    const ownsRequest = () => alive && busy() === request;
    const current = () => ownsRequest() && valid() && grant() && hasSpace(dest.space_id) && scope() === (dest.space_id ?? null);
    busy.set(request); error.set(""); diagnostics.set([]);
    try { await run(client, dest, current); }
    catch (e) { if (current()) { error.set(pagesProblem(e)); diagnostics.set(pageFailure(e).diagnostics); } }
    finally { if (ownsRequest()) busy.set(null); }
  }
  const sameTarget = (dest: PageTarget, result: Pick<PageMeta, "scope" | "space_id">) =>
    scopeVisible(result) && result.scope === dest.scope && result.space_id === (dest.space_id ?? null);
  return <DraftPrompt spaceId={scope()} busy={!!busy()} onSubmit={(prompt, choice) => void create(async (client, dest, current) => {
    const draft = await client.createPageDraft(dest, prompt, choice);
    if (current() && sameTarget(dest, draft)) showDraft(draft, choice);
  })} layout={(fields, generate, checking) => {
    const locked = () => !!busy() || checking();
    const close = () => { if (!locked()) props.close(); };
    return <Modal open size="form" title="New page" testID="new-page-sheet" guardDismiss onClose={close}>
      <PageLayout testID="new-page-form" inset={compactWidth() ? 4 : 6} gap={3} footer={
        <ActionBar>
          <Action label="Cancel" disabled={locked()} variant="ghost" onPress={close} />
          <If when={mode() === "prompt"} else={
            <Action testID="create-page-template" label="Create page" variant="primary"
              disabled={!pickedTemplate() || !hasSpace(scope()) || (needsRoom() && !pickedRoom())} busy={!!busy()}
              onPress={() => void create(async (client, dest, current) => {
                const t = pickedTemplate(), r = needsRoom() ? pickedRoom() : undefined;
                if (!t || (needsRoom() && !r)) return;
                const page = await client.createPage({ ...dest, template: t.id,
                  ...(r ? { room: r.name, ...(t.requires_space_destination ? { room_id: r.id } : {}) } : {}) });
                if (current() && sameTarget(dest, page)) showPage(page);
              })} />
          }>{generate}</If>
        </ActionBar>
      }>
        <PageHeader title="New page" />
        <Column gap={1}>
          <Label variant="meta">Who can see the published page?</Label>
          <Picker testID="new-page-scope" accessibilityLabel="Page scope" disabled={locked()}
            items={[{ value: "personal", label: "Only you", detail: "Keep the page private." },
              ...spaces().map(s => ({ value: `space:${s.id}`, label: `Members of ${s.name}`, group: "Spaces", detail: "Share the page with this Space." }))]}
            value={scope() ? `space:${scope()}` : "personal"} onChange={value => scope.set(value?.startsWith("space:") ? value.slice(6) : null)} />
        </Column>
        <SegmentedControl accessibilityLabel="How to start" value={mode()} disabled={locked()} options={[
          { value: "prompt", label: "Describe a page", testID: "new-page-prompt" },
          { value: "template", label: "Use a template", testID: "new-page-templates" },
        ]} onChange={next => {
          mode.set(next);
          if (next === "template" && !props.templates.length && !props.templatesError) props.loadTemplates();
        }} />
        <If when={mode() === "prompt"} else={<Column gap={2}>
          <If when={!hasSpace(scope())}><Label color="warning">You no longer have access to this Space. Choose a different scope.</Label></If>
          <PagesError message={props.templatesError} onRetry={props.loadTemplates} />
          <Picker accessibilityLabel="Page template" testID="page-template" placeholder={props.loadingTemplates ? "Loading templates…" : "Choose a template"} value={template()}
            disabled={!!busy()} items={options().map(t => ({ value: t.id, label: t.title, detail: t.description }))} onChange={value => {
              template.set(value); room.set(null);
            }} />
          <If when={pickedTemplate()?.description}><Label variant="caption" color="muted">{pickedTemplate()?.description}</Label></If>
          <If when={needsRoom()}>
            <Picker accessibilityLabel="Template room" testID="page-template-room" disabled={!!busy() || !rooms().length} value={pickedRoom()?.id ?? null}
              placeholder="No room available" items={rooms().map(r => ({ value: r.id, label: r.name }))} onChange={room.set} />
            <Label variant="caption" color="muted">This template uses the selected room and the Space’s default agent.</Label>
          </If>
        </Column>}>{fields}</If>
        <Diagnostics items={diagnostics() ?? []} />
        <PagesError message={error()} />
      </PageLayout>
    </Modal>;
  }} />;
}

function DeletedPages(props: { route: Home; close: () => void }) {
  const entries = signal<{ page: PageMeta; access: () => boolean }[]>([]);
  const error = signal(""), loading = signal(true), busy = signal<{ pageId: string } | null>(null);
  const client = apiClient();
  const guard = pageGuard(props.route);
  let alive = true, request = 0;
  cleanup(() => { alive = false; });
  const valid = () => alive && guard();
  const rows = () => entries().filter(({ page, access }) => scopeVisible(page) && access()
    && (!props.route.spaceId || props.route.spaceId === page.space_id));
  async function load() {
    if (!client || !valid()) return;
    const id = ++request;
    const grants = spaceGrants(), session = sessionGuard();
    loading.set(true); error.set(""); entries.set([]);
    try {
      const result = await client.listPages(true);
      if (valid() && id === request) entries.set(result.flatMap(page => {
        const access = page.space_id ? grants.get(page.space_id) : session;
        return access?.() && scopeVisible(page) ? [{ page, access }] : [];
      }));
    } catch (e) { if (valid() && id === request) error.set(pagesProblem(e)); }
    finally { if (valid() && id === request) loading.set(false); }
  }
  async function restore({ page, access }: ReturnType<typeof rows>[number]) {
    if (!client || !valid() || busy() || !access() || !scopeVisible(page)) return;
    const request = { pageId: page.id };
    const ownsRequest = () => alive && busy() === request;
    const current = () => ownsRequest() && valid() && access() && scopeVisible(page);
    busy.set(request); error.set("");
    try {
      const restored = await client.undeletePage(page.id);
      if (current() && scopeVisible(restored) && restored.id === page.id && restored.space_id === page.space_id) showPage(restored);
    } catch (e) {
      if (current()) {
        if (isPageStatus(e, 404)) entries.set(entries().filter(entry => entry.page.id !== page.id));
        error.set(pagesProblem(e));
      }
    } finally { if (ownsRequest()) busy.set(null); }
  }
  untracked(() => void load());
  return <Modal open title="Recently deleted" testID="pages-recently-deleted" onClose={props.close}>
    <PageLayout inset={compactWidth() ? 4 : 6} gap={4} footer={
      <ActionBar>
        <Action label="Refresh list" testID="deleted-pages-refresh" disabled={loading() || !!busy()} onPress={() => void load()} />
        <Action label="Done" onPress={props.close} />
      </ActionBar>
    }>
      <PageHeader title="Recently deleted" subtitle="You can restore pages you manage for 30 days after deletion." />
      <PagesError message={error()} onRetry={() => void load()} />
      <If when={loading()}><Label color="muted">Loading deleted pages…</Label></If>
      <If when={!loading() && !rows().length && !error()}><Label color="muted">No recently deleted pages.</Label></If>
      <If when={rows().length}>
        <Column gap={2}>
          <Each of={rows()} key={entry => entry.page.id}>
            {entry => <Row align="center" gap={2} justify="between" wrap>
              <Column gap={1}>
                <Label>{entry().page.title} · {scopeName(entry().page.space_id)}</Label>
                <Label variant="meta">{restoreTime(entry().page.deleted_at)}</Label>
              </Column>
              <Action compact label="Restore page" accessibilityLabel={`Restore ${entry().page.title}`} testID={`undelete-page:${entry().page.id}`} disabled={!!busy()} busy={busy()?.pageId === entry().page.id} onPress={() => void restore(entry())} />
            </Row>}
          </Each>
        </Column>
      </If>
    </PageLayout>
  </Modal>;
}

function restoreTime(deletedAt: string | null) {
  const days = Math.ceil((Date.parse(deletedAt ?? "") + 30 * 86_400_000 - Date.now()) / 86_400_000);
  return !Number.isFinite(days) ? "" : days <= 0 ? "Restore window ended" : days === 1 ? "Less than a day left to restore" : `${Math.min(days, 30)} days left to restore`;
}
