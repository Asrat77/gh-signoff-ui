//! Reading GitHub's REST API in-process, with conditional requests.
//!
//! # Why
//!
//! Reading a repository through `gh` spawned a process per call, and every
//! call counted against the rate limit whether anything had changed or
//! not. Here one keep-alive HTTPS client asks GitHub directly, and sends
//! back the `ETag` it got last time as `If-None-Match`: an unchanged
//! resource answers `304 Not Modified` with no body, which GitHub does not
//! count against the rate limit, and the copy kept here is used again. So
//! "has anything changed?" is nearly free and can be asked often.
//!
//! # What stays with `gh`
//!
//! - **Sign-in.** The token comes from `gh auth token --hostname <host>`,
//!   so gh keeps owning credentials (keychain, GitHub Enterprise hosts,
//!   `GH_TOKEN`). It is held in memory only, never logged or written, and
//!   read again when GitHub answers 401. Without one, the caller reads
//!   through `gh` exactly as before.
//! - **Writes.** `gh signoff` and branch-rule edits still go through `gh`.
//! - **Finding the repository.** Which GitHub repository and host a
//!   checkout belongs to comes from `gh repo view`, once per checkout per
//!   session, so remotes resolve exactly as gh resolves them.
//!
//! # Rate limits
//!
//! Every response's `X-RateLimit-Remaining`/`-Reset` is noted. A `403` or
//! `429` with `Retry-After`, with no requests remaining, or that names the
//! secondary rate limit stops all reads of that host until the time GitHub
//! gave (a minute for a secondary limit without one), and so does falling
//! to [`RESERVE`] remaining, so `gh signoff` keeps a budget for writes.
//! [`Api::next_allowed`] says when reading may start again.
//!
//! # Spike (2026-11-06, through an HTTPS proxy, against api.github.com)
//!
//! - `GET /repos/{o}/{r}`, `/pulls?state=open`, and
//!   `/commits/{sha}/status` all send a strong `ETag` and
//!   `Cache-Control: private, max-age=60`; sending it back as
//!   `If-None-Match` answers `304` with the same `ETag`, and
//!   `X-RateLimit-Used` did not move for a 304 while a 200 cost one.
//! - Branch protection answers `404 {"message":"Branch not protected"}`
//!   with no `ETag`, and costs a request every time. That is why
//!   protection is also kept for [`PROTECTION_FRESH_FOR`] without asking.
//! - `ETag`, `Link` and the `X-RateLimit-*` headers arrive intact through
//!   the proxy; `Vary` includes `Authorization`, so a changed token simply
//!   answers 200 once.
//! - `/pulls` sends a `Link` header with `rel="next"` pointing at
//!   `/repositories/{id}/…`: pages are followed only on the API host.
//! - [`UreqTransport`] itself (the ignored `live_` test) got 200 then 304
//!   through the proxy, trusting its CA from the platform store
//!   (`SSL_CERT_FILE` on Linux), with `X-RateLimit-Remaining` unchanged.
//!   The spike ran with a token the proxy added, so the free 304 is
//!   confirmed for authenticated requests, which is the only kind sent.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;

use crate::gh::{self, CommitStatus, PrInfo, ProtectionStatus};
use crate::runner::Runner;

/// Requests left in the window at which reads stop until it resets, so
/// writes through `gh` still have room.
pub const RESERVE: u64 = 50;
/// How long branch protection is used without asking again. It changes
/// rarely, and a 404 (the common answer) has no `ETag` to make asking free.
pub const PROTECTION_FRESH_FOR: Duration = Duration::from_secs(10 * 60);
/// How long reads go through `gh` after the API could not be reached at
/// all (a proxy or TLS setup only `gh` understands, say).
pub const OFFLINE_FOR: Duration = Duration::from_secs(5 * 60);
/// How long a host without a token is not asked again.
const NO_TOKEN_FOR: Duration = Duration::from_secs(60);
/// The wait after a secondary rate limit that names no time.
const SECONDARY_WAIT: SignedDuration = SignedDuration::from_secs(60);
/// The longest wait honoured; GitHub's windows are an hour.
const MAX_WAIT: SignedDuration = SignedDuration::from_secs(65 * 60);
/// Pages of commit statuses followed (100 each).
const MAX_STATUS_PAGES: usize = 3;
/// Responses kept; the cache is cleared when it grows past this.
const MAX_CACHED: usize = 2048;
/// The largest response body read.
const MAX_BODY: u64 = 16 * 1024 * 1024;

const ACCEPT: &str = "application/vnd.github+json";
const API_VERSION: &str = "2022-11-28";

/// What came back for one request: enough for the client, nothing more.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpResponse {
    /// The status code.
    pub status: u16,
    /// Header names and values.
    pub headers: Vec<(String, String)>,
    /// The body, empty for a 304.
    pub body: String,
}

