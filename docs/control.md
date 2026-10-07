# Arugula control

Arugula control lets you reach your machines from any device without a
tailnet. It is also where accounts and devices live. The hosted one is at
<https://control.arugula.io>; you can run your own from this
repository (below).

**What it can and can't see.**

- **It sees:** who you are, which devices and machines you have, and when
  they connect.
- **What it relays, it can't read.** Every connection between a device and
  a machine is end-to-end encrypted, including connections it relays, and
  it never holds a private key. A copy of its database or its traffic
  reads nothing.
- **It can't add a device that reads them:**
  - every device and machine is approved by a device you already have;
  - your devices and machines check those approvals themselves;
  - when a machine joins, you check that it shows your account's
    fingerprint, so control can't hand it an account of its own.
- **What you trust it with:**
  - **the web client.** Control serves the page your browsers, your phone
    and the desktop app (once joined) run. A control that served a
    modified page could read what that page shows. A daemon's own page,
    `arugulad` and the CLI don't come from control.
  - **hosted sandboxes.** They run on control's provider, which writes
    their trust files; the operator can read them.
- **Huddles** (voice calls) don't go through control. Control hands
  machines short-lived TURN credentials (from Cloudflare, for the hosted
  control); a relayed call's audio is encrypted end to end, so the relay
  sees only addresses and volume.

