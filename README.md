# Foundry Circle

<!-- simit:badges:start -->

![CI](https://img.shields.io/badge/CI-managed-2088ff) [![Nix](https://img.shields.io/badge/Nix-drift-5277c3)](flake.nix) [![docs](https://img.shields.io/badge/docs-enabled-6f42c1)](https://docs.rs/foundry-circle) [![artifacts](https://img.shields.io/badge/artifacts-configured-2ea44f)](.github/workflows/release.yml)

<!-- simit:badges:end -->

Foundry Circle is a Rust/Axum broker for the active Foundry VTT world, with a
Dioxus 0.8 operator console using tartan-ui. PostgreSQL stores control-plane
state only; Foundry remains authoritative for world documents.

The repository provides health, discovery, a typed driver seam, OIDC login,
PostgreSQL-backed sessions, a capability registry, and a non-ready readiness
contract. World documents are readable at `GET /api/v1/world/documents/{collection}`
and `GET /api/v1/world/documents/{collection}/{id}` for the `actors`, `scenes`,
`messages`, `playlists` and `tables` collections; GMs and admins may read them.
Admins can call `POST /api/v1/world/commands` with `chat.create`, `dice.roll`,
`scene.activate`, `combat.update` and `playlist.control`; every command needs an
`idempotencyKey`. Events and JSON-patch
mutations are not implemented.

## Asset upload

A DM converts assets on their own machine with
[media-oxide](https://github.com/caniko/media-oxide) (choices are mapped to
Foundry's `tokens`, `maps`, `tiles`, `artwork` and `audio` directories) and the
native client uploads the converted file. The server re-validates everything;
nothing the client sends is trusted.

```
PUT /api/v1/assets/{kind}/{name}
Content-Type: application/octet-stream
Cookie: foundry_circle_session=<token>

<file bytes>
```

`kind` is `token`, `map`, `tile`, `artwork` or `audio`, and GMs and admins only.
The body must be exactly `application/octet-stream` (an upload cannot be made
from a cross-origin form), up to 100 MiB, and it must pass the same media-oxide
policy the client used: `[a-z0-9]+(-[a-z0-9]+)*` names with an extension the kind
accepts, magic bytes matching that extension, and for SVG a UTF-8 document whose
root is `<svg>` and which carries no script, entity or JavaScript URL. A refused
upload is JSON `{"error":"<code>"}`; a name that already exists answers `409`
`{"error":"asset_exists"}` and is never overwritten. Success is `201`
`{"path":"<prefix>/<kind>/<name>"}`, which is the path Foundry serves.

Files land in `FOUNDRY_CIRCLE_ASSETS_DIR/<kind>/<name>`, where that directory is
mounted read-only at `<Foundry data dir>/Data/<FOUNDRY_CIRCLE_ASSETS_PREFIX>`.
Both variables are set by `services.foundry-circle.assets`; without them the
route answers `503` `{"error":"assets_not_configured"}`.

### Native sign-in

A native client cannot reuse the browser's session: sessions carry Rauthy
refresh tokens that rotate, so a copied row would break the browser's next
refresh and revoke the subject. The client runs its own login instead:

1. `GET /api/auth/native?port=<1024..65535>&state=<base64url, 16..128>&challenge=<43 chars base64url(sha256(verifier))>`
   starts the ordinary Rauthy login in the system browser.
2. The callback creates a normal session but sets no browser cookie; it answers
   `303` to `http://127.0.0.1:<port>/callback?code=<code>&state=<state>`.
3. `POST /api/auth/native/token` with `{"code":"...","verifier":"..."}` (JSON,
   as every other POST) returns
   `{"token":"...","cookie":"foundry_circle_session","expires_at":"<rfc3339>"}`.
   The code is single use and expires after two minutes; a wrong verifier, an
   unknown or replayed code, or a revoked session is `400`
   `{"error":"invalid_grant"}`.

The client then sends `Cookie: foundry_circle_session=<token>` on every request.

### Desktop uploader

`foundry-circle-desktop` is that same client with a window: sign in through the
browser, choose the asset kind and conversion options, then drop files in and
upload them one at a time. It reuses `foundry-circle-upload` and adds no
protocol of its own. `nix build .#foundry-circle-desktop` builds it on Linux,
where the window needs webkitgtk, and on macOS, where it uses WKWebView. What
stays manual is the same as for the CLI: token art faces south, and maps must
not have the grid baked in.

### Release artifacts

Each signed GitHub release carries one archive per platform; the release
workflow builds `packages.<system>.release-bundle` on that platform's runner
and merges the archives into the single release:

| Archive                                                   | Built on                     | Contents                                                                                                                                                                 |
| --------------------------------------------------------- | ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `foundry-circle-<version>-x86_64-linux-nix-bundle.tar.gz` | `ubuntu-24.04`               | `foundry-circle`, `foundryvtt-fetch`, `foundryvtt-fetchd`, `foundryvtt-fetch-hook`, `foundry-circle-upload`, `foundry-circle-desktop`, self-contained through nix-bundle |
| `foundry-circle-<version>-x86_64-windows.zip`             | `ubuntu-24.04` (MinGW cross) | `foundry-circle-upload.exe`, `foundry-circle-desktop.exe`, `WebView2Loader.dll`                                                                                          |
| `foundry-circle-<version>-aarch64-darwin.tar.gz`          | `macos-15`                   | `foundry-circle-upload`, `foundry-circle-desktop`                                                                                                                        |

The Windows zip needs no MinGW runtime: Rust's `x86_64-pc-windows-gnu` target
links the MinGW startup objects statically, and the WebView2 runtime itself
ships with Windows 10/11. The server and the fetch tools are Linux-only and are
not part of the Windows or macOS archives.

## Development

Rust 1.93 or newer is required to use the security-patched dependency graph;
`media-oxide` (Foundry asset conversion) declares 1.93 as its minimum.

```bash
cargo check --all-targets --all-features
cargo test --all-targets --all-features
```

The production service binds to `127.0.0.1:8032` by default. It does not
accept a Foundry password through command-line arguments or store credentials
in the repository.

Phase 02 also ships the standalone `foundryvtt-fetch` crate. It acquires
licensed releases outside Nix evaluation, records SHA-256/Nix-SRI provenance,
and initially seeds mutable module/system directories from
`services.foundryvtt.declarativePackages`. Use `state = "absent"` to reset an
initialization marker; worlds and package directories remain mutable
Foundry-owned state.

`foundry-circle-upload` is the native asset client. `login` signs in through the
system browser with a loopback PKCE redirect, `push` converts images and audio
with ffmpeg to Foundry's token, map, tile, artwork, and audio policy, and
uploads each result to `PUT /api/v1/assets/{kind}/{file_name}`; `whoami`,
`logout`, and `--dry-run` cover the rest. Conversion policy, including the
per-format defaults and normalized names, comes from `media-oxide`, so the
client and the broker agree. See
[`crates/foundry-circle-upload/README.md`](crates/foundry-circle-upload/README.md)
for usage and for what still needs the DM (south-facing token art, maps with the
grid drawn in).

## Boundaries

- Rauthy is the production OIDC issuer. Existing Kanidm identities reach it
  through the nix-provenance-managed federation surface.
- SQLite is not supported; PostgreSQL is the only database backend.
- Arbitrary JavaScript/evaluate endpoints are not part of the production
  feature set.
- Licensed Foundry archives must not be published to the shared Attic cache.

## License

Licensed under `EUPL-1.2`. See `LICENSE`.