impl HttpResponse {
    /// The first header called `name`, ignoring case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Sends GET requests. Tests replay recorded answers through it.
pub trait Transport: Send + Sync {
    /// GETs `url` with `headers` and returns any HTTP answer, error statuses
    /// included.
    ///
    /// # Errors
    ///
    /// Only when no answer arrived: DNS, TLS, proxy, timeout.
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, String>;
}

/// The real transport: one `ureq` agent, so connections are kept alive and
/// reused across reads.
///
/// TLS is rustls with the platform verifier, so a CA installed in the OS
/// store (a corporate proxy's, say) is trusted on macOS, Windows and Linux;
/// `HTTPS_PROXY`/`ALL_PROXY` and `NO_PROXY` are read from the environment,
/// as `gh` reads them. Plain HTTP is refused, and the token is sent across
/// a redirect only to the same host.
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl fmt::Debug for UreqTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UreqTransport").finish_non_exhaustive()
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        use ureq::config::RedirectAuthHeaders;
        use ureq::tls::{RootCerts, TlsConfig};
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .https_only(true)
            .redirect_auth_headers(RedirectAuthHeaders::SameHost)
            .user_agent(concat!("signoff/", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(30)))
            .max_idle_connections_per_host(8)
            .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
            .build();
        Self { agent: config.new_agent() }
    }
}

impl Transport for UreqTransport {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, String> {
        let mut request = self.agent.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut response = request.call().map_err(|err| err.to_string())?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned())))
            .collect();
        let body = response.body_mut().with_config().limit(MAX_BODY).read_to_string().map_err(|err| err.to_string())?;
        Ok(HttpResponse { status, headers, body })
    }
}

/// Why a read through the API did not work.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// `gh auth token` gave nothing: read through `gh` instead.
    #[error("no token from gh auth token")]
    NoToken,
    /// No answer from GitHub at all: read through `gh` instead.
    #[error("connecting to the GitHub API: {0}")]
    Transport(String),
    /// Reading waits until `until`.
    #[error("GitHub's API rate limit is used up until {until}")]
    RateLimited {
        /// When reading may start again.
        until: Timestamp,
    },
    /// The token was refused, also after reading it again.
    #[error("HTTP 401: Bad credentials; run gh auth login")]
    Unauthorized,
    /// Any other answer than success.
    #[error("HTTP {status}: {message}")]
    Status {
        /// The status code.
        status: u16,
        /// GitHub's `message`, or the start of the body.
        message: String,
    },
    /// The body was not the JSON expected.
    #[error("parsing {url}: {source}")]
    Json {
        /// What was read.
        url: String,
        /// Why.
        source: serde_json::Error,
    },
}

impl ApiError {
    /// Whether the read should go through `gh` instead: the API could not be
    /// used at all, as opposed to answering something.
    pub fn falls_back(&self) -> bool {
        matches!(self, Self::NoToken | Self::Transport(_))
    }

    /// Whether GitHub answered `status`.
    pub fn is_status(&self, status: u16) -> bool {
        matches!(self, Self::Status { status: s, .. } if *s == status)
    }
}

/// A token from `gh`. Never printed.
#[derive(Clone, PartialEq, Eq)]
struct Token(String);

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(…)")
    }
}

#[derive(Debug)]
enum TokenSlot {
    Present(Token),
    /// `gh auth token` gave nothing at this time.
    Missing(Instant),
}

/// The last answer for one URL.
#[derive(Clone, Debug)]
struct Cached {
    etag: Option<String>,
    status: u16,
    body: Arc<str>,
    next: Option<String>,
    /// When GitHub last vouched for it, with a 200 or a 304.
    at: Instant,
}

/// One page of a successful answer.
#[derive(Clone, Debug)]
struct Page {
    body: Arc<str>,
    next: Option<String>,
}

/// The GitHub repository a checkout belongs to, resolved once per session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Remote {
    /// `github.com`, or a GitHub Enterprise host.
    pub host: String,
    /// `owner/name`.
    pub name_with_owner: String,
    /// Its default branch.
    pub default_branch: String,
}

/// The REST client: tokens per host, the `ETag` cache, rate-limit state and
/// resolved remotes, shared by every read.
pub struct Api {
    transport: Arc<dyn Transport>,
    runner: Arc<dyn Runner>,
    tokens: Mutex<HashMap<String, TokenSlot>>,
    cache: Mutex<HashMap<String, Cached>>,
    /// Per host: no reads before this.
    blocked: Mutex<HashMap<String, Timestamp>>,
    remotes: Mutex<HashMap<PathBuf, Remote>>,
    offline_since: Mutex<Option<Instant>>,
}

impl fmt::Debug for Api {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Api").finish_non_exhaustive()
    }
}

impl Api {
    /// A client sending through `transport`, getting tokens by running
    /// `gh auth token` through `runner`.
    pub fn new(transport: Arc<dyn Transport>, runner: Arc<dyn Runner>) -> Self {
        Self {
            transport,
            runner,
            tokens: Mutex::default(),
            cache: Mutex::default(),
            blocked: Mutex::default(),
            remotes: Mutex::default(),
            offline_since: Mutex::default(),
        }
    }

