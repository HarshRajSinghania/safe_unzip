//! Junk-paths mode (`unzip -j`): files land at the destination root.
//!
//! https://github.com/tenuo-ai/safe_unzip/issues/4

use safe_unzip::{
    Driver, Error, ExtractionMode, Extractor, Limits, OverwriteMode, OverwritePolicy,
    ValidationMode, ZipAdapter,
};
use std::io::{Cursor, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::tempdir;
use zip::write::FileOptions;

/// Build a zip. Names ending in '/' become directory entries.
fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options: FileOptions<()> = FileOptions::default();
    for (name, content) in entries {
        if name.ends_with('/') {
            zip.add_directory(*name, options).unwrap();
        } else {
            zip.start_file(*name, options).unwrap();
            zip.write_all(content).unwrap();
        }
    }
    zip.finish().unwrap().into_inner()
}

/// Sorted names directly under `dir`.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn read(dir: &Path, name: &str) -> Vec<u8> {
    std::fs::read(dir.join(name)).unwrap()
}

fn nested_zip() -> Vec<u8> {
    make_zip(&[
        ("docs/", b""),
        ("docs/guide/", b""),
        ("docs/guide/intro.md", b"intro"),
        ("src/main.rs", b"fn main() {}"),
        ("README", b"readme"),
    ])
}

fn colliding_zip() -> Vec<u8> {
    make_zip(&[("a/x.txt", b"first"), ("b/x.txt", b"second")])
}

fn symlink_then_file_zip() -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let symlink: FileOptions<()> = FileOptions::default();
    zip.add_symlink("link", "target", symlink).unwrap();
    let file: FileOptions<()> = FileOptions::default().unix_permissions(0o100644);
    zip.start_file("good.txt", file).unwrap();
    zip.write_all(b"good").unwrap();
    zip.finish().unwrap().into_inner()
}

// ============================================================================
// Extractor
// ============================================================================

#[test]
fn extractor_flattens_without_creating_dirs() {
    let dest = tempdir().unwrap();

    let report = Extractor::new(dest.path())
        .unwrap()
        .junk_paths(true)
        .extract(Cursor::new(nested_zip()))
        .unwrap();

    assert_eq!(listing(dest.path()), ["README", "intro.md", "main.rs"]);
    assert_eq!(read(dest.path(), "intro.md"), b"intro");
    assert_eq!(report.files_extracted, 3);
    assert_eq!(report.dirs_created, 0);
}

#[test]
fn extractor_off_by_default() {
    let dest = tempdir().unwrap();

    Extractor::new(dest.path())
        .unwrap()
        .extract(Cursor::new(nested_zip()))
        .unwrap();

    assert_eq!(listing(dest.path()), ["README", "docs", "src"]);
}

#[test]
fn extractor_collision_names_both_entries() {
    let dest = tempdir().unwrap();

    let result = Extractor::new(dest.path())
        .unwrap()
        .junk_paths(true)
        .extract(Cursor::new(colliding_zip()));

    match result {
        Err(Error::PathCollision {
            entry,
            previous,
            path,
        }) => {
            assert_eq!(entry, "b/x.txt");
            assert_eq!(previous, "a/x.txt");
            assert_eq!(path, "x.txt");
        }
        other => panic!("expected PathCollision, got {other:?}"),
    }
}

#[test]
fn extractor_validate_first_collision_writes_nothing() {
    let dest = tempdir().unwrap();

    let result = Extractor::new(dest.path())
        .unwrap()
        .junk_paths(true)
        .mode(ExtractionMode::ValidateFirst)
        .extract(Cursor::new(colliding_zip()));

    assert!(
        matches!(result, Err(Error::PathCollision { .. })),
        "{result:?}"
    );
    assert!(listing(dest.path()).is_empty());
}

