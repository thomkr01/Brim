//! Inline SVG icons from `assets/icons`, the same set the bot uses as custom emojis.
use leptos::prelude::*;

macro_rules! icons {
    ($($n:literal),*) => {
        fn svg(name: &str) -> &'static str {
            match name {
                $($n => include_str!(concat!("../../../../assets/icons/", $n, ".svg")),)*
                _ => "",
            }
        }
    };
}
icons!(
    "bot",
    "dashboard",
    "image",
    "calendar",
    "clock",
    "crab",
    "reason",
    "quarantine_role",
    "toggle_on",
    "toggle_off",
    "pin",
    "mail",
    "send",
    "copy",
    "eye",
    "success",
    "error",
    "warning",
    "right_sort",
    "bell",
    "menu",
    "arrow_up",
    "arrow_down",
    "edit",
    "link",
    "page_prev",
    "page_next",
    "checkbox_off",
    "modmail",
    "info",
    "setup_wizard",
    "sparkle",
    "fingerprint",
    "logs",
    "tray",
    "channel_text",
    "lock",
    "page",
    "ban",
    "globe",
    "sliders",
    "discord",
    "tag",
    "shield",
    "hand",
    "member_join",
    "glasses",
    "rocket",
    "qr_code",
    "moderator",
    "case",
    "analytics",
    "note",
    "report",
    "ticket",
    "target",
    "settings",
    "restore",
    "revert",
    "refresh",
    "history",
    "palette",
    "rescue_key",
    "chevron_right",
    "chat_shield",
    "announcement",
    "age_18",
    "word_censor",
    "backup",
    "trash",
    "back",
    "channel_voice",
    "category",
    "channel_stage",
    "forum",
    "server",
    "verification",
    "nsfw_filter",
    "webhook",
    "role",
    "dropdown",
    "suspicious_account",
    "support",
    "appeal",
    "question",
    "message",
    "emoji",
    "code",
    "select",
    "notifications_feed",
    "video_new",
    "trophy",
    "user_id",
    "star",
    "members",
    "more"
);

/// A `{e:name}` tag as an inline icon, for previews of text a server owner wrote.
// shortcut: only the icons listed above preview; others show as the tag. Add a name when a default uses it.
pub fn tag_html(name: &str) -> Option<String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let svg = svg(name);
    if svg.is_empty() {
        return None;
    }
    let n = N.fetch_add(1, Ordering::Relaxed);
    let svg = svg
        .replace(&format!("\"{name}-"), &format!("\"t{name}{n}-"))
        .replace(&format!("#{name}-"), &format!("#t{name}{n}-"));
    Some(format!(
        "<span class=\"i emo\" aria-hidden=\"true\">{svg}</span>"
    ))
}

/// Text with each `{e:name}` tag that has an icon drawn as one.
pub fn fill_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find("{e:") {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest.find('}').map_or(rest.len(), |i| i + 1);
        let tag = &rest[..end];
        match tag
            .strip_prefix("{e:")
            .and_then(|t| t.strip_suffix('}'))
            .and_then(tag_html)
        {
            Some(h) => out.push_str(&h),
            None => out.push_str(tag),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Every copy gets its own gradient and mask ids: with shared ids every copy paints with the
/// first one's, which draws nothing once that one is hidden (closed nav group, hidden tab).
pub fn icon(name: &'static str) -> AnyView {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let svg = svg(name)
        .replace(&format!("\"{name}-"), &format!("\"{name}{n}-"))
        .replace(&format!("#{name}-"), &format!("#{name}{n}-"));
    view! { <span class="i" aria-hidden="true" inner_html=svg></span> }.into_any()
}
