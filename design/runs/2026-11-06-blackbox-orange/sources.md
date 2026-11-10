# Integration sources

## Theme broker

Producer: `https://github.com/caniko/nix-theme-broker`, published revision `483595220b0c5c07fe2010ec596020d0b73d13a1` (merge of Blackbox accents, 2026-11-03).

- `providers/blackbox/default.nix`: provider `blackbox`; dark variant; default accent `orange`; Orange resolves to `@bright_orange`. Accent bindings select `ui.accent` and `ui.focus`.
- `providers/blackbox/palette.json`: authoritative inventory, not the preview render. Near-black surfaces and cream text are intentional. No palette is named `BlackboxOrgange`.
- `crates/theme-broker/src/lib.rs`: `stylesheet()` supplies generated `--tb-*` CSS; `role(Mode::Dark, group, name)` supplies canonical role values. This published Rust snapshot selects blue for web by default. Select Orange explicitly rather than claiming the default snapshot is Orange. `syntax.constant` is canonical bright Orange; no copied hex inventory is necessary.
- `crates/theme-broker/generated/theme.css`: dark CSS uses `[data-theme=dark]`; the explicit attribute is necessary for a fixed dark console regardless of OS preference.
- The separate Honey feature branch is not this requested theme and must not be substituted.

## Tartan UI

Existing consumer pin: `d8f994da26846b6785194e7969a5d31d9fc050f1`. Its shared layout already aliases `--tartan-*` to theme-broker `--tb-*`. Use that seam; a framework migration or unrequested light/dark toggle is unnecessary.

## Existing product contract

- `crates/foundry-circle/src/http.rs`: `/api/v1/readyz` is public, reports world state, database availability, and ready. `/api/v1` is authenticated API discovery. Empty capabilities are intentional pending certification. The UI must not label unknown service status as healthy or connected.
- `crates/foundry-circle/src/auth.rs`: Rauthy/OIDC authentication and browser-bound PKCE; no production auth changes in this run.
- `nix/module.nix`: real configuration lives under `services.foundry-circle`; world, PostgreSQL, and Rauthy prerequisites are separate. No API-user secret values belong in the console or test evidence.
- `README.md`: world documents remain unavailable until a licensed archive and disposable v13.351 world certify the browser driver. An explanatory UI is not certification evidence.
- `flake.nix` uses Crane `cleanCargoSource`; the pinned `filterCargoSources.nix` includes only directories, Rust/TOML/Cargo files. New local CSS files need explicit source-filter inclusion, or styles must remain in a Rust source constant. A checkout-only render does not prove packaged source completeness.

## Accessibility verification basis

[W3C WCAG 2.2 contrast minimum](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html), consulted 2026-11-06: normal text ≥4.5:1; large text ≥3:1; compare unrounded ratios against the threshold. Measure browser-computed foreground/background colors, not screenshot pixels. Keyboard focus and responsive reflow require runtime checks beyond screenshots.

[Reflow](https://www.w3.org/WAI/WCAG22/Understanding/reflow.html) requires the relevant vertical content to remain readable at 320 CSS pixels without two-dimensional page scrolling. [Focus visible](https://www.w3.org/WAI/WCAG22/Understanding/focus-visible.html) requires a visible, persistent keyboard focus indicator. [Target size minimum](https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html) specifies 24×24 CSS pixels or its documented spacing/inline exceptions.

The [Dioxus SSR 0.7.9 render API](https://docs.rs/dioxus-ssr/0.7.9/dioxus_ssr/fn.render.html) renders an existing `VirtualDom` to a string. This supports a source-rendered isolated preview, not a claim that a production WASM bundle or authenticated deployment was tested. Native [details/summary](https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/details) can provide the setup disclosure without a separate client-side state machine.
