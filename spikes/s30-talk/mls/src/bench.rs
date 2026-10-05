//! Timings for question 1 and 2. The same code runs natively and in wasm
//! (web_time::Instant is performance.now() there).
//!
//! For a group of N devices: a creator, an observer, and N-2 others who only
//! contributed KeyPackages (their state isn't needed to time the creator's
//! and observer's work). The tree is "cold": the batch Add leaves most parent
//! nodes blank, which makes commits encrypt to more nodes than a warm tree.

use crate::client::{Device, random32};
use crate::ident::Account;
use crate::client::Provider;
use serde::Serialize;
use web_time::Instant;

#[derive(Serialize, Clone)]
pub struct Row {
    pub n: usize,
    pub op: &'static str,
    pub median_ms: f64,
    pub min_ms: f64,
    pub bytes: Option<usize>,
}

fn med(mut v: Vec<f64>) -> (f64, f64) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (v[v.len() / 2], v[0])
}

fn time<T>(acc: &mut Vec<f64>, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    acc.push(t.elapsed().as_secs_f64() * 1000.0);
    r
}

pub fn run(sizes: &[usize], reps: usize) -> Vec<Row> {
    let seed = Provider::default();
    let acct = Account::new("bench", random32(&seed));
    let mut rows = vec![];
    for &n in sizes {
        let mut creator = Device::new(&acct, "creator", 0);
        let mut observer = Device::new(&acct, "observer", 0);
        let mut t_create = vec![];
        time(&mut t_create, || creator.create_group(b"bench"));
        let mut kps = vec![observer.key_package()];
        for i in 0..n.saturating_sub(2) {
            kps.push(Device::new(&acct, &format!("d{i}"), 0).key_package());
        }
        let mut t_batch = vec![];
        let out = time(&mut t_batch, || creator.add(&kps, vec![]).unwrap());
        let mut t_join0 = vec![];
        time(&mut t_join0, || observer.join_welcome(out.welcome.as_ref().unwrap()).unwrap());
        assert_eq!(creator.g().members().count(), n.max(2));

        let mut kp_gen = vec![];
        let (mut add, mut recv_add, mut join, mut enc, mut dec) = (vec![], vec![], vec![], vec![], vec![]);
        let (mut upd, mut recv_upd, mut rem, mut recv_rem, mut ext, mut recv_ext) = (vec![], vec![], vec![], vec![], vec![], vec![]);
        let (mut sz_add, mut sz_wel, mut sz_gi, mut sz_rem, mut sz_upd, mut sz_ext, mut sz_msg) = (0, 0, 0, 0, 0, 0, 0);
        for r in 0..reps {
            let mut x = Device::new(&acct, &format!("x{r}"), 0);
            let kp = time(&mut kp_gen, || x.key_package());
            let o = time(&mut add, || creator.add(&[kp], vec![]).unwrap());
            time(&mut recv_add, || observer.receive(&o.commit).unwrap());
            let w = o.welcome.unwrap();
            time(&mut join, || x.join_welcome(&w).unwrap());
            (sz_add, sz_wel, sz_gi) = (o.commit.len(), w.len(), o.group_info.len());

            let m = time(&mut enc, || creator.send("a typical chat line, about sixty bytes long, give or take."));
            sz_msg = m.len();
            time(&mut dec, || observer.receive(&m).unwrap());

            let o = time(&mut upd, || observer.self_update(vec![]).unwrap());
            sz_upd = o.commit.len();
            time(&mut recv_upd, || creator.receive(&o.commit).unwrap());

            let leaf = creator.leaves_of(|l| l == x.name);
            let o = time(&mut rem, || creator.remove(&leaf, vec![]).unwrap());
            sz_rem = o.commit.len();
            time(&mut recv_rem, || observer.receive(&o.commit).unwrap());

            let mut y = Device::new(&acct, &format!("y{r}"), 0);
            let o = time(&mut ext, || y.join_external(&o.group_info, vec![]).unwrap());
            sz_ext = o.commit.len();
            time(&mut recv_ext, || creator.receive(&o.commit).unwrap());
            observer.receive(&o.commit).unwrap();
            let leaf = creator.leaves_of(|l| l == y.name);
            let o = creator.remove(&leaf, vec![]).unwrap();
            observer.receive(&o.commit).unwrap();
        }
        let mut push = |op, v: Vec<f64>, bytes| {
            let (m, lo) = med(v);
            rows.push(Row { n, op, median_ms: m, min_ms: lo, bytes });
        };
        push("create group", t_create, None);
        push("batch add N-1 (setup)", t_batch, Some(out.commit.len()));
        push("join from Welcome (setup)", t_join0, Some(out.welcome.as_ref().unwrap().len()));
        push("make KeyPackage", kp_gen, None);
        push("add-one commit", add, Some(sz_add));
        push("  receiver processes add", recv_add, None);
        push("join from Welcome", join, Some(sz_wel));
        push("  GroupInfo (published)", vec![0.0], Some(sz_gi));
        push("encrypt message", enc, Some(sz_msg));
        push("decrypt message", dec, None);
        push("self-update commit", upd, Some(sz_upd));
        push("  receiver processes update", recv_upd, None);
        push("remove-one commit", rem, Some(sz_rem));
        push("  receiver processes remove", recv_rem, None);
        push("external-commit join", ext, Some(sz_ext));
        push("  receiver processes ext join", recv_ext, None);
    }
    rows
}

pub fn table(rows: &[Row]) -> String {
    let mut s = String::from("| N | operation | median ms | min ms | bytes |\n|---|---|---|---|---|\n");
    for r in rows {
        s += &format!("| {} | {} | {:.2} | {:.2} | {} |\n", r.n, r.op, r.median_ms, r.min_ms, r.bytes.map(|b| b.to_string()).unwrap_or_default());
    }
    s
}
