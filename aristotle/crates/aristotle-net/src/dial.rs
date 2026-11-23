// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fast TCP dialing and DNS caching for `ureq`:
//!
//! - [`DnsCache`] + [`SmartResolver`]: caches resolved socket addresses per
//!   `host:port` (5 min TTL), orders IPv4 ahead of IPv6, promotes the last fast
//!   winning IP to the front, and demotes IPs that recently timed out or failed.
//! - [`HappyEyeballsTcpConnector`]: dials multiple candidate IPs in parallel
//!   with a 250 ms stagger (RFC 8305 style) so a stalled edge IP (e.g. a flaky
//!   Cloudflare anycast address) never blocks the whole connect timeout.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout, Transport,
};
use ureq::{Error, Timeout};

/// How long resolved DNS entries remain cached.
const DNS_CACHE_TTL: Duration = Duration::from_secs(300);

/// How long a winning fast IP is preferred at the front of the candidate list.
const PREFERRED_IP_TTL: Duration = Duration::from_secs(300);

/// How long a failed or timed-out IP is demoted to the back of the list.
const PENALIZED_IP_TTL: Duration = Duration::from_secs(120);

/// Maximum number of cached `(host, port)` DNS entries.
const DNS_CACHE_CAPACITY: usize = 64;

/// Delay before racing the next candidate IP when the first connect is still
/// pending (RFC 8305 Happy Eyeballs).
const HAPPY_EYEBALLS_STAGGER: Duration = Duration::from_millis(250);

/// Maximum number of candidate IPs raced concurrently per connection attempt.
const MAX_RACE_CANDIDATES: usize = 4;

/// Shared process-wide DNS cache and fast-IP tracker.
pub(crate) fn global_dns_cache() -> Arc<DnsCache> {
    static CACHE: OnceLock<Arc<DnsCache>> = OnceLock::new();
    Arc::clone(CACHE.get_or_init(|| Arc::new(DnsCache::new())))
}

#[derive(Clone, Debug)]
struct CachedHost {
    addrs: Vec<SocketAddr>,
    expires_at: Instant,
}

#[derive(Default)]
struct CacheInner {
    hosts: HashMap<(String, u16), CachedHost>,
    preferred: HashMap<(String, u16), (SocketAddr, Instant)>,
    penalized: HashMap<SocketAddr, Instant>,
}

/// Thread-safe in-memory DNS cache + per-IP fast/slow health memory.
#[derive(Default)]
pub(crate) struct DnsCache {
    inner: Mutex<CacheInner>,
}

impl fmt::Debug for DnsCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DnsCache").finish_non_exhaustive()
    }
}

