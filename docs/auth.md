# Authentication

Metro uses three kinds of secret:

| Who | Credential | Comes from | Used on |
| --- | --- | --- | --- |
| People | `Authorization: Bearer <token>`: a local-account token or an OIDC JWT access token | `METRO_DEV_USERS`, the admin panel (`metro_ut_…`), or your identity provider | `/v1/me`, `/v1/spaces/...`, `/v1/chat/...`, `/v1/runs/...` |
| A Space's runner | `METRO_RUNNER_TOKEN` | Issued by the Space owner; see [spaces.md](spaces.md#runner-pairing) | `/v1/runner/...` |
| The model (optional) | `METRO_MODEL_API_KEY` | Your model provider | Agent replies |

This page covers how people sign in. The full rules and limits are in
[crates/auth/README.md](../crates/auth/README.md).

## Choose exactly one sign-in mode

The server refuses to start in any of these cases:

- neither mode is configured;
- `METRO_DEV_USERS` is set together with any `METRO_OIDC_*` variable;
- the database was created under a different mode or issuer (see
  [spaces.md](spaces.md#identity)).

Empty values count as unset.

### Local accounts

This mode has two kinds of account, both in the server's own `dev`
identity namespace:

- **Environment accounts**, from `METRO_DEV_USERS`. Use them to bootstrap
  the first admin, as in [deploy.md](deploy.md#1-bootstrap-the-first-admin).

  ```sh
  METRO_DEV_USERS='{"admin":"replace-with-token"}'
  ```

  Each token must be 16–512 printable ASCII bytes and distinct per user;
  generate it with `openssl rand -hex 32`. Usernames are 1–64 characters
  from `A–Z a–z 0–9 . _ @ -`, and the username is both the `id` and the
  display name. To rotate one, change the environment and restart the
  server. They can't be revoked from the panel.
- **Managed accounts**, created by an org admin in the admin panel at
  `/admin` ([deploy.md](deploy.md#4-create-peoples-accounts)). The server
  generates a stable `mu:` id and a `metro_ut_` token, shows the token once,
  and stores only its hash. Admins can rename, reissue and revoke, and each
  change takes effect on the next request. Managed accounts can't be admins,
  are never deleted, and start with no Space memberships.

Send these tokens only over loopback, an SSH port forward or HTTPS
([deploy.md](deploy.md#reaching-the-server-from-other-machines)).

### OIDC (JWT access tokens)

| Variable | Required | Default | Notes |
| --- | --- | --- | --- |
| `METRO_OIDC_ISSUER` | yes | | `https://` only. Must equal the discovery `issuer` exactly, including any trailing slash. |
| `METRO_OIDC_AUDIENCE` | yes | | Must appear in the token's `aud` |
| `METRO_OIDC_ACTOR_CLAIM` | no | `sub` | Display name only. The user's `id` is always `sub`. |
| `METRO_OIDC_REQUIRED_TYP` | no | unset | For example `at+jwt` ([RFC 9068](https://datatracker.ietf.org/doc/html/rfc9068)) |
| `METRO_OIDC_LEEWAY_SECS` | no | `60` | Clock-skew allowance, 0–300 |

At startup the server fetches `<issuer>/.well-known/openid-configuration` and
the `jwks_uri` it names, both over HTTPS. If either fetch fails, the server
doesn't start.

Tokens must be signed asymmetrically (RS\*, PS\*, ES256/384, or EdDSA). The
server rejects `none`, `HS*` and opaque tokens; it doesn't do introspection.
Signing keys are refreshed and rotated automatically.

### Provider notes

Metro needs an asymmetrically signed **JWT access token** whose `aud` includes
`METRO_OIDC_AUDIENCE`. Option names vary by version, so check them against
your provider's docs. None of these setups has been tested against a real
deployment yet (see [acceptance.md](acceptance.md#not-demonstrated)).

- **Ory Hydra** ([docs](https://www.ory.sh/docs/hydra/)): access tokens are
  opaque by default. Set `strategies.access_token: jwt`, or use the per-client
  strategy. Allow the audience on the client and request it. Copy the issuer
  from the discovery document, because it often ends with a slash.
- **Authelia**
  ([OIDC provider](https://www.authelia.com/configuration/identity-providers/openid-connect/provider/)):
  set `access_token_signed_response_alg` (for example `RS256`) on the client,
  and configure and request the audience. `METRO_OIDC_REQUIRED_TYP=at+jwt`
  then works.
- **authentik**
  ([OAuth2/OIDC provider](https://docs.goauthentik.io/add-secure-apps/providers/oauth2/)):
  select a **signing key** on the provider. Without one, authentik signs
  tokens with HS256, and Metro rejects them. The issuer is per application
  (`https://authentik.example/application/o/<slug>/`), and `aud` is the
  client ID.

Spec: [OpenID Connect Core](https://openid.net/specs/openid-connect-core-1_0.html).

## Limits

- The desktop app has no login or token refresh. Get an access token from your
  provider and paste it in; paste a new one when it expires.
- OIDC tokens can't be revoked by Metro. Each stays valid until `exp`, so
  keep lifetimes short. Managed local-account tokens can be reissued or
  revoked in the admin panel. Environment accounts change only with the
  server's environment.
- With OIDC there are no managed accounts: the admin user routes answer
  `404`, and managed tokens are never looked up.
- There are no scope or group checks. Space membership is the access control.
- One issuer and one audience per server. If the audience equals a client ID,
  an ID token for that client also passes. Use a distinct audience, or set
  `METRO_OIDC_REQUIRED_TYP=at+jwt`.
