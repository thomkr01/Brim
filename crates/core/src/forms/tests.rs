use super::*;
use serde_json::json;
use twilight_model::id::{marker::RoleMarker, Id};

fn facts(user: u64, roles: &[Id<RoleMarker>]) -> Facts<'_> {
    Facts {
        user,
        roles,
        bot: false,
        quarantined: false,
        flagged: false,
        timed_out: false,
        raid_joiner: false,
        unverified: false,
        joined_ms: Some(0),
        now_ms: 4_000_000_000_000,
    }
}

fn map(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

fn staff() -> Setup {
    Starter::StaffApplication.setup()
}

fn staff_answers() -> Map<String, Value> {
    map(json!({
        "q1": "UTC+1", "q2": "I like to help.", "q3": "Two years.",
        "q4": "5 to 10", "q5": "I talk to them first."
    }))
}

#[test]
fn starters_pass_their_checks() {
    let now = Utc::now();
    for s in Starter::ALL {
        let setup = s.setup();
        assert_eq!(setup.problem(now), None, "{s:?}");
        assert!(setup.body.questions.len() <= 5, "{s:?} fits one pop-up");
    }
    assert_eq!(staff().body.questions.len(), 5);
    let fb = Starter::Feedback.setup();
    assert!(fb.body.several && fb.body.cooldown_secs == 7 * 86_400);
    assert_eq!(Starter::PrizeClaim.setup().kind, Kind::Form);
}

#[test]
fn a_body_breaks_its_rules_with_a_sentence() {
    let kind = Kind::Form;
    let mut b = staff().body;
    assert_eq!(b.problem(kind), None);

    let mut none = b.clone();
    none.questions = vec![Question {
        kind: QKind::Text,
        description: "Read me".into(),
        ..ask("t", QKind::Text, "")
    }];
    assert!(none
        .problem(kind)
        .unwrap()
        .contains("at least one question"));

    let mut long = b.clone();
    long.questions[0].title = "x".repeat(46);
    assert!(long.problem(kind).unwrap().starts_with("Question 1:"));

    let mut twins = b.clone();
    twins.questions[1].id = "q1".into();
    assert!(twins.problem(kind).is_some(), "two questions with one id");

    b.questions.push(ask("q6", QKind::Role, "Pick your role"));
    assert!(
        b.problem(kind).unwrap().contains("DMs"),
        "no pickers in a DM"
    );
    b.ways.dm = false;
    assert_eq!(b.problem(kind), None);
    b.ways = Ways {
        popup: false,
        dm: false,
        web: false,
    };
    assert!(b.problem(kind).is_some(), "no way to answer");

    let mut anon = staff().body;
    anon.anonymous = true;
    assert!(anon.problem(Kind::Application).is_some());
    assert_eq!(anon.problem(Kind::Form), None);

    let mut cool = staff().body;
    cool.cooldown_secs = 30;
    assert!(cool.problem(kind).is_some(), "under a minute");

    let q = |q: Question| q.problem();
    let base = ask("a", QKind::Choice, "Pick");
    assert!(q(options(base.clone(), &["One"])).is_some(), "one option");
    assert!(q(options(base.clone(), &["One", "One"])).is_some(), "twins");
    assert!(q(options(base.clone(), &["1"; 11])).is_some(), "eleven");
    let menu = ask("a", QKind::Dropdown, "Pick");
    let labels: Vec<String> = (0..25).map(|n| n.to_string()).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    assert_eq!(q(options(menu, &labels)), None, "a dropdown takes 25");
    let bad = Question {
        pattern: "(".into(),
        ..ask("a", QKind::Short, "Name")
    };
    assert!(q(bad).unwrap().contains("regular expression"));
    let on_choice = Question {
        pattern: "x".into(),
        ..options(base.clone(), &["x", "y"])
    };
    assert!(
        q(on_choice).is_some(),
        "an answer check only on typed answers"
    );
    let picks = Question {
        max_picks: Some(26),
        ..ask("a", QKind::Member, "Who")
    };
    assert!(q(picks).is_some());
    let boxes = Question {
        max_picks: Some(3),
        ..options(ask("a", QKind::Checkboxes, "Pick"), &["x", "y"])
    };
    assert!(q(boxes).is_some(), "more picks than options");
    assert!(q(ask("a b", QKind::Short, "Name")).is_some(), "bad id");
}

#[test]
fn answers_are_checked_and_stored_by_question_id() {
    let b = staff().body;
    let good = b.answers(&staff_answers()).unwrap();
    assert_eq!(good["q4"], "5 to 10");
    assert_eq!(good.len(), 5);

    let mut a = staff_answers();
    a.remove("q2");
    assert!(b.answers(&a).unwrap_err().contains("needs an answer"));
    a.insert("q2".into(), json!("  "));
    assert!(b.answers(&a).is_err(), "spaces are no answer");
    let mut a = staff_answers();
    a.insert("q1".into(), json!("x".repeat(51)));
    assert!(b.answers(&a).unwrap_err().contains("51"));
    let mut a = staff_answers();
    a.insert("q4".into(), json!("Always"));
    assert!(b.answers(&a).is_err(), "not one of the options");
    let mut a = staff_answers();
    a.insert("q4".into(), json!(["5 to 10"]));
    assert!(b.answers(&a).is_err(), "a list for a single choice");
    let mut a = staff_answers();
    a.insert("zz".into(), json!("extra"));
    assert!(
        !b.answers(&a).unwrap().contains_key("zz"),
        "unknown keys are dropped"
    );

    // The answer check runs on the whole answer.
    let p = Starter::PartnershipRequest.setup().body;
    let base = map(json!({"q1": "discord.gg/abc", "q2": "120", "q3": "Games", "q4": "Events"}));
    assert!(p.answers(&base).is_ok());
    let mut a = base.clone();
    a.insert("q2".into(), json!("120 members"));
    assert!(p.answers(&a).unwrap_err().contains("right form"));
    let mut a = base.clone();
    a.insert("q1".into(), json!("see discord.gg/abc"));
    assert!(p.answers(&a).is_err());

    // An optional question may stay empty and is not stored.
    let prize = Starter::PrizeClaim.setup().body;
    let got = prize.answers(&map(json!({"q1": "Name#1"}))).unwrap();
    assert_eq!(got.len(), 1);

    let boxes = Body {
        questions: vec![
            Question {
                max_picks: Some(2),
                ..options(ask("c", QKind::Checkboxes, "Games"), &["A", "B", "C"])
            },
            Question {
                max_picks: Some(2),
                ..ask("m", QKind::Member, "Who")
            },
            Question {
                description: "Read this first.".into(),
                ..ask("t", QKind::Text, "")
            },
        ],
        ..Body::default()
    };
    let ok = boxes
        .answers(&map(json!({"c": ["A", "C", "A"], "m": ["12", "34"]})))
        .unwrap();
    assert_eq!(ok["c"], json!(["A", "C"]), "repeats go");
    assert!(!ok.contains_key("t"), "a text block has no answer");
    assert!(boxes
        .answers(&map(json!({"c": ["A", "B", "C"], "m": ["1"]})))
        .is_err());
    assert!(boxes
        .answers(&map(json!({"c": ["D"], "m": ["1"]})))
        .is_err());
    assert!(boxes
        .answers(&map(json!({"c": ["A"], "m": ["me"]})))
        .is_err());
}

#[test]
fn the_hash_follows_the_questions_only() {
    let a = staff().body;
    let mut b = a.clone();
    b.ways.web = false;
    b.cooldown_secs = 600;
    assert_eq!(a.hash(), b.hash());
    let mut anon = a.clone();
    anon.anonymous = true;
    assert_ne!(
        a.hash(),
        anon.hash(),
        "a member who saw anonymous is not stored by ID"
    );
    b.questions[0].title = "Your time zone?".into();
    assert_ne!(a.hash(), b.hash());
    assert_eq!(a.hash().len(), 16);
}

#[test]
fn required_roles_all_or_one() {
    let (r1, r2) = (Id::new(1), Id::new(2));
    let mut req = Req {
        required: vec![r1, r2],
        ..Req::default()
    };
    let one = [r1];
    assert_eq!(refusal(&req, None, None, &facts(1 << 22, &one)), None);
    req.required_all = true;
    assert_eq!(
        refusal(&req, None, None, &facts(1 << 22, &one)),
        Some(Refusal::MissingRole)
    );
    assert_eq!(refusal(&req, None, None, &facts(1 << 22, &[r2, r1])), None);
    assert_eq!(
        Refusal::MissingRole.text_for("form"),
        "you do not have a role this form needs."
    );
    assert_eq!(
        Refusal::MissingRole.text(),
        "you do not have a role this giveaway needs."
    );
}

#[test]
fn a_setup_has_limits() {
    let now = Utc::now();
    let mut s = staff();
    s.name = " ".into();
    assert!(s.problem(now).is_some());
    let mut s = staff();
    s.opens_at = Some(now + chrono::TimeDelta::days(400));
    assert!(s.problem(now).unwrap().contains("1 year"));
    let mut s = staff();
    s.opens_at = Some(now + chrono::TimeDelta::days(2));
    s.closes_at = Some(now + chrono::TimeDelta::days(1));
    assert!(s.problem(now).unwrap().contains("close after"));
}

async fn db(offset: i64) -> Option<(PgPool, i64)> {
    let url = crate::db::test_url().ok()?;
    let pool = crate::db::connect(&url).await.unwrap();
    let gid = 7_300_000_000 + std::process::id() as i64 * 10 + offset;
    clean(&pool, gid).await;
    Some((pool, gid))
}

async fn clean(pool: &PgPool, gid: i64) {
    for q in [
        "DELETE FROM forms WHERE guild_id = $1",
        "DELETE FROM scheduled_jobs WHERE guild_id = $1",
    ] {
        sqlx::query(q).bind(gid).execute(pool).await.unwrap();
    }
}

async fn send(
    pool: &PgPool,
    gid: i64,
    form: i64,
    f: &Facts<'_>,
    a: &Map<String, Value>,
    seen: &str,
) -> Result<Sent, NotSent> {
    let s = Submission {
        guild: gid,
        form,
        facts: f,
        answers: a,
        via: Via::Popup,
        seen,
        started_at: None,
    };
    submit(pool, &s).await.unwrap()
}

async fn set(pool: &PgPool, sql: &str, id: i64) {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

/// Once only, a denied member may answer again, the cooldown, edits, closed and changed forms,
/// a timed-out member, another way, another server, and the queued bot job.
#[tokio::test]
async fn submit_follows_the_answering_rules() {
    let Some((pool, gid)) = db(1).await else {
        return;
    };
    let mut s = staff();
    let id = create(&pool, gid, 9, &s).await.unwrap().unwrap();
    let h = s.body.hash();
    let a = staff_answers();
    let me = facts(1 << 22, &[]);

    assert_eq!(
        send(&pool, gid + 1, id, &me, &a, &h).await,
        Err(NotSent::Gone),
        "another server"
    );
    sqlx::query("INSERT INTO form_drafts (form_id, user_id, via, answers, expires_at) VALUES ($1, $2, 'popup', '{}', now() + interval '30 minutes')")
        .bind(id)
        .bind((1u64 << 22) as i64)
        .execute(&pool)
        .await
        .unwrap();
    let first = send(&pool, gid, id, &me, &a, &h).await.unwrap();
    let Sent::New { id: r1, number: 1 } = first else {
        panic!("{first:?}")
    };
    let drafts: i64 = sqlx::query_scalar("SELECT count(*) FROM form_drafts WHERE form_id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(drafts, 0, "a send clears the draft");
    let job: Value = sqlx::query_scalar(
        "SELECT payload FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'form_response'",
    )
    .bind(gid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(job, json!({"id": r1, "edited": false}));
    assert_eq!(
        send(&pool, gid, id, &me, &a, &h).await,
        Err(NotSent::Already)
    );

    // Denied: the member may answer again, as number 2.
    set(
        &pool,
        "UPDATE form_responses SET status = 'denied' WHERE id = $1",
        r1,
    )
    .await;
    let again = send(&pool, gid, id, &me, &a, &h).await.unwrap();
    assert!(matches!(again, Sent::New { number: 2, .. }), "{again:?}");

    // Edit on: answering again replaces the open response and keeps its number.
    s.body.edit = true;
    update(&pool, gid, id, &s).await.unwrap().unwrap();
    let mut changed = a.clone();
    changed.insert("q1".into(), json!("UTC"));
    let edit = send(&pool, gid, id, &me, &changed, &h).await.unwrap();
    assert!(matches!(edit, Sent::Edited { number: 2, .. }), "{edit:?}");
    let stored: Value =
        sqlx::query_scalar("SELECT answers FROM form_responses WHERE form_id = $1 AND number = 2")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored["q1"], "UTC");

    // With a cooldown, an edit waits too; decided, there is no edit and the cooldown holds.
    s.body.cooldown_secs = 3600;
    update(&pool, gid, id, &s).await.unwrap().unwrap();
    assert!(matches!(
        send(&pool, gid, id, &me, &a, &h).await,
        Err(NotSent::Cooldown(_))
    ));
    set(
        &pool,
        "UPDATE form_responses SET status = 'denied' WHERE form_id = $1",
        id,
    )
    .await;
    assert!(matches!(
        send(&pool, gid, id, &me, &a, &h).await,
        Err(NotSent::Cooldown(_))
    ));

    // A timeout blocks every form (D7), and a bad answer says which question.
    let other = facts((1 << 22) + 1, &[]);
    let mut timed_out = facts((1 << 22) + 1, &[]);
    timed_out.timed_out = true;
    assert_eq!(
        send(&pool, gid, id, &timed_out, &a, &h).await,
        Err(NotSent::Refused(Refusal::TimedOut))
    );
    let mut bad = a.clone();
    bad.remove("q5");
    assert!(matches!(
        send(&pool, gid, id, &other, &bad, &h).await,
        Err(NotSent::Bad(_))
    ));

    // The questions changed since the member saw them.
    s.body.questions[0].title = "Your time zone?".into();
    update(&pool, gid, id, &s).await.unwrap().unwrap();
    assert_eq!(
        send(&pool, gid, id, &other, &a, &h).await,
        Err(NotSent::Changed)
    );
    let h = s.body.hash();
    assert!(send(&pool, gid, id, &other, &a, &h).await.is_ok());

    // Another way to answer that is off.
    s.body.ways.dm = false;
    update(&pool, gid, id, &s).await.unwrap().unwrap();
    let dm = Submission {
        guild: gid,
        form: id,
        facts: &other,
        answers: &a,
        via: Via::Dm,
        seen: &h,
        started_at: None,
    };
    assert_eq!(submit(&pool, &dm).await.unwrap(), Err(NotSent::WayOff));

    // Closed, and closed until it opens.
    s.open = false;
    update(&pool, gid, id, &s).await.unwrap().unwrap();
    assert_eq!(
        send(&pool, gid, id, &other, &a, &h).await,
        Err(NotSent::Closed)
    );
    s.open = true;
    s.opens_at = Some(Utc::now() + chrono::TimeDelta::hours(1));
    update(&pool, gid, id, &s).await.unwrap().unwrap();
    assert!(matches!(
        send(&pool, gid, id, &other, &a, &h).await,
        Err(NotSent::NotYet(_))
    ));
    clean(&pool, gid).await;
}

/// An anonymous form stores no user ID, but still knows "once only" for the member.
#[tokio::test]
async fn an_anonymous_form_keeps_no_user_id() {
    let Some((pool, gid)) = db(2).await else {
        return;
    };
    let mut s = Starter::PrizeClaim.setup();
    s.body.anonymous = true;
    let id = create(&pool, gid, 9, &s).await.unwrap().unwrap();
    let h = s.body.hash();
    let a = map(json!({"q1": "Name#1"}));
    let me = facts(1 << 22, &[]);
    assert!(send(&pool, gid, id, &me, &a, &h).await.is_ok());
    assert_eq!(
        send(&pool, gid, id, &me, &a, &h).await,
        Err(NotSent::Already)
    );
    let other = facts((1 << 22) + 5, &[]);
    assert!(send(&pool, gid, id, &other, &a, &h).await.is_ok());
    let rows: Vec<(Option<i64>, Option<String>, String)> = sqlx::query_as(
        "SELECT user_id, user_key, status FROM form_responses WHERE form_id = $1 ORDER BY number",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.0.is_none() && r.2 == "collected"));
    assert_ne!(rows[0].1, rows[1].1, "one key per member");
    assert_eq!(rows[0].1.as_ref().unwrap().len(), 64);
    clean(&pool, gid).await;
}

/// The open and close jobs set the switch, a moved time makes the old job do nothing, and a
/// server has at most 50 forms.
#[tokio::test]
async fn open_and_close_jobs_and_the_form_limit() {
    let Some((pool, gid)) = db(3).await else {
        return;
    };
    let mut s = staff();
    let at = Utc::now() + chrono::TimeDelta::hours(1);
    s.opens_at = Some(at);
    s.closes_at = Some(at + chrono::TimeDelta::hours(1));
    let id = create(&pool, gid, 9, &s).await.unwrap().unwrap();
    let f = get(&pool, gid, id).await.unwrap().unwrap();
    assert!(!f.open, "closed until it opens");
    let jobs: Vec<(String, Value)> = sqlx::query_as(
        "SELECT kind, payload FROM scheduled_jobs WHERE guild_id = $1 ORDER BY kind",
    )
    .bind(gid)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(jobs.len(), 2);
    let open = &jobs.iter().find(|j| j.0 == OPEN_JOB).unwrap().1;
    let close = &jobs.iter().find(|j| j.0 == CLOSE_JOB).unwrap().1;
    // The same times again queue nothing new.
    update(&pool, gid, id, &s).await.unwrap().unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM scheduled_jobs WHERE guild_id = $1")
        .bind(gid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 2);

    time_job(&pool, gid + 1, OPEN_JOB, open).await.unwrap();
    assert!(
        !get(&pool, gid, id).await.unwrap().unwrap().open,
        "other server"
    );
    time_job(&pool, gid, OPEN_JOB, open).await.unwrap();
    assert!(get(&pool, gid, id).await.unwrap().unwrap().open);
    time_job(&pool, gid, CLOSE_JOB, close).await.unwrap();
    assert!(!get(&pool, gid, id).await.unwrap().unwrap().open);
    // Moved: the old open job does nothing.
    s.opens_at = Some(at + chrono::TimeDelta::minutes(30));
    update(&pool, gid, id, &s).await.unwrap().unwrap();
    set(&pool, "UPDATE forms SET open = false WHERE id = $1", id).await;
    time_job(&pool, gid, OPEN_JOB, open).await.unwrap();
    assert!(!get(&pool, gid, id).await.unwrap().unwrap().open);

    // Switched off by staff: the open job never turns it on.
    s.open = false;
    update(&pool, gid, id, &s).await.unwrap().unwrap();
    let f = get(&pool, gid, id).await.unwrap().unwrap();
    assert_eq!((f.open, f.opens_at), (false, None));

    for _ in 1..MAX_FORMS {
        create(&pool, gid, 9, &staff()).await.unwrap().unwrap();
    }
    let over = create(&pool, gid, 9, &staff()).await.unwrap();
    assert!(over.unwrap_err().contains("limit is 50"));
    clean(&pool, gid).await;
}

/// Old drafts go, and above the cap the oldest decided responses go while open ones stay.
#[tokio::test]
async fn prune_keeps_the_cap_and_open_responses() {
    let Some((pool, gid)) = db(4).await else {
        return;
    };
    let id = create(&pool, gid, 9, &staff()).await.unwrap().unwrap();
    // 10 open ones first, then decided ones: 5 over the cap.
    sqlx::query(
        "INSERT INTO form_responses (form_id, guild_id, number, user_id, status, answers, via)
         SELECT $1, $2, n, n, CASE WHEN n <= 10 THEN 'open' ELSE 'denied' END, '{}', 'web'
         FROM generate_series(1, $3) n",
    )
    .bind(id)
    .bind(gid)
    .bind((MAX_RESPONSES + 5) as i32)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO form_drafts (form_id, user_id, via, answers, expires_at) VALUES
         ($1, 1, 'dm', '{}', now() - interval '1 minute'), ($1, 2, 'dm', '{}', now() + interval '1 minute')",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    prune(&pool).await.unwrap();
    let left: Vec<i32> = sqlx::query_scalar(
        "SELECT number FROM form_responses WHERE form_id = $1 ORDER BY number LIMIT 12",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(left, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 16, 17]);
    let drafts: Vec<i64> = sqlx::query_scalar("SELECT user_id FROM form_drafts WHERE form_id = $1")
        .bind(id)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(drafts, [2]);
    clean(&pool, gid).await;
}

/// 10,000 members send a 5-question form at once, and staff change the questions half way.
/// Every member ends with one response, numbers have no gaps or repeats, and a send with the old
/// questions is refused, not stored. Times go to stderr.
#[tokio::test]
async fn ten_thousand_members_send_one_form() {
    let Some((_, gid)) = db(5).await else {
        return;
    };
    // Same size as the real pool, but 10,000 tasks queue for 10 connections at once, so the
    // default 30 s wait for a connection is a limit of the runner's speed, not of the engine.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(std::time::Duration::from_secs(600))
        .connect(&crate::db::test_url().unwrap())
        .await
        .unwrap();
    let s = staff();
    let id = create(&pool, gid, 9, &s).await.unwrap().unwrap();
    let old = s.body.hash();
    let mut edited = s.clone();
    edited.body.questions[0].title = "Your time zone?".into();
    let new = edited.body.hash();
    let a = std::sync::Arc::new(staff_answers());
    let t = std::time::Instant::now();
    // The last 1,000 members opened the form before the edit and send after it.
    let (edited_now, edit_seen) = tokio::sync::watch::channel(false);
    let sends: Vec<_> = (0..10_000u64)
        .map(|n| {
            let (pool, a, old, new) = (pool.clone(), a.clone(), old.clone(), new.clone());
            let mut late = (n >= 9_000).then(|| edit_seen.clone());
            tokio::spawn(async move {
                if let Some(w) = late.as_mut() {
                    w.wait_for(|edited| *edited).await.unwrap();
                }
                let f = facts((1 << 22) + n, &[]);
                let mut changed = false;
                loop {
                    let seen = if changed { &new } else { &old };
                    match send(&pool, gid, id, &f, &a, seen).await {
                        Err(NotSent::Changed) if !changed => changed = true,
                        r => return (r, changed),
                    }
                }
            })
        })
        .collect();
    // Half way, staff save new questions.
    while sqlx::query_scalar::<_, i32>("SELECT next_number FROM forms WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap()
        < 5_000
    {
        tokio::task::yield_now().await;
    }
    update(&pool, gid, id, &edited).await.unwrap().unwrap();
    edited_now.send(true).unwrap();
    let (mut ok, mut again) = (0, 0);
    for s in sends {
        let (r, changed) = s.await.unwrap();
        assert!(matches!(r, Ok(Sent::New { .. })), "{r:?}");
        ok += 1;
        again += usize::from(changed);
    }
    let took = t.elapsed();
    eprintln!(
        "10000 sends: {took:?} ({:?} each on average); {again} started again after the edit",
        took / 10_000
    );
    assert_eq!(ok, 10_000);
    assert!(
        again >= 1_000,
        "members who saw the old questions start again"
    );
    let (n, lo, hi): (i64, i32, i32) = sqlx::query_as(
        "SELECT count(DISTINCT number), min(number), max(number) FROM form_responses WHERE form_id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((n, lo, hi), (10_000, 1, 10_000));
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM scheduled_jobs WHERE guild_id = $1 AND kind = 'form_response'",
    )
    .bind(gid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(jobs, 10_000);
    assert!(
        took < std::time::Duration::from_secs(600),
        "inside 10 minutes"
    );
    clean(&pool, gid).await;
}
