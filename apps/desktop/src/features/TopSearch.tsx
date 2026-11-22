// The search box centred at the top of the main panel, on every page of the
// signed-in app. It is the one query field of Search: there is no second one
// on the results page.
//
// - It edits the query of the current search mode: message search (your
//   Chat and the Spaces you're in) or Documents & media. On the Library page
//   it is the Documents & media query. It never changes the scope or filters.
// - Typing only edits the query: it opens nothing and sends nothing.
// - Return opens Search in that mode and runs its search: the only thing
//   that sends the query to the server (and so to the searcher's embedding
//   or media connection). An empty query only opens the page.
// - Focus alone changes nothing (no change of context on focus), so Tab can
//   pass through it.

import { mainPlace, openPlace } from "../store";
import { SearchBox } from "../ui";
import { libQuery, runLibrarySearch, searchSurface } from "./library/libraryState";
import { MAX_QUERY_CHARS, runSearch, searchQuery } from "./discovery/searchState";

const library = () => mainPlace() === "library" || searchSurface() === "library";

export function TopSearch() {
  const value = () => (library() ? libQuery() : searchQuery());
  const change = (text: string) => {
    if (library()) libQuery.set(text);
    else searchQuery.set(text);
  };
  const submit = () => {
    const files = library();
    if (files) searchSurface.set("library");
    if (mainPlace() !== "search") openPlace("search");
    if (!value().trim()) return;
    if (files) void runLibrarySearch();
    else void runSearch();
  };
  return (
    <SearchBox
      testID="top-search"
      accessibilityLabel={library() ? "Search your documents and media; Return searches" : "Search messages in your Chat and Spaces; Return searches"}
      placeholder={library() ? "Search documents & media" : "Search messages"}
      value={value()}
      onChange={change}
      onSubmit={submit}
      maxLength={MAX_QUERY_CHARS}
    />
  );
}
