import { cleanup, signal, untracked } from "nsh";
import type { PageMeta, PageTarget, Template } from "../../api/pages";
import { isPageStatus, pageFailure } from "../../api/pages";
import { goPages, hasSpace, pageGuard, pages, pagesError, pagesLoading, pagesProblem, recentPageDrafts, refreshPages, scopeVisible, spaceGrants, watchPageScope } from "../../pagesState";
import type { PagesRoute } from "../../pagesState";
import { apiClient, sessionGuard, spaces } from "../../store";
import { Action, ActionBar, Badge, Column, compactWidth, Label, Modal, PageHeader, PageLayout, Picker, Row, SettingsGroup, Surface } from "../../ui";
import { profileName } from "../profile/profileState";
import { Diagnostics, DraftPrompt, PagesError, scopeName, showDraft, showPage } from "./controls";
import { cell } from "./renderPlan";

type Home = Extract<PagesRoute, { kind: "home" }>;

export function PagesHome(props: { route: Home }) {
  const creating = signal(false), deleted = signal(false);
  const scopeId = () => props.route.spaceId;
  const inSpace = () => !!scopeId();
  const drafts = () => recentPageDrafts().filter(d => !scopeId() || d.spaceId === scopeId());
  return <PageLayout testID="pages-home" inset={6} gap={6}>
    <Column gap={2}>
      <If when={inSpace()}><Row><Action compact variant="ghost" label="‹ All pages" onPress={() => goPages()} /></Row></If>
      <PageHeader title={inSpace() ? `${scopeName(scopeId())} pages` : "Pages"}
        subtitle="Build dashboards and small apps with Page TSX. Review generated changes before applying them.">
        <Action testID="new-page" label="New page" variant="primary" onPress={() => creating.set(true)} />
      </PageHeader>
    </Column>
    <Row gap={2} wrap>
      <Action compact testID="pages-reload" label="Refresh list" onPress={() => void refreshPages()} />
      <Action compact testID="pages-deleted" label="Recently deleted" variant="ghost" onPress={() => deleted.set(true)} />
    </Row>
    <PagesError message={pagesError()} onRetry={() => void refreshPages()} />
    <If when={pagesLoading() && !pages().length}><Label color="muted">Loading pages…</Label></If>
    <If when={!inSpace()}><PageGroup title="Personal" entries={pages().filter(p => p.scope === "personal")} /></If>
    <Each of={spaces().filter(s => scopeId() ? s.id === scopeId() : pages().some(p => p.space_id === s.id))
      .slice().sort((a, b) => a.name.localeCompare(b.name))} key={s => s.id}>
      {space => <PageGroup title={space().name} entries={pages().filter(p => p.space_id === space().id)} />}
    </Each>
    <If when={drafts().length}>
      <SettingsGroup title="Continue reviewing" description="Drafts opened in this session. Only you can review or apply them.">
        <Each of={drafts()} key={d => d.id}>{draft => <Row gap={2} padX={4} padY={3} align="center" justify="between" wrap>
          <Label>{pages().find(p => p.id === draft().returnPage)?.title ?? (draft().returnRoom ? "Draft from a room" : "Page draft")} · {scopeName(draft().spaceId)}</Label>
          <Action compact label="Review draft" testID={`continue-page-draft:${draft().id}`} onPress={() => goPages({ ...draft() })} />
        </Row>}</Each>
      </SettingsGroup>
    </If>
    <If when={creating()}><NewPageSheet route={props.route} close={() => creating.set(false)} /></If>
    <If when={deleted()}><DeletedPages route={props.route} close={() => deleted.set(false)} /></If>
  </PageLayout>;
}

function PageGroup(props: { title: string; entries: PageMeta[] }) {
  return <SettingsGroup title={props.title}>
    <If when={!props.entries.length && !pagesLoading() && !pagesError()}>
      <Column padX={4} padY={3}><Label color="muted">No pages yet. Create one from a template or a prompt.</Label></Column>
    </If>
    <Each of={props.entries} key={p => p.id}>{p =>
      <Surface variant="card" padX={4} padY={3}>
        <Row gap={2} align="center" justify="between" wrap>
          <Column gap={1} grow>
            <Label variant="strong">{[p().icon, p().title].filter(Boolean).join(" ")}</Label>
            <Label variant="meta">{`Created by ${profileName(p().creator_id) ?? p().creator_id} · Updated ${cell(p().updated_at, "relative").label.toLowerCase()}`}</Label>
          </Column>
          <If when={p().pinned}><Badge label="Pinned" tone="system" /></If>
          <Action testID={`open-page:${p().id}`} label="Open" accessibilityLabel={`Open ${p().title}`} onPress={() => {
            if (scopeVisible(p())) goPages({ kind: "page", id: p().id, spaceId: p().space_id ?? undefined });
          }} />
        </Row>
      </Surface>
    }</Each>
  </SettingsGroup>;
}

