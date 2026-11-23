// Documents & media, in three places:
//
// - `LibraryHome`: the Library page (`openPlace("library")`): upload, the
//   files the user can access, manual indexing, delete (asked first), and a
//   viewer for a file opened from the list.
// - `LibrarySearch`: Search's Documents & media mode: filters, results by
//   meaning, and the viewer that opens a match at its page, section or time.
//   The query is typed only in the search box at the top; Return there is the
//   only thing that sends it anywhere.
// - `MediaModelSettings`: the media model choice and where files and queries
//   go, mounted by Settings › Model providers.
//
// Message search (Search's Conversations mode) is a separate corpus.

import { untracked } from "nsh";
import { me, openPlace, openSettings, spaces } from "../../store";
import { ACCEPTED } from "../../api/library";
import type { AvailableModel } from "../../api/client";
import {
  Action,
  ActionsMenu,
  Badge,
  Banner,
  Chip,
  Column,
  Disclosure,
  CompactAudioPlayer,
  FileIcon,
  FileList,
  FileRow,
  GlyphAction,
  Label,
  MarkedText,
  MediaImage,
  MediaPlayer,
  PageHeader,
  Picker,
  Row,
  RowDetail,
  ResultCard,
  ScrollArea,
  SettingRow,
  SettingsGroup,
  Surface,
  TextSection,
  Thumbnail,
} from "../../ui";
import type { PickerItem, Tone } from "../../ui";
import { ConnectionBanner } from "../Conversation";
import { markTerms } from "../discovery/searchState";
import {
  askDelete,
  assetsError,
  assetsLoaded,
  canDelete,
  chooseAndUpload,
  clock,
  closeViewer,
  confirmDelete,
  confirmingDelete,
  deleting,
  draftModel,
  fileKindOf,
  fileNotice,
  formatSize,
  hiddenHits,
  indexAsset,
  indexStates,
  lessRelevantHits,
  lessRelevantOpen,
  libAnswer,
  libKind,
  libScope,
  libSearchError,
  libSearching,
  listScope,
  loadAssets,
  loadingAssets,
  loadModels,
  locatorText,
  maxFileBytes,
  mediaModels,
  modelDraft,
  modelError,
  modelsLoaded,
  nextCursor,
  openAsset,
  openLabel,
  openLibrary,
  openLibraryHit,
  openLibrarySearch,
  relevantHits,
  playerError,
  playerFailed,
  playerPosition,
  playFromMatch,
  playing,
  saveCopy,
  saveModel,
  saveNotice,
  savedModel,
  savedModelId,
  saving,
  savingModel,
  scopeLabel,
  seekToken,
  setLibScope,
  setListScope,
  setUploadTarget,
  spaceName,
  suggestedModelId,
  thumbnails,
  uploadOutcomes,
  uploadTarget,
  uploading,
  viewContent,
  viewing,
  visibleAssets,
  visibleHits,
} from "./libraryState";
import type { LibraryAsset, LibraryHit, LibraryRanking, LibraryScope, MediaKind, UploadTarget, ViewTarget } from "./libraryState";

const scopeValue = (s: LibraryScope | UploadTarget) => (s.kind === "space" ? `space:${s.spaceId}` : s.kind);
function scopeFrom(v: string | null): LibraryScope {
  if (v === "personal") return { kind: "personal" };
  if (v?.startsWith("space:")) return { kind: "space", spaceId: v.slice("space:".length) };
  return { kind: "all" };
}

const spaceItems = (): PickerItem[] => spaces().map((s) => ({ value: `space:${s.id}`, label: s.name, group: "Spaces you're in" }));
const accessItems = (): PickerItem[] => [{ value: "all", label: "Everything you can access" }, { value: "personal", label: "Your personal library" }, ...spaceItems()];

const TYPE_LABEL: Record<string, string> = Object.fromEntries(ACCEPTED.map((a) => [a.kind, a.label]));
const KIND_OPTIONS: { value: "any" | MediaKind; label: string }[] = [
  { value: "any", label: "Any type" },
  { value: "document", label: "Documents" },
  { value: "image", label: "Images" },
  { value: "audio", label: "Audio" },
  { value: "video", label: "Video" },
];

