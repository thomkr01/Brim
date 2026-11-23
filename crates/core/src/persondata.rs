//! Where one person's data lives, for their own requests to see or delete it (security review,
//! s02:t-pv-rights-channel). The bot does a request after the bot owner approved it on the
//! dashboard; the dashboard shows the bot owner these counts first.
//!
//! Only data about the person as a member is deleted. Kept, and shown to them: the Raider List (it
//! has its own appeal), appeal and report blacklists (they stop abuse), staff lists (changed on the
//! Permits page) and a live quarantine (it ends with its release). What the person did as staff
//! (cases they gave, notes they wrote) is neither shown nor deleted here.

/// One table: `col` holds the person's user id.
pub struct Place {
    pub table: &'static str,
    pub col: &'static str,
    /// Columns left out of what the person is sent: other people (staff, reporters) and secrets.
    pub hide: &'static [&'static str],
    /// Extra SQL condition for the delete, e.g. open tickets stay.
    pub keep: Option<&'static str>,
    pub delete: bool,
    pub show: bool,
}

const fn p(table: &'static str, col: &'static str, hide: &'static [&'static str]) -> Place {
    Place {
        table,
        col,
        hide,
        keep: None,
        delete: true,
        show: true,
    }
}

/// Kept on a delete request, but shown.
const fn kept(table: &'static str, col: &'static str, hide: &'static [&'static str]) -> Place {
    Place {
        delete: false,
        ..p(table, col, hide)
    }
}

/// The delete condition of `cases`, shared by a request and by [`sweep_kept`].
const CASE_KEEP: &str = "case_no < (SELECT max(case_no) FROM cases m WHERE m.guild_id = t.guild_id)
            AND NOT COALESCE(active AND expires_at > now(), false)";

pub const PLACES: &[Place] = &[
    Place {
        // The server's newest case stays, without the person (see `ANONYMIZE`), so its number is
        // never given again (`cases::create` reads MAX(case_no)).
        // A warn that still counts stays, with the person, until it ends (owner decision 2026-11-19:
        // "Warns stay until they expire"); `sweep_kept` removes it then.
        keep: Some(CASE_KEEP),
        // `proof`: the column from before migration 4037 (staff proof; the file leaves proofs out).
        ..p("cases", "user_id", &["mod_id", "proof"])
    },
    // Note text is left out of the file until owner decision decides (staff write notes privately).
    p("notes", "user_id", &["author_id", "text"]),
    Place {
        // A report staff have not handled yet stays until they do.
        keep: Some("status <> 'open'"),
        // The reporter's reason can name them.
        ..p("reports", "target_id", &["reporter_id", "handled_by", "reason"])
    },
    Place {
        keep: Some("status <> 'open'"),
        // What staff did to the other member is not the reporter's data.
        ..p("reports", "reporter_id", &["handled_by", "action"])
    },
    Place {
        // An appeal staff have not decided yet stays until they do.
        keep: Some("status <> 'open'"),
        ..p("appeals", "user_id", &["decided_by", "log_channel", "log_message"])
    },
    Place {
        // An open ticket still has its channel; it goes once closed. The newest stays, like cases.
        keep: Some("closed_at IS NOT NULL AND number < (SELECT max(number) FROM tickets m WHERE m.guild_id = t.guild_id)"),
        ..p("tickets", "user_id", &["closed_by", "claimed_by"])
    },
    p("ticket_users", "user_id", &[]),
    p("automod_strikes", "user_id", &[]),
    p("member_roles", "user_id", &[]),
    p("invite_joins", "user_id", &["inviter_id"]),
    p("invite_members", "user_id", &["inviter_id"]),
    p("joingate_actions", "user_id", &["adder_id"]),
    p("raid_log", "user_id", &[]),
    p("giveaway_entries", "user_id", &[]),
    p("giveaway_claims", "user_id", &[]),
    p("giveaway_code_tries", "user_id", &[]),
    p("verify_ips", "user_id", &["ip_hash"]),
    p("verify_timeouts", "user_id", &[]),
    Place {
        // A "Not NSFW" row is the server's allow for that image (PITFALLS: user decisions stay).
        keep: Some("feedback IS DISTINCT FROM 'fp'"),
        ..p("nsfw_scans", "user_id", &["sha256", "feedback_by"])
    },
    p("mod_actions", "target_id", &["mod_id", "audit_id"]),
    p("message_runs", "member_id", &[]),
    p("message_tag_runs", "member_id", &[]),
    // The last seen name and avatar, kept for the avatar and name change logs (migration 4044).
    p("profile_snapshot", "user_id", &[]),
    Place {
        show: false,
        ..p("dash_session", "user_id", &[])
    },
    // Recent wrong 2FA codes and rescue claim tries (an hour at most): deleting them lifts a lockout.
    Place {
        show: false,
        ..kept("dash_fails", "user_id", &[])
    },
    // A credential, not member data: deleting it would let whoever holds the Discord account
    // enrol a new authenticator by DM code and pass 2FA (security pass, batch 8).
    Place {
        show: false,
        ..kept("dash_totp", "user_id", &[])
    },
    kept("appeal_blacklist", "user_id", &["added_by"]),
    kept("report_blacklist", "user_id", &["added_by"]),
    kept("quarantines", "user_id", &[]),
    kept("extra_owners", "user_id", &[]),
    kept("trusted_admins", "user_id", &[]),
    kept("permit_members", "user_id", &["added_by"]),
    kept("raiders", "user_id", &["reviewed_by"]),
    kept(
        "raider_evidence",
        "user_id",
        // Which servers reported them stays hidden, as on the Raider List status page.
        &["source_owner", "source_guild", "ref", "reporter_id", "note", "detail"],
    ),
    kept("raider_appeals", "user_id", &["decided_by"]),
];

