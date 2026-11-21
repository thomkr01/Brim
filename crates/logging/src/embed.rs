//! Log layout, shared by every log in the bot: a bold title, a colour bar, then
//! description lines that start with an arrow (`▸ **Responsible:** @user *`id`*`), bold section
//! headings ("Changes", "Previous Settings") and red/green diff blocks for before/after text.
use twilight_model::{
    channel::message::{
        component::{ActionRow, Button, ButtonStyle, Component},
        Embed,
    },
    user::User,
};
use twilight_util::builder::embed::{
    EmbedAuthorBuilder, EmbedBuilder, EmbedFieldBuilder, EmbedFooterBuilder, ImageSource,
};

/// Log colours: Discord logs (create, update, delete) and our own
/// logs (alerts and reports).
pub const GREEN: u32 = 0x1BE725;
pub const ORANGE: u32 = 0xE77E1B;
pub const RED: u32 = 0xE71B1B;
pub const ALERT: u32 = 0xD0021A;
pub const DONE: u32 = 0x08E36B;
pub const HANDLED: u32 = 0x00CB73;
pub const BLUE: u32 = 0x0D9DF8;

use crate::emoji::{for_label, CLOCK};
pub use crate::emoji::{ARROW, NO, YES};

/// A field name with its label icon in front, once the custom emojis are loaded (the Unicode
/// arrow would only clutter a field name). Names that already start with an emoji stay as they are.
pub(crate) fn field_name(name: &str) -> String {
    let icon = for_label(name);
    let shown = icon.to_string();
    match name.chars().next() {
        Some(c) if c.is_ascii_alphabetic() && icon.name != ARROW.name && shown != icon.fallback => {
            format!("{shown} {name}")
        }
        _ => name.into(),
    }
}

/// `<icon> **label:**`, the start of a labelled line.
pub fn label_head(label: &str) -> String {
    format!("{} **{label}:**", for_label(label))
}

/// Discord's embed limits.
const TITLE: usize = 256;
const DESCRIPTION: usize = 4096;
const FIELD_NAME: usize = 256;
const FIELD_VALUE: usize = 1024;
const FIELDS: usize = 25;
/// Discord rejects an embed whose title, description, fields and footer add up to more.
const TOTAL: usize = 6000;

#[derive(Clone)]
pub struct Log {
    embed: EmbedBuilder,
    title: String,
    lines: Vec<String>,
    fields: Vec<(String, String, bool)>,
    footer: Option<String>,
    /// Chars of the author line, which counts toward Discord's 6000 total.
    author: usize,
    /// The entry of this log when its title does not name one, and the values an edited version
    /// of that entry may use (Bot Messages page), see [`Log::entry`] and [`Log::var`].
    entry: Option<String>,
    vars: Vec<(String, String)>,
}

impl Log {
    pub fn new(title: &str, color: u32) -> Self {
        Self {
            embed: EmbedBuilder::new().color(color),
            title: clip(title, TITLE),
            lines: Vec::new(),
            fields: Vec::new(),
            footer: None,
            author: 0,
            entry: None,
            vars: Vec::new(),
        }
    }

    /// The Bot Messages entry of this log (`log.<group>.<type>`), for a log with no title of its
    /// own to find it by.
    pub fn entry(mut self, key: &str) -> Self {
        self.entry = Some(key.into());
        self
    }

    /// A value an edited version of the log's entry fills as `{name}`.
    pub fn var(mut self, name: &str, value: &str) -> Self {
        // A later value replaces an earlier one (a purge fills what the case log left empty).
        self.vars.retain(|(n, _)| n != name);
        self.vars.push((name.into(), value.into()));
        self
    }

    /// The entry key and the values set with [`Log::entry`] and [`Log::var`].
    pub fn meta(&self) -> (Option<&str>, &[(String, String)]) {
        (self.entry.as_deref(), &self.vars)
    }

    /// The built embed and the values for an edited version of its entry.
    pub fn parts(self) -> (Embed, Vec<(String, String)>) {
        let vars = self.vars.clone();
        (self.build(), vars)
    }

    /// A labelled line (left out when `value` is empty) whose value an edited version of the
    /// entry fills as `{var}`; the value is set either way, so an unset `{var}` never shows.
    pub fn fact(self, label: &str, var: &str, value: &str) -> Self {
        let l = if value.is_empty() {
            self
        } else {
            self.line(label, value)
        };
        l.var(var, value)
    }

    /// `▸ **label:** @user *`id`*` that an edited version of the entry fills as `{var}`.
    pub fn user_fact(self, label: &str, var: &str, id: u64) -> Self {
        self.fact(label, var, &user(id))
    }

    /// A free text line (left out when empty) that an edited version of the entry fills as `{var}`.
    pub fn fact_text(self, var: &str, text: &str) -> Self {
        let l = if text.is_empty() {
            self
        } else {
            self.text(text)
        };
        l.var(var, text)
    }

