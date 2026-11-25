// Test-only entry: the real connect screen and Shell, with Search and
// Scheduled tasks reachable by buttons of its own, so this lane's views are
// tested before the sidebar routes them. Until createClient spreads
// `discoveryApi(request)`, it attaches the same wrappers to the session's
// client over a test transport (fake tokens only, never a real one).

import { effect, mount, untracked, View } from "nsh";
import { ApiError } from "../../../api/client";
import { discoveryApi } from "../../../api/discovery";
import type { RequestFn } from "../../../api/client";
import { apiClient, me, openChat, openPlace, place, restorePreferences, status } from "../../../store";
import { Action, Row, Surface } from "../../../ui";
import { ConnectScreen } from "../../ConnectScreen";
import { ScheduledTasksView } from "../../ScheduledTasksView";
import { SearchView } from "../../SearchView";
import { Shell } from "../../Shell";
import { TopSearch } from "../../TopSearch";
import { TOKENS } from "../../../../test/fakeServer";

function testTransport(baseUrl: string): RequestFn {
  return async (method, path, body) => {
    const token = TOKENS[untracked(me) ?? ""] ?? "";
    const response = await fetch(baseUrl + path, {
      method,
      headers: { Accept: "application/json", Authorization: `Bearer ${token}`, ...(body === undefined ? {} : { "Content-Type": "application/json" }) },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await response.text();
    let detail = "";
    try {
      detail = text ? String(JSON.parse(text).error ?? "") : "";
    } catch {
      /* not JSON */
    }
    if (response.status === 401) throw new ApiError("auth", "The server rejected the access token (HTTP 401).", 401);
    if (response.status === 403) throw new ApiError("forbidden", `You don't have access to that (HTTP 403)${detail ? `: ${detail}` : ""}.`, 403);
    if (!response.ok) throw new ApiError(response.status === 404 ? "not_found" : "http", `${method} ${path} failed with HTTP ${response.status}${detail ? `: ${detail}` : ""}`, response.status);
    return text.trim() ? JSON.parse(text) : null;
  };
}

function Harness() {
  void restorePreferences();
  effect(() => {
    status();
    const c = apiClient() as unknown as Record<string, unknown> | null;
    if (c && typeof c.search !== "function") Object.assign(c, discoveryApi(testTransport(String(c.baseUrl))));
  });
  const signedIn = () => status().phase === "online" || status().phase === "retrying";
  return (
    <If when={signedIn()} else={<ConnectScreen />}>
      <Surface variant="app">
        <Row gap={1}>
          <Action testID="h-search" label="Search" onPress={() => openPlace("search")} />
          <Action testID="h-schedules" label="Scheduled" onPress={() => openPlace("schedules")} />
          <Action testID="h-chat" label="Chat" onPress={openChat} />
        </Row>
        {/* The Shell has its own search box; these pages get the same one here. */}
        {/* The search box fills its container (the Shell's header row); here it gets a slot of its own, as wide as it used to be. */}
        <If when={place() === "search" || place() === "schedules"}>
          <View class="w-[560px] shrink-0">
            <TopSearch />
          </View>
        </If>
        <If when={place() === "search"}>
          <SearchView />
        </If>
        <If when={place() === "schedules"}>
          <ScheduledTasksView />
        </If>
        <If when={place() !== "search" && place() !== "schedules"}>
          <Shell />
        </If>
      </Surface>
    </If>
  );
}

mount(Harness);