function whereBadge(spaceId: string | null): { tone: Tone; label: string } {
  return spaceId === null ? { tone: "agent", label: "Personal" } : { tone: "neutral", label: spaceName(spaceId) };
}

// ================================================================== Library page

/** The Library page: the files the user can access, upload, index and delete. The viewer when a file from it is open. */
export function LibraryHome() {
  untracked(() => void openLibrary());
  const open = () => (viewing()?.from === "library" ? viewing() : null);
  return (
    <Surface variant="panel" grow testID="library-home">
      <ConnectionBanner />
      <ScrollArea pad={6}>
        <Surface variant="content" gap={6}>
          <If when={open()} else={<LibraryFiles />}>
            {(target) => <Viewer target={target()} backLabel="‹ Back to files" />}
          </If>
        </Surface>
      </ScrollArea>
    </Surface>
  );
}

function LibraryFiles() {
  return (
    <Column gap={6} testID="library-view">
      <PageHeader
        title="Library"
        subtitle="Your personal uploads and files uploaded by members of your Spaces."
      >
        <GlyphAction testID="library-choose" glyph="upload" label="Upload" busyLabel="Uploading…" busy={uploading()} accessibilityLabel={`Upload files to ${scopeLabel(uploadTarget())}`} onPress={() => void chooseAndUpload()} />
      </PageHeader>
      <UploadTarget />
      <ModelStatus />
      <FilesSection />
    </Column>
  );
}

function UploadTarget() {
  const items = (): PickerItem[] => [{ value: "personal", label: "Your personal library" }, ...spaceItems()];
  const who = () => (uploadTarget().kind === "personal" ? "Only you will see these files." : `Every current member of ${scopeLabel(uploadTarget())} will see these files.`);
  return (
    <Column gap={2} testID="library-upload">
      <SettingsGroup>
        <SettingRow label="Upload to" description={who()}>
          <Picker
            testID="library-upload-target"
            accessibilityLabel="Upload to"
            items={items()}
            value={scopeValue(uploadTarget())}
            onChange={(v) => setUploadTarget(v?.startsWith("space:") ? { kind: "space", spaceId: v.slice(6) } : { kind: "personal" })}
          />
        </SettingRow>
        <SettingRow label="Accepted" description={`TXT, Markdown, PDF, DOCX, PNG, JPEG, WAV, MP3, MP4 and WebM, up to ${formatSize(maxFileBytes())} each.`} />
        <SettingRow label="Where files go" description="Only to your Metro server. Uploading sends nothing to a model, and Metro never scans runner files, workspaces or disks." />
      </SettingsGroup>
      <If when={uploadOutcomes().length > 0}>
        <Column gap={1} testID="library-upload-outcomes">
          <Each of={uploadOutcomes()} key={(o) => o.key}>
            {(o) => (
              <Label variant="caption" color={o().ok ? "success" : "danger"} testID={`library-upload-outcome:${o().name}`}>
                {`${o().name}: ${o().message}`}
              </Label>
            )}
          </Each>
        </Column>
      </If>
    </Column>
  );
}

/** Which media model Index uses; the choice itself lives in Settings › Model providers. */
function ModelStatus() {
  const text = () => {
    if (modelError()) return modelError()!;
    if (!modelsLoaded()) return "Checking your media model…";
    const m = savedModel();
    return m ? `Index sends a file to ${m.name}${m.owned ? "" : `, shared by ${m.ownerId ?? "someone"}`}, only when you press Index.` : "No media model is chosen, so nothing can be indexed or searched yet.";
  };
  return (
    <Banner tone={modelsLoaded() && !savedModel() ? "warning" : "neutral"} testID="library-model-status">
      <Column grow>
        <Label variant="caption" color={modelsLoaded() && !savedModel() ? "warning" : "default"}>
          {text()}
        </Label>
      </Column>
      <Action testID="library-open-model-settings" label={savedModel() ? "Change" : "Choose a media model"} variant="ghost" onPress={() => openSettings("providers")} />
    </Banner>
  );
}

