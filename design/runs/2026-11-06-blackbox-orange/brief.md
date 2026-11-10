# Foundry Circle operator console

Scope: design plus implementation of the existing `/api/console` pending-contract screen. Preserve Dioxus, tartan-ui, production authentication, and all existing API behavior. Use the canonical theme-broker BlackboxOrange palette, verified from its producer; do not invent palette values or world data.

Users: operators connecting and certifying a Foundry VTT world. The console currently contains only a header and pending-contract message. The backend explicitly advertises no implemented world collections or commands. Improve this honest empty state rather than implying a functional world-management product.

## Critical tasks

| Task | Start and friction | Outcome / observable success | Retain |
| --- | --- | --- | --- |
| Understand availability | Open the pending console; little explanation of what is blocked | Identify the pending contract and why document/command views are unavailable without navigating elsewhere | No fabricated connection, health, identity, or world metadata |
| Find setup guidance | Operator needs the next prerequisite | Read clear ordered prerequisites and the real configuration identifiers already documented in source | Foundry authority, Rauthy trust, licensed-artifact boundary |
| Inspect diagnostics | Operator needs service evidence, not inferred status | Reach the existing readiness and API discovery endpoints through clearly labeled links | `/api/v1/readyz`, `/api/v1`, honest unknown/not-checked state |
| Navigate accessibly | Keyboard or narrow/zoomed display | Reach every available action, see focus, read expanded help without horizontal page overflow | Native semantics, useful technical detail, no dead navigation |

Visual character: compact technical console, near-black surfaces, purposeful orange emphasis, strong readable hierarchy, restrained borders and radii. Preserve product identity and information density. No speculative management controls, fake charts, fake identities, new framework, major dependency, or infrastructure edits.

## Frozen evidence matrix

- `/api/console`: desktop 1440×1000, intermediate 1024×900, narrow 390×844, minimum 320×800; dark BlackboxOrange, English, device scale 1, zoom 100%.
- Default pending/unknown state; setup disclosure expanded/collapsed where present; keyboard focus; long technical identifiers; 200% desktop zoom; reduced motion.
- Baseline and candidate use the same source-rendered console and content context. Capture after document fonts are ready.
- No authenticated live environment or private data is required. Any local preview must render the real Dioxus App, bind loopback only, and never initialize production auth, Foundry, or a database. Clearly distinguish preview route behavior from production endpoint verification.
- Loading, success, save, validation, and mutation flows are not applicable to this existing static pending screen. Do not add a fake state just to populate the matrix. Actual endpoint errors remain separately scoped API behavior, not a simulated UI feature.
- Screen-reader user testing, human usability testing, production login/deployment, full production builds, and benchmark superiority claims are not requested and remain unverified/out of scope. Browser semantics and keyboard behavior are in scope.

References: the existing Foundry Circle console and canonical theme-broker/tartan-ui source. No third-party product comparison or superiority claim.

Review rubric: `/home/can/.config/opencode/skills/openpencil-designer/references/review-rubric.md`, frozen unchanged. Separate fresh-context reviews inspect the exact frozen design, browser evidence, changed source, and check receipts. Numerical acceptance target is withheld from review packets.
