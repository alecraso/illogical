//! Each message against JSON recorded from the code it replaced: the
//! daemon's fake controls (`arugula-daemon`'s `control.rs` tests), control's
//! own handlers and the requests in its `routing_wire.rs`. A recorded
//! message must parse and serialize back to the same JSON; one from an older
//! peer (a field missing) must still parse.

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::*;

fn cert() -> Value {
    json!({
        "v": 1, "account": "a1", "device": "d1", "kind": "daemon", "name": "box",
        "noise": "00", "sign": "11", "created": 1_790_000_000_000u64, "approver": "r1", "sig": "ab",
    })
}

fn browser() -> Value {
    json!({
        "v": 1, "account": "a1", "device": "r1", "kind": "browser", "name": "laptop",
        "noise": "00", "sign": "22", "created": 1_790_000_000_000u64, "approver": "r1", "sig": "cd",
    })
}

fn revocation() -> Value {
    json!({ "v": 1, "account": "a1", "device": "d9", "at": 1_790_000_000_001u64, "by": "r1", "sig": "ef" })
}

fn pin() -> Value {
    json!({ "team": "t1", "founder": "a1", "founder_root": "r1" })
}

fn roster() -> Value {
    json!({
        "v": 1, "team": "t1", "name": "crew", "version": 1, "at": 1_790_000_000_000u64,
        "members": [{ "account": "a1", "root": "r1", "role": "owner", "name": "jake" }],
        "by": "r1", "sig": "01",
    })
}

fn parse<T: DeserializeOwned>(v: &Value) -> T {
    serde_json::from_str(&v.to_string()).unwrap_or_else(|e| panic!("{e}: {v}"))
}

/// `recorded` parses, and serializes back to exactly it (key order aside).
fn round<T: Serialize + DeserializeOwned>(recorded: Value) -> T {
    let typed: T = parse(&recorded);
    assert_eq!(serde_json::to_value(&typed).unwrap(), recorded);
    typed
}

fn without(v: &Value, key: &str) -> Value {
    let mut v = v.clone();
    v.as_object_mut().unwrap().remove(key);
    v
}

// ---------------------------------------------------------------- join

/// The daemon's request (`join_start`), nulls and all.
fn join_request() -> Value {
    json!({
        "cert": cert(), "urls": [], "team": null, "ticket": null, "features": "presigned-invites,owner-moves",
        "proof": { "ms": 1_790_000_000_000u64, "sig": "ab" },
    })
}

#[test]
fn the_join_request_is_what_the_daemon_sends() {
    round::<JoinRequest>(join_request());
    let with_team = json!({
        "cert": cert(), "urls": [], "team": "t1", "ticket": "tk", "features": "", "proof": null,
    });
    let r = round::<JoinRequest>(with_team);
    assert_eq!((r.team.as_deref(), r.ticket.as_deref()), (Some("t1"), Some("tk")));
}

/// `routing_wire.rs`'s `join_body`: an older daemon sends no team, ticket or
/// proof (and the CLI no urls or features).
#[test]
fn an_older_daemons_join_request_parses() {
    let old = json!({ "cert": cert(), "urls": [], "features": "presigned-invites" });
    let r: JoinRequest = parse(&old);
    assert!(r.team.is_none() && r.ticket.is_none() && r.proof.is_none());
    let cli: JoinRequest = parse(&json!({ "cert": cert(), "proof": { "ms": 1, "sig": "x" } }));
    assert!(cli.urls.is_empty() && cli.features.is_empty() && cli.proof.is_some());
    for k in ["urls", "team", "ticket", "features", "proof"] {
        parse::<JoinRequest>(&without(&join_request(), k));
    }
}

#[test]
fn join_started_is_what_control_answers() {
    let s = round::<JoinStarted>(json!({ "code": "abcd", "poll": "p", "expires_in_secs": 900, "team_name": null }));
    assert!(s.team_name.is_none());
    round::<JoinStarted>(json!({ "code": "abcd", "poll": "p", "expires_in_secs": 900, "team_name": "crew" }));
}

/// The daemon's fake controls answer without `team_name`, as control did
/// before `--team`.
#[test]
fn join_started_without_a_team_name_parses() {
    let s: JoinStarted = parse(&json!({ "code": "abcd", "poll": "p", "expires_in_secs": 60 }));
    assert!(s.team_name.is_none());
}