function NewPageSheet(props: { route: Home; close: () => void }) {
  const scope = signal<string | null>(props.route.spaceId ?? null);
  const mode = signal<"prompt" | "template">("prompt");
  const templates = signal<Template[]>([]);
  const template = signal<string | null>(null), room = signal<string | null>(null);
  const rooms = signal<{ name: string; id: string }[]>([]);
  const error = signal(""), busy = signal<object | null>(null), templateError = signal("");
  const diagnostics = signal<ReturnType<typeof pageFailure>["diagnostics"]>([]);
  let alive = true, roomTicket = 0;
  cleanup(() => { alive = false; });
  const client = apiClient();
  const valid = pageGuard(props.route);
  const options = () => templates().filter(t => t.scopes.includes(scope() ? "space" : "personal"));
  const target = (): PageTarget => scope() ? { scope: "space", space_id: scope()! } : { scope: "personal" };
  async function templatesLoad() {
    templateError.set("");
    try {
      const list = await client?.pageTemplates();
      if (alive && valid() && list) templates.set(list);
    } catch (e) { if (alive && valid()) templateError.set(pagesProblem(e)); }
  }
  watchPageScope(scope, (sid, visible) => {
    const ticket = ++roomTicket;
    room.set(null); template.set(null); rooms.set([]); error.set(""); diagnostics.set([]);
    if (!sid || !visible || !client) return;
    const grant = sessionGuard(sid);
    void client.listRooms(sid).then(list => {
      if (alive && valid() && grant() && ticket === roomTicket) rooms.set(list.filter(r => !r.parentRoomId));
    }, () => { /* default room is independently checked by the server */ });
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
  return <Modal open title="New page" testID="new-page-sheet" onClose={() => { if (!busy()) props.close(); }}>
    <PageLayout inset={compactWidth() ? 4 : 6} gap={4}>
      <PageHeader title="New page" subtitle="Choose who can see the page and how to start." />
      <Picker testID="new-page-scope" accessibilityLabel="Page scope" disabled={!!busy()}
        items={[{ value: "personal", label: "Personal", detail: "Only you can see this page." },
          ...spaces().map(s => ({ value: `space:${s.id}`, label: s.name, detail: "Members of this Space can see this page.", group: "Spaces" }))]}
        value={scope() ? `space:${scope()}` : "personal"} onChange={value => scope.set(value?.startsWith("space:") ? value.slice(6) : null)} />
      <If when={!hasSpace(scope()) && mode() === "template"}><Label color="warning">You no longer have access to this Space. Choose a different scope.</Label></If>
      <Row gap={2} wrap>
        <Action label="Describe a page" testID="new-page-prompt" disabled={!!busy()} variant={mode() === "prompt" ? "primary" : "secondary"} onPress={() => mode.set("prompt")} />
        <Action label="Use a template" testID="new-page-templates" disabled={!!busy()} variant={mode() === "template" ? "primary" : "secondary"} onPress={() => {
          mode.set("template"); if (!templates().length) void templatesLoad();
        }} />
      </Row>
      <If when={mode() === "prompt"} else={<Column gap={2}>
        <PagesError message={templateError()} onRetry={() => void templatesLoad()} />
        <Picker accessibilityLabel="Page template" testID="page-template" placeholder="Choose a template" value={template()}
          disabled={!!busy()} items={options().map(t => ({ value: t.id, label: t.title, detail: t.description }))} onChange={template.set} />
        <If when={scope() && rooms().length}>
          <Picker accessibilityLabel="Template room" testID="page-template-room" disabled={!!busy()} value={room() ?? ""}
            items={[{ value: "", label: "First room" }, ...rooms().map(r => ({ value: r.id, label: r.name }))]} onChange={room.set} />
          <Label variant="caption" color="muted">Templates use the Space’s default agent.</Label>
        </If>
        <Row justify="end"><Action testID="create-page-template" label="Create page" variant="primary" disabled={!template() || !hasSpace(scope())} busy={!!busy()}
          onPress={() => void create(async (client, dest, current) => {
            const page = await client.createPage({ ...dest, template: template()!, room: rooms().find(r => r.id === room())?.name });
            if (current() && sameTarget(dest, page)) showPage(page);
          })} /></Row>
      </Column>}>
        <DraftPrompt spaceId={scope()} busy={!!busy()} onSubmit={(prompt, choice) => void create(async (client, dest, current) => {
          const draft = await client.createPageDraft(dest, prompt, choice);
          if (current() && sameTarget(dest, draft)) showDraft(draft);
        })} />
      </If>
      <Diagnostics items={diagnostics() ?? []} />
      <PagesError message={error()} />
      <Row justify="end"><Action label="Cancel" disabled={!!busy()} variant="ghost" onPress={props.close} /></Row>
    </PageLayout>
  </Modal>;
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
              <Label>{entry().page.title} · {scopeName(entry().page.space_id)}</Label>
              <Action compact label="Undelete" testID={`undelete-page:${entry().page.id}`} disabled={!!busy()} busy={busy()?.pageId === entry().page.id} onPress={() => void restore(entry())} />
            </Row>}
          </Each>
        </Column>
      </If>
    </PageLayout>
  </Modal>;
}
