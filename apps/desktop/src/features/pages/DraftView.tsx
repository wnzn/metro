import { announce, cleanup, effect, lifecycle, signal, untracked } from "nsh";
import type { ViewRef } from "nsh";
import type { TurnModelChoice } from "../../api/client";
import type { Draft, Page, PageDiagnostic, PageValues } from "../../api/pages";
import { isPageStatus, pageFailure, targetOf } from "../../api/pages";
import { forgetPageDraft, goPages, pageDraftModel, pageDraftUnavailable, pageGuard, pagesProblem, rememberPageDraftModel, sameSpaceGrants, scopeVisible, spaceGrants } from "../../pagesState";
import type { PagesRoute } from "../../pagesState";
import { apiClient, openExactRoom, sessionGuard } from "../../store";
import { Action, ActionBar, Badge, Banner, Column, Label, modalOpen, PageHeader, Row, ScrollArea, SegmentedControl } from "../../ui";
import { PageCode, PageDiff } from "../../ui/pageBlocks";
import { Diagnostics, DraftPrompt, PagesError, scopeName, showDraft, showPage } from "./controls";
import { PageRenderer } from "./PageRenderer";
import { pageReadLane, visiblePolling } from "./refresh";

type DraftRoute = Extract<PagesRoute, { kind: "draft" }>;
type Tab = "preview" | "changes" | "source";

