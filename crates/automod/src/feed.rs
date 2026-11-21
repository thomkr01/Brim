//! RSS/Atom and YouTube feeds (feeds plan F1): the address check, the guarded fetch and the parse.
//! Feed addresses come from server staff, so every fetch is treated as a request into the unknown:
//! https on port 443 only, public addresses only (also after DNS and after each redirect, see
//! [`crate::sites::is_public`]), at most 3 redirects, 10 s, 2 MB, no cookies, no proxy. The XML parser
//! expands no DTD or custom entity (only the five XML ones).
use reqwest::{header, redirect, StatusCode, Url};
use std::{net::IpAddr, time::Duration};

/// Largest feed body.
const MAX_BYTES: usize = 2 << 20;
/// Largest YouTube channel page (read once, when a feed is added by its @handle).
const PAGE_BYTES: usize = 4 << 20;
const MAX_REDIRECTS: usize = 3;
/// Longest text put in `{summary}`.
const SUMMARY_CHARS: usize = 300;

/// Most items read from one feed answer.
const MAX_ITEMS: usize = 100;

/// One item of a feed, newest first in [`Feed::items`].
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub link: String,
    pub author: String,
    pub summary: String,
    pub image: String,
}

#[derive(Debug, Default)]
pub struct Feed {
    pub title: String,
    pub picture: Option<String>,
    pub items: Vec<Item>,
    /// For the next conditional fetch.
    pub etag: Option<String>,
    pub modified: Option<String>,
}

/// What a fetch found: the feed, or nothing new since the given `etag`/`modified`.
#[derive(Debug)]
pub enum Got {
    Same,
    Feed(Feed),
}

/// Why a feed address is refused, in words for the dashboard, or the parsed address.
pub fn check_url(u: &str) -> Result<Url, String> {
    let u = u.trim();
    if u.len() > 2000 {
        return Err("the feed address is longer than 2000 characters".into());
    }
    let url = Url::parse(u).map_err(|_| "that is not a web address".to_string())?;
    safe(&url)?;
    Ok(url)
}

/// The rules each address and each redirect hop must pass.
fn safe(url: &Url) -> Result<(), String> {
    if url.scheme() != "https" {
        return Err("the feed address must start with https://".into());
    }
    if url.port_or_known_default() != Some(443) {
        return Err("the feed address cannot name a port".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("the feed address cannot hold a user name or password".into());
    }
    let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
    // IP literals skip the public-only resolver: a feed has a site name.
    if host.parse::<IpAddr>().is_ok() || !host.contains('.') {
        return Err("the feed address must use a site name, not an IP address".into());
    }
    Ok(())
}

fn follow(a: redirect::Attempt) -> redirect::Action {
    if a.previous().len() >= MAX_REDIRECTS || safe(a.url()).is_err() {
        a.stop()
    } else {
        a.follow()
    }
}

/// The client every feed fetch uses (one per process, it keeps its connections).
pub fn client() -> reqwest::Client {
    crate::sites::public_client(reqwest::Client::builder())
        .timeout(Duration::from_secs(10))
        .redirect(redirect::Policy::custom(follow))
        // The proxy would resolve the name itself, past the public-only check.
        .no_proxy()
        .user_agent("Brim feeds (+https://github.com/thomkr01/Brim)")
        .build()
        .unwrap_or_default()
}

/// What went wrong, in words for the dashboard and the feed's row. Never the inner detail of a
/// refused or private address.
fn why(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "the site did not answer within 10 seconds".into()
    } else if e.is_redirect() {
        "the site redirected too often, or to an address that is not allowed".into()
    } else {
        "the site could not be reached".into()
    }
}

/// GETs `url` (already passed [`check_url`]) and reads at most `max` bytes. `Ok(None)`: 304.
async fn get(
    c: &reqwest::Client,
    url: &str,
    max: usize,
    etag: Option<&str>,
    modified: Option<&str>,
) -> Result<Option<(Vec<u8>, Option<String>, Option<String>, Url)>, String> {
    let mut rq = c.get(url);
    if let Some(e) = etag {
        rq = rq.header(header::IF_NONE_MATCH, e);
    }
    if let Some(m) = modified {
        rq = rq.header(header::IF_MODIFIED_SINCE, m);
    }
    let r = rq.send().await.map_err(|e| why(&e))?;
    if r.status() == StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    if r.status().is_redirection() {
        return Err("the site redirected too often, or to an address that is not allowed".into());
    }
    if !r.status().is_success() {
        return Err(format!("the site answered {}", r.status().as_u16()));
    }
    let head = |h| {
        r.headers()
            .get(h)
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.len() < 200)
            .map(String::from)
    };
    let (etag, modified) = (head(header::ETAG), head(header::LAST_MODIFIED));
    let at = r.url().clone();
    let body = crate::sites::read_capped(r, max)
        .await
        .map_err(|_| format!("the feed is larger than {} MB", max >> 20))?;
    Ok(Some((body, etag, modified, at)))
}

