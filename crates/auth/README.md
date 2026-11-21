# metro-auth

Human bearer-token authentication for the Metro server.

```rust
let auth = metro_auth::Authenticator::from_env().await?;  // Result<Authenticator, ConfigError>
let who: metro_auth::Identity = auth.identity(token).await?;   // Result<Identity, AuthError>
// who.id    -> stable key for memberships and ownership
// who.actor -> display name only
let actor: String = auth.authenticate(token).await?;      // same check, returns who.actor
auth.namespace();  // "dev" or "oidc:<METRO_OIDC_ISSUER>"
auth.issuer();     // Some(issuer) in OIDC mode, None in dev mode
```

Both calls accept the raw token or a full `Bearer <token>` header value.
`authenticate` is kept for compatibility. It runs exactly the same
verification as `identity` (one discovery, one signature check) and returns
only `actor`.

| Mode | `Identity.id` | `Identity.actor` |
| --- | --- | --- |
| Dev tokens | username (never changes) | username |
| OIDC | the token's `sub` | the claim named by `METRO_OIDC_ACTOR_CLAIM` (default `sub`) |

The runner does **not** authenticate through this crate. See
[Runner token](#runner-token).

## Choosing a mode

There are exactly two modes, and they can't be combined:

| Mode | Set | Use for |
| --- | --- | --- |
| Dev tokens | `METRO_DEV_USERS` | Local development only |
| OIDC | `METRO_OIDC_ISSUER` and `METRO_OIDC_AUDIENCE` | Anything shared or deployed |

`from_env` fails closed. It returns a `ConfigError`, and the server should
refuse to start, when:

- neither mode is configured. There is no anonymous or bypass mode.
- `METRO_DEV_USERS` is set together with **any** `METRO_OIDC_*` variable.
- a value is malformed or not valid Unicode.
- in OIDC mode, discovery or the first JWKS fetch fails.

An empty or whitespace-only value counts as unset. `compose.yaml` passes
`VAR:` entries through as empty strings, and without this rule those would be
read as configured.

## Dev tokens

```sh
METRO_DEV_USERS='{"alice":"<openssl rand -hex 32>","bob":"<openssl rand -hex 32>"}'
```

That is the only setting dev mode needs.

Rules (checked at startup):

- The value must be a JSON object of `username -> token` with at least 1 entry
  and at most 256.
- Usernames: 1–64 characters from `[A-Za-z0-9._@-]`, each listed once.
  Duplicate JSON keys are rejected, not silently merged.
- Tokens: 16–512 bytes of printable ASCII with no spaces. Use
  `openssl rand -hex 32`.
- Every person needs a **distinct** token. Two users with the same token is an
  error.
- Don't start a dev token with `metro_rt_`. See [Runner token](#runner-token).
- Error messages name the user and the rule but never print a token.

Only SHA-256 digests of the tokens are kept in memory, and a lookup compares
the digest against every entry in constant time. Both `id` and `actor` are the
username, for example `alice`.

### Runner token

Runners don't authenticate through this crate. Each Space's owner issues
that Space's runner token from the server, and every such token starts with
the reserved prefix `metro_rt_`. The server refuses any bearer token with
that prefix on human endpoints before calling this crate, so a dev token that
starts with `metro_rt_` can never sign anyone in. This crate does not reject
such tokens at startup, so don't create them.

There is no server-wide runner token, and this crate doesn't need one. One
legacy check remains: if the server's environment happens to contain
`METRO_RUNNER_TOKEN`, startup fails when any dev token equals its value. That
variable is never accepted as a credential, and nothing needs to set it.

## OIDC (JWT access tokens)

| Variable | Required | Default | Meaning |
| --- | --- | --- | --- |
| `METRO_OIDC_ISSUER` | yes | | Issuer URL. Must be `https://`, with no credentials, query or fragment. It must **exactly** equal the provider's discovery `issuer` and the tokens' `iss`, including any trailing slash. |
| `METRO_OIDC_AUDIENCE` | yes | | Value that must appear in the token's `aud` (a string, or one entry of an array). |
| `METRO_OIDC_ACTOR_CLAIM` | no | `sub` | String claim used for the **display** `actor`, for example `preferred_username`, `name` or `email`. It does not change `id`, which is always `sub`. |
| `METRO_OIDC_REQUIRED_TYP` | no | unset | If set, the JOSE header `typ` must match, case-insensitively, with an optional `application/` prefix. `at+jwt` (RFC 9068) is recommended when your provider sets it. |
| `METRO_OIDC_LEEWAY_SECS` | no | `60` | Clock-skew allowance for `exp`/`nbf`, from 0 to 300. |

### What is verified

On startup the crate fetches
`<issuer without trailing slash>/.well-known/openid-configuration`, checks that
the document's `issuer` equals `METRO_OIDC_ISSUER` byte for byte, then fetches
its `jwks_uri`, which must also be `https://`. Startup fails if the JWKS has no
usable key.

For each token:

1. The token is at most 16 KiB and is a well-formed JWS.
2. The header `alg` is one of RS256/384/512, PS256/384/512, ES256, ES384 or
   EdDSA (Ed25519). `none` and every `HS*` algorithm are always rejected.
3. If `METRO_OIDC_REQUIRED_TYP` is set, the header `typ` must match it.
4. The key is chosen by `kid`. Each key's allowed algorithms come from its JWK
   key type and curve, narrowed by the JWK's own `alg` if it has one. A key is
   never chosen because of the token's header, which blocks algorithm
   confusion such as signing HS256 with an RSA public key, or using an RSA
   `kid` with ES256. Without a `kid`, every key whose type permits the `alg` is
   tried (at most 32).
5. The signature is checked with aws-lc-rs through `jsonwebtoken`.
6. Claims: `exp` is required and checked. `nbf` is checked if present. `iss`
   must be a single string equal to the issuer. `aud` must contain the
   audience. `sub` is **always** required, even when
   `METRO_OIDC_ACTOR_CLAIM` names a different claim.
7. `sub` must be a string of 1–255 bytes (the OIDC Core limit). The display
   claim must be a string of 1–512 bytes. Neither may contain control
   characters (tab, newline, NUL and so on), zero-width or bidi-override
   characters, or leading or trailing whitespace. Anything else is allowed,
   including common subject formats such as `auth0|5f7c…`, UUIDs, emails,
   URLs, base64 strings and non-ASCII text. A token that fails these checks
   is rejected; `sub` is never replaced by the display claim.

JWKS entries are ignored when they are symmetric (`kty: oct`), marked
`use: enc` (or any `use` other than `sig`), have `key_ops` without `verify`,
use an unsupported curve, or are malformed. The header fields `jku`, `x5u`,
`jwk` and `x5c` are never used to find keys.

### Fetching, caching and rotation

- HTTP: rustls with the platform's trust store. Connect timeout 5 s, total 10 s.
  Redirects are not followed. Bodies over 256 KiB are refused, and only a
  `200` response is accepted. At most 32 keys are read from a JWKS.
- Keys are refreshed on the first request after they are 10 minutes old.
- A token whose `kid` is not cached triggers one refetch, so a newly rotated-in
  key works at once. Refetches are single-flight and at least 30 s apart
  (counting the startup fetch), so tokens with random `kid`s can't flood the
  provider. A retired key stops working after the next refresh.
- If a refresh fails, cached keys stay in use for up to 1 hour after their last
  successful fetch. After that every request fails with
  `AuthError::Unavailable` until the provider answers again.

### Errors

`AuthError::Missing` and `AuthError::Invalid(reason)` should become `401`, and
`AuthError::Unavailable` should become `503`. Use `AuthError::is_unavailable()`
to tell them apart. Log the reason on the server, but don't send it to
clients. No error message contains a token.

## Provider notes

These cover only what Metro needs: an asymmetrically signed **JWT access
token** whose `aud` contains `METRO_OIDC_AUDIENCE`. Check exact option names
against the documentation for your provider version.

- **Ory Hydra.** Access tokens are opaque by default, and this crate can't
  verify opaque tokens because it doesn't do introspection. Switch them to
  JWTs with `strategies.access_token: jwt`, or the per-client access-token
  strategy in newer versions. Add the audience to the client's allowed
  `audience`, and have the client request it with the `audience` parameter.
  Hydra's issuer is whatever `urls.self.issuer` is set to, often with a
  trailing slash, so copy it from the discovery document.
- **Authelia.** Access tokens are JWTs only when the client sets
  `access_token_signed_response_alg` (for example `RS256`) to anything other
  than `none`. Authelia then issues RFC 9068 tokens, so
  `METRO_OIDC_REQUIRED_TYP=at+jwt` works. The client needs `audience`
  configured, and the audience must be requested. `sub` is an opaque per-user
  identifier, not the username.
- **authentik.** The issuer is per application, for example
  `https://authentik.example/application/o/<slug>/`, with a trailing slash.
  Select a **signing key** on the provider. Without one, authentik signs with
  HS256 using the client secret, and this crate rejects those tokens by
  design. `aud` is the provider's client ID, so set `METRO_OIDC_AUDIENCE` to
  it. What `sub` contains depends on the provider's *subject mode*.

### Identity, display and memberships

- **Key memberships on `id`, never on `actor`.** `METRO_OIDC_ACTOR_CLAIM` only
  changes what is shown. Claims such as `preferred_username`, `name` or
  `email` can be changed by users or admins, and two different people can
  share one. Two tokens with different `sub` values are always different
  identities, even if their display names match.
- **`id` is unique only within one issuer.** OIDC guarantees that `sub` is
  unique and stable per issuer, not across issuers. Dev usernames are just
  strings from `METRO_DEV_USERS`. So `alice` in dev mode, `alice` from issuer
  A and `alice` from issuer B may be three different people.
- **Moving data between modes or issuers needs a deliberate step.** Don't
  point a server at a membership database written under a different
  `namespace()`, whether that means another mode or another
  `METRO_OIDC_ISSUER`, unless an operator chooses to migrate it. A stored key
  is safe only as the pair `(namespace, id)`. The server should either store
  `namespace()` next to each id, or store it once and refuse to start when it
  changes. That decision belongs to the server lane; this crate only supplies
  `namespace()` and `issuer()`. The namespace uses the issuer string exactly
  as configured, so changing only a trailing slash also changes it.

## Limitations in this milestone

- **No login flow.** The desktop app has no OAuth/PKCE or device flow. Get an
  access token from your provider yourself and paste it in. There is no
  refresh either: when the token expires, paste a new one.
- Only JWT access tokens are supported. There is no token introspection
  (RFC 7662), no opaque tokens and no userinfo lookup.
- No revocation. A token stays valid until `exp`, so keep access-token
  lifetimes short.
- ID tokens are not rejected as a class. If `METRO_OIDC_AUDIENCE` equals a
  client ID, an ID token for that client would also pass. Use an audience that
  is distinct from any client ID where your provider allows it, or set
  `METRO_OIDC_REQUIRED_TYP=at+jwt` when the provider supports it.
- There are no scope or group checks. Every valid token for the audience is
  accepted.
- One issuer and one audience per server.
- The provider must be reachable at startup.
- P-521 and secp256k1 keys are not supported.

## Tests

```sh
cargo test -p metro-auth
```

The tests use no network access outside loopback. A local wiremock server
plays the identity provider over plain HTTP. That path exists only under
`cfg(test)`, and production builds always require HTTPS. The RSA, P-256 and
Ed25519 signing keys are generated on every run, so the repository contains no
private keys. The tests cover:

- **Configuration:** fail closed when nothing is set; dev and OIDC mutually
  exclusive; blank values; issuer and audience requirements; HTTPS-only issuer
  and `jwks_uri`; validation of the optional settings.
- **Identity:** dev `id == actor == username`; OIDC `id` is `sub` and `actor`
  defaults to `sub`; two different `sub`s that share a display name stay
  distinct; a missing, empty or non-string `sub` is rejected even when the
  display claim is present; subject bounds (common formats including `|`
  accepted; a 256-byte subject, control characters, bidi and zero-width
  characters, and surrounding spaces rejected); display-claim bounds;
  `namespace()` and `issuer()`; `authenticate` equals `identity().actor`.
- **Dev tokens:** distinct tokens per person; runner-token collision;
  duplicate keys; length, character and name rules; no tokens in errors or
  `Debug` output.
- **OIDC, valid tokens:** RS256, ES256 and EdDSA; tokens without a `kid`;
  `aud` arrays; a custom actor claim; `typ` enforcement.
- **OIDC, wrong claims:** wrong, missing or array `iss`; wrong or missing
  `aud`; expired, missing or non-numeric `exp`; future `nbf`; missing or empty
  `sub`; leeway behavior.
- **OIDC, signatures:** an attacker key reusing a trusted `kid`; a tampered
  payload; a stripped signature; HS256 keyed with the RSA public key;
  `alg: none`; mismatched key type and `alg`; a JWK-pinned `alg`; `oct` and
  `enc` keys in the JWKS ignored.
- **OIDC, lifecycle:** discovery issuer mismatch (trailing slash); discovery
  failure; a JWKS with no usable keys; an oversized JWKS; a cap on key count;
  key rotation; rate-limited unknown-`kid` refetches; stale keys used during an
  outage, then failing closed.