#[test]
fn a_join_poll_has_three_shapes() {
    // Waiting, turned down, and approved with and without a team.
    let waiting = round::<JoinPoll>(json!({ "approved": false }));
    assert!(!waiting.approved && waiting.rejected.is_none());
    let rejected = round::<JoinPoll>(json!({ "approved": false, "rejected": "laptop" }));
    assert_eq!(rejected.rejected.as_deref(), Some("laptop"));
    let approved = json!({
        "approved": true, "cert": cert(), "trust": { "account": "a1", "root": "r1" },
        "certs": [browser()], "revocations": [revocation()], "team": null,
    });
    round::<JoinPoll>(approved.clone());
    let team = json!({ "team": "t1", "founder": "a1", "founder_root": "r1", "name": "crew", "sig": "ab" });
    let mut with_team = approved.clone();
    with_team["team"] = team;
    let p = round::<JoinPoll>(with_team);
    assert_eq!(p.team.unwrap().team, "t1");
    // A team pin the approver didn't sign: `sig` is `null`.
    let mut unsigned = approved;
    unsigned["team"] = json!({ "team": "t1", "founder": "a1", "founder_root": "r1", "name": "crew", "sig": null });
    round::<JoinPoll>(unsigned);
    // What the constructors say is what control said.
    assert_eq!(serde_json::to_value(JoinPoll::waiting()).unwrap(), json!({ "approved": false }));
    assert_eq!(
        serde_json::to_value(JoinPoll::turned_down("laptop".into())).unwrap(),
        json!({ "approved": false, "rejected": "laptop" })
    );
}

/// The daemon's fake control in `control.rs`'s tests approves without
/// `team`; a team pin from an older control has no `name` or `sig`.
#[test]
fn an_older_controls_join_poll_parses() {
    let p: JoinPoll = parse(&json!({
        "approved": true, "cert": cert(), "trust": { "account": "evil", "root": "r1" },
        "certs": [browser()], "revocations": [],
    }));
    assert!(p.team.is_none() && p.cert.is_some());
    let bare: JoinPoll = parse(&json!({ "approved": true }));
    assert!(bare.cert.is_none() && bare.trust.is_none() && bare.certs.is_empty());
    let t: JoinTeam = parse(&pin());
    assert!(t.name.is_empty() && t.sig.is_none());
    let full = json!({ "approved": true, "cert": cert(), "trust": null, "certs": [], "revocations": [], "team": null });
    for k in ["rejected", "team", "certs", "revocations"] {
        parse::<JoinPoll>(&without(&full, k));
    }
}

#[test]
fn the_poll_path_is_the_route_filled_in() {
    let path = JoinPoll::path("ab-cd", "tok");
    assert_eq!(path, "/api/join/ab-cd?poll=tok");
    let q: PollQuery = serde_urlencoded::from_str(path.split_once('?').unwrap().1).unwrap();
    assert_eq!(q.poll, "tok");
}

// ---------------------------------------------------------------- certificates

#[test]
fn the_trust_answer_is_what_control_answers() {
    let moved = json!({ "team": pin(), "at": 1_790_000_000_002u64, "by": "r1", "sig": "ab" });
    let full = json!({
        "trust": { "account": "a1", "root": "r1" }, "certs": [browser()], "revocations": [revocation()],
        "moved": moved,
    });
    let a = round::<TrustAnswer>(full);
    assert_eq!(a.moved.unwrap().at, 1_790_000_000_002);
    // No root yet, no move: both `null`.
    round::<TrustAnswer>(json!({ "trust": null, "certs": [], "revocations": [], "moved": null }));
}

/// The daemon's fake control answers `{certs, revocations}` only; an older
/// control sent no `moved`.
#[test]
fn an_older_controls_trust_answer_parses() {
    let a: TrustAnswer = parse(&json!({ "certs": [browser()], "revocations": [] }));
    assert!(a.trust.is_none() && a.moved.is_none());
    let b: TrustAnswer = parse(&json!({ "trust": null, "certs": [], "revocations": [], "moved": null }));
    assert!(b.moved.is_none());
}

#[test]
fn the_team_answer_is_what_control_answers() {
    let full = json!({
        "team": pin(), "locked": false, "rosters": [roster()], "certs": { "a1": [[browser()], [revocation()]] },
        "names": { "a1": "jake" },
    });
    let a = round::<TeamAnswer>(full);
    assert_eq!(a.rosters.len(), 1);
    // A machine not in a team gets `{"team": null}`, which the daemon has
    // always taken as an answer it can't use.
    let none = round::<TeamNone>(json!({ "team": null }));
    assert!(none.team.is_none());
    assert!(serde_json::from_value::<TeamAnswer>(json!({ "team": null })).is_err());
}

