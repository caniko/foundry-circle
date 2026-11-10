# Intermediate review adjudication

The raw review is retained unchanged in [intermediate-P.md](intermediate-P.md). Its incomplete review and numerical results do not establish acceptance.

| ID | Decision | Evidence / next verification |
| --- | --- | --- |
| P-01 | Reject proposed removal of navigation; unverified runtime claim, not an observed defect | Production pages are protected by `auth::require_session` in `main.rs`; the discovery link already says authenticated/session required. The reviewer did not exercise endpoint responses. Disabling a genuine expert control would regress the brief. Preserve the link; clarify expired/missing-session behavior only if verified. |
| P-02 | Accept the guidance improvement; correct the asserted response shape | `http.rs::readyz` returns HTTP 200/503 with JSON `{state,database,ready}`, not an `error` envelope. Explain expected 503/non-ready behavior without suggesting failure means certification. |
| P-03 | Reject claimed contrast failure pending browser measurement | The review substituted light-theme `#7c6f64`/`#5a5550` for dark tokens. The source-rendered wrapper is explicitly dark. Canonical dark `syntax.comment` is `#928374`, muted is `#bdae93`. Unrounded sRGB calculation for `#928374` on the actual card `#1a1a1a` is 4.741360789979879:1; on black 5.720909147581965:1. These calculations are not a browser contrast audit. Measure computed rendered colors before accepting a contrast defect. Tiny 10–11px operational text is a separate readability issue worth improving. |
| P-04 | Record as tradeoff, not P2 | The reviewer explicitly says it is not a defect. Normal vertical scrolling is allowed. `typed-world-v1` remains in the footer; retain access to context at narrow widths. |
| P-05 | Accept | Remove repeated no-request disclaimers while keeping one clear, honest unknown/not-checked statement. |
| P-06 | Accept as design-parity work | Use a verified matching font or update the editable design to the shipped system font; do not claim exact parity from different font metrics. |
| P-07 | Verification gap remains open | Existing capture is explicitly zoom-equivalent emulation. It cannot silently satisfy a real browser-zoom check. Preserve the capture and obtain actual zoom/text-resizing evidence if available; otherwise acceptance remains partial. |
| P-08 | Optional finishing issue | Improve skip-link focus placement if it can preserve the existing native keyboard behavior without unnecessary controls. |

## Additional source-backed findings

- **O-01, P2, inferred from CSS and observed spacing:** `.console-root p/pre/ul/... { margin: 0 }` has greater specificity than `.console-copy`, `.console-note`, `.console-code`, `.console-links`, and other single-class margin rules. Requested margins do not apply, accounting for the compressed implementation spacing versus the design export. Fix the shared reset's specificity once (for example, `:where(...)`), then measure computed margins and recapture affected states.
- **O-02, P2, inferred from CSS and observed narrow indentation:** the narrow `.console-card { padding: 18px }` overrides the earlier single-class `.console-details { padding: 0 }`; summary/body padding is then added again. The prerequisites block is visibly indented further than peer cards. Keep the disclosure container unpadded at all breakpoints through one appropriately scoped rule, then verify 320/390 layouts.
- **O-03, P2, content precision:** the card says document/command handlers return `driver_contract_pending` without qualification. They return `world_not_ready` before a world is ready; ready-world paths still return `driver_contract_pending`. Clarify the distinction without changing any API.

Next implementation should target these actual source/interaction risks, not alter correct palette values or disable navigation to satisfy unsupported findings. No code/design writer has been dispatched while the current evidence-preservation worker is freezing P.
