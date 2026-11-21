//! Everything the MCP server shows the AI model: the `initialize` instructions and every tool's
//! name, description and schema. AI clients take a changed definition without asking the user
//! again, so a change must fail CI here and be reviewed like code (security review-11-17,
//! s02:t-md-tool-poisoning). After a reviewed change, run
//! `UPDATE_DEFINITIONS=1 cargo test -p brim-mcp --test definitions` and commit the file.
use serde_json::json;

#[test]
fn model_visible_definitions_match_the_reviewed_copy() {
    let now = serde_json::to_string_pretty(&json!({
        "instructions": {
            "write": brim_mcp::INSTRUCTIONS_WRITE,
            "read": brim_mcp::INSTRUCTIONS_READ,
        },
        "tools": brim_mcp::tools::list(),
        "write_tools": brim_mcp::writes::list(),
    }))
    .unwrap()
        + "\n";
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/definitions.json");
    // Never in CI: there the update would compare the file with itself and always pass.
    if std::env::var_os("UPDATE_DEFINITIONS").is_some() && std::env::var_os("CI").is_none() {
        std::fs::write(path, &now).unwrap();
    }
    let kept = std::fs::read_to_string(path).unwrap_or_default();
    assert!(
        kept == now,
        "an MCP tool definition or the instructions changed: review the change, then run \
         UPDATE_DEFINITIONS=1 cargo test -p brim-mcp --test definitions and commit \
         crates/mcp/tests/definitions.json"
    );
}
