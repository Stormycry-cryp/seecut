use seecut_flow_core::{
    Result,
    runner::{Outcome, Output, Runner, Scope},
    sample,
    store::Store,
};
use std::{
    io::{BufRead, Write},
    path::Path,
};
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let path = args
        .get(2)
        .ok_or("usage: seecut-flow-core demo|inspect <new-synthetic-directory>")?;
    match args.get(1).map(String::as_str) {
        Some("serve") => {
            let store = if Path::new(path).exists() {
                Store::open(Path::new(path))?
            } else {
                let (mut doc, graph, _) = sample::document();
                let positions = [
                    ("asset", [0., 220.]),
                    ("prompt", [0., 0.]),
                    ("image", [290., 110.]),
                    ("pick_image", [580., 110.]),
                    ("video", [870., 110.]),
                    ("pick_video", [1160., 110.]),
                    ("delivery", [1450., 110.]),
                ];
                for (node, position) in positions {
                    doc.graphs
                        .get_mut(&graph)
                        .unwrap()
                        .nodes
                        .get_mut(node)
                        .unwrap()
                        .position = position;
                }
                Store::create(Path::new(path), doc)?
            };
            let mut runner = Runner::new(store, Scope::synthetic())?;
            let mut session = seecut_flow_core::protocol::Session::default();
            for line in std::io::stdin().lock().lines() {
                let response = line.map_err(|e| e.to_string()).and_then(|line| {
                    if line.len() > 1_048_576 {
                        return Err("request too large".into());
                    }
                    let request = serde_json::from_str(&line).map_err(|e| e.to_string())?;
                    session.request(&mut runner, request)
                });
                let response = match response {
                    Ok(value) => serde_json::json!({"ok":true,"value":value}),
                    Err(error) => serde_json::json!({"ok":false,"error":error}),
                };
                println!("{response}");
                std::io::stdout().flush().map_err(|e| e.to_string())?;
            }
            Ok(())
        }
        Some("demo") => {
            let (doc, graph_id, target) = sample::document();
            let store = Store::create(Path::new(path), doc)?;
            let mut runner = Runner::new(store, Scope::synthetic())?;
            for select_node in ["pick_image", "pick_video"] {
                let outcome = runner.run_to(&graph_id, &target)?;
                println!("{outcome:?}");
                let Outcome::Paused { node, .. } = outcome else {
                    return Err("expected selection stop".into());
                };
                if node != select_node {
                    return Err("unexpected stop".into());
                }
                let upstream = if node == "pick_image" {
                    "image"
                } else {
                    "video"
                };
                let Output::Candidates { assets, .. } =
                    runner.current_output(&graph_id, upstream)?
                else {
                    return Err("missing candidates".into());
                };
                // Explicit scripted user decision in this deterministic test scenario.
                runner.choose(&graph_id, select_node, &assets[0].id)?;
            }
            println!("{:?}", runner.run_to(&graph_id, &target)?);
            drop(runner);
            let reopened = Runner::new(Store::open(Path::new(path))?, Scope::synthetic())?;
            println!(
                "reopened output: {:?}",
                reopened.current_output(&graph_id, &target)?
            );
            println!(
                "virtual points: {}; simulated deliveries: {}",
                reopened.store.runtime().virtual_spent,
                reopened.store.runtime().deliveries.len()
            );
            Ok(())
        }
        Some("inspect") => {
            let store = Store::open(Path::new(path))?;
            println!(
                "{}",
                serde_json::to_string_pretty(store.runtime()).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        _ => Err("unknown command".into()),
    }
}
