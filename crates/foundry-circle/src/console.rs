//! Static operator console for the pending Foundry contract.
//!
//! The screen is deliberately read-only. It explains the contract boundary,
//! surfaces the real Nix configuration identifiers, and links to the genuine
//! readiness/discovery endpoints without claiming that they were checked.

use dioxus::prelude::*;
use tartan_ui_core::Metric;
use tartan_ui_dioxus::MetricStrip;
use theme_broker::{Mode, role, stylesheet};

/// Canonical Blackbox dark Orange role. The generated web snapshot selects blue
/// for its default accent, so the console selects the producer's Orange token
/// explicitly and fails fast if the role disappears.
fn orange() -> &'static str {
    role(Mode::Dark, "syntax", "constant")
        .expect("theme-broker Blackbox dark must expose syntax.constant")
}

/// Console-only bridge and layout. Palette values stay in the producer
/// stylesheet; this file contains no copied palette hexes.
pub const STYLES: &str = r##"
/* The pinned tartan stylesheet still uses literal --tartan-* values, so the
   broker aliases are rebound once at this scope instead of per component. */
.console-root {
  --tb-ui-accent: var(--console-orange);
  --tb-ui-focus: var(--console-orange);
  --tb-ui-link: var(--console-orange);
  --tartan-paper: var(--tb-ui-background);
  --tartan-ink: var(--tb-ui-foreground);
  --tartan-muted: var(--tb-ui-foreground-muted);
  --tartan-line: var(--tb-ui-border);
  --tartan-accent: var(--console-orange);
  --tartan-accent-soft: var(--tb-ui-selection-background);
  --tartan-on-accent: var(--tb-ui-background);
  --tartan-surface: var(--tb-ui-surface);
  --tartan-danger: var(--tb-status-error);
  --tartan-warning: var(--tb-status-warning);
  --tartan-success: var(--tb-status-success);
  --tartan-info: var(--tb-status-info);
  --tartan-focus: var(--console-orange);
  min-height: 100vh;
  background: var(--tb-ui-background);
  color: var(--tb-ui-foreground);
  font-family: ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
  font-size: 14px;
  line-height: 1.5;
  -webkit-font-smoothing: antialiased;
}

.console-root *,
.console-root *::before,
.console-root *::after {
  box-sizing: border-box;
}

html,
body {
  margin: 0;
  padding: 0;
}

body {
  background: var(--tb-ui-background);
}

.console-root h1,
.console-root h2,
.console-root h3,
.console-root p,
.console-root ul,
.console-root ol,
.console-root dl,
.console-root dd,
.console-root pre {
  margin: 0;
}

.console-root ul,
.console-root ol {
  padding: 0;
  list-style: none;
}

.console-root code,
.console-root pre {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}

.console-skip {
  position: absolute;
  left: 16px;
  top: -48px;
  z-index: 10;
  border: 1px solid var(--console-orange);
  border-radius: 4px;
  background: var(--tb-ui-background);
  color: var(--tb-ui-foreground);
  padding: 8px 12px;
  text-decoration: none;
}

.console-skip:focus {
  top: 12px;
}

.console-header {
  border-bottom: 1px solid var(--tb-ui-border);
}

.console-header__inner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 24px;
  width: min(100% - 48px, 1216px);
  min-height: 64px;
  margin: 0 auto;
}

.console-brand {
  display: grid;
  gap: 2px;
  min-width: 0;
}

.console-brand__name {
  color: var(--tb-ui-selection-foreground);
  font-size: 16px;
  font-weight: 700;
  letter-spacing: -0.01em;
}

.console-brand__context {
  color: var(--tb-syntax-comment);
  font-size: 10px;
  letter-spacing: 0.04em;
}

.console-pill {
  display: inline-flex;
  flex: none;
  align-items: center;
  gap: 8px;
  border: 1px solid var(--tb-ui-border);
  border-radius: 4px;
  background: var(--tb-ui-background-alt);
  color: var(--console-orange);
  padding: 5px 10px;
  font-size: 11px;
  white-space: nowrap;
}

.console-pill__dot,
.console-badge__dot {
  width: 6px;
  height: 6px;
  flex: none;
  border-radius: 50%;
  background: currentColor;
}

.console-main {
  width: min(100% - 48px, 1216px);
  margin: 0 auto;
  padding: 32px 0 24px;
}

.console-hero {
  max-width: 840px;
}

.console-kicker {
  color: var(--console-orange);
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.16em;
  text-transform: uppercase;
}