function sourceText(a: LibraryAsset): string {
  return a.uploaderId === me() ? "Uploaded by you" : `Uploaded by ${a.uploaderId || "someone"}`;
}

function FilesSection() {
  return (
    <Column gap={3} testID="library-files">
      <Row align="center" gap={3} wrap>
        <Label variant="heading" heading>
          Files
        </Label>
        <Picker testID="library-list-scope" accessibilityLabel="Show files from" items={accessItems()} value={scopeValue(listScope())} onChange={(v) => setListScope(scopeFrom(v))} />
        <Action testID="library-refresh" label="Refresh" busyLabel="Loading…" busy={loadingAssets()} variant="ghost" onPress={() => void loadAssets(false)} />
      </Row>
      <If when={assetsError()}>
        {(e) => (
          <Banner tone="warning" testID="library-files-error">
            <Label color="warning">{e()}</Label>
          </Banner>
        )}
      </If>
      <If when={fileNotice()}>
        {(n) => (
          <Label variant="caption" color="muted" testID="library-file-notice">
            {n()}
          </Label>
        )}
      </If>
      <If
        when={assetsLoaded() && visibleAssets().length === 0}
        else={
          <FileList testID="library-file-list" accessibilityLabel="Files">
            <Each of={visibleAssets()} key={(a) => a.id}>
              {(a) => <AssetRow asset={a()} />}
            </Each>
          </FileList>
        }
      >
        <SettingsGroup testID="library-file-list">
          <SettingRow label="No files yet" description={`Nothing in ${scopeLabel(listScope())}. Upload some with Upload above.`} testID="library-files-empty" />
        </SettingsGroup>
      </If>
      <If when={nextCursor()}>
        <Row justify="center">
          <Action testID="library-more" label="Show more" busyLabel="Loading…" busy={loadingAssets()} variant="ghost" onPress={() => void loadAssets(true)} />
        </Row>
      </If>
    </Column>
  );
}

function AssetRow(props: { asset: LibraryAsset }) {
  const a = () => props.asset;
  const state = () => indexStates()[a().id];
  const where = () => whereBadge(a().spaceId);
  return (
    <FileRow
      testID={`library-file:${a().id}`}
      leading={<FileIcon kind={fileKindOf(a().kind, a().mimeType)} />}
      title={a().name}
      meta={`${sourceText(a())} · ${TYPE_LABEL[a().kind] ?? a().kind} · ${formatSize(a().sizeBytes)}`}
      trailing={
        <>
          <Badge tone={where().tone} label={where().label} />
          <Action testID={`library-open:${a().id}`} label="Open" accessibilityLabel={`Open ${a().name}`} variant="ghost" onPress={() => openAsset(a())} />
          <ActionsMenu
            testID={`library-actions:${a().id}`}
            label="More"
            accessibilityLabel={`More actions for ${a().name}`}
            items={[
              {
                id: `library-index:${a().id}`,
                label: state()?.state === "indexing" ? "Indexing…" : state()?.state === "indexed" ? "Index again" : "Index",
                disabled: !savedModelId() || state()?.state === "indexing",
                onPress: () => void indexAsset(a()),
              },
              ...(canDelete(a()) ? [{ id: `library-delete:${a().id}`, label: "Delete…", destructive: true, onPress: () => askDelete(a().id) }] : []),
            ]}
          />
        </>
      }
      below={
        <>
          <If when={state()}>
            {(s) => (
              <RowDetail>
                <Label variant="caption" color={s().state === "failed" ? "danger" : s().state === "indexed" ? "success" : "muted"} testID={`library-index-state:${a().id}`}>
                  {s().message}
                </Label>
              </RowDetail>
            )}
          </If>
          <If when={confirmingDelete() === a().id}>
            <Banner tone="danger" testID={`library-confirm-delete:${a().id}`}>
              <Column grow gap={1}>
                <Label variant="strong" color="danger">
                  {`Delete ${a().name}?`}
                </Label>
                <Label variant="caption" color="danger">
                  {a().spaceId === null ? "Its bytes and its index are removed. This can't be undone." : `It's removed for every member of ${spaceName(a().spaceId!)}, with its index. This can't be undone.`}
                </Label>
              </Column>
              <Action testID={`library-delete-cancel:${a().id}`} label="Cancel" variant="ghost" onPress={() => askDelete(null)} />
              <Action testID={`library-delete-confirm:${a().id}`} label="Delete" busyLabel="Deleting…" busy={deleting() === a().id} variant="danger" onPress={() => void confirmDelete(a())} />
            </Banner>
          </If>
        </>
      }
    />
  );
}