    /// Whether to try the API at all: not for [`OFFLINE_FOR`] after it
    /// could not be reached.
    pub fn usable(&self) -> bool {
        lock(&self.offline_since).is_none_or(|since| since.elapsed() >= OFFLINE_FOR)
    }

    /// When reading may start again, if a rate limit stopped it.
    pub fn next_allowed(&self) -> Option<Timestamp> {
        let now = Timestamp::now();
        lock(&self.blocked).values().copied().filter(|until| *until > now).max()
    }

    /// The remote resolved for the checkout at `path` this session.
    pub fn remote(&self, path: &Path) -> Option<Remote> {
        lock(&self.remotes).get(path).cloned()
    }

    /// Remembers what the checkout at `path` belongs to.
    pub fn remember(&self, path: &Path, remote: Remote) {
        lock(&self.remotes).insert(path.to_owned(), remote);
    }

    /// Resolves the checkout at `path` again on its next read: the
    /// repository moved, or access to it changed.
    pub fn forget(&self, path: &Path) {
        lock(&self.remotes).remove(path);
    }

    /// Asks again on the next read for branch protection of `branch` in
    /// `repo`, after the app changed it.
    pub fn forget_protection(&self, repo: &str, branch: &str) {
        let suffix = format!("/repos/{repo}/branches/{}/protection", path_segment(branch));
        lock(&self.cache).retain(|url, _| !url.ends_with(&suffix));
    }

    // --- Reads -----------------------------------------------------------

    /// The open pull requests of `remote`, newest first, at most
    /// [`gh::PULL_REQUEST_LIMIT`], and whether more were open. Their
    /// `contexts` are left empty: [`Api::commit_statuses`] reads them.
    ///
    /// # Errors
    ///
    /// When the API cannot be used, or answers anything but success.
    pub fn pull_requests(&self, remote: &Remote) -> Result<(Vec<PrInfo>, bool), ApiError> {
        // One more than shown says whether the list was cut, as `gh pr list`
        // did, so one page is always enough.
        let url = format!(
            "{}/repos/{}/pulls?state=open&per_page={}",
            api_base(&remote.host),
            remote.name_with_owner,
            gh::PULL_REQUEST_LIMIT + 1
        );
        let page = self.get(&remote.host, &url, None)?;
        let raw: Vec<RestPull> = parse(&url, &page.body)?;
        let (raw, truncated) = gh::limit_pull_requests(raw);
        Ok((raw.into_iter().map(RestPull::into_info).collect(), truncated))
    }

    /// The latest status of every context on `sha`, as the combined status
    /// API reports it, with who posted it.
    ///
    /// # Errors
    ///
    /// When the API cannot be used, or answers anything but success.
    pub fn commit_statuses(&self, remote: &Remote, sha: &str) -> Result<Vec<CommitStatus>, ApiError> {
        let base = api_base(&remote.host);
        let mut url =
            format!("{base}/repos/{}/commits/{}/status?per_page=100", remote.name_with_owner, path_segment(sha));
        let mut statuses = Vec::new();
        for _ in 0..MAX_STATUS_PAGES {
            let page = self.get(&remote.host, &url, None)?;
            statuses.extend(gh::parse_combined_status(&page.body).map_err(|source| ApiError::Json { url, source })?);
            // Follow only links on the same API, so the token never leaves it.
            match page.next.filter(|next| next.starts_with(&format!("{base}/"))) {
                Some(next) => url = next,
                None => break,
            }
        }
        Ok(statuses)
    }

    /// The status contexts branch protection requires on `branch`, and
    /// which of the three outcomes reading it had. Kept for
    /// [`PROTECTION_FRESH_FOR`].
    ///
    /// # Errors
    ///
    /// Only when the API cannot be used or reading must wait; any answer
    /// from GitHub is one of the outcomes.
    pub fn protection_contexts(
        &self,
        remote: &Remote,
        branch: &str,
    ) -> Result<(Vec<String>, ProtectionStatus), ApiError> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Protection {
            required_status_checks: Option<Required>,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Required {
            contexts: Vec<String>,
        }
        let url = format!(
            "{}/repos/{}/branches/{}/protection",
            api_base(&remote.host),
            remote.name_with_owner,
            path_segment(branch)
        );
        match self.get(&remote.host, &url, Some(PROTECTION_FRESH_FOR)) {
            Ok(page) => {
                let protection: Protection = parse(&url, &page.body)?;
                let contexts = protection.required_status_checks.map(|r| r.contexts).unwrap_or_default();
                Ok((contexts, ProtectionStatus::Ok))
            }
            Err(ApiError::Status { message, .. }) if message.contains("Branch not protected") => {
                Ok((Vec::new(), ProtectionStatus::Absent))
            }
            Err(ApiError::Status { .. } | ApiError::Json { .. }) => Ok((Vec::new(), ProtectionStatus::Unreadable)),
            Err(err) => Err(err),
        }
    }

