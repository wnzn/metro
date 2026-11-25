// The non-secret proxy route the packaged launcher passes the UI
// (src/api/proxyEnv.ts). Tests run the app as the launcher would with no
// proxy configured; without it the route is "unknown" and every plain-HTTP
// server, loopback included, would ask first.
export const LAUNCHED_ENV = { METRO_PROXY_ROUTE: "direct", METRO_PROXY_DIRECT_FOR: "localhost,127.0.0.1,::1,[::1]" };

/** nsh/test `modules` with the launcher's route, plus any other variables. */
export const launched = (extra: Record<string, string> = {}) =>
  ({ process: { "default.env": { ...LAUNCHED_ENV, ...extra } } }) as any;
