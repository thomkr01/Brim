//! Forms: one engine for applications (staff accept or deny) and plain forms (answers are only
//! collected). The stored rows, the typed body with its checks, the starters, the open and close
//! jobs, and [`submit`], the one write path of the pop-up, the DM and the website.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::{
    types::{
        chrono::{DateTime, Utc},
        Json,
    },
    PgPool,
};

use crate::giveaways::{refusal, Facts, Refusal, Req};

/// Forms per server (abuse limit).
pub const MAX_FORMS: i64 = 50;
pub const MAX_QUESTIONS: usize = 100;
pub const MAX_NAME: usize = 45;
pub const MAX_DESCRIPTION: usize = 1000;
/// A question's text: Discord's limit for a pop-up label.
pub const MAX_TITLE: usize = 45;
/// A question's description and placeholder, an option's label and description.
pub const MAX_HINT: usize = 100;
/// The text of a text block.
pub const MAX_BLOCK: usize = 1000;
pub const MAX_ANSWER: usize = 4000;
/// Options of a choice or checkboxes question; a dropdown takes up to [`MAX_DROPDOWN`].
pub const MAX_OPTIONS: usize = 10;
pub const MAX_DROPDOWN: usize = 25;
/// Picks of a checkboxes, member, role or channel question.
pub const MAX_PICKS: u8 = 25;
pub const MAX_PATTERN: usize = 300;
pub const MAX_ID: usize = 16;
/// Between two responses of one member: 0 (none), or 1 minute to 1 year.
pub const COOLDOWN_SECS: (u32, u32) = (60, 31_536_000);
/// "Open at" and "close at" lie at most this far ahead.
pub const MAX_AHEAD_SECS: i64 = 31_536_000;
/// Stored responses per form; above it the oldest decided ones go (open ones stay).
pub const MAX_RESPONSES: i64 = 50_000;

/// Sets `open` at `opens_at`. Payload `{"id": form, "at": unix seconds}`.
pub const OPEN_JOB: &str = "form_open";
/// Clears `open` at `closes_at`. Payload as [`OPEN_JOB`].
pub const CLOSE_JOB: &str = "form_close";
/// Queued by [`submit`] for the bot: the review post, the member's DM and the roles. Payload
/// `{"id": response, "edited": bool}`.
pub const RESPONSE_JOB: &str = "form_response";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Staff accept or deny each response.
    Application,
    /// The answers are only collected.
    Form,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Application => "application",
            Kind::Form => "form",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QKind {
    #[default]
    Short,
    Paragraph,
    /// One of the options.
    Choice,
    /// Any of the options.
    Checkboxes,
    /// One of the options, in a menu.
    Dropdown,
    Member,
    Role,
    Channel,
    /// Text to read, not a question.
    Text,
}

impl QKind {
    /// Discord gives no server pickers in a DM.
    pub fn picker(self) -> bool {
        matches!(self, QKind::Member | QKind::Role | QKind::Channel)
    }

    fn options(self) -> Option<usize> {
        match self {
            QKind::Choice | QKind::Checkboxes => Some(MAX_OPTIONS),
            QKind::Dropdown => Some(MAX_DROPDOWN),
            _ => None,
        }
    }