.console-hero h1 {
  margin-top: 8px;
  color: var(--tb-ui-selection-foreground);
  font-size: clamp(2rem, 4vw, 2.5rem);
  font-weight: 700;
  line-height: 1.1;
  letter-spacing: -0.02em;
}

.console-lede {
  max-width: 72ch;
  margin-top: 14px;
  color: var(--tb-ui-foreground-muted);
  font-size: 14px;
  line-height: 1.55;
}

.console-status {
  margin-top: 28px;
  border-top: 1px solid var(--tb-ui-border);
  padding-top: 14px;
}

.console-status .tartan-metrics {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 24px;
  border: 0;
  padding: 0;
}

.console-status .tartan-metric {
  display: grid;
  gap: 2px;
  min-width: 0;
}

.console-status .tartan-metric span {
  order: 1;
  color: var(--tb-syntax-comment);
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.11em;
  text-transform: uppercase;
}

.console-status .tartan-metric strong {
  order: 2;
  color: var(--tb-ui-selection-foreground);
  font-size: 14px;
  font-weight: 700;
  line-height: 1.35;
}

.console-status .tartan-metric small {
  order: 3;
  color: var(--tb-ui-foreground-muted);
  font-size: 11px;
  line-height: 1.45;
}

.console-grid {
  display: grid;
  grid-template-columns: minmax(0, 1.9fr) minmax(0, 1fr);
  gap: 24px;
  margin-top: 28px;
}

.console-column {
  display: flex;
  flex-direction: column;
  gap: 16px;
  min-width: 0;
}

.console-card {
  min-width: 0;
  border: 1px solid var(--tb-ui-border);
  border-radius: 6px;
  background: var(--tb-ui-background-alt);
  padding: 24px;
}

.console-card h2 {
  margin-top: 8px;
  color: var(--tb-ui-selection-foreground);
  font-size: 20px;
  font-weight: 700;
  line-height: 1.3;
  letter-spacing: -0.01em;
}

.console-copy {
  margin-top: 12px;
  color: var(--tb-ui-foreground-muted);
  font-size: 13px;
  line-height: 1.55;
}

.console-copy code,
.console-note code,
.console-prereqs code,
.console-identifiers code,
.console-source code {
  color: var(--tb-ui-foreground);
  overflow-wrap: anywhere;
}

.console-code {
  margin-top: 14px;
  border: 1px solid var(--tb-ui-border);
  border-radius: 4px;
  background: var(--tb-ui-background);
  color: var(--tb-ui-foreground);
  padding: 10px 12px;
  font-size: 11px;
  line-height: 1.6;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.console-note {
  margin-top: 14px;
  color: var(--tb-syntax-comment);
  font-size: 11px;
  line-height: 1.5;
}

.console-details {
  padding: 0;
}

.console-details > summary {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  gap: 4px 16px;
  align-items: start;
  cursor: pointer;
  padding: 20px 24px;
  list-style: none;
}

.console-details > summary::-webkit-details-marker {
  display: none;
}

.console-details > summary::after {
  grid-column: 2;
  grid-row: 1 / span 2;
  align-self: center;
  color: var(--console-orange);
  content: "+";
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 18px;
  line-height: 1;
}

.console-details[open] > summary::after {
  content: "−";
}

.console-details__title {
  grid-column: 1;
  color: var(--tb-ui-selection-foreground);
  font-size: 16px;
  font-weight: 700;
}

.console-details__hint {
  grid-column: 1;
  color: var(--tb-syntax-comment);
  font-size: 11px;
}

.console-details__body {
  padding: 0 24px 22px;
}

.console-prereqs {
  display: grid;
  gap: 14px;
}

.console-prereqs li {
  display: grid;
  grid-template-columns: 28px minmax(0, 1fr);
  gap: 8px;
  min-width: 0;
}

.console-prereqs__num {
  color: var(--console-orange);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 10px;
  line-height: 1.6;
}

.console-prereqs h3 {
  color: var(--tb-ui-selection-foreground);
  font-size: 13px;
  font-weight: 700;
}

.console-prereqs p {
  margin-top: 2px;
  color: var(--tb-ui-foreground-muted);
  font-size: 11px;
  line-height: 1.5;
  overflow-wrap: anywhere;
}

.console-diagnostics {
  display: flex;
  flex-direction: column;
  align-self: start;
}

.console-badge {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  width: fit-content;
  margin-top: 12px;
  border: 1px solid var(--tb-ui-border);
  border-radius: 3px;
  background: var(--tb-ui-background);
  color: var(--tb-ui-foreground-muted);
  padding: 4px 8px;
  font-size: 10px;
}

.console-badge__dot {
  color: var(--tb-syntax-comment);
}

.console-links {
  margin-top: 14px;
}

.console-links li + li {
  border-top: 1px solid var(--tb-ui-border);
}

.console-link {
  display: grid;
  gap: 2px;
  padding: 10px 0;
  color: inherit;
  text-decoration: none;
}

.console-link__head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
}

