//! Self-host updates: the bot owner picks a published release here and the updater container
//! (`scripts/updater.sh`) installs exactly that build (by image digest), writing its progress to
//! `update_runs`. The dashboard never touches Docker itself.
use crate::{api::Res, auth::Sess, ApiError, AppState};
use axum::{extract::State, routing::get, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

/// Set once at start: this server still runs under the names from before the rename to Brim
/// (`brim_core::db::old_names`), in `~/tommybot`, with the `tommybot-<img>` images. No new builds
/// come under those names, so the Update button refuses until its owner moves it to the Brim names.
pub static OLD_NAMES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

fn old_names() -> bool {
    OLD_NAMES.get().copied().unwrap_or(false)
}

/// The GitHub API of the repository whose builds this server installs.
fn api(path: &str) -> String {
    format!("https://api.github.com/repos/thomkr01/Brim/{path}")
}

/// The package of image `img`.
fn package(img: &str) -> String {
    format!("thomkr01/brim-{img}")
}

/// The Images runs, newest first. Only `workflow_run` runs: those run images.yml as it is on main,
/// whatever branch or tag started them. Not `status=success`: GitHub's filtered list left out a
/// finished run for over 15 minutes, so the page showed an older build as the newest (PITFALLS).
const LATEST_PATH: &str = "actions/workflows/images.yml/runs?event=workflow_run&per_page=100";
/// GHCR keeps the 10 most recent versions of each image, one per release.
const KEPT: usize = 10;
/// The only repository a build record may name.
const REPO: &str = "https://github.com/thomkr01/Brim";
/// The images the updater installs (mcp only where it runs).
const IMAGES: [&str; 3] = ["bot", "dashboard", "mcp"];
/// Files that reach the server only with the command there, not with the Update button.
const SIDE: [&str; 2] = ["scripts/updater.sh", "docker-compose.yml"];
const UA: &str = "Brim-dashboard";
/// Seconds per step until an update has finished once: check, download, database backup and restart
/// bot, restart dashboard, health check.
const DEFAULT_STEPS: [i32; 5] = [2, 60, 20, 10, 20];

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/update", get(status).post(start))
}

pub async fn is_bot_owner(st: &AppState, s: &Sess) -> bool {
    st.discord
        .bot_owner()
        .await
        .is_ok_and(|o| o == s.user_id.to_string())
}

async fn owner(st: &AppState, s: &Sess) -> Result<(), ApiError> {
    if is_bot_owner(st, s).await {
        Ok(())
    } else {
        Err(ApiError::Forbidden("only the bot owner can update Brim"))
    }
}

/// A GET to GitHub or the registry, as JSON; None on any failure.
async fn get_json(st: &AppState, url: &str) -> Option<Value> {
    let r = st.discord.http.get(url).header("User-Agent", UA);
    r.send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()
}

/// The published releases, newest first ({sha, version, at}), cached for a minute (GitHub allows 60 calls an
/// hour without a token). Empty when GitHub cannot be reached; a failure is not cached.
async fn builds(st: &AppState) -> Vec<Value> {
    static CACHE: Mutex<Option<(Instant, Vec<Value>)>> = Mutex::new(None);
    if let Some((at, v)) = CACHE.lock().unwrap().clone() {
        if at.elapsed() < Duration::from_secs(60) {
            return v;
        }
    }
    let Some(r) = get_json(st, &api(LATEST_PATH)).await else {
        return vec![];
    };
    let v = runs_to_builds(&r);
    *CACHE.lock().unwrap() = Some((Instant::now(), v.clone()));
    v
}