impl DnsCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Return cached addresses ordered by fast-IP preference if still fresh.
    pub(crate) fn lookup(&self, host: &str, port: u16) -> Option<Vec<SocketAddr>> {
        let now = Instant::now();
        let key = (host.to_ascii_lowercase(), port);
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.hosts.get(&key)?;
        if now >= entry.expires_at {
            guard.hosts.remove(&key);
            return None;
        }
        let addrs = entry.addrs.clone();
        Some(Self::rank_locked(&mut guard, &key, addrs, now))
    }

    /// Store freshly resolved addresses and return them ordered by preference.
    pub(crate) fn store(&self, host: &str, port: u16, addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
        let now = Instant::now();
        let key = (host.to_ascii_lowercase(), port);
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if guard.hosts.len() >= DNS_CACHE_CAPACITY && !guard.hosts.contains_key(&key) {
            // Evict expired entries first; if still full, drop the earliest-expiring one.
            guard.hosts.retain(|_, v| now < v.expires_at);
            if guard.hosts.len() >= DNS_CACHE_CAPACITY {
                if let Some(oldest_key) = guard
                    .hosts
                    .iter()
                    .min_by_key(|(_, v)| v.expires_at)
                    .map(|(k, _)| k.clone())
                {
                    guard.hosts.remove(&oldest_key);
                }
            }
        }
        guard.hosts.insert(
            key.clone(),
            CachedHost {
                addrs: addrs.clone(),
                expires_at: now + DNS_CACHE_TTL,
            },
        );
        Self::rank_locked(&mut guard, &key, addrs, now)
    }

    /// Promote `addr` as the preferred fast IP for `(host, port)`.
    pub(crate) fn record_winner(&self, host: &str, port: u16, addr: SocketAddr) {
        let now = Instant::now();
        let key = (host.to_ascii_lowercase(), port);
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.penalized.remove(&addr);
        guard.preferred.insert(key, (addr, now + PREFERRED_IP_TTL));
    }

    /// Demote `addr` so other IPs are dialed ahead of it for a short window.
    pub(crate) fn record_failure(&self, addr: SocketAddr) {
        let now = Instant::now();
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.penalized.insert(addr, now + PENALIZED_IP_TTL);
        guard.preferred.retain(|_, (pref, _)| *pref != addr);
    }

    /// Drop cached DNS entries for `(host, port)` after all addresses fail so
    /// the next attempt performs a fresh lookup.
    pub(crate) fn invalidate(&self, host: &str, port: u16) {
        let key = (host.to_ascii_lowercase(), port);
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.hosts.remove(&key);
        guard.preferred.remove(&key);
    }

    /// Rank addresses: preferred non-penalized IP first, then healthy IPv4,
    /// healthy IPv6, penalized IPv4, penalized IPv6.
    fn rank_locked(
        guard: &mut CacheInner,
        key: &(String, u16),
        mut addrs: Vec<SocketAddr>,
        now: Instant,
    ) -> Vec<SocketAddr> {
        guard.penalized.retain(|_, exp| now < *exp);
        let preferred = guard.preferred.get(key).and_then(|(addr, exp)| {
            if now < *exp && !guard.penalized.contains_key(addr) {
                Some(*addr)
            } else {
                None
            }
        });
        if guard.preferred.get(key).is_some_and(|(_, exp)| now >= *exp) {
            guard.preferred.remove(key);
        }
        // Preserve original relative order within each bucket using stable sort.
        addrs.sort_by_key(|addr| {
            let is_pref = preferred == Some(*addr);
            let is_penalized = guard.penalized.contains_key(addr);
            let is_v6 = addr.is_ipv6();
            if is_pref {
                0u8
            } else if !is_penalized && !is_v6 {
                1u8
            } else if !is_penalized && is_v6 {
                2u8
            } else if !is_v6 {
                3u8
            } else {
                4u8
            }
        });
        addrs
    }
}

/// Caching + fast-IP-ranking DNS resolver for `ureq`.
#[derive(Debug)]
pub(crate) struct SmartResolver {
    cache: Arc<DnsCache>,
    fallback: DefaultResolver,
}

impl SmartResolver {
    pub(crate) fn new(cache: Arc<DnsCache>) -> Self {
        Self {
            cache,
            fallback: DefaultResolver::default(),
        }
    }
}

fn uri_host_port(uri: &Uri) -> Option<(&str, u16)> {
    let authority = uri.authority()?;
    let host = authority.host();
    let port = authority.port_u16().or_else(|| match uri.scheme_str() {
        Some("https") => Some(443),
        Some("http") => Some(80),
        _ => None,
    })?;
    Some((host, port))
}

/// Parse an IP literal (including bracketed IPv6 like `[::1]`) directly without
/// invoking system DNS or touching the cache.
fn parse_ip_literal(host: &str) -> Option<IpAddr> {
    let trimmed = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    trimmed.parse::<IpAddr>().ok()
}

impl Resolver for SmartResolver {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, Error> {
        let Some((host, port)) = uri_host_port(uri) else {
            return self.fallback.resolve(uri, config, timeout);
        };

