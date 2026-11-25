import type { AvailableCatalog, Client, ProviderCatalog } from "./api/client";
import { sessionGuard } from "./store";

type PendingModels = { result: Promise<AvailableCatalog>; current: () => boolean };
const models = new WeakMap<Client, Map<string | null, PendingModels>>();
const catalogs = new WeakMap<Client, Promise<ProviderCatalog>>();

/** Join only outstanding reads of the same personal Chat or exact Space on this session's client. */
export function readStartupModels(client: Client, spaceId: string | null, fresh = false): Promise<AvailableCatalog> {
  let byScope = models.get(client);
  if (!byScope) models.set(client, byScope = new Map());
  const pending = byScope.get(spaceId);
  if (!fresh && pending?.current()) return pending.result;

  // Explicit refreshes (e.g. after editing providers) must not join a request
  // sent before the change. Subsequent startup readers can join the newer read.
  const current = sessionGuard(spaceId ?? undefined);
  const result = client.availableCatalog(spaceId).finally(() => {
    if (byScope.get(spaceId)?.result === result) byScope.delete(spaceId);
  });
  byScope.set(spaceId, { result, current });
  return result;
}

/** Provider metadata is not scoped. Don't retain the catalog after the read settles. */
export function readStartupProviderCatalog(client: Client): Promise<ProviderCatalog> {
  const pending = catalogs.get(client);
  if (pending) return pending;
  const result = client.providerCatalog().finally(() => catalogs.delete(client));
  catalogs.set(client, result);
  return result;
}
