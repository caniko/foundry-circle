# Foundry Circle

<!-- simit:badges:start -->

![CI](https://img.shields.io/badge/CI-managed-2088ff) [![Nix](https://img.shields.io/badge/Nix-managed-5277c3)](flake.nix) [![docs](https://img.shields.io/badge/docs-enabled-6f42c1)](https://docs.rs/foundry-circle) [![release](https://img.shields.io/badge/release-configured-2ea44f)](.forgejo/workflows/release.yml) [![artifacts](https://img.shields.io/badge/artifacts-configured-2ea44f)](.forgejo/workflows/release.yml)

<!-- simit:badges:end -->

Foundry Circle is a Rust/Axum broker for the active Foundry VTT world, with a
Dioxus 0.7 operator console using tartan-ui. PostgreSQL stores control-plane
state only; Foundry remains authoritative for world documents.

The repository is intentionally bootstrapped with health, discovery, a typed
driver seam, and a non-ready readiness contract. The live browser driver,
OIDC, migrations, capability registry, and route integrations are added only
after the licensed Foundry archive and disposable v13.351 world are available.

## Development

```bash
cargo check --all-targets --all-features
cargo test --all-targets --all-features
```

The production service binds to `127.0.0.1:8032` by default. It does not
accept a Foundry password through command-line arguments or store credentials
in the repository.

Phase 02 also ships the standalone `foundryvtt-fetch` crate. It acquires
licensed releases outside Nix evaluation, records SHA-256/Nix-SRI provenance,
and reconciles immutable module/system outputs from
`services.foundryvtt.declarativePackages`. Use `state = "absent"` for an
explicit package deletion; worlds remain mutable Foundry-owned state.

## Boundaries

- Kanidm is the primary internal OIDC issuer; Rauthy remains the
  nix-provenance-managed federation surface for outward identity flows.
- SQLite is not supported; PostgreSQL is the only database backend.
- Arbitrary JavaScript/evaluate endpoints are not part of the production
  feature set.
- Licensed Foundry archives must not be published to the shared Attic cache.