/// Each successful run's release once (a re-run publishes the same release again), highest version
/// first (a re-run of an older release is not the newest), at most `KEPT`. Builds of main from
/// before releases ("Images for <sha>") are not listed.
fn runs_to_builds(r: &Value) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
    let mut v: Vec<(Vec<u64>, Value)> = r["workflow_runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|run| run["conclusion"] == "success")
        .filter_map(|run| {
            let (version, sha) = published(run["display_title"].as_str()?)?;
            let key = version[1..]
                .split('.')
                .map(|n| n.parse().unwrap_or(u64::MAX))
                .collect();
            seen.insert(sha).then(|| {
                (
                    key,
                    json!({"sha": sha, "version": version, "at": run["created_at"]}),
                )
            })
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.into_iter().take(KEPT).map(|(_, b)| b).collect()
}

/// The release and commit an Images run published, from its title ("Release v1.2.3 at <sha>").
/// Not the run's head_sha: for a run started by CI finishing, that is main's tip when it was
/// queued, so an older commit whose CI finished last showed as the newest (PITFALLS: update loop).
fn published(title: &str) -> Option<(&str, &str)> {
    let (version, sha) = title.strip_prefix("Release ")?.split_once(" at ")?;
    (is_version(version) && is_sha(sha)).then_some((version, sha))
}

/// A release tag: v, then three numbers (v1.2.3).
fn is_version(v: &str) -> bool {
    v.strip_prefix('v').is_some_and(|n| {
        let parts: Vec<&str> = n.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    })
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Which of the `SIDE` files changed from the updater's commit to `latest`; None when GitHub cannot
/// say. Only the last pair is kept, like `newer`.
async fn behind(st: &AppState, updater: &str, latest: &str) -> Option<Vec<&'static str>> {
    static LAST: Mutex<Option<(String, String, Vec<&'static str>)>> = Mutex::new(None);
    if updater == latest {
        return Some(vec![]);
    }
    if !is_sha(updater) || !is_sha(latest) {
        return None;
    }
    if let Some((u, l, f)) = LAST.lock().unwrap().clone() {
        if u == updater && l == latest {
            return Some(f);
        }
    }
    let r = get_json(st, &api(&format!("compare/{updater}...{latest}"))).await?;
    let f = side_changes(&r)?;
    *LAST.lock().unwrap() = Some((updater.into(), latest.into(), f.clone()));
    Some(f)
}

/// The `SIDE` files a compare touches. An updater newer than `latest` lags behind nothing; a file
/// list GitHub cut short (it lists at most 300) could miss one, so None.
fn side_changes(compare: &Value) -> Option<Vec<&'static str>> {
    if !matches!(compare["status"].as_str()?, "ahead" | "diverged") {
        return Some(vec![]);
    }
    let files = compare["files"].as_array()?;
    if files.len() >= 300 {
        return None;
    }
    Some(
        SIDE.into_iter()
            .filter(|f| files.iter().any(|x| x["filename"] == *f))
            .collect(),
    )
}

/// The registry digest of each image of release `version`, accepted only when GitHub holds the
/// signed build record (attestation) the Images workflow made for it. The updater pulls these
/// digests and nothing else, so a tag moved in the registry after the owner chose cannot reach the
/// server. Looked up by the version tag, not the sha tag: two releases on one commit share that.
async fn approved(st: &AppState, version: &str) -> Result<Value, String> {
    let short = version;
    let unreachable = |what: &str| {
        format!("Could not reach {what} to check release {short}. Nothing was started; try again in a minute.")
    };
    let mut out = serde_json::Map::new();
    for img in IMAGES {
        let repo = package(img);
        let token = get_json(
            st,
            &format!("https://ghcr.io/token?scope=repository:{repo}:pull"),
        )
        .await
        .and_then(|v| Some(v["token"].as_str()?.to_string()))
        .ok_or_else(|| unreachable("the image registry"))?;
        let r = st
            .discord
            .http
            .head(format!("https://ghcr.io/v2/{repo}/manifests/{version}"))
            .bearer_auth(token)
            .header("User-Agent", UA)
            .header(
                "Accept",
                "application/vnd.oci.image.manifest.v1+json, application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.v2+json",
            )
            .send()
            .await
            .map_err(|_| unreachable("the image registry"))?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(format!("The {img} image of release {short} is no longer in the registry (it keeps the 10 most recent releases)."));
        }
        let digest = r
            .error_for_status()
            .ok()
            .and_then(|r| {
                Some(
                    r.headers()
                        .get("docker-content-digest")?
                        .to_str()
                        .ok()?
                        .to_string(),
                )
            })
            .filter(|d| is_digest(d))
            .ok_or_else(|| unreachable("the image registry"))?;
        // shortcut: trusts GitHub's list of this repository's attestations, without checking the
        // Sigstore signature itself; check the bundle when the GitHub account is the threat.
        let r = st
            .discord
            .http
            .get(api(&format!("attestations/{digest}")))
            .header("User-Agent", UA)
            .send()
            .await
            .map_err(|_| unreachable("GitHub"))?;
        let signed = match r.status() {
            reqwest::StatusCode::NOT_FOUND => false,
            st if st.is_success() => r
                .json::<Value>()
                .await
                .is_ok_and(|v| attests(&v, &format!("ghcr.io/{repo}"), &digest)),
            _ => return Err(unreachable("GitHub")),
        };
        if !signed {
            return Err(format!("The {img} image of release {short} has no signed build record on GitHub, so it is not installed."));
        }
        out.insert(img.into(), json!(digest));
    }
    Ok(Value::Object(out))
}

/// Whether one of the attestations GitHub lists says the Images workflow on main of this repository
/// built `digest` as image `name` (`ghcr.io/<package>`): the same bytes copied into another image's
/// package (bot running the updater's image) or a record from any other workflow do not count.
fn attests(list: &Value, name: &str, digest: &str) -> bool {
    use base64::Engine;
    let hex = digest.trim_start_matches("sha256:");
    list["attestations"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|a| {
            let Some(p) = a["bundle"]["dsseEnvelope"]["payload"].as_str() else {
                return false;
            };
            let Some(st) = base64::engine::general_purpose::STANDARD
                .decode(p)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            else {
                return false;
            };
            let w = &st["predicate"]["buildDefinition"]["externalParameters"]["workflow"];
            st["subject"].as_array().is_some_and(|s| {
                s.iter()
                    .any(|x| x["name"] == name && x["digest"]["sha256"] == hex)
            }) && w["repository"] == REPO
                && w["path"] == ".github/workflows/images.yml"
                && w["ref"] == "refs/heads/main"
        })
}

fn is_digest(d: &str) -> bool {
    d.strip_prefix("sha256:")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// The most changes listed under "Newest"; the rest are behind "See what's new".
const MAX_CHANGES: usize = 10;

/// What one commit brought in: one line, and a pull request number (`pr`) only when it has one. A
/// merge commit ("Merge pull request #N from ...", the title on the next line), a squash commit
/// ("Title (#N)"), or a plain commit (its subject, no `pr`). Text from GitHub: one line, capped
/// (shown as plain text).
fn commit_change(c: &Value) -> Option<Value> {
    let msg = c["commit"]["message"].as_str()?;
    let first = msg.lines().next().unwrap_or("");
    let (pr, what) = match c["parents"].as_array().map_or(0, Vec::len) {
        // Only real merges: a branch commit can carry the same words.
        2 => {
            let pr = first.strip_prefix("Merge pull request #")?;
            let pr: u64 = pr
                .split(|ch: char| !ch.is_ascii_digit())
                .next()?
                .parse()
                .ok()?;
            let what = msg
                .lines()
                .skip(1)
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            (pr, what)
        }
        1 => match first
            .strip_suffix(')')
            .and_then(|f| f.rsplit_once(" (#"))
            .and_then(|(t, n)| Some((n.parse::<u64>().ok()?, t)))
        {
            Some((n, title)) => (n, title.trim()),
            None => {
                let what = first.trim();
                return (!what.is_empty()).then(
                    || json!({"sha": c["sha"], "what": what.chars().take(200).collect::<String>()}),
                );
            }
        },
        _ => return None,
    };
    Some(json!({"sha": c["sha"], "pr": pr, "what": what.chars().take(200).collect::<String>()}))
}

/// What each commit in a compare did, newest first. GitHub lists at most 250
/// commits, oldest first: a cut list would miss the newest, so none then.
fn changes(compare: &Value) -> Vec<Value> {
    let commits = compare["commits"].as_array().map_or(0, Vec::len) as u64;
    if compare["total_commits"].as_u64().unwrap_or(0) > commits {
        return vec![];
    }
    let mut out: Vec<Value> = compare["commits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(commit_change)
        .collect();
    out.reverse();
    out
}

/// The changes each published build brought: those after the build before it up to its own
/// commit, newest first, keyed by build. One compare (oldest build to newest) covers them all.
fn changes_per_build(
    compare: &Value,
    shas: &[&str],
) -> std::collections::HashMap<String, Vec<Value>> {
    let mut out = std::collections::HashMap::new();
    let commits = compare["commits"].as_array().map_or(0, Vec::len) as u64;
    if compare["total_commits"].as_u64().unwrap_or(0) > commits {
        return out;
    }
    let mut pending: Vec<Value> = vec![];
    for c in compare["commits"].as_array().into_iter().flatten() {
        pending.extend(commit_change(c));
        if let Some(sha) = c["sha"].as_str().filter(|s| shas.contains(s)) {
            pending.reverse();
            out.insert(sha.to_string(), std::mem::take(&mut pending));
        }
    }
    out
}

/// `builds` with each build's `changes`; the oldest has none (its base is not in the list). One
/// GitHub call per new pair of oldest and newest build.
async fn with_changes(st: &AppState, builds: Vec<Value>) -> Vec<Value> {
    type PerBuild = std::collections::HashMap<String, Vec<Value>>;
    static LAST: Mutex<Option<(String, String, PerBuild)>> = Mutex::new(None);
    let shas: Vec<&str> = builds.iter().filter_map(|b| b["sha"].as_str()).collect();
    let (Some(new), Some(old)) = (shas.first(), shas.last()) else {
        return builds;
    };
    if old == new || !shas.iter().all(|s| is_sha(s)) {
        return builds;
    }
    let cached = LAST
        .lock()
        .unwrap()
        .clone()
        .filter(|(o, n, _)| o == old && n == new);
    let per = match cached {
        Some((_, _, m)) => m,
        None => {
            let Some(r) = get_json(st, &api(&format!("compare/{old}...{new}"))).await else {
                return builds;
            };
            let m = changes_per_build(&r, &shas);
            *LAST.lock().unwrap() = Some((old.to_string(), new.to_string(), m.clone()));
            m
        }
    };
    builds
        .into_iter()
        .map(|mut b| {
            if let Some(c) = b["sha"].as_str().and_then(|s| per.get(s)) {
                b["changes"] = json!(c);
            }
            b
        })
        .collect()
}

/// Whether `latest` is newer than `current`, from GitHub's compare ("ahead", or "diverged" after a
/// history rewrite), with what changed; None when GitHub cannot say. Only the last pair is kept: a
/// pair's answer never changes. A finished update can run a commit newer than the newest one seen
/// here (that list is cached, and the images of a newer commit may already be out), so "not the
/// same" is not "newer".
#[allow(clippy::type_complexity)]
async fn newer(st: &AppState, current: &str, latest: &str) -> Option<(bool, Vec<Value>)> {
    static LAST: Mutex<Option<(String, String, bool, Vec<Value>)>> = Mutex::new(None);
    if current == latest {
        return Some((false, vec![]));
    }
    // Both go into a URL: commit ids only.
    if !is_sha(current) || !is_sha(latest) {
        return None;
    }
    if let Some((c, l, n, ch)) = LAST.lock().unwrap().clone() {
        if c == current && l == latest {
            return Some((n, ch));
        }
    }
    let r = get_json(st, &api(&format!("compare/{current}...{latest}"))).await?;
    let n = matches!(r["status"].as_str()?, "ahead" | "diverged");
    let mut ch = if n { changes(&r) } else { vec![] };
    // A merge made without its title in the message: ask GitHub for the pull request's title
    // (only for the ones shown, once per pair; a failed call leaves "No description").
    for c in ch.iter_mut().take(MAX_CHANGES).filter(|c| c["what"] == "") {
        let Some(pr) = c["pr"].as_u64() else {
            continue;
        };
        let title = get_json(st, &api(&format!("pulls/{pr}")))
            .await
            .and_then(|v| Some(v["title"].as_str()?.chars().take(200).collect::<String>()));
        if let Some(t) = title {
            c["what"] = json!(t);
        }
    }
    *LAST.lock().unwrap() = Some((current.into(), latest.into(), n, ch.clone()));
    Some((n, ch))
}

/// Seconds each step is expected to take: the average of the last finished runs, else a default.
fn estimate(done: &[Vec<i32>]) -> Vec<i32> {
    DEFAULT_STEPS
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let xs: Vec<i32> = done.iter().filter_map(|t| t.get(i).copied()).collect();
            if xs.is_empty() {
                *d
            } else {
                (xs.iter().sum::<i32>() / xs.len() as i32).max(1)
            }
        })
        .collect()
}

/// Whether the updater sent a heartbeat lately, and the commit it was built from (None for an
/// updater from before it reported one, which cannot install an approved build).
async fn updater(st: &AppState) -> Result<(bool, Option<String>), ApiError> {
    let r: Option<(bool, Option<String>)> =
        sqlx::query_as("select at > now() - interval '90 seconds', case when sha_at > now() - interval '90 seconds' then sha end from updater_seen")
            .fetch_optional(&st.db)
            .await?;
    Ok(r.unwrap_or((false, None)))
}

async fn status(State(st): State<AppState>, s: Sess) -> Res {
    owner(&st, &s).await?;
    let run: Option<Value> = sqlx::query_scalar(
        "select jsonb_build_object('status', status, 'step', step, 'step_times', step_times,
            'step_elapsed', extract(epoch from now() - step_started_at)::int, 'to_sha', to_sha, 'error', error, 'took', extract(epoch from finished_at - created_at)::int)
         from update_runs order by id desc limit 1",
    )
    .fetch_optional(&st.db)
    .await?;
    let done: Vec<Vec<i32>> = sqlx::query_scalar(
        "select step_times from update_runs where status = 'done' order by id desc limit 5",
    )
    .fetch_all(&st.db)
    .await?;
    let current = std::env::var("BRIM_SHA").ok().filter(|v| v.len() == 40);
    let builds = with_changes(&st, builds(&st).await).await;
    let latest = builds.first().cloned();
    let (online, updater_sha) = updater(&st).await?;
    let behind = match (
        &updater_sha,
        latest.as_ref().and_then(|l| l["sha"].as_str()),
    ) {
        (Some(u), Some(l)) => behind(&st, u, l).await,
        _ => None,
    };
    let newer = match (&current, latest.as_ref().and_then(|l| l["sha"].as_str())) {
        (Some(c), Some(l)) => newer(&st, c, l).await,
        _ => None,
    };
    let (newer, changes) = match newer {
        Some((n, ch)) => (Some(n), ch),
        None => (None, vec![]),
    };
    Ok(Json(json!({
        "current": current,
        "latest": latest,
        // true: an update is there to take; false: up to date (or running something newer).
        "newer": newer,
        // The changes it brings, newest first: {sha, what, pr (only with a pull request)}; "more"
        // are not listed.
        "changes": changes.iter().take(MAX_CHANGES).collect::<Vec<_>>(),
        "more": changes.len().saturating_sub(MAX_CHANGES),
        "updater": online,
        // None: an updater from before it reported its commit (it must be replaced on the server).
        "updater_sha": updater_sha,
        // The files only the command on the server updates that changed since the updater's commit.
        "behind": behind,
        // The published releases to install, newest first: {sha, version, at}.
        "builds": builds,
        "run": run,
        "estimate": estimate(&done),
        // The names from before the rename to Brim: the page names that folder.
        "old_names": old_names(),
    })))
}

