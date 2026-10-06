//! The web client's copy of these types, `web/src/proto.gen.ts`, made
//! from the Rust (ts-rs, the `ts` feature). [`current`] fails when the
//! committed file differs from what the types say now; `just proto-ts`
//! writes it again.

use std::{any::TypeId, collections::HashSet, path::PathBuf};

use ts_rs::{Config, TS, TypeVisitor};

use crate::{ClientMsg, RemoteRef, ServerMsg, api::ActRequest, hosts::HostFeatures};

/// Rust's aliases, which ts-rs sees through: the web client names them.
const ALIASES: &[&str] = &["PaneId", "TabId", "SessionId", "NodeId", "ClientId", "MachineId"];

/// Every type reachable from the roots, declared once, by name.
struct Collect<'a> {
    cfg: &'a Config,
    seen: HashSet<TypeId>,
    decls: Vec<(String, String)>,
}

impl TypeVisitor for Collect<'_> {
    fn visit<T: TS + 'static + ?Sized>(&mut self) {
        // Only types with a declaration of their own: derived ones.
        if T::output_path().is_none() || !self.seen.insert(TypeId::of::<T>()) {
            return;
        }
        let mut decl = T::docs().unwrap_or_default();
        decl.push_str("export ");
        decl.push_str(&T::decl(self.cfg));
        self.decls.push((T::ident(self.cfg), decl));
        T::visit_dependencies(self);
    }
}

fn generate() -> String {
    // Offsets and times stay far below 2^53, so a number is exact.
    let cfg = Config::new().with_large_int("number");
    let mut c = Collect { cfg: &cfg, seen: HashSet::new(), decls: Vec::new() };
    c.visit::<ServerMsg>();
    c.visit::<ClientMsg>();
    c.visit::<ActRequest>();
    c.visit::<HostFeatures>();
    c.visit::<RemoteRef>();
    c.decls.sort();
    let mut out = String::from(
        "// Generated from crates/proto by `just proto-ts`: don't edit. Change the\n\
         // Rust types and run it again; CI checks this file is current.\n\n",
    );
    for a in ALIASES {
        out.push_str(&format!("export type {a} = number;\n"));
    }
    out.push_str(&format!("\nexport const CALL_MAX = {};\n", crate::CALL_MAX));
    for (_, decl) in &c.decls {
        out.push('\n');
        out.push_str(decl);
        out.push('\n');
    }
    out
}

fn path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/src/proto.gen.ts")
}

#[test]
fn current() {
    let want = generate();
    if std::env::var_os("ILLOGICAL_WRITE_TS").is_some() {
        std::fs::write(path(), &want).unwrap();
        return;
    }
    let have = std::fs::read_to_string(path()).unwrap_or_default();
    assert!(have == want, "web/src/proto.gen.ts is stale: run `just proto-ts`");
}