    fn typed(self) -> bool {
        matches!(self, QKind::Short | QKind::Paragraph)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Opt {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Question {
    /// Stable, so answers keep their question when the questions move.
    pub id: String,
    pub kind: QKind,
    pub title: String,
    /// Under the question; the text itself for a text block.
    pub description: String,
    pub placeholder: String,
    pub required: bool,
    /// Answer length for short and paragraph.
    pub min: Option<u16>,
    pub max: Option<u16>,
    pub options: Vec<Opt>,
    /// Picks for checkboxes, member, role and channel.
    pub min_picks: Option<u8>,
    pub max_picks: Option<u8>,
    /// The whole answer must match this regular expression (short and paragraph).
    pub pattern: String,
}

impl Default for Question {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: QKind::Short,
            title: String::new(),
            description: String::new(),
            placeholder: String::new(),
            required: true,
            min: None,
            max: None,
            options: Vec::new(),
            min_picks: None,
            max_picks: None,
            pattern: String::new(),
        }
    }
}

/// `pattern` as Brim runs it: on the whole answer, with a size cap. Rust's regex runs in linear
/// time, so no pattern can hang the bot.
fn compile(pattern: &str) -> Result<regex::Regex, regex::Error> {
    regex::RegexBuilder::new(&format!(r"\A(?:{pattern})\z"))
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()
}

fn long(s: &str, max: usize) -> bool {
    s.chars().count() > max
}

impl Question {
    /// The first rule this question breaks, as a sentence for the person who set it.
    pub fn problem(&self) -> Option<String> {
        if self.id.is_empty()
            || self.id.len() > MAX_ID
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Some("The question has a bad id.".into());
        }
        let block = self.kind == QKind::Text;
        if self.title.trim().is_empty() && !block {
            return Some("The question has no text.".into());
        }
        if long(&self.title, MAX_TITLE) {
            return Some(format!(
                "The question is longer than {MAX_TITLE} characters."
            ));
        }
        if block && self.description.trim().is_empty() {
            return Some("The text block is empty.".into());
        }
        let max_desc = if block { MAX_BLOCK } else { MAX_HINT };
        if long(&self.description, max_desc) {
            return Some(format!(
                "The description is longer than {max_desc} characters."
            ));
        }
        if long(&self.placeholder, MAX_HINT) {
            return Some(format!(
                "The placeholder is longer than {MAX_HINT} characters."
            ));
        }
        let (min, max) = (self.min.unwrap_or(0), self.max.unwrap_or(MAX_ANSWER as u16));
        if max as usize > MAX_ANSWER || min > max {
            return Some(format!(
                "Set a length from 0 to {MAX_ANSWER}, with the minimum not above the maximum."
            ));
        }
        if let Some(top) = self.kind.options() {
            if !(2..=top).contains(&self.options.len()) {
                return Some(format!("Give 2 to {top} options."));
            }
            let mut seen = std::collections::HashSet::new();
            for o in &self.options {
                if o.label.trim().is_empty() {
                    return Some("An option has no text.".into());
                }
                if long(&o.label, MAX_HINT) || long(&o.description, MAX_HINT) {
                    return Some(format!(
                        "An option or its description is longer than {MAX_HINT} characters."
                    ));
                }
                if !seen.insert(o.label.trim()) {
                    return Some(format!("Two options say \"{}\".", o.label.trim()));
                }
            }
        }
        if let Some((lo, hi, top)) = self.picks() {
            if hi > top || hi == 0 || lo > hi {
                return Some(format!(
                    "Set 1 to {top} picks, with the minimum not above the maximum."
                ));
            }
        }
        if !self.pattern.is_empty() {
            if !self.kind.typed() {
                return Some("Only short and paragraph answers can have an answer check.".into());
            }
            if long(&self.pattern, MAX_PATTERN) {
                return Some(format!(
                    "The answer check is longer than {MAX_PATTERN} characters."
                ));
            }
            if compile(&self.pattern).is_err() {
                return Some("The answer check is not a valid regular expression.".into());
            }
        }
        None
    }

    /// (fewest, most, the top for most) picks of a checkboxes or picker question.
    fn picks(&self) -> Option<(usize, usize, usize)> {
        let top = match self.kind {
            QKind::Checkboxes => self.options.len().min(MAX_PICKS as usize),
            k if k.picker() => MAX_PICKS as usize,
            _ => return None,
        };
        let most = self.max_picks.map_or(
            if self.kind == QKind::Checkboxes {
                top
            } else {
                1
            },
            usize::from,
        );
        Some((self.min_picks.unwrap_or(1).max(1) as usize, most, top))
    }

