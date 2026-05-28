//! Real-file smoke test for the `pom.xml` parser. The fixture exercises
//! the four shapes most likely to drift: namespaced root element,
//! top-level `<dependencies>`, `<dependencyManagement>`, and a dep
//! with an extra `<scope>` child element.

use uptick_lsp::parsers::pom_xml;

#[test]
fn parses_fixture_pom_xml() {
    let path: std::path::PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", "pom.xml"]
        .iter()
        .collect();
    let text = std::fs::read_to_string(&path).expect("read fixture pom.xml");
    let entries = pom_xml::parse(&text);

    // 3 top-level + 1 under dependencyManagement = 4 user-facing deps.
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        entries.len(),
        4,
        "expected 4 deps, got {} ({:?})",
        entries.len(),
        names
    );

    assert!(names.contains(&"org.springframework:spring-core"));
    assert!(names.contains(&"com.fasterxml.jackson.core:jackson-databind"));
    assert!(names.contains(&"junit:junit"));
    assert!(names.contains(&"org.slf4j:slf4j-api"));

    // Spot-check version literal capture is clean (no surrounding
    // whitespace from the XML pretty-print).
    let spring = entries
        .iter()
        .find(|e| e.name == "org.springframework:spring-core")
        .unwrap();
    assert_eq!(spring.version_literal, "5.3.0");
}