**What it costs.** The hosted control is free during the beta. It's
provided as is, without guarantees, and its pricing may change; any change
is announced before it applies. Its [terms](https://arugula.io/terms) and
[privacy notice](https://arugula.io/privacy) say what it keeps
and the rules; questions to <privacy@arugula.io>.

The design, and exactly what holds if control itself turns hostile, is in
[control-e2e.md](control-e2e.md#what-holds-against-control). Teams, roles, personal
vs team machines and sharing a session:
[Your machines, your team](teams.md).

## Using it

1. **Sign in** at control's page, with GitHub or a passkey (a passkey can
   also make an account by itself).
   - The first browser you use becomes your account's first device.
   - It shows two **recovery codes** once. Keep them offline: if you lose
     every device, one of them lets a new browser in, once. *Devices and
     machines…* says how many are left; *Make new codes* there replaces
     them (the old ones stop working).
2. **Add a machine.** Install Arugula on it, then run:

   ```
   arugulad join https://control.arugula.io
   ```

   It prints a link with a code (good for 15 minutes). Open it on a
   signed-in device, check the code matches, and approve. The machine then
   shows your account's fingerprint (`1a2b-3c4d-…`): check it's the one
   the approving device showed (*Your account*, also under *Devices and
   machines…*) and answer `y` (Getting started on the machine's own page
   asks the same with two buttons). It trusts nothing until you do;
   `--account FINGERPRINT` answers ahead, for scripts. *Join to* picks
   your account (*Just me*) or a team you own; `--team ID` picks the team
   ahead. *Cancel* turns it down. A running daemon connects within a few
   seconds, and the machine appears in the host menu. A machine runs one
   join at a time: while Getting started's waits, `arugulad join` says
   which code it is and where to approve it, and the other way round.
   An approval is never lost to a second request from the same machine.
   If control refuses an approval, the page says which check failed and
   what to do; closing it then doesn't turn the machine down.
   The machine's approval is the only one it needs. If the browser you
   open its link in isn't one of your devices yet, it can't approve the
   machine: it says so, shows the link to open on a device you already
   use, and asks to be approved itself. That device then shows the machine
   and the browser side by side, and *Approve both* lets both in (untick
   the browser to leave it out: the machine joins either way). Each is
   still a device approval, signed by the device that approves it.
3. **Add your phone** (or any other browser): *Add a phone or browser…* in
   the host menu shows control's address as a QR code and a link. Sign in
   there. It shows a fingerprint and waits. Your devices ask *New device?*
   with the same fingerprint; approve it on one of them.
4. **Sign in the desktop app** (optional). Once its machine has joined
   (*Getting started*'s *Cloud* step, or `arugulad join`), the app
   offers to sign in, so its window reaches your other machines too; the
   machine needs nothing more, and *Just this machine for now* skips it.
   A machine control dropped counts as not joined: the app opens its own
   page with *Getting started*'s join, not the sign-in. The window can't
   use passkeys, so the app signs in through your browser:
   - *Sign in* in the app opens control in your browser and shows a short
     code;
   - signed in there, control asks *Sign in the app?* with the machine's
     name and the same code: check it matches, then *Allow*;
   - the app is then a new device: approve it, with its fingerprint, on a
     device you already have (the browser you just used is one).

   It then shows every machine in your account and your teams, like any
   other device. The sign-in link is good for ten minutes and works once.
5. **Use the command line.** `arugula login` makes the CLI one of your
   devices: it shows a link with a code, you approve it on a signed-in
   device (*Add a terminal?*), and it shows your account's fingerprint to
   check, as a machine does (`--account FINGERPRINT` answers ahead). Then
   `arugula hosts` lists your machines from control (next to the local
   daemon's own list), and `arugula --host NAME run|ls|capture …`
   reaches any of them, directly when it lists a URL that answers, else
   through the relay, with nothing in `hosts.json`. `ARUGULA_VERBOSE=1`
   says which. `attach`, `tui` and `--follow` work the same way, and
   your teams' machines and those shared with you are listed and reached
   too ([cli.md](cli.md)).
   `arugula logout` forgets the CLI's key; remove it under *Devices and
   machines…* to revoke it.
6. **Remove a device or machine** from *Devices and machines…* in the host
   menu. It loses access at once. A removed machine keeps running
   Arugula, reachable only locally. Its key never counts again, so when
   control says it was removed, the machine sets the key aside
   (`daemon.key.removed-…`) and asks to join again with a new one: a new
   code to approve, shown in *Getting started* and by `arugulad join`.
   Approved into the same account, it's back without checking the
   fingerprint again.
   A removed browser says so when it opens control's page, and offers to
   forget its key and enroll again, approved by another device or a
   recovery code.

**From the desktop app, first run or after a drop**, it's one
sequence:

1. Install the app and open it. It starts the daemon and shows the
   daemon's own page; Getting started opens once.
2. Join the machine: Getting started's *Cloud* step ("Add … to your
   account or team", or "Put … back" after a drop) shows a code. Approve
   it on a device you already use, picking your account or a team there,
   then check the account's fingerprint (*They match*). That's the one
   approval the machine needs. After a drop, the app opens this step by
   itself, once.
3. Sign the app in (optional): its window then reaches your other
   machines too. It's a second approval, of the app's window as a device;
   the sign-in page says so, and *Just this machine for now* skips it.
4. The machine is in the host menu of your devices and, by role, your
   team's.

**How a device reaches a machine:**

- **Directly, when it can.** The machine's tailnet name, or any
  `--direct-url` the daemon was given.
- **Otherwise through control's relay.** The host menu says which:
  "direct" or "relayed".
- Chrome asks once for permission to reach your local network when a
  machine has a tailnet or LAN address. Without that permission, it uses
  the relay.

A joined machine's own page doesn't list your other machines (that would
make it a hub): its host menu has *All your machines…*, which opens
control's page.

**Leaving.** `arugulad leave` takes a machine off your account (or its
team). Arugula keeps running there, at `http://127.0.0.1:7681`. The
daemon's log says it left, so a leave reads differently from a removal.

**Dropped by control.** If control stops knowing a machine (it left, it
was removed from a browser, its account was deleted), the daemon notices
within a minute or two: control refuses its certificate refresh, or its
relay, and asking again says it has no such machine. Then:

- the machine's page shows a banner, *This machine is no longer in …*,
  with *Join again* (Getting started's join, to the same control), and
  the host menu says *Dropped by control*;
- the desktop app posts a notification, once per drop; a click opens
  Getting started at the join;
- `arugula status` says so and exits 1;
- the daemon logs what control said, and keeps it (and when) in
  `<state>/control-dropped.json`.

A machine removed from a browser has a key that never counts again, so
it sets the key aside and asks to join again with a new one by itself
(above): the banner, Getting started and `arugula status` show that
join's code. Otherwise its `control.json` stays (control could be
wrong): the daemon stops redialling the relay and asks again every 10
minutes, until you join again (the old enrollment is set aside as
`control.json.dropped`) or run `arugulad leave`. If `control.json`
disappears while the daemon runs and nothing of Arugula's removed it,
the log says that too.

`GET /api/host`'s `control_state` (for the machine's owner only) has all
of this: `state` (`not_joined`, `joined` or `dropped`), `url`, `kind`
(`account` or `team`), `name`, `connected`, `seen_ms`, `error`, and for a
drop `said`, `dropped_ms`, and the `code` and `approve` link of a join
waiting for approval. `GET /api/setup?part=control` has it as
`control.state`.

**Your sign-ins, and deleting your account:** *Sign-in and account…* in
the host menu.

- **Where you're signed in:** each session, by browser and when it
  started. *Sign out* ends one; *Sign out everywhere* ends them all, this
  one too. Signing out doesn't remove a device: its key still reaches your
  machines until you remove it in *Devices and machines…*. Expired
  sessions are deleted within the hour.
- **Passkeys:** remove one, as long as another way to sign in is left
  (another passkey, or GitHub).
- **Delete account…** asks you to type your GitHub login (or your name,
  for a passkey-only account). Control then deletes your account, its
  GitHub link, sessions, passkeys, devices and machines (they're refused
  from then on; Arugula keeps running on them, reachable only locally),
  pending joins, push subscriptions, hosted VMs (deleted), usage counts,
  team requests and the invites you made. Your open relay connections
  close.
  - **A team you founded is deleted with it.** Its signed history starts
    at your first device's key, so it can't outlive your account. Its
    other members are told; its machines go back to their owners.
  - **In someone else's team** as a plain member, you can delete right
    away: the roster keeps your name, with no device behind it (so it
    lets nothing in), until an owner removes it. Its owners are told.
  - **As an owner of someone else's team**, or if your devices ever signed
    its roster, leave it first (make someone else an owner if it's only
    you). If your devices signed part of its history, control keeps those
    devices' certificates (public keys and device names, nothing else) to
    check that history, until the team is deleted.
  - **Paying for a plan?** Cancel it first.
  - The hosted control's backups keep a deleted account for up to a week
    more (see below).

**Moving a machine** between your account and a team (or between teams):
*In …* on it in *Devices and machines…*, into any team you're in. Your
device signs the move and the machine checks that signature, so control
can't move a machine by itself. A team's owners can take a member's
machine out of the team (*Take out* in *Teams…*): the machine checks that
an owner of the team, in the member list it checked, signed it, and takes
nothing else from them. An offline machine moves when it next connects.

**What isn't here yet:** the CLI (`arugula`) still reaches only the local
daemon, or others over the tailnet.

## Running your own

`arugula-control` is one static binary with an SQLite database. Put TLS
in front of it: Caddy, Fly, or `tailscale serve`.

```
arugula-control --public-url https://control.example.com --listen 127.0.0.1:7690 --db /var/lib/arugula/control.db
```

- **Moving to another URL:** run with `--public-url` set to the new
  one and `--also-url` (`ARUGULA_CONTROL_ALSO_URLS`, comma-separated) to
  the old. Both answer: a browser is served as the site it came in on, with
  that site's own origin, cookies and passkeys (a passkey works only where
  it was made), and its page on an old URL says where control is now.
  Daemons and CLIs work at any of them. Register the GitHub callback for
  each URL (an OAuth app takes one host; a GitHub App takes several). Once
  people have signed in and approved their browsers at the new URL,
  `--also-redirect` (`ARUGULA_CONTROL_ALSO_REDIRECT=true`) sends browsers
  from an old URL's pages to the new one, while daemons, the API and
  sign-ins already under way are still answered there.
- **Sign-in:**
  - **Passkeys** work whenever control has a domain name (WebAuthn refuses
    IP addresses).
  - **GitHub**: register a GitHub App (or OAuth app) with the callback
    `https://control.example.com/auth/github/callback`, then set
    `GITHUB_CLIENT_ID` and `GITHUB_CLIENT_SECRET`.
- **The GitHub App** (optional): forge blocks' live updates from
  GitHub, and read access for hosted boxes with no `gh` login. Make it
  with GitHub's manifest flow (or by hand at *Settings → Developer settings
  → GitHub Apps*):
  - **Permissions**, all read-only: metadata, contents, pull requests,
    issues, checks, commit statuses, actions.
  - **Events:** pull request, pull request review, pull request review
    comment, issues, issue comment, check run, check suite, status,
    workflow run.
  - **Webhook URL** `https://control.example.com/github/webhook`, with a
    secret. **Callback URL** `https://control.example.com/auth/github/callback`
    if it also signs people in.
  - Install it on the accounts (or organizations) whose repositories you
    want live, then give control its id, slug, client id and secret,
    webhook secret and private key: `GITHUB_APP_ID`, `GITHUB_APP_SLUG`,
    `GITHUB_APP_CLIENT_ID`, `GITHUB_APP_CLIENT_SECRET`,
    `GITHUB_APP_WEBHOOK_SECRET`, and `GITHUB_APP_PRIVATE_KEY_FILE` (a
    `.pem` path) or `GITHUB_APP_PRIVATE_KEY` (the PEM itself; `\n` for
    newlines works). On Fly:
    `fly secrets set GITHUB_APP_ID=… GITHUB_APP_SLUG=… GITHUB_APP_CLIENT_ID=… GITHUB_APP_CLIENT_SECRET=… GITHUB_APP_WEBHOOK_SECRET=… GITHUB_APP_PRIVATE_KEY="$(cat app.pem)"`.
  - With no `GITHUB_CLIENT_ID`/`GITHUB_CLIENT_SECRET`, the App's client id
    and secret sign people in with GitHub.
  - **What control keeps of a webhook:** the signature is checked
    (HMAC-SHA256, constant time), the delivery id deduplicated, and only
    `{provider, host, repo, number?, event, delivery}` goes to daemons,
    over their relay sockets. Its log has the event, delivery, repository
    and how many daemons heard; never the payload.
  - **Who hears what:** a daemon's account must have signed in with GitHub,
    and the App's installation for the repository must be that GitHub
    user's, or GitHub must list them as a collaborator on it (asked with
    an installation token; answers kept ten minutes). Both go by the
    numeric GitHub user id, not the login. Organization membership alone
    doesn't count, and passkey-only accounts hear nothing: their blocks
    poll. A machine watches at most 200 repositories and an account 400.
  - **Hosted boxes** ask `POST /api/daemon/github/token {repo}` (signed as
    the daemon) for an installation token scoped to that repository with
    read-only permissions; control keeps one until five minutes before it
    expires.