    /// The stored form of one answer: None when it is empty and not required.
    fn answer(&self, v: Option<&Value>) -> Result<Option<Value>, String> {
        let q = self.title.trim();
        let none = || {
            if self.required {
                Err(format!("\"{q}\" needs an answer."))
            } else {
                Ok(None)
            }
        };
        let picks = |v: Option<&Value>| -> Result<Vec<String>, String> {
            match v {
                None | Some(Value::Null) => Ok(Vec::new()),
                Some(Value::Array(a)) => a
                    .iter()
                    .map(|x| x.as_str().map(|s| s.trim().to_string()))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| format!("\"{q}\" has a bad answer.")),
                Some(_) => Err(format!("\"{q}\" has a bad answer.")),
            }
        };
        let text = match v {
            None | Some(Value::Null) => Some(String::new()),
            Some(Value::String(s)) => Some(s.trim().to_string()),
            Some(_) => None,
        };
        let is_option = |s: &str| self.options.iter().any(|o| o.label.trim() == s);
        match self.kind {
            QKind::Text => Ok(None),
            QKind::Short | QKind::Paragraph => {
                let a = text.ok_or_else(|| format!("\"{q}\" has a bad answer."))?;
                if a.is_empty() {
                    return none();
                }
                let n = a.chars().count();
                let min = self.min.unwrap_or(0) as usize;
                let max = (self.max.map_or(MAX_ANSWER, usize::from)).min(MAX_ANSWER);
                if n < min || n > max {
                    return Err(format!(
                        "\"{q}\" needs {min} to {max} characters. Yours has {n}."
                    ));
                }
                if !self.pattern.is_empty() && !compile(&self.pattern).is_ok_and(|r| r.is_match(&a))
                {
                    return Err(format!("\"{q}\" does not have the right form."));
                }
                Ok(Some(Value::String(a)))
            }
            QKind::Choice | QKind::Dropdown => {
                let a = text.ok_or_else(|| format!("\"{q}\" has a bad answer."))?;
                if a.is_empty() {
                    return none();
                }
                if !is_option(&a) {
                    return Err(format!("\"{q}\": pick one of the options."));
                }
                Ok(Some(Value::String(a)))
            }
            QKind::Checkboxes | QKind::Member | QKind::Role | QKind::Channel => {
                let mut seen = std::collections::HashSet::new();
                let mut a = picks(v)?;
                a.retain(|s| seen.insert(s.clone()));
                if a.is_empty() {
                    return none();
                }
                let ok = if self.kind == QKind::Checkboxes {
                    a.iter().all(|s| is_option(s))
                } else {
                    // Stored as Discord writes the ID, never as typed ("+0012").
                    a.iter_mut().all(|s| match s.parse::<u64>() {
                        Ok(n) if n > 0 => {
                            *s = n.to_string();
                            true
                        }
                        _ => false,
                    })
                };
                let (lo, hi, _) = self.picks().unwrap_or((1, 1, 1));
                if !ok || a.len() < lo || a.len() > hi {
                    return Err(format!("\"{q}\": pick {lo} to {hi}."));
                }
                Ok(Some(Value::Array(
                    a.into_iter().map(Value::String).collect(),
                )))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ways {
    pub popup: bool,
    pub dm: bool,
    pub web: bool,
}

impl Default for Ways {
    fn default() -> Self {
        Self {
            popup: true,
            dm: true,
            web: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    Popup,
    Dm,
    Web,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Via::Popup => "popup",
            Via::Dm => "dm",
            Via::Web => "web",
        }
    }
}

/// What the `body` column holds: the questions and the settings of the form's tabs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Body {
    pub questions: Vec<Question>,
    pub ways: Ways,
    /// Who can answer: the giveaway entry rules (extra entries do not apply).
    pub who: Req,
    /// Answering again replaces the member's latest response until staff decide on it.
    pub edit: bool,
    /// Keep each response. Off: once only, but a denied member may answer again.
    pub several: bool,
    /// From the member's last send; 0 is none.
    pub cooldown_secs: u32,
    /// Only with "only collect": no user ID is stored, only a keyed hash.
    pub anonymous: bool,
}

impl Body {
    /// The first rule this body breaks for a form of `kind`, as a sentence for the person who set it.
    pub fn problem(&self, kind: Kind) -> Option<String> {
        if self.questions.iter().all(|q| q.kind == QKind::Text) {
            return Some("Add at least one question.".into());
        }
        if self.questions.len() > MAX_QUESTIONS {
            return Some(format!("A form has at most {MAX_QUESTIONS} questions."));
        }
        let mut ids = std::collections::HashSet::new();
        for (n, q) in self.questions.iter().enumerate() {
            if let Some(p) = q.problem() {
                return Some(format!("Question {}: {p}", n + 1));
            }
            if !ids.insert(q.id.as_str()) {
                return Some(format!("Question {}: two questions have one id.", n + 1));
            }
        }
        if !(self.ways.popup || self.ways.dm || self.ways.web) {
            return Some("Turn on at least one way to answer.".into());
        }
        if self.ways.dm && self.questions.iter().any(|q| q.kind.picker()) {
            return Some(
                "Member, role and channel questions cannot be answered in DMs. Turn DMs off or remove them."
                    .into(),
            );
        }
        if self.cooldown_secs > 0
            && !(COOLDOWN_SECS.0..=COOLDOWN_SECS.1).contains(&self.cooldown_secs)
        {
            return Some("The cooldown is 1 minute to 1 year.".into());
        }
        if self.anonymous && kind == Kind::Application {
            return Some(
                "Anonymous responses work only on a form that only collects answers.".into(),
            );
        }
        self.who.problem()
    }

    /// A short hash of the questions and the anonymous switch. A pop-up page or a web page carries it, so a send after
    /// staff changed the questions is refused instead of stored against the new ones.
    pub fn hash(&self) -> String {
        use sha2::{Digest, Sha256};
        // With the anonymous switch: a member who saw "anonymous" is never stored by ID.
        let json = serde_json::to_vec(&(&self.questions, self.anonymous)).unwrap_or_default();
        Sha256::digest(json)[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// The answers as they are stored (question id to answer), or the first wrong one.
    pub fn answers(&self, given: &Map<String, Value>) -> Result<Map<String, Value>, String> {
        let mut out = Map::new();
        for q in &self.questions {
            if let Some(a) = q.answer(given.get(&q.id))? {
                out.insert(q.id.clone(), a);
            }
        }
        Ok(out)
    }
}

/// A form as the dashboard saves it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setup {
    pub kind: Kind,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "yes")]
    pub open: bool,
    #[serde(default)]
    pub body: Body,
    #[serde(default)]
    pub opens_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub closes_at: Option<DateTime<Utc>>,
}

fn yes() -> bool {
    true
}

impl Setup {
    /// The first rule this setup breaks. `now`: the times must lie within a year from it.
    pub fn problem(&self, now: DateTime<Utc>) -> Option<String> {
        if self.name.trim().is_empty() {
            return Some("The form has no name.".into());
        }
        if long(self.name.trim(), MAX_NAME) {
            return Some(format!("The name is longer than {MAX_NAME} characters."));
        }
        if long(&self.description, MAX_DESCRIPTION) {
            return Some(format!(
                "The description is longer than {MAX_DESCRIPTION} characters."
            ));
        }
        let far =
            |t: &Option<DateTime<Utc>>| t.is_some_and(|t| (t - now).num_seconds() > MAX_AHEAD_SECS);
        if far(&self.opens_at) || far(&self.closes_at) {
            return Some("Open and close times are at most 1 year ahead.".into());
        }
        if let (Some(o), Some(c)) = (self.opens_at, self.closes_at) {
            if c <= o {
                return Some("The form must close after it opens.".into());
            }
        }
        if self.body.anonymous && !anonymous_ready() {
            return Some(
                "Anonymous forms need FORMS_KEY in the server's .env (DEPLOY.md, Forms key)."
                    .into(),
            );
        }
        self.body.problem(self.kind)
    }
}

/// One stored form.
#[derive(Clone, Debug, sqlx::FromRow, Serialize)]
pub struct Form {
    pub id: i64,
    pub guild_id: i64,
    pub kind: String,
    pub name: String,
    pub description: String,
    pub open: bool,
    pub body: Json<Body>,
    pub opens_at: Option<DateTime<Utc>>,
    pub closes_at: Option<DateTime<Utc>>,
    pub next_number: i32,
    pub created_by: i64,
    pub updated_at: DateTime<Utc>,
}

const COLS: &str = "id, guild_id, kind, name, description, open, body, opens_at, closes_at,
    next_number, created_by, updated_at";

pub async fn get(pool: &PgPool, guild: i64, id: i64) -> Result<Option<Form>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLS} FROM forms WHERE id = $1 AND guild_id = $2"
    )))
    .bind(id)
    .bind(guild)
    .fetch_optional(pool)
    .await?)
}