#[test]
fn extractor_validate_first_portable_collision_writes_nothing() {
    let zip = make_zip(&[("a/File.TXT", b"first"), ("b/file.txt", b"second")]);
    let dest = tempdir().unwrap();

    let result = Extractor::new(dest.path())
        .unwrap()
        .junk_paths(true)
        .mode(ExtractionMode::ValidateFirst)
        .extract(Cursor::new(zip));

    assert!(
        matches!(result, Err(Error::PathCollision { .. })),
        "{result:?}"
    );
    assert!(listing(dest.path()).is_empty());
}

#[test]
fn extractor_validate_first_rejects_unflattenable_names_before_writing() {
    let zip = make_zip(&[("good.txt", b"good"), (".", b"bad")]);

    for overwrite in [OverwritePolicy::Skip, OverwritePolicy::Overwrite] {
        let dest = tempdir().unwrap();
        let result = Extractor::new(dest.path())
            .unwrap()
            .junk_paths(true)
            .mode(ExtractionMode::ValidateFirst)
            .overwrite(overwrite)
            .extract(Cursor::new(zip.clone()));

        assert!(matches!(result, Err(Error::InvalidFilename { .. })));
        assert!(listing(dest.path()).is_empty(), "{overwrite:?}");
    }
}

#[test]
fn extractor_validate_first_reuses_filter_decisions() {
    let dest = tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let filter_calls = Arc::clone(&calls);

    let report = Extractor::new(dest.path())
        .unwrap()
        .junk_paths(true)
        .mode(ExtractionMode::ValidateFirst)
        .filter(move |_| filter_calls.fetch_add(1, Ordering::SeqCst) == 0)
        .extract(Cursor::new(colliding_zip()))
        .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(report.files_extracted, 1);
    assert_eq!(read(dest.path(), "x.txt"), b"first");
}

#[test]
fn extractor_junk_paths_filter_skips_symlinks_consistently() {
    for mode in [ExtractionMode::Streaming, ExtractionMode::ValidateFirst] {
        let dest = tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let filter_calls = Arc::clone(&calls);

        let report = Extractor::new(dest.path())
            .unwrap()
            .junk_paths(true)
            .mode(mode)
            .filter(move |_| filter_calls.fetch_add(1, Ordering::SeqCst) == 0)
            .extract(Cursor::new(symlink_then_file_zip()))
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1, "{mode:?}");
        assert_eq!(report.files_extracted, 1, "{mode:?}");
        assert_eq!(read(dest.path(), "good.txt"), b"good", "{mode:?}");
        assert!(!dest.path().join("link").exists(), "{mode:?}");
    }
}

#[test]
fn extractor_skip_keeps_first_overwrite_keeps_last() {
    let dest = tempdir().unwrap();
    let report = Extractor::new(dest.path())
        .unwrap()
        .junk_paths(true)
        .overwrite(OverwritePolicy::Skip)
        .extract(Cursor::new(colliding_zip()))
        .unwrap();
    assert_eq!(read(dest.path(), "x.txt"), b"first");
    assert_eq!(report.entries_skipped, 1);

    let dest = tempdir().unwrap();
    Extractor::new(dest.path())
        .unwrap()
        .junk_paths(true)
        .overwrite(OverwritePolicy::Overwrite)
        .extract(Cursor::new(colliding_zip()))
        .unwrap();
    assert_eq!(read(dest.path(), "x.txt"), b"second");
}