#[derive(Deserialize)]
struct Start {
    /// The commit of the release to install: one of `builds`.
    sha: String,
}

/// The command that updates what the Update button does not: the updater and docker-compose.yml.
/// A server under the old names gets no `git pull`: the compose file it would bring runs the Brim
/// names (new, empty database volume) and refuses to start there (`BRIM_NAMES`).
pub fn server_cmd() -> &'static str {
    if old_names() {
        "cd ~/tommybot && docker compose pull && docker compose up -d"
    } else {
        "cd ~/brim && git pull && docker compose pull && docker compose up -d"
    }
}

async fn start(State(st): State<AppState>, s: Sess, Json(b): Json<Start>) -> Res {
    owner(&st, &s).await?;
    if !s.stepped_up {
        return Err(ApiError::StepUp);
    }
    if old_names() {
        return Err(ApiError::BadRequest(
            "This server still runs the old names, and no new builds come under them. Move it to the Brim names first. Nothing was started.".into(),
        ));
    }
    let (online, updater_sha) = updater(&st).await?;
    if !online {
        return Err(ApiError::BadRequest(
            "The updater is not running. Start it on the server: docker compose up -d updater"
                .into(),
        ));
    }
    if updater_sha.is_none() {
        return Err(ApiError::BadRequest(format!(
            "The updater on the server is too old to install an approved build. Run this on the server once: {}", server_cmd()
        )));
    }
    if std::env::var("BRIM_SHA").is_ok_and(|c| c == b.sha) {
        return Err(ApiError::BadRequest(
            "That build is the version already running.".into(),
        ));
    }
    let builds = builds(&st).await;
    if builds.is_empty() {
        return Err(ApiError::BadRequest(
            "Could not reach GitHub to list the published releases. Nothing was started; try again in a minute.".into(),
        ));
    }
    let Some(version) = builds
        .iter()
        .find(|x| x["sha"] == b.sha)
        .and_then(|x| x["version"].as_str())
    else {
        return Err(ApiError::BadRequest(
            "That version is not one of the 10 most recent releases. Reload the page and pick again."
                .into(),
        ));
    };
    let digests = approved(&st, version).await.map_err(ApiError::BadRequest)?;
    queue(&st, s.user_id, &b.sha, &digests).await
}