    /// The description lines so far, as the log shows them.
    pub fn lines_text(&self) -> String {
        self.lines.join("\n")
    }

    /// `▸ **label:** value`
    pub fn line(mut self, label: &str, value: &str) -> Self {
        self.lines.push(format!("{} {value}", label_head(label)));
        self
    }

    /// A line as given, for ✅/❌ status lines and free text.
    pub fn text(mut self, text: &str) -> Self {
        self.lines.push(text.into());
        self
    }

    /// `▸ <time>` with the current time, as on Discord logs.
    pub fn time(self) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let when = format!("{CLOCK} <t:{now}:F>");
        self.var("when", &when).text(&when)
    }

    /// A bold heading ("Changes", "Settings", "Previous Settings", "Additional Information").
    pub fn section(mut self, name: &str) -> Self {
        self.lines.push(format!("**{name}**"));
        self
    }

    /// Red `- old` and green `+ new` diff blocks, before/after display.
    pub fn diff(mut self, old: Option<&str>, new: Option<&str>) -> Self {
        for (sign, text) in [("-", old), ("+", new)] {
            if let Some(t) = text {
                let body: Vec<String> = t
                    // Every backtick: a run of 4 or 5 would keep a real fence after a ``` replace.
                    .replace('`', "`\u{200b}")
                    .lines()
                    .map(|l| format!("{sign} {l}"))
                    .collect();
                let body = if body.is_empty() {
                    format!("{sign} ")
                } else {
                    body.join("\n")
                };
                self.lines
                    .push(format!("```diff\n{}\n```", clip(&body, 1500)));
            }
        }
        self
    }

    /// `▸ **label:** @user *`id`*`
    pub fn user(self, label: &str, id: u64) -> Self {
        self.line(label, &user(id))
    }

    /// A field below the description, for columns ("Initial Flags:", "Roles:").
    pub fn field(mut self, name: &str, value: &str) -> Self {
        self.fields.push((field_name(name), value.into(), false));
        self
    }

    /// A field whose name already has its icon (a rendered default's).
    pub(crate) fn raw_field(mut self, name: &str, value: &str, inline: bool) -> Self {
        self.fields.push((name.into(), value.into(), inline));
        self
    }

    pub fn inline(mut self, name: &str, value: &str) -> Self {
        self.fields.push((field_name(name), value.into(), true));
        self
    }

    pub fn thumbnail(mut self, u: &User) -> Self {
        if let Ok(src) = ImageSource::url(avatar(u)) {
            self.embed = self.embed.thumbnail(src);
        }
        self
    }

    /// A small picture at the top right and a large one below (the old and the new avatar).
    pub fn pictures(mut self, small: &str, large: &str) -> Self {
        if let (Ok(s), Ok(l)) = (ImageSource::url(small), ImageSource::url(large)) {
            self.embed = self.embed.thumbnail(s).image(l);
        }
        self
    }

    /// Report layout: the title sits in the author line next to the avatar.
    pub fn author(mut self, u: &User, text: &str) -> Self {
        let text = clip(text, 256);
        self.author = text.chars().count();
        let mut a = EmbedAuthorBuilder::new(text);
        if let Ok(src) = ImageSource::url(avatar(u)) {
            a = a.icon_url(src);
        }
        self.embed = self.embed.author(a);
        self
    }

    pub fn footer(mut self, text: &str) -> Self {
        self.footer = Some(clip(text, 2048));
        self
    }

    /// Cuts the parts to Discord's limits, description first, so a long log is shortened
    /// instead of rejected.
    pub fn build(self) -> Embed {
        let mut left = TOTAL - self.title.chars().count() - self.author;
        // Mod logs have no title, and Discord rejects an empty one.
        let mut embed = if self.title.is_empty() {
            self.embed
        } else {
            self.embed.title(self.title)
        };
        if let Some(f) = self.footer {
            left -= f.chars().count();
            embed = embed.footer(EmbedFooterBuilder::new(f));
        }
        if !self.lines.is_empty() {
            let d = clip(&self.lines.join("\n"), DESCRIPTION.min(left));
            left -= d.chars().count();
            embed = embed.description(d);
        }
        for (name, value, inline) in self.fields.into_iter().take(FIELDS) {
            let name = clip(&name, FIELD_NAME);
            let room = left.saturating_sub(name.chars().count()).min(FIELD_VALUE);
            if room < 2 {
                break;
            }
            let value = value_of(&value, room);
            left -= name.chars().count() + value.chars().count();
            let f = EmbedFieldBuilder::new(name, value);
            embed = embed.field(if inline { f.inline() } else { f });
        }
        embed.build()
    }
}

/// `@user *`id`*`: the id stays readable when the mention can't resolve.
pub fn user(id: u64) -> String {
    format!("<@{id}> *`{id}`*")
}