.console-link__path {
  color: var(--tb-ui-selection-foreground);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 13px;
  overflow-wrap: anywhere;
}

.console-link__arrow {
  flex: none;
  color: var(--console-orange);
  font-size: 12px;
}

.console-link__title {
  color: var(--tb-ui-foreground-muted);
  font-size: 11px;
  font-weight: 700;
}

.console-link__detail {
  color: var(--tb-syntax-comment);
  font-size: 11px;
  line-height: 1.45;
}

.console-link:hover .console-link__path,
.console-link:hover .console-link__arrow {
  color: var(--console-orange);
}

.console-link:focus-visible,
.console-details > summary:focus-visible,
.console-skip:focus-visible {
  outline: 2px solid var(--console-orange);
  outline-offset: 3px;
}

.console-boundaries {
  margin-top: 16px;
  border-top: 1px solid var(--tb-ui-border);
  padding-top: 14px;
}

.console-boundaries ul {
  display: grid;
  gap: 3px;
  margin-top: 6px;
  color: var(--tb-ui-foreground-muted);
  font-size: 11px;
}

.console-boundaries li::before {
  color: var(--tb-syntax-comment);
  content: "· ";
}

.console-boundary-line {
  margin-top: 16px;
  color: var(--tb-syntax-comment);
  font-size: 11px;
  line-height: 1.5;
}

.console-source {
  margin-top: 12px;
  color: var(--tb-syntax-comment);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 10px;
}

.console-footer {
  border-top: 1px solid var(--tb-ui-border);
  width: min(100% - 48px, 1216px);
  margin: 20px auto 0;
  padding: 12px 0 24px;
  color: var(--tb-syntax-comment);
  font-size: 10px;
}