// ================================================================== media model (Settings)

/** What reaches this model, and who can see it. */
export function dataFlowText(m: AvailableModel): string {
  const common = "Index sends the whole original file to it, which extracts, renders or splits it there. Each search sends your query text to it. Uploading and opening files send it nothing.";
  if (m.runsOn === "owner_connection") {
    const who = m.ownerId ?? "its owner";
    return m.execution === "device"
      ? `${common} Shared by ${who}: it runs on ${who}'s own device, so ${who} can see the files and queries sent and pays for it.`
      : `${common} Shared by ${who}: it runs on ${who}'s endpoint, so whoever runs that endpoint can see the files and queries sent, and ${who} may pay for it.`;
  }
  if (m.execution === "device") return `${common} It runs in your own metro-connector on your device; the Metro server relays the request.`;
  return `${common} The Metro server sends them to this connection's endpoint; whoever runs it can see the files and queries sent.`;
}

/** Search may also rerank document text with the person's own System One connection. */
const RERANK_NOTE = "With a System One connection, search also sends short extracted text excerpts of matching documents to it for reranking, never whole files.";

/**
 * The media model Documents & media indexes and searches with, and where
 * files and queries go. Settings › Model providers mounts it. Nothing is
 * used until the person saves a choice.
 */
export function MediaModelSettings() {
  untracked(() => void loadModels());
  const items = (): PickerItem[] => [
    { value: "none", label: "None" },
    ...mediaModels().map((m) => ({ value: m.connectionId, label: `${m.name}${m.model ? ` · ${m.model}` : ""}${m.owned ? "" : ` · shared by ${m.ownerId ?? "someone"}`}` })),
  ];
  const dirty = () => modelDraft() !== savedModelId();
  const suggestion = () => {
    const id = suggestedModelId();
    return id && id !== savedModelId() ? mediaModels().find((m) => m.connectionId === id) ?? null : null;
  };
  return (
    <SettingsGroup
      title="Documents & media"
      description="Indexing and searching your files use the media embeddings connection you choose here, and nothing else. A text-only embeddings connection can't stand in."
      testID="library-model"
    >
      <SettingRow
        label="Index and search with"
        description={
          modelsLoaded() && mediaModels().length === 0
            ? "You have no media embeddings connection yet. Add EmbeddingGemma 2 (google/embeddinggemma-2) as a connection, or ask someone to share theirs."
            : savedModel()
              ? `Saved: ${savedModel()!.name}.`
              : "Nothing is chosen, so nothing can be indexed or searched."
        }
      >
        <Picker testID="library-model-picker" accessibilityLabel="Media model" items={items()} value={modelDraft() ?? "none"} onChange={(v) => modelDraft.set(v === "none" ? null : v)} />
      </SettingRow>
      <If when={draftModel()}>
        {(m) => (
          <Column padX={4} padY={3} gap={1} testID="library-model-dataflow">
            <Label variant="strong">Where your files go</Label>
            <Label variant="caption" color="muted">
              {dataFlowText(m())}
            </Label>
            <Label variant="caption" color="muted">
              {RERANK_NOTE}
            </Label>
          </Column>
        )}
      </If>
      <If when={modelError()}>
        {(e) => (
          <Column padX={4} padY={2}>
            <Label variant="caption" color="danger" testID="library-model-error">
              {e()}
            </Label>
          </Column>
        )}
      </If>
      <Row padX={4} padY={3} gap={2} justify="end">
        <If when={suggestion()}>
          {(s) => <Action testID="library-model-suggested" label={`Use ${s().name}`} variant="ghost" onPress={() => modelDraft.set(s().connectionId)} />}
        </If>
        <Action testID="library-model-save" label="Save" busyLabel="Saving…" busy={savingModel()} disabled={!dirty()} variant="primary" onPress={() => void saveModel()} />
      </Row>
    </SettingsGroup>
  );
}