pub fn channel(id: u64) -> String {
    format!("<#{id}> *`{id}`*")
}

pub fn role(id: u64) -> String {
    format!("<@&{id}> *`{id}`*")
}

/// The user's avatar, or Discord's default one.
pub fn avatar(u: &User) -> String {
    avatar_url(u.id.get(), u.avatar.map(|h| h.to_string()).as_deref())
}

/// [`avatar`] from a stored avatar hash.
pub fn avatar_url(user: u64, hash: Option<&str>) -> String {
    match hash {
        Some(h) => format!("https://cdn.discordapp.com/avatars/{user}/{h}.png"),
        None => format!(
            "https://cdn.discordapp.com/embed/avatars/{}.png",
            (user >> 22) % 6
        ),
    }
}

/// Short durations: `45s`, `30m`, `2h`, `7d`.
pub fn duration(secs: i64) -> String {
    match secs {
        s if s >= 86_400 && s % 86_400 == 0 => format!("{}d", s / 86_400),
        s if s >= 3600 && s % 3600 == 0 => format!("{}h", s / 3600),
        s if s >= 60 && s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

/// Discord rejects empty field values.
fn value_of(v: &str, max: usize) -> String {
    if v.trim().is_empty() {
        return "None".into();
    }
    clip(v, max)
}

/// Keeps the first `max` characters, ending in "…" when cut.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.into();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

pub const VERIFY_START: &str = "verify:start";
pub const VERIFY_HELP: &str = "verify:help";
/// "I accept the rules" on a saved message: gives the verification roles only where a press on
/// Verify would give them without a challenge.
pub const VERIFY_ACCEPT: &str = "verify:accept";

/// A button that sends an interaction with `id`.
pub fn button(label: &str, id: &str, style: ButtonStyle) -> Component {
    Component::Button(Button {
        id: None,
        custom_id: Some(id.into()),
        disabled: false,
        emoji: None,
        label: Some(label.into()),
        style,
        url: None,
        sku_id: None,
    })
}

/// The verify panel, posted by the `verifysetup` command and by the dashboard wizard.
pub fn verify_panel(server: &str) -> (Embed, Component) {
    let embed = EmbedBuilder::new()
        .color(GREEN)
        .title("🧩 Verification Required!")
        .description(format!(
            "To access `{server}`, you need to pass the verification first.\n{ARROW} Press on the **Verify** button below."
        ))
        .build();
    let row = Component::ActionRow(ActionRow {
        id: None,
        components: vec![
            button("Verify", VERIFY_START, ButtonStyle::Success),
            button("Help", VERIFY_HELP, ButtonStyle::Secondary),
        ],
    });
    (embed, row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_marks_cut_text() {
        assert_eq!(clip("abc", 3), "abc");
        assert_eq!(clip("abcd", 3), "ab…");
        assert_eq!(clip(&"é".repeat(2000), 1024).chars().count(), 1024);
    }

    #[test]
    fn caps_fields_and_fills_empty_values() {
        let mut log = Log::new("t", RED);
        for i in 0..30 {
            log = log.field(&i.to_string(), "");
        }
        let e = log.build();
        assert_eq!(e.fields.len(), FIELDS);
        assert_eq!(e.fields[0].value, "None");
    }

    #[test]
    fn lines_and_diff() {
        let e = Log::new("Message Edited", ORANGE)
            .user("Responsible", 7)
            .section("Changes")
            .diff(Some("a"), Some("b\nc"))
            .build();
        assert_eq!(
            e.description.unwrap(),
            "▸ **Responsible:** <@7> *`7`*\n**Changes**\n```diff\n- a\n```\n```diff\n+ b\n+ c\n```"
        );
    }

    #[test]
    fn long_logs_fit_discords_total_limit() {
        let mut log = Log::new("t", RED).text(&"d".repeat(5000));
        for _ in 0..6 {
            log = log.field("f", &"v".repeat(1024));
        }
        let e = log.build();
        let fields: usize = e
            .fields
            .iter()
            .map(|f| f.name.len() + f.value.chars().count())
            .sum();
        assert!(1 + e.description.unwrap().chars().count() + fields <= TOTAL);
        assert_eq!(e.fields[0].value.chars().count(), 1024);
    }

    #[test]
    fn short_durations() {
        assert_eq!(duration(1800), "30m");
        assert_eq!(duration(7200), "2h");
        assert_eq!(duration(90), "90s");
        assert_eq!(duration(604_800), "7d");
    }

    #[test]
    fn diff_cannot_close_its_block() {
        for n in 3..=8 {
            let ticks = "`".repeat(n);
            let e = Log::new("t", RED)
                .diff(Some(&format!("x{ticks}y\n**forged**")), None)
                .build();
            assert_eq!(e.description.unwrap().matches("```").count(), 2, "{n}");
        }
    }
}
