import { cleanup, effect, lifecycle, signal, untracked } from "nsh";
import type { EditorDiagnostic } from "nsh/base";
import type { Page, PageDiagnostic, VersionMeta, Definition } from "../../api/pages";
import { isPageStatus, pageFailure, targetOf } from "../../api/pages";
import { pagesProblem } from "../../pagesState";
import { apiClient } from "../../store";
import { Action, ActionBar, Badge, Banner, Column, compactWidth, Field, Label, Modal, PageHeader, Row, ScrollArea, SegmentedControl, Surface, Toggle } from "../../ui";
import { PageCode, PageDiff } from "../../ui/pageBlocks";
import { profileName } from "../profile/profileState";
import { Diagnostics, PagesError } from "./controls";
import { PageRenderer } from "./PageRenderer";
import type { PageToolProps } from "./PageView";
import { cell } from "./renderPlan";
import { pageReadLane } from "./refresh";

function useTool(props: PageToolProps) {
  const client = apiClient(), base = props.page;
  let alive = true;
  cleanup(() => { alive = false; });
  const valid = () => alive && props.stillHere();
  const finish = (next: Page) => {
    if (valid() && next.id === base.id && next.space_id === base.space_id) props.saved(next);
  };
  return { base, client, valid, finish };
}

const stale = (error: unknown) => isPageStatus(error, 409) && Number.isSafeInteger(pageFailure(error).head_version);
const outOfDate = "The page changed. Close this editor and refresh before trying again. Nothing was overwritten.";

