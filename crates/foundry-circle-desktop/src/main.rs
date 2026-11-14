//! Foundry Circle's desktop asset uploader.
//!
//! One window over the `foundry-circle-upload` library: sign in through the
//! system browser, choose the conversion policy, then convert and upload files
//! one at a time. This crate owns no protocol of its own — the HTTP, PKCE, and
//! media-oxide conversion contracts all come from the library, which the CLI
//! uses too.

use std::path::PathBuf;

use dioxus::html::HasFileData;
use dioxus::prelude::*;
use foundry_circle_upload::{
    AudioCodec, Error, Session, convert, forget_session, load_session, login, open_browser,
    save_session, staged_asset, upload, whoami,
};
use tartan_ui_core::{Feedback, FeedbackKind};
use tartan_ui_dioxus::{DashboardHeader, FeedbackBanner, Field, Panel, SHARED_STYLES};

mod model;

use model::{
    AUDIO_KBPS_MAX, AUDIO_KBPS_MIN, GRID_PX_DEFAULT, Job, JobStatus, KindChoice, MessagePart,
    SessionGeneration, TOKEN_PX_MAX, TOKEN_PX_MIN, UiOptions, account_label, clamp_audio_kbps,
    clamp_token_px, codec_from_value, codec_label, codec_value, describe_error, message_parts,
    server_prefill,
};

fn main() {
    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            dioxus::desktop::Config::new().with_window(
                dioxus::desktop::WindowBuilder::new()
                    .with_title("Foundry Circle asset uploader")
                    .with_inner_size(dioxus::desktop::LogicalSize::new(960.0, 800.0))
                    .with_min_inner_size(dioxus::desktop::LogicalSize::new(720.0, 560.0)),
            ),
        )
        .launch(App);
}

/// The window's state, as one copyable bundle of signals.
///
/// Plain Dioxus signals: this only saves threading nine of them through every
/// helper, it is not a store.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Uploader {
    session: Signal<Option<Session>>,
    /// Bumped whenever the session changes, so in-flight work can tell that it
    /// no longer speaks for the signed-in account.
    generation: Signal<SessionGeneration>,
    account: Signal<Option<String>>,
    message: Signal<Option<Feedback>>,
    options: Signal<UiOptions>,
    jobs: Signal<Vec<Job>>,
    running: Signal<bool>,
    busy: Signal<bool>,
    server: Signal<String>,
    staging: Signal<PathBuf>,
}

impl Uploader {
    fn session(&self) -> Option<Session> {
        (self.session)()
    }

    fn generation(&self) -> SessionGeneration {
        (self.generation)()
    }

    fn account(&self) -> Option<String> {
        (self.account)()
    }

    fn message(&self) -> Option<Feedback> {
        (self.message)()
    }

    fn options(&self) -> UiOptions {
        (self.options)()
    }

    fn jobs(&self) -> Vec<Job> {
        (self.jobs)()
    }

    fn running(&self) -> bool {
        (self.running)()
    }

    fn busy(&self) -> bool {
        (self.busy)()
    }

    fn server(&self) -> String {
        (self.server)()
    }

    fn staging(&self) -> PathBuf {
        (self.staging)()
    }

