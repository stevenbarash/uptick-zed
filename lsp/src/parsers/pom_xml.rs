//! Maven `pom.xml` parser using `roxmltree`'s position-preserving DOM.
//!
//! Extracts every `<dependency>` under any `<dependencies>` ancestor
//! (top-level, `<dependencyManagement>`, profiles, …). The reported
//! `name` is the standard Maven coordinate `groupId:artifactId`, which
//! is also what Maven Central and OSV's `Maven` ecosystem expect.
//!
//! ## What we skip
//!
//! - Deps with no inline `<version>` — those inherit from parent POMs
//!   or `<dependencyManagement>` and we have no way to resolve them
//!   without a full Maven runtime.
//! - Deps whose `<version>` is a property reference (`${spring.version}`).
//!   Same reason: we'd need to walk `<properties>` and parent POMs.
//!
//! Both omissions are documented in `README.md`'s Known Limitations
//! section once Maven support ships.

use crate::manifest::RawEntry;
use crate::position::LineIndex;

pub fn parse(source: &str) -> Vec<RawEntry> {
    let Ok(doc) = roxmltree::Document::parse(source) else {
        return Vec::new();
    };
    let idx = LineIndex::new(source);
    let mut out = Vec::new();
    visit(doc.root_element(), source, &idx, &mut out);
    out
}

fn visit(node: roxmltree::Node, source: &str, idx: &LineIndex, out: &mut Vec<RawEntry>) {
    if node.has_tag_name("dependency") {
        if let Some(entry) = parse_dependency(node, source, idx) {
            out.push(entry);
        }
        // Don't recurse into a `<dependency>` — its grandchildren
        // (`<exclusions><exclusion>...</exclusion>`) reuse the same
        // groupId/artifactId/version shape but represent excluded
        // transitive deps, which the user can't bump.
        return;
    }
    for child in node.children().filter(|n| n.is_element()) {
        visit(child, source, idx, out);
    }
}

fn parse_dependency(node: roxmltree::Node, source: &str, idx: &LineIndex) -> Option<RawEntry> {
    let group = child_text(node, "groupId")?;
    let artifact = child_text(node, "artifactId")?;
    // `<version>` is required for the entry to be actionable. If absent,
    // the version is inherited from a parent POM or
    // `<dependencyManagement>` — we'd have to resolve that to surface a
    // hint, which is out of scope for v1 Maven support.
    let version_node = child_element(node, "version")?;
    let version_text_node = version_node.children().find(|c| c.is_text())?;
    let version_literal = version_text_node.text()?.trim().to_string();
    if version_literal.is_empty() {
        return None;
    }
    // Property references like `${spring.version}` need parent-POM
    // resolution we can't do. Skip cleanly.
    if version_literal.starts_with("${") {
        return None;
    }

    // roxmltree's `Node::range()` on a text child gives the byte span
    // of the literal text (whitespace included). Trim the surrounding
    // whitespace against the original source to get just the literal.
    let text_range = version_text_node.range();
    let raw_text = source.get(text_range.clone())?;
    let lead_ws = raw_text.len() - raw_text.trim_start().len();
    let trail_ws = raw_text.trim_start().len() - raw_text.trim().len();
    let ver_start = text_range.start + lead_ws;
    let ver_end = text_range.end - trail_ws;

    // For the name, anchor on the `<artifactId>` text. Hover and the
    // document-link surface highlight the artifact id, which is the
    // user-recognisable half of the coordinate.
    let artifact_node = child_element(node, "artifactId")?;
    let artifact_text_node = artifact_node.children().find(|c| c.is_text())?;
    let artifact_range = artifact_text_node.range();

    Some(RawEntry {
        name: format!("{group}:{artifact}"),
        version_literal,
        version_range: idx.range(ver_start..ver_end),
        name_range: idx.range(artifact_range),
        group: Some("dependencies"),
    })
}

