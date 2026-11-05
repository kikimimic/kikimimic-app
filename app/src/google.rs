//! Google sign-in and the Calendar API. Blocking; callers run it on a thread.
//! Nothing here runs unless the user pressed "Sign in with Google".

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::calendar::{Meeting, parse_events};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const API: &str = "https://www.googleapis.com/calendar/v3";
const SCOPES: [&str; 2] = [
    "https://www.googleapis.com/auth/calendar.calendarlist.readonly",
    "https://www.googleapis.com/auth/calendar.events.readonly",
];
const KEYCHAIN_SERVICE: &str = "io.github.kikimimic.app.google";
const KEYCHAIN_USER: &str = "account";
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_PAGES: usize = 20;

/// A Google OAuth client of type "Desktop app", baked in at build time.
#[derive(Clone, Copy)]
pub struct Client {
    id: &'static str,
    secret: &'static str,
}

/// `None` for a build made without `KKM_GOOGLE_CLIENT_ID` and `_SECRET`.
pub fn client() -> Option<Client> {
    let id = option_env!("KKM_GOOGLE_CLIENT_ID")?.trim();
    let secret = option_env!("KKM_GOOGLE_CLIENT_SECRET")?.trim();
    (!id.is_empty() && !secret.is_empty()).then_some(Client { id, secret })
}

#[derive(Debug, PartialEq)]
pub enum Error {
    Denied(String),
    TimedOut,
    Cancelled,
    NoRefreshToken,
    MissingScopes,
    Other(String),
}

impl Error {
    fn other(e: impl std::fmt::Display) -> Self {
        Self::Other(e.to_string())
    }
}

/// What the keychain holds, as one JSON item.
#[derive(Clone, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    pub refresh_token: String,
}

fn entry() -> Result<keyring::Entry, Error> {
    keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER).map_err(Error::other)
}

fn load_account() -> Result<Option<Account>, Error> {
    match entry()?.get_password() {
        Ok(json) => serde_json::from_str(&json).map(Some).map_err(Error::other),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(Error::other(e)),
    }
}

fn save_account(account: &Account) -> Result<(), Error> {
    let json = serde_json::to_string(account).map_err(Error::other)?;
    entry()?.set_password(&json).map_err(Error::other)
}

pub fn sign_out() -> Result<(), Error> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(Error::other(e)),
    }
}

fn base64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS has randomness");
    base64url(&bytes)
}

fn pkce_challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

/// Google lets the user untick a scope on the consent screen. A grant missing
/// one would sign in and then never read a meeting.
fn grants_everything(granted: &str) -> bool {
    SCOPES
        .iter()
        .all(|needed| granted.split(' ').any(|g| g == *needed))
}

fn authorization_url(client_id: &str, redirect_uri: &str, challenge: &str, state: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", &SCOPES.join(" "))
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("access_type", "offline")
        // Google issues a refresh token only on a consent screen.
        .append_pair("prompt", "consent")
        .finish()
        .replace('+', "%20");
    format!("{AUTH_URL}?{query}")
}

#[derive(Debug, PartialEq)]
enum Redirect {
    Code { code: String, state: String },
    Denied(String),
    Unrelated,
}

impl Redirect {
    /// The browser's request to the loopback port. Only a complete request
    /// line counts; the favicon and half-sent lines are `Unrelated`.
    fn parse(raw: &str) -> Self {
        let Some((line, _)) = raw.split_once("\r\n") else {
            return Self::Unrelated;
        };
        let mut parts = line.split(' ');
        let (Some("GET"), Some(target)) = (parts.next(), parts.next()) else {
            return Self::Unrelated;
        };
        let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
            return Self::Unrelated;
        };
        let find = |key: &str| {
            url.query_pairs()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.into_owned())
        };
        if let Some(error) = find("error") {
            Self::Denied(error)
        } else if let (Some(code), Some(state)) = (find("code"), find("state")) {
            Self::Code { code, state }
        } else {
            Self::Unrelated
        }
    }
}

/// A revoked or expired grant cannot recover without the user. Client errors
/// such as `unauthorized_client` are not the same: they point at the embedded
/// client, and dropping the token over them would force a sign-in for nothing.
fn requires_sign_in(token_error_body: &str) -> bool {
    #[derive(Deserialize)]
    struct Body {
        error: String,
    }
    serde_json::from_str::<Body>(token_error_body).is_ok_and(|b| b.error == "invalid_grant")
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into()
}

fn finish(response: ureq::http::Response<ureq::Body>) -> Result<(u16, String), Error> {
    let status = response.status().as_u16();
    let body = response
        .into_body()
        .read_to_string()
        .map_err(Error::other)?;
    Ok((status, body))
}

fn post_form(pairs: &[(&str, &str)]) -> Result<(u16, String), Error> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish();
    finish(
        agent()
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(body)
            .map_err(Error::other)?,
    )
}