    /// One file at a time: conversion calls ffmpeg, uploads are async.
    ///
    /// The run belongs to the session it started under. Signing in, out, or
    /// letting the session expire bumps the generation, so this loop stops
    /// handing out work and a job whose conversion finishes afterwards is
    /// never uploaded with the replacement session.
    async fn drain(mut self) {
        let generation = self.generation();
        while let Some(index) = self.next_queued() {
            if !generation.is_current(self.generation()) {
                break;
            }
            // Each job carries the options and kind it was queued with.
            let job = self.jobs.read()[index].clone();
            let (path, options, kind) = (job.path, job.options, job.kind);
            let staging = self.staging();

            self.update(index, JobStatus::Converting, None);
            // `convert` spawns ffmpeg and writes the file: never on the UI thread.
            let converted =
                tokio::task::spawn_blocking(move || convert(&path, kind, &options, &staging)).await;
            let converted = match converted {
                Ok(Ok(converted)) => converted,
                Ok(Err(error)) => {
                    self.update(index, JobStatus::Failed, Some(error.to_string()));
                    continue;
                }
                Err(error) => {
                    self.update(
                        index,
                        JobStatus::Failed,
                        Some(format!("the conversion task stopped: {error}")),
                    );
                    continue;
                }
            };

            let Some((asset_kind, _relative)) = staged_asset(&self.staging(), &converted) else {
                self.update(
                    index,
                    JobStatus::Failed,
                    Some(format!(
                        "converted to {}, which is not a Foundry asset directory",
                        converted.display()
                    )),
                );
                let _ = std::fs::remove_file(&converted);
                continue;
            };

            // The session may have changed during the conversion: skipping the
            // upload is the whole point of binding the run to its generation.
            let session = if generation.is_current(self.generation()) {
                self.session()
            } else {
                None
            };
            let Some(session) = session else {
                self.update(
                    index,
                    JobStatus::Failed,
                    Some(
                        "the session changed before the upload started; add the file again"
                            .to_string(),
                    ),
                );
                let _ = std::fs::remove_file(&converted);
                break;
            };

            self.update(index, JobStatus::Uploading, None);
            match upload(&session, asset_kind, &converted).await {
                Ok(foundry_path) => self.update(index, JobStatus::Done, Some(foundry_path)),
                Err(error) => {
                    // The broker never overwrites, so a taken name is its own
                    // state the table reports; everything else is a failure.
                    let (text, expired) = describe_error(&error);
                    if matches!(error, Error::Exists) {
                        self.update(index, JobStatus::Exists, None);
                    } else {
                        self.update(index, JobStatus::Failed, Some(text));
                    }
                    if expired {
                        // Remaining files stay queued so a new sign-in resumes them.
                        // Only this run's session: a replacement signed in
                        // meanwhile is not the one the server refused.
                        self.expire(
                            generation,
                            Some("Signed out: the server session expired.".to_string()),
                        );
                        let _ = std::fs::remove_file(&converted);
                        break;
                    }
                }
            }
            let _ = std::fs::remove_file(&converted);
            // ponytail: an upload already on the wire is not aborted — it began
            // under its own session and only its result is recorded; the run
            // stops here instead of starting the next job under a new account.
            if !generation.is_current(self.generation()) {
                break;
            }
        }
        self.running.set(false);
        // A sign-in during this run found a live worker and left the queue to
        // it; pick the queue up now under the session it signed in with.
        if self.next_queued().is_some() {
            self.start_worker();
        }
    }

    fn next_queued(&self) -> Option<usize> {
        self.jobs
            .read()
            .iter()
            .position(|job| job.status == JobStatus::Queued)
    }

    fn update(mut self, index: usize, status: JobStatus, detail: Option<String>) {
        if let Some(job) = self.jobs.write().get_mut(index) {
            job.status = status;
            job.detail = detail;
        }
    }

    fn note(mut self, feedback: Feedback) {
        self.message.set(Some(feedback));
    }

    /// Queue dropped or picked files. Nothing runs while signed out: the files
    /// wait, and the next sign-in starts the worker.
    ///
    /// The conversion options and kind are snapshotted per file here, which is
    /// what "applied to files added from now on" means. A path that no longer
    /// exists is left to conversion, which reports the real error.
    fn enqueue(mut self, paths: Vec<PathBuf>) {
        let options = self.options();
        let jobs: Vec<Job> = paths
            .into_iter()
            .map(|path| Job::new(path, options))
            .collect();
        if jobs.is_empty() {
            return;
        }
        self.jobs.write().extend(jobs);
        if self.session.read().is_none() {
            self.note(feedback_info("Sign in to upload; the files are queued."));
            return;
        }
        self.start_worker();
    }