    // --- Requests --------------------------------------------------------

    /// GETs `url` from `host`'s API, conditionally when a copy is kept.
    /// With `fresh_for`, a copy (or a 4xx answer) younger than that is used
    /// without asking.
    fn get(&self, host: &str, url: &str, fresh_for: Option<Duration>) -> Result<Page, ApiError> {
        if let Some(until) = self.blocked_until(host) {
            return Err(ApiError::RateLimited { until });
        }
        let cached = lock(&self.cache).get(url).cloned();
        if let (Some(copy), Some(fresh_for)) = (&cached, fresh_for)
            && copy.at.elapsed() < fresh_for
        {
            return answer(copy);
        }
        let mut token = self.token(host).ok_or(ApiError::NoToken)?;
        let mut reread = false;
        loop {
            let authorization = format!("Bearer {}", token.0);
            let mut headers =
                vec![("Accept", ACCEPT), ("X-GitHub-Api-Version", API_VERSION), ("Authorization", &authorization)];
            if let Some(etag) = cached.as_ref().and_then(|c| c.etag.as_deref()) {
                headers.push(("If-None-Match", etag));
            }
            let response = match self.transport.get(url, &headers) {
                Ok(response) => response,
                Err(err) => {
                    *lock(&self.offline_since) = Some(Instant::now());
                    return Err(ApiError::Transport(err));
                }
            };
            *lock(&self.offline_since) = None;
            if let Some(until) = self.note_rate_limit(host, &response) {
                return Err(ApiError::RateLimited { until });
            }
            match response.status {
                304 => {
                    let Some(mut copy) = cached else {
                        return Err(ApiError::Status {
                            status: 304,
                            message: "Not Modified, with nothing kept".into(),
                        });
                    };
                    copy.at = Instant::now();
                    self.keep(url, copy.clone());
                    return answer(&copy);
                }
                401 if !reread => {
                    // A token gh has since refreshed, or one revoked: ask gh
                    // again once, and retry only with a different one.
                    reread = true;
                    lock(&self.tokens).remove(host);
                    match self.token(host) {
                        Some(fresh) if fresh != token => token = fresh,
                        _ => return Err(ApiError::Unauthorized),
                    }
                }
                401 => {
                    lock(&self.tokens).remove(host);
                    return Err(ApiError::Unauthorized);
                }
                status => {
                    let copy = Cached {
                        etag: response.header("etag").map(ToOwned::to_owned),
                        status,
                        next: response.header("link").and_then(next_link),
                        body: response.body.into(),
                        at: Instant::now(),
                    };
                    let worth_keeping = if (200..300).contains(&status) {
                        copy.etag.is_some() || fresh_for.is_some()
                    } else {
                        fresh_for.is_some() && (400..500).contains(&status)
                    };
                    if worth_keeping {
                        self.keep(url, copy.clone());
                    }
                    return answer(&copy);
                }
            }
        }
    }

    fn keep(&self, url: &str, copy: Cached) {
        let mut cache = lock(&self.cache);
        if cache.len() >= MAX_CACHED && !cache.contains_key(url) {
            cache.clear();
        }
        cache.insert(url.to_owned(), copy);
    }

    /// A token for `host`, from memory or from `gh auth token`.
    fn token(&self, host: &str) -> Option<Token> {
        // Held while gh runs, so concurrent reads ask it once.
        let mut tokens = lock(&self.tokens);
        match tokens.get(host) {
            Some(TokenSlot::Present(token)) => return Some(token.clone()),
            Some(TokenSlot::Missing(at)) if at.elapsed() < NO_TOKEN_FOR => return None,
            _ => {}
        }
        let token = self
            .runner
            .run(None, None, "gh", &["auth", "token", "--hostname", host])
            .ok()
            .filter(|out| out.code == 0)
            .map(|out| out.stdout.trim().to_owned())
            .filter(|token| !token.is_empty() && !token.contains(char::is_whitespace))
            .map(Token);
        let slot = match &token {
            Some(token) => TokenSlot::Present(token.clone()),
            None => TokenSlot::Missing(Instant::now()),
        };
        tokens.insert(host.to_owned(), slot);
        token
    }

    fn blocked_until(&self, host: &str) -> Option<Timestamp> {
        let mut blocked = lock(&self.blocked);
        match blocked.get(host) {
            Some(until) if *until > Timestamp::now() => Some(*until),
            Some(_) => {
                blocked.remove(host);
                None
            }
            None => None,
        }
    }

