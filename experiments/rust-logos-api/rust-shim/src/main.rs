// Phase B smoke test — equivalent of probe-cpp/main.cpp but in Rust.
// Validates the full path: Rust crate → bindgen-generated FFI →
// C++ shim → Qt event loop → LogosAPI → QtRO → logoscore-hosted
// agent module → back.
//
// Usage:
//   1. Stage a modules dir as in ../README.md (agent + capability_module).
//   2. Start logoscore --mode 0 -m <dir> -l capability_module,agent
//      with the same LOGOS_INSTANCE_ID exported here.
//   3. ./result/bin/agent_info_probe_rs

use logos_shim::Shim;

fn main() {
    let shim = Shim::new("agent_info_probe_rs").expect("shim init");
    match shim.call("agent", "info", "[]", 30_000) {
        Ok(json) => {
            println!("{json}");
            // Try to pretty-print if it parses, otherwise leave raw.
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
                eprintln!(
                    "agent.info() →\n{}",
                    serde_json::to_string_pretty(&v).unwrap_or(json)
                );
            }
        }
        Err(e) => {
            eprintln!("call failed: {e}");
            std::process::exit(1);
        }
    }
}