/// Find a direct child element by local tag name, ignoring XML
/// namespaces. POMs typically declare `xmlns="http://maven.apache.org/
/// POM/4.0.0"` but plenty of in-the-wild files omit it.
fn child_element<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'i>> {
    node.children()
        .find(|n| n.is_element() && n.tag_name().name() == name)
}

fn child_text<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<String> {
    let child = child_element(node, name)?;
    let text = child.children().find(|c| c.is_text())?.text()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    #[test]
    fn parses_top_level_dependencies() {
        let pom = indoc! {r#"
            <?xml version="1.0" encoding="UTF-8"?>
            <project xmlns="http://maven.apache.org/POM/4.0.0">
              <modelVersion>4.0.0</modelVersion>
              <groupId>com.example</groupId>
              <artifactId>app</artifactId>
              <version>1.0</version>
              <dependencies>
                <dependency>
                  <groupId>org.springframework</groupId>
                  <artifactId>spring-core</artifactId>
                  <version>5.3.0</version>
                </dependency>
                <dependency>
                  <groupId>junit</groupId>
                  <artifactId>junit</artifactId>
                  <version>4.13.2</version>
                  <scope>test</scope>
                </dependency>
              </dependencies>
            </project>
        "#};
        let entries = parse(pom);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "org.springframework:spring-core");
        assert_eq!(entries[0].version_literal, "5.3.0");
        assert_eq!(entries[1].name, "junit:junit");
        assert_eq!(entries[1].version_literal, "4.13.2");
    }

    #[test]
    fn parses_dependencies_under_management_block() {
        // `<dependencyManagement>` is the standard place to centralise
        // versions; we should pick those up too.
        let pom = indoc! {r#"
            <project>
              <dependencyManagement>
                <dependencies>
                  <dependency>
                    <groupId>com.fasterxml.jackson.core</groupId>
                    <artifactId>jackson-databind</artifactId>
                    <version>2.15.2</version>
                  </dependency>
                </dependencies>
              </dependencyManagement>
            </project>
        "#};
        let entries = parse(pom);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].name,
            "com.fasterxml.jackson.core:jackson-databind"
        );
    }

    #[test]
    fn skips_entries_with_no_inline_version() {
        // Inherits from parent POM; we have no resolver, so omit.
        let pom = indoc! {r#"
            <project>
              <dependencies>
                <dependency>
                  <groupId>org.springframework</groupId>
                  <artifactId>spring-core</artifactId>
                </dependency>
              </dependencies>
            </project>
        "#};
        let entries = parse(pom);
        assert!(entries.is_empty());
    }

    #[test]
    fn skips_property_reference_versions() {
        // `${spring.version}` would require walking `<properties>` and
        // potentially parent POMs — beyond v1 Maven scope.
        let pom = indoc! {r#"
            <project>
              <dependencies>
                <dependency>
                  <groupId>org.springframework</groupId>
                  <artifactId>spring-core</artifactId>
                  <version>${spring.version}</version>
                </dependency>
              </dependencies>
            </project>
        "#};
        let entries = parse(pom);
        assert!(entries.is_empty());
    }

    #[test]
    fn skips_exclusion_blocks() {
        // `<exclusions><exclusion>` reuses the groupId/artifactId
        // shape but represents transitives the user wants OUT — not
        // a bumpable entry.
        let pom = indoc! {r#"
            <project>
              <dependencies>
                <dependency>
                  <groupId>org.springframework</groupId>
                  <artifactId>spring-core</artifactId>
                  <version>5.3.0</version>
                  <exclusions>
                    <exclusion>
                      <groupId>commons-logging</groupId>
                      <artifactId>commons-logging</artifactId>
                    </exclusion>
                  </exclusions>
                </dependency>
              </dependencies>
            </project>
        "#};
        let entries = parse(pom);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "org.springframework:spring-core");
    }

    #[test]
    fn malformed_xml_returns_empty_vec() {
        let entries = parse("<project><not closed>");
        assert!(entries.is_empty());
    }
}
