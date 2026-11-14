//! HTTP behaviour of the client against a local fake broker.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use foundry_circle_upload::{AssetKind, Error, MAX_UPLOAD_BYTES, Session, login_with, upload};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[derive(Clone, Debug)]
struct Record {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Record {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn path(&self) -> &str {
        self.target
            .split_once('?')
            .map_or(self.target.as_str(), |(path, _)| path)
    }

    fn query(&self) -> Vec<(&str, &str)> {
        self.target
            .split_once('?')
            .map(|(_, query)| query)
            .unwrap_or_default()
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
            .collect()
    }
}

struct FakeResponse {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

impl FakeResponse {
    fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: vec![("Content-Type", "application/json".to_string())],
            body: body.as_bytes().to_vec(),
        }
    }

    fn redirect(location: String) -> Self {
        Self {
            status: 303,
            headers: vec![("Location", location)],
            body: Vec::new(),
        }
    }

    fn empty(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }
}

struct FakeServer {
    base: String,
    records: Arc<Mutex<Vec<Record>>>,
}

impl FakeServer {
    async fn spawn<H>(handler: H) -> Self
    where
        H: Fn(&Record) -> FakeResponse + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let records = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(handler);
        let stored = records.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                let stored = stored.clone();
                tokio::spawn(async move {
                    let Some(record) = read_request(&mut stream).await else {
                        return;
                    };
                    let response = handler(&record);
                    stored.lock().unwrap().push(record);
                    write_response(&mut stream, &response).await;
                });
            }
        });
        Self { base, records }
    }

    fn records(&self) -> Vec<Record> {
        self.records.lock().unwrap().clone()
    }

    fn session(&self) -> Session {
        Session {
            server: self.base.clone(),
            cookie: "foundry_circle_session".to_string(),
            token: "secret".to_string(),
            expires_at: "2030-01-01T00:00:00Z".to_string(),
        }
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Record> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request = lines.next()?.split_whitespace();
    let method = request.next()?.to_string();
    let target = request.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Some(Record {
        method,
        target,
        headers,
        body,
    })
}

async fn write_response(stream: &mut TcpStream, response: &FakeResponse) {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        reason(response.status),
        response.body.len()
    );
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(&response.body).await;
    let _ = stream.shutdown().await;
}