/// Fetches and parses the feed at `url`.
pub async fn fetch(
    c: &reqwest::Client,
    url: &str,
    etag: Option<&str>,
    modified: Option<&str>,
) -> Result<Got, String> {
    let Some((body, etag, modified, at)) = get(c, url, MAX_BYTES, etag, modified).await? else {
        return Ok(Got::Same);
    };
    let mut f = parse(&body, at.as_str())?;
    (f.etag, f.modified) = (etag, modified);
    Ok(Got::Feed(f))
}

/// The plain text of feed HTML: tags dropped, the five XML entities and `&nbsp;` read, spaces
/// joined, cut to `max` characters.
fn plain(html: &str, max: usize) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in html.chars() {
        match c {
            '<' => tag = true,
            '>' if tag => {
                tag = false;
                out.push(' ');
            }
            _ if !tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut cut: String = out.chars().take(max).collect();
    if cut.chars().count() < out.chars().count() {
        cut.push('…');
    }
    cut
}

/// An https link, else nothing (a picture Discord must fetch).
fn https(u: &str) -> Option<String> {
    let u = u.trim();
    (u.starts_with("https://") && u.len() <= 2000 && !u.contains(char::is_whitespace))
        .then(|| u.to_string())
}

/// A link members open: http or https only.
fn web_link(u: &str) -> Option<String> {
    let u = u.trim();
    let ok = (u.starts_with("https://") || u.starts_with("http://"))
        && u.len() <= 2000
        && !u.contains(char::is_whitespace);
    ok.then(|| u.to_string())
}

/// Parses an RSS, Atom or JSON feed read from `base`. Items with no link are left out: the link
/// is what a feed post is for.
pub fn parse(body: &[u8], base: &str) -> Result<Feed, String> {
    let parser = feed_rs::parser::Builder::new()
        .base_uri(Some(base))
        // An item without an id is known by its link (the default would make a random one, and
        // post the item again at every check).
        .id_generator(|links, title, _| {
            links.first().map_or_else(
                || {
                    title
                        .as_ref()
                        .map(|t| t.content.clone())
                        .unwrap_or_default()
                },
                |l| l.href.clone(),
            )
        })
        .build();
    let f = parser
        .parse(body)
        .map_err(|_| "this is not an RSS or Atom feed".to_string())?;
    let mut entries = f.entries;
    // Newest first, items without a date after them; a feed without dates keeps its own order
    // (most list the newest first). The sort is stable.
    entries.sort_by_key(|e| std::cmp::Reverse(e.published.or(e.updated)));
    // Only the newest 100: a 2 MB feed can list tens of thousands, and each one is remembered.
    entries.truncate(MAX_ITEMS);
    let items = entries
        .into_iter()
        .filter_map(|e| {
            let link = e
                .links
                .iter()
                .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
                .or(e.links.first())
                .and_then(|l| web_link(&l.href))?;
            let summary = e
                .summary
                .as_ref()
                .map(|s| s.content.clone())
                .or_else(|| {
                    e.media
                        .iter()
                        .find_map(|m| m.description.as_ref())
                        .map(|d| d.content.clone())
                })
                .unwrap_or_default();
            let image = e
                .media
                .iter()
                .flat_map(|m| &m.thumbnails)
                .find_map(|t| https(&t.image.uri))
                .or_else(|| {
                    e.media
                        .iter()
                        .flat_map(|m| &m.content)
                        .filter(|c| c.content_type.as_ref().is_some_and(|t| t.ty() == "image"))
                        .find_map(|c| c.url.as_ref().and_then(|u| https(u.as_str())))
                })
                .unwrap_or_default();
            let id: String = e.id.chars().take(500).collect();
            (!id.is_empty()).then(|| Item {
                id,
                title: e.title.map(|t| plain(&t.content, 256)).unwrap_or_default(),
                link,
                author: e
                    .authors
                    .first()
                    .and_then(|a| a.name.clone())
                    .map(|n| plain(&n, 100))
                    .unwrap_or_default(),
                summary: plain(&summary, SUMMARY_CHARS),
                image,
            })
        })
        .collect();
    Ok(Feed {
        title: f.title.map(|t| plain(&t.content, 80)).unwrap_or_default(),
        picture: f.icon.or(f.logo).and_then(|i| https(&i.uri)),
        items,
        etag: None,
        modified: None,
    })
}

/// The feed address of a YouTube channel.
pub fn youtube_feed(channel: &str) -> String {
    format!("https://www.youtube.com/feeds/videos.xml?channel_id={channel}")
}

/// A YouTube channel id: `UC` and 22 letters, digits, `-` or `_`.
pub fn is_channel_id(s: &str) -> bool {
    s.len() == 24
        && s.starts_with("UC")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// What the owner typed for a YouTube channel: its id (`UC...`, or a `/channel/UC...` link), or
/// the channel page to read the id from (an `@handle` or a youtube.com link with one).
#[derive(Debug, PartialEq)]
pub enum YouTube {
    Id(String),
    Page(String),
}

pub fn youtube_input(s: &str) -> Result<YouTube, String> {
    let s = s.trim().trim_end_matches('/');
    let bad = || {
        "type the channel's @handle, its channel id (UC...) or a youtube.com link to the channel"
            .to_string()
    };
    if is_channel_id(s) {
        return Ok(YouTube::Id(s.into()));
    }
    let handle_ok = |h: &str| {
        (3..=30).contains(&h.len())
            && h.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    };
    if let Some(h) = s.strip_prefix('@') {
        return handle_ok(h)
            .then(|| YouTube::Page(format!("https://www.youtube.com/@{h}")))
            .ok_or_else(bad);
    }
    let with = if s.contains("://") {
        s.to_string()
    } else {
        format!("https://{s}")
    };
    let url = Url::parse(&with).map_err(|_| bad())?;
    if !matches!(
        url.host_str(),
        Some("youtube.com" | "www.youtube.com" | "m.youtube.com")
    ) {
        return Err(bad());
    }
    let mut parts = url.path_segments().into_iter().flatten();
    match (parts.next(), parts.next()) {
        (Some("channel"), Some(id)) if is_channel_id(id) => Ok(YouTube::Id(id.into())),
        (Some(h), None) if h.starts_with('@') && handle_ok(&h[1..]) => {
            Ok(YouTube::Page(format!("https://www.youtube.com/{h}")))
        }
        _ => Err(bad()),
    }
}

/// The value of `<meta property="{prop}" content="...">` or `<link rel="canonical" href="...">`.
fn tag_value(page: &str, open: &str, attr: &str) -> Option<String> {
    let at = page.find(open)? + open.len();
    let rest = &page[at..page.len().min(at + 600)];
    let v = rest
        .split(&format!("{attr}=\""))
        .nth(1)?
        .split('"')
        .next()?;
    Some(v.replace("&amp;", "&"))
}

/// Reads a channel page: (channel id, channel name, picture). YouTube's own pages, fixed host.
pub async fn youtube_page(
    c: &reqwest::Client,
    page: &str,
) -> Result<(String, String, Option<String>), String> {
    let not_found = || "YouTube has no channel with that @handle".to_string();
    let (body, ..) = get(c, page, PAGE_BYTES, None, None)
        .await
        .map_err(|e| if e.contains("404") { not_found() } else { e })?
        .ok_or_else(not_found)?;
    let html = String::from_utf8_lossy(&body);
    let id = tag_value(&html, "<link rel=\"canonical\"", "href")
        .and_then(|u| {
            u.strip_prefix("https://www.youtube.com/channel/")
                .map(String::from)
        })
        .filter(|id| is_channel_id(id))
        .ok_or_else(|| {
            "YouTube did not show that channel: type its channel id (UC...) instead".to_string()
        })?;
    let name = tag_value(&html, "<meta property=\"og:title\"", "content")
        .map(|n| plain(&n, 80))
        .unwrap_or_default();
    let picture =
        tag_value(&html, "<meta property=\"og:image\"", "content").and_then(|p| https(&p));
    Ok((id, name, picture))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_addresses_must_be_public_https_names() {
        assert!(check_url("https://blog.example.com/feed.xml").is_ok());
        for bad in [
            "http://blog.example.com/feed",
            "https://blog.example.com:8443/feed",
            "https://user:pw@blog.example.com/feed",
            "https://127.0.0.1/feed",
            "https://[::1]/feed",
            "https://169.254.169.254/latest",
            "https://localhost/feed",
            "file:///etc/passwd",
            "gopher://x.example/",
            "not a url",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
        assert!(check_url(&format!("https://a.example/{}", "x".repeat(2000))).is_err());
    }

    /// A feed with a DTD that defines entities: nothing is expanded, nothing outside is read.
    #[test]
    fn no_dtd_or_custom_entity_is_expanded() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE rss [
  <!ENTITY a "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa">
  <!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">
  <!ENTITY xxe SYSTEM "file:///etc/passwd">
]>
<rss version="2.0"><channel><title>T &b;</title>
<item><title>x &xxe; y</title><link>https://a.example/1</link><guid>1</guid></item>
</channel></rss>"#;
        // Either refused or read with the entities left out: never expanded.
        if let Ok(f) = parse(xml.as_bytes(), "https://a.example/feed") {
            assert!(!f.title.contains("aaaa"), "{}", f.title);
            assert!(f.items.iter().all(|i| !i.title.contains("root:")));
        }
    }

    #[test]
    fn reads_rss_and_youtube_atom() {
        let rss = r#"<rss version="2.0"><channel><title>My &amp; Blog</title>
<image><url>https://a.example/logo.png</url><title>x</title><link>https://a.example</link></image>
<item><title>Old</title><link>https://a.example/old</link><pubDate>Mon, 02 Jan 2023 10:00:00 GMT</pubDate></item>
<item><title>New</title><link>https://a.example/new</link><description>&lt;p&gt;Hello &lt;b&gt;world&lt;/b&gt;&lt;/p&gt;</description><pubDate>Tue, 03 Jan 2023 10:00:00 GMT</pubDate></item>
<item><title>No link</title><guid>z</guid></item>
<item><title>Script</title><link>javascript:alert(1)</link></item>
</channel></rss>"#;
        let f = parse(rss.as_bytes(), "https://a.example/feed").unwrap();
        assert_eq!(f.title, "My & Blog");
        let links: Vec<&str> = f.items.iter().map(|i| i.link.as_str()).collect();
        assert_eq!(links, ["https://a.example/new", "https://a.example/old"]);
        assert_eq!(f.items[0].summary, "Hello world");
        // Items with no guid are known by their link, the same at every check.
        assert_eq!(f.items[0].id, "https://a.example/new");
        let yt = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns:yt="http://www.youtube.com/xml/schemas/2015" xmlns:media="http://search.yahoo.com/mrss/" xmlns="http://www.w3.org/2005/Atom">
 <title>Some Channel</title>
 <entry>
  <id>yt:video:abc</id><yt:videoId>abc</yt:videoId>
  <title>A video</title>
  <link rel="alternate" href="https://www.youtube.com/watch?v=abc"/>
  <author><name>Some Channel</name></author>
  <published>2026-11-01T10:00:00+00:00</published>
  <media:group><media:title>A video</media:title>
   <media:content url="https://www.youtube.com/v/abc?version=3" type="application/x-shockwave-flash" width="640" height="390"/>
   <media:thumbnail url="https://i1.ytimg.com/vi/abc/hqdefault.jpg" width="480" height="360"/>
   <media:description>About it</media:description></media:group>
 </entry>
</feed>"#;
        let f = parse(yt.as_bytes(), "https://www.youtube.com/feeds/videos.xml").unwrap();
        assert_eq!(f.title, "Some Channel");
        let i = &f.items[0];
        assert_eq!(
            (i.id.as_str(), i.link.as_str()),
            ("yt:video:abc", "https://www.youtube.com/watch?v=abc")
        );
        assert_eq!(i.author, "Some Channel");
        assert_eq!(i.image, "https://i1.ytimg.com/vi/abc/hqdefault.jpg");
        assert_eq!(i.summary, "About it");
        assert!(parse(b"<html><body>hi</body></html>", "https://a.example").is_err());
    }

    #[test]
    fn reads_what_the_owner_typed_for_youtube() {
        let id = "UC_x5XG1OV2P6uZZ5FSM9Ttw";
        assert_eq!(youtube_input(id), Ok(YouTube::Id(id.into())));
        assert_eq!(
            youtube_input(&format!("https://www.youtube.com/channel/{id}/")),
            Ok(YouTube::Id(id.into()))
        );
        assert_eq!(
            youtube_input("@GoogleDevelopers"),
            Ok(YouTube::Page(
                "https://www.youtube.com/@GoogleDevelopers".into()
            ))
        );
        assert_eq!(
            youtube_input("youtube.com/@Some.Name"),
            Ok(YouTube::Page("https://www.youtube.com/@Some.Name".into()))
        );
        for bad in [
            "https://evil.example/@x",
            "@a/../../x",
            "https://www.youtube.com/watch?v=1",
            "@",
        ] {
            assert!(youtube_input(bad).is_err(), "{bad}");
        }
        let page = r#"<link rel="canonical" href="https://www.youtube.com/channel/UC_x5XG1OV2P6uZZ5FSM9Ttw"><meta property="og:title" content="Google &amp; Devs">"#;
        assert_eq!(
            tag_value(page, "<link rel=\"canonical\"", "href").as_deref(),
            Some("https://www.youtube.com/channel/UC_x5XG1OV2P6uZZ5FSM9Ttw")
        );
        assert_eq!(
            tag_value(page, "<meta property=\"og:title\"", "content").as_deref(),
            Some("Google & Devs")
        );
    }
}
