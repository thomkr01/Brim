//! Phishing and NSFW domain blocklists (downloaded, refreshed on a timer) and shortener unrolling.
use reqwest::{
    dns::{Addrs, Name, Resolve, Resolving},
    redirect::Policy,
};
use std::{
    borrow::Cow,
    net::{IpAddr, SocketAddr},
    sync::RwLock,
    time::Duration,
};

const PHISHING_URL: &str =
    "https://raw.githubusercontent.com/blocklistproject/Lists/master/alt-version/phishing-nl.txt";
const NSFW_URL: &str =
    "https://raw.githubusercontent.com/blocklistproject/Lists/master/alt-version/porn-nl.txt";
pub const REFRESH: Duration = Duration::from_secs(12 * 3600);
/// Largest domain list download (the NSFW list is about 18 MB).
const LIST_MAX_BYTES: usize = 64 << 20;

/// Sites that normal chat links to all day. An entry on one of them, or under one, would hit
/// normal posts in every server that has the filter on, so it is refused.
const NEVER_LISTED: &[&str] = &[
    "discord.com",
    "discord.gg",
    "discordapp.com",
    "discordapp.net",
    "discord.media",
    "discord.gift",
    "discord.new",
    "tenor.com",
    "giphy.com",
    "youtube.com",
    "youtu.be",
    "google.com",
    "github.com",
    "twitch.tv",
    "twitter.com",
    "x.com",
    "reddit.com",
    "imgur.com",
    "spotify.com",
    "wikipedia.org",
];

/// Hosting suffixes where every customer has a subdomain: only the bare entry is refused, a
/// listed subdomain is one site.
const SHARED_SUFFIXES: &[&str] = &[
    "github.io",
    "pages.dev",
    "workers.dev",
    "web.app",
    "firebaseapp.com",
    "vercel.app",
    "netlify.app",
    "herokuapp.com",
    "blogspot.com",
];

/// An entry that would hit far more than one site: see [`NEVER_LISTED`], [`SHARED_SUFFIXES`],
/// and two-label country suffixes such as `co.uk` or `com.au`.
fn too_broad(d: &str) -> bool {
    let cc_suffix = d.split_once('.').is_some_and(|(a, b)| {
        b.len() == 2
            && !b.contains('.')
            && [
                "co", "com", "net", "org", "gov", "edu", "ac", "or", "ne", "go", "gob", "mil",
            ]
            .contains(&a)
    });
    cc_suffix
        || SHARED_SUFFIXES.contains(&d)
        || NEVER_LISTED
            .iter()
            .any(|n| d == *n || d.strip_suffix(n).is_some_and(|p| p.ends_with('.')))
}

/// Bare domains in one compact `fst` set (about 11 MB for 1.1 million names, where a `HashSet<String>`
/// takes about 100 MB and leaves freed fragments behind on every refresh). A host hits when it or any
/// parent domain is listed.
#[derive(Default)]
pub struct SiteList(fst::Set<Vec<u8>>);

impl SiteList {
    /// One domain per line; `#` comments and hosts-file lines (`0.0.0.0 domain`) are tolerated.
    /// Entries that are [`too_broad`] are refused and logged.
    pub fn parse(text: &str) -> Self {
        let (mut refused, mut list): (Vec<Cow<str>>, Vec<Cow<str>>) = text
            .lines()
            .filter_map(|l| l.split('#').next()?.split_whitespace().last())
            .map(|d| d.trim_end_matches('.'))
            .map(|d| {
                let lower = d.to_lowercase();
                if lower == d {
                    Cow::Borrowed(d)
                } else {
                    Cow::Owned(lower)
                }
            })
            .filter(|d| d.contains('.'))
            .partition(|d| too_broad(d));
        // The fst builder takes sorted, unique keys.
        list.sort_unstable();
        list.dedup();
        refused.sort_unstable();
        refused.dedup();
        if !refused.is_empty() {
            tracing::warn!(
                count = refused.len(),
                entries = ?refused.iter().take(20).collect::<Vec<_>>(),
                "site list entries refused: each would match far more than one site"
            );
        }
        let set = fst::Set::from_iter(list.iter().map(|d| d.as_bytes())).unwrap_or_default();
        Self(set)
    }

