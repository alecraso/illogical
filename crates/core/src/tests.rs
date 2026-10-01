use proptest::prelude::*;

use crate::{Edge, Effect, Intent, Mux, Node};

fn mux_with_session() -> Mux {
    let mut m = Mux::new();
    m.apply(Intent::NewSession { name: None, from_pane: None }).unwrap();
    m
}

#[test]
fn new_session_spawns_one_pane_in_one_tab() {
    let mut m = Mux::new();
    let fx = m.apply(Intent::NewSession { name: Some("work".into()), from_pane: None }).unwrap();
    assert_eq!(fx, vec![Effect::Spawn { pane: 1, cwd_from: None }]);
    assert_eq!(m.sessions[0].name, "work");
    assert_eq!(m.tab(1).unwrap().root, Node::pane(1));
}

#[test]
fn split_spawns_with_the_source_pane_cwd() {
    let mut m = mux_with_session();
    let fx = m.apply(Intent::Split { pane: 1, edge: Edge::Right, local: false }).unwrap();
    assert_eq!(fx, vec![Effect::Spawn { pane: 2, cwd_from: Some(1) }]);
    assert_eq!(m.tab(1).unwrap().root.panes(), vec![1, 2]);
}

#[test]
fn closing_the_last_pane_closes_tab_and_session() {
    let mut m = mux_with_session();
    m.apply(Intent::NewTab { session: 1, from_pane: None }).unwrap();
    m.apply(Intent::ClosePane { pane: 2 }).unwrap();
    assert_eq!(m.sessions[0].tabs, vec![1]);
    let fx = m.apply(Intent::ClosePane { pane: 1 }).unwrap();
    assert_eq!(fx, vec![Effect::Kill { pane: 1 }]);
    assert!(m.sessions.is_empty() && m.tabs.is_empty());
}

#[test]
fn move_pane_across_tabs_and_break_it_out_again() {
    let mut m = mux_with_session();
    m.apply(Intent::NewTab { session: 1, from_pane: None }).unwrap();
    // Pane 2 (tab 2) docks left of pane 1; tab 2 is now empty and goes away.
    m.apply(Intent::MovePane { pane: 2, target: 1, edge: Edge::Left }).unwrap();
    assert_eq!(m.sessions[0].tabs, vec![1]);
    assert_eq!(m.tab(1).unwrap().root.panes(), vec![2, 1]);
    m.apply(Intent::BreakPane { pane: 2, session: 1, index: Some(0) }).unwrap();
    assert_eq!(m.sessions[0].tabs.len(), 2);
    let first = m.sessions[0].tabs[0];
    assert_eq!(m.tab(first).unwrap().root, Node::pane(2));
}

#[test]
fn swap_with_center() {
    let mut m = mux_with_session();
    m.apply(Intent::Split { pane: 1, edge: Edge::Right, local: false }).unwrap();
    m.apply(Intent::NewTab { session: 1, from_pane: None }).unwrap();
    m.apply(Intent::MovePane { pane: 3, target: 1, edge: Edge::Center }).unwrap();
    assert_eq!(m.tab(1).unwrap().root.panes(), vec![3, 2]);
    assert_eq!(m.tab(2).unwrap().root.panes(), vec![1]);
}

#[test]
fn dock_tab_merges_its_whole_layout() {
    let mut m = mux_with_session();
    m.apply(Intent::NewTab { session: 1, from_pane: None }).unwrap();
    m.apply(Intent::Split { pane: 2, edge: Edge::Bottom, local: false }).unwrap();
    m.apply(Intent::DockTab { tab: 2, target: 1, edge: Edge::Right }).unwrap();
    assert_eq!(m.sessions[0].tabs, vec![1]);
    assert_eq!(m.tab(1).unwrap().root.panes(), vec![1, 2, 3]);
    assert!(m.apply(Intent::DockTab { tab: 1, target: 1, edge: Edge::Left }).is_err());
}

#[test]
fn resize_split_renormalizes() {
    let mut m = mux_with_session();
    m.apply(Intent::Split { pane: 1, edge: Edge::Right, local: false }).unwrap();
    let split = match &m.tab(1).unwrap().root {
        Node::Split { id, .. } => *id,
        n => panic!("{n:?}"),
    };
    m.apply(Intent::ResizeSplit { split, weights: vec![3.0, 1.0] }).unwrap();
    let l = m.layout(1).unwrap();
    assert_eq!(l.panes[0].1.cols, 59);
    assert_eq!(l.panes[1].1.cols, 20);
    assert!(m.apply(Intent::ResizeSplit { split, weights: vec![1.0] }).is_err());
}