    /// Notes the rate-limit headers of `response`, and returns until when
    /// `host` must not be read if they say to stop. A 403 or 429 that says so
    /// is that stop; a success that leaves [`RESERVE`] or fewer requests is
    /// still used, but the next read waits.
    fn note_rate_limit(&self, host: &str, response: &HttpResponse) -> Option<Timestamp> {
        let now = Timestamp::now();
        let number = |name: &str| response.header(name).and_then(|v| v.trim().parse::<i64>().ok());
        let remaining = number("x-ratelimit-remaining");
        let reset = number("x-ratelimit-reset").and_then(|s| Timestamp::from_second(s).ok());
        let retry_after =
            number("retry-after").map(|s| now + SignedDuration::from_secs(s.clamp(0, MAX_WAIT.as_secs())));
        let refused = matches!(response.status, 403 | 429);
        let wait = if refused {
            if let Some(at) = retry_after {
                Some(at)
            } else if remaining == Some(0) {
                Some(reset.unwrap_or(now + SECONDARY_WAIT))
            } else if response.body.to_ascii_lowercase().contains("secondary rate limit") {
                Some(now + SECONDARY_WAIT)
            } else {
                None
            }
        } else if remaining.is_some_and(|r| r <= RESERVE as i64) {
            reset
        } else {
            None
        };
        let until = wait.filter(|at| *at > now).map(|at| at.min(now + MAX_WAIT))?;
        log::warn!("GitHub rate limit: not reading {host} until {until}");
        lock(&self.blocked).insert(host.to_owned(), until);
        // A success is still an answer; only a refusal stops this request.
        refused.then_some(until)
    }
}

/// The API root for `host`.
pub fn api_base(host: &str) -> String {
    match host {
        "" | "github.com" => "https://api.github.com".to_owned(),
        // GitHub Enterprise Cloud with data residency.
        host if host.ends_with(".ghe.com") => format!("https://api.{host}"),
        // GitHub Enterprise Server.
        host => format!("https://{host}/api/v3"),
    }
}

/// The host of a repository's web URL, `github.com` when it has none.
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    if host.is_empty() { "github.com".to_owned() } else { host.to_ascii_lowercase() }
}

/// `value` escaped for one segment of a URL path, so a branch such as
/// `release/1.0` stays one segment.
pub(crate) fn path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A kept answer as the caller sees it.
fn answer(copy: &Cached) -> Result<Page, ApiError> {
    if (200..300).contains(&copy.status) {
        return Ok(Page { body: copy.body.clone(), next: copy.next.clone() });
    }
    Err(ApiError::Status { status: copy.status, message: github_message(&copy.body) })
}

/// GitHub's `message`, or the start of the body.
fn github_message(body: &str) -> String {
    #[derive(Deserialize)]
    struct Message {
        message: String,
    }
    match serde_json::from_str::<Message>(body) {
        Ok(m) => m.message,
        Err(_) => body.chars().take(200).collect(),
    }
}

/// The `rel="next"` URL of a `Link` header.
fn next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        params
            .split(';')
            .any(|p| p.trim() == "rel=\"next\"")
            .then(|| url.trim().trim_start_matches('<').trim_end_matches('>').to_owned())
    })
}

