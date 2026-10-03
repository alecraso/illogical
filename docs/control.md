# illogical control

illogical control lets you reach your machines from any device without a
tailnet. It is also where accounts and devices live. The hosted one is at
<https://control.illogical.widgets.wtf>; you can run your own from this
repository (below).

**What it can and can't see.**

- **It sees:** who you are, which devices and machines you have, and when
  they connect.
- **It never sees what your terminals say.** Every connection between a
  device and a machine is end-to-end encrypted, including connections it
  relays.
- **It can't add a device that reads them:**
  - every device and machine is approved by a device you already have;
  - your devices and machines check those approvals themselves.

The design is in [control-e2e.md](control-e2e.md).

## Using it

1. **Sign in** at control's page, with GitHub or a passkey (a passkey can
   also make an account by itself).
   - The first browser you use becomes your account's first device.
   - It shows two **recovery codes** once. Keep them offline: if you lose
     every device, one of them lets a new browser in, once. *Devices and
     machines…* says how many are left; *Make new codes* there replaces
     them (the old ones stop working).
2. **Add a machine.** Install illogical on it, then run:

   ```
   illogicald join https://control.illogical.widgets.wtf
   ```

   It prints a link with a code. Open it on a signed-in device, check the
   code matches, and approve. A running daemon connects within a few
   seconds, and the machine appears in the host menu.
3. **Add your phone** (or any other browser): *Add a phone or browser…* in
   the host menu shows control's address as a QR code and a link. Sign in
   there. It shows a fingerprint and waits. Your devices ask *New device?*
   with the same fingerprint; approve it on one of them.
4. **Remove a device or machine** from *Devices and machines…* in the host
   menu. It loses access at once. A removed machine keeps running
   illogical, reachable only locally; `illogicald join` adds it back.

**How a device reaches a machine:**

- **Directly, when it can.** The machine's tailnet name, or any
  `--direct-url` the daemon was given.
- **Otherwise through control's relay.** The host menu says which:
  "direct" or "relayed".
- Chrome asks once for permission to reach your local network when a
  machine has a tailnet or LAN address. Without that permission, it uses
  the relay.

**Leaving.** `illogicald leave` takes a machine off your account.

**What isn't here yet:** the CLI (`illogical`) still reaches only the local
daemon, or others over the tailnet.

## Running your own

`illogical-control` is one static binary with an SQLite database. Put TLS
in front of it: Caddy, Fly, or `tailscale serve`.

```
illogical-control --public-url https://control.example.com --listen 127.0.0.1:7690 --db /var/lib/illogical/control.db
```

- **Sign-in:**
  - **Passkeys** work whenever control has a domain name (WebAuthn refuses
    IP addresses).
  - **GitHub**: register a GitHub App (or OAuth app) with the callback
    `https://control.example.com/auth/github/callback`, then set
    `GITHUB_CLIENT_ID` and `GITHUB_CLIENT_SECRET`.
- **The GitHub App** (M40, optional): forge blocks' live updates from
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
    an installation token; answers kept ten minutes). Organization
    membership alone doesn't count, and passkey-only accounts hear
    nothing: their blocks poll.
  - **Hosted boxes** ask `POST /api/daemon/github/token {repo}` (signed as
    the daemon) for an installation token scoped to that repository with
    read-only permissions; control keeps one until five minutes before it
    expires.
- **Behind a proxy** that passes the client's address in a header, use
  `--trust-proxy-header` (for example `Fly-Client-IP`), so rate limits are
  per client.
- **Build it** with `just static` (`target/x86_64-unknown-linux-musl/release/illogical-control`).
  - The web client is built into the binary.
  - `--static-dir web/dist` serves a local build instead.
- **The hosted one** is `packaging/control/` on Fly: `just control-deploy`.

Daemons join a self-hosted control the same way:
`illogicald join https://control.example.com`.

## Testing

- `just control-smoke` runs it end to end without a browser:
  - a fake GitHub sign-in, then enrolment and approval;
  - a daemon joining by code;
  - its API and protocol through the relay and directly;
  - a check that control's wire traffic, database and logs never contain
    what was typed.
- `web/e2e/control.spec.ts` and `web/e2e/passkey.spec.ts` drive it in Chrome.