export function DraftView(props: { route: DraftRoute }) {
  const route = props.route, client = apiClient();
  const guard = pageGuard(route);
  const draft = signal<Draft | null>(null), data = signal<PageValues | null>(null);
  const diagnostics = signal<PageDiagnostic[]>([]);
  const currentHead = signal<Page | null>(null), stale = signal(false);
  const tab = signal<Tab>("preview"), error = signal("");
  const outside = signal<string[]>([]), confirmOutside = signal(false), retry = signal(false), discard = signal(false);
  const busy = signal(false), loading = signal(true), previewing = signal(false), unavailable = signal(pageDraftUnavailable(route.id));
  const previewError = signal("");
  let retryControl: ViewRef | undefined;
  function closeRetry() { if (!busy()) { retry.set(false); retryControl?.focus(); } }
  const applyAccess = signal<"allowed" | "checking" | "denied">("allowed");
  const checkError = signal("");
  const conflict = signal<{ pageId: string; version: number } | null>(null), readingHead = signal(false), headError = signal("");
  const draftReads = pageReadLane<string>(), previewReads = pageReadLane<Draft>(), headReads = pageReadLane<string>();
  let alive = true, grant = sessionGuard(route.spaceId);
  let draftStatus: Draft["status"] | undefined;
  cleanup(() => { alive = false; });
  const valid = () => alive && guard() && grant() && (!draft() || scopeVisible(draft()!));
  const visible = () => valid() && !modalOpen() && lifecycle() === "active";
  const available = () => valid() && !unavailable();

  function missing() {
    draftReads.cancel(); previewReads.cancel(); headReads.cancel();
    forgetPageDraft(route.id);
    unavailable.set(true);
    checkError.set("");
    applyAccess.set("denied");
    error.set("This draft is no longer available. It may have been applied, discarded, expired, or you may no longer have access.");
    loading.set(false); previewing.set(false); readingHead.set(false); previewError.set("");
    data.set(null); draft.set(null); currentHead.set(null); diagnostics.set([]); outside.set([]);
  }
  function problem(e: unknown) {
    if (isPageStatus(e, 404) || isPageStatus(e, 403)) missing();
    else { error.set(pagesProblem(e)); diagnostics.set(pageFailure(e).diagnostics ?? []); }
  }
  function load(recheck = false) {
    if (!client || !visible() || !available()) return;
    const grants = spaceGrants();
    return draftReads.run(recheck ? "recheck" : "draft", async (signal, current) => {
      const allowed = () => current() && visible() && available();
      try {
        if (!allowed()) return;
        const next = await client.getPageDraft(route.id, signal);
        if (!allowed()) return;
        if (next.id !== route.id || next.space_id !== (route.spaceId ?? null) || (route.returnPage && next.page_id !== route.returnPage)
          || (next.space_id && !grants.get(next.space_id)?.()) || !scopeVisible(next)) { missing(); return; }
        grant = sessionGuard(next.space_id ?? undefined);
        draft.set(next);
        diagnostics.set(next.diagnostics);
        outside.set(next.outside_selection);
        error.set("");
        if (next.status !== draftStatus) {
          if (next.status === "failed") announce(next.error || "Draft generation failed.");
          else if (next.status === "ready" && draftStatus === "generating") announce("Draft ready for review.");
          draftStatus = next.status;
        }
        if (recheck) applyAccess.set("denied");
      } catch (e) {
        if (allowed()) {
          if (recheck && (isPageStatus(e, 403) || isPageStatus(e, 404))) missing();
          else if (recheck) checkError.set(pagesProblem(e));
          else problem(e);
        }
      }
      finally { if (allowed()) loading.set(false); }
    });
  }
  if (unavailable()) missing();
  const generating = () => !draft() || draft()?.status === "generating";
  visiblePolling(() => visible() && available() && generating() && applyAccess() !== "checking", () => 1_500, () => void load());
  function preview(review: Draft) {
    if (!client || !visible() || !available()) return;
    const grants = spaceGrants();
    return previewReads.run(review, async (signal, current) => {
      const allowed = () => current() && visible() && available() && tab() === "preview" && draft() === review && [...grants.values()].every(ok => ok());
      if (!allowed()) return;
      previewing.set(true); previewError.set("");
      try {
        const values = await client.pageDraftData(route.id, signal);
        if (allowed()) data.set(values);
      } catch (e) {
        if (allowed()) {
          if (isPageStatus(e, 404) || isPageStatus(e, 403)) missing();
          else previewError.set(pagesProblem(e));
        }
      }
      finally { if (allowed()) previewing.set(false); }
    });
  }
  function loadConflictHead() {
    const target = conflict();
    if (!client || !target || !available() || !visible() || tab() !== "changes" || currentHead()) return;
    return headReads.run(target.pageId, async (signal, current) => {
      const allowed = () => current() && visible() && available() && tab() === "changes" && conflict() === target;
      if (!allowed()) return;
      readingHead.set(true); headError.set("");
      try {
        const head = await client.getPage(target.pageId, signal);
        if (!allowed()) return;
        if (!scopeVisible(head) || head.id !== target.pageId || head.space_id !== draft()?.space_id) { missing(); return; }
        if (head.head_version < target.version) { headError.set("Metro did not return the current version. Try again."); return; }
        currentHead.set(head);
      } catch (e) {
        if (!allowed()) return;
        if (isPageStatus(e, 404) || isPageStatus(e, 403)) missing();
        else headError.set(pagesProblem(e));
      } finally { if (allowed()) readingHead.set(false); }
    });
  }
  let oldGrants: ReturnType<typeof spaceGrants> | undefined, oldPreview: Draft | null = null, wasVisible = false;
  effect(() => {
    const grants = spaceGrants(), shown = visible() && available(), review = draft();
    const checkingAccess = applyAccess() === "checking";
    const toPreview = shown && !checkingAccess && tab() === "preview" && review?.status === "ready" && review.spec ? review : null;
    const toCompare = shown && !checkingAccess && tab() === "changes" && !!conflict();
    const changed = !!oldGrants && !sameSpaceGrants(oldGrants, grants);
    const show = shown && !wasVisible;
    const previewChanged = oldPreview !== toPreview || changed;
    oldGrants = grants; wasVisible = shown; oldPreview = toPreview;
    untracked(() => {
      if (!shown || changed) draftReads.cancel();
      if (shown && checkingAccess) void load(true);
      else if ((show || changed) && shown && generating()) void load();
      if (previewChanged || !shown) { previewReads.cancel(); data.set(null); previewing.set(false); previewError.set(""); }
      if (toPreview && previewChanged) void preview(toPreview);
      if (!toCompare || changed) { headReads.cancel(); readingHead.set(false); }
      if (toCompare) void loadConflictHead();
    });
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
    if (!client || !visible() || !available() || review?.status !== "ready" || stale() || busy() || applyAccess() !== "allowed") return;
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
      if (isPageStatus(e, 403) && review.page_id) {
        // Loss of edit rights isn't loss of the private draft. Only show it
        // again after the author-only read verifies live view access.
        applyAccess.set("checking");
      } else if (isPageStatus(e, 409) && Number.isSafeInteger(detail.head_version) && review.page_id) {
        stale.set(true);
        error.set("This draft is based on an older version. Nothing was changed. Compare with the current source or edit the prompt and generate a new draft.");
        conflict.set({ pageId: review.page_id, version: detail.head_version! });
        tab.set("changes");
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
    if (!client || !review || !available() || !visible() || busy() || applyAccess() !== "allowed") return;
    busy.set(true); error.set("");
    try {
      const next = await client.createPageDraft(review.page_id ? { page_id: review.page_id, block_key: review.block_key ?? undefined } : targetOf(review), prompt, choice);
      if (!available() || !scopeVisible(next) || next.space_id !== review.space_id || next.page_id !== review.page_id) return;
      // Retry is a new draft against the current head. Retire the old one;
      // if that fails it remains private and expires on the server.
      void client.discardPageDraft(review.id).catch(() => {});
      forgetPageDraft(review.id);
      if (route.returnRoom) {
        rememberPageDraftModel(next.id, choice);
        goPages({ ...route, id: next.id });
      } else showDraft(next, choice);
    } catch (e) {
      // A missing selected block or newly revoked edit grant must never
      // quietly widen a selected edit to the whole document.
      if (available()) { error.set(pagesProblem(e)); diagnostics.set(pageFailure(e).diagnostics ?? []); }
    } finally { if (valid()) busy.set(false); }
  }
  const changed = () => new Set([...(draft()?.changes?.added ?? []), ...(draft()?.changes?.changed ?? [])]);
  const readable = () => available() && applyAccess() !== "checking";
  const title = () => (readable() && draft()?.spec?.title) || "Page draft";
  const changeSummary = () => {
    const c = draft()?.changes;
    if (!c) return "";
    return [`${c.added.length} added`, `${c.changed.length} changed`, `${c.removed.length} removed`,
      c.data.length ? `${c.data.length} data sources changed` : "", c.page ? "Page layout or settings changed" : ""].filter(Boolean).join(" · ");
  };
  return <Column grow pad={6} gap={6} testID="page-draft-view">
    <Column gap={2}>
      <Row gap={2} wrap>
        <Action compact testID="page-draft-back" label={route.returnRoom ? "‹ Back to room" : route.returnPage ? "‹ Back to page" : "‹ Back to pages"}
          variant="ghost" disabled={busy()} onPress={back} />
        <If when={route.returnRoom || route.returnPage}><Action compact variant="ghost" label="All pages" onPress={() => goPages()} /></If>
      </Row>
      <PageHeader title={title()} subtitle="Review the draft before it changes a page." focusTitle testID="page-draft-heading">
        <Badge label={scopeName(route.spaceId)} tone="system" />
      </PageHeader>
    </Column>
    <PagesError message={error()} />
    <If when={loading()}><Label color="muted">Loading draft…</Label></If>
    <If when={applyAccess() === "checking"}>
      <If when={!checkError()}><Label color="muted">Checking whether you can still review this draft…</Label></If>
      <PagesError message={checkError()} onRetry={() => { checkError.set(""); void load(true); }} />
    </If>
    <If when={readable() ? draft() : null}>{review => <>
      <If when={applyAccess() === "denied"}>
        <Banner tone="warning" variant="inline" message="You can still review or discard this draft, but you no longer have permission to apply it." />
      </If>
      <If when={review().status === "generating"}><Label testID="page-draft-generating">Building draft… You can leave and resume under Pages → Continue reviewing.</Label></If>
      <If when={review().status === "failed"}>
        <Banner tone="danger" variant="inline" message={review().error || "Couldn’t build this draft."}
          action={applyAccess() === "allowed" ? <Action compact testID="page-retry-failed" label={retry() ? "Close prompt" : "Edit prompt and retry"} disabled={busy()}
            viewRef={node => { retryControl = node; }} onPress={() => { if (retry()) closeRetry(); else retry.set(true); }} /> : undefined} />
      </If>
      <If when={review().status !== "generating"}>
        <Row gap={2} wrap align="center">
          <SegmentedControl testID="page-draft-tab" accessibilityLabel="Draft review tab" value={tab()} onChange={tab.set}
            options={[{ value: "preview", label: "Preview" }, { value: "changes", label: "Changes" }, { value: "source", label: "Source" }]} />
          <Label variant="meta">{review().base_version ? `Based on version ${review().base_version}` : "New page"}</Label>
        </Row>
        <If when={tab() === "preview"}>
          <ScrollArea gap={2}>
            <PagesError message={previewError()} retryLabel="Retry preview" onRetry={() => void preview(review())} />
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
          <If when={!stale() || currentHead()} else={<Column gap={2}>
            <If when={readingHead()}><Label color="muted">Loading current source…</Label></If>
            <PagesError message={headError()} onRetry={() => void loadConflictHead()} />
          </Column>}>
            <PageDiff before={currentHead()?.source ?? review().base_source ?? ""} after={review().source ?? ""}
              accessibilityLabel={`${currentHead() ? `Current version ${currentHead()!.head_version}` : review().base_version ? `Published version ${review().base_version}` : "Empty page"} to draft source`} />
          </If>
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
        <If when={retry() && applyAccess() === "allowed"}>
          <ScrollArea gap={2}>
            <Label variant="strong">{review().block_key ? `Retry edit to block ${review().block_key}` : "Edit prompt and retry"}</Label>
            <DraftPrompt spaceId={review().space_id} initial={review().prompt} initialModel={pageDraftModel(review().id)}
              label="Revise the prompt" busy={busy()} focus onCancel={closeRetry} onSubmit={(prompt, choice) => void regenerate(prompt, choice)} />
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
        <If when={review().status === "ready"}>
          <Action testID="page-draft-retry" label={retry() ? "Close prompt" : "Edit prompt and retry"} variant="ghost" disabled={busy() || applyAccess() === "denied"}
            viewRef={node => { retryControl = node; }} onPress={() => { if (retry()) closeRetry(); else retry.set(true); }} />
        </If>
        <Action testID="page-apply" label="Apply" variant="primary" disabled={applyAccess() !== "allowed" || stale() || review().status !== "ready" || confirmOutside() || discard()} busy={busy()} onPress={() => void apply()} />
      </ActionBar>
    </>}</If>
  </Column>;
}