/// Queues the run for the updater; at most one queued or running (a unique index).
async fn queue(st: &AppState, user: i64, sha: &str, digests: &Value) -> Res {
    let id: Option<i64> = sqlx::query_scalar(
        "insert into update_runs (requested_by, target_sha, digests) values ($1, $2, $3)
         on conflict do nothing returning id",
    )
    .bind(user)
    .bind(sha)
    .bind(digests)
    .fetch_optional(&st.db)
    .await?;
    match id {
        Some(_) => {
            let build = &sha[..sha.len().min(7)];
            // The run is queued already: a failed alert is logged, not a reason to report failure.
            if let Err(e) = crate::raiders::alert_update(&st.db, user, build).await {
                tracing::warn!(?e, "update alert to the bot owner not queued");
            }
            Ok(Json(json!({"ok": true})))
        }
        None => Err(ApiError::BadRequest("An update is already running.".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_published_release_comes_from_the_run_title() {
        let sha = "0d89861a4e8d47d58ee773cc5ea1e4fff8fc3810";
        assert_eq!(
            published(&format!("Release v1.20.3 at {sha}")),
            Some(("v1.20.3", sha))
        );
        // Builds of main from before releases, other tags and anything else give no release.
        assert_eq!(published(&format!("Images for {sha}")), None);
        assert_eq!(published("Images"), None);
        assert_eq!(published("Release v1.0.0 at 0d89861"), None);
        assert_eq!(published(&format!("Release v1.0.0 at {sha}x")), None);
        for v in [
            "v1.0",
            "1.0.0",
            "v1.0.0-rc1",
            "v1..0",
            "va.b.c",
            "v1.0.0.0",
            "feature-x",
        ] {
            assert_eq!(published(&format!("Release {v} at {sha}")), None, "{v}");
        }
    }

    /// Runs only on a scratch Postgres. `POST /api/update` makes the host pull and start new code:
    /// only the bot owner, after 2FA, with an updater online that reports its commit, for one of
    /// the published builds, one run at a time.
    #[tokio::test]
    async fn start_needs_the_bot_owner_2fa_a_new_updater_a_published_build_and_no_other_run() {
        let Some((st, mut s, _)) = crate::api::g3_handler_tests::setup(9_311_000_000).await else {
            return;
        };
        let me = 9_320_000_000 + std::process::id() as i64;
        st.discord.memo.lock().unwrap().insert(
            "/applications/@me".into(),
            (
                Instant::now() + Duration::from_secs(60),
                Some(json!({"owner": {"id": me.to_string()}})),
            ),
        );
        let sha = "ab".repeat(20);
        let start = |s: Sess| start(State(st.clone()), s, Json(Start { sha: sha.clone() }));
        let bad = |r: Res, has: &str| matches!(r, Err(ApiError::BadRequest(m)) if m.contains(has));
        // Shared tables on a scratch database: no run left active, the updater silent.
        sqlx::query(
            "update update_runs set status = 'failed' where status in ('queued', 'running')",
        )
        .execute(&st.db)
        .await
        .unwrap();
        sqlx::query("delete from updater_seen")
            .execute(&st.db)
            .await
            .unwrap();
        assert!(matches!(
            start(s.clone()).await,
            Err(ApiError::Forbidden(_))
        ));
        s.user_id = me;
        s.stepped_up = false;
        assert!(matches!(start(s.clone()).await, Err(ApiError::StepUp)));
        s.stepped_up = true;
        assert!(bad(start(s.clone()).await, "not running"));
        // An updater from before it reported its commit would pull whatever is tagged.
        sqlx::query("insert into updater_seen (at) values (now())")
            .execute(&st.db)
            .await
            .unwrap();
        assert!(bad(start(s.clone()).await, "too old"));
        // A sha a newer updater left behind, while an older one sends the heartbeat: still too old.
        sqlx::query("update updater_seen set sha = $1, sha_at = now() - interval '1 hour'")
            .bind("cd".repeat(20))
            .execute(&st.db)
            .await
            .unwrap();
        assert!(bad(start(s.clone()).await, "too old"));
        sqlx::query("update updater_seen set sha_at = now()")
            .execute(&st.db)
            .await
            .unwrap();
        // Not a published build (the list holds others, or GitHub is out of reach here): refused
        // before any lookup.
        let r = start(s.clone()).await;
        assert!(
            matches!(&r, Err(ApiError::BadRequest(m)) if m.contains("10 most recent") || m.contains("Could not reach GitHub"))
        );
        let d = json!({"bot": format!("sha256:{}", "0".repeat(64))});
        assert!(queue(&st, me, &sha, &d).await.is_ok());
        // A queued update sends the bot owner a security alert.
        let alert: String = sqlx::query_scalar(
            "select payload->>'text' from scheduled_jobs where kind = 'raider_notify' and payload->>'user' = $1 order by id desc limit 1",
        )
        .bind(me.to_string())
        .fetch_one(&st.db)
        .await
        .unwrap();
        assert!(
            alert.contains("updating to build **abababa**")
                && alert.contains(&format!("<@{me}> ({me})"))
                && alert.contains("Not you?"),
            "{alert}"
        );
        let fixed: String = sqlx::query_scalar(
            "select payload->>'fixed' from scheduled_jobs where kind = 'raider_notify' and payload->>'user' = $1 order by id desc limit 1",
        )
        .bind(me.to_string())
        .fetch_one(&st.db)
        .await
        .unwrap();
        assert_eq!(fixed, "update");
        assert!(bad(queue(&st, me, &sha, &d).await, "already running"));
        sqlx::query("delete from update_runs where requested_by = $1")
            .bind(me)
            .execute(&st.db)
            .await
            .unwrap();
        sqlx::query("delete from updater_seen")
            .execute(&st.db)
            .await
            .unwrap();
    }

    #[test]
    fn builds_and_side_files_come_from_github_answers() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let c = "c".repeat(40);
        let ok = "success";
        let runs = json!({"workflow_runs": [
            // A cancelled or running build is not published.
            {"display_title": format!("Release v1.2.0 at {c}"), "created_at": "3", "conclusion": "cancelled"},
            {"display_title": format!("Release v1.2.0 at {c}"), "created_at": "3", "conclusion": null},
            {"display_title": format!("Release v1.1.0 at {b}"), "created_at": "2", "conclusion": ok},
            // A re-run publishes the same release again; a build of main from before releases is none.
            {"display_title": format!("Release v1.1.0 at {b}"), "created_at": "1", "conclusion": ok},
            {"display_title": format!("Images for {c}"), "created_at": "1", "conclusion": ok},
            {"display_title": "Images", "created_at": "0", "conclusion": ok},
            {"display_title": format!("Release v1.0.0 at {a}"), "created_at": "0", "conclusion": ok},
        ]});
        let v = runs_to_builds(&runs);
        assert_eq!(
            v,
            vec![
                json!({"sha": b, "version": "v1.1.0", "at": "2"}),
                json!({"sha": a, "version": "v1.0.0", "at": "0"})
            ]
        );
        // A re-run of an older release is listed after the higher one.
        let rerun = json!({"workflow_runs": [
            {"display_title": format!("Release v1.9.0 at {a}"), "created_at": "5", "conclusion": ok},
            {"display_title": format!("Release v1.10.0 at {b}"), "created_at": "4", "conclusion": ok},
        ]});
        let order: Vec<Value> = runs_to_builds(&rerun)
            .iter()
            .map(|x| x["version"].clone())
            .collect();
        assert_eq!(order, [json!("v1.10.0"), json!("v1.9.0")]);
        // Only the releases GHCR still keeps.
        let many: Vec<Value> = (0..12)
            .map(|i| json!({"display_title": format!("Release v1.{i}.0 at {i:040x}"), "conclusion": ok}))
            .collect();
        assert_eq!(runs_to_builds(&json!({"workflow_runs": many})).len(), KEPT);

        let f = |status: &str, files: &[&str]| json!({"status": status, "files": files.iter().map(|f| json!({"filename": f})).collect::<Vec<_>>()});
        assert_eq!(
            side_changes(&f("ahead", &["README.md", "docker-compose.yml"])),
            Some(vec!["docker-compose.yml"])
        );
        assert_eq!(
            side_changes(&f("behind", &["scripts/updater.sh"])),
            Some(vec![])
        );
        let many: Vec<String> = (0..300).map(|i| format!("f{i}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(side_changes(&f("ahead", &many)), None);

        // The record must name this image's package, this digest and the Images workflow on main.
        use base64::Engine;
        let hex = "0a".repeat(32);
        let rec_in = |repo: &str, name: &str, path: &str| {
            let st = json!({"subject": [{"name": name, "digest": {"sha256": hex}}],
                "predicate": {"buildDefinition": {"externalParameters": {"workflow": {
                    "repository": repo, "path": path, "ref": "refs/heads/main"}}}}});
            let p = base64::engine::general_purpose::STANDARD.encode(st.to_string());
            json!({"attestations": [{"bundle": {"dsseEnvelope": {"payload": p}}}]})
        };
        let rec = |name: &str, path: &str| rec_in("https://github.com/thomkr01/Brim", name, path);
        let d = format!("sha256:{hex}");
        // Only Brim: builds of the old repository (under either image name) and of any other
        // repository do not count.
        let img = "ghcr.io/thomkr01/brim-bot";
        let wf = ".github/workflows/images.yml";
        for name in [img, "ghcr.io/thomkr01/tommybot-bot"] {
            let old = rec_in("https://github.com/thomkr01/TommyBot", name, wf);
            assert!(!attests(&old, name, &d));
        }
        assert!(!attests(
            &rec_in("https://github.com/thomkr01/other", img, wf),
            img,
            &d
        ));
        assert!(!attests(
            &rec_in("https://github.com/someone/Brim", img, wf),
            img,
            &d
        ));
        let ok = rec(img, wf);
        assert!(attests(&ok, img, &d));
        assert!(!attests(&ok, "ghcr.io/thomkr01/brim-dashboard", &d));
        assert!(!attests(&ok, "ghcr.io/thomkr01/tommybot-bot", &d));
        assert!(!attests(&ok, img, &format!("sha256:{}", "1b".repeat(32))));
        assert!(!attests(&rec(img, ".github/workflows/other.yml"), img, &d));
        assert!(!attests(&json!({"attestations": []}), img, &d));
        // Not set: the Brim names.
        assert_eq!(package("bot"), "thomkr01/brim-bot");
        assert!(api("pulls/1").starts_with("https://api.github.com/repos/thomkr01/Brim/"));
        assert!(server_cmd().starts_with("cd ~/brim && "));

        assert!(is_digest(&format!("sha256:{}", "0a".repeat(32))));
        assert!(!is_digest(&format!("sha256:{}", "0A".repeat(32))));
        assert!(!is_digest("sha256:abc"));
    }

    #[test]
    fn estimate_averages_finished_runs_per_step() {
        assert_eq!(estimate(&[]), DEFAULT_STEPS.to_vec());
        let runs = [vec![1, 40, 8, 6, 17], vec![3, 80, 12, 6, 17]];
        assert_eq!(estimate(&runs), vec![2, 60, 10, 6, 17]);
        // A step that took under a second still counts as one, so progress never divides by zero.
        assert_eq!(estimate(&[vec![0, 0, 0, 0, 0]]), vec![1; 5]);
    }

    #[test]
    fn changes_lists_pull_requests_and_plain_commits_newest_first() {
        let long = "x".repeat(300);
        let m = |msg: String| json!({"parents": [{}, {}], "commit": {"message": msg}});
        let p = |msg: String| json!({"parents": [{}], "commit": {"message": msg}});
        let c = json!({"total_commits": 8, "commits": [
            m("Merge pull request #7 from a/b\n\nFirst thing".into()),
            p("  a plain commit  \n\nbody not listed".into()),
            // A branch commit with a merge's words is not a merge: a plain commit.
            p("Merge pull request #1 from x/y\n\nfake".into()),
            m(format!("Merge pull request #9 from a/c\n\n{long}")),
            m("Merge pull request #10 from a/d".into()),
            // A merge that is not a pull request, and a commit with no subject: not listed.
            m("Merge branch 'x'".into()),
            p("\n\nbody only".into()),
            p(long.clone()),
        ]});
        let ch = changes(&c);
        assert_eq!(ch.len(), 6);
        // A plain commit: its subject, capped, and no pull request number.
        assert_eq!(ch[0]["what"].as_str().unwrap().len(), 200);
        assert!(ch[0].get("pr").is_none());
        assert_eq!(
            (ch[1]["pr"].clone(), ch[1]["what"].clone()),
            (json!(10), json!(""))
        );
        assert_eq!(ch[2]["what"].as_str().unwrap().len(), 200);
        assert_eq!(
            (ch[3]["pr"].clone(), ch[3]["what"].clone()),
            (Value::Null, json!("Merge pull request #1 from x/y"))
        );
        assert_eq!(
            (ch[4]["pr"].clone(), ch[4]["what"].clone()),
            (Value::Null, json!("a plain commit"))
        );
        assert_eq!(
            (ch[5]["pr"].clone(), ch[5]["what"].clone()),
            (json!(7), json!("First thing"))
        );
        // A list GitHub cut short would miss the newest: none then.
        let mut cut = c.clone();
        cut["total_commits"] = json!(300);
        assert!(changes(&cut).is_empty());
    }

    #[test]
    fn squash_commits_are_listed_and_split_per_build() {
        let q =
            |sha: &str, msg: &str| json!({"sha": sha, "parents": [{}], "commit": {"message": msg}});
        let c = json!({"total_commits": 4, "commits": [
            q("a", "Fix one (#5)\n\nbody"), q("b", "plain commit"),
            q("c", "Fix two (#6)"), q("d", "Fix three (#7)")]});
        assert_eq!(
            changes(&c)
                .iter()
                .map(|x| x["pr"].as_u64())
                .collect::<Vec<_>>(),
            [Some(7), Some(6), None, Some(5)]
        );
        let per = changes_per_build(&c, &["d", "b"]);
        assert_eq!(per["b"].len(), 2);
        assert_eq!(per["b"][0]["what"], "plain commit");
        assert_eq!(per["b"][1]["what"], "Fix one");
        assert_eq!(
            per["d"]
                .iter()
                .map(|x| x["pr"].as_u64())
                .collect::<Vec<_>>(),
            [Some(7), Some(6)]
        );
    }
}
