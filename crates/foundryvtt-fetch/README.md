# foundryvtt-fetch

`foundryvtt-fetch` acquires a licensed Foundry VTT release and produces a
validated, content-addressed archive for a Nix update. It follows the
official account flow used by the Foundry Docker ecosystem, but keeps
credentials in files, never logs signed URLs, rejects unsafe ZIP entries, and
records a JSON provenance sidecar next to the cached archive.

The crate does not redistribute Foundry content. Operators must provide a
licensed account or a pre-acquired archive and should keep the cache private.

For declarative Nix builds, `foundryvtt-fetchd` is run as the dedicated
`foundryvtt-acquire` systemd user. Its password arrives through
`LoadCredential`, and build users can only request an exact release/hash over
the local Unix socket. `foundryvtt-fetch-hook` inspects the Nix derivation
markers and emits the `extra-sandbox-paths` mapping after the daemon validates
the archive. The password is never an argument, environment value, derivation
input, or Nix store path.

## Acquisition

`foundryvtt-fetch acquire` applies this precedence:

1. `--release-url-file` (a private, time-limited presigned URL);
2. `--username-file` plus `--password-file` (the account/CSRF flow); and
3. the exact validated cache entry. `--offline` selects only the last source.

The command emits JSON containing the archive path, SHA-256, Nix SRI hash,
release, source kind, size, and acquisition timestamp. It never emits
credentials or presigned URLs. Interactive caches are atomically copied into a
mode `0700` cache with mode `0600` contents; the daemon's local-only cache is
changed to mode `2750`/`0440` with the trusted `nixbld` group so Nix can read a
validated archive without receiving account credentials.

## Declarative package links

The `reconcile` subcommand consumes the Nix-generated desired manifest and
maintains only recorded symlinks under `Data/modules` and `Data/systems`:

```text
foundryvtt-fetch reconcile \
  --desired /nix/store/...-foundry-circle-packages.json \
  --data-dir /var/lib/foundryvtt \
  --state-file /var/lib/foundryvtt/.foundry-circle-packages.json
```

Version changes replace a previously recorded link atomically. Deletion is an
explicit `state = "absent"` tombstone; foreign directories and symlinks fail
closed. The immutable package outputs are produced by
`foundry-circle.lib.mkFoundryPackage` and are never modified in place.
