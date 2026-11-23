// Test-only entry: the real connect screen, then quick-search FILE rows as
// the shell lane mounts them (`<QuickAssetPreview asset={asset} />`), with
// buttons to hide every row (unmount), show a second row of the first file
// (sharing), point the first row at another file (a query switch), and sign
// out.

import { mount, signal } from "nsh";
import { disconnect, restorePreferences, status } from "../../../store";
import { Action, Column, Label, Row, ScrollArea, Surface } from "../../../ui";
import { ConnectScreen } from "../../ConnectScreen";
import { QuickAssetPreview } from "../QuickAssetPreview";
import { library } from "../libraryState";
import type { LibraryAsset } from "../libraryState";

const assets = signal<LibraryAsset[]>([]);
const shown = signal(true);
const duplicate = signal(false);
const swapped = signal(false);

function Rows() {
  void library()
    ?.listAssets({ kind: "all" }, null)
    .then((page) => assets.set(page.assets))
    .catch(() => {});
  const rows = () => {
    const list = assets();
    if (list.length === 0) return [];
    const first = swapped() && list[1] ? list[1] : list[0]!;
    return [{ key: "row-0", asset: first }, ...list.slice(1).map((a) => ({ key: `row-${a.id}`, asset: a })), ...(duplicate() ? [{ key: "row-dup", asset: list[0]! }] : [])];
  };
  return (
    <Column gap={2}>
      <Each of={rows()} key={(r) => r.key}>
        {(r) => (
          <Row gap={2} align="center" testID={`h-row:${r().key}`}>
            <Label variant="caption">{r().asset.name}</Label>
            <QuickAssetPreview asset={r().asset} />
          </Row>
        )}
      </Each>
    </Column>
  );
}

function Harness() {
  void restorePreferences();
  const signedIn = () => status().phase === "online" || status().phase === "retrying";
  return (
    <If when={signedIn()} else={<ConnectScreen />}>
      <Surface variant="app">
        <Column grow>
          <Row gap={1}>
            <Action testID="h-toggle" label="Toggle" onPress={() => shown.set(!shown())} />
            <Action testID="h-dup" label="Duplicate" onPress={() => duplicate.set(!duplicate())} />
            <Action testID="h-swap" label="Swap" onPress={() => swapped.set(!swapped())} />
            <Action testID="h-signout" label="Sign out" onPress={() => {
              assets.set([]);
              disconnect();
            }} />
          </Row>
          <ScrollArea pad={2}>
            <If when={shown()}>
              <Rows />
            </If>
          </ScrollArea>
        </Column>
      </Surface>
    </If>
  );
}

mount(Harness);