/// Whole seconds, so a job's time compares equal to the stored one.
fn secs(t: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    t.and_then(|t| DateTime::from_timestamp(t.timestamp(), 0))
}

/// "Open at" and "close at".
type Times = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// Queues the open or close job for a time ahead that was not set before.
async fn queue_times(
    tx: &mut sqlx::PgConnection,
    guild: i64,
    id: i64,
    old: Times,
    new: Times,
) -> Result<()> {
    let now = Utc::now();
    for (kind, was, at) in [(OPEN_JOB, old.0, new.0), (CLOSE_JOB, old.1, new.1)] {
        if let Some(at) = at.filter(|t| *t > now && Some(*t) != was) {
            let p = serde_json::json!({"id": id, "at": at.timestamp()});
            crate::jobs::enqueue(&mut *tx, guild, kind, p, at.timestamp()).await?;
        }
    }
    Ok(())
}

/// A form that opens later is closed until its open job runs.
fn open_now(s: &Setup, now: DateTime<Utc>) -> bool {
    s.open && s.opens_at.is_none_or(|t| t <= now)
}

/// Saves a new form, or says which limit it breaks. The count and the insert are one transaction
/// under a per-server lock.
pub async fn create(pool: &PgPool, guild: i64, by: i64, s: &Setup) -> Result<Result<i64, String>> {
    let now = Utc::now();
    if let Some(p) = s.problem(now) {
        return Ok(Err(p));
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('forms:' || $1::text, 0))")
        .bind(guild)
        .execute(&mut *tx)
        .await?;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM forms WHERE guild_id = $1")
        .bind(guild)
        .fetch_one(&mut *tx)
        .await?;
    if n >= MAX_FORMS {
        return Ok(Err(format!(
            "This server has {n} forms. The limit is {MAX_FORMS}."
        )));
    }
    // Off stays off: no open job may turn on a form staff switched off.
    let opens = secs(s.opens_at).filter(|_| s.open);
    let closes = secs(s.closes_at);
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO forms (guild_id, kind, name, description, open, body, opens_at, closes_at, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(guild)
    .bind(s.kind.as_str())
    .bind(s.name.trim())
    .bind(&s.description)
    .bind(open_now(s, now))
    .bind(Json(&s.body))
    .bind(opens)
    .bind(closes)
    .bind(by)
    .fetch_one(&mut *tx)
    .await?;
    queue_times(&mut tx, guild, id, (None, None), (opens, closes)).await?;
    tx.commit().await?;
    Ok(Ok(id))
}

/// Saves a changed form. Ok(false): no such form in this server.
pub async fn update(pool: &PgPool, guild: i64, id: i64, s: &Setup) -> Result<Result<bool, String>> {
    let now = Utc::now();
    if let Some(p) = s.problem(now) {
        return Ok(Err(p));
    }
    let mut tx = pool.begin().await?;
    let old: Option<Times> = sqlx::query_as(
        "SELECT opens_at, closes_at FROM forms WHERE id = $1 AND guild_id = $2 FOR UPDATE",
    )
    .bind(id)
    .bind(guild)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(old) = old else {
        return Ok(Ok(false));
    };
    // Off stays off: no open job may turn on a form staff switched off.
    let opens = secs(s.opens_at).filter(|_| s.open);
    let closes = secs(s.closes_at);
    sqlx::query(
        "UPDATE forms SET kind = $3, name = $4, description = $5, open = $6, body = $7,
           opens_at = $8, closes_at = $9, updated_at = now() WHERE id = $1 AND guild_id = $2",
    )
    .bind(id)
    .bind(guild)
    .bind(s.kind.as_str())
    .bind(s.name.trim())
    .bind(&s.description)
    .bind(open_now(s, now))
    .bind(Json(&s.body))
    .bind(opens)
    .bind(closes)
    .execute(&mut *tx)
    .await?;
    queue_times(&mut tx, guild, id, old, (opens, closes)).await?;
    tx.commit().await?;
    Ok(Ok(true))
}

/// The open or close job. A job whose time was changed or removed since it was queued does nothing.
pub async fn time_job(pool: &PgPool, guild: i64, kind: &str, payload: &Value) -> Result<()> {
    let sql = if kind == OPEN_JOB {
        "UPDATE forms SET open = true WHERE id = $1 AND guild_id = $2 AND opens_at = to_timestamp($3::float8)"
    } else {
        "UPDATE forms SET open = false WHERE id = $1 AND guild_id = $2 AND closes_at = to_timestamp($3::float8)"
    };
    sqlx::query(sql)
        .bind(payload["id"].as_i64().unwrap_or(0))
        .bind(guild)
        .bind(payload["at"].as_i64().unwrap_or(0) as f64)
        .execute(pool)
        .await?;
    Ok(())
}

/// One send of a form.
pub struct Submission<'a> {
    pub guild: i64,
    pub form: i64,
    /// The member, checked by the same rules as a giveaway entry.
    pub facts: &'a Facts<'a>,
    /// Question id to answer: a string, or a list of strings for checkboxes and pickers.
    pub answers: &'a Map<String, Value>,
    pub via: Via,
    /// [`Body::hash`] of the questions the member saw.
    pub seen: &'a str,
    /// When the member started (the first pop-up page or the first DM question).
    pub started_at: Option<DateTime<Utc>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Sent {
    New {
        id: i64,
        number: i32,
    },
    /// Answering again replaced the member's open response.
    Edited {
        id: i64,
        number: i32,
    },
}

/// Why a send was not stored.
#[derive(Debug, PartialEq, Eq)]
pub enum NotSent {
    /// No such form in this server.
    Gone,
    /// This way to answer is off for the form.
    WayOff,
    Closed,
    /// Opens at this time.
    NotYet(DateTime<Utc>),
    /// The questions changed since the member saw them.
    Changed,
    Refused(Refusal),
    /// An answer breaks its question's rules; the text says which.
    Bad(String),
    /// Once only, and the member already sent it.
    Already,
    /// The member may answer again at this time.
    Cooldown(DateTime<Utc>),
    /// The form holds [`MAX_RESPONSES`]; the hourly prune makes room.
    Full,
}

impl NotSent {
    /// The answer the member reads. Times are Discord time tags.
    pub fn text(&self) -> String {
        match self {
            Self::Gone => "This form does not exist anymore.".into(),
            Self::WayOff => "This form cannot be answered here.".into(),
            Self::Closed => "This form is closed.".into(),
            Self::NotYet(t) => format!("This form opens <t:{}:R>.", t.timestamp()),
            Self::Changed => "This form changed. Please start again.".into(),
            Self::Refused(r) => format!("You cannot answer this form: {}", r.text_for("form")),
            Self::Bad(why) => why.clone(),
            Self::Already => "You already answered this form.".into(),
            Self::Cooldown(t) => format!("You can answer again <t:{}:R>.", t.timestamp()),
            Self::Full => "This form is full. Please try again later.".into(),
        }
    }
}

/// The secret of anonymous forms: `FORMS_KEY` in .env, which only the bot and the dashboard get.
/// It is never in the database, so no dump or database role can tie a response to a member.
fn secret() -> Option<String> {
    let key = std::env::var("FORMS_KEY").ok().filter(|k| k.len() >= 32);
    if cfg!(test) {
        return key.or_else(|| Some("a test key that is long enough..".into()));
    }
    key
}

/// Anonymous forms work only when the key is set.
pub fn anonymous_ready() -> bool {
    secret().is_some()
}

/// The keyed hash that stands for `user` on anonymous form `form`.
fn user_key(form: i64, user: u64) -> Option<String> {
    use hmac::{Hmac, KeyInit, Mac};
    let mut m = Hmac::<sha2::Sha256>::new_from_slice(secret()?.as_bytes())
        .expect("HMAC takes any key length");
    m.update(format!("brim form\0{form}\0{user}").as_bytes());
    Some(
        m.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}

/// The one write path of every way to answer: checks that the form is open, the member may answer
/// (the giveaway rules, so a timeout or quarantine blocks), the questions are the ones the member
/// saw, every answer, "once only" and the cooldown; then stores the response, clears the draft and
/// queues [`RESPONSE_JOB`]. One transaction with the form row locked, so two sends of one member
/// and an edit of the form never cross.
pub async fn submit(pool: &PgPool, s: &Submission<'_>) -> Result<Result<Sent, NotSent>> {
    let mut tx = pool.begin().await?;
    let r = submit_in(&mut tx, s).await?;
    // A refusal ends the transaction at once: the form row stays locked until it does.
    if r.is_ok() {
        tx.commit().await?;
    } else {
        tx.rollback().await?;
    }
    Ok(r)
}

async fn submit_in(
    tx: &mut sqlx::PgConnection,
    s: &Submission<'_>,
) -> Result<Result<Sent, NotSent>> {
    type Row = (
        String,
        bool,
        Json<Body>,
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
        i32,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT kind, open, body, opens_at, closes_at, next_number FROM forms
         WHERE id = $1 AND guild_id = $2 FOR UPDATE",
    )
    .bind(s.form)
    .bind(s.guild)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((kind, open, Json(body), opens_at, closes_at, next)) = row else {
        return Ok(Err(NotSent::Gone));
    };
    let now = Utc::now();
    let way = match s.via {
        Via::Popup => body.ways.popup,
        Via::Dm => body.ways.dm,
        Via::Web => body.ways.web,
    };
    if !way {
        return Ok(Err(NotSent::WayOff));
    }
    if let Some(t) = opens_at.filter(|t| !open && *t > now) {
        return Ok(Err(NotSent::NotYet(t)));
    }
    if !open || closes_at.is_some_and(|t| t <= now) {
        return Ok(Err(NotSent::Closed));
    }
    if body.hash() != s.seen {
        return Ok(Err(NotSent::Changed));
    }
    if let Some(r) = refusal(&body.who, None, None, s.facts) {
        return Ok(Err(NotSent::Refused(r)));
    }
    let answers = match body.answers(s.answers) {
        Ok(a) => a,
        Err(why) => return Ok(Err(NotSent::Bad(why))),
    };
    let user = s.facts.user;
    let (uid, ukey) = if body.anonymous {
        let key = user_key(s.form, user).context("FORMS_KEY is not set in .env")?;
        (None, Some(key))
    } else {
        (Some(user as i64), None)
    };
    let last: Option<(i64, i32, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, number, status, submitted_at FROM form_responses
         WHERE form_id = $1 AND (user_id = $2 OR user_key = $3)
         ORDER BY submitted_at DESC LIMIT 1",
    )
    .bind(s.form)
    .bind(uid)
    .bind(&ukey)
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM form_drafts WHERE form_id = $1 AND user_id = $2")
        .bind(s.form)
        .bind(user as i64)
        .execute(&mut *tx)
        .await?;
    // The cooldown holds for an edit too, so answering again cannot flood the review channel.
    if let Some((_, _, _, at)) = &last {
        let until = *at + chrono::TimeDelta::seconds(body.cooldown_secs as i64);
        if body.cooldown_secs > 0 && until > now {
            return Ok(Err(NotSent::Cooldown(until)));
        }
    }
    // Only an application waits for a decision; a collected response is never replaced, so
    // "several responses" keeps each one.
    let editable = |st: &str| st == "open";
    let sent = match last {
        Some((id, number, status, _)) if body.edit && editable(&status) => {
            sqlx::query(
                "UPDATE form_responses SET answers = $2, via = $3, started_at = $4, submitted_at = now()
                 WHERE id = $1",
            )
            .bind(id)
            .bind(Json(&answers))
            .bind(s.via.as_str())
            .bind(s.started_at)
            .execute(&mut *tx)
            .await?;
            Sent::Edited { id, number }
        }
        last => {
            if let Some((_, _, status, _)) = &last {
                if !body.several && status != "denied" && status != "withdrawn" {
                    return Ok(Err(NotSent::Already));
                }
            }
            // Only a form that numbered past the cap can hold it: no count on a normal send.
            if next as i64 > MAX_RESPONSES {
                let n: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM form_responses WHERE form_id = $1")
                        .bind(s.form)
                        .fetch_one(&mut *tx)
                        .await?;
                if n >= MAX_RESPONSES {
                    return Ok(Err(NotSent::Full));
                }
            }
            let number: i32 = sqlx::query_scalar(
                "UPDATE forms SET next_number = next_number + 1 WHERE id = $1 AND guild_id = $2
                 RETURNING next_number - 1",
            )
            .bind(s.form)
            .bind(s.guild)
            .fetch_one(&mut *tx)
            .await?;
            let status = if kind == "application" {
                "open"
            } else {
                "collected"
            };
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO form_responses (form_id, guild_id, number, user_id, user_key, status, answers, via, started_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
            )
            .bind(s.form)
            .bind(s.guild)
            .bind(number)
            .bind(uid)
            .bind(&ukey)
            .bind(status)
            .bind(Json(&answers))
            .bind(s.via.as_str())
            .bind(s.started_at)
            .fetch_one(&mut *tx)
            .await?;
            Sent::New { id, number }
        }
    };
    let (id, edited) = match sent {
        Sent::New { id, .. } => (id, false),
        Sent::Edited { id, .. } => (id, true),
    };
    let p = serde_json::json!({"id": id, "edited": edited});
    crate::jobs::enqueue(&mut *tx, s.guild, RESPONSE_JOB, p, now.timestamp()).await?;
    Ok(Ok(sent))
}

/// The hourly prunes of every server: drafts after their 30 minutes, and per form the oldest
/// decided responses above [`MAX_RESPONSES`]. In batches, so no statement holds many rows.
pub async fn prune(pool: &PgPool) -> Result<()> {
    sqlx::query("DELETE FROM form_drafts WHERE expires_at < now()")
        .execute(pool)
        .await?;
    while sqlx::query(
        "DELETE FROM form_responses WHERE id IN (
           SELECT r.id FROM (SELECT form_id, count(*) - $1 AS over FROM form_responses
                             GROUP BY form_id HAVING count(*) > $1) b
           CROSS JOIN LATERAL (SELECT id FROM form_responses WHERE form_id = b.form_id
                               AND status <> 'open' ORDER BY number LIMIT LEAST(b.over, 10000)) r)",
    )
    .bind(MAX_RESPONSES)
    .execute(pool)
    .await?
    .rows_affected()
        > 0
    {}
    Ok(())
}

/// The four starters. A starter only fills a new form; after that it is a normal form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Starter {
    StaffApplication,
    PartnershipRequest,
    PrizeClaim,
    Feedback,
}

