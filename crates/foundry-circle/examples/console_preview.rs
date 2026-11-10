//! Renders the real console `App` to static HTML for bounded visual checks.
//!
//! This is a native SSR preview, not the hydrated production route: it does not
//! initialize OIDC, PostgreSQL, or the Foundry driver. Run it with the server
//! feature disabled and serve the output on loopback only.
//!
//! `TARTAN_CSS` points at the stylesheet from the pinned tartan-ui revision.
//! The preview injects the same broker and console styles that the real app
//! registers in the document head, because `render_element` does not process
//! head components.

use dioxus::prelude::*;
use std::env;

fn main() {
    let body = dioxus_ssr::render_element(rsx! { foundry_circle::App {} });
    let tartan = std::fs::read_to_string(
        env::var("TARTAN_CSS").expect("TARTAN_CSS must point at the pinned tartan stylesheet"),
    )
    .expect("read tartan stylesheet");
    let theme = theme_broker::stylesheet();
    let console = foundry_circle::console::STYLES;
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Foundry Circle</title>\
         <style>{tartan}\n{theme}\n{console}</style></head><body>{body}</body></html>"
    );
    print!("{html}");
}
