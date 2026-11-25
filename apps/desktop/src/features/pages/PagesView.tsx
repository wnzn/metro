import { goPages, hasSpace, pagesRoute } from "../../pagesState";
import type { PagesRoute } from "../../pagesState";
import { Action, Label, PageHeader, PageLayout } from "../../ui";
import { DraftView } from "./DraftView";
import { PagesHome } from "./PagesHome";
import { PageView } from "./PageView";

// Route identity owns the entire view (including its requests, timers, open
// dialogs and source). Switching between two page IDs never reuses a view.
export function PagesView() {
  return <Each of={[pagesRoute()]} key={route => route}>
    {route => <If when={hasSpace(route().spaceId)} else={<PageLayout gap={3}>
      <PageHeader title="Pages" />
      <Label>This Space is no longer available.</Label>
      <Action label="All pages" onPress={() => goPages()} />
    </PageLayout>}>
      <Route route={route()} />
    </If>}
  </Each>;
}

function Route(props: { route: PagesRoute }) {
  const route = props.route;
  switch (route.kind) {
    case "home": return <PagesHome route={route} />;
    case "page": return <PageView route={route} />;
    case "draft": return <DraftView route={route} />;
  }
}
