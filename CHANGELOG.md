# Changelog

All notable changes to Foundry Circle are documented here.

## Unreleased

- Implement typed world document reads with SHA-256 ETags and the chat, dice, scene, combat, and playlist commands; events and JSON-patch mutations stay unsupported.
- Add credential-safe Foundry VTT account, signed-URL, archive, and offline-cache acquisition.
- Validate Foundry v13/v14 package metadata and emit SHA-256 plus Nix SRI provenance.
- Add fail-closed declarative module/system add, version, rollback, and tombstone reconciliation.
- Add the Nix package and NixOS module interfaces consumed by Canix.
- Fix the local PostgreSQL URL (empty-host userinfo form rejected by sqlx), report ready once the database is up even with no world launched, and add GM/player lobby roles (`oidc.gmGroup`, `foundryWorld.playBaseUrl`) with a live-game card from Foundry `/api/status`.

## 0.1.0 - 2026-07-16

- Bootstrap the PostgreSQL-backed Rust control plane and Dioxus/tartan-ui operator console.
