# Running on a server

A step-by-step guide from an empty server to a running Brim with a dashboard at `https://brim.example.com` and, optionally, an AI connector (MCP server) at `https://mcp.example.com`. Replace `example.com` with your domain everywhere.

The dashboard and the MCP server are reached through a Cloudflare Tunnel, so the server opens no ports and needs no reverse proxy or certificates.

**Only Cloudflare Tunnel is supported for web verification.** The dashboard takes the visitor's IP from the `CF-Connecting-IP` header for the alt-account and VPN checks and for its rate limits. It believes that header only on a connection from the `cloudflared` container of this compose file; from the host, another container or any other proxy the header is dropped, so with a tunnel run elsewhere the alt-account check is skipped and a server with the VPN block on cannot verify on the web.

`DASHBOARD_URL` must start with `https://`, so the login cookie is marked Secure; the dashboard refuses to start otherwise. Only a local run may use `http://localhost` or `http://127.0.0.1` (then the cookie is not Secure).

**You need:**

- A Linux server (VPS) you can reach with SSH. The published images are **arm64** (for example Oracle Cloud's free Ampere servers). On an amd64 server, build the images yourself (`docker compose up -d --build`, see [Updating](#5-updating)).
- A domain whose DNS is on Cloudflare (the free plan is enough).
- A Discord account.

Commands marked **(server)** run in an SSH session on the server, **(computer)** on your own computer. Values in `<angle brackets>` are placeholders: replace the whole thing, brackets included.

## 1. Discord application

In the [Discord Developer Portal](https://discord.com/developers/applications):

1. **New Application**, name it (for example `Brim`).
2. **OAuth2**: copy the **Client ID** and **Client Secret** (reset it if it is hidden). Under **Redirects**, add `https://brim.example.com/auth/callback` (the dashboard login). If you will use an MCP client with OAuth login (step 6.3), also add `https://mcp.example.com/oauth/callback`. Save.
3. **Bot**: **Reset Token** and copy it. This is `DISCORD_TOKEN`; anyone with it controls the bot, so never paste it anywhere but `.env`. Enable **Server Members Intent** and **Message Content Intent**. Save.
4. Invite the bot to your server with this URL (replace `CLIENT_ID`), then in Discord drag the bot's role to the top of the role list:

   ```text
   https://discord.com/oauth2/authorize?client_id=CLIENT_ID&scope=bot+applications.commands&permissions=8
   ```

## 2. Cloudflare Tunnel

In the [Cloudflare dashboard](https://dash.cloudflare.com):

1. **Networking > Tunnels > Create a tunnel**, pick **Cloudflared**, and name it `brim`.
2. Copy the token from the install command: the long string after `--token`, not the tunnel ID. Skip installing the connector; Docker runs it. This is `TUNNEL_TOKEN`.
3. In the tunnel, **Routes > Add route > Published application**: subdomain `brim`, your domain, service URL `http://dashboard:8080`, path empty. If a DNS record with that name already exists, delete it first (Cloudflare creates the record itself).
4. Optional, for the MCP server (step 6): add a second route the same way, subdomain `mcp`, service URL `http://mcp:8090`. Until the `mcp` container runs, that address answers with a 502 error page; that is harmless.

The service URLs are container names, so they only work from inside the Docker network, which is where the tunnel runs.

**Limits at the edge (optional, recommended).** The dashboard and the MCP server limit requests per visitor address themselves (dashboard: about 160 at once and 80 a second per address, 40 and 20 per login; MCP login routes: 60 at once, 1 a second). Cloudflare can stop a flood before it reaches the server. On the free plan, in your domain: **Security > WAF > Rate limiting rules > Create rule**, name `brim public`, field **URI Path** starts with `/raid/`, or starts with `/verify/`, or starts with `/auth/`, or (on the `mcp.` host) equals `/register` or `/token`; **Same characteristics: IP**; 60 requests per 10 seconds; action **Block** for 10 seconds. Keep the rule written here up to date when you change it, so the edge rules are known.

## 3. Server

**(server)** Install Docker with the compose plugin and make it start at boot:

```bash
curl -fsSL https://get.docker.com | sh
sudo systemctl enable docker
sudo usermod -aG docker $USER   # then log out and in again, so docker works without sudo
```

The `docker` group is root-equivalent; only add your own account.

**(server)** Get the code and create `.env`:

```bash
git clone https://github.com/thomkr01/Brim.git ~/brim
cd ~/brim
cp .env.example .env
nano .env
```

Fill in (nano: arrow keys to move, Ctrl+O and Enter to save, Ctrl+X to close):

```bash
DISCORD_TOKEN=<bot token from step 1.3>
DISCORD_CLIENT_ID=<client id from step 1.2>
DISCORD_CLIENT_SECRET=<client secret from step 1.2>
DASHBOARD_URL=https://brim.example.com
POSTGRES_PASSWORD=<letters and digits only, it goes into a URL>
DASHBOARD_DB_PASSWORD=<another one, letters and digits only>
TUNNEL_TOKEN=<token from step 2.2>
COMPOSE_PROFILES=tunnel
```

`COMPOSE_PROFILES=tunnel` makes every `docker compose` command include the tunnel container. A random password: `openssl rand -hex 24`.

**(server)** Start:

```bash
docker compose pull
docker compose up -d
docker compose ps
```

All services should show `running` (postgres `healthy`). Open `https://brim.example.com` and log in with Discord. The containers restart on their own after a crash or a reboot.

## 4. GitHub packages (images)

GitHub Actions builds the images after every CI check passed on a push to `main` (the **Images** workflow starts when **CI** finishes) and publishes four packages to GitHub's container registry (GHCR): `brim-bot`, `brim-dashboard`, `brim-updater` and `brim-mcp` (arm64, tags `latest` and the short commit sha; the newest 10 builds are kept, one package version each). `docker compose pull` downloads them. Each image has a signed build record (attestation) on GitHub; check one with `gh attestation verify oci://ghcr.io/thomkr01/brim-bot:<sha> -R thomkr01/Brim`.

- **Using these images as they are:** nothing to do; they are public.
- **Running your own fork:** your Actions publish to `ghcr.io/<your-github-name>/brim-*`. Change the four `image:` lines in `docker-compose.yml` to your name. New packages start **private**, and the server then gets `denied` when it pulls. Make each package public once: GitHub > your profile > **Packages** > the package > **Package settings** (bottom right) > **Danger Zone** > **Change visibility** > **Public**, and type the name to confirm. A package only appears after its first build. This makes the image public, not your code or `.env`. To keep them private instead, log in on the server once with a classic personal access token that has only `read:packages`: `echo <token> | docker login ghcr.io -u <your-github-name> --password-stdin`.

## 5. Updating

**(server)**

```bash
cd ~/brim && git pull && docker compose pull && docker compose up -d && docker image prune -f
```

**Before the first update after 18 Nov 2026 (security review):** check that `DASHBOARD_URL` in `.env` starts with `https://` (`grep DASHBOARD_URL ~/brim/.env`). If it does not, the new dashboard stops at start with `DASHBOARD_URL must start with https://` in `docker compose logs dashboard`, and an update from the dashboard rolls back to the previous build.

Wait for the **Images** workflow on GitHub (Actions tab) to finish after a merge first (it starts when **CI** passed), or you pull the previous version.

To build on the server instead of pulling (needed on amd64): `docker compose up -d --build`. Building takes a few GB of RAM and up to 20 minutes on a small arm64 server.

Postgres runs with `shared_buffers=32MB` and `max_connections=40` (set in `docker-compose.yml`; the defaults are 128 MB and 100). That saves about 23 MB of RAM (measured, empty database) and each idle connection costs about 1.5 MB more. The 40 covers the worst case of all services together: bot pool 10 + 2 listeners, dashboard pool 10 + 2 listeners, MCP 3, the updater, the dumps and an admin shell (about 33). Raise `max_connections` when you add a service or enlarge a pool. A change to these lines applies with `docker compose up -d`, which restarts Postgres once (the bot and dashboard reconnect on their own).

The updater image is plain Alpine with `psql` and the libraries it loads copied out of the pinned `postgres:18-alpine` image (see the `pgclient` stage in the `Dockerfile`), plus the Docker CLI and compose plugin; no package is installed. It is about 400 MB smaller on disk than building on the whole Postgres image.

### From the dashboard

The bot owner (the owner of the Discord application) sees **Update** in the dashboard header. It shows the running and the newest published version and updates with one click, with live progress and a time estimate based on earlier updates. **Published builds** lists the newest 10 builds; **Install** puts any of them on the server, for example to go back to one that worked (the database stays as it is). The page warns when `scripts/updater.sh` or `docker-compose.yml` changed since the server was last updated by hand, and gives the command for it. It needs the `updater` service, which starts with the others; on a server set up before it existed, run `git pull && docker compose up -d` once.

The updater is the only container with the Docker socket. It has no open port and takes no input except an "install this build" row in the database, so the dashboard (which faces the internet) never controls Docker. It mounts the project folder read-only, `.env` with all its secrets included, so it can recreate the containers exactly as configured. When you click, the dashboard looks up each image's digest for that build in GHCR and accepts it only when GitHub holds the signed build record the Images workflow on `main` made for that image. The updater checks that record again itself for every digest before it pulls (it needs to reach `api.github.com`), so a row written into the database by anything else installs nothing. The updater installs only builds whose Update page works this way, so builds from before it (17 Nov 2026 and older) cannot be installed from the page. The updater pulls exactly those digests for the bot, the dashboard and, when it is running, the MCP server (a stopped one stays stopped), refuses an image whose version label is another build, saves a database dump to `~/brim/pre-update/` (the newest three are kept; the update stops before any restart if the dump fails, takes over 15 minutes, or the disk has less free space than twice the database), recreates them, and checks that they stay up. If anything fails it puts the previous images back and restarts on them (they stay on disk under the `rollback` tag). It does not update itself or `docker-compose.yml`: run `cd ~/brim && git pull && docker compose pull && docker compose up -d` for those (an updater from before this must be replaced this way once; the page says so). The version check compares with `thomkr01/Brim` on GitHub (`crates/dashboard/src/update.rs`, `scripts/updater.sh`), also in a fork: a fork changes those URLs to its own repository. A record of the old repository `thomkr01/TommyBot` (kept as an archive) is only asked for by a server on the old names, and the updater installs only dashboard images labelled `org.brim.update`. A server set up before the rename to Brim (folder `~/tommybot`, database user `tommybot`) keeps installing the `tommybot-*` images of the old repository, and its Update page gives a command without `git pull`, until its owner moves it to the Brim names (see "Old names" below). With `BRIM_TAG` pinned (see Rolling back) the updater refuses to run: remove the pin first.

### Dashboard database role

The dashboard faces the internet, so it logs in to Postgres as its own role, `brim_dashboard`, and not as `brim` (a superuser, which can also run programs in the Postgres container and change other roles). The dashboard's role can read and change rows, but cannot change the schema, roles or files, and opens at most 12 connections. The bot makes the role at every start and sets its password from `DASHBOARD_DB_PASSWORD`; it runs all migrations, and the dashboard waits for them. The bot, the updater and the backup script stay on `brim`.

A server set up before this (18 Nov 2026) does this once, after the **Images** workflow of that merge has finished. The first line makes a random password in `.env`, so you never type or copy it:

```bash
cd ~/brim && grep -q '^DASHBOARD_DB_PASSWORD=.' .env || { p=$(openssl rand -hex 24) && [ -n "$p" ] && printf '\nDASHBOARD_DB_PASSWORD=%s\n' "$p" >> .env; }
git pull && docker compose pull && docker compose up -d && docker image prune -f
docker compose ps
docker compose logs dashboard | tail -20
```

All services show `running`. The dashboard may log `database login refused, trying again in 2 s` a few times while the bot starts; then it listens as before. Until you do this the old setup keeps working: the Update button installs the new images, and the dashboard still logs in as the owner role. After `git pull`, every `docker compose` command stops with `set DASHBOARD_DB_PASSWORD in .env` until the line is there, without touching the running containers.

The same compose change hardens the bot and the dashboard, which hold the bot token: they drop all Linux capabilities, cannot gain privileges, and read the token from the file `/run/secrets/discord_token` (compose makes it from `DISCORD_TOKEN` in `.env`, which stays as it is), so it is no longer in their environment. If compose stops with an error about `secrets` or `environment`, Docker is too old: `curl -fsSL https://get.docker.com | sh`, then run the commands again.

Check (optional): `docker compose exec postgres psql -U brim brim -c "select usename, count(*) from pg_stat_activity where datname = 'brim' group by 1"` lists `brim_dashboard`.

Going back to a build from before this: **Install** on the Update page fails and puts the running version back (that bot looks for the token in its environment, and that dashboard cannot start on the new role). By hand, take the compose file of that build as well, then pin it: `git checkout <sha> -- docker-compose.yml`, add `BRIM_TAG=<sha>` to `.env`, `docker compose pull && docker compose up -d`. Forward again: remove the `BRIM_TAG` line, `git checkout HEAD -- docker-compose.yml`, `docker compose pull && docker compose up -d`.

### Rolling back

Use **Install** on the Update page (see above). Without the dashboard: pin an older sha tag (see the package versions on GitHub) in `.env`, then `docker compose pull && docker compose up -d`. Remove the line to follow `latest` again. Migrations only add to the database, so an older image keeps working on it, with one exception: migration 3989 removed the old `embeds.embed` column, so an image from before it (before 11-17) cannot save or show Message Builder messages. To bring the database back to its state before an update as well, restore the updater's dump from before it (see Restoring for real in section 8).

```bash
BRIM_TAG=a1b2c3d
```


### Old names (servers set up before the rename to Brim)

A server set up before the rename runs the compose project `tommybot` in `~/tommybot`: database and owner role `tommybot`, roles `tommybot_dashboard` and `tommybot_mcp`, volumes `tommybot_pgdata` and `tommybot_logs`, images `tommybot-*` of the old repository. New builds keep working there: the bot and the dashboard read the role names from the database they log in to (`brim_core::db::roles`), and the updater reads the repository from its database user. Do not `git pull` in that folder: the compose file it brings runs the Brim names on new, empty volumes, so it refuses to start without `BRIM_NAMES=1` in `.env` (go back with `git checkout HEAD@{1}`). Moving it to the Brim names takes a few minutes of downtime: stop the old project, clone into `~/brim` and copy `.env` there (with `BRIM_NAMES=1`), copy the volume `tommybot_pgdata` into `brim_pgdata`, rename the database and the three roles in the copy (`ALTER DATABASE ... RENAME`, `ALTER ROLE ... RENAME`, run as a second, temporary superuser; grants and passwords stay), then start. The old folder and volumes stay untouched as the way back. A dump made before the move (daily backup or pre-update) restores only there, with the old folder's scripts: its objects belong to the role `tommybot`, which no longer exists after the move.

## 6. AI connector (MCP) (optional)

The `mcp` service lets an AI client read Brim: running version, log lines from the last 24 hours (with bot tokens, Bearer values, webhook and Postgres URLs, login and OAuth codes and state, session ids, and member ids and names masked; server, channel, role and message ids stay), guild config and its history, cases, the raid and join gate logs, the action log, antinuke incidents and quarantines, automod strikes, appeals and reports (without the message or answers), notes, scheduled jobs and backups. It does not show NSFW scans. Each read of a server's data is listed for that server's staff on the dashboard **History** page (tool, when and which login; kept 90 days); log lines, the server list, the status and the Raider List are not one server's records and are not listed. Every change needs a code the bot DMs you (step 6.4). It holds no bot token and no Docker socket, logs in to Postgres as `brim_mcp` (which sees only the `mcp_*` views, read-only; it can only add rows to the read log and ask whether a token hash is live), and mounts the log files read-only. Two ways in:

- **An MCP client that sends a fixed token** uses a token you make on the dashboard's **MCP tokens** page. It can read and propose changes.
- **An MCP client that logs in with OAuth** goes through Discord; only the user in `MCP_OWNER_ID` gets in. It can only read.

### 6.1 Turn it on

You need the `mcp` route from step 2.4.

**(server)** Add the secrets. Each `echo` line adds one line to `.env` and prints nothing; that is normal:

```bash
cd ~/brim
echo "MCP_DB_PASSWORD=$(openssl rand -hex 24)" >> .env
nano .env
```

In nano, change `COMPOSE_PROFILES=tunnel` to `COMPOSE_PROFILES=tunnel,mcp`, save and close. Then:

```bash
docker compose pull && docker compose up -d
docker compose logs mcp | tail -3
```

The log shows `mcp listening on 0.0.0.0:8090`. The bot restarts too, because it sets the `brim_mcp` password at start.

**(dashboard)** Make a token: click the round account button (your initial) at the top right and choose **MCP tokens** (only the bot owner sees it). Type a name, for example `AI client`, and click **Make token**. Confirm 2FA when asked. The token starts with `tbmcp_` and is shown once: click **Copy**. Only a hash of it is stored, so a token you lose cannot be shown again: make a new one and revoke the old. The bot DMs you each time a token is made or revoked.

**(computer)** Test it with that token:

```bash
curl -s https://mcp.example.com/mcp -H "Authorization: Bearer <token>" -H "content-type: application/json" -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | head -c 300
```

The answer starts with `{"id":1,"jsonrpc":"2.0","result":{"tools":[`.

### 6.2 MCP client with a token

**(computer)** In the MCP settings of your client, add an HTTP MCP server with this URL and this header:

- URL: `https://mcp.example.com/mcp`
- Header: `Authorization: Bearer <token>`

Use the token from step 6.1. The server shows as connected. Then ask the client, for example, "use brim and show the last 20 lines of the bot log"; allow the tool the first time.

### 6.3 MCP client with OAuth login

Some MCP clients cannot send a fixed token. They use OAuth, and the MCP server sends you through your Discord application to log in.

1. Discord Developer Portal > your app > **OAuth2 > Redirects**: `https://mcp.example.com/oauth/callback` must be listed (step 1.2).
2. Find your Discord user id: Discord > **User Settings > Advanced > Developer Mode** on, then right-click your name > **Copy User ID**. It is a long number.
3. **(server)** Replace `<your id>` with that number (digits only, no brackets):

   ```bash
   cd ~/brim
   echo "MCP_OAUTH_SECRET=$(openssl rand -hex 32)" >> .env
   echo "MCP_PUBLIC_URL=https://mcp.example.com" >> .env
   echo "MCP_OWNER_ID=<your id>" >> .env
   echo "MCP_OAUTH_REDIRECTS=https://client.example.com/oauth/callback" >> .env
   grep MCP_ .env
   docker compose pull mcp && docker compose up -d mcp
   docker compose logs mcp | tail -3
   ```

   The log no longer says `oauth off`.

   `MCP_OAUTH_REDIRECTS` is a comma-separated list of the exact https callback URLs the login may return to. Host and path must match exactly; no port, no query. Empty means no https callback is accepted. A client on your own computer (http on `localhost`, `127.0.0.1` or `[::1]`) is always accepted.
4. In the client, add a custom connector with the URL `https://mcp.example.com/mcp`. Let it sign in with OAuth and register automatically, and add no headers. Then log in with Discord. Anyone else who tries is refused.

Access tokens last an hour and the client renews them until 30 days after you logged in; then you log in with Discord again. Changing `MCP_OAUTH_SECRET` (then `docker compose up -d mcp`) logs every OAuth client out at once. The login returns only to the callbacks in `MCP_OAUTH_REDIRECTS` or to localhost.

### 6.4 Changes through the AI connector

A login with a dashboard token (MCP client with a token) gets `propose_change`, `confirm_change` and `score`. A change takes two steps:

1. The AI client calls `propose_change`. Nothing changes yet. The bot sends **you** a Discord DM with the server name, what will change (a diff for config; for a case, note or lockdown change the record or locks as they are now next to the request) and a 6-digit code, valid 5 minutes. The AI client never sees the code. If the record changes before you confirm, the change is refused and the AI client must propose it again.
2. Read the DM. If it is what you want, type the code to the AI client, which calls `confirm_change`. Anti-nuke, whitelist, panic, catch, permits and rollback also need a code from your authenticator app, the same as on the dashboard (you must have set up 2FA there). Three wrong codes, from the DM or the authenticator app, close the change. Wrong authenticator codes sent through MCP have their own lockout, so they never lock you out of the dashboard.
3. If the AI client gets no answer in 30 seconds, the change may still be running. It then calls `confirm_change` again with the same id, which says whether the change ran, failed, or was cut off; it does not run it twice.

The change then runs through the dashboard as you, the bot owner, with every check the dashboard page has, and the log channel names it `AI connector (MCP)`. Notes and case edits made this way show "AI via bot owner" with the token name, on the dashboard, in Discord and in the MCP reads, so nobody takes them for your own words. The AI client can change config sections, permits, roll config back, lock and unlock, add and edit notes, edit cases, and save and post embeds. It can never start an update, delete server data, load or delete backups, or revert anti-nuke actions; those routes are not reachable from the MCP server at all. At most 5 changes can wait for a code at once.

Coming from the old fixed `MCP_TOKEN`: follow the upgrade note for MCP access in CHANGELOG.md. An OAuth login gets no change tools; it only reads. Each place that uses the MCP server gets its own token (step 6.1). The DM names the token that proposed the change, and so does the audit trail.

To shut a token out, click **Revoke** on the MCP tokens page (2FA). This works at once for reads and changes, also closes the changes that token still had waiting for a code, and is the first thing to do if you get a DM you did not ask for. Every proposal is kept in the `mcp_writes` table with the token name, the action, when and the outcome; the request itself is removed once it no longer waits for a code.

### 6.5 Keeping it safe

What the tools show includes private staff note text, case reasons and the proof staff added (not the stored copies of members' messages), servers in their 30 days after the bot was removed (until their data is deleted), and the updater's error text. Everything the AI client reads goes to the AI provider you connect; PRIVACY.md says so for server owners.

Every tool call is logged by name only (`docker compose logs mcp`); arguments can hold codes, note text and member ids, so they are not logged. Anyone with a token can read everything the tools show, so keep it out of chats, screenshots and repositories. To replace one, make a new token, change the header in your client's MCP settings (step 6.2), then revoke the old one. Text from Discord (reasons, names, messages) reaches the AI client through these tools; such text cannot make a change on its own, because every change needs the code from your DM. Never type that code anywhere except to your AI client, and only after you have read the DM.

## 7. Troubleshooting

| What you see | Why | Fix |
|---|---|---|
| `Error response from daemon: ... denied` on `docker compose pull` | The package is private, or not built yet | Wait for the **Images** workflow to finish (it runs after **CI** passed), then make the package public (step 4) |
| `WARNING: Some service image(s) must be built from source` | Same: an image could not be pulled | As above, then pull again |
| Dashboard or `mcp.` address shows a Cloudflare 502 page | The container is not running, or the route's service URL is wrong | `docker compose ps`; check the route is `http://dashboard:8080` or `http://mcp:8090` |
| `mcp` keeps restarting, log says `MCP_OWNER_ID must be a Discord user id` | The placeholder was pasted literally | `sed -i 's/^MCP_OWNER_ID=.*/MCP_OWNER_ID=<your id>/' .env`, then `docker compose up -d mcp` |
| `propose_change` says `needs a token from the dashboard's MCP tokens page` | You are logged in with OAuth, which only reads | Use an MCP client with a dashboard token (steps 6.1 and 6.2) |
| `propose_change` says `dashboard refused (401 Unauthorized)` | The token was revoked while the client was connected | Make a new token and add it again (step 6.2) |
| `propose_change` says `dashboard refused (404 Not Found)` | The dashboard image is older than this feature | `git pull && docker compose pull && docker compose up -d` |
| `propose_change` says `could not DM the bot owner` | Your DMs from the bot are closed | Allow DMs from server members in a server you share with the bot |
| `mcp` log says `oauth off` | One of the three OAuth values is missing | Step 6.3.3; `grep MCP_ .env` shows which |
| OAuth login ends on Discord with "Invalid OAuth2 redirect_uri" | The redirect is not in the Discord app | Step 1.2 |
| OAuth login says only the bot owner has access | You logged in with another Discord account, or `MCP_OWNER_ID` is wrong | Check the id (step 6.3.2) |
| OAuth login is refused with `invalid_redirect_uri` | The callback of the client is not in `MCP_OAUTH_REDIRECTS` | Add its exact callback URL (step 6.3) |
| curl answers `{"error":"unauthorized"}` | Wrong, incomplete or revoked token | One space between `Bearer` and the token, no `<>`; the token starts with `tbmcp_`. Make a new one if it was revoked |
| A tool says `the read could not be logged` | The read log is not in the database yet (the bot runs the migration at start) | `docker compose logs bot \| tail -20`; wait for the bot to start, then try again |
| `logs_tail` says no logs | The `logs` volume is missing | `git pull && docker compose up -d` once |
| No log output at all | `RUST_LOG` too low | Compose sets `RUST_LOG=info`; keep it |

## 8. Database backup

`scripts/backup.sh` dumps the database daily and keeps 7 copies in `~/brim-backups`. Once the encryption key below is set up, every copy is encrypted with [age](https://age-encryption.org) to a public key as it is made: the private key that opens it is kept off the server, by you, so neither someone who copies the backup folder (which holds data deleted since), the B2 account nor a stolen rclone config can read it (security review). The live database itself is not encrypted by this. So every restore from a daily copy needs you to paste that key, and if the key is lost, no copy can be opened. Optionally the script also uploads the copies to Backblaze B2 with rclone. B2's object lock keeps each copy 30 days, and nobody can delete it earlier, not even with the server's own key. The rclone image is pinned by digest in `docker-compose.yml` (service `rclone`), so nothing is installed for it.

### Encryption key (once)

1. Install age on the server: `sudo apt install age`. Then run `age-keygen` (with no file name, so the private key is never written to the server's disk). It prints the public key (`# public key: age1...`) and the private key (`AGE-SECRET-KEY-1...`).
2. Copy the `AGE-SECRET-KEY-1...` line into your password manager, in two places, and run `clear`. Without this key no copy, local or off-site, can be opened.
3. Set the public key in `.env` (a comment on the same line would become part of the value):

```bash
BACKUP_AGE_RECIPIENT=age1...
```

Until `BACKUP_AGE_RECIPIENT` holds a valid public key and age is installed, the script still makes the daily copy, but unencrypted, uploads nothing, and fails with a message that says so, so cron reports it every day. Copies made before the key was set are deleted by the normal 7-day rule.

### Off-site copy (Backblaze B2, free up to 10 GB)

Google Drive has no object lock, so it is not used. First set up the encryption key (above). Then, once:

1. In B2: create a private bucket (for example `brim-backups`) with **Object Lock** on, default retention **Compliance, 30 days**, and a lifecycle rule that deletes files after 30 days ("Keep only the last version" is not enough: use a custom rule with `daysFromUploadingToHiding` 30 and `daysFromHidingToDeleting` 1).
2. In B2 make an application key for that bucket only, with **Read and Write** (Application Keys, Add a New Application Key). It can hide a file, but in Compliance mode no key, yours included, can delete a locked copy before its 30 days end, and a hidden copy can still be fetched.
3. Make the rclone remote with that key: `mkdir -p ~/.config/rclone && cd ~/brim && docker compose run --rm -v ~/.config/rclone:/config/rclone rclone config`, choose `n` (new remote), name `b2`, storage `b2`, and paste the key id and key.
4. Set in `.env` (`b2:` is the remote name, then the bucket):

```bash
RCLONE_REMOTE=b2:brim-backups
```

Each run uploads the encrypted copies the bucket does not have yet, so a copy a failed day could not send goes with the next one. Leave `RCLONE_REMOTE` unset to keep local copies only.

### Schedule

Test with `~/brim/scripts/backup.sh`, then add to `crontab -e`:

```bash
0 4 * * * ~/brim/scripts/backup.sh
```

### Restore test

On the 1st of each month `backup.sh` runs `scripts/restore-test.sh` on the new dump before it encrypts it, so no private key is needed on the server and nothing is needed from you. The test restores the dump into a throwaway Postgres container (the same image as the live one), runs the deletion log step of a real restore on it, checks that the main tables exist and that `guild_config` is not empty, and writes the row counts to `~/brim-backups/restore-test.log`. If it fails, the copy is still saved and encrypted, and `backup.sh` fails so cron reports it. With the key set, the plain dump exists only during the test (readable only by you) and is deleted once encrypted.

What it proves: that day's dump restores. What it does not prove: that your private key opens the encrypted copies (that is checked only when you restore with it). To test an encrypted copy by hand: `AGE_KEY=<file with the private key> scripts/restore-test.sh` (newest copy) or add a file name.

An older setup had its own cron line for `restore-test.sh`; it is no longer needed, and fails on encrypted copies. Remove it: `crontab -l | grep -v restore-test.sh | crontab -`.

### Restoring for real

Put the private key in a file for the restore only, and delete the key file after:

```bash
(umask 077; nano /dev/shm/age.key)   # paste the AGE-SECRET-KEY-1... line; /dev/shm is memory, not disk
cd ~/brim && AGE_KEY=/dev/shm/age.key scripts/restore.sh ~/brim-backups/2026-11-01.sql.gz.age; rm /dev/shm/age.key
```

Without the key, with another key, or with a damaged file the script stops before it changes anything and says why. A copy made before the key was set (`.sql.gz`) restores without `AGE_KEY`.

The script checks that the file is complete, stops the bot and the dashboard (so they do not write during the restore or keep cached data from before it), replaces the whole schema in one transaction that stops at the first error (a damaged dump changes nothing; if it stops, `docker compose up -d` starts the bot on the database as it was), and starts everything again.

Deletions made after the dump are applied again (security review): before the restore the script reads the live database's deletion log (migration 4035: purged servers, removed Extra Owners, Trusted Admins, permits, permit holders, permit roles and Raider List allows, replaced or deleted rescue keys, reset dashboard 2FA, revoked MCP tokens, carried-out data deletion requests; ids only, kept 35 days), and after it removes those grants and servers from the restored data again. Every dashboard login ends, so everyone logs in again. Not applied again (they come back as they were in the dump): user IDs removed from the quarantine or anti-nuke whitelists in the settings, cases, notes and other records deleted after the dump. A person's data deletion request that was carried out after the dump is queued again: the bot runs it while the bot owner who approved it is still the bot owner (the person gets the DM again), otherwise it waits on the Data requests page. A dump from before 19 Nov 2026 (no data requests yet) cannot queue it: the restore prints a WARNING with the user ID, and you run that request again by hand. A permit renamed after the dump counts as deleted, so the restore removes it with its holders: add it again. If the live database cannot be read, the script stops and starts the bot again without restoring. On a new server (an empty Postgres) there is no log, so nothing is applied again. A dump older than 35 days can bring back deletions older than the log.

An off-site copy: fetch it, then restore it the same way:

```bash
cd ~/brim && docker compose run --rm -v ~/.config/rclone:/config/rclone -v ~/brim-backups:/data rclone copy b2:brim-backups/2026-11-01.sql.gz.age /data
(umask 077; nano /dev/shm/age.key)   # paste the AGE-SECRET-KEY-1... line
AGE_KEY=/dev/shm/age.key scripts/restore.sh ~/brim-backups/2026-11-01.sql.gz.age; rm /dev/shm/age.key
```

To undo an update together with its migrations, pin the previous version (see Rolling back), then restore the dump the updater made just before that update: `sudo scripts/restore.sh ~/brim/pre-update/pre-update-<id>.sql.gz` (`<id>` is the update's number; the newest three are kept; the files are readable only by root).

## 9. Stolen Discord account

There is no second person who can take over, so these steps are yours alone. The bot DMs you a security alert each time a Raider List entry is confirmed, an update is started, or an MCP token is made or revoked. If you get one you did not cause, or you lose your Discord account, act at once.

**First choice: the dashboard.** Press **Secure my account** on the alert (or open the account menu > **Secure my account**). It needs your dashboard 2FA and the Rescue Key of a server where you hold the Owner level, made after you became its owner, with the code of that key's own authenticator entry. One press logs out every dashboard session, cancels a queued update, revokes every MCP token and closes the changes waiting for a code. It does not remove your 2FA, and it does not use up the Rescue Key: make a new key for that server afterwards, because you typed it in while your login may be watched. Then do steps 3, 4 and 6 below.

**When the dashboard cannot be used** (no 2FA, no Rescue Key, lost phone), use the steps below. Everything here runs on the server over SSH, which does not need Discord.

1. **(server)** Stop the dashboard, the MCP server and the updater, so nobody can use a stolen login and no update someone queued can run while you work:

   ```bash
   cd ~/brim && docker compose stop dashboard mcp updater
   ```

2. **(server)** Log out every dashboard session, cancel a queued update, revoke every MCP token (this also closes the changes waiting for a code) and remove your dashboard 2FA, which someone with your session may have set up again. Replace `<your id>` with your Discord user id:

   ```bash
   docker compose exec postgres psql -U brim -d brim -c "delete from dash_session; update update_runs set status = 'failed' where status = 'queued'; update mcp_tokens set revoked_at = now where revoked_at is null; update mcp_writes set status = 'revoked', body = null where status = 'pending'; delete from dash_totp where user_id = <your id>;"
   ```

3. **(server)** Log every OAuth login out: run `nano .env`, replace the value after `MCP_OAUTH_SECRET=` with the output of `openssl rand -hex 32`, save and close.
4. Get your Discord account back with Discord's password reset or support, and turn on Discord's own 2FA. If the account owns the bot's Discord application, open the Developer Portal: **Bot > Reset Token** and **OAuth2 > Reset Secret**. Put the new values in `.env` as `DISCORD_TOKEN=` and `DISCORD_CLIENT_SECRET=`.
5. **(server)** Only once step 4 is done (else whoever has the account can log in first and set up 2FA as you), start everything again: `docker compose up -d`. Log in to the dashboard and set up 2FA again. Make new MCP tokens for the places that need them (step 6.1).
6. Check what happened while the account was gone: the **Update** page (an update you did not start: **Install** the build you trust), each server's **History** page (config changes and AI reads; **Roll back** where needed), and the Raider List (remove entries you did not confirm).