// ================================================================== Search › Documents & media

/** Search's Documents & media mode: filters and results, or the viewer for an opened match. */
export function LibrarySearch() {
  untracked(() => void openLibrarySearch());
  const open = () => (viewing()?.from === "search" ? viewing() : null);
  return (
    <If when={open()} else={<LibrarySearchPage />}>
      {(target) => <Viewer target={target()} backLabel="‹ Back to results" />}
    </If>
  );
}

function LibrarySearchPage() {
  return (
    <Column gap={6} testID="library-search">
      <LibraryFilters />
      <Results />
    </Column>
  );
}

function LibraryFilters() {
  const scopes = () => [{ value: "all", label: "Everything" }, { value: "personal", label: "Personal" }, ...spaces().map((s) => ({ value: `space:${s.id}`, label: s.name }))];
  const disclosure = () => {
    const m = savedModel();
    if (!m) return null;
    const who = m.owned ? "" : m.execution === "device" ? `, which runs on ${m.ownerId ?? "its owner"}'s device` : `, shared by ${m.ownerId ?? "someone"}: whoever runs it sees the query`;
    return libSearching() ? "Searching…" : `Return in the search box at the top, or More results under it, sends your query to ${m.name}${who}. While you type, Metro only looks up literal matches itself, with no model; changing a filter sends nothing. ${RERANK_NOTE}`;
  };
  return (
    <Column gap={3} testID="library-search-form">
      <Row align="center" gap={2} wrap testID="library-search-scope" accessibilityLabel="Search files in">
        <Label variant="caption" color="muted">
          In
        </Label>
        <Each of={scopes()} key={(s) => s.value}>
          {(s) => (
            <Chip
              testID={`library-search-scope:${s().value}`}
              label={s().label}
              accessibilityLabel={`Search files in ${s().label}`}
              selected={scopeValue(libScope()) === s().value}
              onPress={() => setLibScope(scopeFrom(s().value))}
            />
          )}
        </Each>
      </Row>
      <Row align="center" gap={2} wrap testID="library-search-kind" accessibilityLabel="File type">
        <Label variant="caption" color="muted">
          Type
        </Label>
        <Each of={KIND_OPTIONS} key={(k) => k.value}>
          {(k) => (
            <Chip
              testID={`library-search-kind:${k().value}`}
              label={k().label}
              selected={(libKind() ?? "any") === k().value}
              onPress={() => libKind.set(k().value === "any" ? null : (k().value as MediaKind))}
            />
          )}
        </Each>
      </Row>
      <If
        when={disclosure()}
        else={
          <If when={modelsLoaded()}>
            <Banner tone="warning" testID="library-search-no-model">
              <Column grow>
                <Label variant="caption" color="warning">
                  Choose a media model in Settings › Model providers to search your files. Nothing is sent anywhere until you do and search.
                </Label>
              </Column>
              <Action testID="library-open-model-settings" label="Choose a media model" variant="ghost" onPress={() => openSettings("providers")} />
            </Banner>
          </If>
        }
      >
        {(d) => (
          <Label variant="caption" color="muted" testID="library-search-disclosure">
            {d()}
          </Label>
        )}
      </If>
      <If when={libSearchError()}>
        {(e) => (
          <Banner tone="danger" testID="library-search-error">
            <Column grow>
              <Label color="danger">{e().message}</Label>
            </Column>
            <If when={e().needsModel}>
              <Action testID="library-search-open-providers" label="Open Model providers" onPress={() => openSettings("providers")} />
            </If>
          </Banner>
        )}
      </If>
    </Column>
  );
}

