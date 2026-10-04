# Security

illogical runs your terminals, and its hosted service (control) relays
between your devices, so security reports matter a lot to us.

## Reporting a vulnerability

Please don't open a public issue. Report it privately instead:

- **GitHub:** [report a vulnerability](https://github.com/arugula-salad/illogical/security/advisories/new)
  (Security → Advisories → *Report a vulnerability*), or
- **Email:** security@illogical.widgets.wtf

Include what you found, how to reproduce it, and the version
(`illogical --version`) or the date, for control. You'll hear back within
three days. We'll agree a disclosure date with you once a fix is out, and
credit you in the advisory unless you'd rather not be named.

## In scope

- The daemon (`illogicald`), the CLI (`illogical`) and the web client it
  serves.
- The desktop app.
- control: control.illogical.widgets.wtf and its relay.
- The install script at illogical.widgets.wtf/install.sh and the release
  artifacts.

Please test against your own machines and accounts only. Don't access other
people's data, degrade the hosted service, or run automated scans against
control at volume.

## Supported versions

Fixes go into the latest release. Update with the same method you installed
with (see the README).