export function SourceEditor(props: PageToolProps) {
  const { base, client, valid, finish } = useTool(props);
  const target = signal(base), latest = signal<Page | null>(null);
  const source = signal(base.source), diagnostics = signal<PageDiagnostic[]>([]);
  const status = signal<"checking" | "valid" | "invalid">("valid");
  const error = signal(""), busy = signal(false), conflict = signal<number | null>(null);
  const headError = signal(""), reading = signal(false);
  const review = signal<"diff" | "source">("source");
  const headReads = pageReadLane<number>();
  let checked = base.source, validationTicket = 0;
  effect(() => {
    const value = source();
    const ticket = ++validationTicket;
    if (lifecycle() !== "active" || !valid()) return;
    if (value === checked) { status.set("valid"); diagnostics.set([]); return; }
    untracked(() => { status.set("checking"); diagnostics.set([]); error.set(""); });
    const controller = new AbortController();
    const timer = setTimeout(() => {
      if (!client || !valid() || ticket !== validationTicket) return;
      if (new TextEncoder().encode(value).length > 65_536) {
        error.set("Page TSX cannot be larger than 64 KiB."); status.set("invalid"); return;
      }
      void client.validatePage(targetOf(base), value, controller.signal).then(answer => {
        if (!valid() || controller.signal.aborted || ticket !== validationTicket) return;
        diagnostics.set(answer.diagnostics);
        if (answer.ok) checked = value;
        status.set(answer.ok ? "valid" : "invalid");
      }, e => {
        if (!valid() || controller.signal.aborted || ticket !== validationTicket) return;
        if (isPageStatus(e, 404) || isPageStatus(e, 403)) { props.lost(); return; }
        error.set(pagesProblem(e));
        status.set("invalid");
      });
    }, 350);
    return () => { clearTimeout(timer); controller.abort(); };
  });
  function readHead() {
    const minimum = conflict();
    if (!client || minimum === null || latest() || !valid() || lifecycle() !== "active") return;
    return headReads.run(minimum, async (signal, current) => {
      const allowed = () => current() && valid() && lifecycle() === "active" && conflict() === minimum;
      if (!allowed()) return;
      reading.set(true); headError.set("");
      try {
        const head = await client.getPage(base.id, signal);
        if (!allowed()) return;
        if (head.id !== base.id || head.space_id !== base.space_id || !!head.deleted_at) { props.lost(); return; }
        if (head.head_version < minimum) { headError.set("Metro did not return the current version. Try again."); return; }
        latest.set(head);
      } catch (e) {
        if (!allowed()) return;
        if (isPageStatus(e, 404) || isPageStatus(e, 403)) props.lost();
        else headError.set(pagesProblem(e));
      } finally { if (allowed()) reading.set(false); }
    });
  }
  effect(() => {
    const wanted = conflict() !== null && lifecycle() === "active" && valid() && review() === "diff";
    untracked(() => {
      if (wanted) void readHead();
      else { headReads.cancel(); reading.set(false); }
    });
  });
  async function save() {
    const against = target(), edited = source();
    if (!client || !valid() || !against.can_edit || busy() || conflict() !== null || status() !== "valid" || edited !== checked || edited === against.source) return;
    busy.set(true); error.set("");
    try { finish(await client.savePage(base.id, against.head_version, edited)); }
    catch (e) {
      if (!valid()) return;
      if (isPageStatus(e, 404)) { props.lost(); return; }
      if (stale(e)) {
        latest.set(null);
        conflict.set(pageFailure(e).head_version!);
        review.set("diff");
      } else error.set(pagesProblem(e));
      diagnostics.set(pageFailure(e).diagnostics ?? []);
    } finally { if (valid()) busy.set(false); }
  }
  function useReviewedBase() {
    const head = latest();
    if (!head?.can_edit || busy() || !valid() || conflict() === null || review() !== "diff") return;
    target.set(head);
    conflict.set(null);
    review.set("source");
  }
  const codeDiagnostics = (): EditorDiagnostic[] => diagnostics().flatMap(d => {
    if (!Number.isSafeInteger(d.line) || d.line < 1 || !Number.isSafeInteger(d.column) || d.column < 1) return [];
    const start = { line: d.line - 1, column: d.column - 1 };
    return [{ range: { start, end: { ...start, column: start.column + 1 } }, message: d.message, severity: "error" }];
  });
  return <Modal open testID="page-source-sheet" title="Edit page source" onClose={() => { if (!busy()) props.close(); }}>
    <Column grow pad={compactWidth() ? 4 : 6} gap={4}>
      <PageHeader title="Edit page source" subtitle={`Page TSX · based on version ${target().head_version} · checked by Metro, never run as code.`} />
      <If when={conflict() !== null}>
        <Banner tone="warning" variant="inline" title="The published page changed"
          message="Your unsaved source is still here. Review the current version and merge any changes you want to keep. Nothing was overwritten." />
        <SegmentedControl testID="page-source-conflict-tab" accessibilityLabel="Review source conflict" value={review()} onChange={review.set}
          options={[{ value: "diff", label: "Compare" }, { value: "source", label: "Your source" }]} />
      </If>
      <If when={review() === "diff" && conflict() !== null} else={
        <PageCode value={source()} onChange={source.set} readOnly={busy()} diagnostics={codeDiagnostics()} />
      }>
        <If when={latest()} else={<Column gap={2}>
          <If when={reading()}><Label color="muted">Loading current source…</Label></If>
          <PagesError message={headError()} onRetry={() => void readHead()} />
        </Column>}>
          {head => <>
            <Label variant="caption" color="muted">{`Published version ${head().head_version} → your unsaved source`}</Label>
            <PageDiff before={head().source} after={source()} />
            <If when={!head().can_edit}><Label color="warning">You can no longer edit this page.</Label></If>
            <If when={head().can_edit}>
              <Label variant="caption" color="muted">Using this base keeps your source as-is. Saving later replaces the published source; Metro does not merge the edits.</Label>
              <Row justify="end"><Action testID="page-use-current-base"
                label={`Use version ${head().head_version} as base`} onPress={useReviewedBase} /></Row>
            </If>
          </>}
        </If>
      </If>
      <If when={status() === "checking"}><Label color="muted">Checking Page TSX…</Label></If>
      <If when={diagnostics().length}><ScrollArea><Diagnostics items={diagnostics()} /></ScrollArea></If>
      <PagesError message={error()} />
      <ActionBar><Action label="Close" variant="ghost" disabled={busy()} onPress={props.close} />
        <Action testID="page-save-source" label="Save source" variant="primary" busy={busy()}
          disabled={!target().can_edit || source() === target().source || status() !== "valid" || conflict() !== null} onPress={() => void save()} /></ActionBar>
    </Column>
  </Modal>;
}