    fn start_worker(mut self) {
        if self.session.read().is_none() || self.running() {
            return;
        }
        // Set before spawning so a second call cannot start a second worker.
        self.running.set(true);
        spawn(async move { self.drain().await });
    }

    fn sign_in(mut self, server: String) {
        if self.busy() {
            return;
        }
        self.busy.set(true);
        self.note(feedback_info("Waiting for the browser sign-in…"));
        spawn(async move {
            let result = login(&server).await;
            match result {
                Ok(session) => {
                    // `save_session` writes and `sync_all`s: keep it off the UI
                    // thread, and behind any deletion queued before it.
                    let to_save = session.clone();
                    let saved = {
                        let _file = SESSION_FILE.lock().await;
                        tokio::task::spawn_blocking(move || save_session(&to_save))
                            .await
                            .unwrap_or_else(|error| Err(Error::Io(std::io::Error::other(error))))
                    };
                    // Install and bump together, with no await between: work
                    // captured under any earlier session is stale from here on.
                    self.generation.set(self.generation().bumped());
                    self.server.set(session.server.clone());
                    self.session.set(Some(session));
                    self.account.set(None);
                    self.note(match saved {
                        Ok(()) => feedback_success("Signed in. Pick files to convert and upload."),
                        Err(error) => feedback_error(format!(
                            "Signed in, but the session could not be saved: {error}"
                        )),
                    });
                    self.start_worker();
                }
                Err(error) => {
                    let (text, _) = describe_error(&error);
                    self.note(feedback_error(text));
                }
            }
            // Busy until the session is installed, so a second sign-in cannot
            // overlap this one's persistence.
            self.busy.set(false);
        });
    }

    /// Drop the session locally; the server's copy stays valid until it expires.
    fn sign_out(self) {
        self.expire(self.generation(), None);
    }

    /// Forget the session captured as `captured`, in memory and on disk.
    ///
    /// A stale capture is ignored: an expiry reported for an earlier session
    /// must not sign out its replacement or delete its saved copy.
    fn expire(mut self, captured: SessionGeneration, message: Option<String>) {
        if !captured.is_current(self.generation()) {
            return;
        }
        let current = self.generation().bumped();
        self.generation.set(current);
        self.session.set(None);
        self.account.set(None);
        spawn(async move {
            // Queued on the same lock as saves, so a later sign-in's save runs
            // after this deletion instead of being undone by it.
            let failure = {
                let _file = SESSION_FILE.lock().await;
                match tokio::task::spawn_blocking(forget_session).await {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(format!("the saved session was not removed: {error}")),
                    Err(error) => Some(format!(
                        "the saved session was not removed, the disk task stopped: {error}"
                    )),
                }
            };
            // A sign-in since then owns the message line.
            if !current.is_current(self.generation()) {
                return;
            }
            self.note(match (message, failure) {
                (Some(message), Some(failure)) => {
                    feedback_error(format!("{message} Also, {failure}"))
                }
                (Some(message), None) => feedback_error(message),
                (None, Some(failure)) => {
                    feedback_error(format!("Signed out locally, but {failure}"))
                }
                (None, None) => {
                    feedback_info("Signed out. The server session stays valid until it expires.")
                }
            });
        });
    }
}