#[test]
fn view_ownership_and_zoom() {
    let mut m = mux_with_session();
    m.apply(Intent::Split { pane: 1, edge: Edge::Right, local: false }).unwrap();
    // First viewer sizes the tab even without claiming.
    assert!(m.view(7, 1, 120, 40, None, false).unwrap());
    // Another client's unclaimed view doesn't change it...
    assert!(!m.view(8, 1, 50, 30, Some(2), false).unwrap());
    // ...until it claims (a phone showing pane 2 alone).
    assert!(m.view(8, 1, 50, 30, Some(2), true).unwrap());
    let rects = m.pane_rects();
    assert_eq!(rects.get(&2).map(|r| (r.cols, r.rows)), Some((50, 30)));
    assert!(!rects.contains_key(&1), "hidden by zoom: keeps its size");
    // The phone leaves; the desktop's next view takes over without a claim.
    assert!(m.release(8));
    assert!(m.view(7, 1, 120, 40, None, false).unwrap());
    assert_eq!(m.pane_rects().len(), 2);
}

#[test]
fn layout_serializes_for_clients() {
    let mut m = mux_with_session();
    m.apply(Intent::Split { pane: 1, edge: Edge::Bottom, local: false }).unwrap();
    let json = serde_json::to_value(&m.tab(1).unwrap().root).unwrap();
    assert_eq!(json["type"], "split");
    assert_eq!(json["dir"], "column");
    assert_eq!(json["children"][0]["node"], serde_json::json!({"type": "pane", "pane": 1}));
    let intent: Intent = serde_json::from_str(r#"{"op":"split","pane":1,"edge":"right"}"#).unwrap();
    assert_eq!(intent, Intent::Split { pane: 1, edge: Edge::Right, local: false });
}

/// An intent built from small random numbers, aimed at IDs that may or may
/// not exist (so failures are exercised too).
fn arb_intent() -> impl Strategy<Value = Intent> {
    let id = 1u32..12;
    let edge =
        prop_oneof![Just(Edge::Left), Just(Edge::Right), Just(Edge::Top), Just(Edge::Bottom), Just(Edge::Center)];
    prop_oneof![
        Just(Intent::NewSession { name: None, from_pane: None }),
        (id.clone(), id.clone()).prop_map(|(s, p)| Intent::NewTab { session: s % 3 + 1, from_pane: Some(p) }),
        (id.clone(), edge.clone()).prop_map(|(pane, edge)| Intent::Split { pane, edge, local: false }),
        id.clone().prop_map(|pane| Intent::ClosePane { pane }),
        id.clone().prop_map(|tab| Intent::CloseTab { tab }),
        id.clone().prop_map(|session| Intent::CloseSession { session: session % 3 + 1 }),
        (id.clone(), id.clone(), edge.clone()).prop_map(|(pane, target, edge)| Intent::MovePane { pane, target, edge }),
        (id.clone(), id.clone(), 0usize..4).prop_map(|(pane, s, i)| Intent::BreakPane {
            pane,
            session: s % 3 + 1,
            index: Some(i)
        }),
        (id.clone(), id.clone(), edge).prop_map(|(tab, target, edge)| Intent::DockTab { tab, target, edge }),
        (id.clone(), id.clone(), 0usize..4).prop_map(|(tab, s, index)| Intent::MoveTab {
            tab,
            session: s % 3 + 1,
            index
        }),
        (id.clone(), prop::collection::vec(-1.0f64..5.0, 1..4))
            .prop_map(|(split, weights)| Intent::ResizeSplit { split, weights }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// Whatever clients send, the state stays valid, effects match what
    /// panes exist, and every visible pane gets at least one cell.
    #[test]
    fn random_intents_keep_the_mux_valid(
        intents in prop::collection::vec(arb_intent(), 1..60),
        views in prop::collection::vec((1u64..4, 1u32..12, 2u16..200, 1u16..80, any::<bool>()), 0..10),
    ) {
        let mut m = Mux::new();
        let mut live = std::collections::BTreeSet::new();
        for intent in intents {
            let before = m.clone();
            match m.apply(intent.clone()) {
                Ok(effects) => {
                    for e in effects {
                        match e {
                            Effect::Spawn { pane, .. } => prop_assert!(live.insert(pane), "spawned {pane} twice"),
                            Effect::Kill { pane } => prop_assert!(live.remove(&pane), "killed unknown {pane}"),
                        }
                    }
                }
                Err(_) => {
                    // A failed intent changes nothing except possibly nothing.
                    prop_assert_eq!(&m.sessions, &before.sessions, "failed {:?} changed sessions", intent);
                }
            }
            prop_assert_eq!(m.validate(), Ok(()), "after {:?}", intent);
            let panes: std::collections::BTreeSet<_> = m.panes().into_iter().collect();
            prop_assert_eq!(&panes, &live, "panes vs effects after {:?}", intent);
        }
        for (client, tab, cols, rows, claim) in views {
            let _ = m.view(client, tab, cols, rows, None, claim);
        }
        for (tab, t) in &m.tabs {
            for (_, r) in m.layout(*tab).unwrap().panes {
                prop_assert!(r.cols >= 1 && r.rows >= 1);
                prop_assert!(r.x < t.cols.max(2) * 4, "rect way outside tab");
            }
        }
    }
}
