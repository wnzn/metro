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

import { mainPlace } from "../store";
import { SearchBox, SearchFileRow, SearchMoreRow, SearchResultGlyph, SearchResultRow, SearchResultStatus } from "../ui";
import { MAX_QUERY_CHARS, searchQuery, spaceName } from "./discovery/searchState";
import { QuickAssetPreview } from "./library/QuickAssetPreview";
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

/** The file's own extension, as its name has it (".jpg", ".md"); none when the name has none. */
export function extensionOf(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 && dot < name.length - 1 ? name.slice(dot).toLowerCase() : "";
}

const fileSource = (a: LibraryAsset) => (a.spaceId === null ? "Your personal library" : spaceName(a.spaceId));
const messageSource = (m: QuickMessage) => (m.spaceId === null ? "Your personal Chat" : spaceName(m.spaceId));

export function assetMeta(a: LibraryAsset): string {
  const ext = extensionOf(a.name);
  return ext ? `${ext} · ${fileSource(a)}` : fileSource(a);
}

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
  if (row.kind === "asset") {
    return (
      <SearchFileRow
        testID={props.id}
        preview={<QuickAssetPreview asset={row.asset} />}
        label={row.asset.name}
        meta={assetMeta(row.asset)}
        selected={selected()}
        onPress={open}
      />
    );
  }
  const m = row.message;
  return (
    <SearchResultRow
      testID={props.id}
      preview={<SearchResultGlyph icon={m.spaceId === null ? "chat" : "hash"} />}
      label={m.text}
      lines={2}
      meta={`${m.actor} · ${messageSource(m)}`}
      selected={selected()}
      onPress={open}
    />
  );
}