fn ask(id: &str, kind: QKind, title: &str) -> Question {
    Question {
        id: id.into(),
        kind,
        title: title.into(),
        ..Question::default()
    }
}

fn options(q: Question, labels: &[&str]) -> Question {
    let options = labels
        .iter()
        .map(|l| Opt {
            label: (*l).into(),
            description: String::new(),
        })
        .collect();
    Question { options, ..q }
}

impl Starter {
    pub const ALL: [Starter; 4] = [
        Starter::StaffApplication,
        Starter::PartnershipRequest,
        Starter::PrizeClaim,
        Starter::Feedback,
    ];

    pub fn setup(self) -> Setup {
        use QKind::*;
        let (kind, name, questions, several, cooldown_secs) = match self {
            Starter::StaffApplication => (
                Kind::Application,
                "Staff application",
                vec![
                    Question {
                        max: Some(50),
                        ..ask("q1", Short, "Which time zone are you in?")
                    },
                    ask("q2", Paragraph, "Why do you want to help as staff?"),
                    // Without the full stop: a pop-up label holds 45 characters.
                    ask("q3", Paragraph, "Tell us about your experience with moderation"),
                    options(
                        ask("q4", Choice, "How many hours each week can you be active?"),
                        &["Less than 5", "5 to 10", "10 to 20", "More than 20"],
                    ),
                    ask("q5", Paragraph, "What do you do when a member breaks a rule?"),
                ],
                false,
                0,
            ),
            Starter::PartnershipRequest => (
                Kind::Application,
                "Partnership request",
                vec![
                    Question {
                        pattern: r"(https?://)?(www\.)?(discord\.gg|discord(app)?\.com/invite)/[A-Za-z0-9-]{2,32}/?".into(),
                        ..ask("q1", Short, "Invite link to your server")
                    },
                    Question {
                        pattern: "[0-9]{1,9}".into(),
                        ..ask("q2", Short, "How many members does your server have?")
                    },
                    ask("q3", Paragraph, "What is your server about?"),
                    ask("q4", Paragraph, "What can you offer our server?"),
                ],
                false,
                0,
            ),
            Starter::PrizeClaim => (
                Kind::Form,
                "Prize claim",
                vec![
                    Question {
                        max: Some(200),
                        placeholder: "in-game name, username or email".into(),
                        ..ask("q1", Short, "Where do we send your prize?")
                    },
                    Question {
                        max: Some(100),
                        required: false,
                        ..ask("q2", Short, "Which region or platform?")
                    },
                    Question {
                        required: false,
                        ..ask("q3", Paragraph, "Anything else for the host?")
                    },
                ],
                false,
                0,
            ),
            Starter::Feedback => (
                Kind::Form,
                "Feedback",
                vec![
                    options(
                        ask("q1", Choice, "How do you rate the server?"),
                        &["1", "2", "3", "4", "5"],
                    ),
                    ask("q2", Paragraph, "What do you like most?"),
                    Question {
                        required: false,
                        ..ask("q3", Paragraph, "What should we change?")
                    },
                ],
                true,
                7 * 86_400,
            ),
        };
        Setup {
            kind,
            name: name.into(),
            description: String::new(),
            open: true,
            body: Body {
                questions,
                several,
                cooldown_secs,
                ..Body::default()
            },
            opens_at: None,
            closes_at: None,
        }
    }
}

#[cfg(test)]
mod tests;