fn parse<T: serde::de::DeserializeOwned>(url: &str, body: &str) -> Result<T, ApiError> {
    serde_json::from_str(body).map_err(|source| ApiError::Json { url: url.to_owned(), source })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One pull request as `GET /repos/{o}/{r}/pulls` lists it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RestPull {
    number: u64,
    title: String,
    html_url: String,
    state: String,
    user: Option<RestUser>,
    head: RestRef,
    base: RestRef,
    draft: bool,
    #[serde(deserialize_with = "gh::timestamp_or_default")]
    updated_at: Timestamp,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RestUser {
    login: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RestRef {
    sha: String,
    #[serde(rename = "ref")]
    name: String,
}

impl RestPull {
    /// The same [`PrInfo`] `gh pr list` gave: upper-case state, and a bot
    /// spelled `app/<name>` as GraphQL spells it.
    fn into_info(self) -> PrInfo {
        let author = self
            .user
            .map(|user| match user.login.strip_suffix("[bot]") {
                Some(name) if user.kind == "Bot" => format!("app/{name}"),
                _ => user.login,
            })
            .unwrap_or_default();
        PrInfo {
            number: self.number,
            title: self.title,
            author,
            url: self.html_url,
            state: self.state.to_ascii_uppercase(),
            head_sha: self.head.sha,
            head_ref: self.head.name,
            base_ref: self.base.name,
            // The list does not compute it; GraphQL's word for that.
            mergeable: "UNKNOWN".into(),
            is_draft: self.draft,
            updated_at: self.updated_at,
            contexts: Vec::new(),
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A recorded transport for tests.

    use super::{HttpResponse, Transport};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// One recorded request.
    #[derive(Clone, Debug)]
    pub(crate) struct Sent {
        pub(crate) url: String,
        pub(crate) headers: Vec<(String, String)>,
    }

    impl Sent {
        pub(crate) fn header(&self, name: &str) -> Option<&str> {
            self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
        }
    }

    /// Answers each request with the first queued answer whose needle the
    /// URL contains, and records every request.
    #[derive(Default)]
    pub(crate) struct FakeTransport {
        answers: Mutex<VecDeque<(String, Result<HttpResponse, String>)>>,
        pub(crate) sent: Mutex<Vec<Sent>>,
    }

    impl FakeTransport {
        pub(crate) fn answer(&self, needle: &str, response: HttpResponse) -> &Self {
            self.answers.lock().expect("answers").push_back((needle.into(), Ok(response)));
            self
        }

        pub(crate) fn fail(&self, needle: &str, error: &str) -> &Self {
            self.answers.lock().expect("answers").push_back((needle.into(), Err(error.into())));
            self
        }

        pub(crate) fn sent(&self) -> Vec<Sent> {
            self.sent.lock().expect("sent").clone()
        }

        /// Requests whose URL contains `needle`.
        pub(crate) fn count(&self, needle: &str) -> usize {
            self.sent().iter().filter(|s| s.url.contains(needle)).count()
        }
    }

    pub(crate) fn json(status: u16, etag: Option<&str>, body: &str) -> HttpResponse {
        let mut headers = vec![("X-RateLimit-Remaining".to_owned(), "4999".to_owned())];
        if let Some(etag) = etag {
            headers.push(("ETag".into(), etag.into()));
        }
        HttpResponse { status, headers, body: body.into() }
    }

    pub(crate) fn not_modified(etag: &str) -> HttpResponse {
        HttpResponse { status: 304, headers: vec![("ETag".into(), etag.into())], body: String::new() }
    }

    impl Transport for FakeTransport {
        fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, String> {
            self.sent.lock().expect("sent").push(Sent {
                url: url.into(),
                headers: headers.iter().map(|(n, v)| ((*n).to_owned(), (*v).to_owned())).collect(),
            });
            let mut answers = self.answers.lock().expect("answers");
            let index = answers
                .iter()
                .position(|(needle, _)| url.contains(needle.as_str()))
                .unwrap_or_else(|| panic!("unexpected request: {url}"));
            answers.remove(index).map(|(_, answer)| answer).unwrap_or_else(|| Err("gone".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeTransport, json, not_modified};
    use super::*;
    use crate::checks::CheckState;
    use crate::runner::Output;
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// `gh auth token` answering each token in turn, the last one forever;
    /// none means not signed in.
    struct Tokens {
        tokens: Vec<&'static str>,
        asked: AtomicUsize,
    }

    impl Runner for Tokens {
        fn run(&self, _dir: Option<&Path>, _input: Option<&str>, program: &str, args: &[&str]) -> io::Result<Output> {
            assert_eq!((program, &args[..3]), ("gh", &["auth", "token", "--hostname"][..]));
            let n = self.asked.fetch_add(1, Ordering::SeqCst);
            Ok(match self.tokens.get(n.min(self.tokens.len().saturating_sub(1))) {
                Some(token) => Output { stdout: (*token).into(), ..Output::default() },
                None => Output { stderr: "no oauth token found for github.com".into(), code: 1, ..Output::default() },
            })
        }
    }

    fn client(tokens: &[&'static str]) -> (Api, Arc<FakeTransport>, Arc<Tokens>) {
        let transport = Arc::new(FakeTransport::default());
        let runner = Arc::new(Tokens { tokens: tokens.to_vec(), asked: AtomicUsize::new(0) });
        (Api::new(transport.clone(), runner.clone()), transport, runner)
    }

    fn remote() -> Remote {
        Remote { host: "github.com".into(), name_with_owner: "o/r".into(), default_branch: "main".into() }
    }

    const PULLS: &str = r#"[{
        "number":7,"title":"Add retry","html_url":"https://github.com/o/r/pull/7","state":"open",
        "user":{"login":"dependabot[bot]","type":"Bot"},"draft":true,
        "head":{"sha":"aaa","ref":"retry"},"base":{"sha":"x","ref":"main"},
        "updated_at":"2026-08-10T08:18:52Z"}]"#;

    #[test]
    fn pull_requests_read_like_gh_did() {
        let (api, transport, _) = client(&["t0k"]);
        transport.answer("/pulls", json(200, Some("\"p1\""), PULLS));
        let (prs, truncated) = api.pull_requests(&remote()).expect("list");
        assert!(!truncated);
        let pr = &prs[0];
        assert_eq!((pr.number, pr.state.as_str(), pr.author.as_str()), (7, "OPEN", "app/dependabot"));
        assert_eq!((pr.head_sha.as_str(), pr.head_ref.as_str(), pr.base_ref.as_str()), ("aaa", "retry", "main"));
        assert!(pr.is_draft);
        assert_eq!(pr.updated_at.to_string(), "2026-08-10T08:18:52Z");

        let sent = &transport.sent()[0];
        assert_eq!(sent.url, "https://api.github.com/repos/o/r/pulls?state=open&per_page=31");
        assert_eq!(sent.header("Authorization"), Some("Bearer t0k"));
        assert_eq!(sent.header("If-None-Match"), None);
    }

    #[test]
    fn pull_requests_report_truncation() {
        let (api, transport, _) = client(&["t"]);
        let many: Vec<String> = (1..=gh::PULL_REQUEST_LIMIT + 1).map(|n| format!(r#"{{"number":{n}}}"#)).collect();
        transport.answer("/pulls", json(200, None, &format!("[{}]", many.join(","))));
        let (prs, truncated) = api.pull_requests(&remote()).expect("list");
        assert!(truncated);
        assert_eq!(prs.len(), gh::PULL_REQUEST_LIMIT);
    }

    /// The point of the module: a 304 answers with the kept copy, and sends
    /// the `ETag` it was given.
    #[test]
    fn a_304_reuses_the_kept_body() {
        let (api, transport, runner) = client(&["t"]);
        transport.answer("/pulls", json(200, Some("\"p1\""), PULLS)).answer("/pulls", not_modified("\"p1\""));
        let first = api.pull_requests(&remote()).expect("first");
        let second = api.pull_requests(&remote()).expect("second");
        assert_eq!(first, second);
        let sent = transport.sent();
        assert_eq!(sent[1].header("If-None-Match"), Some("\"p1\""));
        assert_eq!(runner.asked.load(Ordering::SeqCst), 1, "the token is read once");
    }

    #[test]
    fn a_401_reads_the_token_again_once() {
        let (api, transport, runner) = client(&["old", "new"]);
        transport.answer("/pulls", json(401, None, r#"{"message":"Bad credentials"}"#));
        transport.answer("/pulls", json(200, None, "[]"));
        api.pull_requests(&remote()).expect("retried with the new token");
        let sent = transport.sent();
        assert_eq!(sent[0].header("Authorization"), Some("Bearer old"));
        assert_eq!(sent[1].header("Authorization"), Some("Bearer new"));
        assert_eq!(runner.asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_same_token_refused_twice_is_unauthorized() {
        let (api, transport, _) = client(&["same"]);
        transport.answer("/pulls", json(401, None, r#"{"message":"Bad credentials"}"#));
        assert!(matches!(api.pull_requests(&remote()), Err(ApiError::Unauthorized)));
        assert_eq!(transport.sent().len(), 1, "no retry with a token already refused");
    }

    #[test]
    fn no_token_means_no_request() {
        let (api, transport, runner) = client(&[]);
        assert!(matches!(api.pull_requests(&remote()), Err(ApiError::NoToken)));
        assert!(api.pull_requests(&remote()).is_err_and(|e| e.falls_back()));
        assert!(transport.sent().is_empty());
        assert_eq!(runner.asked.load(Ordering::SeqCst), 1, "gh is not asked on every read");
    }

    #[test]
    fn a_used_up_rate_limit_stops_reads_until_reset() {
        let (api, transport, _) = client(&["t"]);
        let reset = Timestamp::now().as_second() + 600;
        transport.answer(
            "/pulls",
            HttpResponse {
                status: 403,
                headers: vec![
                    ("x-ratelimit-remaining".into(), "0".into()),
                    ("x-ratelimit-reset".into(), reset.to_string()),
                ],
                body: r#"{"message":"API rate limit exceeded"}"#.into(),
            },
        );
        let Err(ApiError::RateLimited { until }) = api.pull_requests(&remote()) else { panic!("rate limited") };
        assert_eq!(until.as_second(), reset);
        assert_eq!(api.next_allowed(), Some(until));
        assert!(matches!(api.commit_statuses(&remote(), "aaa"), Err(ApiError::RateLimited { .. })));
        assert_eq!(transport.sent().len(), 1, "nothing is sent while limited");
    }

    #[test]
    fn retry_after_and_a_low_reserve_back_off() {
        let (api, transport, _) = client(&["t"]);
        transport.answer(
            "/pulls",
            HttpResponse {
                status: 429,
                headers: vec![("Retry-After".into(), "30".into())],
                body: "You have exceeded a secondary rate limit".into(),
            },
        );
        let Err(ApiError::RateLimited { until }) = api.pull_requests(&remote()) else { panic!("rate limited") };
        let wait = until.as_second() - Timestamp::now().as_second();
        assert!((28..=31).contains(&wait), "{wait}");

        // A success that leaves too little is used, and the next read waits.
        let (api, transport, _) = client(&["t"]);
        let reset = Timestamp::now().as_second() + 120;
        transport.answer(
            "/pulls",
            HttpResponse {
                status: 200,
                headers: vec![
                    ("x-ratelimit-remaining".into(), RESERVE.to_string()),
                    ("x-ratelimit-reset".into(), reset.to_string()),
                ],
                body: "[]".into(),
            },
        );
        assert!(api.pull_requests(&remote()).is_ok());
        assert_eq!(api.next_allowed().map(|t| t.as_second()), Some(reset));
        assert!(matches!(api.pull_requests(&remote()), Err(ApiError::RateLimited { .. })));
    }

    #[test]
    fn statuses_follow_pages_on_the_same_api_only() {
        let (api, transport, _) = client(&["t"]);
        let page = |context: &str, next: Option<&str>| {
            let mut response = json(
                200,
                Some("\"s\""),
                &format!(
                    r#"{{"statuses":[{{"context":"{context}","state":"success","description":"Ann signed off",
                    "created_at":"2026-08-10T08:18:36Z","creator":{{"login":"ann","avatar_url":"a"}}}}]}}"#
                ),
            );
            if let Some(next) = next {
                response.headers.push(("Link".into(), format!(r#"<{next}>; rel="next", <{next}>; rel="last""#)));
            }
            response
        };
        transport
            .answer(
                "per_page=100",
                page("signoff/tests", Some("https://api.github.com/repositories/1/commits/aaa/status?page=2")),
            )
            .answer("page=2", page("signoff/lint", Some("https://evil.example/steal")));
        let statuses = api.commit_statuses(&remote(), "aaa").expect("statuses");
        let contexts: Vec<&str> = statuses.iter().map(|s| s.context.as_str()).collect();
        assert_eq!(contexts, ["signoff/tests", "signoff/lint"]);
        assert_eq!(statuses[0].creator, "ann");
        assert_eq!(statuses[0].state, CheckState::Success);
        assert_eq!(transport.sent().len(), 2, "the foreign link is not followed");
    }

    #[test]
    fn protection_is_kept_and_its_outcomes_told_apart() {
        let (api, transport, _) = client(&["t"]);
        transport
            .answer("/branches/main/", json(200, None, r#"{"required_status_checks":{"contexts":["signoff/tests"]}}"#))
            .answer("/branches/dev/", json(404, None, r#"{"message":"Branch not protected"}"#))
            .answer("/branches/rel%2F1/", json(404, None, r#"{"message":"Not Found"}"#));
        let r = remote();
        assert_eq!(
            api.protection_contexts(&r, "main").expect("main"),
            (vec!["signoff/tests".into()], ProtectionStatus::Ok)
        );
        assert_eq!(api.protection_contexts(&r, "dev").expect("dev"), (vec![], ProtectionStatus::Absent));
        assert_eq!(api.protection_contexts(&r, "rel/1").expect("rel"), (vec![], ProtectionStatus::Unreadable));
        for branch in ["main", "dev", "rel/1"] {
            api.protection_contexts(&r, branch).expect("kept");
        }
        assert_eq!(transport.sent().len(), 3, "kept, 404s included");

        api.forget_protection("o/r", "main");
        transport.answer("/branches/main/", json(200, None, r#"{"required_status_checks":null}"#));
        assert_eq!(api.protection_contexts(&r, "main").expect("main"), (vec![], ProtectionStatus::Ok));
        assert_eq!(transport.sent().len(), 4, "asked again after an edit");
    }

    #[test]
    fn an_unreachable_api_is_left_alone_for_a_while() {
        let (api, transport, _) = client(&["t"]);
        transport.fail("/pulls", "proxy refused");
        assert!(api.pull_requests(&remote()).is_err_and(|e| e.falls_back()));
        assert!(!api.usable());
    }

    #[test]
    fn hosts_map_to_their_api() {
        assert_eq!(api_base("github.com"), "https://api.github.com");
        assert_eq!(api_base("octo.ghe.com"), "https://api.octo.ghe.com");
        assert_eq!(api_base("git.example.com"), "https://git.example.com/api/v3");
        assert_eq!(host_of("https://github.com/o/r"), "github.com");
        assert_eq!(host_of("https://Git.Example.com/o/r"), "git.example.com");
        assert_eq!(host_of(""), "github.com");
    }

    #[test]
    fn the_token_is_never_printed() {
        assert_eq!(format!("{:?}", Token("secret".into())), "Token(…)");
    }

    /// The spike, kept runnable: `cargo test -p signoff-core -- --ignored
    /// live_`, with `SIGNOFF_LIVE_URL` to read another resource. Needs network;
    /// through a proxy, `HTTPS_PROXY` must be set.
    #[test]
    #[ignore = "reaches api.github.com"]
    fn live_github_answers_304_to_its_own_etag() {
        let transport = UreqTransport::default();
        let url = std::env::var("SIGNOFF_LIVE_URL")
            .unwrap_or_else(|_| "https://api.github.com/repos/basecamp/gh-signoff".to_owned());
        let url = url.as_str();
        let headers = [("Accept", ACCEPT), ("X-GitHub-Api-Version", API_VERSION)];
        let first = transport.get(url, &headers).expect("first");
        println!("first: {} remaining {:?}", first.status, first.header("x-ratelimit-remaining"));
        let etag = first.header("etag").expect("an ETag").to_owned();
        let mut conditional = headers.to_vec();
        conditional.push(("If-None-Match", &etag));
        let second = transport.get(url, &conditional).expect("second");
        println!("second: {} remaining {:?}", second.status, second.header("x-ratelimit-remaining"));
        assert_eq!(second.status, 304);
        assert!(second.body.is_empty());
        assert!(second.header("x-ratelimit-reset").is_some());
    }
}
