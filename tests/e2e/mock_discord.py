"""Minimal fake of the Discord REST API the dashboard calls, so the e2e run needs no token.
Point the dashboard at it with DISCORD_API_BASE=http://127.0.0.1:9100. Stdlib only.
The OAuth code picks the user: `x` is the guild owner, the others (see USERS) test lower access levels."""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

GUILD = {"id": "100000000000000001", "name": "E2E Server", "icon": None}
GUILD2 = {"id": "100000000000000002", "name": "Second Server", "icon": None}
GUILDS = {g["id"]: g for g in (GUILD, GUILD2)}
USER = {"id": "200000000000000001", "username": "e2e-owner"}
BOT = {"id": "1", "username": "Brim", "bot": True}
# code -> (user, role ids). Roles 4000000000000000xx get permits in tests/e2e/levels.sql.
USERS = {
    "x": (USER, []),
    "eo": ({"id": "200000000000000002", "username": "e2e-extra-owner"}, []),
    "ta": ({"id": "200000000000000003", "username": "e2e-trusted-admin"}, []),
    "editor": ({"id": "200000000000000004", "username": "e2e-editor"}, ["400000000000000001"]),
    "viewer": ({"id": "200000000000000005", "username": "e2e-viewer"}, ["400000000000000002"]),
    "cases": ({"id": "200000000000000006", "username": "e2e-cases"}, ["400000000000000003"]),
    "none": ({"id": "200000000000000007", "username": "e2e-none"}, []),
}
ROLES_OF = {u["id"]: r for u, r in USERS.values()}
ROLES = [
    {"id": "400000000000000001", "name": "Editors", "position": 3, "permissions": "0"},
    {"id": "400000000000000002", "name": "Viewers", "position": 2, "permissions": "0"},
    {"id": "400000000000000003", "name": "Case staff", "position": 1, "permissions": "0"},
]
sent = []  # messages the dashboard posted, for assertions
missing = set()  # GET paths the mock has no answer for, printed to help extend it


class H(BaseHTTPRequestHandler):
    def reply(self, body, code=200):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def user(self):
        """The user of the request's token, or None: an unknown token must not log in as the owner."""
        tok = (self.headers.get("Authorization") or "").removeprefix("Bearer mock-")
        return USERS[tok][0] if tok in USERS else None

    def do_GET(self):
        p = self.path.split("?")[0]
        parts = p.split("/")
        gid = parts[2] if len(parts) > 2 and parts[1] == "guilds" else None
        if p == "/users/@me" and (self.headers.get("Authorization") or "").startswith("Bot"):
            self.reply(BOT)
        elif p == "/users/@me":
            u = self.user()
            self.reply(u) if u else self.reply({"message": "401: Unauthorized", "code": 0}, 401)
        elif p.startswith("/users/") and p.count("/") == 2 and p.split("/")[2].isdigit():
            uid = p.split("/")[2]
            self.reply(next((u for u, _ in USERS.values() if u["id"] == uid), {"id": uid, "username": f"user{uid[-2:]}"}))
        elif p == "/users/@me/guilds":
            self.reply(list(GUILDS.values()))
        elif p == "/applications/@me":
            self.reply({"id": "1", "owner": USER})
        elif p == "/sent":
            self.reply(sent)
        elif p == "/missing":
            self.reply(sorted(missing))
        elif gid in GUILDS and len(parts) == 3:
            roles = [{"id": gid, "name": "@everyone", "position": 0, "permissions": "0"}, *ROLES]
            self.reply({**GUILDS[gid], "owner_id": USER["id"], "approximate_member_count": 7, "roles": roles})
        elif p.startswith("/channels/") and "/messages/" in p and p.count("/") == 4:
            # a member's message a trigger answers: still there
            self.reply({"id": parts[-1], "channel_id": parts[2]})
        elif p.startswith("/channels/") and p.count("/") == 2:
            self.reply({"id": p.split("/")[2], "guild_id": GUILD["id"], "type": 0})
        elif p.endswith("/channels"):
            self.reply([
                # Brim may post here (View Channel, Send Messages, Embed Links for its own member)
                {"id": "300000000000000001", "name": "general", "type": 0, "position": 0,
                 "permission_overwrites": [{"id": "1", "type": 1, "allow": "19456", "deny": "0"}]},
                # a public category the log setup reuses; it must turn private
                {"id": "300000000000000002", "name": "Logs", "type": 4, "position": 1},
            ])
        elif p.endswith("/emojis") and gid:
            self.reply([{"id": "400000000000000001", "name": "tb_wave", "animated": False}])
        elif p == "/applications/1/emojis":
            self.reply({"items": []})
        elif p.endswith("/roles"):
            self.reply([{"id": gid or GUILD["id"], "name": "@everyone", "position": 0, "permissions": "0"}, *ROLES])
        elif p.endswith("/auto-moderation/rules"):
            self.reply([])
        elif "/members/search" in p:
            self.reply([{"user": u, "roles": r} for u, r in USERS.values()])
        elif "/members/" in p:
            uid = parts[-1]
            if uid == BOT["id"]:
                # the Raider List report checks how long the bot has been in the server
                self.reply({"user": BOT, "roles": [], "joined_at": "2026-01-01T00:00:00+00:00"})
            elif uid in ROLES_OF:
                u = next(u for u, _ in USERS.values() if u["id"] == uid)
                self.reply({"user": u, "roles": ROLES_OF[uid]})
            else:
                self.reply({"roles": []})
        else:
            if p not in missing:
                missing.add(p)
                print(f"mock: no answer for GET {p}", file=sys.stderr)
            self.reply({}, 404)

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n)
        if self.path == "/oauth2/token":
            code = dict(x.split("=", 1) for x in body.decode().split("&")).get("code", "x")
            self.reply({"access_token": f"mock-{code}"})
        else:
            sent.append({"path": self.path, "method": self.command, "body": body.decode()})
            if self.command == "POST" and self.path.endswith("/webhooks"):
                self.reply({"id": "500000000000000001", "token": "mock-webhook-token"})
            elif self.command == "POST" and self.path.endswith(("/roles", "/channels")):
                # Each made role or channel its own id: Auto Setup refuses one role for two levels.
                self.reply({"id": str(600000000000000000 + len(sent))})
            else:
                self.reply({"id": "1"})

    do_PUT = do_PATCH = do_POST

    def do_DELETE(self):
        sent.append({"path": self.path, "method": "DELETE"})
        self.send_response(204)
        self.end_headers()

    def log_message(self, *a):
        pass


# The port is the first argument (9100 by default): run.sh starts a second mock for the matrix.
HTTPServer(("127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 9100), H).serve_forever()
