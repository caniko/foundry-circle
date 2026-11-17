//! C4/C5 acceptance (README "Approval"): a disposable dnd5e 6.1.0 world on a
//! second, socket-only Foundry, adopted, synced and approved by the broker's own
//! code against a throwaway PostgreSQL. Atlas only; see README for the command.

use serde_json::{Value, json};

fn validate_preflight(
    system: &Value,
    licence: &Value,
    hostname: &str,
    status: &Value,
) -> Result<(), &'static str> {
    if system["version"] != "6.1.0" {
        return Err("dnd5e version is not 6.1.0");
    }
    if licence["host"] != hostname || !present(&licence["signature"]) {
        return Err("licence host or signature is invalid");
    }
    if status["active"] != false {
        return Err("production Foundry is not explicitly idle");
    }
    Ok(())
}

#[test]
fn preflight_fails_closed() {
    let system = json!({"version":"6.1.0"});
    let licence = json!({"host":"atlas", "signature":"signature"});
    assert!(validate_preflight(&system, &licence, "atlas", &json!({"active":false})).is_ok());
    assert!(validate_preflight(&system, &licence, "other", &json!({"active":false})).is_err());
    assert!(validate_preflight(&system, &licence, "atlas", &json!({})).is_err());
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
struct Diff {
    check: &'static str,
    path: String,
    reference: Value,
    created: Value,
}

fn compare(reference: &Value, created: &Value) -> Vec<Diff> {
    let mut diffs = Vec::new();
    for field in ["abilities", "skills", "tools", "traits", "hp", "currency"] {
        if reference.get(field) != created.get(field) {
            diffs.push(Diff {
                check: "C4",
                path: field.to_owned(),
                reference: reference[field].clone(),
                created: created[field].clone(),
            });
        }
    }
    let items = |actor: &Value| actor["items"].as_array().cloned().unwrap_or_default();
    let reference_items = items(reference);
    let created_items = items(created);
    // Name and quantity: ten sheets of parchment are not one.
    let names = |items: &[Value]| {
        let mut names: Vec<Value> = items
            .iter()
            .map(|item| json!([item["name"], item["quantity"]]))
            .collect();
        names.sort_by_key(Value::to_string);
        names
    };
    let expected = names(&reference_items);
    let actual = names(&created_items);
    if expected != actual {
        diffs.push(Diff {
            check: "C4",
            path: "items".into(),
            reference: json!(expected),
            created: json!(actual),
        });
    }
    for item in &created_items {
        if reference_items.iter().any(|reference| {
            reference["name"] == item["name"] && present(&reference["advancementOrigin"])
        }) {
            for field in ["sourceId", "advancementOrigin"] {
                if !present(&item[field]) {
                    diffs.push(Diff {
                        check: "C4",
                        path: format!("items/{}/{}", item["name"], field),
                        reference: json!("non-empty"),
                        created: item[field].clone(),
                    });
                }
            }
        }
    }
    diffs
}

fn present(value: &Value) -> bool {
    value.as_str().is_some_and(|value| !value.is_empty())
}

#[test]
fn comparison_checks_values_items_and_provenance() {
    let reference = json!({"skills":{"med":1}, "items":[
        {"name":"Feature", "sourceId":"source", "advancementOrigin":"origin"}
    ]});
    assert!(compare(&reference, &reference).is_empty());
    let mut changed = reference.clone();
    changed["skills"]["med"] = json!(0);
    assert_eq!(compare(&reference, &changed)[0].path, "skills");
    changed = reference.clone();
    changed["items"] = json!([]);
    assert_eq!(compare(&reference, &changed)[0].path, "items");
    changed = reference.clone();
    changed["items"][0]["advancementOrigin"] = Value::Null;
    assert_eq!(compare(&reference, &changed).len(), 1);
    changed = reference.clone();
    changed["items"][0]["quantity"] = json!(10);
    assert_eq!(compare(&reference, &changed)[0].path, "items");
    changed = reference.clone();
    changed["currency"] = json!({"gp": 15});
    assert_eq!(compare(&reference, &changed)[0].path, "currency");
}

mod live {
    use super::{compare, validate_preflight};
    use crate::Role;
    use crate::builder::{
        self, AbilityMethod, CampaignRules, Catalog, DecisionKind, DraftChoices, Scores, Slot,
    };
    use crate::driver::DriverPage;
    use crate::driver::{DriverConfig, LiveFoundryDriver, WorldState};
    use crate::http::auth::Principal;
    use crate::http::builder_store;
    use crate::http::{AppState, ConsoleLinks, DatabaseState, session};
    use chromiumoxide::page::Page;
    use serde::Deserialize;
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
        process::Stdio,
        time::{Duration, Instant},
    };
    use tokio::{
        net::{TcpListener, UnixStream},
        process::{Child, Command},
        task::JoinHandle,
        time::sleep,
    };
    use uuid::Uuid;

    const GM: &str = "acceptance-gm";
    const PLAYER: &str = "acceptance-player";
    const PRODUCTION_STATUS: &str = "http://127.0.0.1:8030/api/status";

    fn env(name: &str) -> String {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| panic!("{name} is not set"))
    }

    fn read_json(path: &Path) -> Value {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    /// Everything the run owns; dropping it kills both servers and deletes the
    /// work and socket directories, licence copy included.
    struct World {
        work: PathBuf,
        sockets: PathBuf,
        _foundry: Child,
        _postgres: Child,
        forwarder: JoinHandle<()>,
        base_url: String,
        admin_password: String,
        evidence: PathBuf,
    }

    impl Drop for World {
        fn drop(&mut self) {
            self.forwarder.abort();
            // Keep the servers' logs even when the run panicked.
            for log in ["foundry.log", "postgres.err.log"] {
                let _ = std::fs::copy(self.work.join(log), self.evidence.join(log));
            }
            let _ = std::fs::remove_dir_all(&self.work);
            let _ = std::fs::remove_dir_all(&self.sockets);
        }
    }

    async fn wait_for(what: &str, limit: Duration, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + limit;
        while !ready() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            sleep(Duration::from_millis(250)).await;
        }
    }

    async fn run(program: &str, args: &[&str]) {
        let status = Command::new(program)
            .args(args)
            .env_remove("PGDATA")
            .env_remove("PGHOST")
            .env_remove("DATABASE_URL")
            .stdout(Stdio::null())
            .status()
            .await
            .unwrap_or_else(|error| panic!("{program}: {error}"));
        assert!(status.success(), "{program} {args:?} failed: {status}");
    }

    async fn boot(foundry: &str, dnd5e: &str, licence: &Path, evidence: PathBuf) -> World {
        let id = Uuid::new_v4().simple().to_string();
        let work = std::env::temp_dir().join(format!("fc-c4c5-{id}"));
        // A unix socket path is capped at 108 bytes; the runtime dir is short.
        let sockets =
            PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into()))
                .join(format!("fc-{}", &id[..8]));
        // Both roots are private before anything is written into them: the
        // licence copy, the admin password and the derived-password secret live
        // here, whatever the umask or the parent's mode.
        for root in [&work, &sockets] {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(root)
                .unwrap_or_else(|error| panic!("{}: {error}", root.display()));
        }
        for dir in ["Config", "Data/systems", "Data/modules", "Data/worlds"] {
            std::fs::create_dir_all(work.join(dir)).unwrap();
        }
        std::fs::write(
            work.join("Config/license.json"),
            std::fs::read(licence).unwrap(),
        )
        .unwrap();
        // Store copies are read-only and the packs are LevelDB directories
        // Foundry locks and writes: copy them writable.
        let module = concat!(env!("CARGO_MANIFEST_DIR"), "/../../foundry-module");
        for (source, target) in [
            (dnd5e.to_owned(), work.join("Data/systems/dnd5e")),
            (module.to_owned(), work.join("Data/modules/foundry-circle")),
        ] {
            run(
                "cp",
                &[
                    "-rL",
                    "--no-preserve=mode",
                    &source,
                    target.to_str().unwrap(),
                ],
            )
            .await;
        }
        let foundry_socket = sockets.join("foundry.sock");
        // No TCP port at all: the default GM has a blank password until adoption.
        std::fs::write(
            work.join("Config/options.json"),
            serde_json::to_vec_pretty(&json!({
                "unixSocket": foundry_socket,
                "upnp": false,
                "hostname": null,
                "proxySSL": false,
                "proxyPort": null,
                "routePrefix": null,
                "language": "en.core",
                "compressStatic": true,
                "compressSocket": true,
                "cssTheme": "dark",
            }))
            .unwrap(),
        )
        .unwrap();

        let pg = work.join("pg");
        run(
            "initdb",
            &[
                "-D",
                pg.to_str().unwrap(),
                "-U",
                "postgres",
                "--auth=trust",
                "--no-sync",
            ],
        )
        .await;
        let postgres = Command::new("postgres")
            .args(["-D", pg.to_str().unwrap(), "-k", sockets.to_str().unwrap()])
            .args(["-c", "listen_addresses=", "-c", "fsync=off"])
            .env_remove("PGDATA")
            .env_remove("PGHOST")
            .stdout(std::fs::File::create(work.join("postgres.log")).unwrap())
            .stderr(std::fs::File::create(work.join("postgres.err.log")).unwrap())
            .kill_on_drop(true)
            .spawn()
            .expect("postgres");
        let pg_socket = sockets.join(".s.PGSQL.5432");
        wait_for("PostgreSQL", Duration::from_secs(30), || pg_socket.exists()).await;

        let admin_password = crate::adoption::random_password();
        let log = std::fs::File::create(work.join("foundry.log")).unwrap();
        let foundry = Command::new(foundry)
            .args(["--headless", "--noupdate", "--noipdiscovery"])
            .arg(format!("--dataPath={}", work.display()))
            .arg(format!("--adminPassword={admin_password}"))
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .expect("foundryvtt");
        let log_path = work.join("foundry.log");
        let listening = || {
            std::fs::read_to_string(&log_path)
                .unwrap_or_default()
                .contains("Server started and listening")
        };
        wait_for("Foundry", Duration::from_secs(60), listening).await;
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            log.contains("listening on socket") && !log.contains("listening on port"),
            "Foundry did not bind only its unix socket:\n{log}"
        );

        // The only TCP listener is ours, on loopback.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let forwarder = tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                let socket = foundry_socket.clone();
                tokio::spawn(async move {
                    if let Ok(mut unix) = UnixStream::connect(socket).await {
                        let _ = tokio::io::copy_bidirectional(&mut tcp, &mut unix).await;
                    }
                });
            }
        });
        World {
            work,
            sockets,
            _foundry: foundry,
            _postgres: postgres,
            forwarder,
            base_url,
            admin_password,
            evidence,
        }
    }

    async fn status(url: &str) -> Value {
        reqwest::get(url)
            .await
            .unwrap_or_else(|error| panic!("{url}: {error}"))
            .json()
            .await
            .unwrap()
    }

    async fn post(base: &str, path: &str, cookie: &str, body: Value) -> Value {
        reqwest::Client::new()
            .post(format!("{base}{path}"))
            .header("cookie", format!("session={cookie}"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or(Value::Null)
    }

    async fn insert_principal(pool: &PgPool, subject: &str, name: &str) {
        sqlx::query(
            "INSERT INTO principals (subject, issuer, display_name) VALUES ($1, 'acceptance.invalid', $2)",
        )
        .bind(subject)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    }

    /// The fixture character in a player's terms: names, trait keys and
    /// scores, never advancement ids, so the Rust draft and the reference
    /// browser build encode the same choices independently.
    const SHEET: &str = include_str!("../tests/fixtures/acceptance/cleric.json");

    #[derive(Deserialize)]
    struct Sheet {
        name: String,
        method: AbilityMethod,
        base: Scores,
        roots: Roots,
        /// Root name (class or background) → `kit`, `kit2`, … or `gold`.
        equipment: BTreeMap<String, String>,
        details: BTreeMap<String, String>,
        answers: Vec<Answer>,
    }

    #[derive(Deserialize)]
    struct Roots {
        species: String,
        background: String,
        class: String,
    }

    /// One answer, keyed by what a player sees: the document that opens the
    /// choice, its kind, its level and its title.
    #[derive(Deserialize, serde::Serialize)]
    struct Answer {
        owner: String,
        #[serde(rename = "type")]
        kind: String,
        level: u8,
        title: String,
        #[serde(default)]
        values: Vec<String>,
        /// Item names, for an item choice.
        #[serde(default)]
        items: Vec<String>,
        #[serde(default)]
        asi: BTreeMap<String, u8>,
    }

    fn kind_name(kind: &DecisionKind) -> &'static str {
        match kind {
            DecisionKind::Trait { .. } => "trait",
            DecisionKind::Items { .. } => "items",
            DecisionKind::Asi { .. } => "asi",
            DecisionKind::Size { .. } => "size",
            DecisionKind::Unsupported { .. } => "unsupported",
        }
    }

    /// Answer every decision the walker opens, re-walking after each round
    /// because a pick (a feat) opens nested decisions. A decision with no sheet
    /// entry, an entry matching two decisions, or an entry left unused fails.
    fn encode(
        sheet: &Sheet,
        catalog: &Catalog,
        level: u8,
        choices: &mut DraftChoices,
    ) -> Result<Vec<String>, String> {
        let mut used = vec![None::<String>; sheet.answers.len()];
        loop {
            let mut changed = false;
            for decision in builder::decisions(catalog, choices, level) {
                if decision.is_automatic()
                    || choices.picks.contains_key(&decision.key)
                    || choices.asi.contains_key(&decision.key)
                {
                    continue;
                }
                let decision_level: u8 = decision
                    .key
                    .rsplit('#')
                    .next()
                    .and_then(|level| level.parse().ok())
                    .unwrap_or(u8::MAX);
                let id = format!(
                    "{}/{}/{}/{}",
                    decision.owner_name,
                    kind_name(&decision.kind),
                    decision_level,
                    decision.title
                );
                let matches: Vec<usize> = (0..sheet.answers.len())
                    .filter(|&index| {
                        let answer = &sheet.answers[index];
                        answer.owner == decision.owner_name
                            && answer.kind == kind_name(&decision.kind)
                            && answer.level == decision_level
                            && answer.title == decision.title
                    })
                    .collect();
                let index = match matches.as_slice() {
                    [] => return Err(format!("unanswered: {id}")),
                    [index] => *index,
                    _ => return Err(format!("ambiguous_key: {id}")),
                };
                if let Some(previous) = used[index].replace(decision.key.clone()) {
                    return Err(format!("ambiguous_key: {id} also answers {previous}"));
                }
                let answer = &sheet.answers[index];
                match &decision.kind {
                    DecisionKind::Asi { .. } => {
                        choices.asi.insert(decision.key.clone(), answer.asi.clone());
                    }
                    DecisionKind::Items { options, .. } => {
                        let values = answer
                            .items
                            .iter()
                            .map(|name| {
                                options
                                    .iter()
                                    .find(|option| &option.label == name)
                                    .map(|option| option.value.clone())
                                    .ok_or_else(|| format!("not_offered: {id}: {name}"))
                            })
                            .collect::<Result<_, _>>()?;
                        choices.picks.insert(decision.key.clone(), values);
                    }
                    DecisionKind::Unsupported { kind } => {
                        return Err(format!("unsupported: {id}: {kind}"));
                    }
                    _ => {
                        choices
                            .picks
                            .insert(decision.key.clone(), answer.values.clone());
                    }
                }
                changed = true;
            }
            if !changed {
                break;
            }
        }
        if let Some(index) = used.iter().position(Option::is_none) {
            let answer = &sheet.answers[index];
            return Err(format!(
                "unused: {}/{}/{}/{}",
                answer.owner, answer.kind, answer.level, answer.title
            ));
        }
        for (root, option) in &sheet.equipment {
            let owner = if *root == sheet.roots.class {
                choices.class.clone()
            } else if *root == sheet.roots.background {
                choices.background.clone()
            } else {
                None
            }
            .ok_or_else(|| format!("equipment for unknown root {root}"))?;
            choices.equipment.insert(owner, option.clone());
        }
        // Every answer was used once: the decision key each one answers.
        Ok(used.into_iter().flatten().collect())
    }

    /// The reference build's browser side; see the file's own header.
    const REFERENCE: &str = include_str!("../tests/fixtures/acceptance/reference.js");

    /// One short call into `window.__c4c5`, so no single evaluation outlives
    /// chromiumoxide's request timeout.
    async fn call(page: &Page, method: &str, args: Value) -> Value {
        let script =
            format!("async function () {{ return await window.__c4c5.{method}({args}); }}");
        let reply = tokio::time::timeout(Duration::from_secs(60), page.evaluate(script.as_str()))
            .await
            .unwrap_or_else(|_| panic!("{method} timed out"))
            .unwrap_or_else(|error| panic!("{method}: {error}"));
        reply.into_value().unwrap_or(Value::Null)
    }

    /// A Foundry page logged in as one principal through the broker's own entry.
    async fn enter_page(
        pool: &PgPool,
        state: &AppState,
        config: &DriverConfig,
        session: Uuid,
        subject: &str,
        world: &str,
    ) -> DriverPage {
        // `DriverPage::open` lands on `/join`, and Foundry's `GET /join` logs the
        // session's user out of the world (`sessions.logoutWorld`): open on a
        // throwaway session, then enter and go straight to `/game`, as
        // adoption does with the API user.
        let page = DriverPage::open(config, "acceptance-unjoined")
            .await
            .unwrap();
        let (cookie, _) = crate::adoption::enter(
            pool,
            state.driver.as_ref(),
            &config.base_url,
            session,
            subject,
            subject,
            world,
        )
        .await
        .unwrap_or_else(|error| panic!("{subject} could not enter: {error:?}"));
        if let Err(error) = page.goto_game(config, &cookie).await {
            let seen = page
                .page
                .evaluate(
                    "() => ({url: location.href, ready: !!window.game?.ready, \
                     user: window.game?.user?.name ?? null, \
                     text: (document.body?.innerText ?? '').slice(0, 600)})",
                )
                .await
                .ok()
                .and_then(|reply| reply.into_value::<Value>().ok());
            let direct = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap()
                .get(format!("{}/game", config.base_url))
                .header("cookie", format!("session={cookie}"))
                .send()
                .await
                .map(|reply| (reply.status(), reply.headers().get("location").cloned()));
            panic!("{subject} did not reach the game ({error:?}): {seen:?}; GET /game: {direct:?}");
        }
        let install = format!("async function () {{ {REFERENCE}\n return true; }}");
        page.page.evaluate(install.as_str()).await.unwrap();
        page
    }

    /// The sheet answer for the flow on screen. Titles differ between the
    /// builder and dnd5e for an untitled advancement, so the match is the
    /// advancement id each answer resolved to through the builder's own
    /// decision key (`<item uuid>#<advancement id>#<level>`), plus the owner,
    /// kind and level the flow shows. Only the lookup is shared; the values are
    /// applied by dnd5e's own forms.
    fn answer_for<'a>(
        sheet: &'a Sheet,
        resolved: &[String],
        state: &Value,
    ) -> Result<Option<&'a Answer>, String> {
        let matches: Vec<&Answer> = sheet
            .answers
            .iter()
            .zip(resolved)
            .filter(|(answer, key)| {
                let mut parts = key.split('#');
                let (_, advancement) = (parts.next(), parts.next());
                state["owner"] == answer.owner.as_str()
                    && state["kind"] == answer.kind.as_str()
                    && state["level"] == answer.level
                    && advancement.is_some_and(|id| state["id"] == id)
            })
            .map(|(answer, _)| answer)
            .collect();
        match matches.as_slice() {
            [] => Ok(None),
            [answer] => Ok(Some(answer)),
            _ => Err("ambiguous_key".into()),
        }
    }

    fn uuid_of(options: &[builder::ClassOption], name: &str) -> String {
        let found: Vec<_> = options
            .iter()
            .filter(|option| option.name == name)
            .collect();
        assert_eq!(found.len(), 1, "expected one {name}, found {found:?}");
        found[0].uuid.clone()
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs licensed Foundry on atlas; see README"]
    async fn c4_c5() {
        let foundry = env("FOUNDRY_ACCEPTANCE_FOUNDRY");
        let dnd5e = env("FOUNDRY_ACCEPTANCE_DND5E");
        let licence = PathBuf::from(env("FOUNDRY_ACCEPTANCE_LICENSE"));
        let chromium = PathBuf::from(env("FOUNDRY_CHROMIUM"));
        let started = Instant::now();
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap();
        validate_preflight(
            &read_json(&Path::new(&dnd5e).join("system.json")),
            &read_json(&licence),
            hostname.trim(),
            &status(PRODUCTION_STATUS).await,
        )
        .unwrap();

        let evidence = PathBuf::from(env("FOUNDRY_ACCEPTANCE_REPORT_DIR"));
        std::fs::create_dir_all(&evidence).unwrap();
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(
                std::fs::File::create(evidence.join("broker.log")).unwrap(),
            ))
            .try_init();
        let world = boot(&foundry, &dnd5e, &licence, evidence.clone()).await;
        let mut phases = serde_json::Map::new();
        phases.insert("boot".into(), json!(started.elapsed().as_secs_f32()));

        let admin_file = world.work.join("admin-password");
        let secret_file = world.work.join("password-secret");
        std::fs::write(&admin_file, &world.admin_password).unwrap();
        std::fs::write(&secret_file, crate::adoption::random_password()).unwrap();
        // SAFETY: set before the broker's tasks start; this test runs alone
        // (`--exact`), and the broker reads these at use time.
        unsafe {
            std::env::set_var("FOUNDRY_ADMIN_PASSWORD_FILE", &admin_file);
            std::env::set_var("FOUNDRY_CIRCLE_PASSWORD_SECRET_FILE", &secret_file);
            // The lease must never restart the production Foundry unit.
            std::env::set_var("FOUNDRY_CIRCLE_SYSTEMCTL", "false");
            std::env::set_var("FOUNDRY_CIRCLE_RESTART_UNIT", "fc-c4c5-none.service");
        }
        let config = DriverConfig {
            base_url: world.base_url.clone(),
            api_user: "foundry-circle-api".into(),
            foundry_version: "14.367".into(),
            system_id: Some("dnd5e".into()),
            system_version: Some("6.1.0".into()),
            chromium,
            profile_dir: world.work.join("browser"),
        };
        let database = DatabaseState::new();
        database.supervise(
            Some(format!(
                "postgresql:///postgres?host={}&user=postgres",
                world.sockets.display()
            )),
            None,
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        let pool = loop {
            if let Some(pool) = database.pool().await {
                break pool;
            }
            assert!(
                Instant::now() < deadline,
                "the throwaway database did not migrate"
            );
            sleep(Duration::from_millis(250)).await;
        };
        let lease = session::Lease::from_env(&world.base_url);
        let driver = LiveFoundryDriver::start(config.clone());
        let state = AppState {
            driver: driver.clone(),
            database: database.clone(),
            auth: None,
            console: ConsoleLinks::default(),
            lease: lease.clone(),
            assets: None,
        };
        session::spawn_reconciler(database, lease.clone(), driver, config.clone());

        // Adoption, exactly as a GM's "Start session" then "Open Foundry Setup".
        insert_principal(&pool, GM, "Acceptance GM").await;
        insert_principal(&pool, PLAYER, "Acceptance Player").await;
        let open = session::start(&pool, &lease, GM).await.unwrap();
        let setup = lease.open_setup().await.unwrap();
        lease.remember_bootstrap(open.id, lease.open_setup().await.unwrap());
        let world_id = format!("c4c5-{}", &Uuid::new_v4().simple().to_string()[..8]);
        let created = post(
            &world.base_url,
            "/create",
            &setup,
            json!({"action": "createWorld", "id": world_id, "title": "C4/C5 acceptance", "system": "dnd5e"}),
        )
        .await;
        assert!(created.get("error").is_none(), "createWorld: {created}");
        let launched = post(
            &world.base_url,
            "/setup",
            &setup,
            json!({"action": "launchWorld", "world": world_id}),
        )
        .await;
        assert!(launched.get("error").is_none(), "launchWorld: {launched}");
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let snapshot = state.driver.snapshot();
            if snapshot.state == WorldState::Ready && snapshot.id.as_deref() == Some(&world_id) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the world was not adopted: {snapshot:?}"
            );
            sleep(Duration::from_secs(1)).await;
        }
        phases.insert("adopt".into(), json!(started.elapsed().as_secs_f32()));

        let packs = builder_store::sync_catalog_for(&state, &pool, GM, &world_id)
            .await
            .unwrap();
        phases.insert("sync".into(), json!(started.elapsed().as_secs_f32()));
        let rules: CampaignRules = serde_json::from_value(
            sqlx::query_scalar::<_, Value>("SELECT rules FROM campaign_rules WHERE world_id = $1")
                .bind(&world_id)
                .fetch_one(&pool)
                .await
                .unwrap(),
        )
        .unwrap();
        let catalog = builder_store::catalog(&pool, &world_id, &rules.sources)
            .await
            .unwrap();
        let sheet: Sheet = serde_json::from_str(SHEET).unwrap();
        let mut choices = DraftChoices {
            method: Some(sheet.method),
            base: Some(sheet.base),
            species: Some(uuid_of(
                &builder::origin_options(&catalog, Slot::Species),
                &sheet.roots.species,
            )),
            background: Some(uuid_of(
                &builder::origin_options(&catalog, Slot::Background),
                &sheet.roots.background,
            )),
            class: Some(uuid_of(&builder::classes(&catalog), &sheet.roots.class)),
            details: sheet.details.clone(),
            ..DraftChoices::default()
        };
        let encoded = encode(&sheet, &catalog, rules.starting_level, &mut choices);
        // Whatever the walker offers for the draft so far: the sheet is filled
        // from this file, never guessed.
        std::fs::write(
            evidence.join("decisions.json"),
            serde_json::to_vec_pretty(&json!({
                "choices": choices,
                "decisions": builder::decisions(&catalog, &choices, rules.starting_level),
            }))
            .unwrap(),
        )
        .unwrap();
        let resolved = encoded.unwrap();

        // Session zero open, then the player's submitted draft.
        sqlx::query("UPDATE campaign_rules SET open = true WHERE world_id = $1")
            .bind(&world_id)
            .execute(&pool)
            .await
            .unwrap();
        let draft = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO character_drafts (id, world_id, subject, name, status, choices)
             VALUES ($1, $2, $3, $4, 'submitted', $5)",
        )
        .bind(draft)
        .bind(&world_id)
        .bind(PLAYER)
        .bind(&sheet.name)
        .bind(serde_json::to_value(&choices).unwrap())
        .execute(&pool)
        .await
        .unwrap();
        let gm = Principal {
            subject: GM.into(),
            issuer: "acceptance.invalid".into(),
            display_name: Some("Acceptance GM".into()),
            email: None,
            groups: Vec::new(),
        };
        builder_store::approve(&state, &pool, draft, &gm, Role::Gm)
            .await
            .unwrap();
        let actor_id: String =
            sqlx::query_scalar("SELECT actor_id FROM character_drafts WHERE id = $1")
                .bind(draft)
                .fetch_one(&pool)
                .await
                .unwrap();
        let owner = crate::adoption::mapped_user(&pool, PLAYER, &world_id)
            .await
            .unwrap()
            .unwrap();
        std::fs::write(
            evidence.join("create-args.json"),
            serde_json::to_vec_pretty(
                &builder::create_args(
                    &catalog,
                    &rules,
                    &choices,
                    &sheet.name,
                    None,
                    &draft.to_string(),
                    &owner,
                )
                .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        phases.insert("approve".into(), json!(started.elapsed().as_secs_f32()));

        // C4: the same character by hand, in the GM's own page.
        let gm_page = enter_page(&pool, &state, &config, open.id, GM, &world_id).await;
        let page = &gm_page.page;
        let reference_id = call(
            page,
            "create",
            json!({"name": format!("{} (reference)", sheet.name), "base": sheet.base}),
        )
        .await;
        let actor = json!({"actorId": reference_id});
        let mut transcript = Vec::new();
        let mut blocked = None;
        let mut not_exercised = Vec::new();
        'roots: for root in [&choices.species, &choices.background, &choices.class] {
            let mut step = call(page, "drop", json!({"actorId": reference_id, "uuid": root})).await;
            transcript.push(json!({"drop": root, "state": step}));
            while step["open"] == true {
                if step["choosable"] == true {
                    match answer_for(&sheet, &resolved, &step) {
                        Ok(Some(answer)) => {
                            let kind = step["kind"].clone();
                            step = call(
                                page,
                                "answer",
                                json!({"actorId": reference_id, "kind": kind, "answer": answer}),
                            )
                            .await;
                            transcript.push(json!({"answer": answer, "state": step}));
                            if step.get("error").is_some_and(Value::is_string)
                                && step["open"].is_null()
                            {
                                blocked =
                                    Some(json!({"check": "reference_blocked", "detail": step}));
                                break 'roots;
                            }
                        }
                        // The builder never answers spell choices; neither does the
                        // reference (reported, not compared).
                        Ok(None) if step["spellChoice"] == true => not_exercised.push(step.clone()),
                        Ok(None) => {
                            blocked = Some(json!({"check": "unanswered", "detail": step}));
                            break 'roots;
                        }
                        Err(code) => {
                            blocked = Some(json!({"check": code, "detail": step}));
                            break 'roots;
                        }
                    }
                }
                step = call(page, "next", actor.clone()).await;
                transcript.push(json!({"next": step}));
                if step["error"].is_string() {
                    blocked = Some(json!({"check": "reference_blocked", "detail": step}));
                    break 'roots;
                }
            }
        }
        if blocked.is_none() {
            for (root, option) in &sheet.equipment {
                let uuid = if *root == sheet.roots.class {
                    &choices.class
                } else {
                    &choices.background
                };
                // `kit` is the first option, `kit2` the second; gold is not a kit.
                let index = match option.strip_prefix("kit") {
                    Some("") => 0,
                    Some(n) => n.parse::<usize>().unwrap() - 1,
                    None => panic!("the sheet buys only kits, not {option}"),
                };
                let plan = call(
                    page,
                    "kit",
                    json!({"actorId": reference_id, "root": uuid, "option": index}),
                )
                .await;
                transcript.push(json!({"kit": root, "plan": plan}));
            }
        }
        let reference = call(page, "snapshot", actor.clone()).await;
        let created = call(page, "snapshot", json!({"actorId": actor_id})).await;
        let diffs = compare(&reference, &created);
        phases.insert("reference".into(), json!(started.elapsed().as_secs_f32()));

        // C5: the player's own session, through the broker's entry.
        let player_page = enter_page(&pool, &state, &config, open.id, PLAYER, &world_id).await;
        let c5 = call(
            &player_page.page,
            "owner",
            json!({"actorId": actor_id, "ownerId": owner}),
        )
        .await;
        let c5_pass = c5["isPlayer"] == true
            && c5["character"] == actor_id.as_str()
            && c5["owner"] == true
            && c5["sheet"] == true;
        phases.insert("c5".into(), json!(started.elapsed().as_secs_f32()));
        player_page.close().await;
        gm_page.close().await;

        let pass = blocked.is_none() && diffs.is_empty() && c5_pass;
        for (name, value) in [
            ("reference-actor.json", &reference),
            ("created-actor.json", &created),
            ("transcript.json", &json!(transcript)),
        ] {
            std::fs::write(
                evidence.join(name),
                serde_json::to_vec_pretty(value).unwrap(),
            )
            .unwrap();
        }
        let report = json!({
            "verdict": if pass { "pass" } else { "fail" },
            "c4": {"blocked": blocked, "diffs": diffs},
            "c5": {"pass": c5_pass, "observed": c5},
            "notExercised": {
                "spellChoices": not_exercised,
                "playerChosenEquipment": "category entries such as \"any holy symbol\", on both sides",
                "outsideTheHarness": [
                    "Rauthy/nix-provenance sign-in",
                    "the server-function wrappers (member(), role checks)",
                    "create_draft, update_draft and submit_draft (the draft row is inserted as submitted)"
                ],
            },
            "actorId": actor_id,
            "referenceActorId": reference_id,
            "ownerUserId": owner,
            "phases": phases,
            "worldId": world_id,
            "packs": packs,
            "licence": {"host": read_json(&licence)["host"], "version": read_json(&licence)["version"]},
            "foundry": foundry,
            "dnd5e": dnd5e,
        });
        std::fs::write(
            evidence.join("report.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        println!("evidence: {}", evidence.display());
        assert!(
            pass,
            "C4/C5 failed; see {}",
            evidence.join("report.json").display()
        );
    }
}