/// Serializes the saved-session file: saves and deletions run in the order
/// they were requested (tokio's mutex is FIFO).
static SESSION_FILE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[component]
fn App() -> Element {
    // `use_hook` needs a `Clone` value and the library error is not `Clone`,
    // so only the parts the window renders are kept.
    #[derive(Clone)]
    struct Startup {
        session: Option<Session>,
        server: String,
        message: Option<Feedback>,
    }
    let startup = use_hook(|| match load_session() {
        Ok(session) => Startup {
            server: server_prefill(session.as_ref()),
            session,
            message: None,
        },
        Err(error) => Startup {
            session: None,
            server: String::new(),
            message: Some(feedback_error(error.to_string())),
        },
    });

    let mut uploader = Uploader {
        session: use_signal(|| startup.session.clone()),
        generation: use_signal(SessionGeneration::default),
        account: use_signal(|| None::<String>),
        message: use_signal(|| startup.message.clone()),
        options: use_signal(UiOptions::default),
        jobs: use_signal(Vec::<Job>::new),
        running: use_signal(|| false),
        busy: use_signal(|| false),
        server: use_signal(|| startup.server.clone()),
        // ponytail: one staging directory per process under the system temp
        // dir; each converted copy is deleted after its upload and the empty
        // directories are left behind. Sweep them when an install cares.
        staging: use_signal(|| {
            std::env::temp_dir().join(format!("foundry-circle-desktop-{}", std::process::id()))
        }),
    };

    // Keep the account line tied to the session; an expired session signs out.
    use_effect(move || match uploader.session() {
        None => uploader.account.set(None),
        Some(session) => {
            // `peek` keeps the effect keyed to the session itself, not to the
            // counter this spawn is about to read.
            let generation = *uploader.generation.peek();
            spawn(async move {
                let result = whoami(&session).await;
                // A late answer for a session that is gone must not name the
                // old account over the new one, nor expire its replacement.
                if !generation.is_current(uploader.generation()) {
                    return;
                }
                match result {
                    Ok(principal) => uploader.account.set(Some(account_label(&principal))),
                    Err(error) => {
                        let (text, expired) = describe_error(&error);
                        uploader.account.set(Some(text.clone()));
                        if expired {
                            uploader.expire(
                                generation,
                                Some("Signed out: the server session expired.".into()),
                            );
                        }
                    }
                }
            });
        }
    });

    let signed_in = uploader.session.read().is_some();
    let feedback = uploader.message().clone();
    rsx! {
        style { dangerous_inner_html: SHARED_STYLES }
        style { {STYLES} }
        div { class: "uploader",
            main { class: "uploader-main",
                DashboardHeader {
                    eyebrow: "Foundry Circle".to_string(),
                    heading: "Asset uploader".to_string(),
                    description: "Convert assets with ffmpeg and upload them to Foundry's Data folders.".to_string(),
                }
                if let Some(feedback) = feedback {
                    div { class: "uploader-feedback", role: "status",
                        FeedbackBanner { feedback }
                    }
                }
                {account_panel(uploader, signed_in)}
                {options_panel(uploader)}
                {files_panel(uploader, signed_in)}
            }
        }
    }
}

fn account_panel(mut uploader: Uploader, signed_in: bool) -> Element {
    if signed_in {
        let account = uploader.account().unwrap_or_else(|| "loading…".to_string());
        let server = uploader
            .session()
            .map(|session| session.server)
            .unwrap_or_default();
        rsx! {
            Panel { title: "Account".to_string(),
                dl { class: "uploader-account",
                    dt { "Signed in as" }
                    dd { {account} }
                    dt { "Server" }
                    dd { class: "uploader-path", {server} }
                }
                button {
                    class: "tartan-button",
                    r#type: "button",
                    onclick: move |_| uploader.sign_out(),
                    "Sign out"
                }
            }
        }
    } else {
        let busy = uploader.busy();
        rsx! {
            Panel {
                title: "Sign in".to_string(),
                description: "Your browser opens; this window waits for the redirect back.".to_string(),
                form {
                    class: "uploader-form",
                    onsubmit: move |event| {
                        event.prevent_default();
                        uploader.sign_in(uploader.server());
                    },
                    Field {
                        id: "server".to_string(),
                        label: "Broker URL".to_string(),
                        hint: "https, or http on localhost. Ask your GM for the address.".to_string(),
                        input {
                            id: "server",
                            name: "server",
                            r#type: "url",
                            required: true,
                            placeholder: "https://foundry.example.com",
                            value: "{uploader.server()}",
                            oninput: move |event| uploader.server.set(event.value()),
                        }
                    }
                    button {
                        class: "tartan-button",
                        r#type: "submit",
                        disabled: busy,
                        if busy { "Waiting for the browser…" } else { "Sign in" }
                    }
                }
            }
        }
    }
}

