// The search box centred at the top of the main panel, on every page of the
// signed-in app. It is the one query field of Search: there is no second one
// on the results page.
//
// - It edits the query of the current search mode: message search (your
//   Chat and the Spaces you're in) or Documents & media. On the Library page
//   it is the Documents & media query. It never changes the scope or filters.
// - Typing shows quick results under it (./quickSearch): literal matches
//   Metro looks up itself, with no model and no indexing. A row opens its
//   message or file; "More results", always last, opens full Search.
// - Return with no row highlighted opens Search in that mode and runs its
//   search, as before: only that (or More results) sends the query on to the
//   searcher's embedding or media connection. An empty query only opens the page.
// - Focus alone changes nothing (no change of context on focus), so Tab can
//   pass through it.

import { computed } from "nsh";
import { mainPlace } from "../store";
import { SearchBox, SearchFileRow, SearchGroupHeader, SearchMoreRow, SearchResultGlyph, SearchResultRow, SearchResultStatus } from "../ui";
import { MAX_QUERY_CHARS, searchQuery, spaceName } from "./discovery/searchState";
import { previewLabel, QuickPreviewDetail, QuickPreviewTile, useQuickPreview } from "./library/QuickAssetPreview";
import { libQuery, searchSurface } from "./library/libraryState";
import type { LibraryAsset } from "./library/libraryState";
import {
  boxQuery,
  quickActivate,
  quickClose,
  quickContext,
  quickEdited,
  quickFocus,
  quickMove,
  quickOpen,
  quickRows,
  quickSelected,
  quickState,
  quickSubmit,
} from "./quickSearch";
import type { QuickMessage, QuickRow } from "./quickSearch";

const library = () => mainPlace() === "library" || searchSurface() === "library";

/** The file's own extension, exactly as its name has it (".jpg", ".JPG", ".md"); none when the name has none. */
export function extensionOf(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 && dot < name.length - 1 ? name.slice(dot) : "";
}

/** The name without its extension, for the row's title (the extension is its badge). */
export function baseNameOf(name: string): string {
  const ext = extensionOf(name);
  return ext ? name.slice(0, name.length - ext.length) : name;
}

/** Where a file is, in full (screen readers), and short (the row, only when needed). */
const fileSource = (a: LibraryAsset) => (a.spaceId === null ? "Your personal library" : spaceName(a.spaceId));
const shortFileSource = (a: LibraryAsset) => (a.spaceId === null ? "Personal" : spaceName(a.spaceId));
const messageSource = (m: QuickMessage) => (m.spaceId === null ? "Chat" : spaceName(m.spaceId));
const messageSourceFull = (m: QuickMessage) => (m.spaceId === null ? "your personal Chat" : spaceName(m.spaceId));

/** Names shown by more than one file row now: those rows say where each file is. */
const sharedNames = computed(() => {
  const seen = new Map<string, number>();
  for (const r of quickRows()) if (r.kind === "asset") seen.set(r.asset.name, (seen.get(r.asset.name) ?? 0) + 1);
  return new Set([...seen].filter(([, n]) => n > 1).map(([name]) => name));
});

/** Files and messages both found: each group gets a heading. */
const mixed = computed(() => {
  const rows = quickRows();
  return rows.some((r) => r.kind === "asset") && rows.some((r) => r.kind === "message");
});

/** Where the lookup looks, said in the list. */
function scopeText(): string {
  const ctx = quickContext();
  const s = ctx.scope;
  const where = "spaceId" in s ? spaceName(s.spaceId) : s.kind === "all" ? "everything you can read" : s.kind === "chat" ? "your personal Chat" : "your personal library";
  const what = ctx.mode === "all" ? "Files and messages" : ctx.mode === "library" ? "Files" : "Messages";
  return `${what} in ${where}`;
}

export function TopSearch() {
  const change = (text: string) => {
    if (library()) libQuery.set(text);
    else searchQuery.set(text);
    quickEdited(text);
  };
  const mode = () => quickContext().mode;
  return (
    <SearchBox
      testID="top-search"
      accessibilityLabel={
        mode() === "all"
          ? "Search files and messages in your Chat, library and Spaces; Return searches"
          : mode() === "library"
            ? "Search your documents and media; Return searches"
            : "Search messages in your Chat and Spaces; Return searches"
      }
      placeholder={mode() === "all" ? "Search files and messages" : mode() === "library" ? "Search documents & media" : "Search messages"}
      value={boxQuery()}
      onChange={change}
      onSubmit={quickSubmit}
      onFocus={() => quickFocus(true)}
      onBlur={() => quickFocus(false)}
      maxLength={MAX_QUERY_CHARS}
      popup={{
        open: quickOpen(),
        onClose: quickClose,
        onMove: quickMove,
        testID: "quick-results",
        accessibilityLabel: `Quick results: ${scopeText()}`,
        children: (
          <>
            <QuickStatusLine />
            <Each of={quickRows()} key={rowID}>
              {(row, index) => <Row row={row()} index={index()} id={rowID(row())} />}
            </Each>
          </>
        ),
      }}
    />
  );
}

