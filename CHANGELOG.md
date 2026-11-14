# Changelog

All notable changes to Foundry Circle are documented here.

## Unreleased

- Target Dioxus 0.8.0-alpha.3 (tartan-ui pinned to its 0.8 branch) and build
  `dioxus-cli` 0.8.0-alpha.3 plus the matching `wasm-bindgen-cli` 0.2.130 in this
  flake: nixpkgs' 0.7 `dx` cannot build the 0.8 crates and ships no wasm-bindgen
  for the ABI pair the lockfile needs.
- Add `foundry-circle-desktop`, the uploader as a Dioxus desktop window: browser
  sign-in, the converted kind and options, drag-and-drop or picker, and a
  one-at-a-time queue that reuses `foundry-circle-upload` without a second
  protocol.
- Add GM/admin asset upload at `PUT /api/v1/assets/{kind}/{name}`, re-validating the media-oxide Foundry asset policy server side (kind, normalised name, extension, magic bytes, safe SVG), writing without ever replacing an existing file, and serving the result from a Foundry-mounted directory.
- Add native client sign-in (RFC 8252 loopback + PKCE): `GET /api/auth/native` starts the Rauthy login, the callback hands a single-use code to the client's loopback listener, and `POST /api/auth/native/token` exchanges it for the client's own session.
- Add `foundry-circle-upload`: browser PKCE sign-in, stored broker session, and
  `push` to convert images and audio with ffmpeg and upload them as Foundry
  tokens, maps, tiles, artwork, and audio.
- Raise the minimum Rust version to 1.93, which `media-oxide` requires for its
  Foundry conversion policy.
- Implement typed world document reads with SHA-256 ETags and the chat, dice, scene, combat, and playlist commands; events and JSON-patch mutations stay unsupported.
- Add credential-safe Foundry VTT account, signed-URL, archive, and offline-cache acquisition.
- Validate Foundry v13/v14 package metadata and emit SHA-256 plus Nix SRI provenance.
- Add fail-closed declarative module/system add, version, rollback, and tombstone reconciliation.
- Add the Nix package and NixOS module interfaces consumed by Canix.
- Fix the local PostgreSQL URL (empty-host userinfo form rejected by sqlx), report ready once the database is up even with no world launched, and add GM/player lobby roles (`oidc.gmGroup`, `foundryWorld.playBaseUrl`) with a live-game card from Foundry `/api/status`.

## 0.1.0 - 2026-07-16

- Bootstrap the PostgreSQL-backed Rust control plane and Dioxus/tartan-ui operator console.