- **Billing** (optional) needs both `STRIPE_SECRET_KEY` and
  `STRIPE_WEBHOOK_SECRET`; with the key alone control doesn't start.
- **Behind a proxy** that passes the client's address in a header, use
  `--trust-proxy-header` (for example `Fly-Client-IP`), so rate limits are
  per client (an IPv6 client counts by its /64).
- **Daemons from before 0.17** sign their requests to control the old
  way. Control takes that, each signature once, unless it's started with
  `--refuse-old-daemon-signatures` (`ARUGULA_CONTROL_REFUSE_OLD_DAEMON_SIGNATURES=1`),
  when those daemons are told to update. A machine control already knows
  joins again only from 0.17 on.
- **Build it** with `just static` (`target/x86_64-unknown-linux-musl/release/arugula-control`).
  - The web client is built into the binary.
  - `--static-dir web/dist` serves a local build instead.
- **The hosted one** is `packaging/control/` on Fly: `just control-deploy`.

- **Relay limits per account**, so one account can't take the whole
  machine (0 turns each off):
  - `ARUGULA_RELAY_MAX_SOCKETS` (32): client connections through the
    relay at once (a page uses one for all its machines).
  - `ARUGULA_RELAY_MAX_MACHINES` (50): machines dialed in at once.
  - `ARUGULA_RELAY_DAILY_MB` (2000): relayed traffic a day, while
    billing is off. Past it the account's relayed traffic slows to about
    64 KB/s, as billing's allowance does. Direct connections don't count.
