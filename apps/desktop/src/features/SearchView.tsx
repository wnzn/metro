// Search: semantic search over what the user can already read: their own
// personal Chat and the rooms and threads of Spaces they're a member of now.
// It says how results were ordered (System One rerank or similarity only)
// and when indexing is incomplete, and it never shows a result from a Space
// the user has left. A switch at the top turns to Documents & media
// (./library), a separate corpus of files the user uploaded.

import { openSettings, roomsOf, spaces } from "../store";
import { Action, Badge, Banner, Column, Label, PageHeader, Picker, Row, ScrollArea, SegmentedControl, SettingRow, SettingsGroup, Surface } from "../ui";
import type { PickerItem } from "../ui";
import { ConnectionBanner } from "./Conversation";
import { LibrarySurface } from "./library/LibraryView";
import { openLibrary, searchSurface } from "./library/libraryState";
import { rankingText, scoreText } from "./discovery/present";
import {
  hiddenHits,
  indexError,
  indexing,
  indexMore,
  indexProgress,
  openError,
  openHit,
  runSearch,
  scopeLabel,
  searchAnswer,
  searchError,
  searching,
  searchQuery,
  searchScope,
  setScope,
  spaceName,
  visibleHits,
} from "./discovery/searchState";
import type { SearchHit, SearchRanking, SearchScope } from "./discovery/searchState";

const scopeValue = (scope: SearchScope) => (scope.kind === "space" ? `space:${scope.spaceId}` : scope.kind);

function scopeFrom(value: string | null): SearchScope {
  if (value === "chat") return { kind: "chat" };
  if (value?.startsWith("space:")) return { kind: "space", spaceId: value.slice("space:".length) };
  return { kind: "all" };
}

const SURFACES = [
  { value: "conversations", label: "Conversations", description: "Messages in your personal Chat and your Spaces" },
  { value: "library", label: "Documents & media", description: "Files you uploaded" },
] as const;

function showSurface(value: "conversations" | "library") {
  if (searchSurface() === value) return;
  searchSurface.set(value);
  if (value === "library") void openLibrary();
}

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
                ? "Search by meaning across your personal library and the files members uploaded to Spaces you're in. It's separate from message search."
                : "Search by meaning across your personal Chat and the rooms and threads of Spaces you're a member of. Only messages you can already read are searched. Runner files and tool output aren't."
            }
          />
          <SegmentedControl testID="search-surface" accessibilityLabel="Search" options={SURFACES} value={searchSurface()} onChange={showSurface} />
          <If when={searchSurface() === "library"} else={<ConversationSearch />}>
            <LibrarySurface />
          </If>
        </Surface>
      </ScrollArea>
    </Surface>
  );
}

/** Message search, unchanged. */
function ConversationSearch() {
  return (
    <Column gap={6}>
      <SearchForm />
      <Messages />
      <Results />
    </Column>
  );
}

function SearchForm() {
  const items = (): PickerItem[] => [
    { value: "all", label: "Everything you can read" },
    { value: "chat", label: "Your personal Chat" },
    ...spaces().map((s) => ({ value: `space:${s.id}`, label: s.name, group: "Spaces you're in" })),
  ];
  return (
    <SettingsGroup testID="search-form">
      {/* The query is typed in the search box at the top of the window (TopSearch); no second field here. */}
      <SettingRow
        testID="search-query-echo"
        label={searchQuery().trim() ? `Looking for “${searchQuery().trim()}”` : "Type what you're looking for in the search box at the top"}
        description="Such as the deploy checklist, or what we decided about pricing. Return there, or Search, runs it."
      />
      <SettingRow label="Search in" description={`Now: ${scopeLabel(searchScope())}. Only Spaces you're a member of are listed.`}>
        <Picker testID="search-scope" accessibilityLabel="Search in" items={items()} value={scopeValue(searchScope())} onChange={(v) => setScope(scopeFrom(v))} />
      </SettingRow>
      <Row padX={4} padY={3} justify="end">
        <Action testID="search-submit" label="Search" busyLabel="Searching…" busy={searching()} variant="primary" onPress={() => void runSearch()} />
      </Row>
    </SettingsGroup>
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
          <Each of={visibleHits()} key={hitKey}>
            {(hit) => <ResultCard hit={hit()} ranking={answer().ranking} />}
          </Each>
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

function ResultCard(props: { hit: SearchHit; ranking: SearchRanking }) {
  const hit = () => props.hit;
  return (
    <Surface variant="card" pad={4} gap={2} testID={`search-result:${hitKey(hit())}`}>
      <Row align="center" gap={2} wrap>
        <Label variant="strong">{hit().actor}</Label>
        <Badge tone={hit().spaceId === null ? "agent" : "neutral"} label={whereText(hit())} />
        <Label variant="caption" color="muted">
          {scoreText(hit().score, props.ranking)}
        </Label>
      </Row>
      <Label lines={6}>{hit().text}</Label>
      <Row justify="end">
        <Action
          testID={`search-open:${hitKey(hit())}`}
          label="Open conversation"
          accessibilityLabel={`Open conversation: ${whereText(hit())}`}
          variant="ghost"
          onPress={() => void openHit(hit())}
        />
      </Row>
    </Surface>
  );
}