    pub fn hits(&self, hosts: &[String]) -> bool {
        hosts.iter().any(|h| {
            let mut d = h.as_str();
            loop {
                if self.0.contains(d.as_bytes()) {
                    return true;
                }
                match d.split_once('.') {
                    Some((_, rest)) if rest.contains('.') => d = rest,
                    _ => return false,
                }
            }
        })
    }
}

/// Not loopback, private, link-local, site-local, CGNAT, benchmarking, reserved, documentation,
/// NAT64, 6to4, Teredo, multicast or unspecified (v4-mapped v6 judged as v4).
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || (o[0] == 100 && o[1] & 0xc0 == 64)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 198 && o[1] & 0xfe == 18)
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public(v4.into()),
            None => {
                let seg = v6.segments();
                let first = seg[0];
                // `::/96` covers ::, ::1 and the old IPv4-compatible `::a.b.c.d`.
                !(seg[..6] == [0; 6]
                    || v6.is_multicast()
                    || first & 0xfe00 == 0xfc00
                    || first & 0xffc0 == 0xfe80
                    || first & 0xffc0 == 0xfec0
                    || seg[..3] == [0x64, 0xff9b, 1]
                    || (first == 0x2001 && seg[1] == 0)
                    || v6.segments()[..6] == [0x64, 0xff9b, 0, 0, 0, 0]
                    || first == 0x2002
                    || (first == 0x2001 && v6.segments()[1] == 0xdb8))
            }
        },
    }
}