fn options_panel(mut uploader: Uploader) -> Element {
    let options = uploader.options();
    rsx! {
        Panel {
            title: "Conversion options".to_string(),
            description: "Applied to files added from now on; the defaults follow the Foundry media guide.".to_string(),
            div { class: "uploader-form uploader-form--grid",
                Field {
                    id: "kind".to_string(),
                    label: "Asset kind".to_string(),
                    select {
                        id: "kind",
                        name: "kind",
                        value: "{options.kind.value()}",
                        onchange: move |event| uploader.options.write().kind = KindChoice::from_value(&event.value()),
                        for choice in KindChoice::ALL {
                            option { key: "{choice.value()}", value: "{choice.value()}", {choice.label()} }
                        }
                    }
                }
                Field {
                    id: "token-px".to_string(),
                    label: "Token side (px)".to_string(),
                    hint: "{TOKEN_PX_MIN}-{TOKEN_PX_MAX} px, square.".to_string(),
                    input {
                        id: "token-px",
                        name: "token-px",
                        r#type: "number",
                        min: "{TOKEN_PX_MIN}",
                        max: "{TOKEN_PX_MAX}",
                        value: "{options.token_px}",
                        oninput: move |event| {
                            if let Ok(value) = event.value().parse::<u32>() {
                                uploader.options.write().token_px = value;
                            }
                        },
                        onchange: move |event| {
                            if let Ok(value) = event.value().parse::<u32>() {
                                uploader.options.write().token_px = clamp_token_px(value);
                            }
                        },
                    }
                }
                Field {
                    id: "grid-px".to_string(),
                    label: "Map grid cell (px)".to_string(),
                    hint: "One Foundry grid square; {GRID_PX_DEFAULT} px matches the default scenes.".to_string(),
                    input {
                        id: "grid-px",
                        name: "grid-px",
                        r#type: "number",
                        min: "1",
                        max: "1000",
                        value: "{options.grid_px}",
                        oninput: move |event| {
                            if let Ok(value) = event.value().parse::<u32>() {
                                uploader.options.write().grid_px = value;
                            }
                        },
                        onchange: move |event| {
                            if let Ok(value) = event.value().parse::<u32>() {
                                uploader.options.write().grid_px =
                                    if value == 0 { GRID_PX_DEFAULT } else { value };
                            }
                        },
                    }
                }
                Field {
                    id: "audio-codec".to_string(),
                    label: "Audio codec".to_string(),
                    select {
                        id: "audio-codec",
                        name: "audio-codec",
                        value: "{codec_value(options.audio)}",
                        onchange: move |event| uploader.options.write().audio = codec_from_value(&event.value()),
                        for codec in [AudioCodec::Vorbis, AudioCodec::Opus] {
                            option { key: "{codec_value(codec)}", value: "{codec_value(codec)}", {codec_label(codec)} }
                        }
                    }
                }
                Field {
                    id: "audio-kbps".to_string(),
                    label: "Audio bitrate (kbps)".to_string(),
                    hint: "{AUDIO_KBPS_MIN}-{AUDIO_KBPS_MAX} kbps.".to_string(),
                    input {
                        id: "audio-kbps",
                        name: "audio-kbps",
                        r#type: "number",
                        min: "{AUDIO_KBPS_MIN}",
                        max: "{AUDIO_KBPS_MAX}",
                        value: "{options.audio_kbps}",
                        oninput: move |event| {
                            if let Ok(value) = event.value().parse::<u32>() {
                                uploader.options.write().audio_kbps = value;
                            }
                        },
                        onchange: move |event| {
                            if let Ok(value) = event.value().parse::<u32>() {
                                uploader.options.write().audio_kbps = clamp_audio_kbps(value);
                            }
                        },
                    }
                }
            }
        }
    }
}

