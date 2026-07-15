# Foundry Circle work standard

Do not fabricate Foundry schemas, API-user credentials, OIDC claims, licensed
archives, route topology, or provenance fragments. If one is missing, report
the producer, the exact regeneration workflow, and the validation command.

Foundry is the authority for world documents. PostgreSQL is control-plane state
only. Production authentication trusts Rauthy through nix-provenance and does
not accept direct Kanidm tokens, local passwords, or arbitrary JavaScript.

Use Crane/rs-harbor for Rust builds and Dioxus/tartan-ui for the operator
console. Keep production asset and API paths below `/api`.
