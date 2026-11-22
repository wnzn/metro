// Search › Documents & media: files the user uploaded themselves, the media
// model they chose, manual indexing, search by meaning across them, and a
// viewer that opens a match at its page, section or time. Message search
// (the Conversations surface) is a separate corpus and stays as it is.

import { me, spaces } from "../../store";
import { ACCEPTED } from "../../api/library";
import type { AvailableModel } from "../../api/client";
import {
  Action,
  Badge,
  Banner,
  Column,
  Label,
  MediaImage,
  MediaPlayer,
  Picker,
  Row,
  SettingRow,
  SettingsGroup,
  Surface,
  TextSection,
} from "../../ui";
import type { PickerItem, Tone } from "../../ui";
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
  fileNotice,
  formatSize,
  hiddenHits,
  indexAsset,
  indexStates,
  libAnswer,
  libKind,
  libQuery,
  libScope,
  libSearchError,
  libSearching,
  listScope,
  loadAssets,
  loadingAssets,
  locatorText,
  maxFileBytes,
  mediaModels,
  modelDraft,
  modelError,
  modelsLoaded,
  nextCursor,
  openAsset,
  openLabel,
  openLibraryHit,
  playerError,
  playerFailed,
  playerPosition,
  playFromMatch,
  playing,
  runLibrarySearch,
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
const KIND_ITEMS: PickerItem[] = [
  { value: "any", label: "Any type" },
  { value: "document", label: "Documents" },
  { value: "image", label: "Images" },
  { value: "audio", label: "Audio" },
  { value: "video", label: "Video" },
];

/** The Documents & media surface: the viewer when a file is open, the library otherwise. */
export function LibrarySurface() {
  return (
    <If when={viewing()} else={<LibraryPage />}>
      {(target) => <Viewer target={target()} />}
    </If>
  );
}

function LibraryPage() {
  return (
    <Column gap={6} testID="library-view">
      <Label variant="caption" color="muted" testID="library-intro">
        Your personal library is yours alone. A Space's library holds what any member uploaded there, and its current members see it. Only uploaded files are here: Metro never scans runner files, workspaces or disks. Uploading sends nothing to a model; only Index does.
      </Label>
      <UploadGroup />
      <ModelGroup />
      <FilesGroup />
      <SearchGroup />
      <Results />
    </Column>
  );
}

// ------------------------------------------------------------------ upload

function UploadGroup() {
  const items = (): PickerItem[] => [{ value: "personal", label: "Your personal library" }, ...spaceItems()];
  const targetText = () => (uploadTarget().kind === "personal" ? "Only you will see these files." : `Every current member of ${scopeLabel(uploadTarget())} will see these files.`);
  return (
    <SettingsGroup title="Upload" description="Pick files with your system's file picker. Their own bytes go to your Metro server, and nowhere else." testID="library-upload">
      <SettingRow label="Upload to" description={targetText()}>
        <Picker
          testID="library-upload-target"
          accessibilityLabel="Upload to"
          items={items()}
          value={scopeValue(uploadTarget())}
          onChange={(v) => setUploadTarget(v?.startsWith("space:") ? { kind: "space", spaceId: v.slice(6) } : { kind: "personal" })}
        />
      </SettingRow>
      <SettingRow label="Accepted" description={`TXT, Markdown, PDF, DOCX, PNG, JPEG, WAV, MP3, MP4 and WebM, up to ${formatSize(maxFileBytes())} each.`}>
        <Action testID="library-choose" label="Choose files…" busyLabel="Uploading…" busy={uploading()} variant="primary" onPress={() => void chooseAndUpload()} />
      </SettingRow>
      <If when={uploadOutcomes().length > 0}>
        <Column padX={4} padY={3} gap={1} testID="library-upload-outcomes">
          <Each of={uploadOutcomes()} key={(o) => o.key}>
            {(o) => (
              <Label variant="caption" color={o().ok ? "success" : "danger"} testID={`library-upload-outcome:${o().name}`}>
                {`${o().name}: ${o().message}`}
              </Label>
            )}
          </Each>
        </Column>
      </If>
    </SettingsGroup>
  );
}

// ------------------------------------------------------------------ media model

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