export function PageHistory(props: PageToolProps) {
  const { base, client, valid, finish } = useTool(props);
  const history = signal<VersionMeta[]>([]), head = signal<Page>(base);
  const version = signal<(VersionMeta & Definition) | null>(null);
  const tab = signal<"preview" | "changes" | "source">("preview");
  const busy = signal(false), loading = signal(true), error = signal(""), confirm = signal(false);
  let seq = 0;
  async function load(sync = false) {
    if (!client || !valid()) return;
    const id = ++seq;
    error.set(""); loading.set(true);
    try {
      const list = await client.pageVersions(base.id);
      if (!valid() || id !== seq) return;
      if (sync || list.head_version !== head().head_version) {
        const next = await client.getPage(base.id);
        if (!valid() || id !== seq || next.space_id !== base.space_id || next.id !== base.id) return;
        head.set(next);
      }
      history.set(list.versions);
    } catch (e) {
      if (valid() && id === seq) {
        if (isPageStatus(e, 404) || isPageStatus(e, 403)) props.lost();
        else error.set(pagesProblem(e));
      }
    } finally { if (valid() && id === seq) loading.set(false); }
  }
  async function inspect(n: number) {
    if (!client || !valid() || busy()) return;
    const id = ++seq;
    loading.set(true); error.set(""); confirm.set(false); version.set(null);
    try {
      const detail = await client.pageVersion(base.id, n);
      if (valid() && id === seq && detail.version === n) version.set(detail);
    } catch (e) { if (valid() && id === seq) error.set(pagesProblem(e)); }
    finally { if (valid() && id === seq) loading.set(false); }
  }
  async function restore() {
    const selected = version();
    if (!client || !selected || !valid() || !head().can_edit || busy() || loading() || !confirm()) return;
    confirm.set(false); busy.set(true); error.set("");
    try { finish(await client.restorePageVersion(base.id, selected.version, head().head_version)); }
    catch (e) {
      if (!valid()) return;
      if (stale(e)) {
        await load(true);
        if (valid()) error.set("The page changed. Review the current version before restoring. Nothing was overwritten.");
      } else error.set(pagesProblem(e));
    } finally { if (valid()) busy.set(false); }
  }
  untracked(() => void load());
  return <Modal open title="Page history" testID="page-history-sheet" onClose={() => { if (!busy()) props.close(); }}>
    <Column grow pad={compactWidth() ? 4 : 6} gap={4}>
      <PageHeader title="Page history" subtitle="Restoring makes a new version instead of changing history." />
      <PagesError message={error()} />
      <If when={loading()}><Label color="muted">Loading history…</Label></If>
      <If when={version()} else={<ScrollArea gap={2}>
        <Each of={history()} key={v => v.version}>{v =>
          <Surface variant="card" pad={3} gap={1}>
            <Row gap={2} align="center" justify="between" wrap>
              <Column gap={1} grow>
                <Label variant="strong">Version {v().version} · {versionKind(v())}</Label>
                <Label variant="meta">{profileName(v().author_id) ?? v().author_id} · {cell(v().created_at, "datetime").label}</Label>
                <If when={v().prompt}><Label lines={2} color="muted">{v().prompt}</Label></If>
              </Column>
              <If when={v().version === head().head_version}><Badge tone="system" label="Current" /></If>
              <Action compact label="Review" accessibilityLabel={`Review version ${v().version}`} testID={`page-version:${v().version}`} onPress={() => void inspect(v().version)} />
            </Row>
          </Surface>
        }</Each>
      </ScrollArea>}>{v => <>
        <Row gap={2} wrap align="center">
          <Action compact label="‹ All versions" variant="ghost" disabled={busy()} onPress={() => { confirm.set(false); version.set(null); }} />
          <SegmentedControl testID="page-history-tab" accessibilityLabel="Review version" value={tab()} onChange={tab.set}
            options={[{ value: "preview", label: "Preview" }, { value: "changes", label: "Changes" }, { value: "source", label: "Source" }]} />
        </Row>
        <If when={tab() === "preview"}><ScrollArea gap={2}>
          <Label variant="caption" color="muted">Historical layout only. Live data and actions aren’t available in this preview.</Label>
          <PageRenderer spec={v().spec} data={null} readOnly />
        </ScrollArea></If>
        <If when={tab() === "changes"}><PageDiff before={v().source} after={head().source} /></If>
        <If when={tab() === "source"}><PageCode value={v().source} readOnly /></If>
        <If when={head().can_edit && v().version !== head().head_version}>
          <If when={confirm()} else={<Action testID="page-restore-version" label="Restore this version…" onPress={() => confirm.set(true)} disabled={busy()} />}>
            <Banner tone="warning" variant="inline" title={`Restore version ${v().version}?`} message="Current content will be replaced in a new version." />
            <ActionBar><Action label="Cancel" variant="ghost" onPress={() => confirm.set(false)} />
              <Action testID="page-restore-now" label="Restore version" variant="primary" busy={busy()} onPress={() => void restore()} /></ActionBar>
          </If>
        </If>
      </>}</If>
      <ActionBar><Action label="Done" onPress={props.close} disabled={busy()} /></ActionBar>
    </Column>
  </Modal>;
}

