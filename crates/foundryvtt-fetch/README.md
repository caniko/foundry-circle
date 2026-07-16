# foundryvtt-fetch

`foundryvtt-fetch` acquires a licensed Foundry VTT release and produces a
validated, content-addressed archive for a Nix update.  It follows the
official account flow used by the Foundry Docker ecosystem, but keeps
credentials in files, never logs signed URLs, rejects unsafe ZIP entries, and
records a JSON provenance sidecar next to the cached archive.

The crate does not redistribute Foundry content.  Operators must provide a
licensed account or a pre-acquired archive and should keep the cache private.