fn reason(status: u16) -> &'static str {
    match status {
        200 | 201 => "OK",
        303 => "See Other",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

/// A named file in this process's temp directory.
fn scratch(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("foundry-circle-upload-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write");
    path
}

#[tokio::test]
async fn upload_returns_the_path_and_sends_the_session() {
    let server = FakeServer::spawn(|_| {
        FakeResponse::json(201, r#"{"path":"circle/tokens/guard-red.webp"}"#)
    })
    .await;
    let file = scratch("guard-red.webp", b"RIFF\0\0\0\0WEBPVP8 ");

    let path = upload(&server.session(), AssetKind::Token, &file)
        .await
        .expect("upload");
    assert_eq!(path, "circle/tokens/guard-red.webp");

    let records = server.records();
    assert_eq!(records.len(), 1);
    let request = &records[0];
    assert_eq!(request.method, "PUT");
    assert_eq!(request.path(), "/api/v1/assets/token/guard-red.webp");
    assert_eq!(
        request.header("cookie"),
        Some("foundry_circle_session=secret")
    );
    assert_eq!(
        request.header("content-type"),
        Some("application/octet-stream")
    );
    assert_eq!(request.body, b"RIFF\0\0\0\0WEBPVP8 ");
}

#[tokio::test]
async fn upload_maps_conflict_to_exists() {
    let server =
        FakeServer::spawn(|_| FakeResponse::json(409, r#"{"error":"asset_exists"}"#)).await;
    let file = scratch("guard-conflict.webp", b"asset");

    let error = upload(&server.session(), AssetKind::Token, &file)
        .await
        .expect_err("conflict");
    assert!(matches!(error, Error::Exists), "unexpected {error:?}");
}

#[tokio::test]
async fn upload_maps_a_login_redirect_to_expired() {
    let server = FakeServer::spawn(|_| FakeResponse::redirect("/api/auth/login".to_string())).await;
    let file = scratch("guard-redirect.webp", b"asset");

    let error = upload(&server.session(), AssetKind::Token, &file)
        .await
        .expect_err("expired");
    assert!(matches!(error, Error::Expired), "unexpected {error:?}");
}

#[tokio::test]
async fn upload_maps_unauthorized_to_expired() {
    let server = FakeServer::spawn(|_| FakeResponse::empty(401)).await;
    let file = scratch("guard-unauthorized.webp", b"asset");

    let error = upload(&server.session(), AssetKind::Token, &file)
        .await
        .expect_err("expired");
    assert!(matches!(error, Error::Expired), "unexpected {error:?}");
}

#[tokio::test]
async fn upload_maps_forbidden_and_rejected() {
    let server = FakeServer::spawn(|_| FakeResponse::empty(403)).await;
    let file = scratch("guard-refused.webp", b"asset");
    let error = upload(&server.session(), AssetKind::Token, &file)
        .await
        .expect_err("forbidden");
    assert!(matches!(error, Error::Forbidden), "unexpected {error:?}");

    let server =
        FakeServer::spawn(|_| FakeResponse::json(400, r#"{"error":"invalid_name"}"#)).await;
    let error = upload(&server.session(), AssetKind::Token, &file)
        .await
        .expect_err("rejected");
    assert!(
        matches!(&error, Error::Rejected { status, message } if *status == 400 && message == "invalid_name"),
        "unexpected {error:?}"
    );

    let server =
        FakeServer::spawn(|_| FakeResponse::json(503, r#"{"error":"assets_not_configured"}"#))
            .await;
    let error = upload(&server.session(), AssetKind::Token, &file)
        .await
        .expect_err("not configured");
    assert!(
        matches!(error, Error::NotConfigured),
        "unexpected {error:?}"
    );
}

#[tokio::test]
async fn upload_refuses_an_oversized_file_locally() {
    let server = FakeServer::spawn(|_| FakeResponse::empty(201)).await;
    let path = scratch("huge.webp", b"");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open")
        .set_len(MAX_UPLOAD_BYTES + 1)
        .expect("sparse file");

    let error = upload(&server.session(), AssetKind::Token, &path)
        .await
        .expect_err("too large");
    assert!(
        matches!(error, Error::TooLarge { .. }),
        "unexpected {error:?}"
    );
    assert!(server.records().is_empty(), "the file must not be sent");
}

#[tokio::test]
async fn upload_refuses_a_name_the_broker_would_reject() {
    let server = FakeServer::spawn(|_| FakeResponse::empty(201)).await;
    let file = scratch("Guard Red.webp", b"asset");

    let error = upload(&server.session(), AssetKind::Token, &file)
        .await
        .expect_err("invalid name");
    assert!(
        matches!(error, Error::InvalidName(_)),
        "unexpected {error:?}"
    );
    assert!(server.records().is_empty());
}

#[tokio::test]
async fn login_runs_the_loopback_flow_with_pkce() {
    let challenge = Arc::new(Mutex::new(String::new()));
    let slot = challenge.clone();
    let server = FakeServer::spawn(move |request| match request.path() {
        "/api/auth/native" => {
            let params = request.query();
            let param = |name: &str| {
                params
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_string())
                    .unwrap_or_default()
            };
            *slot.lock().unwrap() = param("challenge");
            FakeResponse::redirect(format!(
                "http://127.0.0.1:{}/callback?code=e2e-code&state={}",
                param("port"),
                param("state")
            ))
        }
        "/api/auth/native/token" => {
            let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json");
            let verifier = body["verifier"].as_str().expect("verifier");
            let derived = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
            let expected = slot.lock().unwrap().clone();
            if body["code"] == "e2e-code" && derived == expected && verifier.len() == 43 {
                FakeResponse::json(
                    200,
                    r#"{"token":"e2e-token","cookie":"foundry_circle_session","expires_at":"2030-01-01T00:00:00Z"}"#,
                )
            } else {
                FakeResponse::json(400, r#"{"error":"invalid_grant"}"#)
            }
        }
        _ => FakeResponse::json(404, r#"{"error":"not_found"}"#),
    })
    .await;
    let base = server.base.clone();

    // The "browser": follows the authorize redirect back to the loopback.
    let session = tokio::time::timeout(
        Duration::from_secs(30),
        login_with(&base, |url| {
            let url = url.to_string();
            tokio::spawn(async move {
                // Browsers preconnect: an idle connection to the loopback
                // must not keep the real callback from being answered.
                let port: u16 = url
                    .split("port=")
                    .nth(1)
                    .and_then(|rest| rest.split('&').next())
                    .and_then(|port| port.parse().ok())
                    .expect("loopback port in the authorize URL");
                let _idle = TcpStream::connect(("127.0.0.1", port))
                    .await
                    .expect("idle preconnection");
                let _ = reqwest::get(url).await;
            });
        }),
    )
    .await
    .expect("login finished")
    .expect("login");

    assert_eq!(session.server, base);
    assert_eq!(session.token, "e2e-token");
    assert_eq!(session.cookie, "foundry_circle_session");
    assert_eq!(session.expires_at, "2030-01-01T00:00:00Z");

    let records = server.records();
    assert_eq!(records.len(), 2, "authorize then token exchange");
    let authorize = &records[0];
    assert_eq!(authorize.path(), "/api/auth/native");
    let params = authorize.query();
    let value = |name: &str| {
        params
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| *value)
            .unwrap_or_default()
    };
    assert_eq!(value("state").len(), 43);
    assert_eq!(value("challenge").len(), 43);
    assert_eq!(value("challenge"), challenge.lock().unwrap().clone());
    assert!(!value("port").is_empty(), "the loopback port is announced");
    assert_eq!(records[1].path(), "/api/auth/native/token");
}