/// A control from before names (#208) sends none, nor a `team` of its own.
#[test]
fn an_older_controls_team_answer_parses() {
    let a: TeamAnswer = parse(&json!({ "locked": true, "rosters": [], "certs": {} }));
    assert!(a.locked && a.names.is_empty() && a.team.is_none());
}

#[test]
fn the_shared_teams_answer_is_what_control_answers() {
    let one = json!({
        "team": pin(), "name": "crew", "locked": true, "rosters": [roster()], "certs": { "a1": [[browser()], []] },
        "names": {},
    });
    let got: std::collections::BTreeMap<String, SharedTeamAnswer> = round(json!({ "t1": one }));
    assert!(got["t1"].locked);
}

#[test]
fn an_older_controls_shared_team_parses() {
    let got: std::collections::BTreeMap<String, SharedTeamAnswer> =
        parse(&json!({ "t1": { "locked": false, "rosters": [], "certs": {} } }));
    assert!(got["t1"].name.is_empty() && got["t1"].names.is_empty());
}

#[test]
fn peers_are_certificates_by_account() {
    let both = json!({ "b1": { "certs": [browser()], "revocations": [revocation()], "name": "bea" } });
    let got: std::collections::BTreeMap<String, PeerCerts> = round(both);
    assert_eq!(got["b1"].name, "bea");
    // The daemon's fake control, and a control before names (M30).
    let fake: std::collections::BTreeMap<String, PeerCerts> =
        parse(&json!({ "b1": { "name": "bea", "certs": [browser()], "revocations": [] } }));
    assert_eq!(fake["b1"].certs.len(), 1);
    let old: PeerCerts = parse(&json!({ "certs": [browser()] }));
    assert!(old.name.is_empty() && old.revocations.is_empty());
    assert_eq!(parse::<PeerCerts>(&json!({})), PeerCerts::default());
}

#[test]
fn the_access_list_is_what_the_daemon_publishes() {
    let a = round::<AccessList>(json!({ "accounts": ["a1", "b1"], "links_until": 1_790_000_000_000u64 }));
    assert_eq!(a.links_until, Some(1_790_000_000_000));
    // No link open: `null`, not left out.
    let none = round::<AccessList>(json!({ "accounts": [], "links_until": null }));
    assert_eq!(none.links_until, None);
    // routing_wire.rs posts no `links_until` at all.
    assert_eq!(parse::<AccessList>(&json!({ "accounts": [] })).links_until, None);
}

// ---------------------------------------------------------------- queries

/// The paths as the daemon built them with `format!`, and the queries as
/// control reads them.
#[test]
fn queries_read_what_the_daemon_writes() {
    fn query<T: DeserializeOwned>(path: &str) -> T {
        serde_urlencoded::from_str(path.split_once('?').unwrap().1).unwrap()
    }
    let f = Features::path("presigned-invites,owner-moves");
    assert_eq!(f, "/api/daemon/trust?features=presigned-invites,owner-moves");
    assert_eq!(query::<Features>(&f).features, "presigned-invites,owner-moves");
    let t = TeamQuery::path(3, "a,b");
    assert_eq!(t, "/api/daemon/team?since=3&features=a,b");
    let q: TeamQuery = query(&t);
    assert_eq!((q.since, q.features.as_str()), (3, "a,b"));
    let ts = TeamsQuery::path(&["t1", "t2"], "a");
    assert_eq!(ts, "/api/daemon/teams?ids=t1,t2&features=a");
    let q: TeamsQuery = query(&ts);
    assert_eq!((q.ids.as_str(), q.features.as_str()), ("t1,t2", "a"));
    let p = PeersQuery::path(&["a1".into(), "b1".into()]);
    assert_eq!(p, "/api/daemon/peers?accounts=a1,b1");
    assert_eq!(query::<PeersQuery>(&p).accounts, "a1,b1");
    // Older daemons send no `since` or `features`.
    assert_eq!(serde_urlencoded::from_str::<TeamQuery>("").unwrap().since, 0);
    assert!(serde_urlencoded::from_str::<Features>("").unwrap().features.is_empty());
    assert!(serde_urlencoded::from_str::<TeamsQuery>("ids=t1").unwrap().features.is_empty());
}