/** How a result list was ordered, never claiming a System One rerank that didn't happen. */
export function libraryRankingText(ranking: LibraryRanking, hits: readonly LibraryHit[]): { tone: Tone; label: string; explain: string } {
  const reranked = hits.some((h) => h.scoreKind === "classifier");
  if (ranking !== "embeddings" && reranked) {
    return {
      tone: "system",
      label: "Documents reranked by System One",
      explain: "System One scored the text excerpts of matching documents. Images, audio and video are never sent to System One; they keep their similarity order.",
    };
  }
  return { tone: "neutral", label: "Similarity order", explain: "Ordered by embedding similarity. System One didn't rerank these." };
}

const hitKey = (h: LibraryHit) => `${h.assetId}:${h.locator.kind}:${h.locator.kind === "page" ? h.locator.page : h.locator.kind === "time" ? h.locator.startMs : h.locator.kind === "text" ? h.locator.start ?? 0 : 0}`;

function Results() {
  return (
    <If
      when={libAnswer()}
      else={
        <If when={!libSearchError()}>
          <SettingsGroup testID="library-search-start">
            <SettingRow
              label="Search your files by meaning"
              description="Only files you indexed are searched."
            >
              <Action testID="search-open-library" label="Open Library" variant="ghost" onPress={() => openPlace("library")} />
            </SettingRow>
          </SettingsGroup>
        </If>
      }
    >
      {(answer) => (
        <Column gap={3} testID="library-results">
          <Row align="center" gap={2} wrap>
            <Label variant="heading" heading testID="library-summary">
              {`${visibleHits().length} ${visibleHits().length === 1 ? "match" : "matches"} for “${answer().query}” · ${scopeLabel(answer().scope)}`}
            </Label>
            <Badge testID="library-ranking" tone={libraryRankingText(answer().ranking, visibleHits()).tone} label={libraryRankingText(answer().ranking, visibleHits()).label} />
          </Row>
          <Label variant="caption" color="muted" testID="library-ranking-explain">
            {libraryRankingText(answer().ranking, visibleHits()).explain}
          </Label>
          <If when={answer().partial || answer().notice}>
            <Banner tone="warning" testID="library-partial">
              <Label variant="caption" color="warning">
                {answer().notice ?? "Incomplete: not every file is indexed. Index more files, then search again."}
              </Label>
            </Banner>
          </If>
          <If when={hiddenHits() > 0}>
            <Label variant="caption" color="muted" testID="library-hidden">
              {`${hiddenHits()} ${hiddenHits() === 1 ? "match" : "matches"} from a file you deleted or a Space you're no longer in aren't shown.`}
            </Label>
          </If>
          <If when={visibleHits().length === 0}>
            <SettingsGroup testID="library-results-empty">
              <SettingRow label="No matches" description="Index the files you want to find (only files in your own index are searched), or try other words.">
                <Action testID="library-results-open-library" label="Open Library" variant="ghost" onPress={() => openPlace("library")} />
              </SettingRow>
            </SettingsGroup>
          </If>
          <Each of={relevantHits()} key={hitKey}>
            {(hit) => <HitCard hit={hit()} query={answer().query} />}
          </Each>
          <If when={lessRelevantHits().length > 0}>
            <Disclosure
              testID="library-less-relevant"
              title={`Less relevant (${lessRelevantHits().length})`}
              summary="No measurable similarity to your query"
              accessibilityLabel={`Less relevant: ${lessRelevantHits().length} ${lessRelevantHits().length === 1 ? "match" : "matches"} with no measurable similarity to your query`}
              open={lessRelevantOpen()}
              onToggle={(open) => lessRelevantOpen.set(open)}
            >
              <If when={lessRelevantOpen()}>
                <Column gap={3}>
                  <Each of={lessRelevantHits()} key={hitKey}>
                    {(hit) => <HitCard hit={hit()} query={answer().query} />}
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

function HitCard(props: { hit: LibraryHit; query: string }) {
  const h = () => props.hit;
  const where = () => whereBadge(h().spaceId);
  const thumb = () => (h().kind === "image" ? thumbnails()[h().assetId] ?? null : null);
  return (
    <Column testID={`library-hit:${hitKey(h())}`}>
      <ResultCard
        testID={`library-hit-open:${hitKey(h())}`}
        accessibilityLabel={`${openLabel(h())}: ${h().name}, ${where().label}, ${locatorText(h().locator)}`}
        onPress={() => openLibraryHit(h())}
        leading={
          <If when={thumb()} else={<FileIcon kind={h().kind} />}>
            {(src) => <Thumbnail src={src()} testID={`library-hit-thumb:${hitKey(h())}`} />}
          </If>
        }
      >
        <Row align="center" gap={2} wrap>
          <Label variant="strong">{h().name}</Label>
          <Badge tone={where().tone} label={where().label} />
          <Badge tone="system" label={locatorText(h().locator)} />
        </Row>
        <If when={h().excerpt}>{(x) => <MarkedText parts={markTerms(x(), props.query)} lines={3} />}</If>
        <Label variant="caption" color="accent">
          {openLabel(h())}
        </Label>
      </ResultCard>
    </Column>
  );
}

// ================================================================== viewer

function Viewer(props: { target: ViewTarget; backLabel: string }) {
  const t = () => props.target;
  const where = () => whereBadge(t().spaceId);
  return (
    <Column gap={4} testID="library-viewer">
      <Row align="center" gap={2} wrap>
        <Action testID="library-viewer-back" label={props.backLabel} variant="ghost" onPress={closeViewer} />
      </Row>
      <Row align="center" gap={2} wrap>
        <FileIcon kind={fileKindOf(t().kind, t().mimeType)} />
        <Label variant="title" heading testID="library-viewer-title">
          {t().name}
        </Label>
        <Badge tone={where().tone} label={where().label} />
        <If when={t().locator}>
          <Badge tone="system" label={locatorText(t().locator)} testID="library-viewer-locator" />
        </If>
      </Row>
      <Label variant="caption" color="muted">
        {t().query ? `Match for “${t().query}”. Opened through your Metro session; the bytes are kept only while this is open.` : "Opened through your Metro session; the bytes are kept only while this is open."}
      </Label>
      <ViewerBody target={t()} />
      <Row gap={2} align="center" wrap>
        <Action testID="library-save-copy" label="Save a copy to Downloads" busyLabel="Saving…" busy={saving()} disabled={!viewContent() || viewContent()!.status === "loading" || viewContent()!.status === "error"} onPress={() => void saveCopy()} />
        <If when={saveNotice()}>
          {(n) => (
            <Label variant="caption" color={n().ok ? "success" : "danger"} testID="library-save-notice">
              {n().message}
            </Label>
          )}
        </If>
      </Row>
    </Column>
  );
}

type Player = { url: string; audioOnly: boolean };

function ViewerBody(props: { target: ViewTarget }) {
  const t = () => props.target;
  const c = () => viewContent();
  const time = () => (t().locator?.kind === "time" ? (t().locator as { startMs: number; endMs: number | null }) : null);
  const startSeconds = () => (time() ? time()!.startMs / 1000 : 0);
  return (
    <Column gap={3}>
      <If when={c()?.status === "loading"}>
        <Label color="muted" testID="library-viewer-loading">
          Opening…
        </Label>
      </If>
      <If when={c()?.status === "error" ? (c() as { message: string }) : null}>
        {(e) => (
          <Banner tone="danger" testID="library-viewer-error">
            <Label color="danger">{e().message}</Label>
          </Banner>
        )}
      </If>
      <If when={c()?.status === "text" ? (c() as Extract<NonNullable<ReturnType<typeof viewContent>>, { status: "text" }>) : null}>
        {(x) => (
          <Column gap={2}>
            <Label variant="caption" color="muted" testID="library-viewer-section-note">
              {x().located ? `The matching section is marked${x().truncatedTop || x().truncatedBottom ? "; the text around it is shortened" : ""}.` : x().truncatedBottom ? "The start of the file (it's longer than shown)." : "The whole file."}
            </Label>
            <TextSection testID="library-viewer-text" before={x().before} match={x().match} after={x().after} mono={t().kind === "text"} />
          </Column>
        )}
      </If>
      <If when={c()?.status === "image" ? (c() as { url: string }) : null}>
        {(img) => <MediaImage testID="library-viewer-image" src={img().url} accessibilityLabel={t().name} />}
      </If>
      <If when={c()?.status === "player" ? (c() as Player) : null}>
        {(p) => (
          <Column gap={2}>
            <If when={p().audioOnly} else={<VideoPlayer target={t()} player={p()} startSeconds={startSeconds()} />}>
              <CompactAudioPlayer
                testID="library-viewer-player"
                title={t().name}
                accessibilityLabel={`${t().name}${time() ? `, match at ${locatorText(t().locator)}` : ""}`}
                src={p().url}
                startSeconds={startSeconds()}
                matchStart={time() ? time()!.startMs / 1000 : null}
                matchEnd={time()?.endMs != null ? time()!.endMs! / 1000 : null}
                seekToken={seekToken()}
                playing={playing()}
                onPlayingChange={(next) => playing.set(next)}
                onError={(m) => void playerFailed(m)}
                onTime={(current, duration) => playerPosition.set({ current, duration })}
                onLoaded={(duration) => playerPosition.set({ current: playerPosition()?.current ?? 0, duration })}
                onEnd={() => playing.set(false)}
              />
            </If>
            <If when={playerError()}>
              {(e) => (
                <Label variant="caption" color="danger" testID="library-player-error">
                  {`${e().replace(/\.?$/, ".")} Save a copy to play it in another app.`}
                </Label>
              )}
            </If>
          </Column>
        )}
      </If>
      <If when={c()?.status === "document" ? (c() as { sizeBytes: number }) : null}>
        {(d) => (
          <Column gap={2} testID="library-viewer-document">
            <Banner tone="neutral">
              <Label variant="caption">
                {`${TYPE_LABEL[t().kind ?? ""] ?? "This"} files can't be drawn inside the app yet. The file (${formatSize(d().sizeBytes)}) was read through your session; save a copy and open it in your ${t().kind === "pdf" ? "PDF viewer" : "word processor"}${t().locator?.kind === "page" ? `, then go to page ${(t().locator as { page: number }).page}` : ""}.`}
              </Label>
            </Banner>
            <If when={t().excerpt}>
              {(x) => (
                <Column gap={1}>
                  <Label variant="strong">{t().locator?.kind === "page" ? `Indexed text of page ${(t().locator as { page: number }).page}` : "Indexed text of the match"}</Label>
                  <TextSection testID="library-viewer-excerpt" before="" match={x()} after="" />
                </Column>
              )}
            </If>
          </Column>
        )}
      </If>
    </Column>
  );
}

/** Video keeps the platform's player and its own controls. */
function VideoPlayer(props: { target: ViewTarget; player: Player; startSeconds: number }) {
  const t = () => props.target;
  return (
    <Column gap={2}>
      <MediaPlayer
        testID="library-viewer-player"
        accessibilityLabel={`${t().name}, ${locatorText(t().locator)}`}
        src={props.player.url}
        startSeconds={props.startSeconds}
        seekToken={seekToken()}
        playing={playing()}
        onError={(m) => void playerFailed(m)}
        onTime={(current, duration) => playerPosition.set({ current, duration })}
        onLoaded={(duration) => playerPosition.set({ current: playerPosition()?.current ?? 0, duration })}
        onEnd={() => playing.set(false)}
      />
      <Row gap={2} align="center" wrap>
        <If when={t().locator?.kind === "time"}>
          <Action testID="library-play-match" label={`Play from ${clock(props.startSeconds * 1000)}`} variant="primary" onPress={playFromMatch} />
        </If>
        <Action testID="library-play-toggle" label={playing() ? "Pause" : "Play"} variant="ghost" onPress={() => playing.set(!playing())} />
        <If when={t().locator?.kind === "time"}>
          <Label variant="caption" color="muted">
            {`The player starts at the match (${locatorText(t().locator)}).`}
          </Label>
        </If>
      </Row>
      <If when={playerPosition()}>
        {(pos) => (
          <Label variant="caption" color="muted" testID="library-player-position">
            {`Player position ${clock(pos().current * 1000)}${pos().duration > 0 ? ` of ${clock(pos().duration * 1000)}` : ""}${playing() ? ", playing" : ""}`}
          </Label>
        )}
      </If>
    </Column>
  );
}
