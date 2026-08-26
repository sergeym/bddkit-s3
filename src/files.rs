//! Where a step's bytes come from and go to. Two sources, both named in the
//! step text, so a reader never has to guess which one a step used.

use std::path::{Component, Path, PathBuf};

/// A file inside this feature file's workspace. The name must be bare, and
/// everything else is refused: a separator, a `.` or `..`, an absolute path, a
/// trailing slash, an empty string, and a NUL byte — which is what `<<null>>`
/// interpolates to, and which `Path` would otherwise accept as an ordinary
/// character. This is a trust boundary: without the check a feature file
/// writes anywhere the CI user can write.
// No caller until Task 10/11 wire the file steps into steps.rs.
#[allow(dead_code)]
pub fn in_workspace(workspace_dir: &str, name: &str) -> Result<PathBuf, String> {
    if name.is_empty() {
        return Err("the file name must not be empty".to_string());
    }
    if name.contains('\0') {
        return Err(format!("{name:?} must not contain a NUL byte"));
    }
    let path = Path::new(name);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(single)), None) if single == name => {
            Ok(Path::new(workspace_dir).join(single))
        }
        _ => Err(format!(
            "{name:?} must be a bare file name: no directories, no \"..\", no absolute path"
        )),
    }
}

/// A prepared file. A relative path resolves against `fixtures_dir` when the
/// instance declares one, and against the process working directory otherwise;
/// an absolute path is used as given.
// No caller until Task 10/11 wire the file steps into steps.rs.
#[allow(dead_code)]
pub fn fixture(fixtures_dir: &Option<String>, path: &str) -> PathBuf {
    let path = Path::new(path);
    match fixtures_dir {
        Some(dir) if path.is_relative() => Path::new(dir).join(path),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_name_resolves_inside_the_workspace() {
        let path = in_workspace("/tmp/ws", "report.pdf").expect("accepted");
        assert_eq!(path, std::path::Path::new("/tmp/ws/report.pdf"));
    }

    #[test]
    fn a_traversal_is_refused_by_name() {
        for bad in ["../escape.pdf", "a/b.pdf", "/etc/passwd", "", "."] {
            let error = in_workspace("/tmp/ws", bad).expect_err("refused");
            assert!(
                error.contains(bad) || bad.is_empty(),
                "the message must quote the offending value: {error}"
            );
        }
    }

    #[test]
    fn more_traversal_shapes_are_refused() {
        for bad in [
            "..",
            "a/../b.pdf",
            "./report.pdf",
            "sub/",
            "report.pdf/",
            "a\0b",
        ] {
            in_workspace("/tmp/ws", bad).expect_err("refused");
        }
    }

    #[test]
    fn a_fixture_path_resolves_against_fixtures_dir() {
        let path = fixture(&Some("features/files".to_string()), "report.pdf");
        assert_eq!(path, std::path::Path::new("features/files/report.pdf"));
    }

    #[test]
    fn an_absolute_fixture_path_is_used_as_given() {
        let path = fixture(&Some("features/files".to_string()), "/data/report.pdf");
        assert_eq!(path, std::path::Path::new("/data/report.pdf"));
    }

    #[test]
    fn without_fixtures_dir_a_relative_path_is_left_relative() {
        let path = fixture(&None, "features/files/report.pdf");
        assert_eq!(path, std::path::Path::new("features/files/report.pdf"));
    }
}