/// Resolves names but only to public addresses, so no redirect hop can reach the internal network.
struct PublicOnly;

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((name.as_str(), 0))
                .await?
                .filter(|a| is_public(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err("no public address".into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// IP literals skip the resolver, so redirects to them are checked here.
fn follow(attempt: reqwest::redirect::Attempt) -> reqwest::redirect::Action {
    let literal = attempt
        .url()
        .host_str()
        .map(|h| h.trim_matches(['[', ']']))
        .and_then(|h| h.parse::<IpAddr>().ok());
    if attempt.previous().len() >= 5 || literal.is_some_and(|ip| !is_public(ip)) {
        attempt.stop()
    } else {
        attempt.follow()
    }
}

/// A refreshed list with under half the entries of the one it replaces is refused, unless the next
/// refresh brings about the same non-empty count again (within 10%): then the source really shrank.
/// Per list name.
#[derive(Default)]
pub(crate) struct ShrinkGuard(std::sync::Mutex<std::collections::HashMap<&'static str, usize>>);

impl ShrinkGuard {
    /// Whether a list of `new` entries may replace one of `old`.
    pub(crate) fn accept(&self, var: &'static str, old: usize, new: usize) -> bool {
        let mut seen = self.0.lock().unwrap();
        if new * 2 >= old {
            seen.remove(var);
            return true;
        }
        let close = |prev: usize| new.abs_diff(prev) * 10 <= prev;
        if new > 0 && seen.get(var).copied().is_some_and(close) {
            seen.remove(var);
            tracing::warn!(
                var,
                old,
                new,
                "list shrank by over half twice in a row, taking it"
            );
            return true;
        }
        seen.insert(var, new);
        tracing::warn!(
            var,
            old,
            new,
            "list shrank by over half, keeping the old one"
        );
        false
    }
}

/// Wait before the next list refresh. While a list that is on has nothing loaded (its filter is off),
/// retry after 1, 5 and 15 minutes before falling back to [`REFRESH`]; `failures` counts the retries.
pub fn next_refresh(loaded: bool, failures: &mut usize) -> Duration {
    const RETRY_SECS: [u64; 3] = [60, 300, 900];
    if loaded {
        *failures = 0;
        return REFRESH;
    }
    let wait = RETRY_SECS
        .get(*failures)
        .map_or(REFRESH, |s| Duration::from_secs(*s));
    *failures += 1;
    wait
}

/// The body, refused as soon as it passes `max` bytes (a missing or wrong Content-Length included).
pub async fn read_capped(mut r: reqwest::Response, max: usize) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        r.content_length().is_none_or(|l| l <= max as u64),
        "body over {max} bytes"
    );
    let mut buf = Vec::new();
    while let Some(c) = r.chunk().await? {
        anyhow::ensure!(buf.len() + c.len() <= max, "body over {max} bytes");
        buf.extend_from_slice(&c);
    }
    Ok(buf)
}

pub struct Sites {
    phishing: RwLock<SiteList>,
    nsfw: RwLock<SiteList>,
    http: reqwest::Client,
    /// Shortener lookups: no redirect followed. None when it did not build: no unrolling then,
    /// never a client that follows redirects.
    short: Option<reqwest::Client>,
    shrink: ShrinkGuard,
}

/// Client for list downloads and fetches: public addresses only, at most 5 redirects.
pub(crate) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .dns_resolver(std::sync::Arc::new(PublicOnly))
        .redirect(Policy::custom(follow))
        .build()
        .unwrap_or_default()
}

/// `b` with the public-only resolver: no name it looks up can reach the internal network.
pub(crate) fn public_client(b: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    b.dns_resolver(std::sync::Arc::new(PublicOnly))
}

/// Client for shortener lookups: public addresses only, no redirect followed.
fn short_client(b: reqwest::ClientBuilder) -> Option<reqwest::Client> {
    b.timeout(Duration::from_secs(4))
        .dns_resolver(std::sync::Arc::new(PublicOnly))
        .redirect(Policy::none())
        .build()
        .map_err(|e| {
            tracing::warn!(
                ?e,
                "shortener client did not build, short links are not unrolled"
            )
        })
        .ok()
}

/// Most shortener hops one link may take (a shortener pointing at another shortener).
const MAX_HOPS: usize = 3;

fn host_of(u: &reqwest::Url) -> Option<String> {
    let host = u.host_str()?.to_lowercase();
    Some(
        host.trim_start_matches("www.")
            .trim_end_matches('.')
            .to_string(),
    )
}

impl Default for Sites {
    fn default() -> Self {
        Self {
            phishing: Default::default(),
            nsfw: Default::default(),
            http: client(),
            short: short_client(reqwest::Client::builder()),
            shrink: Default::default(),
        }
    }
}

impl Sites {
    pub fn phishing(&self, hosts: &[String]) -> bool {
        self.phishing.read().unwrap().hits(hosts)
    }

    pub fn nsfw(&self, hosts: &[String]) -> bool {
        self.nsfw.read().unwrap().hits(hosts)
    }

    /// Re-downloads both lists. `PHISHING_LIST_URL` / `NSFW_LIST_URL` override the sources, empty disables one.
    /// A failed download keeps the old list. True when every list that is on holds entries afterwards.
    pub async fn refresh(&self) -> bool {
        let mut all = true;
        for (var, default, slot) in [
            ("PHISHING_LIST_URL", PHISHING_URL, &self.phishing),
            ("NSFW_LIST_URL", NSFW_URL, &self.nsfw),
        ] {
            let url = std::env::var(var).unwrap_or_else(|_| default.into());
            if url.is_empty() {
                continue;
            }
            match self.download(&url).await {
                // An empty or garbled page would switch the filter off until the next refresh.
                Ok(list) => {
                    let old = slot.read().unwrap().0.len();
                    if self.shrink.accept(var, old, list.0.len()) {
                        *slot.write().unwrap() = list;
                    }
                }
                // With no list yet the filter is off, so that is an error, not a stale list.
                Err(e) if slot.read().unwrap().0.is_empty() => {
                    tracing::error!(?e, var, "site list not loaded, its filter is off")
                }
                Err(e) => tracing::warn!(?e, var, "refreshing a site list failed"),
            }
            all &= !slot.read().unwrap().0.is_empty();
        }
        crate::mem::release_freed_memory();
        all
    }

    /// Downloads an attachment for the QR check; `None` past `max` bytes or on any error.
    pub async fn fetch(&self, url: &str, max: usize) -> Option<Vec<u8>> {
        let r = self
            .http
            .get(url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?;
        read_capped(r, max).await.ok()
    }

    async fn download(&self, url: &str) -> anyhow::Result<SiteList> {
        let r = self.http.get(url).send().await?.error_for_status()?;
        let body = read_capped(r, LIST_MAX_BYTES).await?;
        // Sorting and building the set takes seconds: not on the async worker.
        Ok(
            tokio::task::spawn_blocking(move || SiteList::parse(&String::from_utf8_lossy(&body)))
                .await?,
        )
    }

    /// Host a shortener link points at: a HEAD to the shortener itself and its `Location` read.
    /// Only while that points at another known shortener is it asked too (up to [`MAX_HOPS`]); the
    /// final host is never contacted, so its server never sees the bot (its address, or a different
    /// answer to the bot than to members).
    pub async fn unshorten(&self, url: &str) -> Option<String> {
        let mut u = reqwest::Url::parse(url).ok()?;
        for _ in 0..MAX_HOPS {
            if !crate::filters::has_shortener(&[host_of(&u)?]) {
                break;
            }
            u = self.location(&u).await?;
        }
        host_of(&u)
    }

    async fn location(&self, u: &reqwest::Url) -> Option<reqwest::Url> {
        let res = self.short.as_ref()?.head(u.clone()).send().await.ok()?;
        let to = res
            .headers()
            .get(reqwest::header::LOCATION)?
            .to_str()
            .ok()?;
        u.join(to).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The redirect target is read from `Location`, never fetched; a relative `Location` stays on
    /// the shortener; a non-shortener URL is not fetched at all.
    #[tokio::test]
    async fn location_is_read_without_following() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = hits.clone();
        let next = format!("{base}/second");
        std::thread::spawn(move || {
            for loc in [next.as_str(), "/relative"] {
                let (mut c, _) = l.accept().unwrap();
                h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = c.read(&mut [0u8; 4096]);
                let _ = c.write_all(
                    format!("HTTP/1.1 301 X\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                );
            }
        });
        let s = Sites {
            short: short_client(reqwest::Client::builder().no_proxy()),
            ..Sites::default()
        };
        let url = reqwest::Url::parse(&format!("{base}/x")).unwrap();
        let path = |u: Option<reqwest::Url>| u.map(|u| u.path().to_string());
        assert_eq!(path(s.location(&url).await).as_deref(), Some("/second"));
        assert_eq!(path(s.location(&url).await).as_deref(), Some("/relative"));
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "no hop followed"
        );
        // Not a shortener: no request, its own host.
        assert_eq!(
            s.unshorten(&format!("{base}/x")).await.as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
        // No client: nothing is unrolled.
        let none = Sites {
            short: None,
            ..Sites::default()
        };
        assert_eq!(none.unshorten("https://bit.ly/x").await, None);
    }

    #[test]
    fn internal_addresses_are_refused() {
        let ip = |s: &str| is_public(s.parse().unwrap());
        for bad in [
            "127.0.0.1",
            "10.0.0.5",
            "172.17.0.2",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "192.0.0.8",
            "198.18.0.1",
            "240.0.0.1",
            "64:ff9b::7f00:1",
            "2002:c0a8:101::1",
            "2001:db8::1",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
            "::7f00:1",
            "fec0::1",
            "64:ff9b:1::1",
            "2001::1",
        ] {
            assert!(!ip(bad), "{bad}");
        }
        assert!(ip("8.8.8.8") && ip("2606:4700::1111") && ip("::ffff:8.8.8.8"));
    }

    /// One-request-per-connection HTTP server answering each GET with the next body; `chunked`
    /// sends it without Content-Length.
    fn serve(bodies: Vec<(Vec<u8>, bool)>) -> String {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/list.txt", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for (body, chunked) in bodies {
                let (mut c, _) = l.accept().unwrap();
                let mut req = [0u8; 4096];
                let _ = c.read(&mut req);
                let head = if chunked {
                    "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
                        .to_string()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                };
                let _ = c.write_all(head.as_bytes());
                if chunked {
                    for part in body.chunks(1 << 20) {
                        let _ = c.write_all(format!("{:x}\r\n", part.len()).as_bytes());
                        let _ = c.write_all(part);
                        let _ = c.write_all(b"\r\n");
                    }
                    let _ = c.write_all(b"0\r\n\r\n");
                } else {
                    let _ = c.write_all(&body);
                }
            }
        });
        url
    }

    /// Through `refresh`: a list that comes back empty (or lost over half) or over 64 MB keeps the
    /// old list, so the phishing filter stays on; a list of about the same size replaces it.
    #[tokio::test]
    async fn refresh_keeps_the_old_list_when_the_new_one_is_empty_or_huge() {
        let full: String = (0..1000).map(|i| format!("evil{i}.com\n")).collect();
        let same: String = (0..900).map(|i| format!("bad{i}.com\n")).collect();
        let huge: Vec<u8> = (0..1_000_000)
            .flat_map(|i| format!("x{i}.example.org\n").into_bytes())
            .chain(std::iter::repeat_n(b'#', 65 << 20))
            .collect();
        let url = serve(vec![
            (full.into_bytes(), false),
            (Vec::new(), false),
            (huge, true),
            (same.into_bytes(), false),
        ]);
        // SAFETY of the env writes: no other test in this crate reads these variables.
        std::env::set_var("PHISHING_LIST_URL", &url);
        std::env::set_var("NSFW_LIST_URL", "");
        let s = Sites {
            phishing: Default::default(),
            nsfw: Default::default(),
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            short: None,
            shrink: Default::default(),
        };
        let hit = |h: &str| s.phishing(&[h.to_string()]);
        s.refresh().await;
        assert!(hit("evil7.com"), "first list loaded");
        s.refresh().await;
        assert!(
            hit("evil7.com"),
            "an empty list must not replace a full one"
        );
        s.refresh().await;
        assert!(
            hit("evil7.com") && !hit("x7.example.org"),
            "a list over 64 MB is refused"
        );
        s.refresh().await;
        assert!(
            hit("bad7.com") && !hit("evil7.com"),
            "a normal refresh still replaces"
        );
    }

    #[test]
    fn a_list_that_lost_over_half_is_taken_only_when_it_stays_that_size() {
        let g = ShrinkGuard::default();
        assert!(g.accept("A", 1000, 500) && g.accept("A", 0, 0) && g.accept("A", 10, 50));
        assert!(!g.accept("A", 1000, 499), "first shrunk refresh is refused");
        assert!(
            !g.accept("A", 1000, 300),
            "a size more than 10% off starts over"
        );
        assert!(
            g.accept("A", 1000, 320),
            "within 10% of the last one is taken"
        );
        assert!(
            !g.accept("A", 1000, 300) && g.accept("A", 1000, 300),
            "the same size too"
        );
        assert!(
            !g.accept("A", 1000, 0) && !g.accept("A", 1000, 0),
            "never an empty list"
        );
        assert!(!g.accept("B", 1000, 300), "per list");
    }

    #[test]
    fn a_list_that_is_not_loaded_is_retried_soon() {
        let mut n = 0;
        let waits: Vec<u64> = (0..5)
            .map(|_| next_refresh(false, &mut n).as_secs())
            .collect();
        assert_eq!(waits, [60, 300, 900, REFRESH.as_secs(), REFRESH.as_secs()]);
        assert_eq!(next_refresh(true, &mut n), REFRESH);
        assert_eq!(
            next_refresh(false, &mut n).as_secs(),
            60,
            "starts over after a load"
        );
    }

    /// The set is built from sorted unique keys: unsorted, repeated and mixed-case input still works.
    #[test]
    fn parse_sorts_and_dedups_before_building() {
        let l = SiteList::parse("b.com\nA.com\na.com\nB.COM.\n0.0.0.0 c.org\n");
        let h = |s: &str| vec![s.to_string()];
        assert_eq!(l.0.len(), 3);
        assert!(l.hits(&h("a.com")) && l.hits(&h("x.b.com")) && l.hits(&h("c.org")));
    }

    #[test]
    fn lists_match_parent_domains() {
        let l = SiteList::parse("# c\nevil.com\n0.0.0.0 bad.example.org # x\nlocalhost\n");
        let h = |s: &str| vec![s.to_string()];
        assert!(l.hits(&h("evil.com")) && l.hits(&h("a.b.evil.com")));
        assert!(l.hits(&h("bad.example.org")) && !l.hits(&h("example.org")));
        assert!(!l.hits(&h("notevil.com")) && !l.hits(&h("com")));
    }

    #[test]
    fn too_broad_entries_are_refused() {
        let l = SiteList::parse(
            "discord.com\ncdn.discordapp.com\nco.uk\ncom.au\ngithub.io\nevil.github.io\n\
             evil.co.uk\ndiscord.com.evil.example\nnot-discord.com\nevil.example.\n",
        );
        let h = |s: &str| vec![s.to_string()];
        for ok in [
            "discord.com",
            "cdn.discordapp.com",
            "bbc.co.uk",
            "x.github.io",
        ] {
            assert!(!l.hits(&h(ok)), "{ok}");
        }
        for bad in [
            "evil.github.io",
            "evil.co.uk",
            "discord.com.evil.example",
            "not-discord.com",
            "evil.example",
        ] {
            assert!(l.hits(&h(bad)), "{bad}");
        }
    }
}
