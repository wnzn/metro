// The search box centred at the top of the main panel, on every page of the
// signed-in app (Chat, rooms, threads, a Space's management, Scheduled tasks
// and Search itself). It is the one query field of Search: there is no
// second one on the results page.
//
// - It edits the query of the search mode chosen on the Search page: message
//   search (your Chat and the Spaces you're in) or Documents & media. It never
//   changes that mode, the scope or the filters.
// - Typing opens the Search page, where the mode, scope and filters are, and
//   sends nothing anywhere.
// - Return runs that mode's search, as the Search page's own button does: the
//   only thing that sends the query to the server (and so to the
//   searcher's embedding connection). An empty query only opens the page.
// - Focus alone changes nothing (no change of context on focus), so Tab can
//   pass through it.

import { mainPlace, openPlace } from "../store";
import { SearchBox } from "../ui";
import { libQuery, openLibrary, runLibrarySearch, searchSurface } from "./library/libraryState";
import { MAX_QUERY_CHARS, runSearch, searchQuery } from "./discovery/searchState";

const library = () => searchSurface() === "library";

function showSearch() {
  if (mainPlace() === "search") return;
  openPlace("search");
  if (library()) void openLibrary();
}

export function TopSearch() {
  const value = () => (library() ? libQuery() : searchQuery());
  const change = (text: string) => {
    if (library()) libQuery.set(text);
    else searchQuery.set(text);
    showSearch();
  };
  const submit = () => {
    showSearch();
    if (!value().trim()) return;
    if (library()) void runLibrarySearch();
    else void runSearch();
  };
  return (
    <SearchBox
      testID="top-search"
      accessibilityLabel={library() ? "Search your documents and media" : "Search messages in your Chat and Spaces"}
      placeholder={library() ? "Search documents & media" : "Search messages"}
      value={value()}
      onChange={change}
      onSubmit={submit}
      maxLength={MAX_QUERY_CHARS}
    />
  );
}
