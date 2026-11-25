import { goPages, hasSpace, pagesRoute, pagesRouteMount } from "../../pagesState";
import type { PagesRoute } from "../../pagesState";
import { mainPlace } from "../../store";
import { Action, Column, Label, PageHeader, PageLayout } from "../../ui";
import { DraftView } from "./DraftView";
import { PagesHome } from "./PagesHome";
import { PageView } from "./PageView";

// Route identity owns the entire view (including its requests, timers, open
// dialogs and source). Switching between two page IDs never reuses a view.
export function PagesView() {
  const active = () => mainPlace() === "pages";
  const slot = () => {
    const route = pagesRoute(), allowed = hasSpace(route.spaceId);
    return { route, allowed, key: `${pagesRouteMount()}:${allowed}` };
  };
  // Keep the keyed list inside a stable host, so removing its last slot
  // doesn't lose its insertion point among the Shell's other panes.
  const slots = <Each of={active() ? [slot()] : []} key={entry => entry.key}>
    {entry => <Route route={entry().route} allowed={entry().allowed} />}
  </Each>;
  return <Column grow={active()}>{slots}</Column>;
}

function Route(props: { route: PagesRoute; allowed: boolean }) {
  if (!props.allowed) return <PageLayout gap={3}>
    <PageHeader title="Pages" />
    <Label>This Space is no longer available.</Label>
    <Action label="All pages" onPress={() => goPages()} />
  </PageLayout>;
  const route = props.route;
  switch (route.kind) {
    case "home": return <PagesHome route={route} />;
    case "page": return <PageView route={route} />;
    case "draft": return <DraftView route={route} />;
  }
}
