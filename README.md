# Foundry Circle

<!-- simit:badges:start -->

[![CI](https://img.shields.io/badge/CI-managed-2088ff)](.forgejo/workflows/ci.yaml) [![Nix](https://img.shields.io/badge/Nix-managed-5277c3)](flake.nix) [![crates.io](https://img.shields.io/badge/crates.io-ready-f46623)](https://crates.io/crates/foundry-circle)

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

The production service binds to `127.0.0.1:8031` by default. It does not
accept a Foundry password through command-line arguments or store credentials
in the repository.

## Boundaries

- Kanidm is the credential source; Rauthy is the only OIDC issuer trusted by
  the service.
- SQLite is not supported; PostgreSQL is the only database backend.
- Arbitrary JavaScript/evaluate endpoints are not part of the production
  feature set.
- Licensed Foundry archives must not be published to the shared Attic cache.