#[test]
fn extractor_still_rejects_traversal() {
    // The base name "evil.txt" is harmless; the full path is not.
    let zip = make_zip(&[("../evil.txt", b"evil")]);
    let dest = tempdir().unwrap();

    let result = Extractor::new(dest.path())
        .unwrap()
        .junk_paths(true)
        .extract(Cursor::new(zip));

    assert!(
        matches!(result, Err(Error::PathEscape { .. })),
        "{result:?}"
    );
    assert!(listing(dest.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn junk_paths_rejects_flattened_symlink_escape() {
    use std::os::unix::fs::symlink;

    let zip = make_zip(&[("good.txt", b"good"), ("nested/link", b"pwned")]);
    let outside = tempdir().unwrap();
    let target = outside.path().join("target.txt");
    std::fs::write(&target, b"sensitive").unwrap();

    let extractor_dest = tempdir().unwrap();
    symlink(&target, extractor_dest.path().join("link")).unwrap();
    let result = Extractor::new(extractor_dest.path())
        .unwrap()
        .junk_paths(true)
        .overwrite(OverwritePolicy::Overwrite)
        .mode(ExtractionMode::ValidateFirst)
        .extract(Cursor::new(zip.clone()));
    assert!(matches!(result, Err(Error::PathEscape { .. })));
    assert_eq!(std::fs::read(&target).unwrap(), b"sensitive");
    assert!(!extractor_dest.path().join("good.txt").exists());

    let driver_dest = tempdir().unwrap();
    symlink(&target, driver_dest.path().join("link")).unwrap();
    let result = Driver::new(driver_dest.path())
        .unwrap()
        .junk_paths(true)
        .overwrite(OverwriteMode::Overwrite)
        .validation(ValidationMode::ValidateFirst)
        .extract_zip(ZipAdapter::new(Cursor::new(zip)).unwrap());
    assert!(matches!(result, Err(Error::PathEscape { .. })));
    assert_eq!(std::fs::read(&target).unwrap(), b"sensitive");
    assert!(!driver_dest.path().join("good.txt").exists());
}

#[test]
fn extractor_filter_sees_full_path() {
    // b/x.txt is filtered out, so it can't collide with a/x.txt.
    for mode in [ExtractionMode::Streaming, ExtractionMode::ValidateFirst] {
        let dest = tempdir().unwrap();

        Extractor::new(dest.path())
            .unwrap()
            .junk_paths(true)
            .mode(mode)
            .include_glob(&["a/**"])
            .extract(Cursor::new(colliding_zip()))
            .unwrap();

        assert_eq!(listing(dest.path()), ["x.txt"], "{mode:?}");
        assert_eq!(read(dest.path(), "x.txt"), b"first");
    }
}

#[test]
fn extractor_depth_limit_uses_full_path() {
    let zip = make_zip(&[("a/b/c/d.txt", b"deep")]);
    let dest = tempdir().unwrap();

    let result = Extractor::new(dest.path())
        .unwrap()
        .junk_paths(true)
        .limits(Limits {
            max_path_depth: 2,
            ..Default::default()
        })
        .extract(Cursor::new(zip));

    assert!(
        matches!(result, Err(Error::PathTooDeep { .. })),
        "{result:?}"
    );
}

// ============================================================================
// Driver
// ============================================================================

#[test]
fn driver_zip_flattens() {
    let dest = tempdir().unwrap();

    let report = Driver::new(dest.path())
        .unwrap()
        .junk_paths(true)
        .extract_zip(ZipAdapter::new(Cursor::new(nested_zip())).unwrap())
        .unwrap();

    assert_eq!(listing(dest.path()), ["README", "intro.md", "main.rs"]);
    assert_eq!(report.dirs_created, 0);
}

#[test]
fn driver_zip_collisions() {
    for validation in [ValidationMode::Streaming, ValidationMode::ValidateFirst] {
        let dest = tempdir().unwrap();

        let result = Driver::new(dest.path())
            .unwrap()
            .junk_paths(true)
            .validation(validation)
            .extract_zip(ZipAdapter::new(Cursor::new(colliding_zip())).unwrap());

        assert!(
            matches!(result, Err(Error::PathCollision { .. })),
            "{validation:?}: {result:?}"
        );
        if validation == ValidationMode::ValidateFirst {
            assert!(listing(dest.path()).is_empty());
        }
    }

    let dest = tempdir().unwrap();
    Driver::new(dest.path())
        .unwrap()
        .junk_paths(true)
        .overwrite(OverwriteMode::Overwrite)
        .extract_zip(ZipAdapter::new(Cursor::new(colliding_zip())).unwrap())
        .unwrap();
    assert_eq!(read(dest.path(), "x.txt"), b"second");
}

#[test]
fn driver_validate_first_reuses_filter_decisions() {
    let dest = tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let filter_calls = Arc::clone(&calls);

    let report = Driver::new(dest.path())
        .unwrap()
        .junk_paths(true)
        .validation(ValidationMode::ValidateFirst)
        .filter(move |_| filter_calls.fetch_add(1, Ordering::SeqCst) == 0)
        .extract_zip(ZipAdapter::new(Cursor::new(colliding_zip())).unwrap())
        .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(report.files_extracted, 1);
    assert_eq!(read(dest.path(), "x.txt"), b"first");
}

#[test]
fn driver_validate_first_rejects_unflattenable_names_before_writing() {
    let zip = make_zip(&[("good.txt", b"good"), (".", b"bad")]);

    for overwrite in [OverwriteMode::Skip, OverwriteMode::Overwrite] {
        let dest = tempdir().unwrap();
        let result = Driver::new(dest.path())
            .unwrap()
            .junk_paths(true)
            .validation(ValidationMode::ValidateFirst)
            .overwrite(overwrite)
            .extract_zip(ZipAdapter::new(Cursor::new(zip.clone())).unwrap());

        assert!(matches!(result, Err(Error::InvalidFilename { .. })));
        assert!(listing(dest.path()).is_empty(), "{overwrite:?}");
    }
}

#[cfg(feature = "tar")]
#[test]
fn driver_tar_flattens() {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, content) in [("pkg/lib/a.txt", &b"a"[..]), ("pkg/b.txt", &b"b"[..])] {
        let mut header = tar::Header::new_gnu();
        header.set_path(name).unwrap();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, content).unwrap();
    }
    let tar = builder.into_inner().unwrap();

    for validation in [ValidationMode::Streaming, ValidationMode::ValidateFirst] {
        let dest = tempdir().unwrap();
        Driver::new(dest.path())
            .unwrap()
            .junk_paths(true)
            .validation(validation)
            .extract_tar(safe_unzip::TarAdapter::new(Cursor::new(tar.clone())))
            .unwrap();
        assert_eq!(listing(dest.path()), ["a.txt", "b.txt"], "{validation:?}");
    }
}

#[cfg(feature = "sevenz")]
#[test]
fn driver_7z_flattens() {
    // basic.7z: a.txt, dir/, dir/b.txt
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/7z/basic.7z");

    for validation in [ValidationMode::Streaming, ValidationMode::ValidateFirst] {
        let dest = tempdir().unwrap();
        Driver::new(dest.path())
            .unwrap()
            .junk_paths(true)
            .validation(validation)
            .extract_7z_file(&fixture)
            .unwrap();
        assert_eq!(listing(dest.path()), ["a.txt", "b.txt"], "{validation:?}");
    }
}

#[cfg(feature = "sevenz")]
#[test]
fn driver_7z_validate_first_matches_cached_filters_by_entry_identity() {
    // basic.7z header order is a.txt, dir/, dir/b.txt. sevenz-rust2 decodes
    // stream entries first and reports the empty directory afterward.
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/7z/basic.7z");
    let dest = tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let filter_calls = Arc::clone(&calls);

    let report = Driver::new(dest.path())
        .unwrap()
        .junk_paths(true)
        .validation(ValidationMode::ValidateFirst)
        .filter(move |_| filter_calls.fetch_add(1, Ordering::SeqCst) == 2)
        .extract_7z_file(&fixture)
        .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(report.files_extracted, 1);
    assert_eq!(listing(dest.path()), ["b.txt"]);
    assert_eq!(read(dest.path(), "b.txt"), b"beta");
}

#[cfg(feature = "async")]
#[tokio::test]
async fn async_extractor_flattens() {
    let dest = tempdir().unwrap();

    safe_unzip::r#async::AsyncExtractor::new(dest.path())
        .unwrap()
        .junk_paths(true)
        .extract_bytes(nested_zip())
        .await
        .unwrap();

    assert_eq!(listing(dest.path()), ["README", "intro.md", "main.rs"]);
}
