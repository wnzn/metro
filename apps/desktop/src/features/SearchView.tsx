// Search: by meaning, in two modes.
//
// - Conversations: the user's own personal Chat and the rooms and threads of
//   Spaces they're a member of now. It says how results were ordered (System
//   One rerank or similarity only) and when indexing is incomplete, and it
//   never shows a result from a Space the user has left.
// - Documents & media (./library): the separate corpus of files people
//   uploaded.
//
// The query is typed only in the search box at the top (TopSearch). Return
// there is the only thing that sends it anywhere; typing, changing a mode or a
// filter sends nothing.

import { openSettings, roomsOf, spaces } from "../store";
import { Action, Badge, Banner, Chip, Column, Disclosure, Label, MarkedText, PageHeader, ResultCard, Row, ScrollArea, SegmentedControl, SettingRow, SettingsGroup, Surface } from "../ui";
import { ConnectionBanner } from "./Conversation";
import { LibrarySearch } from "./library/LibraryView";
import { searchSurface } from "./library/libraryState";
import { rankingText } from "./discovery/present";
import {
  hiddenHits,
  indexError,
  indexing,
  indexMore,
  indexProgress,
  lessRelevantHits,
  lessRelevantOpen,
  markTerms,
  openError,
  openHit,
  relevantHits,
  scopeLabel,
  searchAnswer,
  searchError,
  searching,
  searchScope,
  setScope,
  spaceName,
  visibleHits,
} from "./discovery/searchState";
import type { SearchHit, SearchScope } from "./discovery/searchState";

const scopeValue = (scope: SearchScope) => (scope.kind === "space" ? `space:${scope.spaceId}` : scope.kind);

function scopeFrom(value: string | null): SearchScope {
  if (value === "chat") return { kind: "chat" };
  if (value?.startsWith("space:")) return { kind: "space", spaceId: value.slice("space:".length) };
  return { kind: "all" };
}

const MODES = [
  { value: "conversations", label: "Conversations", description: "Messages in your personal Chat and your Spaces" },
  { value: "library", label: "Documents & media", description: "Files people uploaded" },
] as const;

export function SearchView() {
  return (
    <Surface variant="panel" grow testID="search-view">
      <ConnectionBanner />
      <ScrollArea pad={6}>
        <Surface variant="content" gap={6}>
          <PageHeader
            title="Search"
            subtitle={
              searchSurface() === "library"
                ? "By meaning, across your personal library and the files members uploaded to Spaces you're in."
                : "By meaning, across your personal Chat and the rooms and threads of Spaces you're a member of. Runner files and tool output aren't searched."
            }
          />
          <SegmentedControl testID="search-surface" accessibilityLabel="Search mode" options={MODES} value={searchSurface()} onChange={(v) => searchSurface.set(v)} />
          <If when={searchSurface() === "library"} else={<ConversationSearch />}>
            <LibrarySearch />
          </If>
        </Surface>
      </ScrollArea>
    </Surface>
  );
}

function ConversationSearch() {
  return (
    <Column gap={6}>
      <Filters />
      <Messages />
      <Results />
    </Column>
  );
}

function Filters() {
  const scopes = () => [
    { value: "all", label: "Everything" },
    { value: "chat", label: "Personal Chat" },
    ...spaces().map((s) => ({ value: `space:${s.id}`, label: s.name })),
  ];
  return (
    <Column gap={3} testID="search-form">
      <Row align="center" gap={2} wrap testID="search-scope" accessibilityLabel="Search in">
        <Label variant="caption" color="muted">
          In
        </Label>
        <Each of={scopes()} key={(s) => s.value}>
          {(s) => (
            <Chip
              testID={`search-scope:${s().value}`}
              label={s().label}
              accessibilityLabel={`Search in ${s().label}`}
              selected={scopeValue(searchScope()) === s().value}
              onPress={() => setScope(scopeFrom(s().value))}
            />
          )}
        </Each>
      </Row>
      <Label variant="caption" color="muted" testID="search-disclosure">
        {searching()
          ? "Searching…"
          : "Return in the search box at the top sends your query to your embeddings connection, and, if you have a System One connection, a short list of matching messages you can read to it for reranking. Typing or changing a filter sends nothing."}
      </Label>
    </Column>
  );
}

function Messages() {
  return (
    <Column gap={2}>
      <If when={searchError()}>
        {(e) => (
          <Banner tone="danger" testID="search-error">
            <Column grow gap={1}>
              <Label color="danger">{e().message}</Label>
            </Column>
            <If when={e().needsEmbeddings}>
              <Action testID="search-open-providers" label="Open Model providers" onPress={() => openSettings("providers")} />
            </If>
          </Banner>
        )}
      </If>
      <If when={openError()}>
        {(message) => (
          <Banner tone="warning" testID="search-open-error">
            <Label color="warning">{message()}</Label>
          </Banner>
        )}
      </If>
    </Column>
  );
}

