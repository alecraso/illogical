//! Runnable scenarios that print a transcript: the done-when demo (accounts,
//! roster events, a removal), an offline device, and a hostile delivery service.

use crate::Log;
use crate::client::{Device, Event, random32};
use crate::ds::Ds;
use crate::ident::{Account, Revocation};
use crate::roster::{Invite, Role, Roster, RosterChain, RosterMember};
use ed25519_dalek::SigningKey;
use crate::client::Provider;
use web_time::Instant;

/// What control holds: the channel's log, the signed rosters and the
/// revocation log. All of it is public to control; none of it decrypts.
pub struct Net {
    pub ds: Ds,
    pub rosters: Vec<Roster>,
    pub revocations: Vec<Revocation>,
}

impl Net {
    fn new() -> Self {
        Net { ds: Ds::new(0), rosters: vec![], revocations: vec![] }
    }
}

/// A device fetches and verifies rosters and revocations it hasn't seen.
fn sync(d: &mut Device, net: &Net) {
    let chain = d.chain.as_mut().unwrap();
    let head = chain.head().v;
    for r in net.rosters.iter().filter(|r| r.v > head) {
        chain.push(r.clone()).expect("roster from control doesn't verify");
    }
    for r in net.revocations.iter().skip(chain.revocations.len()) {
        chain.revoke(r.clone()).expect("revocation from control doesn't verify");
    }
}

/// Fetch everything new for a device and process it in order. Returns (ok, failed).
fn deliver(d: &mut Device, net: &mut Net, log: &mut Log) -> (usize, usize) {
    if d.chain.is_some() {
        sync(d, net);
    }
    let (mut ok, mut bad) = (0, 0);
    for e in net.ds.fetch(&d.name) {
        match d.receive(&e.bytes) {
            Ok(Event::App { from, text, epoch }) => {
                ok += 1;
                log.say(format!("    {:<14} reads   [{from} @{epoch}] {text}", d.name))
            }
            Ok(Event::Commit { from, epoch, summary }) => {
                ok += 1;
                log.say(format!("    {:<14} applies commit from {from} ({summary}) -> epoch {epoch}", d.name))
            }
            Err(err) => {
                bad += 1;
                log.say(format!("    {:<14} CAN'T   seq {} ({}): {err}", d.name, e.seq, if e.commit { "commit" } else { "message" }))
            }
        }
    }
    (ok, bad)
}

fn post(d: &mut Device, net: &mut Net, log: &mut Log, text: &str) -> u64 {
    let b = d.send(text);
    let seq = net.ds.submit(&d.name, b);
    log.say(format!("  {} posts (seq {seq}, epoch {}): {text}", d.name, d.epoch()));
    seq
}

fn commit(d: &Device, net: &mut Net, log: &mut Log, out: crate::client::Out, what: &str) {
    let n = out.commit.len();
    let seq = net.ds.submit_commit(&d.name, out.commit, out.group_info).unwrap();
    log.say(format!("  {} commits {what} (seq {seq}, {n} bytes) -> epoch {}", d.name, d.epoch()));
}

