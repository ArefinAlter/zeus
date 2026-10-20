//! Workflow networking: pinned public peers, bounded redirects, robots and per-host pacing.
use crate::workflows::WorkflowStore;
use async_trait::async_trait;
use futures::StreamExt;
use std::{
    collections::{BTreeMap, HashMap},
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};
use zeus_http::{HttpRequest, HttpResponse, HttpTransport, TransportError, TransportFailure};
use zeus_workflow::{Document, Plan, RuntimeError, RuntimeResult};
fn failure() -> TransportError {
    TransportError::new(
        TransportFailure::PolicyDenied,
        "workflow network policy refused or request failed",
    )
}
/// Every selected destination address is vetted before opening a connection.
pub(crate) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 192 && b == 0 && c == 0)
                && !(a == 198 && (b == 18 || b == 19))
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(v4));
            }
            let s = ip.segments();
            // Use assigned general-purpose prefixes, not the entire 2000::/3
            // envelope: unlisted space is reserved. Special-purpose allocations
            // are deliberately excluded even if they are globally reachable.
            let prefix = (u32::from(s[0]) << 16) | u32::from(s[1]);
            GLOBAL_IPV6_PREFIXES
                .iter()
                .any(|&(base, bits)| prefix >> (32 - bits) == base >> (32 - bits))
                && !(s[0] == 0x2001 && s[1] == 0x0db8)
                && !(s[0] == 0x2620 && s[1] == 0x004f && s[2] == 0x8000)
        }
    }
}
// Verified 2026-10-16 against IANA's registries (last updates 2025-10-09/10):
// https://www.iana.org/assignments/ipv6-unicast-address-assignments/
// https://www.iana.org/assignments/iana-ipv6-special-registry/
// Each pair is the first 32 address bits and the prefix length. New IANA
// allocations fail closed until this table and its boundary tests are reviewed.
const GLOBAL_IPV6_PREFIXES: &[(u32, u8)] = &[
    (0x2001_0200, 23),
    (0x2001_0400, 23),
    (0x2001_0600, 23),
    (0x2001_0800, 22),
    (0x2001_0c00, 23),
    (0x2001_0e00, 23),
    (0x2001_1200, 23),
    (0x2001_1400, 22),
    (0x2001_1800, 23),
    (0x2001_1a00, 23),
    (0x2001_1c00, 22),
    (0x2001_2000, 19),
    (0x2001_4000, 23),
    (0x2001_4200, 23),
    (0x2001_4400, 23),
    (0x2001_4600, 23),
    (0x2001_4800, 23),
    (0x2001_4a00, 23),
    (0x2001_4c00, 23),
    (0x2001_5000, 20),
    (0x2001_8000, 19),
    (0x2001_a000, 20),
    (0x2001_b000, 20),
    (0x2003_0000, 18),
    (0x2400_0000, 12),
    (0x2410_0000, 12),
    (0x2600_0000, 12),
    (0x2610_0000, 23),
    (0x2620_0000, 23),
    (0x2630_0000, 12),
    (0x2800_0000, 12),
    (0x2a00_0000, 12),
    (0x2a10_0000, 12),
    (0x2c00_0000, 12),
];
#[derive(Clone)]
struct Robots {
    loaded: Instant,
    allow_all: bool,
    deny_all: bool,
    rules: Vec<(bool, String)>,
    delay_ms: u64,
}
struct RobotsGroup {
    agents: Vec<String>,
    rules: Vec<(bool, String)>,
    delay_ms: u64,
}
impl Robots {
    fn parse(body: &str) -> Self {
        let mut groups = Vec::new();
        let mut agents = Vec::new();
        let mut rules = Vec::new();
        let mut delay = 0;
        let mut have_rules = false;
        for line in body.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match name.trim().to_ascii_lowercase().as_str() {
                "user-agent" => {
                    if have_rules {
                        groups.push(RobotsGroup {
                            agents: std::mem::take(&mut agents),
                            rules: std::mem::take(&mut rules),
                            delay_ms: delay,
                        });
                        delay = 0;
                        have_rules = false;
                    }
                    agents.push(value.to_ascii_lowercase());
                }
                "allow" | "disallow" if !agents.is_empty() => {
                    have_rules = true;
                    if !value.is_empty() {
                        rules.push((name.trim().eq_ignore_ascii_case("allow"), value.into()));
                    }
                }
                "crawl-delay" if !agents.is_empty() => {
                    have_rules = true;
                    delay = value
                        .parse::<f64>()
                        .ok()
                        .filter(|v| v.is_finite() && *v >= 0.)
                        .map(|v| (v * 1000.).min(3_600_000.) as u64)
                        .unwrap_or(0);
                }
                _ => {}
            }
        }
        groups.push(RobotsGroup {
            agents,
            rules,
            delay_ms: delay,
        });
        let exact = groups
            .iter()
            .any(|group| group.agents.iter().any(|a| a == "zeus"));
        let mut result = Self {
            loaded: Instant::now(),
            allow_all: false,
            deny_all: false,
            rules: vec![],
            delay_ms: 0,
        };
        for group in groups {
            if group
                .agents
                .iter()
                .any(|a| if exact { a == "zeus" } else { a == "*" })
            {
                result.rules.extend(group.rules);
                result.delay_ms = result.delay_ms.max(group.delay_ms);
            }
        }
        result
    }
    fn permits(&self, url: &url::Url) -> bool {
        if url.path() == "/robots.txt" || self.allow_all {
            return true;
        }
        if self.deny_all {
            return false;
        }
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        let mut winner: Option<(usize, bool)> = None;
        for (allow, pattern) in &self.rules {
            let anchored = pattern.ends_with('$');
            let pattern = if anchored {
                &pattern[..pattern.len() - 1]
            } else {
                pattern.as_str()
            };
            let expression = format!(
                "^{}{}",
                pattern
                    .split('*')
                    .map(regex::escape)
                    .collect::<Vec<_>>()
                    .join(".*"),
                if anchored { "$" } else { "" }
            );
            let Ok(matcher) = regex::Regex::new(&expression) else {
                return false;
            };
            if matcher.is_match(&path) {
                let length = pattern.bytes().filter(|b| *b != b'*').count();
                if winner.is_none_or(|(best, allowed)| {
                    length > best || (length == best && *allow && !allowed)
                }) {
                    winner = Some((length, *allow));
                }
            }
        }
        winner.is_none_or(|(_, allowed)| allowed)
    }
}
struct HostGate {
    last: Mutex<Option<Instant>>,
    slots: Arc<Semaphore>,
}
/// Shared gates cover document fetches, download ranges and redirect hops.
pub(crate) struct WorkflowNetwork {
    store: Arc<WorkflowStore>,
    journal: zeus_journal::Journal,
    robots: Mutex<HashMap<String, Robots>>,
    hosts: Mutex<HashMap<String, Arc<HostGate>>>,
}
impl WorkflowNetwork {
    pub fn new(store: Arc<WorkflowStore>, journal: zeus_journal::Journal) -> Self {
        Self {
            store,
            journal,
            robots: Mutex::new(HashMap::new()),
            hosts: Mutex::new(HashMap::new()),
        }
    }
    fn authorize(&self, run: zeus_types::RunId) -> Result<Plan, TransportError> {
        if self
            .journal
            .meta()
            .map_err(|_| failure())?
            .kill_switch_engaged()
        {
            return Err(failure());
        }
        let record = self
            .store
            .run(run)
            .map_err(|_| failure())?
            .ok_or_else(failure)?;
        if record.snapshot.report.status != zeus_workflow::RunStatus::Running {
            return Err(failure());
        }
        if self.store.elapsed(&record) >= record.plan.budgets.max_duration_ms {
            let _ = self.store.exhausted(run, "budget_duration");
            return Err(failure());
        }
        let a = self
            .journal
            .read(|t| {
                record
                    .approval_id
                    .map(|id| t.approval(id))
                    .transpose()
                    .map(Option::flatten)
            })
            .map_err(|_| failure())?
            .ok_or_else(failure)?;
        if a.state != zeus_types::ApprovalState::Approved
            || a.run_id != Some(run)
            || a.facts
                .as_ref()
                .and_then(|f| f.get("plan_blake3"))
                .and_then(|v| v.as_str())
                != Some(record.snapshot.report.plan_hash.as_str())
        {
            return Err(failure());
        }
        Ok(record.plan)
    }
    fn check_authority(&self, run: Option<zeus_types::RunId>) -> Result<(), TransportError> {
        if self
            .journal
            .meta()
            .map_err(|_| failure())?
            .kill_switch_engaged()
        {
            return Err(failure());
        }
        if let Some(run) = run {
            self.authorize(run)?;
        }
        Ok(())
    }
    pub(crate) async fn gate(
        &self,
        url: &url::Url,
        interval: u64,
        _concurrency: u8,
        run: Option<zeus_types::RunId>,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, TransportError> {
        let host = url.host_str().ok_or_else(failure)?.to_owned();
        let gate = {
            let mut hosts = self.hosts.lock().await;
            hosts
                .entry(host)
                .or_insert_with(|| {
                    Arc::new(HostGate {
                        last: Mutex::new(None),
                        slots: Arc::new(Semaphore::new(1)),
                    })
                })
                .clone()
        };
        let acquire = gate.slots.clone().acquire_owned();
        tokio::pin!(acquire);
        let permit = loop {
            self.check_authority(run)?;
            tokio::select! {
                result = &mut acquire => break result.map_err(|_| failure())?,
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        };
        let mut last = gate.last.lock().await;
        let delay = Duration::from_millis(interval)
            .saturating_sub(last.map(|previous| previous.elapsed()).unwrap_or_default());
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            self.check_authority(run)?;
            tokio::select! {
                _ = &mut sleep => break,
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
        self.check_authority(run)?;
        *last = Some(Instant::now());
        Ok(permit)
    }
    async fn raw(
        &self,
        request: &HttpRequest,
        allowed: &[String],
        run: Option<zeus_types::RunId>,
    ) -> Result<reqwest::Response, TransportError> {
        let url = &request.url;
        let host = url.host_str().ok_or_else(failure)?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || !zeus_workflow::validate::host_allowed(host, allowed)
        {
            return Err(failure());
        }
        let port = url.port_or_known_default().ok_or_else(failure)?;
        let peers: Vec<SocketAddr> = tokio::time::timeout(
            Duration::from_secs(15),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| failure())?
        .map_err(|_| failure())?
        .collect();
        if peers.is_empty() || peers.iter().any(|p| !public_ip(p.ip())) {
            return Err(failure());
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .pool_max_idle_per_host(0)
            .connect_timeout(request.head_timeout)
            .read_timeout(request.idle_timeout)
            .resolve_to_addrs(host, &peers)
            .build()
            .map_err(|_| failure())?;
        // DNS can be slow; pace the next request from the actual send instant.
        let host_gate = { self.hosts.lock().await.get(host).cloned() };
        if let Some(gate) = host_gate {
            *gate.last.lock().await = Some(Instant::now());
        }
        self.check_authority(run)?;
        let response = tokio::time::timeout(
            request.head_timeout,
            client
                .request(request.method.clone(), url.clone())
                .headers(request.headers.clone())
                .send(),
        )
        .await
        .map_err(|_| failure())?
        .map_err(|_| failure())?;
        if response
            .remote_addr()
            .is_none_or(|peer| !public_ip(peer.ip()) || !peers.iter().any(|p| p.ip() == peer.ip()))
        {
            return Err(failure());
        }
        Ok(response)
    }
    async fn robots(
        &self,
        url: &url::Url,
        plan: &Plan,
        run: Option<zeus_types::RunId>,
    ) -> Result<Robots, TransportError> {
        let origin = url.origin().ascii_serialization();
        if let Some(cached) = self
            .robots
            .lock()
            .await
            .get(&origin)
            .filter(|v| v.loaded.elapsed() < Duration::from_secs(86_400))
            .cloned()
        {
            return Ok(cached);
        }
        let mut location =
            url::Url::parse(&format!("{origin}/robots.txt")).map_err(|_| failure())?;
        let mut parsed = Robots {
            loaded: Instant::now(),
            allow_all: false,
            deny_all: true,
            rules: vec![],
            delay_ms: 0,
        };
        for _ in 0..=5 {
            let permit = self
                .gate(
                    &location,
                    plan.fetch.min_interval_ms,
                    plan.fetch.max_concurrency_per_host,
                    run,
                )
                .await?;
            let mut headers = http::HeaderMap::new();
            headers.insert("user-agent", http::HeaderValue::from_static("Zeus/0.1"));
            headers.insert(
                "accept-encoding",
                http::HeaderValue::from_static("identity"),
            );
            let request = HttpRequest {
                method: http::Method::GET,
                url: location.clone(),
                headers,
                head_timeout: Duration::from_secs(15),
                idle_timeout: Duration::from_secs(30),
            };
            let response = self.raw(&request, &plan.allowed_domains, run).await?;
            if response.status().is_redirection() {
                let target = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(failure)?;
                let next = location.join(target).map_err(|_| failure())?;
                if location.scheme() == "https" && next.scheme() != "https" {
                    return Err(failure());
                }
                location = next;
                drop(permit);
                continue;
            }
            if response.status().is_client_error() {
                parsed.allow_all = true;
                parsed.deny_all = false;
            } else if response.status().is_success() {
                let mut bytes = Vec::new();
                let mut body = response.bytes_stream();
                while let Some(chunk) = body.next().await {
                    self.check_authority(run)?;
                    let chunk = chunk.map_err(|_| failure())?;
                    let remaining = (512 * 1024usize).saturating_sub(bytes.len());
                    bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                    if bytes.len() == 512 * 1024 {
                        break;
                    }
                }
                parsed = Robots::parse(&String::from_utf8_lossy(&bytes));
            }
            break;
        }
        self.robots.lock().await.insert(origin, parsed.clone());
        Ok(parsed)
    }
    pub async fn send(
        self: &Arc<Self>,
        request: HttpRequest,
        plan: &Plan,
        run: Option<zeus_types::RunId>,
    ) -> Result<HttpResponse, TransportError> {
        self.check_authority(run)?;
        let robots = self.robots(&request.url, plan, run).await?;
        if !robots.permits(&request.url) {
            return Err(failure());
        }
        let permit = self
            .gate(
                &request.url,
                plan.fetch.min_interval_ms.max(robots.delay_ms),
                plan.fetch.max_concurrency_per_host,
                run,
            )
            .await?;
        if let Some(run) = run {
            self.authorize(run)?;
        }
        let response = self.raw(&request, &plan.allowed_domains, run).await?;
        let status = response.status();
        let version = response.version();
        let headers = response.headers().clone();
        let idle = request.idle_timeout;
        let network = self.clone();
        let body = futures::stream::unfold(
            (
                Box::pin(response.bytes_stream()),
                Some(permit),
                false,
                network,
            ),
            move |(mut stream, permit, ended, network)| async move {
                if ended {
                    return None;
                }
                if network.check_authority(run).is_err() {
                    return Some((Err(failure()), (stream, None, true, network)));
                }
                match tokio::time::timeout(idle, stream.next()).await {
                    Ok(Some(Ok(bytes))) => {
                        if network.check_authority(run).is_err() {
                            Some((Err(failure()), (stream, None, true, network)))
                        } else {
                            Some((Ok(bytes), (stream, permit, false, network)))
                        }
                    }
                    Ok(None) => None,
                    _ => Some((Err(failure()), (stream, None, true, network))),
                }
            },
        );
        Ok(HttpResponse {
            status,
            version,
            headers,
            url: request.url,
            body: Box::pin(body),
        })
    }
    pub async fn document(
        self: &Arc<Self>,
        plan: &Plan,
        url: &str,
        run: Option<zeus_types::RunId>,
    ) -> RuntimeResult<Document> {
        let mut url =
            url::Url::parse(url).map_err(|_| RuntimeError::Policy("workflow.url_invalid"))?;
        for _ in 0..=10 {
            let mut headers = http::HeaderMap::new();
            headers.insert("user-agent", http::HeaderValue::from_static("Zeus/0.1"));
            headers.insert(
                "accept-encoding",
                http::HeaderValue::from_static("identity"),
            );
            headers.insert(
                "accept",
                http::HeaderValue::from_static("text/html,application/json;q=0.9,*/*;q=0.1"),
            );
            let mut response = self
                .send(
                    HttpRequest {
                        method: http::Method::GET,
                        url: url.clone(),
                        headers,
                        head_timeout: Duration::from_secs(15),
                        idle_timeout: Duration::from_secs(30),
                    },
                    plan,
                    run,
                )
                .await
                .map_err(|_| RuntimeError::Port("workflow.document_unavailable"))?;
            if response.status.is_redirection() {
                let location = response
                    .headers
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or(RuntimeError::Policy("workflow.redirect_invalid"))?;
                let next = url
                    .join(location)
                    .map_err(|_| RuntimeError::Policy("workflow.redirect_invalid"))?;
                if url.scheme() == "https" && next.scheme() != "https" {
                    return Err(RuntimeError::Policy("workflow.redirect_insecure"));
                }
                url = next;
                continue;
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.body.next().await {
                let chunk =
                    chunk.map_err(|_| RuntimeError::Port("workflow.document_unavailable"))?;
                if body.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                    return Err(RuntimeError::Policy("workflow.page_too_large"));
                }
                body.extend_from_slice(&chunk);
            }
            let content_type = response
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("text/plain")
                .into();
            let headers = response
                .headers
                .get("link")
                .and_then(|v| v.to_str().ok())
                .map(|v| BTreeMap::from([("link".into(), v.into())]))
                .unwrap_or_default();
            return Ok(Document {
                final_url: url.into(),
                status: response.status.as_u16(),
                content_type,
                body: String::from_utf8_lossy(&body).into_owned(),
                headers,
            });
        }
        Err(RuntimeError::Policy("workflow.redirect_limit"))
    }
}
struct Scoped {
    network: Arc<WorkflowNetwork>,
    plan: Plan,
    run: zeus_types::RunId,
}
#[async_trait]
impl HttpTransport for Scoped {
    async fn send(&self, r: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.network.send(r, &self.plan, Some(self.run)).await
    }
}
pub(crate) struct Factory(pub Arc<WorkflowNetwork>);
impl zeus_engine::JobTransportFactory for Factory {
    fn for_job(
        &self,
        spec: &zeus_types::JobSpec,
    ) -> Result<Option<Arc<dyn HttpTransport>>, zeus_types::ErrorEnvelope> {
        if spec.source.kind != zeus_types::SourceKind::Workflow {
            return Ok(None);
        }
        let denied = || {
            zeus_types::ErrorEnvelope::from_code(
                "policy.approval_denied",
                "workflow authority unavailable",
            )
        };
        let run = spec.source.run_id.ok_or_else(denied)?;
        let plan = self.0.authorize(run).map_err(|_| denied())?;
        Ok(Some(Arc::new(Scoped {
            network: self.0.clone(),
            plan,
            run,
        })))
    }
    fn validate_commit(
        &self,
        spec: &zeus_types::JobSpec,
        txn: &dyn zeus_journal::ReadTxn,
        range: zeus_types::ByteRange,
    ) -> Result<(), zeus_types::ErrorEnvelope> {
        if spec.source.kind != zeus_types::SourceKind::Workflow {
            return Ok(());
        }
        let deny = || {
            zeus_types::ErrorEnvelope::from_code(
                "policy.approval_denied",
                "workflow authority unavailable",
            )
        };
        let run = spec.source.run_id.ok_or_else(deny)?;
        let row = self
            .0
            .store
            .run(run)
            .map_err(|_| deny())?
            .ok_or_else(deny)?;
        if row.snapshot.report.status != zeus_workflow::RunStatus::Running {
            return Err(deny());
        }
        let approval = row
            .approval_id
            .and_then(|id| txn.approval(id).ok().flatten())
            .ok_or_else(deny)?;
        if approval.state != zeus_types::ApprovalState::Approved
            || approval.run_id != Some(run)
            || approval
                .facts
                .as_ref()
                .and_then(|f| f.get("plan_blake3"))
                .and_then(|f| f.as_str())
                != Some(row.snapshot.report.plan_hash.as_str())
        {
            return Err(deny());
        }
        let bytes = txn
            .jobs(&zeus_journal::store::JobFilter::default())
            .map_err(|_| deny())?
            .iter()
            .filter(|r| r.job.spec.source.run_id == Some(run))
            .fold(0u64, |n, r| n.saturating_add(r.job.progress.bytes_done));
        let reason = if self.0.store.elapsed(&row) >= row.plan.budgets.max_duration_ms {
            Some("budget_duration")
        } else if bytes.saturating_add(range.end - range.start) > row.plan.budgets.max_bytes {
            Some("budget_bytes")
        } else {
            None
        };
        if let Some(reason) = reason {
            self.0.store.exhausted(run, reason).map_err(|_| deny())?;
            return Err(zeus_types::ErrorEnvelope::from_code(
                "workflow.budget_exhausted",
                "approved workflow budget reached",
            ));
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_special_and_mapped_peers_are_refused() {
        for text in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.64.1.2",
            "192.168.1.2",
            "198.18.1.1",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "64:ff9b::7f00:1",
        ] {
            assert!(!public_ip(text.parse().unwrap()), "{text}");
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
    }
    #[test]
    fn robots_specific_group_longest_rule_and_allow_tie() {
        let policy = Robots::parse(
            "User-agent: *\nDisallow: /\nUser-agent: Zeus\nDisallow: /private\nAllow: /private/public\nDisallow: /same\nAllow: /same\nCrawl-delay: 2\n",
        );
        assert!(policy.permits(&url::Url::parse("https://a.example/open").unwrap()));
        assert!(!policy.permits(&url::Url::parse("https://a.example/private/x").unwrap()));
        assert!(policy.permits(&url::Url::parse("https://a.example/private/public/x").unwrap()));
        assert!(policy.permits(&url::Url::parse("https://a.example/same").unwrap()));
        assert_eq!(policy.delay_ms, 2000);
    }
    #[test]
    fn ipv6_special_use_and_reserved_assignment_boundaries_are_refused() {
        for text in [
            "2001::",
            "2001:1::1",
            "2001:2::1",
            "2001:10::1",
            "2001:20::1",
            "2001:30::1",
            "2001:1ff:ffff:ffff:ffff:ffff:ffff:ffff",
            "2001:1000::",
            "2001:11ff:ffff:ffff:ffff:ffff:ffff:ffff",
            "2001:4e00::",
            "2001:4fff::",
            "2001:6000::",
            "2001:7fff::",
            "2001:c000::",
            "2001:db8::",
            "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff",
            "2002::1",
            "2003:4000::",
            "2420::",
            "2610:200::",
            "2620:200::",
            "2620:4f:8000::",
            "2620:4f:8000:ffff:ffff:ffff:ffff:ffff",
            "2640::",
            "2c10::",
            "2d00::",
            "3ffe::1",
            "3fff::",
            "3fff:fff:ffff:ffff:ffff:ffff:ffff:ffff",
            "4000::",
            "5f00::1",
            "64:ff9b::808:808",
            "64:ff9b:1::1",
            "100::1",
            "100:0:0:1::1",
        ] {
            assert!(
                !public_ip(text.parse().unwrap()),
                "special/reserved: {text}"
            );
        }
        for text in [
            "2001:db7:ffff:ffff:ffff:ffff:ffff:ffff",
            "2001:db9::",
            "2001:4860:4860::8888",
            "2606:4700:4700::1111",
            "2620:4f:7fff:ffff:ffff:ffff:ffff:ffff",
            "2620:4f:8001::",
        ] {
            assert!(
                public_ip(text.parse().unwrap()),
                "ordinary allocation: {text}"
            );
        }
    }
    #[test]
    fn ordinary_iana_allocations_include_both_prefix_boundaries() {
        for &(base, bits) in GLOBAL_IPV6_PREFIXES {
            let first = u128::from(base) << 96;
            let last = first | ((1u128 << (128 - bits)) - 1);
            for value in [first, last] {
                let ip = std::net::Ipv6Addr::from(value);
                assert!(public_ip(IpAddr::V6(ip)), "ordinary /{bits} boundary: {ip}");
            }
        }
    }
}