function whereText(hit: SearchHit): string {
  if (hit.spaceId === null || hit.roomId === null) return "Personal Chat";
  const room = roomsOf(hit.spaceId).find((r) => r.id === hit.roomId);
  return `${spaceName(hit.spaceId)} › #${room?.name ?? hit.roomId}`;
}

const hitKey = (hit: SearchHit) => `${hit.spaceId ?? "chat"}:${hit.roomId ?? "chat"}:${hit.eventId}`;

function Results() {
  return (
    <If when={searchAnswer()}>
      {(answer) => (
        <Column gap={3} testID="search-results">
          <Row align="center" gap={2} wrap>
            <Label variant="heading" heading testID="search-summary">
              {`${visibleHits().length} ${visibleHits().length === 1 ? "result" : "results"} for “${answer().query}” · ${scopeLabel(answer().scope)}`}
            </Label>
            <Badge testID="search-ranking" tone={rankingText(answer().ranking).tone} label={rankingText(answer().ranking).label} />
          </Row>
          <Label variant="caption" color="muted" testID="search-ranking-explain">
            {rankingText(answer().ranking).explain}
          </Label>
          <If
            when={answer().partial}
            else={
              <If when={answer().notice}>
                {(n) => (
                  <Label variant="caption" color="muted" testID="search-notice">
                    {n()}
                  </Label>
                )}
              </If>
            }
          >
            <Banner tone="warning" testID="search-partial">
              <Column grow gap={1}>
                <Label variant="strong" color="warning">
                  Incomplete: not everything was searched.
                </Label>
                <Label variant="caption" color="warning">
                  {answer().notice ?? "Some messages aren't indexed yet. Index more, then search again."}
                </Label>
                <IndexProgressLine />
              </Column>
              <Action testID="search-index-more" label="Index more" busyLabel="Indexing…" busy={indexing()} disabled={indexProgress()?.done === true} onPress={() => void indexMore()} />
            </Banner>
          </If>
          <If when={hiddenHits() > 0}>
            <Label variant="caption" color="muted" testID="search-hidden">
              {`${hiddenHits()} ${hiddenHits() === 1 ? "result" : "results"} from a Space you're no longer a member of aren't shown.`}
            </Label>
          </If>
          <If when={visibleHits().length === 0}>
            <SettingsGroup testID="search-empty">
              <SettingRow
                label={answer().partial ? "No matches in what's indexed so far" : "No matches"}
                description={answer().partial ? "Index more and search again, or try other words." : "Try other words, or search everything you can read."}
              />
            </SettingsGroup>
          </If>
          <Each of={relevantHits()} key={hitKey}>
            {(hit) => <MessageResult hit={hit()} query={answer().query} />}
          </Each>
          <If when={lessRelevantHits().length > 0}>
            <Disclosure
              testID="search-less-relevant"
              title={`Less relevant (${lessRelevantHits().length})`}
              summary="No measurable similarity to your query"
              accessibilityLabel={`Less relevant: ${lessRelevantHits().length} ${lessRelevantHits().length === 1 ? "result" : "results"} with no measurable similarity to your query`}
              open={lessRelevantOpen()}
              onToggle={(open) => lessRelevantOpen.set(open)}
            >
              <If when={lessRelevantOpen()}>
                <Column gap={3}>
                  <Each of={lessRelevantHits()} key={hitKey}>
                    {(hit) => <MessageResult hit={hit()} query={answer().query} />}
                  </Each>
                </Column>
              </If>
            </Disclosure>
          </If>
        </Column>
      )}
    </If>
  );
}

function IndexProgressLine() {
  return (
    <Column gap={1}>
      <If when={indexProgress()}>
        {(p) => (
          <Label variant="caption" color="warning" testID="search-index-progress">
            {p().done
              ? `Indexing is complete (${p().indexed} more just now). Search again to include them.`
              : `Indexed ${p().indexed} more; ${p().remaining} still waiting. Index more, or search again now.`}
          </Label>
        )}
      </If>
      <If when={indexProgress()?.notice}>
        {(n) => (
          <Label variant="caption" color="warning" testID="search-index-notice">
            {n()}
          </Label>
        )}
      </If>
      <If when={indexError()}>
        {(message) => (
          <Label variant="caption" color="danger" testID="search-index-error">
            {message()}
          </Label>
        )}
      </If>
    </Column>
  );
}

/** One message; the whole card opens its conversation at that message. */
function MessageResult(props: { hit: SearchHit; query: string }) {
  const hit = () => props.hit;
  return (
    <Column testID={`search-result:${hitKey(hit())}`}>
      <ResultCard
        testID={`search-open:${hitKey(hit())}`}
        accessibilityLabel={`Open conversation: ${hit().actor} in ${whereText(hit())}`}
        onPress={() => void openHit(hit())}
      >
        <Row align="center" gap={2} wrap>
          <Label variant="strong">{hit().actor}</Label>
          <Badge tone={hit().spaceId === null ? "agent" : "neutral"} label={whereText(hit())} />
        </Row>
        <MarkedText parts={markTerms(hit().text, props.query)} lines={4} />
      </ResultCard>
    </Column>
  );
}
