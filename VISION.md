# Foundry Circle vision

Foundry Circle is a Rust/Axum broker for the active Foundry VTT world, fronted
by a Dioxus operator console. It gives operators health and discovery, OIDC
login, PostgreSQL-backed sessions, a capability registry, a typed seam toward
Foundry's documents, and a separate crate that acquires licensed Foundry
releases with recorded provenance. Foundry remains the authority for world
content; this project owns only the control plane around it.

## Problem

Running Foundry VTT for a group means handling a licensed archive, mutable
module and system directories, authentication, and provenance by hand, without
a safe boundary between "control-plane state" and "the world's documents". It
is easy to fabricate a plausible-looking schema or credential path that does not
match what Foundry actually does.

## Principles

- Foundry is the authority for world documents; PostgreSQL stores control-plane
  state only.
- Do not fabricate Foundry schemas, API-user credentials, OIDC claims, licensed
  archives, route topology, or provenance fragments. When one is missing,
  report the producer, the regeneration workflow, and the validation command.
- Production authentication trusts Rauthy through nix-provenance and does not
  accept direct Kanidm tokens, local passwords, or arbitrary JavaScript.
- Licensed artifacts are acquired outside Nix evaluation, with SHA-256 and Nix
  SRI provenance recorded, and declarative package reconciliation fails closed.
- Build with Crane/harbor-rs and use Dioxus/tartan-ui for the operator console;
  keep production asset and API paths below `/api`.
- Stay honest about readiness: world documents sit behind the typed driver seam
  until a licensed archive and a disposable v13.351 world certify the live
  browser driver, and the readiness contract reports not-ready rather than
  pretending otherwise.

## Scope

The broker service (health, discovery, typed driver seam, OIDC login,
PostgreSQL-backed sessions, capability registry), the Dioxus console, the
`foundryvtt-fetch` crate and its provenance model, package metadata validation,
declarative module/system reconciliation with an initialization marker, and the
Nix package and NixOS module interfaces a consumer flake imports.

## Non-goals

SQLite is not supported; PostgreSQL is the only database backend. Arbitrary
JavaScript or `evaluate` endpoints are not part of the production feature set.
The project does not ship or redistribute the licensed Foundry archive and does
not claim to be the Foundry server itself.

## Direction

The unreleased changelog is the roadmap: credential-safe account, signed-URL,
archive, and offline-cache acquisition; v13/v14 package metadata validation with
provenance; fail-closed declarative add, version, rollback, and tombstone
reconciliation; and the Nix interfaces consumed by canix. Until the live browser
driver is certified, the driver seam stays non-ready.

## Place in the fleet

Canix pins this flake and deploys the service through its Foundry VTT host
module (`root/hosts/atlas/server/web-apps/foundry_vtt.nix`), with the
integration research kept in canix planning docs.