.console-footer span {
  color: var(--tb-ui-foreground-muted);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

.sr-only {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  margin: -1px;
  overflow: hidden;
  clip: rect(0, 0, 0, 0);
  white-space: nowrap;
  border: 0;
}

@media (max-width: 980px) {
  .console-grid {
    grid-template-columns: minmax(0, 1fr);
  }

  .console-diagnostics {
    align-self: stretch;
  }
}

@media (max-width: 560px) {
  .console-header__inner,
  .console-main,
  .console-footer {
    width: calc(100% - 32px);
  }

  .console-header__inner {
    min-height: 56px;
    gap: 12px;
  }

  .console-brand__context {
    display: none;
  }

  .console-main {
    padding-top: 24px;
  }

  .console-hero h1 {
    font-size: 2rem;
  }

  .console-status .tartan-metrics {
    grid-template-columns: minmax(0, 1fr);
    gap: 12px;
  }

  .console-status .tartan-metric {
    border-top: 1px solid var(--tb-ui-border);
    padding-top: 10px;
  }

  .console-status .tartan-metric:first-child {
    border-top: 0;
    padding-top: 0;
  }

  .console-card {
    padding: 18px;
  }

  .console-details > summary {
    padding: 18px;
  }

  .console-details__body {
    padding: 0 18px 18px;
  }

  .console-prereqs li {
    grid-template-columns: 22px minmax(0, 1fr);
  }

  .console-footer {
    margin-top: 16px;
  }
}

@media (prefers-reduced-motion: reduce) {
  .console-root *,
  .console-root *::before,
  .console-root *::after {
    scroll-behavior: auto;
    transition-duration: 0.01ms;
    animation-duration: 0.01ms;
    animation-iteration-count: 1;
  }
}
"##;

#[component]
pub fn App() -> Element {
    let orange = orange();
    let metrics = vec![
        Metric {
            label: "World documents".to_string(),
            value: "Not exposed".to_string(),
            description: Some("No collection advertised".to_string()),
        },
        Metric {
            label: "Commands".to_string(),
            value: "Not exposed".to_string(),
            description: Some("No command advertised".to_string()),
        },
        Metric {
            label: "Readiness".to_string(),
            value: "Not checked".to_string(),
            description: Some("No endpoint called from this screen".to_string()),
        },
    ];

    rsx! {
        document::Title { "Foundry Circle" }
        document::Stylesheet { href: tartan_ui_dioxus::SHARED_STYLES }
        document::Style { {stylesheet()} }
        document::Style { {STYLES} }
        div {
            class: "console-root",
            "data-theme": "dark",
            style: "--console-orange: {orange}",
            a { class: "console-skip", href: "#console-main", "Skip to console content" }
            header { class: "console-header",
                div { class: "console-header__inner",
                    div { class: "console-brand",
                        span { class: "console-brand__name", "Foundry Circle" }
                        span { class: "console-brand__context", "Operator console · typed-world-v1" }
                    }
                    div { class: "console-pill",
                        span { class: "console-pill__dot", aria_hidden: "true" }
                        span { "Contract pending" }
                    }
                }
            }
            main { id: "console-main", class: "console-main", tabindex: "-1",
                section { class: "console-hero", aria_labelledby: "console-title",
                    p { class: "console-kicker", "Foundry VTT · typed broker" }
                    h1 { id: "console-title", "World contract pending" }
                    p { class: "console-lede",
                        "Document and command views are not available. The API advertises no world collections or commands, and the live driver contract is still pending."
                    }
                }

                section { class: "console-status", aria_label: "Contract status",
                    MetricStrip { metrics }
                }

                div { class: "console-grid",
                    div { class: "console-column",
                        section { class: "console-card", aria_labelledby: "contract-title",
                            p { class: "console-kicker", "Contract boundary" }
                            h2 { id: "contract-title", "Document and command views are unavailable" }
                            p { class: "console-copy",
                                "The broker exposes the pending contract honestly: no collections or commands are advertised, and document and command handlers return "
                                code { "driver_contract_pending" }
                                "."
                            }
                            pre { class: "console-code", aria_label: "Advertised capabilities",
                                code { "capabilities.collections = []\ncapabilities.commands = []" }
                            }
                            p { class: "console-note",
                                "A ready world is not sufficient. Live browser-driver certification and the document/command implementation remain pending."
                            }
                        }

                        details { class: "console-details console-card", open: true,
                            summary { class: "console-details__summary",
                                span { class: "console-kicker", "Operator setup" }
                                span { class: "console-details__title", "Prerequisites" }
                                span { class: "console-details__hint", "3 configuration groups" }
                            }
                            div { class: "console-details__body",
                                ol { class: "console-prereqs",
                                    li {
                                        span { class: "console-prereqs__num", "01" }
                                        div {
                                            h3 { "Rauthy OIDC" }
                                            p {
                                                "Set "
                                                code { "services.foundry-circle.oidc.issuer" }
                                                ", "
                                                code { "clientId" }
                                                ", and "
                                                code { "publicBaseUrl" }
                                                ". Production trusts Rauthy through nix-provenance; Kanidm is upstream, not a direct client."
                                            }
                                        }
                                    }
                                    li {
                                        span { class: "console-prereqs__num", "02" }
                                        div {
                                            h3 { "PostgreSQL" }
                                            p {
                                                "Configure "
                                                code { "services.foundry-circle.database.name" }
                                                ", "
                                                code { "database.user" }
                                                ", and "
                                                code { "database.urlFile" }
                                                ". PostgreSQL stores control-plane state only, not Foundry world documents."
                                            }
                                        }
                                    }
                                    li {
                                        span { class: "console-prereqs__num", "03" }
                                        div {
                                            h3 { "Foundry world" }
                                            p {
                                                "Configure "
                                                code { "foundryWorld.baseUrl" }
                                                ", "
                                                code { "apiUser" }
                                                " (default "
                                                code { "foundry-circle-api" }
                                                "), "
                                                code { "apiPasswordFile" }
                                                ", "
                                                code { "worldId" }
                                                ", "
                                                code { "foundryVersion" }
                                                ", "
                                                code { "systemId" }
                                                ", and "
                                                code { "systemVersion" }
                                                "."
                                            }
                                        }
                                    }
                                }
                                p { class: "console-note",
                                    "These settings prepare the service. They do not certify the live driver or enable document/command views. Certification remains a separate gate: a licensed Foundry archive and a disposable v13.351 world. The repository does not contain that certification workflow."
                                }
                            }
                        }

                    }

                    aside { class: "console-card console-diagnostics", aria_labelledby: "diagnostics-title",
                        p { class: "console-kicker", "Diagnostics" }
                        h2 { id: "diagnostics-title", "Readiness and discovery" }
                        p { class: "console-badge",
                            span { class: "console-badge__dot", aria_hidden: "true" }
                            "Not checked from this screen"
                        }
                        ul { class: "console-links",
                            li {
                                a { class: "console-link", href: "/api/v1/readyz", target: "_blank", rel: "noopener",
                                    span { class: "console-link__head",
                                        span { class: "console-link__path", "/api/v1/readyz" }
                                        span { class: "console-link__arrow", aria_hidden: "true", "↗" }
                                    }
                                    span { class: "console-link__title", "Public readiness probe" }
                                    span { class: "console-link__detail",
                                        "World state, database readiness, and overall ready. This screen does not call it."
                                    }
                                    span { class: "sr-only", "(opens in a new tab)" }
                                }
                            }
                            li {
                                a { class: "console-link", href: "/api/v1", target: "_blank", rel: "noopener",
                                    span { class: "console-link__head",
                                        span { class: "console-link__path", "/api/v1" }
                                        span { class: "console-link__arrow", aria_hidden: "true", "↗" }
                                    }
                                    span { class: "console-link__title", "Authenticated API discovery" }
                                    span { class: "console-link__detail",
                                        "Lists the available links. Requires an operator session."
                                    }
                                    span { class: "sr-only", "(opens in a new tab)" }
                                }
                            }
                            li {
                                a { class: "console-link", href: "/api/v1/healthz", target: "_blank", rel: "noopener",
                                    span { class: "console-link__head",
                                        span { class: "console-link__path", "/api/v1/healthz" }
                                        span { class: "console-link__arrow", aria_hidden: "true", "↗" }
                                    }
                                    span { class: "console-link__title", "Public liveness probe" }
                                    span { class: "console-link__detail",
                                        "Returns ok when HTTP is up; not world certification."
                                    }
                                    span { class: "sr-only", "(opens in a new tab)" }
                                }
                            }
                        }
                        div { class: "console-boundaries",
                            p { class: "console-kicker", "This screen does not verify" }
                            ul {
                                li { "No readiness or discovery request is made." }
                                li { "No operator identity is loaded." }
                                li { "No world snapshot or capability list is loaded." }
                            }
                        }
                        p { class: "console-boundary-line",
                            "Foundry remains authoritative for world documents. PostgreSQL stores control-plane state only. Rauthy is the production OIDC issuer."
                        }
                        p { class: "console-source",
                            "Source: "
                            code { "http.rs" }
                            " · "
                            code { "nix/module.nix" }
                        }
                    }
                }
            }
            footer { class: "console-footer",
                span { "typed-world-v1" }
                " · No world data is loaded by this screen. Endpoint responses are the service evidence."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blackbox_orange_bridge_uses_producer_tokens() {
        let orange = role(Mode::Dark, "syntax", "constant")
            .expect("theme-broker exposes the dark syntax.constant role");
        assert_eq!(
            Some(orange),
            role(Mode::Dark, "status", "warning"),
            "Blackbox Orange is the canonical warning/constant token"
        );
        let broker_css = stylesheet();
        assert!(broker_css.contains("[data-theme=dark]"));
        assert!(broker_css.contains(&format!("--tb-syntax-constant: {orange};")));
        assert!(STYLES.contains("--tartan-accent: var(--console-orange)"));
        assert!(STYLES.contains("--tartan-focus: var(--console-orange)"));
        assert!(STYLES.contains("--tb-ui-accent: var(--console-orange)"));
        assert!(STYLES.contains("--tb-ui-focus: var(--console-orange)"));
        assert!(STYLES.contains("--tb-ui-link: var(--console-orange)"));
        assert!(STYLES.contains("--tartan-paper: var(--tb-ui-background)"));
    }

    #[test]
    fn console_exposes_real_links_and_does_not_claim_readiness() {
        let html = dioxus_ssr::render_element(rsx! { App {} });
        assert!(html.contains("World contract pending"));
        assert!(html.contains("href=\"/api/v1/readyz\""));
        assert!(html.contains("href=\"/api/v1\""));
        assert!(html.contains("href=\"/api/v1/healthz\""));
        assert!(html.contains("Not checked from this screen"));
        assert!(!html.contains("Connected"));
        assert!(!html.contains("healthy"));
    }
}
