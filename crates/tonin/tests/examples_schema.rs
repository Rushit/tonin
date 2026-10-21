//! Every `examples/*/tonin.toml` in the repository passes the strict schema
//! and resolves for dev and prd.

use std::path::Path;

#[test]
fn every_example_tonin_toml_loads() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut checked = 0;
    for entry in std::fs::read_dir(&examples).expect("examples/ exists") {
        let toml = entry.expect("dir entry").path().join("tonin.toml");
        if !toml.is_file() {
            continue;
        }
        for env in ["dev", "prd"] {
            tonin_plugin::Plan::load_with_env(&toml, env)
                .unwrap_or_else(|e| panic!("{} ({env}): {e}", toml.display()));
        }
        checked += 1;
    }
    assert!(
        checked > 0,
        "no examples found under {}",
        examples.display()
    );
}
