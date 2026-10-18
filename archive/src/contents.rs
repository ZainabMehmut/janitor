//! Contents-<arch> file generation.
//!
//! A `Contents-<arch>` file maps installed file paths to the packages
//! that provide them, letting apt clients answer "which package
//! contains `/usr/bin/foo`?" quickly. Format (see
//! <https://wiki.debian.org/DebianRepository/Format#A.22Contents.22_indices>):
//!
//! ```text
//! bin/uname                                               utils/coreutils
//! usr/bin/hello                                           devel/hello
//! usr/share/doc/foo/copyright                             misc/bar,misc/foo
//! ```
//!
//! Column 1 is the file path (no leading `/`), left-justified to a
//! nominal width. Column 2 is a comma-separated `section/package`
//! list -- a single file can be shipped by multiple packages, and
//! Contents lists them all on one line. The spec drops the old
//! `FILE  LOCATION` header line.
//!
//! We generate (a) an uncompressed `Contents-<arch>` and (b) a
//! gzipped `Contents-<arch>.gz`, and add both to the Release file
//! with size + hashes so apt clients actually pick them up.

use std::collections::BTreeMap;

/// A single package's contribution to a Contents index: its location
/// plus the list of file paths it installs (paths already stripped of
/// leading `./` -- use [`crate::deb::list_deb_files`] to produce them).
#[derive(Debug, Clone)]
pub struct ContentsEntry {
    /// `[<area>/]<section>/<package>`, e.g. `devel/hello` or
    /// `contrib/kernel/firmware-nvidia`, as the package's Section
    /// field gives it.
    pub qualified_name: String,
    /// Every installed file path this package ships. Order is
    /// preserved for stability across runs but need not be sorted;
    /// the writer sorts by file path across all entries.
    pub files: Vec<String>,
}

/// Format a Contents-<arch> file from a set of per-package entries.
///
/// Produces one line per distinct file path, with all packages that
/// provide the same path merged on that line (comma-separated). Lines
/// are sorted alphabetically by file path for stable, diffable output.
pub fn format_contents(entries: &[ContentsEntry]) -> String {
    // Reverse index: file path -> set of packages. Using BTreeMap
    // + BTreeSet gives sorted-by-path output for free and
    // de-duplicates packages within a line.
    let mut by_path: BTreeMap<&str, std::collections::BTreeSet<&str>> = BTreeMap::new();
    for entry in entries {
        for file in &entry.files {
            by_path
                .entry(file.as_str())
                .or_default()
                .insert(entry.qualified_name.as_str());
        }
    }

    let mut out = String::new();
    // Column widths mirror what dak (Debian's archive-kit) writes:
    // 55 characters for the path column, then packages.
    for (path, pkgs) in by_path {
        let joined = pkgs.into_iter().collect::<Vec<_>>().join(",");
        out.push_str(&format!("{:<55} {}\n", path, joined));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(qualified: &str, files: &[&str]) -> ContentsEntry {
        ContentsEntry {
            qualified_name: qualified.to_string(),
            files: files.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn format_contents_single_package_sorts_files() {
        let out = format_contents(&[entry(
            "devel/hello",
            &["usr/share/doc/hello/copyright", "usr/bin/hello"],
        )]);
        assert_eq!(
            out,
            format!(
                "{:<55} devel/hello\n{:<55} devel/hello\n",
                "usr/bin/hello", "usr/share/doc/hello/copyright"
            )
        );
    }

    /// Packages shipping the same file share its line, sorted.
    #[test]
    fn format_contents_merges_shared_paths() {
        let out = format_contents(&[
            entry("misc/foo", &["usr/share/common/doc"]),
            entry("misc/bar", &["usr/share/common/doc"]),
        ]);
        assert_eq!(
            out,
            format!("{:<55} misc/bar,misc/foo\n", "usr/share/common/doc")
        );
    }

    #[test]
    fn format_contents_dedupes_within_package() {
        let out = format_contents(&[entry(
            "misc/foo",
            &["usr/bin/foo", "usr/bin/foo", "usr/bin/foo"],
        )]);
        assert_eq!(out, format!("{:<55} misc/foo\n", "usr/bin/foo"));
    }

    /// A suite without builds gets an empty Contents file.
    #[test]
    fn format_contents_empty() {
        assert_eq!(format_contents(&[]), "");
    }

    /// Paths render verbatim, spaces and all, as dak does.
    #[test]
    fn format_contents_preserves_special_characters() {
        let out = format_contents(&[entry(
            "misc/foo",
            &[
                "usr/share/foo/file with spaces.txt",
                "usr/share/foo/дата.txt",
            ],
        )]);
        assert_eq!(
            out,
            format!(
                "{:<55} misc/foo\n{:<55} misc/foo\n",
                "usr/share/foo/file with spaces.txt", "usr/share/foo/дата.txt"
            )
        );
    }

    /// Lines are sorted by path across all packages, not by package.
    #[test]
    fn format_contents_output_is_sorted_by_path() {
        let out = format_contents(&[
            entry("misc/z-pkg", &["a/first"]),
            entry("misc/a-pkg", &["z/last"]),
        ]);
        assert_eq!(
            out,
            format!(
                "{:<55} misc/z-pkg\n{:<55} misc/a-pkg\n",
                "a/first", "z/last"
            )
        );
    }
}