        // Fast path for IP literals (localhost/test servers/LAN IPs): skip DNS cache.
        if let Some(ip) = parse_ip_literal(host) {
            let addr = SocketAddr::new(ip, port);
            let mut out = self.empty();
            for kept in config.ip_family().keep_wanted(std::iter::once(addr)) {
                out.push(kept);
            }
            return if out.is_empty() {
                Err(Error::HostNotFound)
            } else {
                Ok(out)
            };
        }

        if let Some(cached) = self.cache.lookup(host, port) {
            let mut out = self.empty();
            for addr in config.ip_family().keep_wanted(cached.into_iter()).take(16) {
                out.push(addr);
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }

        let resolved = self.fallback.resolve(uri, config, timeout)?;
        let raw: Vec<SocketAddr> = resolved.iter().copied().collect();
        let ranked = self.cache.store(host, port, raw);

        let mut out = self.empty();
        for addr in config.ip_family().keep_wanted(ranked.into_iter()).take(16) {
            out.push(addr);
        }
        if out.is_empty() {
            Err(Error::HostNotFound)
        } else {
            Ok(out)
        }
    }
}

/// Parallel Happy-Eyeballs TCP connector that races candidate IPs with a short
/// stagger and records the fastest winner in [`DnsCache`].
#[derive(Debug)]
pub(crate) struct HappyEyeballsTcpConnector {
    cache: Arc<DnsCache>,
    stagger: Duration,
}

impl HappyEyeballsTcpConnector {
    pub(crate) fn new(cache: Arc<DnsCache>) -> Self {
        Self {
            cache,
            stagger: HAPPY_EYEBALLS_STAGGER,
        }
    }
}

impl<In: Transport> Connector<In> for HappyEyeballsTcpConnector {
    type Out = Either<In, FastTcpTransport>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, Error> {
        if chained.is_some() {
            return Ok(chained.map(Either::A));
        }

        let host_port = uri_host_port(details.uri).map(|(h, p)| (h.to_string(), p));
        let budget = details.timeout.not_zero().map(|d| *d);
        let addrs: Vec<SocketAddr> = details.addrs.iter().copied().collect();

        let (winner_addr, stream) = match race_connect(
            &addrs,
            budget,
            self.stagger,
            details.config.no_delay(),
            details.timeout.reason,
            &self.cache,
            host_port.as_ref().map(|(h, p)| (h.as_str(), *p)),
        ) {
            Ok(pair) => pair,
            Err(e) => {
                if let Some((host, port)) = host_port.as_ref() {
                    if parse_ip_literal(host).is_none() {
                        self.cache.invalidate(host, *port);
                    }
                }
                return Err(e);
            }
        };

        if let Some((host, port)) = host_port.as_ref() {
            if parse_ip_literal(host).is_none() {
                self.cache.record_winner(host, *port, winner_addr);
            }
        }

        let buffers = LazyBuffers::new(
            details.config.input_buffer_size(),
            details.config.output_buffer_size(),
        );
        Ok(Some(Either::B(FastTcpTransport::new(stream, buffers))))
    }
}

fn connect_one(
    addr: SocketAddr,
    timeout: Option<Duration>,
    no_delay: bool,
) -> io::Result<TcpStream> {
    let stream = match timeout {
        Some(t) => TcpStream::connect_timeout(&addr, t),
        None => TcpStream::connect(addr),
    }?;
    if no_delay {
        let _ = stream.set_nodelay(true);
    }
    Ok(stream)
}

/// Race TCP connections across `addrs` with RFC 8305 style staggering.
fn race_connect(
    addrs: &[SocketAddr],
    budget: Option<Duration>,
    stagger: Duration,
    no_delay: bool,
    timeout_reason: Timeout,
    cache: &DnsCache,
    host_port: Option<(&str, u16)>,
) -> Result<(SocketAddr, TcpStream), Error> {
    race_connect_with(
        addrs,
        budget,
        stagger,
        timeout_reason,
        cache,
        host_port,
        move |addr, timeout| connect_one(addr, timeout, no_delay),
    )
}

/// Generic Happy-Eyeballs racer parameterized by dialer for deterministic tests.
fn race_connect_with<T, F>(
    addrs: &[SocketAddr],
    budget: Option<Duration>,
    stagger: Duration,
    timeout_reason: Timeout,
    cache: &DnsCache,
    host_port: Option<(&str, u16)>,
    dial: F,
) -> Result<(SocketAddr, T), Error>
where
    T: Send + 'static,
    F: Fn(SocketAddr, Option<Duration>) -> io::Result<T> + Send + Sync + 'static,
{
    if addrs.is_empty() {
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "Connection refused",
        )));
    }

    // Re-rank in case another thread updated preferred/penalized status after resolve.
    let ordered: Vec<SocketAddr> = if let Some((host, port)) = host_port {
        if parse_ip_literal(host).is_none() {
            let now = Instant::now();
            let key = (host.to_ascii_lowercase(), port);
            let mut guard = cache.inner.lock().unwrap_or_else(|e| e.into_inner());
            DnsCache::rank_locked(&mut guard, &key, addrs.to_vec(), now)
        } else {
            addrs.to_vec()
        }
    } else {
        addrs.to_vec()
    };

    // Fast path for a single address: no worker thread needed.
    if ordered.len() == 1 {
        let addr = ordered[0];
        return match dial(addr, budget) {
            Ok(stream) => Ok((addr, stream)),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                cache.record_failure(addr);
                Err(Error::Timeout(timeout_reason))
            }
            Err(e) => {
                cache.record_failure(addr);
                Err(Error::Io(e))
            }
        };
    }

    let dial = Arc::new(dial);
    let candidates: Vec<SocketAddr> = ordered.into_iter().take(MAX_RACE_CANDIDATES).collect();
    let deadline = budget.map(|b| Instant::now() + b);
    let (tx, rx) = mpsc::channel::<(SocketAddr, io::Result<T>)>();

    let mut next_idx = 0usize;
    let mut in_flight = 0usize;
    let mut last_err: Option<io::Error> = None;
    let mut any_timeout = false;

    let spawn_next = |idx: &mut usize,
                      in_flight: &mut usize,
                      tx: &mpsc::Sender<(SocketAddr, io::Result<T>)>|
     -> bool {
        if *idx >= candidates.len() {
            return false;
        }
        let addr = candidates[*idx];
        *idx += 1;
        let rem = deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
            .map(|d| d.max(Duration::from_millis(50)));
        let tx_clone = tx.clone();
        let dial_clone = Arc::clone(&dial);
        if thread::Builder::new()
            .name("http-dial".into())
            .spawn(move || {
                let res = dial_clone(addr, rem);
                let _ = tx_clone.send((addr, res));
            })
            .is_ok()
        {
            *in_flight += 1;
            true
        } else {
            false
        }
    };

    // Kick off the first candidate immediately.
    spawn_next(&mut next_idx, &mut in_flight, &tx);

    while in_flight > 0 || next_idx < candidates.len() {
        let now = Instant::now();
        let rem_total = match deadline {
            Some(d) => {
                if now >= d {
                    return Err(Error::Timeout(timeout_reason));
                }
                Some(d - now)
            }
            None => None,
        };

        let wait_for = if next_idx < candidates.len() {
            match rem_total {
                Some(rem) => Some(rem.min(stagger)),
                None => Some(stagger),
            }
        } else {
            rem_total
        };

        let msg = match wait_for {
            Some(d) => match rx.recv_timeout(d) {
                Ok(m) => Some(m),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => rx.recv().ok(),
        };

        match msg {
            Some((addr, Ok(stream))) => {
                // First socket to complete TCP handshake wins immediately!
                // Any slower in-flight threads will drop their stream when send fails.
                return Ok((addr, stream));
            }
            Some((addr, Err(e))) => {
                in_flight = in_flight.saturating_sub(1);
                cache.record_failure(addr);
                if e.kind() == io::ErrorKind::TimedOut {
                    any_timeout = true;
                } else {
                    last_err = Some(e);
                }
                // Immediately launch the next candidate if one failed quickly.
                if in_flight == 0 && next_idx < candidates.len() {
                    spawn_next(&mut next_idx, &mut in_flight, &tx);
                }
            }
            None => {
                // Either stagger timer fired or overall deadline expired.
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    // Mark all launched candidates that haven't finished as slow.
                    for &addr in &candidates[..next_idx] {
                        cache.record_failure(addr);
                    }
                    return Err(Error::Timeout(timeout_reason));
                }
                if next_idx < candidates.len() {
                    spawn_next(&mut next_idx, &mut in_flight, &tx);
                }
            }
        }
    }

    if let Some(e) = last_err {
        Err(Error::Io(e))
    } else if any_timeout {
        Err(Error::Timeout(timeout_reason))
    } else {
        Err(Error::Io(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "Connection refused",
        )))
    }
}