/// `SELECT count(*)` of the rows a request would touch; $1 the user id.
pub fn count_sql(p: &Place) -> String {
    format!("SELECT count(*) FROM {} WHERE {} = $1", p.table, p.col)
}

/// The person's rows as JSON without the hidden columns; $1 the user id. At most 5,000 per table.
pub fn show_sql(p: &Place) -> String {
    let hide: String = p.hide.iter().map(|c| format!(" - '{c}'")).collect();
    format!(
        "SELECT COALESCE(jsonb_agg(r), '[]') FROM (SELECT to_jsonb(t){hide} AS r FROM {} t WHERE {} = $1 LIMIT 5000) x",
        p.table, p.col
    )
}

/// The DELETE for a deleted place; $1 the user id.
pub fn delete_sql(p: &Place) -> Option<String> {
    p.delete.then(|| {
        let keep = p.keep.map(|k| format!(" AND {k}")).unwrap_or_default();
        format!("DELETE FROM {} t WHERE {} = $1{keep}", p.table, p.col)
    })
}

/// Run after the deletes: the rows a delete keeps for their number or as a user decision lose the
/// person; $1 the user id.
pub const ANONYMIZE: &[&str] = &[
    // The case's proofs and edit history go with the person: trigger `case_forget_person` (4037).
    // Not a warn that still counts: without the person its points would be gone at once.
    "UPDATE cases SET user_id = 0, reason = NULL, proof = NULL WHERE user_id = $1
     AND NOT COALESCE(active AND expires_at > now(), false)",
    "UPDATE tickets SET user_id = 0 WHERE user_id = $1 AND closed_at IS NOT NULL",
    // A "Not NSFW" row stays as the server's allow for that image.
    "UPDATE nsfw_scans SET user_id = NULL WHERE user_id = $1",
];

/// Older config versions name the person (a removed staff member, a blocked ticket user): their id
/// becomes "0" there. The current version stays, so nothing changes in what the bot does; $1 the
/// id as text with its quotes, e.g. `"123"`.
pub const SCRUB_CONFIG_HISTORY: &str =
    "UPDATE guild_config c SET config = replace(c.config::text, $1, '\"0\"')::jsonb
     WHERE strpos(c.config::text, $1) > 0
       AND c.version < (SELECT max(version) FROM guild_config m WHERE m.guild_id = c.guild_id)";

/// What a delete request keeps until it ends, with the column that says when the row was made:
/// a warn that still counts, an open report or appeal, an open ticket.
const KEPT_UNTIL_ENDED: &[(&str, &str, &str)] = &[
    ("cases", "user_id", "created_at"),
    ("reports", "target_id", "created_at"),
    ("reports", "reporter_id", "created_at"),
    ("appeals", "user_id", "created_at"),
    ("tickets", "user_id", "opened_at"),
];

/// Finishes done delete requests (owner decision 2026-11-19): a row the request kept until it ended (see
/// [`KEPT_UNTIL_ENDED`]) is deleted once it has ended, with the request's own keep condition, and
/// a server's newest case or ticket then loses the person, as [`ANONYMIZE`] does. Only rows from
/// before the request: later ones are new data. Every done request, with no time window: a
/// restored backup or a long stop can bring an ended row back long after its request. Migration
/// 4040 has the indexes that find the rows.
pub async fn sweep_kept(pool: &sqlx::PgPool) -> sqlx::Result<u64> {
    let asked = |col: &str, made: &str| {
        format!(
            "EXISTS (SELECT 1 FROM data_requests r WHERE r.kind = 'delete' AND r.status = 'done'
                     AND r.user_id = t.{col} AND t.{made} < r.done_at)"
        )
    };
    let mut sql = vec![];
    for (table, col, made) in KEPT_UNTIL_ENDED {
        let place = PLACES.iter().find(|p| p.table == *table && p.col == *col);
        let keep = place
            .and_then(|p| p.keep)
            .expect("a kept place has a keep condition");
        let asked = asked(col, made);
        sql.push(format!("DELETE FROM {table} t WHERE {asked} AND {keep}"));
    }
    sql.push(format!(
        "UPDATE cases t SET user_id = 0, reason = NULL, proof = NULL WHERE {}
         AND NOT COALESCE(active AND expires_at > now(), false)",
        asked("user_id", "created_at")
    ));
    sql.push(format!(
        "UPDATE tickets t SET user_id = 0 WHERE {} AND closed_at IS NOT NULL",
        asked("user_id", "opened_at")
    ));
    let mut tx = pool.begin().await?;
    let mut n = 0;
    for q in sql {
        n += sqlx::query(sqlx::AssertSqlSafe(q))
            .execute(&mut *tx)
            .await?
            .rows_affected();
    }
    tx.commit().await?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_is_built_from_the_list_only() {
        let t = PLACES.iter().find(|p| p.table == "tickets").unwrap();
        assert_eq!(
            delete_sql(t).unwrap(),
            "DELETE FROM tickets t WHERE user_id = $1 AND closed_at IS NOT NULL AND number < (SELECT max(number) FROM tickets m WHERE m.guild_id = t.guild_id)"
        );
        assert!(show_sql(&PLACES[0]).contains("to_jsonb(t) - 'mod_id' - 'proof' AS r"));
        assert!(PLACES
            .iter()
            .filter(|p| p.table.starts_with("raider") || p.table.ends_with("blacklist"))
            .all(|p| delete_sql(p).is_none()));
    }
}