function versionKind(v: VersionMeta) {
  switch (v.kind) {
    case "create": return "Created";
    case "manual": return "Source edit";
    case "prompt": return "Prompt edit";
    case "restore": return `Restored from ${v.restored_from ?? "history"}`;
    case "template": return "Created from template";
    case "rename": return "Renamed";
    default: return "Saved";
  }
}

export function PageSettings(props: PageToolProps) {
  const { base, client, valid, finish } = useTool(props);
  const title = signal(base.title), icon = signal(base.icon ?? ""), members = signal(base.members_edit);
  const busy = signal(false), error = signal(""), conflict = signal(false);
  const changed = () => (base.can_edit && title().trim() !== base.title)
    || (base.can_manage && (icon().trim() !== (base.icon ?? "") || (base.space_id && members() !== base.members_edit)));
  async function save() {
    if (!client || !valid() || busy() || conflict() || !changed() || !title().trim()) return;
    busy.set(true); error.set("");
    try {
      finish(await client.patchPage(base.id, {
        ...(base.can_edit && title().trim() !== base.title ? { title: title().trim(), base_version: base.head_version } : {}),
        ...(base.can_manage && icon().trim() !== (base.icon ?? "") ? { icon: icon().trim() || null } : {}),
        ...(base.can_manage && base.space_id && members() !== base.members_edit ? { members_edit: members() } : {}),
      }));
    } catch (e) {
      if (!valid()) return;
      if (isPageStatus(e, 404)) { props.lost(); return; }
      conflict.set(stale(e));
      error.set(conflict() ? outOfDate : pagesProblem(e));
    } finally { if (valid()) busy.set(false); }
  }
  return <Modal open size="compact" title="Page settings" testID="page-settings-sheet" onClose={() => { if (!busy()) props.close(); }}>
    <Column pad={4} gap={3}>
      <PageHeader title="Page settings" />
      <If when={base.can_edit}>
        <Column gap={1}>
          <Label variant="strong">Title</Label>
          <Field accessibilityLabel="Page title" testID="page-setting-title" value={title()} onChange={title.set} maxLength={120} disabled={busy()} />
        </Column>
      </If>
      <If when={base.can_manage}>
        <Column gap={1}>
          <Label variant="strong">Icon (optional)</Label>
          <Field accessibilityLabel="Page icon" testID="page-setting-icon" value={icon()} onChange={icon.set} placeholder="Emoji or short text" maxLength={16} disabled={busy()} />
        </Column>
        <If when={base.space_id}>
          <Toggle label="Members can edit" checked={members()} onChange={members.set} testID="page-members-edit" disabled={busy()} />
          <Label variant="caption" color="muted">When off, only this page’s creator and the Space owner can edit.</Label>
        </If>
      </If>
      <PagesError message={error()} />
      <ActionBar><Action label="Cancel" variant="ghost" disabled={busy()} onPress={props.close} />
        <Action label="Save settings" testID="page-save-settings" variant="primary" disabled={!changed() || !title().trim() || conflict()}
          busy={busy()} onPress={() => void save()} /></ActionBar>
    </Column>
  </Modal>;
}
