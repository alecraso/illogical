# Your machines, your team

illogical runs on your machines. Through
[illogical control](control.md) they're yours alone, a team's, or yours
with one session shared. This page says how to set each up and who sees
what.

Two words:

- **A device** is a browser you use illogical from: your laptop's
  browser, your phone. It holds your account's keys.
- **A machine** runs `illogicald`, the daemon that owns the terminals.

## Just you

1. **Sign in** at <https://control.illogical.widgets.wtf>, with GitHub or a
   passkey.
   - A passkey account asks for a display name when you make it: it's
     what teammates see. GitHub accounts go by their login. Either can
     change it later, in *Devices and machines…*.
   - Your first browser becomes your first device, and shows two
     recovery codes once. Keep them offline.
2. **Install illogical** on a machine
   (`curl -fsSL https://illogical.widgets.wtf/install.sh | sh`), then join
   it:

   ```
   illogicald join https://control.illogical.widgets.wtf
   ```

   It prints a link with a code. Open it on a signed-in device, check the
   code matches, pick *Just me* under *Join to*, and approve.
3. **Add your phone** (or another browser): sign in there, then approve
   it on a device you already have, after checking the fingerprint both
   show.

Only your devices reach your machines. Control relays for them but can't
read your terminals ([control-e2e.md](control-e2e.md)).

## A team

A team shares its machines with its members.

1. **Make one:** *Teams…* in the host menu, a name, *Make a team*. You're
   its owner.
2. **Invite someone:** in the team, pick their role and *Make a link*.
   Send it to that one person. They open it, press *Join {team}*, and
   they're in: no second step from you. You get a notification saying who
   joined, with their fingerprint and *Remove*. The link works once,
   for a day, and never as an owner.
   - Your device signs the invite when you make the link, and the link
     carries a one-time key that never reaches control. The person's own
     device adds them to the member list with that key, so control still
     can't add anyone by itself ([control-e2e.md](control-e2e.md)).
   - If a machine in the team runs an older illogical, the link asks you
     first instead, and says which machine to update. Once someone has
     joined with one, a machine on an older illogical can't join the team
     until it's updated.
   - **Ask me first** (and every owner invite) makes the old kind of link:
     it lasts a week, anyone with it can ask to join, and each request
     waits for an owner.
3. **Admit them (Ask me first only):** an owner sees *Add to {team}?* with
   the fingerprint of the person's first device. Check it with them if you
   can (a call, a message), then *Add them*. Adding someone signs the
   team's new member list on your device.
4. **Add team machines:** on the machine, `illogicald join` as above.
   When you approve, pick the team under *Join to*: the list is the teams
   you own. To have it picked already, join with the team's id (it's in
   *Teams…*, with the command to copy):

   ```
   illogicald join https://control.illogical.widgets.wtf --team ID
   ```

**Roles.** Owners change them in *Teams…*.

| Role | May |
| --- | --- |
| watches | see the team's machines and panes |
| drives | also type, answer agents and send them follow-ups |
| owner | also add machines, invite and admit people, change roles, remove members, lock the team |

**Locking.** *Lock (owners only)* is the kill switch: only owners reach
the team's machines, and open invites and requests are dropped. Members'
panes leave their screens within a second. *Unlock* lets them back in.

## Personal machine or team machine

|  | Your own machine | A team machine |
| --- | --- | --- |
| Who reaches it | you, on your devices | every member, by their role |
| Teammates see | only sessions you share with them | every pane that isn't private |
| A teammate types in a pane | after you allow it, for a set time | if they drive |
| Private panes | only you | only the team's owners |

- **On your own machine, a pane runs as you.** A teammate who wants to
  type in one (or answer its agent) asks first, and you get *{name} asks
  to drive %N* with how long: 10 minutes, 30 minutes or 2 hours. Until
  then they can work in throwaway VM tabs of their own (Linux, with wisp;
  see [advanced.md](advanced.md)).
- **On a team machine** nobody's OK is needed: members drive it by their
  role.
- **Private panes:** *Private (only you see it)* in a pane's menu keeps it
  on your screen alone, whatever is shared. On a team machine the team's
  owners count as its owner. Sharing warns about panes that look like
  they show a secret, and offers to make them private.
- **Moving a machine** between your account and a team, or between teams:
  *Move to…* on it in *Devices and machines…* (you must own the teams on
  both sides). The members of the team it leaves lose it at once. Your
  device signs the move and the machine checks it, as at a join.

## Sharing one session

To share a session (a set of tabs) instead of a whole machine:
*Share session…* in the session menu.

- **Someone:** their login on illogical, then check their first device's
  fingerprint and *Share with* them. They *can watch* or *can drive*.
  *with history* also shows what was there before; without it they see
  from now on.
  - Someone you're not in a team with is asked first: they see *{you}
    wants to share a session on {machine} with you* and *Accept* or
    *Decline*. Until they accept, the machine isn't listed for them and
    its notifications don't reach them. Teammates aren't asked.
- **A team:** *Share with everyone in {team}*, as members come and go.
  Each member gets the role you picked.
- **A read-only link:** *Make a read-only link*. Anyone with it watches,
  from now on, for an hour; no account needed.
- **Revoking:** *Remove* beside a person or team. Their view of it closes
  within a second.

On your own machine, people you share with still can't type in its panes
until you allow it (above).

## Over a tailnet

Without control, the same sharing works between Tailscale logins:

- In *Share session…*, give their **tailnet login** (`them@example.com`).
- **They have to reach the machine too.** If they're not on your tailnet,
  share the machine with them in Tailscale's admin console
  ([node sharing](https://tailscale.com/kb/1084/sharing)), then they open
  `https://<this machine>.<tailnet>.ts.net`.
- Read-only links work the same way: `illogical share %N --ttl 1h` (or
  *Share read-only link…* on a pane), for anyone who can reach the node.
- Only the machine's owner (its Tailscale login, or `--owner`) gets
  everything; see the README's quickstart.