function ModelGroup() {
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
      title="Media model"
      description="Indexing and search use the media embeddings connection you choose here, and nothing else. A text-only embeddings connection can't stand in."
      testID="library-model"
    >
      <SettingRow
        label="Index and search with"
        description={
          modelsLoaded() && mediaModels().length === 0
            ? "You have no media embeddings connection yet. Add EmbeddingGemma 2 (google/embeddinggemma-2) in Settings › Model providers, or ask someone to share theirs."
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

// ------------------------------------------------------------------ files

function whereBadge(spaceId: string | null): { tone: Tone; label: string } {
  return spaceId === null ? { tone: "agent", label: "Personal" } : { tone: "neutral", label: spaceName(spaceId) };
}

function sourceText(a: LibraryAsset): string {
  return a.uploaderId === me() ? "Uploaded by you" : `Uploaded by ${a.uploaderId || "someone"}`;
}

function FilesGroup() {
  return (
    <Column gap={2} testID="library-files">
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
      <SettingsGroup testID="library-file-list">
        <If when={assetsLoaded() && visibleAssets().length === 0}>
          <SettingRow label="No files yet" description={`Nothing in ${scopeLabel(listScope())}. Choose files above to upload some.`} testID="library-files-empty" />
        </If>
        <Each of={visibleAssets()} key={(a) => a.id}>
          {(a) => <FileRow asset={a()} />}
        </Each>
      </SettingsGroup>
      <If when={nextCursor()}>
        <Row justify="center">
          <Action testID="library-more" label="Show more" busyLabel="Loading…" busy={loadingAssets()} variant="ghost" onPress={() => void loadAssets(true)} />
        </Row>
      </If>
    </Column>
  );
}

function FileRow(props: { asset: LibraryAsset }) {
  const a = () => props.asset;
  const state = () => indexStates()[a().id];
  const where = () => whereBadge(a().spaceId);
  return (
    <Column testID={`library-file:${a().id}`}>
      <SettingRow label={a().name} description={`${sourceText(a())} · ${TYPE_LABEL[a().kind] ?? a().kind} · ${formatSize(a().sizeBytes)}`}>
        <Row gap={2} align="center" wrap justify="end">
          <Badge tone={where().tone} label={where().label} />
          <Action testID={`library-open:${a().id}`} label="Open" accessibilityLabel={`Open ${a().name}`} variant="ghost" onPress={() => openAsset(a())} />
          <Action
            testID={`library-index:${a().id}`}
            label={state()?.state === "indexed" ? "Index again" : "Index"}
            accessibilityLabel={`Index ${a().name} with ${savedModel()?.name ?? "your media model"}`}
            busyLabel="Indexing…"
            busy={state()?.state === "indexing"}
            disabled={!savedModelId()}
            onPress={() => void indexAsset(a())}
          />
          <If when={canDelete(a())}>
            <Action testID={`library-delete:${a().id}`} label="Delete" accessibilityLabel={`Delete ${a().name}`} variant="ghost" onPress={() => askDelete(a().id)} />
          </If>
        </Row>
      </SettingRow>
      <If when={state()}>
        {(s) => (
          <Column padX={4} padY={2}>
            <Label variant="caption" color={s().state === "failed" ? "danger" : s().state === "indexed" ? "success" : "muted"} testID={`library-index-state:${a().id}`}>
              {s().message}
            </Label>
          </Column>
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
    </Column>
  );
}

// ------------------------------------------------------------------ search

function SearchGroup() {
  return (
    <SettingsGroup title="Search your files" description="By meaning, across text, pages, images and audio or video, with your saved media model. Only files you can access now are searched." testID="library-search-form">
      {/* The query is typed in the search box at the top of the window (TopSearch); no second field here. */}
      <SettingRow
        testID="library-query-echo"
        label={libQuery().trim() ? `Looking for “${libQuery().trim()}”` : "Type what you're looking for in the search box at the top"}
        description="Such as a whiteboard photo of the roadmap, or the clause about termination. Return there, or Search files, runs it."
      />
      <SettingRow label="Search in" description={`Now: ${scopeLabel(libScope())}. Only Spaces you're a member of are listed.`}>
        <Picker testID="library-search-scope" accessibilityLabel="Search files in" items={accessItems()} value={scopeValue(libScope())} onChange={(v) => setLibScope(scopeFrom(v))} />
      </SettingRow>
      <SettingRow label="Type">
        <Picker testID="library-search-kind" accessibilityLabel="File type" items={KIND_ITEMS} value={libKind() ?? "any"} onChange={(v) => libKind.set(v === null || v === "any" ? null : (v as MediaKind))} />
      </SettingRow>
      <If when={libSearchError()}>
        {(e) => (
          <Column padX={4} padY={2}>
            <Label variant="caption" color="danger" testID="library-search-error">
              {e().message}
            </Label>
          </Column>
        )}
      </If>
      <Row padX={4} padY={3} justify="end">
        <Action testID="library-search-submit" label="Search files" busyLabel="Searching…" busy={libSearching()} variant="primary" onPress={() => void runLibrarySearch()} />
      </Row>
    </SettingsGroup>
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

export function hitScoreText(hit: LibraryHit): string {
  return hit.scoreKind === "classifier" ? `System One relevance ${hit.score.toFixed(2)}` : `similarity ${hit.score.toFixed(2)}`;
}

const hitKey = (h: LibraryHit) => `${h.assetId}:${h.locator.kind}:${h.locator.kind === "page" ? h.locator.page : h.locator.kind === "time" ? h.locator.startMs : h.locator.kind === "text" ? h.locator.start ?? 0 : 0}`;

function Results() {
  return (
    <If when={libAnswer()}>
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
              <SettingRow label="No matches" description="Index the files you want to find (only files in your own index are searched), or try other words." />
            </SettingsGroup>
          </If>
          <Each of={visibleHits()} key={hitKey}>
            {(hit) => <HitCard hit={hit()} />}
          </Each>
        </Column>
      )}
    </If>
  );
}

function HitCard(props: { hit: LibraryHit }) {
  const h = () => props.hit;
  const where = () => whereBadge(h().spaceId);
  return (
    <Surface variant="card" pad={4} gap={2} testID={`library-hit:${hitKey(h())}`}>
      <Row align="center" gap={2} wrap>
        <Label variant="strong">{h().name}</Label>
        <Badge tone={where().tone} label={where().label} />
        <Badge tone="system" label={locatorText(h().locator)} />
        <Label variant="caption" color="muted">
          {hitScoreText(h())}
        </Label>
      </Row>
      <If when={h().excerpt} else={<Label variant="caption" color="muted">{`No text excerpt: matched by its ${h().kind === "document" ? "page image" : h().kind} content.`}</Label>}>
        {(x) => <Label lines={4}>{x()}</Label>}
      </If>
      <Row justify="end">
        <Action testID={`library-hit-open:${hitKey(h())}`} label={openLabel(h())} accessibilityLabel={`${openLabel(h())}: ${h().name}`} variant="ghost" onPress={() => openLibraryHit(h())} />
      </Row>
    </Surface>
  );
}

// ------------------------------------------------------------------ viewer

function Viewer(props: { target: ViewTarget }) {
  const t = () => props.target;
  const where = () => whereBadge(t().spaceId);
  return (
    <Column gap={4} testID="library-viewer">
      <Row align="center" gap={2} wrap>
        <Action testID="library-viewer-back" label={libAnswer() ? "‹ Back to results" : "‹ Back to files"} variant="ghost" onPress={closeViewer} />
      </Row>
      <Row align="center" gap={2} wrap>
        <Label variant="title" heading testID="library-viewer-title">
          {t().name}
        </Label>
        <Badge tone={where().tone} label={where().label} />
        <Badge tone="system" label={locatorText(t().locator)} testID="library-viewer-locator" />
      </Row>
      <If when={t().query}>
        {(q) => (
          <Label variant="caption" color="muted">
            {`Match for “${q()}”. Opened through your Metro session; the bytes are kept only while this is open.`}
          </Label>
        )}
      </If>
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

function ViewerBody(props: { target: ViewTarget }) {
  const t = () => props.target;
  const c = () => viewContent();
  const startSeconds = () => (t().locator?.kind === "time" ? (t().locator as { startMs: number }).startMs / 1000 : 0);
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
      <If when={c()?.status === "player" ? (c() as { url: string; audioOnly: boolean }) : null}>
        {(p) => (
          <Column gap={2}>
            <MediaPlayer
              testID="library-viewer-player"
              accessibilityLabel={`${t().name}, ${locatorText(t().locator)}`}
              src={p().url}
              audioOnly={p().audioOnly}
              startSeconds={startSeconds()}
              seekToken={seekToken()}
              playing={playing()}
              onError={(m) => void playerFailed(m)}
              onTime={(current, duration) => playerPosition.set({ current, duration })}
              onLoaded={(duration) => playerPosition.set({ current: playerPosition()?.current ?? 0, duration })}
              onEnd={() => playing.set(false)}
            />
            <Row gap={2} align="center" wrap>
              <If when={t().locator?.kind === "time"}>
                <Action testID="library-play-match" label={`Play from ${clock(startSeconds() * 1000)}`} variant="primary" onPress={playFromMatch} />
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