/**
 * A row's key and testID. Message ids are per place (the personal Chat and
 * each Space number their own events), so a message is named by its Space,
 * room and event: `quick-message:<spaceId|chat>:<roomId|chat>:<eventId>`.
 */
export function rowID(row: QuickRow): string {
  if (row.kind === "asset") return `quick-asset:${row.asset.id}`;
  if (row.kind === "more") return "quick-more";
  const m = row.message;
  return `quick-message:${m.spaceId ?? "chat"}:${m.roomId ?? "chat"}:${m.eventId}`;
}

function QuickStatusLine() {
  const line = (): { text: string; tone?: "danger"; id: string } | null => {
    const s = quickState();
    if (s.status === "loading") return { text: "Looking for matches…", id: "quick-loading" };
    if (s.status === "unavailable") return { text: "Quick results aren't available on this server. More results searches in full.", id: "quick-unavailable" };
    if (s.status === "error") return { text: s.message, tone: "danger", id: "quick-error" };
    if (s.status === "too-long") return { text: "Open full search for longer queries.", id: "quick-too-long" };
    return quickRows().length === 1 ? { text: `No quick matches. ${scopeText()}.`, id: "quick-empty" } : null;
  };
  return <If when={line()}>{(l) => <SearchResultStatus testID={l().id} text={l().text} tone={l().tone} />}</If>;
}

function Row(props: { row: QuickRow; index: number; id: string }) {
  const selected = () => quickSelected() === props.index;
  const open = () => quickActivate(props.row);
  const row = props.row;
  if (row.kind === "more") return <SearchMoreRow testID={props.id} label="More results" accessibilityLabel="More results in full Search" selected={selected()} onPress={open} />;
  // A heading before the first of each kind, only when both kinds are listed. Not a row: Up and Down skip it.
  const first = () => {
    const rows = quickRows();
    const at = props.index;
    return at === 0 || rows[at - 1]?.kind !== row.kind;
  };
  const header = () => (mixed() && first() ? (row.kind === "asset" ? "Files" : "Messages") : null);
  return (
    <>
      <If when={header()}>{(h) => <SearchGroupHeader testID={`quick-group:${row.kind === "asset" ? "files" : "messages"}`} label={h()} />}</If>
      {row.kind === "asset" ? <FileResult asset={row.asset} id={props.id} selected={selected()} onPress={open} /> : <MessageResult message={row.message} id={props.id} selected={selected()} onPress={open} />}
    </>
  );
}

/** One file: its tile and detail from one preview, its name, its extension badge, its source when two share a name. */
function FileResult(props: { asset: LibraryAsset; id: string; selected: boolean; onPress: () => void }) {
  const preview = useQuickPreview(() => props.asset);
  const a = props.asset;
  const label = () => `${a.name}, ${fileSource(a)}. ${previewLabel(a.name, preview())}`;
  return (
    <SearchFileRow
      testID={props.id}
      accessibilityLabel={label()}
      tile={<QuickPreviewTile asset={a} preview={preview} />}
      title={baseNameOf(a.name)}
      detail={<QuickPreviewDetail asset={a} preview={preview} />}
      badge={extensionOf(a.name)}
      source={sharedNames().has(a.name) ? shortFileSource(a) : undefined}
      selected={props.selected}
      onPress={props.onPress}
    />
  );
}

/** One message: a glyph of its place, one line of its text, then who and where. */
function MessageResult(props: { message: QuickMessage; id: string; selected: boolean; onPress: () => void }) {
  const m = props.message;
  return (
    <SearchResultRow
      testID={props.id}
      accessibilityLabel={`${m.text}. By ${m.actor} in ${messageSourceFull(m)}`}
      preview={<SearchResultGlyph icon={m.spaceId === null ? "chat" : "hash"} />}
      label={m.text}
      meta={`${m.actor} · ${messageSource(m)}`}
      selected={props.selected}
      onPress={props.onPress}
    />
  );
}
