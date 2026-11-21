# Authentication

Metro uses three kinds of secret:

| Who | Credential | Comes from | Used on |
| --- | --- | --- | --- |
| People | `Authorization: Bearer <token>`: a dev token or an OIDC JWT access token | `METRO_DEV_USERS`, or your identity provider | `/v1/me`, `/v1/spaces/...`, `/v1/chat/...`, `/v1/runs/...` |
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

### Dev users (your own machine only)

```sh
METRO_DEV_USERS='{"alice":"replace-with-token","bob":"replace-with-token"}'
```

- Each token must be 16–512 printable ASCII bytes and distinct per user.
  Generate them with `openssl rand -hex 32`.
- Usernames are 1–64 characters from `A–Z a–z 0–9 . _ @ -`. The username is
  both the user's `id` and their display name.

The [README quickstart](../README.md#1-dev-users-with-tokens-kept-off-screen)
writes these to private files without printing them.

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
- User tokens can't be revoked. Each stays valid until `exp`, so keep
  lifetimes short.
- There are no scope or group checks. Space membership is the access control.
- One issuer and one audience per server. If the audience equals a client ID,
  an ID token for that client also passes. Use a distinct audience, or set
  `METRO_OIDC_REQUIRED_TYP=at+jwt`.
