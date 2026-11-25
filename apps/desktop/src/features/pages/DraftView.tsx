import { cleanup, effect, lifecycle, signal, untracked } from "nsh";
import type { TurnModelChoice } from "../../api/client";
import type { Draft, Page, PageData, PageDiagnostic } from "../../api/pages";
import { isPageStatus, pageFailure, targetOf } from "../../api/pages";
import { forgetPageDraft, goPages, pageGuard, pagesProblem, scopeVisible } from "../../pagesState";
import type { PagesRoute } from "../../pagesState";
import { apiClient, openExactRoom, sessionGuard, spaces } from "../../store";
import { Action, ActionBar, Badge, Banner, Column, Label, modalOpen, PageHeader, Row, ScrollArea, SegmentedControl } from "../../ui";
import { PageCode, PageDiff } from "../../ui/pageBlocks";
import { Diagnostics, DraftPrompt, PagesError, scopeName, showDraft, showPage } from "./controls";
import { PageRenderer } from "./PageRenderer";
import { visiblePolling } from "./refresh";

type DraftRoute = Extract<PagesRoute, { kind: "draft" }>;
type Tab = "preview" | "changes" | "source";

export function DraftView(props: { route: DraftRoute }) {
  const route = props.route, client = apiClient();
  const guard = pageGuard(route);
  const draft = signal<Draft | null>(null), data = signal<PageData | null>(null);
  const diagnostics = signal<PageDiagnostic[]>([]);
  const currentHead = signal<Page | null>(null), stale = signal(false);
  const tab = signal<Tab>("preview"), error = signal("");
  const outside = signal<string[]>([]), confirmOutside = signal(false), retry = signal(false), discard = signal(false);
  const busy = signal(false), loading = signal(true), previewing = signal(false), unavailable = signal(false);
  let alive = true, checking = false, grant = sessionGuard(route.spaceId);
  let dataSeq = 0;
  cleanup(() => { alive = false; });
  const valid = () => alive && guard() && grant() && (!draft() || scopeVisible(draft()!));
  const visible = () => valid() && !modalOpen() && lifecycle() === "active";
  const available = () => valid() && !unavailable();

  function missing() {
    forgetPageDraft(route.id);
    unavailable.set(true);
    error.set("This draft is no longer available. It may have been applied, discarded, expired, or you may no longer have access.");
    data.set(null); draft.set(null); currentHead.set(null); diagnostics.set([]); outside.set([]);
  }
  function problem(e: unknown) {
    if (isPageStatus(e, 404) || isPageStatus(e, 403)) missing();
    else { error.set(pagesProblem(e)); diagnostics.set(pageFailure(e).diagnostics ?? []); }
  }
  async function load() {
    if (!client || !available() || checking) return;
    checking = true;
    const grants = new Map(spaces().map(s => [s.id, sessionGuard(s.id)]));
    try {
      const next = await client.getPageDraft(route.id);
      if (!available()) return;
      if (next.id !== route.id || next.space_id !== (route.spaceId ?? null) || (next.space_id && !grants.get(next.space_id)?.()) || !scopeVisible(next)) { missing(); return; }
      grant = sessionGuard(next.space_id ?? undefined);
      draft.set(next);
      diagnostics.set(next.diagnostics);
      outside.set(next.outside_selection);
      error.set(next.error ?? "");
    } catch (e) { if (available()) problem(e); }
    finally { checking = false; if (valid()) loading.set(false); }
  }
  visiblePolling(() => available() && (!draft() || draft()?.status === "generating"), () => 1_500, () => void load(), true);
  async function preview() {
    if (!client || !available() || draft()?.status !== "ready" || !draft()?.spec) return;
    const index = ++dataSeq, grants = spaces().map(s => sessionGuard(s.id));
    const allowed = () => available() && index === dataSeq && grants.every(ok => ok());
    previewing.set(true);
    try {
      const values = await client.pageDraftData(route.id);
      if (allowed()) data.set(values);
    } catch (e) { if (allowed()) problem(e); }
    finally { if (available() && index === dataSeq) previewing.set(false); }
  }
  effect(() => {
    const usePreview = visible() && tab() === "preview" && draft()?.status === "ready" && !!draft()?.spec;
    // Personal drafts may read any Space the author currently belongs to.
    const scope = JSON.stringify(spaces().map(s => s.id).sort());
    if (usePreview) untracked(() => { data.set(null); void preview(); });
    else untracked(() => { ++dataSeq; data.set(null); previewing.set(false); });
    void scope;
  });

  function back() {
    if (!valid() || busy()) return;
    if (route.returnRoom && route.spaceId) {
      void openExactRoom(route.spaceId, route.returnRoom, valid).then(opened => {
        if (!opened && valid()) error.set("The room where this draft was proposed is no longer available. Use All pages to leave this review.");
      }, e => { if (valid()) error.set(pagesProblem(e)); });
    } else if (route.returnPage) goPages({ kind: "page", id: route.returnPage, spaceId: route.spaceId });
    else goPages({ kind: "home", spaceId: route.spaceId });
  }
  async function apply(confirmed = false) {
    const review = draft();
    if (!client || !visible() || !available() || review?.status !== "ready" || stale() || busy()) return;
    if (outside().length && !confirmed) { confirmOutside.set(true); return; }
    if (confirmed && !confirmOutside()) return;
    busy.set(true); error.set(""); confirmOutside.set(false);
    try {
      const applied = await client.applyPageDraft(route.id, confirmed);
      if (available() && scopeVisible(applied) && applied.space_id === review.space_id && (!review.page_id || applied.id === review.page_id)) {
        forgetPageDraft(route.id);
        showPage(applied);
      }
    } catch (e) {
      if (!available()) return;
      const detail = pageFailure(e);
      if (isPageStatus(e, 409) && Number.isSafeInteger(detail.head_version) && review.page_id) {
        stale.set(true);
        error.set("This draft is based on an older version. Nothing was changed. Compare with the current source or edit the prompt and generate a new draft.");
        tab.set("changes");
        try {
          const head = await client.getPage(review.page_id);
          if (available() && scopeVisible(head) && head.id === review.page_id && head.space_id === review.space_id) currentHead.set(head);
        } catch (headError) {
          if (available() && (isPageStatus(headError, 404) || isPageStatus(headError, 403))) missing();
          else if (available() && headError instanceof Error) error.set(`The page changed. Could not load the current version. ${pagesProblem(headError)}`);
        }
      } else if (isPageStatus(e, 409) && Array.isArray(detail.outside_selection) && detail.outside_selection.length) {
        outside.set(detail.outside_selection);
        confirmOutside.set(true);
      } else problem(e);
    } finally { if (valid()) busy.set(false); }
  }
  async function remove() {
    if (!client || !available() || !visible() || !discard() || busy()) return;
    busy.set(true); error.set("");
    try {
      await client.discardPageDraft(route.id);
      if (valid()) {
        forgetPageDraft(route.id);
        busy.set(false);
        unavailable.set(true); draft.set(null); data.set(null);
        back();
      }
    } catch (e) { if (available()) problem(e); }
    finally { if (valid()) { busy.set(false); discard.set(false); } }
  }
  async function regenerate(prompt: string, choice?: TurnModelChoice) {
    const review = draft();
    if (!client || !review || !available() || !visible() || busy()) return;
    busy.set(true); error.set("");
    try {
      const next = await client.createPageDraft(review.page_id ? { page_id: review.page_id, block_key: review.block_key ?? undefined } : targetOf(review), prompt, choice);
      if (!available() || !scopeVisible(next) || next.space_id !== review.space_id || next.page_id !== review.page_id) return;
      // Retry is a new draft against the current head. Retire the old one;
      // if that fails it remains private and expires on the server.
      void client.discardPageDraft(review.id).catch(() => {});
      forgetPageDraft(review.id);
      if (route.returnRoom) goPages({ ...route, id: next.id });
      else showDraft(next);
    } catch (e) {
      // A missing selected block or newly revoked edit grant must never
      // quietly widen a selected edit to the whole document.
      if (available()) { error.set(pagesProblem(e)); diagnostics.set(pageFailure(e).diagnostics ?? []); }
    } finally { if (valid()) busy.set(false); }
  }
  const changed = () => new Set([...(draft()?.changes?.added ?? []), ...(draft()?.changes?.changed ?? [])]);
  const title = () => draft()?.spec?.title || "Page draft";
  const changeSummary = () => {
    const c = draft()?.changes;
    if (!c) return "";
    return [`${c.added.length} added`, `${c.changed.length} changed`, `${c.removed.length} removed`,
      c.data.length ? `${c.data.length} data sources changed` : "", c.page ? "Page layout or settings changed" : ""].filter(Boolean).join(" · ");
  };
  return <Column grow pad={4} gap={3} testID="page-draft-view">
    <Row gap={2} wrap>
      <Action compact testID="page-draft-back" label={route.returnRoom ? "‹ Back to room" : route.returnPage ? "‹ Back to page" : "‹ Back to pages"}
        variant="ghost" disabled={busy()} onPress={back} />
      <If when={route.returnRoom || route.returnPage}><Action compact variant="ghost" label="All pages" onPress={() => goPages()} /></If>
    </Row>
    <PageHeader title={title()} subtitle="Review the draft before it changes a page.">
      <Badge label={scopeName(route.spaceId)} tone="system" />
    </PageHeader>
    <PagesError message={error()} />
    <If when={loading()}><Label color="muted">Loading draft…</Label></If>
    <If when={available() ? draft() : null}>{review => <>
      <If when={review().status === "generating"}><Label testID="page-draft-generating">Building your draft… You can leave this view while it runs.</Label></If>
      <If when={review().status !== "generating"}>
        <Row gap={2} wrap align="center">
          <SegmentedControl testID="page-draft-tab" accessibilityLabel="Draft review tab" value={tab()} onChange={tab.set}
            options={[{ value: "preview", label: "Preview" }, { value: "changes", label: "Changes" }, { value: "source", label: "Source" }]} />
          <Label variant="meta">{review().base_version ? `Based on version ${review().base_version}` : "New page"}</Label>
        </Row>
        <If when={tab() === "preview"}>
          <ScrollArea gap={2}>
            <If when={review().spec} else={<Label color="muted">No preview is available. Review the diagnostics or edit the prompt and retry.</Label>}>
              {spec => <PageRenderer spec={spec()} data={data()} pending={previewing()} changed={changed()} readOnly />}
            </If>
            <Diagnostics items={diagnostics()} />
          </ScrollArea>
        </If>
        <If when={tab() === "changes"}>
          <Label variant="caption" color="muted">{currentHead() ? `Comparing draft with current version ${currentHead()!.head_version}` : changeSummary()}</Label>
          <If when={!currentHead() && review().base_version !== null && review().base_source === null}>
            <Label variant="caption" color="warning">The original version is no longer in history. Its source cannot be compared.</Label>
          </If>
          <PageDiff before={currentHead()?.source ?? review().base_source ?? ""} after={review().source ?? ""} />
        </If>
        <If when={tab() === "source"}><PageCode value={review().source ?? ""} readOnly /></If>
        <If when={outside().length}>
          <Banner tone="warning" variant="inline" title="Changes outside the selected block"
            message={outside().slice(0, 12).join(", ")} />
        </If>
        <If when={confirmOutside()}>
          <ActionBar><Action label="Cancel" variant="ghost" onPress={() => confirmOutside.set(false)} />
            <Action testID="page-apply-outside" label="Apply all changes" variant="primary" disabled={stale()} onPress={() => void apply(true)} /></ActionBar>
        </If>
        <If when={retry()}>
          <ScrollArea gap={2}>
            <Label variant="strong">{review().block_key ? `Retry edit to block ${review().block_key}` : "Edit prompt and retry"}</Label>
            <DraftPrompt spaceId={review().space_id} initial={review().prompt} label="Revise the prompt" busy={busy()} onSubmit={(prompt, choice) => void regenerate(prompt, choice)} />
          </ScrollArea>
        </If>
      </If>
      <If when={discard()}>
        <Banner tone="warning" variant="inline" message="Discard this draft? The published page won’t change." />
        <ActionBar><Action label="Cancel" variant="ghost" onPress={() => discard.set(false)} />
          <Action testID="page-discard-now" label="Discard draft" variant="danger" busy={busy()} onPress={() => void remove()} /></ActionBar>
      </If>
      <ActionBar>
        <Action label="Discard…" testID="page-discard" variant="destructive" disabled={busy() || discard()} onPress={() => discard.set(true)} />
        <If when={review().status !== "generating"}>
          <Action testID="page-draft-retry" label={retry() ? "Close prompt" : "Edit prompt and retry"} variant="ghost" disabled={busy()} onPress={() => retry.set(!retry())} />
        </If>
        <Action testID="page-apply" label="Apply" variant="primary" disabled={stale() || review().status !== "ready" || confirmOutside() || discard()} busy={busy()} onPress={() => void apply()} />
      </ActionBar>
    </>}</If>
  </Column>;
}