/// Safe `ureq::Transport` wrapping a connected `TcpStream` (mirrors `ureq`'s
/// internal `TcpTransport` so Keep-Alive pooling and `is_open()` work identically).
pub(crate) struct FastTcpTransport {
    stream: TcpStream,
    buffers: LazyBuffers,
    timeout_write: Option<Duration>,
    timeout_read: Option<Duration>,
}

impl FastTcpTransport {
    fn new(stream: TcpStream, buffers: LazyBuffers) -> Self {
        Self {
            stream,
            buffers,
            timeout_write: None,
            timeout_read: None,
        }
    }
}

impl fmt::Debug for FastTcpTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FastTcpTransport")
            .field("addr", &self.stream.peer_addr().ok())
            .finish()
    }
}

fn maybe_update_timeout(
    timeout: NextTimeout,
    previous: &mut Option<Duration>,
    stream: &TcpStream,
    setter: impl Fn(&TcpStream, Option<Duration>) -> io::Result<()>,
) -> io::Result<()> {
    let target = timeout.not_zero().map(|d| *d);
    if target != *previous {
        setter(stream, target)?;
        *previous = target;
    }
    Ok(())
}

fn is_timeout_io(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

impl Transport for FastTcpTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        maybe_update_timeout(
            timeout,
            &mut self.timeout_write,
            &self.stream,
            TcpStream::set_write_timeout,
        )?;
        let output = &self.buffers.output()[..amount];
        match self.stream.write_all(output) {
            Ok(()) => Ok(()),
            Err(e) if is_timeout_io(&e) => Err(Error::Timeout(timeout.reason)),
            Err(e) => Err(e.into()),
        }
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        maybe_update_timeout(
            timeout,
            &mut self.timeout_read,
            &self.stream,
            TcpStream::set_read_timeout,
        )?;
        let input = self.buffers.input_append_buf();
        let amount = match self.stream.read(input) {
            Ok(n) => n,
            Err(e) if is_timeout_io(&e) => return Err(Error::Timeout(timeout.reason)),
            Err(e) => return Err(e.into()),
        };
        self.buffers.input_appended(amount);
        Ok(amount > 0)
    }

    fn is_open(&mut self) -> bool {
        probe_tcp_stream(&mut self.stream).unwrap_or(false)
    }
}