#[test]
fn the_dial_query_carries_the_direct_urls_as_json() {
    let urls = vec!["https://a.example".to_owned(), "http://10.0.0.2:7777".to_owned()];
    let q = DialQuery::new(&urls);
    assert_eq!(q.urls.as_deref(), Some(r#"["https://a.example","http://10.0.0.2:7777"]"#));
    assert_eq!(q.direct_urls(), Some(urls));
    assert!(DialQuery::default().direct_urls().is_none());
    assert!(DialQuery { urls: Some("not json".into()) }.direct_urls().is_none());
    assert!(serde_urlencoded::from_str::<DialQuery>("").unwrap().urls.is_none());
}

// ---------------------------------------------------------------- control.json

#[test]
fn control_json_is_what_control_says() {
    let full = json!({
        "control": true, "url": "https://control.example", "github": true, "passkeys": true,
        "vapid": "BKey", "github_app": null, "daemon_auth": 2, "cli_join": 1,
        "guest_ssh": { "host": "jump.example", "port": 22, "known_hosts": "jump.example ssh-ed25519 AAAA", "fingerprint": "SHA256:x" },
    });
    let c = round::<ControlInfo>(full.clone());
    assert_eq!(c.guest_ssh.unwrap().port, 22);
    let mut bare = full;
    bare["guest_ssh"] = Value::Null;
    bare["github_app"] = json!("slug");
    round::<ControlInfo>(bare);
}

/// The daemon's fake control says only `daemon_auth`; an older control says
/// no more than that and `control`.
#[test]
fn an_older_controls_control_json_parses() {
    let c: ControlInfo = parse(&json!({ "daemon_auth": 2 }));
    assert_eq!(c.daemon_auth, 2);
    assert!(c.guest_ssh.is_none() && c.cli_join == 0);
    let old: ControlInfo = parse(&json!({ "control": true, "url": "u" }));
    assert_eq!(old.daemon_auth, 0);
}

/// A `/control.json` with a field this daemon can't take still says how to
/// sign, and still has its jump host.
#[test]
fn a_malformed_field_in_control_json_loses_neither_auth_nor_the_jump_host() {
    let bad_jump = json!({ "daemon_auth": 2, "guest_ssh": "nope", "vapid": 5, "github_app": { "slug": "x" } });
    assert!(serde_json::from_value::<ControlInfo>(bad_jump.clone()).is_err());
    assert_eq!(parse::<ControlAuth>(&bad_jump).daemon_auth, 2);
    let bad_vapid = json!({
        "daemon_auth": 2, "vapid": 5,
        "guest_ssh": { "host": "jump.example", "port": 22, "known_hosts": "jump.example ssh-ed25519 AAAA" },
    });
    assert_eq!(parse::<ControlJump>(&bad_vapid).guest_ssh.unwrap().host, "jump.example");
    assert!(parse::<ControlJump>(&json!({ "daemon_auth": 2 })).guest_ssh.is_none());
    assert_eq!(parse::<ControlAuth>(&json!({})).daemon_auth, 0);
}

// ---------------------------------------------------------------- the rest

#[test]
fn the_410_says_when_and_by_whom() {
    let to_holder = json!({ "error": "gone", "removed": { "at": 1_790_000_000_000u64, "by": "laptop" } });
    let r = round::<RemovedAnswer>(to_holder);
    assert_eq!(r.removed.unwrap().by.as_deref(), Some("laptop"));
    // Said to anyone else: no `by`, and `routing_wire.rs` checks it is absent.
    let anyone = round::<RemovedAnswer>(json!({ "error": "gone", "removed": { "at": 1_790_000_000_000u64 } }));
    assert!(anyone.removed.unwrap().by.is_none());
    // The daemon takes anything: no body, or no `removed`.
    assert!(parse::<RemovedAnswer>(&json!({})).error.is_none());
}

#[test]
fn an_ack_is_an_empty_object() {
    assert_eq!(serde_json::to_value(Ack {}).unwrap(), json!({}));
    parse::<Ack>(&json!({}));
}

// ---------------------------------------------------------------- the CLI's join

/// What the CLI's `login` built with `json!`: only `cert` and `proof`.
#[test]
fn the_cli_joins_with_a_cert_and_a_proof_only() {
    let recorded = json!({ "cert": cert(), "proof": { "ms": 1_790_000_000_000u64, "sig": "ab" } });
    round::<CliJoinRequest>(recorded.clone());
    // Control reads it as the daemon's request, the rest defaulted.
    let r: JoinRequest = parse(&recorded);
    assert!(r.urls.is_empty() && r.team.is_none() && r.ticket.is_none() && r.features.is_empty());
}

/// The CLI read `expires_in_secs` with a default of ten minutes.
#[test]
fn a_join_without_a_lifetime_lasts_ten_minutes() {
    let s: JoinStarted = parse(&json!({ "code": "abcd", "poll": "p" }));
    assert_eq!(s.expires_in_secs, 600);
}

/// The CLI only needed `cli_join` of `/control.json`: a field it can't take
/// elsewhere in it doesn't hide that.
#[test]
fn the_cli_reads_only_cli_join_of_control_json() {
    let c: ControlCliJoin = parse(&json!({ "control": "yes", "vapid": 1, "cli_join": 1 }));
    assert_eq!(c.cli_join, Some(1));
    let old: ControlCliJoin = parse(&json!({ "control": true, "daemon_auth": 2 }));
    assert_eq!(old.cli_join, None);
}

// ---------------------------------------------------------------- push

fn push_sub() -> Value {
    json!({
        "v": 1, "account": "a1", "device": "r1", "endpoint": "https://fcm.googleapis.com/fcm/send/x",
        "p256dh": "AAAA", "auth": "BBBB", "at": 1_790_000_000_000u64, "sig": "ab",
    })
}

/// The daemon's fake control in `control.rs`'s tests: `{subs}`.
#[test]
fn push_subs_are_what_control_answers() {
    let s = round::<PushSubs>(json!({ "subs": [push_sub()] }));
    assert_eq!(s.subs[0].device, "r1");
    round::<PushSubs>(json!({ "subs": [] }));
    // Control answers with the bodies it stored, unread.
    round::<PushSubs<Value>>(json!({ "subs": [push_sub(), { "account": "a1" }] }));
}

/// What the daemon built with `json!` to relay a push: `endpoint` and
/// `body`, nothing else; one with `ttl` and `urgency` (control reads both)
/// keeps them.
#[test]
fn a_push_request_is_what_the_daemon_posts() {
    let r = round::<PushRequest>(json!({ "endpoint": "https://fcm.googleapis.com/x", "body": "AAAA" }));
    assert!(r.ttl.is_none() && r.urgency.is_none());
    let full = json!({ "endpoint": "https://fcm.googleapis.com/x", "body": "AAAA", "ttl": 60, "urgency": "low" });
    let r = round::<PushRequest>(full);
    assert_eq!((r.ttl, r.urgency.as_deref()), (Some(60), Some("low")));
}

#[test]
fn a_push_answer_is_the_push_services_status() {
    assert_eq!(round::<PushAnswer>(json!({ "status": 201 })).status, 201);
}

// ---------------------------------------------------------------- TURN

fn cloudflare_server() -> Value {
    json!({
        "urls": ["stun:stun.cloudflare.com:3478", "turn:turn.cloudflare.com:3478?transport=udp"],
        "username": "u", "credential": "c",
    })
}

/// Control's answer with a TURN key, and without.
#[test]
fn ice_servers_are_what_control_answers() {
    let turn = round::<IceServers>(json!({ "ice_servers": [cloudflare_server()], "ttl": 28_800, "turn": true }));
    assert_eq!(turn, IceServers::turn(vec![parse(&cloudflare_server())], 28_800));
    let stun = round::<IceServers>(
        json!({ "ice_servers": [{ "urls": ["stun:stun.cloudflare.com:3478"] }], "ttl": 28_800, "turn": false }),
    );
    assert_eq!(stun, IceServers::stun(28_800));
}

/// The daemon's own answer when it isn't joined or control can't be
/// reached, as it was built with `json!`: no `ttl`.
#[test]
fn the_stun_only_fallback_is_what_the_daemon_built() {
    let recorded = json!({ "ice_servers": [{ "urls": ["stun:stun.cloudflare.com:3478"] }], "turn": false });
    assert_eq!(serde_json::to_value(IceServers::stun_only()).unwrap(), recorded);
    assert_eq!(round::<IceServers>(recorded), IceServers::stun_only());
}

/// The web client takes whatever `RTCIceServer` holds, so what Cloudflare
/// adds to a server (or control to the answer's servers) is passed on, and
/// `urls` may be one string.
#[test]
fn an_ice_server_passes_on_what_it_isnt_told_about() {
    let server =
        json!({ "urls": "turn:t.example:3478", "username": "u", "credential": "c", "credentialType": "password" });
    round::<IceServers>(json!({ "ice_servers": [server], "ttl": 1, "turn": true }));
}

/// An older control (or one that left something out) still parses: the web
/// reads `ice_servers` alone, and `?? []`.
#[test]
fn an_older_controls_ice_servers_parse() {
    let full = json!({ "ice_servers": [cloudflare_server()], "ttl": 1, "turn": true });
    for k in ["ttl", "turn", "ice_servers"] {
        parse::<IceServers>(&without(&full, k));
    }
    assert!(parse::<IceServers>(&json!({})).ice_servers.is_empty());
}