fn get(url: &str, access_token: &str) -> Result<String, Error> {
    let response = agent()
        .get(url)
        .header("Authorization", &format!("Bearer {access_token}"))
        .call()
        .map_err(Error::other)?;
    match finish(response)? {
        (200, body) => Ok(body),
        (status, body) => Err(Error::Other(format!(
            "Google Calendar API returned {status}: {}",
            body.chars().take(200).collect::<String>()
        ))),
    }
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
    refresh_token: Option<String>,
    scope: Option<String>,
}

fn token_from(status: u16, body: &str) -> Result<Token, Error> {
    if status != 200 {
        return Err(Error::Other(format!(
            "Google token endpoint returned {status}: {}",
            body.chars().take(200).collect::<String>()
        )));
    }
    serde_json::from_str(body).map_err(Error::other)
}

/// The account's address is the id of its primary calendar, so no identity
/// scope is needed.
fn primary_email(access_token: &str) -> Result<String, Error> {
    #[derive(Deserialize)]
    struct Primary {
        id: String,
    }
    let body = get(
        &format!("{API}/users/me/calendarList/primary"),
        access_token,
    )?;
    let primary: Primary = serde_json::from_str(&body).map_err(Error::other)?;
    Ok(primary.id.to_lowercase())
}

/// A sign-in waiting for the browser: the loopback port is bound, the page to
/// open is `url`.
pub struct Pending {
    listener: TcpListener,
    redirect_uri: String,
    verifier: String,
    state: String,
    pub url: String,
}

pub fn begin(client: Client) -> std::io::Result<Pending> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let redirect_uri = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    let verifier = random_token();
    let state = random_token();
    let url = authorization_url(client.id, &redirect_uri, &pkce_challenge(&verifier), &state);
    Ok(Pending {
        listener,
        redirect_uri,
        verifier,
        state,
        url,
    })
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Kikimimic</title>\
<body style=\"font-family:system-ui;text-align:center;margin-top:20vh\">\
<p>Done. You can close this tab and go back to Kikimimic.</p>\
<p>完了しました。このタブを閉じて Kikimimic に戻ってください。</p>";

fn respond(mut stream: TcpStream, ok: bool) {
    let (status, body) = if ok {
        ("200 OK", DONE_PAGE)
    } else {
        ("404 Not Found", "")
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn read_request_line(stream: &mut TcpStream) -> String {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut raw = Vec::new();
    let mut buf = [0u8; 1024];
    while !raw.windows(2).any(|w| w == b"\r\n") && raw.len() < 8192 {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
    }
    String::from_utf8_lossy(&raw).into_owned()
}

impl Pending {
    /// Waits for the redirect, trades the code for tokens and stores the
    /// refresh token. `cancel` frees the port early.
    pub fn wait(self, client: Client, cancel: &AtomicBool) -> Result<Account, Error> {
        let deadline = Instant::now() + SIGN_IN_TIMEOUT;
        let code = loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(Error::TimedOut);
            }
            let mut stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(e) => return Err(Error::other(e)),
            };
            match Redirect::parse(&read_request_line(&mut stream)) {
                Redirect::Code { code, state } if state == self.state => {
                    respond(stream, true);
                    break code;
                }
                Redirect::Denied(reason) => {
                    respond(stream, true);
                    return Err(Error::Denied(reason));
                }
                _ => respond(stream, false),
            }
        };
        let (status, body) = post_form(&[
            ("client_id", client.id),
            ("client_secret", client.secret),
            ("code", &code),
            ("code_verifier", &self.verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", &self.redirect_uri),
        ])?;
        let token = token_from(status, &body)?;
        let refresh_token = token.refresh_token.ok_or(Error::NoRefreshToken)?;
        if !grants_everything(token.scope.as_deref().unwrap_or("")) {
            return Err(Error::MissingScopes);
        }
        let account = Account {
            email: primary_email(&token.access_token)?,
            refresh_token,
        };
        save_account(&account)?;
        Ok(account)
    }
}

pub enum Fetched {
    SignedOut,
    NeedsSignIn {
        email: String,
    },
    Read {
        email: String,
        meetings: Vec<Meeting>,
    },
    Failed {
        email: Option<String>,
        error: Error,
    },
}

