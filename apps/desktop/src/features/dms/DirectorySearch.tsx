import { cleanup, effect, signal, untracked } from "nsh";
import type { DirectoryPerson } from "../../api/directory";
import { ApiError, apiClient, isAborted, me, readHostedAccess } from "../../store";
import { Action, Column, Field, Label, NavItem, Row } from "../../ui";
import { PersonAvatar } from "../profile/PersonAvatar";

const DEBOUNCE_MS = 350;
const MAX_RESULTS = 60;

export function DirectorySearch(props: {
  placeholder: string; onPick: (person: DirectoryPerson) => void; onCancel?: () => void;
  disabled?: boolean; selectedId?: string; autoFocus?: boolean;
}) {
  const query = signal("");
  const retry = signal(0);
  const users = signal<DirectoryPerson[]>([]);
  const next = signal<string | null>(null);
  const busy = signal(true);
  const problem = signal("");
  let controller: AbortController | undefined;
  let shownQuery: string | undefined;
  const term = () => [...query().trim()].slice(0, 64).join("");
  const load = async (run: AbortController, q: string, cursor: string | null) => {
    const api = apiClient();
    if (!api || run.signal.aborted) return;
    busy.set(true);
    problem.set("");
    try {
      const page = await readHostedAccess(api, () => api.searchDirectory(q, cursor, run.signal));
      if (run.signal.aborted || apiClient() !== api || term() !== q) return;
      const rows = new Map((cursor ? users() : []).map(p => [p.id, p]));
      for (const p of page.users) if (p.id !== me()) rows.set(p.id, p);
      users.set([...rows.values()].slice(0, MAX_RESULTS));
      next.set(rows.size < MAX_RESULTS && page.nextCursor !== cursor ? page.nextCursor : null);
      shownQuery = q;
    } catch (error) {
      if (!run.signal.aborted && apiClient() === api && !isAborted(error)) {
        problem.set(error instanceof ApiError && error.status === 429
          ? "Too many searches. Wait a moment, then try again." : "Couldn't search people. Try again.");
      }
    } finally {
      if (!run.signal.aborted && apiClient() === api) busy.set(false);
    }
  };
  effect(() => {
    const q = term();
    retry();
    controller?.abort();
    const run = controller = new AbortController();
    shownQuery = undefined;
    users.set([]);
    next.set(null);
    busy.set(true);
    problem.set("");
    const timer = setTimeout(() => void load(run, q, null), DEBOUNCE_MS);
    return () => { clearTimeout(timer); run.abort(); };
  });
  cleanup(() => controller?.abort());
  const pick = (person: DirectoryPerson | undefined) => {
    if (person && !props.disabled && shownQuery === term()) props.onPick(person);
  };
  return <Column gap={1} testID="people-picker">
    <Field testID="directory-search" value={query()} onChange={query.set} placeholder={props.placeholder}
      autoFocus={props.autoFocus} disabled={props.disabled} maxLength={64} verbatim keepFocusOnSubmit
      onSubmit={() => { if (!busy()) pick(users().find(p => p.id === term()) ?? users()[0]); }} onCancel={props.onCancel} />
    <If when={problem()}>
      <Label color="warning" variant="caption" testID="directory-error">{problem()}</Label>
      <Row><Action compact label="Retry search" variant="ghost" disabled={props.disabled} onPress={() => retry.set(untracked(retry) + 1)} /></Row>
    </If>
    <Each of={users()} key={person => person.id}>
      {person => <NavItem testID={`directory-person:${person().id}`}
        label={users().some(p => p.id !== person().id && p.name === person().name) ? `${person().name} · ${person().id}` : person().name}
        accessibilityLabel={`${person().name}, ${person().id}`}
        tooltip={person().id} selected={props.selectedId === person().id} disabled={props.disabled} onPress={() => pick(person())}
        avatar={<PersonAvatar userId={person().id} name={person().name} verifiedName size="sm" />} />}
    </Each>
    <If when={busy()}><Label variant="caption" color="muted">Searching people…</Label></If>
    <If when={!busy() && !problem() && !users().length}>
      <Label variant="caption" color="muted">No people match</Label>
    </If>
    <If when={!busy() && !problem() && next()}>
      <Row><Action compact label="More people" variant="ghost" disabled={props.disabled}
        onPress={() => { if (controller && next() && !busy()) void load(controller, term(), next()); }} /></Row>
    </If>
  </Column>;
}