- **A ceiling on the relay,** every account's sockets together:
  `ARUGULA_RELAY_MAX_TOTAL` (5000; 0 for none). Keep it below the
  proxy's connection limit, so a full relay still leaves room for
  control's pages and sign-ins. Past it a new relay socket is refused
  (what's open stays): daemons and the CLI get `503` with
  `Retry-After: 30`, and a browser's socket closes at once with code
  1013 and the reason. Daemons wait that long and up to as long again;
  a page says control is full and waits 30 to 60 seconds.
- **Backups:** set `LITESTREAM_BUCKET` and control backs its database up
  continuously with Litestream (below).

Daemons join a self-hosted control the same way:
`arugulad join https://control.example.com`. To have Getting started's
*Connect* button join it too, start the daemon with
`--control https://control.example.com` (or `ARUGULA_CONTROL`).

## Operating the hosted one

The hosted control is one Fly machine (`packaging/control/fly.toml`):
`shared-cpu-1x` with 1 GB, in `ewr`, SQLite on the `control_data` volume.

**Capacity.** An idle relay connection costs control about 28 KB: in a
1 GB VM, 3,900 of them took 121 MB. So the connection limits in
`fly.toml` (6,000 soft, 8,000 hard) are about open files, not memory.
Control raises its open-file limit to the hard one at start and logs it
(`open file limit open_files=…` in `fly logs`); keep `hard_limit` below
that. Each account is held to the relay limits above, and the relay as
a whole to its ceiling (5,000, under Fly's soft limit, so pages and
sign-ins still get through when it's full). Each minute it changed,
`fly logs` has `relay sockets` with the count (`sockets`, `daemons`,
`clients`), the ceiling (`max`) and how many were refused for it
(`refused`); `the relay is full` and `the relay has room again` mark
when it fills and empties.

**Backups (Litestream to Cloudflare R2).** Off until its secrets are set;
then:

- on start, with no database on the volume (a new or lost volume),
  control restores the latest backup first. If the bucket can't be
  reached it doesn't start, rather than start empty;
- it runs `litestream replicate` beside itself (in the image at
  `/litestream`): changes reach R2 within about a second. A snapshot a
  day, each kept 7 days, so a point-in-time restore reaches back a week,
  and anything deleted is gone from the backup within a week;
- on a stop it lets Litestream sync one last time.

To turn it on:

1. In Cloudflare: *R2 → Create bucket* (say `illogical-control-backup`,
   automatic location). Then *R2 → Manage API tokens → Create API token*:
   *Object Read & Write*, for that bucket only. Note the access key id,
   the secret, and the S3 endpoint
   (`https://<account id>.r2.cloudflarestorage.com`).
2. Set the secrets (Fly restarts the machine with them):

   ```
   fly secrets set -a arugula-control \
     LITESTREAM_BUCKET=illogical-control-backup \
     LITESTREAM_ENDPOINT=https://<account id>.r2.cloudflarestorage.com \
     LITESTREAM_ACCESS_KEY_ID=… LITESTREAM_SECRET_ACCESS_KEY=…
   ```

   `LITESTREAM_PATH` (default `control`) is the prefix in the bucket;
   `LITESTREAM_REGION` defaults to `auto`.
3. Check `fly logs`: `backing up with Litestream`, then Litestream's own
   `replicating to` and `snapshot complete` lines.

**Restoring.**

- **Onto a new volume:** create it (`fly volumes create control_data -r ewr
  -s 1`), point the machine at it (or destroy the old volume), and start
  the machine. Control finds no database and restores the latest backup.
- **To look at a backup, or go back in time,** on any machine with
  `litestream` and the same four values in its environment:

  ```
  cat > ls.yml <<EOF
  dbs:
    - path: /tmp/control.db
      replica: {type: s3, bucket: illogical-control-backup, path: control, region: auto, endpoint: "https://<account id>.r2.cloudflarestorage.com", force-path-style: true}
  EOF
  LITESTREAM_ACCESS_KEY_ID=… LITESTREAM_SECRET_ACCESS_KEY=… litestream restore -config ls.yml -o /tmp/control.db /tmp/control.db
  ```

  Add `-timestamp 2026-10-04T12:00:00Z` for a point in time. To put it
  back: stop the machine, copy the file over `/data/control.db` (and
  delete `control.db-wal`, `control.db-shm` and `.control.db-litestream`
  beside it), start it.
- **Restored once to prove it** (2026-10-04, staging): a control in one VM
  replicated to an S3-compatible store in another; the VM was destroyed;
  a fresh VM with an empty disk restored on start, and every account,
  device and machine was there, and replication carried on.

Fly's own daily volume snapshots (kept 5 days) still run.

**TURN for huddles.** Machines ask control for TURN credentials for
their huddles (`GET /api/daemon/turn`), and control asks Cloudflare's TURN
service for short-lived ones (8 hours; a machine reuses them for an hour).
Without a key, machines get Cloudflare's public STUN only, which is enough
unless both ends are behind strict NATs. In the Cloudflare dashboard,
*Realtime → TURN Server → Create*, then:

```
fly secrets set -a arugula-control \
  CLOUDFLARE_TURN_KEY_ID=… CLOUDFLARE_TURN_API_TOKEN=…
```

`fly logs` says `no CLOUDFLARE_TURN_KEY_ID/CLOUDFLARE_TURN_API_TOKEN` at
start when they're missing. A self-hosted control can run coturn instead
(not wired up yet).

**A spend alert** isn't something this repository sets: it belongs to
the Fly organization's billing settings in Fly's dashboard (it's the
operator's to set).

## Testing

- `just control-smoke` runs it end to end without a browser:
  - a fake GitHub sign-in, then enrolment and approval;
  - a daemon joining by code;
  - its API and protocol through the relay and directly;
  - a check that control's wire traffic, database and logs never contain
    what was typed;
  - deleting an account: its machine is refused, and no row in the
    database mentions it.
- `web/e2e/control.spec.ts` and `web/e2e/passkey.spec.ts` drive it in Chrome.
- `web/e2e/device-keys.webkit.spec.ts` drives it in Playwright's WebKit
  (`pnpm exec playwright install webkit` first): device keys survive a
  reload, a join is approved from a later page load, and a browser that
  lost its key enrolls again.
- **Safari itself:** open `/key-probe.html` on control (or any daemon) in
  Safari. It stores a set of device keys, reloads, and says whether they
  still work, and which Safari it is.
- Control logs every refused enrolment and approval (`refused`, with the
  request, the reason, and the account, device and approver ids; never a
  signature or a body), so `fly logs` shows why.
