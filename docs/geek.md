# geek: one real setup

The machine illogical was built on, as a worked example of
[advanced.md](advanced.md). geek is a Linux workstation on the tailnet
`tailb2e8f2.ts.net`, running wispd beside illogical.

**Service.** `just install` from this checkout (a release build). The page
is <https://geek.tailb2e8f2.ts.net> from anywhere on the tailnet:

```
tailscale serve --bg --https=443 http://127.0.0.1:7681
```

**Browser blocks on ports.**

```
illogicald install -- --block-listen 100.71.195.119:7443 \
  --block-domain illogical.widgets.wtf \
  --block-acme-cloudflare-token-file ~/.local/share/wisp/cloudflare-token
```

- `*.illogical.widgets.wtf` is a Cloudflare DNS-only record pointing at
  geek's tailnet address. Port 443 there is `tailscale serve`'s and 8443 is
  wispd's, hence 7443.
- The Cloudflare token is the one wisp already uses.
- The bare `illogical.widgets.wtf` is the public project page (Cloudflare
  Pages); it doesn't touch the wildcard or the `_acme-challenge` records the
  daemon writes.

**VMs.** wispd is a user service on geek (`systemctl --user status wisp`);
its token is in `~/.local/share/wisp/token`, the default.

**The Mac.** jake-mini runs illogicald as a launchd agent, with
`--allow-origin https://geek.tailb2e8f2.ts.net`, and is on geek's host list
as `jake-mini` (<https://jake-mini.tailb2e8f2.ts.net>). Its checkout is in
`~/src/illogical`.

**iTerm2.** `ssh -t geek '~/.local/bin/illogical tmux -CC attach'`.

**End-to-end against the real daemon:** `just e2e https://geek.tailb2e8f2.ts.net`.
