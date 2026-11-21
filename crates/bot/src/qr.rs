//! QR codes in posted images: Discord login codes and scam links that text filters cannot see.
use crate::app::App;
use twilight_model::channel::Attachment;

const MAX_BYTES: usize = 8 << 20;
const MAX_IMAGES: usize = 3;
/// Bigger images are scaled down first; a QR code meant to be scanned stays readable.
const MAX_SIDE: u32 = 1024;

/// At most two images are decoded at a time across all messages, so a flood of image posts cannot fill
/// memory or the blocking threads. Downloads do not hold a slot, so a slow one holds up nobody.
static DECODE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// Messages deleted while their images wait (owner decision 11-16): no new download or decode starts for them.
/// Keyed (message, 0); a deleted channel or thread is (0, channel).
static GONE: std::sync::LazyLock<crate::automod::Seen> = std::sync::LazyLock::new(Default::default);

/// MessageDelete and MessageDeleteBulk (whoever deleted it) give `(message, 0)`; a channel or thread
/// delete takes its messages with it and gives `(0, channel)`.
pub fn deleted(key: (u64, u64)) {
    GONE.first(key);
}

fn gone((msg, channel): (u64, u64)) -> bool {
    GONE.contains((msg, 0)) || GONE.contains((0, channel))
}

/// Codes read from an image seen before, by the SHA-256 of its bytes (not a fast hash: a crafted
/// collision could make a scam image reuse a clean image's result). The decode depends on the bytes
/// and `MAX_SIDE` only, so a result is valid for every server and setting. A repost costs a hash, not
/// a decode (a 12 MP photo decodes in ~250 ms). Bounded in count and age.
static CODES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<[u8; 32], Cached>>> =
    std::sync::LazyLock::new(Default::default);
type Cached = (std::time::Instant, Vec<String>);
const CACHE_CAP: usize = 2_000;
const CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(3600);
/// A code is at most a few KB; a result with more text than this is not kept, so the cache holds
/// at most about 2,000 x 16 KB = 32 MB in the worst case.
const CACHE_MAX_TEXT: usize = 16 * 1024;

/// The codes of `bytes`, from the cache or decoded now.
fn decode_cached(bytes: &[u8]) -> Vec<String> {
    use sha2::{Digest, Sha256};
    let key: [u8; 32] = Sha256::digest(bytes).into();
    let now = std::time::Instant::now();
    if let Some((at, found)) = CODES.lock().unwrap().get(&key) {
        if now.duration_since(*at) < CACHE_TTL {
            return found.clone();
        }
    }
    let found = decode(bytes);
    if found.iter().map(String::len).sum::<usize>() <= CACHE_MAX_TEXT {
        remember(&mut CODES.lock().unwrap(), key, found.clone(), now);
    }
    found
}

/// Adds a result, first dropping expired entries (and everything when still full) so the map stays
/// within `CACHE_CAP`.
fn remember(
    c: &mut std::collections::HashMap<[u8; 32], Cached>,
    key: [u8; 32],
    found: Vec<String>,
    now: std::time::Instant,
) {
    if c.len() >= CACHE_CAP {
        c.retain(|_, (at, _)| now.duration_since(*at) < CACHE_TTL);
        // shortcut: still full after dropping the old ones, so everything goes; reposts decode once more.
        if c.len() >= CACHE_CAP {
            c.clear();
        }
    }
    c.insert(key, (now, found));
}

/// Decodes `bytes` in a slot, or None when the message was deleted before its decode started. A decode
/// that already runs finishes and counts, so a scam deleted at once is still acted on (owner decision 11-16).
async fn scan(at: (u64, u64), bytes: Vec<u8>) -> Option<Vec<String>> {
    let slot = DECODE.acquire().await.ok()?;
    if gone(at) {
        return None;
    }
    // The slot goes with the work, which runs on even if this message's task is dropped.
    let work = move || {
        let found = decode_cached(&bytes);
        drop(slot);
        found
    };
    tokio::task::spawn_blocking(work).await.ok()
}

/// The text of every QR code in the first few image attachments, one per line.
pub async fn texts(
    app: &App,
    (msg, channel): (u64, u64),
    attachments: impl Iterator<Item = &Attachment>,
) -> String {
    static READ: std::sync::LazyLock<crate::automod::Seen> =
        std::sync::LazyLock::new(Default::default);
    let mut out = String::new();
    let images = attachments.filter(|a| {
        a.size <= MAX_BYTES as u64
            && a.content_type
                .as_deref()
                .is_some_and(|t| t.starts_with("image/"))
            && !READ.contains((msg, a.id.get()))
    });
    for a in images.take(MAX_IMAGES) {
        // Deleted: no new download or decode, but codes already read still count.
        if gone((msg, channel)) {
            break;
        }
        let Some(bytes) = app.sites.fetch(&a.url, MAX_BYTES).await else {
            continue;
        };
        // None: deleted while it waited (the next round stops), or the decoder panicked.
        let Some(found) = scan((msg, channel), bytes).await else {
            continue;
        };
        READ.first((msg, a.id.get()));
        for t in found {
            out.push_str(&t);
            out.push('\n');
        }
    }
    out
}

