//! Real-file smoke test for the `go.mod` parser. Catches format-shape
//! regressions that pure-synthetic unit tests would miss — comments,
//! tab indentation, mixed single-line + block require, indirect markers.

use uptick_lsp::parsers::go_mod;

#[test]
fn parses_fixture_go_mod() {
    let path: std::path::PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", "go.mod"]
        .iter()
        .collect();
    let text = std::fs::read_to_string(&path).expect("read fixture go.mod");
    let entries = go_mod::parse(&text);

    // Expected: 4 (block) + 1 (single) + 0 (second block, both
    // // indirect) = 5 user-facing dependencies.
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        entries.len(),
        5,
        "expected 5 direct deps, got {} ({:?})",
        entries.len(),
        names
    );

    // Spot-check both forms appear and indirect deps don't.
    assert!(names.contains(&"github.com/gin-gonic/gin"));
    assert!(names.contains(&"github.com/google/uuid")); // single-line require
    assert!(
        !names.contains(&"github.com/bytedance/sonic"),
        "indirect deps must not appear in user-facing entries"
    );

    // Spot-check version literals carry the `v` prefix verbatim.
    let gin = entries
        .iter()
        .find(|e| e.name == "github.com/gin-gonic/gin")
        .unwrap();
    assert_eq!(gin.version_literal, "v1.10.0");
}