fn files_panel(uploader: Uploader, signed_in: bool) -> Element {
    let jobs = uploader.jobs();
    rsx! {
        Panel {
            title: "Files".to_string(),
            description: "One file at a time; each upload runs conversion first.".to_string(),
            div {
                class: "uploader-drop",
                ondragover: move |event| event.prevent_default(),
                ondrop: move |event| {
                    event.prevent_default();
                    let paths: Vec<PathBuf> = event.files().iter().map(|file| file.path()).collect();
                    uploader.enqueue(paths);
                },
                Field {
                    id: "files".to_string(),
                    label: "Add files".to_string(),
                    hint: "Drop files in this panel or use the picker.".to_string(),
                    input {
                        id: "files",
                        name: "files",
                        r#type: "file",
                        multiple: true,
                        disabled: !signed_in,
                        onchange: move |event| {
                            let paths: Vec<PathBuf> = event.files().iter().map(|file| file.path()).collect();
                            uploader.enqueue(paths);
                        },
                    }
                }
            }

            if jobs.is_empty() {
                p { class: "tartan-muted", "Nothing queued yet." }
            } else {
                table { class: "uploader-queue",
                    thead {
                        tr {
                            th { scope: "col", "File" }
                            th { scope: "col", "Status" }
                            th { scope: "col", "Foundry path" }
                        }
                    }
                    tbody {
                        for (index, job) in jobs.iter().enumerate() {
                            tr { key: "{index}",
                                td { class: "uploader-path", {job.name.clone()} }
                                td {
                                    {job.status_label()}
                                    if job.status == JobStatus::Failed {
                                        if let Some(reason) = job.detail.clone() {
                                            div { class: "uploader-reason", {reason_text(&reason)} }
                                        }
                                    }
                                }
                                td { class: "uploader-path", {foundry_cell(job, uploader)} }
                            }
                        }
                    }
                }
            }

            p { class: "tartan-muted uploader-help",
                "Still manual: token art faces south, and maps have no baked-in grid."
            }
        }
    }
}

/// A failure reason, with any URL it carries as a link to the system browser.
///
/// media-oxide's missing-ffmpeg message ends in its download page; inside the
/// webview that must open in the real browser, not replace the window.
fn reason_text(reason: &str) -> Element {
    rsx! {
        for (index, part) in message_parts(reason).into_iter().enumerate() {
            match part {
                MessagePart::Text(text) => rsx! { span { key: "{index}", {text} } },
                MessagePart::Url(url) => {
                    let label = url.clone();
                    let target = url.clone();
                    rsx! {
                        a {
                            key: "{index}",
                            class: "uploader-link",
                            href: "{url}",
                            onclick: move |event| {
                                event.prevent_default();
                                let _ = open_browser(&target);
                            },
                            {label}
                        }
                    }
                }
            }
        }
    }
}

/// The Foundry path of a finished upload with its Copy button.
fn foundry_cell(job: &Job, uploader: Uploader) -> Element {
    match job.foundry_path() {
        Some(path) => {
            let path = path.to_string();
            let shown = path.clone();
            let label = format!("Copy {path}");
            rsx! {
                button {
                    class: "tartan-button",
                    r#type: "button",
                    aria_label: "{label}",
                    onclick: move |_| copy_path(path.clone(), uploader),
                    "Copy"
                }
                span { class: "uploader-foundry-path", {shown} }
            }
        }
        None if job.status == JobStatus::Exists => {
            rsx! { "Already on the server; nothing was replaced." }
        }
        None => rsx! {},
    }
}