fn member(a: &Account, role: Role) -> RosterMember {
    RosterMember { account: a.id.clone(), root: a.root_pub(), role }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// The done-when demo: two accounts talk through the stub delivery service,
/// one is removed mid-conversation and reads nothing after. Along the way,
/// every roster event from the Talk track becomes an MLS commit.
pub fn demo(log: &mut Log) {
    let seed = Provider::default();
    let alice = Account::new("alice", random32(&seed));
    let bob = Account::new("bob", random32(&seed));
    let carol = Account::new("carol", random32(&seed));
    let mut a1 = Device::new(&alice, "laptop", 0);
    let mut b1 = Device::new(&bob, "laptop", 0);
    let mut c1 = Device::new(&carol, "browser", 0);
    let mut a2 = Device::new(&alice, "phone", 0);
    let mut net = Net::new();

    log.say("== Team founded: alice (owner) makes #general ==");
    let v1 = Roster::founding("team-1", member(&alice, Role::Owner), 1000).sign_by_device(&a1.device_key, &a1.cert);
    net.rosters.push(v1.clone());
    for d in [&mut a1, &mut b1, &mut c1, &mut a2] {
        d.chain = Some(RosterChain::found(v1.clone()).unwrap());
    }
    a1.create_group(b"team-1/#general");
    a1.applied_now();
    net.ds.mark_joined(&a1.name);
    log.say(format!("  roster v1 = [alice owner], signed by alice/laptop; group epoch {}", a1.epoch()));

    log.say("\n== Ask me first: alice's device admits bob (roster v2 + Add + Welcome) ==");
    let mut v2 = v1.next(1010);
    v2.members.push(member(&bob, Role::Member));
    let v2 = v2.sign_by_device(&a1.device_key, &a1.cert);
    net.rosters.push(v2);
    sync(&mut a1, &net);
    let kp = b1.key_package();
    log.say(format!("  bob/laptop uploads a KeyPackage ({} bytes) to control", kp.len()));
    let out = a1.add(&[kp], a1.aad()).unwrap();
    a1.applied_now();
    let welcome = out.welcome.clone().unwrap();
    commit(&a1, &mut net, log, out, "Add bob/laptop (AAD: roster v2)");
    sync(&mut b1, &net);
    b1.join_welcome(&welcome).unwrap();
    b1.applied_now();
    net.ds.mark_joined(&b1.name);
    log.say(format!("  bob/laptop joins from the Welcome ({} bytes); members: {:?}", welcome.len(), b1.members()));

    log.say("\n== Two accounts talk ==");
    post(&mut a1, &mut net, log, "hi bob, welcome to #general");
    deliver(&mut b1, &mut net, log);
    post(&mut b1, &mut net, log, "thanks alice. the build on geek is green");
    deliver(&mut a1, &mut net, log);

    log.say("\n== Invite link: carol's own device joins (one-time key signs roster v3, external commit) ==");
    let otk = SigningKey::from_bytes(&random32(&seed));
    let inv = Invite::new(&a1.device_key, &a1.cert, "team-1", Role::Member, 1000 + 86_400, &otk);
    log.say(format!("  alice/laptop makes a link: .../join#pinvite=team-1.{}... (the seed never reaches control)", &hex::encode(otk.to_bytes())[..12]));
    sync(&mut c1, &net);
    let v3 = Roster::redeem(c1.chain.as_ref().unwrap().head(), &inv, member(&carol, Role::Member), &otk, 1100);
    c1.chain.as_mut().unwrap().push(v3.clone()).unwrap();
    net.rosters.push(v3);
    let gi = net.ds.group_info.clone().unwrap();
    log.say(format!("  carol/browser fetches the GroupInfo control holds ({} bytes)", gi.len()));
    let out = c1.join_external(&gi, c1.aad()).unwrap();
    c1.applied_now();
    commit(&c1, &mut net, log, out, "an external join (AAD: roster v3)");
    net.ds.mark_joined(&c1.name);
    deliver(&mut a1, &mut net, log);
    deliver(&mut b1, &mut net, log);

    log.say("\n== A new device for an existing member: alice/phone joins itself (no roster change) ==");
    sync(&mut a2, &net);
    let gi = net.ds.group_info.clone().unwrap();
    let out = a2.join_external(&gi, a2.aad()).unwrap();
    a2.applied_now();
    commit(&a2, &mut net, log, out, "an external join (AAD: roster v3)");
    net.ds.mark_joined(&a2.name);
    for d in [&mut a1, &mut b1, &mut c1] {
        deliver(d, &mut net, log);
    }
    post(&mut c1, &mut net, log, "hello all, carol here from the browser");
    for d in [&mut a1, &mut b1, &mut a2] {
        deliver(d, &mut net, log);
    }

    log.say("\n== Control tries to add a reader of its own (forged alice device, external commit) ==");
    let fake_alice = Account::new("alice", random32(&seed));
    let mut eve = Device::new(&fake_alice, "control-tap", 0);
    let gi = net.ds.group_info.clone().unwrap();
    let out = eve.join_external(&gi, a1.aad()).unwrap();
    // A hostile control just puts it in the log; it skips its own ordering rule.
    net.ds.inject(&eve.name, out.commit);
    log.say("  control appends the forged external commit to the log");
    for d in [&mut a1, &mut b1, &mut c1, &mut a2] {
        deliver(d, &mut net, log);
    }
    log.say("\n== bob's device tries to kick carol without a roster change ==");
    let leaves = b1.leaves_of(|l| l.starts_with("carol/"));
    let rogue = b1.remove_staged(&leaves, b1.aad());
    b1.clear_pending();
    net.ds.inject(&b1.name, rogue);
    for d in [&mut a1, &mut c1, &mut a2] {
        deliver(d, &mut net, log);
    }
    net.ds.fetch(&b1.name);
    net.ds.mark_joined(&eve.name);
    post(&mut a1, &mut net, log, "nobody new got in");
    let (ok, _) = deliver(&mut eve, &mut net, log);
    log.say(format!("  the forged device read {ok} messages"));
    for d in [&mut b1, &mut c1, &mut a2] {
        deliver(d, &mut net, log);
    }

    log.say("\n== Removal: alice removes bob (roster v4, one commit removes all his devices) ==");
    let mut v4 = net.rosters.last().unwrap().next(1200);
    v4.members.retain(|m| m.account != "bob");
    net.rosters.push(v4.sign_by_device(&a1.device_key, &a1.cert));
    sync(&mut a1, &net);
    let leaves = a1.leaves_of(|l| l.starts_with("bob/"));
    let removal_seq = net.ds.log.len(); // nothing pruned in the demo, so index == seq
    let out = a1.remove(&leaves, a1.aad()).unwrap();
    a1.applied_now();
    commit(&a1, &mut net, log, out, "Remove bob/* (AAD: roster v4)");
    for d in [&mut c1, &mut a2] {
        deliver(d, &mut net, log);
    }
    post(&mut a1, &mut net, log, "bob's gone. the launch moves to tuesday");
    post(&mut c1, &mut net, log, "got it, tuesday");
    for d in [&mut c1, &mut a2, &mut a1] {
        deliver(d, &mut net, log);
    }

    log.say("\n  bob/laptop takes everything control stored after his removal:");
    let after: Vec<_> = net.ds.log[removal_seq..].to_vec();
    let msgs: Vec<_> = after.iter().filter(|e| !e.commit).collect();
    let mut read = 0;
    log.say("  (1) a patched client that ignores the removal commit, using bob's last keys:");
    for e in &msgs {
        match b1.receive(&e.bytes) {
            Ok(_) => read += 1,
            Err(err) => log.say(format!("    bob/laptop     CAN'T   seq {}: {err}", e.seq)),
        }
    }
    log.say("  (2) the honest client, which fetches roster v4 and applies the removal first:");
    sync(&mut b1, &net);
    match b1.receive(&after[0].bytes) {
        Ok(Event::Commit { summary, .. }) => log.say(format!("    bob/laptop applies its own removal ({summary}); group active: {}", b1.g().is_active())),
        other => log.say(format!("    {other:?}")),
    }
    for e in &msgs {
        match b1.receive(&e.bytes) {
            Ok(_) => read += 1,
            Err(err) => log.say(format!("    bob/laptop     CAN'T   seq {}: {err}", e.seq)),
        }
    }
    net.ds.fetch(&b1.name);
    log.say(format!("  bob read {read} of {} messages sent after his removal", msgs.len()));

    log.say("\n== A device removed: alice revokes alice/phone; any member's device commits the removal ==");
    net.revocations.push(Revocation::new(&alice, "phone"));
    sync(&mut c1, &net);
    let leaves = c1.leaves_of(|l| l == "alice/phone");
    let out = c1.remove(&leaves, c1.aad()).unwrap();
    c1.applied_now();
    commit(&c1, &mut net, log, out, "Remove alice/phone (AAD: roster v4, 1 revocation)");
    deliver(&mut a1, &mut net, log);
    post(&mut a1, &mut net, log, "lost my phone, it's out");
    deliver(&mut c1, &mut net, log);
    let (ok, _) = deliver(&mut a2, &mut net, log);
    log.say(format!("  alice/phone read {} messages after its removal (its own removal commit aside)", ok.saturating_sub(1)));

    log.say("\n== Lock: roster v5 locked; one commit removes every non-owner device ==");
    let mut v5 = net.rosters.last().unwrap().next(1300);
    v5.locked = true;
    net.rosters.push(v5.sign_by_device(&a1.device_key, &a1.cert));
    sync(&mut a1, &net);
    let t = Instant::now();
    let leaves = a1.leaves_of(|l| !l.starts_with("alice/"));
    let out = a1.remove(&leaves, a1.aad()).unwrap();
    let t_commit = ms(t);
    a1.applied_now();
    commit(&a1, &mut net, log, out, "Remove carol/browser (AAD: roster v5, locked)");
    log.say(format!("  building the Lock commit took {t_commit:.2} ms"));
    let locked_seq = post(&mut a1, &mut net, log, "locked while we rotate secrets");
    let (ok, _) = deliver(&mut c1, &mut net, log);
    log.say(format!("  carol read {} messages while locked", ok.saturating_sub(1)));

    log.say("\n== Unlock: roster v6; carol's device rejoins from the current epoch (external commit) ==");
    let mut v6 = net.rosters.last().unwrap().next(1400);
    v6.locked = false;
    net.rosters.push(v6.sign_by_device(&a1.device_key, &a1.cert));
    sync(&mut c1, &net);
    let gi = net.ds.group_info.clone().unwrap();
    let out = c1.join_external(&gi, c1.aad()).unwrap();
    c1.applied_now();
    commit(&c1, &mut net, log, out, "an external rejoin (AAD: roster v6)");
    net.ds.mark_joined(&c1.name);
    deliver(&mut a1, &mut net, log);
    post(&mut a1, &mut net, log, "unlocked, welcome back");
    deliver(&mut c1, &mut net, log);
    let locked_msg = net.ds.entry(locked_seq).clone();
    match c1.receive(&locked_msg.bytes) {
        Ok(_) => log.say("  carol read the locked-period message (unexpected)"),
        Err(e) => log.say(format!("  carol still can't read the locked-period message (seq {}): {e}", locked_msg.seq)),
    }

    log.say("\n== What control stored ==");
    let bytes: usize = net.ds.log.iter().map(|e| e.bytes.len()).sum();
    log.say(format!(
        "  {} log entries ({} commits, {} messages, {bytes} bytes), {} signed rosters, {} revocation, 1 GroupInfo. No private key.",
        net.ds.log.len(),
        net.ds.log.iter().filter(|e| e.commit).count(),
        net.ds.log.iter().filter(|e| !e.commit).count(),
        net.rosters.len(),
        net.revocations.len()
    ));
}

fn plain_group(names: &[&str], max_past_epochs: usize) -> (Vec<Device>, Net) {
    let seed = Provider::default();
    let mut devs: Vec<Device> = names.iter().map(|n| Device::new(&Account::new(n, random32(&seed)), "laptop", max_past_epochs)).collect();
    let mut net = Net::new();
    devs[0].create_group(b"g");
    net.ds.mark_joined(&devs[0].name.clone());
    let kps: Vec<_> = devs[1..].iter().map(|d| d.key_package()).collect();
    let out = devs[0].add(&kps, vec![]).unwrap();
    let w = out.welcome.clone().unwrap();
    let name = devs[0].name.clone();
    net.ds.submit_commit(&name, out.commit, out.group_info).unwrap();
    for d in devs[1..].iter_mut() {
        d.join_welcome(&w).unwrap();
        net.ds.mark_joined(&d.name);
    }
    (devs, net)
}

/// Question 4: a device that's offline across several commits.
pub fn offline(log: &mut Log) {
    log.say("== Offline device: carol misses 3 commits and 4 messages, then catches up in order ==");
    let (mut d, mut net) = plain_group(&["alice", "bob", "carol"], 0);
    let (a, rest) = d.split_at_mut(1);
    let (b, c) = rest.split_at_mut(1);
    let (a, b, c) = (&mut a[0], &mut b[0], &mut c[0]);
    log.say(format!("  all at epoch {}; carol goes offline", a.epoch()));
    post(a, &mut net, log, "m1 before any commit");
    let o = a.self_update(vec![]).unwrap();
    commit(a, &mut net, log, o, "a self-update");
    deliver(b, &mut net, log);
    post(b, &mut net, log, "m2 after commit 1");
    let dave = Device::new(&Account::new("dave", [7; 32]), "laptop", 0);
    let o = a.add(&[dave.key_package()], vec![]).unwrap();
    commit(a, &mut net, log, o, "Add dave");
    deliver(b, &mut net, log);
    post(b, &mut net, log, "m3 after commit 2");
    let o = b.self_update(vec![]).unwrap();
    commit(b, &mut net, log, o, "a self-update");
    deliver(a, &mut net, log);
    log.say("    (alice had moved to epoch 3 before bob's epoch-2 message reached her; with max_past_epochs=0 she can't read it. See below.)");
    post(a, &mut net, log, "m4 after commit 3");
    log.say("  carol comes back:");
    let (ok, bad) = deliver(c, &mut net, log);
    log.say(format!("  carol processed {ok}, failed {bad}; now at epoch {} with the same epoch authenticator as alice: {}", c.epoch(), c.epoch_auth() == a.epoch_auth()));

    log.say("\n== A message from an old epoch that arrives after the commits (past-epoch secrets) ==");
    for keep in [0usize, 2, 3] {
        let (mut d, mut net) = plain_group(&["alice", "bob"], keep);
        let (a, b) = d.split_at_mut(1);
        let (a, b) = (&mut a[0], &mut b[0]);
        let late = a.send("sent at epoch 1, delivered late");
        for _ in 0..3 {
            let o = a.self_update(vec![]).unwrap();
            commit(a, &mut net, &mut Log::quiet(), o, "");
        }
        deliver(b, &mut net, &mut Log::quiet());
        let r = b.receive(&late);
        log.say(format!(
            "  max_past_epochs={keep}: bob is at epoch {}, message from epoch 1 -> {}",
            b.epoch(),
            match r {
                Ok(Event::App { text, .. }) => format!("read: {text}"),
                Ok(e) => format!("{e:?}"),
                Err(e) => format!("CAN'T: {e}"),
            }
        ));
    }

    log.say("\n== Too far behind: control kept only the last 2 commits; carol resyncs by external commit ==");
    let (mut d, mut net) = plain_group(&["alice", "bob", "carol"], 0);
    let (a, rest) = d.split_at_mut(1);
    let (b, c) = rest.split_at_mut(1);
    let (a, b, c) = (&mut a[0], &mut b[0], &mut c[0]);
    let carol_key = c.cred.signature_key.as_slice().to_vec();
    for i in 0..5 {
        post(a, &mut net, &mut Log::quiet(), &format!("missed message {i}"));
        let o = a.self_update(vec![]).unwrap();
        net.ds.submit_commit(&a.name, o.commit, o.group_info).unwrap();
    }
    deliver(b, &mut net, &mut Log::quiet());
    // Retention: control drops everything but the last 2 commits.
    let keep_from = net.ds.log.iter().filter(|e| e.commit).rev().nth(1).unwrap().seq;
    net.ds.prune_before(keep_from);
    log.say(format!("  alice is at epoch {}, carol at {}; the oldest commit control still has is for epoch {}", a.epoch(), c.epoch(), net.ds.log[0].epoch));
    let (_, bad) = deliver(c, &mut net, log);
    log.say(format!("  {bad} failures: carol can't bridge the gap from the log"));
    let gi = net.ds.group_info.clone().unwrap();
    let o = c.join_external(&gi, vec![]).unwrap();
    commit(c, &mut net, log, o, "an external rejoin (same MLS key: openmls removes the old leaf in the same commit)");
    net.ds.mark_joined(&c.name);
    deliver(a, &mut net, log);
    deliver(b, &mut net, log);
    let n_carol = a.g().members().filter(|m| m.signature_key.as_slice() == carol_key.as_slice()).count();
    log.say(format!("  members now: {:?} (carol's leaves: {n_carol})", a.members()));
    post(a, &mut net, log, "carol, you're back");
    deliver(c, &mut net, log);
    log.say("  the 5 messages from the gap stay unreadable to carol unless someone re-shares them.");
}

/// Question 5: what a hostile delivery service can do.
pub fn hostile(log: &mut Log) {
    let (mut d, mut net) = plain_group(&["alice", "bob", "carol", "dave"], 0);
    let [a, b, c, dd] = &mut d[..] else { unreachable!() };

    log.say("== Replay ==");
    let m = a.send("pay the invoice");
    let seq = net.ds.submit(&a.name, m.clone());
    log.say(format!("  alice posts seq {seq}"));
    deliver(b, &mut net, log);
    log.say(format!("  control sends seq {seq} to bob again -> {:?}", b.receive(&m).map(|_| "accepted".to_string()).unwrap_or_else(|e| e)));
    for x in [&mut *c, &mut *dd] {
        deliver(x, &mut net, &mut Log::quiet());
    }
    let o = a.self_update(vec![]).unwrap();
    let old_commit = o.commit.clone();
    commit(a, &mut net, log, o, "a self-update");
    for x in [&mut *b, &mut *c, &mut *dd] {
        deliver(x, &mut net, &mut Log::quiet());
    }
    log.say(format!("  control replays that commit to bob -> {}", b.receive(&old_commit).err().unwrap_or_default()));

    log.say("\n== Reordered commits ==");
    let o1 = a.self_update(vec![]).unwrap();
    let o2 = a.self_update(vec![]).unwrap();
    log.say(format!("  alice commits twice: epoch {} -> {}", a.epoch() - 2, a.epoch()));
    log.say(format!("  control gives carol the second first -> {}", c.receive(&o2.commit).err().unwrap_or_default()));
    log.say(format!("  carol's epoch is still {}; first commit -> {:?}", c.epoch(), c.receive(&o1.commit).map(|_| "ok").unwrap_or("err")));
    log.say(format!("  then the second -> {:?}; carol at epoch {}", c.receive(&o2.commit).map(|_| "ok").unwrap_or("err"), c.epoch()));
    for x in [&mut *b, &mut *dd] {
        x.receive(&o1.commit).unwrap();
        x.receive(&o2.commit).unwrap();
    }
    net.ds = Ds::new(a.epoch());
    for x in [&*a, &*b, &*c, &*dd] {
        net.ds.mark_joined(&x.name);
    }

    log.say("\n== Reordered messages within an epoch ==");
    let ms_: Vec<_> = (1..=3).map(|i| a.send(&format!("part {i}"))).collect();
    for i in [2, 0, 1] {
        log.say(format!("  bob gets part {} -> {:?}", i + 1, b.receive(&ms_[i]).map(|_| "read").unwrap_or("err")));
    }
    for m in &ms_ {
        c.receive(m).unwrap();
        dd.receive(m).unwrap();
    }

    log.say("\n== A dropped commit ==");
    let o = a.self_update(vec![]).unwrap();
    for x in [&mut *b, &mut *c] {
        x.receive(&o.commit).unwrap();
    }
    let m = a.send("after the commit dave never got");
    log.say(format!("  control withholds alice's commit from dave; then alice posts at epoch {}", a.epoch()));
    log.say(format!("  dave (epoch {}) -> {}", dd.epoch(), dd.receive(&m).err().unwrap_or_default()));
    log.say("  dave sees a message for an epoch ahead of his own: he knows he's missing a commit.");
    dd.receive(&o.commit).unwrap();
    dd.receive(&m).unwrap();
    b.receive(&m).unwrap();
    c.receive(&m).unwrap();

    log.say("\n== Concurrent commits, honest control: first one wins ==");
    net.ds = Ds::new(a.epoch());
    for x in [&*a, &*b, &*c, &*dd] {
        net.ds.mark_joined(&x.name);
    }
    let ca = a.self_update_staged(vec![]);
    let cb = b.self_update_staged(vec![]);
    let gia = vec![];
    let ra = net.ds.submit_commit(&a.name, ca, gia.clone());
    let rb = net.ds.submit_commit(&b.name, cb, gia);
    log.say(format!("  alice and bob both commit for epoch {}: alice -> {:?}, bob -> {:?}", a.epoch(), ra, rb));
    a.merge_pending();
    b.clear_pending();
    log.say("  alice merges hers; bob drops his pending commit, applies alice's, and can commit again");
    for x in [&mut *b, &mut *c, &mut *dd] {
        deliver(x, &mut net, &mut Log::quiet());
    }
    let o = b.self_update(vec![]).unwrap();
    net.ds.submit_commit(&b.name, o.commit, o.group_info).unwrap();
    for x in [&mut *a, &mut *c, &mut *dd] {
        deliver(x, &mut net, &mut Log::quiet());
    }
    log.say(format!(
        "  all at epoch {}, same epoch authenticator: {}",
        a.epoch(),
        [&*b, &*c, &*dd].iter().all(|x| x.epoch_auth() == a.epoch_auth())
    ));

    log.say("\n== A fork: hostile control sends alice's commit to carol and bob's to dave ==");
    let e = a.epoch();
    let ca = a.self_update_staged(vec![]);
    let cb = b.self_update_staged(vec![]);
    a.merge_pending();
    b.merge_pending();
    c.receive(&ca).unwrap();
    dd.receive(&cb).unwrap();
    for x in [&*a, &*b, &*c, &*dd] {
        log.say(format!("  {:<14} epoch {}  epoch authenticator {}", x.name, x.epoch(), x.epoch_auth()));
    }
    let m = c.send("carol on branch A");
    log.say(format!("  carol posts; alice -> {:?}", a.receive(&m).map(|_| "read").unwrap_or("err")));
    log.say(format!("  dave (same epoch number {}, other branch) -> {}", e + 1, dd.receive(&m).err().unwrap_or_default()));
    log.say("  Detection: same epoch number, different epoch authenticator. Clients that compare");
    log.say("  (epoch, H(epoch authenticator)) over a channel control can't forge see the fork at once.");
    log.say("  Resolution: control's log says which commit came first (alice's). bob and dave rejoin");
    log.say("  that branch by external commit from its GroupInfo; their branch's messages are lost.");
    let gi = a.g().export_group_info(openmls_traits::OpenMlsProvider::crypto(&a.provider), &a.signer, true).unwrap().to_bytes().unwrap();
    let rb = b.join_external(&gi, vec![]).unwrap();
    a.receive(&rb.commit).unwrap();
    c.receive(&rb.commit).unwrap();
    let gi = rb.group_info;
    let rd = dd.join_external(&gi, vec![]).unwrap();
    for x in [&mut *a, &mut *b, &mut *c] {
        x.receive(&rd.commit).unwrap();
    }
    for x in [&*a, &*b, &*c, &*dd] {
        log.say(format!("  {:<14} epoch {}  epoch authenticator {}  members {}", x.name, x.epoch(), x.epoch_auth(), x.g().members().count()));
    }
    let m = dd.send("dave is back on the main branch");
    log.say(format!("  dave posts; alice -> {:?}", a.receive(&m).map(|e| format!("{e:?}")).unwrap_or_else(|e| e)));
}