fn probe_tcp_stream(stream: &mut TcpStream) -> io::Result<bool> {
    stream.set_nonblocking(true)?;
    let mut buf = [0u8; 1];
    let open = matches!(stream.read(&mut buf), Err(e) if e.kind() == io::ErrorKind::WouldBlock);
    stream.set_nonblocking(false)?;
    Ok(open)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};

    #[test]
    fn dns_cache_prefers_ipv4_and_promotes_fast_winner_and_demotes_failures() {
        let cache = DnsCache::new();
        let v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);
        let ip_a = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(104, 21, 4, 16)), 443);
        let ip_b = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(172, 67, 131, 132)), 443);

        // Initial order puts IPv4 ahead of IPv6.
        let ordered = cache.store("web.readest.com", 443, vec![v6, ip_a, ip_b]);
        assert_eq!(ordered, vec![ip_a, ip_b, v6]);

        // Recording ip_b as winner promotes it to index 0; failing ip_a demotes it.
        cache.record_failure(ip_a);
        cache.record_winner("web.readest.com", 443, ip_b);
        let ranked = cache.lookup("web.readest.com", 443).unwrap();
        assert_eq!(ranked[0], ip_b);
        assert_eq!(ranked[2], ip_a);
    }

    #[test]
    fn happy_eyeballs_races_past_refused_or_stalled_first_ip_and_promotes_winner() {
        // Bind a healthy local listener as candidate #2.
        let good = TcpListener::bind("127.0.0.1:0").unwrap();
        let good_addr = good.local_addr().unwrap();

        // Bind-and-drop a closed port as candidate #1 (immediate refusal) and
        // test that candidate #2 connects promptly and is promoted.
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let closed_addr = closed.local_addr().unwrap();
        drop(closed);

        let cache = DnsCache::new();
        cache.store(
            "race.example",
            good_addr.port(),
            vec![closed_addr, good_addr],
        );

        let t0 = Instant::now();
        let (winner, stream) = race_connect(
            &[closed_addr, good_addr],
            Some(Duration::from_secs(3)),
            Duration::from_millis(100),
            true,
            Timeout::Connect,
            &cache,
            Some(("race.example", good_addr.port())),
        )
        .expect("second candidate should win");
        let elapsed = t0.elapsed();
        drop(stream);

        assert_eq!(winner, good_addr);
        assert!(
            elapsed < Duration::from_secs(1),
            "expected fast failover, took {elapsed:?}"
        );

        cache.record_winner("race.example", good_addr.port(), winner);
        let ranked = cache.lookup("race.example", good_addr.port()).unwrap();
        assert_eq!(ranked[0], good_addr, "winner should be promoted to index 0");
        assert_eq!(ranked[1], closed_addr, "failed IP should be demoted");
    }

    #[test]
    fn happy_eyeballs_stagger_connects_second_ip_even_when_first_hangs() {
        // Simulate Cloudflare's real Kindle failure mode: IP #0 (104.21.4.16)
        // hangs for 2s while IP #1 (172.67.131.132) answers in 10ms.
        // Happy Eyeballs must launch IP #1 after the 80ms stagger and return
        // the winner well under 500ms instead of blocking for 2s+.
        let stalled_ip = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(104, 21, 4, 16)), 443);
        let fast_ip = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(172, 67, 131, 132)), 443);

        let cache = DnsCache::new();
        cache.store("web.readest.com", 443, vec![stalled_ip, fast_ip]);

        let t0 = Instant::now();
        let (winner, label) = race_connect_with(
            &[stalled_ip, fast_ip],
            Some(Duration::from_secs(5)),
            Duration::from_millis(80),
            Timeout::Connect,
            &cache,
            Some(("web.readest.com", 443)),
            move |addr, _timeout| {
                if addr == stalled_ip {
                    thread::sleep(Duration::from_secs(2));
                    Err(io::Error::new(io::ErrorKind::TimedOut, "simulated hang"))
                } else {
                    thread::sleep(Duration::from_millis(10));
                    Ok("fast-conn")
                }
            },
        )
        .expect("healthy second address should win past stalled first address");
        let elapsed = t0.elapsed();

        assert_eq!(winner, fast_ip);
        assert_eq!(label, "fast-conn");
        assert!(
            elapsed < Duration::from_millis(500),
            "Happy Eyeballs must win shortly after 80ms stagger, took {elapsed:?}"
        );

        // Recording winner promotes fast_ip ahead of stalled_ip for future dials.
        cache.record_winner("web.readest.com", 443, winner);
        let ranked = cache.lookup("web.readest.com", 443).unwrap();
        assert_eq!(ranked[0], fast_ip);
    }
}
