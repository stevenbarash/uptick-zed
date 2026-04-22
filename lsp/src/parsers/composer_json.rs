use crate::manifest::RawEntry;
use crate::parsers::json_common;

const GROUPS: &[&str] = &["require", "require-dev"];

pub fn parse(source: &str) -> Vec<RawEntry> {
    let mut entries = json_common::parse_deps(source, GROUPS);
    // Composer has a handful of meta-packages in `require` that aren't real
    // Packagist packages (the PHP version constraint, PHP extensions,
    // composer-plugin-api, etc.). Drop them up front so we don't make futile
    // HTTP requests.
    entries.retain(|e| {
        let n = e.name.as_str();
        n != "php"
            && !n.starts_with("ext-")
            && !n.starts_with("lib-")
            && n != "composer-plugin-api"
            && n != "composer-runtime-api"
    });
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_require_blocks_and_skips_php() {
        let src = r#"{
  "require": {
    "php": ">=8.1",
    "ext-json": "*",
    "monolog/monolog": "^2.0"
  },
  "require-dev": {
    "phpunit/phpunit": "^10.0"
  }
}"#;
        let entries = parse(src);
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["monolog/monolog", "phpunit/phpunit"]);
    }
}