/// Put a Foundry path on the clipboard.
///
/// `navigator.clipboard` needs a secure context the webview's `dioxus://` page
/// is not guaranteed to have, so a hidden textarea carries the fallback and the
/// result is reported either way.
fn copy_path(path: String, uploader: Uploader) {
    spawn(async move {
        let quoted = serde_json::to_string(&path).unwrap_or_else(|_| "\"\"".to_string());
        let script = format!(
            "const text = {quoted};
            const area = document.createElement('textarea');
            area.value = text;
            area.setAttribute('readonly', '');
            area.style.position = 'fixed';
            area.style.opacity = '0';
            document.body.appendChild(area);
            area.select();
            let copied = false;
            try {{ copied = document.execCommand('copy'); }} catch (error) {{ copied = false; }}
            area.remove();
            return copied;"
        );
        let copied = matches!(
            document::eval(&script).await,
            Ok(serde_json::Value::Bool(true))
        );
        uploader.note(if copied {
            feedback_success(format!("Copied {path}"))
        } else {
            feedback_info(format!("Select the path and press Ctrl+C: {path}"))
        });
    });
}

fn feedback_info(message: impl Into<String>) -> Feedback {
    Feedback {
        kind: FeedbackKind::Info,
        message: message.into(),
    }
}

fn feedback_success(message: impl Into<String>) -> Feedback {
    Feedback {
        kind: FeedbackKind::Success,
        message: message.into(),
    }
}

fn feedback_error(message: impl Into<String>) -> Feedback {
    Feedback {
        kind: FeedbackKind::Error,
        message: message.into(),
    }
}

/// Window-local layout only; colours come from the shared stylesheet.
const STYLES: &str = r#"
:where(html, body) { margin: 0; padding: 0; }
.uploader { min-height: 100vh; background: var(--tartan-paper); color: var(--tartan-ink);
  font-family: ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif; font-size: 14px; line-height: 1.5; }
.uploader-main { display: grid; gap: 1.25rem; width: min(100% - 2.5rem, 62rem); margin: 0 auto; padding: 1.5rem 0 3rem; }
.uploader-main > .tartan-panel { display: grid; gap: .9rem; }
.uploader-feedback:empty { display: none; }
.uploader-form { display: grid; gap: .9rem; }
.uploader-form--grid { grid-template-columns: repeat(auto-fit, minmax(13rem, 1fr)); align-items: end; }
.uploader-form input, .uploader-form select { width: 100%; padding: .45rem .55rem; border: 1px solid var(--tartan-line);
  border-radius: .4rem; background: #fff; color: inherit; font: inherit; }
.uploader-form button[type="submit"] { justify-self: start; }
.uploader-form button:focus-visible, .uploader-form input:focus-visible, .uploader-form select:focus-visible,
.uploader button:focus-visible { outline: 2px solid var(--tartan-accent); outline-offset: 2px; }
.uploader-account { display: grid; grid-template-columns: minmax(7rem, max-content) minmax(0, 1fr); gap: .35rem 1rem; margin: 0; }
.uploader-account dt { font-weight: 600; }
.uploader-account dd { margin: 0; }
.uploader-drop { display: grid; gap: .6rem; border: 1px dashed var(--tartan-line); border-radius: .55rem; padding: 1rem; }
.uploader-queue { width: 100%; border-collapse: collapse; }
.uploader-queue th, .uploader-queue td { border-bottom: 1px solid var(--tartan-line); padding: .45rem .5rem; text-align: left; vertical-align: top; }
.uploader-queue th { font-size: .75rem; letter-spacing: .06em; text-transform: uppercase; color: var(--tartan-muted); }
.uploader-queue td .tartan-button { margin-right: .5rem; }
.uploader-path, .uploader-foundry-path { overflow-wrap: anywhere; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.uploader-reason { color: var(--tartan-danger); font-size: .85rem; }
.uploader-link { color: var(--tartan-accent); }
.uploader-help { margin: 0; }
@media (prefers-reduced-motion: reduce) { .uploader :where(*) { transition-duration: .01ms; animation-duration: .01ms; } }
"#;