/// Decodes an untrusted image (the first frame of an animation). A tiny file can claim a huge size;
/// the limits stop it before it allocates.
pub(crate) fn read_image(bytes: &[u8]) -> Option<image::DynamicImage> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 << 20);
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits);
    reader.decode().ok()
}

fn decode(bytes: &[u8]) -> Vec<String> {
    let Some(img) = read_image(bytes) else {
        return vec![];
    };
    let img = if img.width().max(img.height()) > MAX_SIDE {
        img.thumbnail(MAX_SIDE, MAX_SIDE)
    } else {
        img
    };
    let g = img.to_luma8();
    let (w, h) = (g.width() as usize, g.height() as usize);
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| {
        g.get_pixel(x as u32, y as u32).0[0]
    });
    prepared
        .detect_grids()
        .iter()
        .filter_map(|grid| grid.decode().ok().map(|(_, text)| text))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qr_png(text: &str, scale: u32) -> Vec<u8> {
        let code = qrcode::QrCode::new(text).unwrap();
        let n = code.width() as u32;
        let quiet = 4;
        let side = (n + 2 * quiet) * scale;
        let colors = code.to_colors();
        let img = image::GrayImage::from_fn(side, side, |x, y| {
            let (cx, cy) = (x / scale, y / scale);
            let dark = cx >= quiet
                && cy >= quiet
                && cx < n + quiet
                && cy < n + quiet
                && colors[((cy - quiet) * n + cx - quiet) as usize] == qrcode::Color::Dark;
            image::Luma([if dark { 0 } else { 255 }])
        });
        let mut out = std::io::Cursor::new(vec![]);
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn key(bytes: &[u8]) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes).into()
    }

    #[test]
    fn a_reposted_image_is_read_from_the_cache_with_the_same_result() {
        let url = "https://discord.com/ra/cacheTEST123";
        let png = qr_png(url, 8);
        assert!(!CODES.lock().unwrap().contains_key(&key(&png)));
        assert_eq!(decode_cached(&png), [url]);
        assert!(CODES.lock().unwrap().contains_key(&key(&png)));
        assert_eq!(decode_cached(&png), [url], "a hit answers like the decode");
        // An image without a code is cached too, as no codes.
        let plain = {
            let mut out = std::io::Cursor::new(vec![]);
            image::GrayImage::from_pixel(64, 64, image::Luma([200]))
                .write_to(&mut out, image::ImageFormat::Png)
                .unwrap();
            out.into_inner()
        };
        assert!(decode_cached(&plain).is_empty());
        assert!(CODES.lock().unwrap().contains_key(&key(&plain)));
        // Other bytes never get this result.
        assert_ne!(
            decode_cached(&qr_png("https://discord.com/ra/other0000001", 8)),
            [url]
        );
    }

    #[test]
    fn the_cache_is_bounded_in_count_and_age() {
        let now = std::time::Instant::now();
        let old = now - CACHE_TTL - std::time::Duration::from_secs(1);
        let fill = |at, tag: u8| -> std::collections::HashMap<[u8; 32], Cached> {
            (0..CACHE_CAP as u32)
                .map(|i| {
                    let mut k = [tag; 32];
                    k[..4].copy_from_slice(&i.to_le_bytes());
                    (k, (at, vec![]))
                })
                .collect()
        };
        // Full of expired entries: they go, the new one stays.
        let mut c = fill(old, 0);
        remember(&mut c, [9; 32], vec!["a".into()], now);
        assert_eq!(c.len(), 1);
        // Full of fresh entries: everything goes, then the new one is kept.
        let mut c = fill(now, 1);
        remember(&mut c, [9; 32], vec![], now);
        assert_eq!(c.len(), 1);
        // Not full: it only adds.
        remember(&mut c, [8; 32], vec![], now);
        assert_eq!(c.len(), 2);
        // An expired entry is decoded again, not trusted.
        let png = qr_png("https://discord.com/ra/boundTEST123", 8);
        CODES
            .lock()
            .unwrap()
            .insert(key(&png), (old, vec!["stale".into()]));
        assert_eq!(decode_cached(&png), ["https://discord.com/ra/boundTEST123"]);
    }

    #[tokio::test]
    async fn a_deleted_message_is_not_scanned() {
        let url = "https://discord.com/ra/abcdefGHIJ123";
        let ok = Some(vec![url.to_string()]);
        assert_eq!(scan((9_001, 1), qr_png(url, 8)).await, ok);
        deleted((9_002, 0));
        assert_eq!(scan((9_002, 1), qr_png(url, 8)).await, None);
        deleted((0, 2));
        assert_eq!(
            scan((9_003, 2), qr_png(url, 8)).await,
            None,
            "its channel was deleted"
        );
    }

    #[test]
    fn reads_a_login_qr_code_also_from_a_big_image() {
        let url = "https://discord.com/ra/abcdefGHIJ123";
        assert_eq!(decode(&qr_png(url, 8)), [url]);
        assert_eq!(decode(&qr_png(url, 60)), [url]);
        assert!(decode(b"not an image").is_empty());
    }
}