/// The meetings from `now - 1 h` to `now + 36 h` on the primary calendar.
pub fn read_calendar(client: Client, now: DateTime<Utc>) -> Fetched {
    let account = match load_account() {
        Ok(Some(account)) => account,
        Ok(None) => return Fetched::SignedOut,
        Err(error) => return Fetched::Failed { email: None, error },
    };
    let email = account.email.clone();
    let failed = |error| Fetched::Failed {
        email: Some(email.clone()),
        error,
    };
    let (status, body) = match post_form(&[
        ("client_id", client.id),
        ("client_secret", client.secret),
        ("grant_type", "refresh_token"),
        ("refresh_token", &account.refresh_token),
    ]) {
        Ok(response) => response,
        Err(e) => return failed(e),
    };
    if status != 200 && requires_sign_in(&body) {
        let _ = sign_out();
        return Fetched::NeedsSignIn { email };
    }
    let token = match token_from(status, &body) {
        Ok(token) => token,
        Err(e) => return failed(e),
    };
    match list_events(&token.access_token, now) {
        Ok(meetings) => Fetched::Read { email, meetings },
        Err(e) => failed(e),
    }
}

fn list_events(access_token: &str, now: DateTime<Utc>) -> Result<Vec<Meeting>, Error> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Page {
        next_page_token: Option<String>,
    }
    let from =
        (now - chrono::Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let to = (now + chrono::Duration::hours(36)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut meetings = Vec::new();
    let mut page_token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut url =
            Url::parse(&format!("{API}/calendars/primary/events")).map_err(Error::other)?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("timeMin", &from)
                .append_pair("timeMax", &to)
                .append_pair("singleEvents", "true")
                .append_pair("orderBy", "startTime")
                .append_pair("maxResults", "250");
            if let Some(token) = &page_token {
                q.append_pair("pageToken", token);
            }
        }
        let body = get(url.as_str(), access_token)?;
        meetings.extend(parse_events(&body).map_err(Error::other)?);
        let page: Page = serde_json::from_str(&body).map_err(Error::other)?;
        page_token = page.next_page_token;
        if page_token.is_none() {
            break;
        }
    }
    Ok(meetings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc_7636_appendix_b() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifiers_are_long_url_safe_and_fresh() {
        let (a, b) = (random_token(), random_token());
        assert_ne!(a, b);
        assert!(
            a.len() >= 43
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
        );
    }

    #[test]
    fn authorization_url_carries_every_parameter_encoded() {
        let url = authorization_url("id 1", "http://127.0.0.1:5000", "chal", "st&te");
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        for expected in [
            "client_id=id%201",
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A5000",
            "response_type=code",
            "scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcalendar.calendarlist.readonly%20https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcalendar.events.readonly",
            "code_challenge=chal",
            "code_challenge_method=S256",
            "state=st%26te",
            "access_type=offline",
            "prompt=consent",
        ] {
            assert!(url.contains(expected), "{expected} missing from {url}");
        }
    }

    fn get_line(target: &str) -> String {
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
    }

    #[test]
    fn redirect_with_code_and_state() {
        assert_eq!(
            Redirect::parse(&get_line("/?state=s%201&code=4%2F0abc&scope=x")),
            Redirect::Code {
                code: "4/0abc".into(),
                state: "s 1".into()
            }
        );
    }

    #[test]
    fn redirect_with_error() {
        assert_eq!(
            Redirect::parse(&get_line("/?error=access_denied&state=s")),
            Redirect::Denied("access_denied".into())
        );
    }

    #[test]
    fn favicon_and_partial_lines_are_unrelated() {
        assert_eq!(
            Redirect::parse(&get_line("/favicon.ico")),
            Redirect::Unrelated
        );
        assert_eq!(
            Redirect::parse("GET /?code=a&state=b HTTP/1.1"),
            Redirect::Unrelated
        );
        assert_eq!(Redirect::parse("GET /?co"), Redirect::Unrelated);
        assert_eq!(Redirect::parse(""), Redirect::Unrelated);
        assert_eq!(Redirect::parse(&get_line("/?code=a")), Redirect::Unrelated);
        assert_eq!(
            Redirect::parse("POST /?code=a&state=b HTTP/1.1\r\n"),
            Redirect::Unrelated
        );
    }

    #[test]
    fn both_scopes_must_be_granted() {
        let both = SCOPES.join(" ");
        assert!(grants_everything(&both));
        assert!(grants_everything(&format!("openid {both}")));
        assert!(!grants_everything(SCOPES[0]));
        assert!(!grants_everything(SCOPES[1]));
        assert!(!grants_everything(""));
    }

    #[test]
    fn only_a_dead_grant_asks_for_sign_in() {
        assert!(requires_sign_in(
            r#"{"error":"invalid_grant","error_description":"Bad"}"#
        ));
        assert!(!requires_sign_in(r#"{"error":"unauthorized_client"}"#));
        assert!(!requires_sign_in(r#"{"error":"invalid_client"}"#));
        assert!(!requires_sign_in("<html>502</html>"));
    }

    #[test]
    fn account_is_one_json_item() {
        let json = serde_json::to_string(&Account {
            email: "a@b.c".into(),
            refresh_token: "r".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"email":"a@b.c","refresh_token":"r"}"#);
    }
}
