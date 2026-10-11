//! Supplemental tooling tests. These are not Android Studio reference ports.
//! Source proposal only until the lead applies and runs the Rust test suite.

use super::*;

fn request() -> Request {
    Request {
        schema_version: 1,
        repository: REPOSITORY.to_owned(),
        source_sha: "1".repeat(40),
        source_tree: "2".repeat(40),
        source_manifest_sha256: "3".repeat(64),
        source_owner_receipt_sha256: "4".repeat(64),
    }
}

#[test]
fn request_rejects_duplicate_unknown_wrong_type_and_cross_repository_fields() {
    let bytes = canonical(&request(), MAX_REQUEST_BYTES).unwrap();
    assert_eq!(Request::parse(&bytes).unwrap().source_tree, "2".repeat(40));
    let original = String::from_utf8(bytes).unwrap();
    for modified in [
        original.replacen("\"schema_version\":1", "\"schema_version\":true", 1),
        original.replacen(
            "\"schema_version\":1",
            "\"schema_version\":1,\"schema_version\":1",
            1,
        ),
        original.replacen(
            "\"schema_version\":1",
            "\"schema_version\":1,\"extra\":0",
            1,
        ),
        original.replace(REPOSITORY, "other/koda"),
        original.replace(&"1".repeat(40), &"A".repeat(40)),
    ] {
        assert!(Request::parse(modified.as_bytes()).is_err());
    }
    assert!(Request::parse(&[]).is_err());
    assert!(Request::parse(&vec![b' '; MAX_REQUEST_BYTES as usize + 1]).is_err());
}

#[test]
fn paths_and_link_graphs_refuse_escape_cycles_and_ancestor_substitution() {
    for path in [
        "",
        "/etc/passwd",
        "../outside",
        "foo/../bar",
        "foo//bar",
        "foo/./bar",
        "foo\\bar",
        "foo\nbar",
        ".git/config",
    ] {
        assert!(relative(path).is_err(), "{path:?}");
    }
    assert!(relative("crates/δ/file.rs").is_ok());
    assert_eq!(
        normalized_link("crates/demo/LICENSE", "../../LICENSE").unwrap(),
        "LICENSE"
    );
    assert!(normalized_link("demo", "../outside").is_err());
    assert!(normalized_link("demo", "/outside").is_err());
    let cycle = [
        ("a".to_owned(), "b".to_owned()),
        ("b".to_owned(), "a".to_owned()),
    ]
    .into_iter()
    .collect();
    assert!(validate_link_graph(["a", "b"].into_iter(), &cycle).is_err());
    let ancestor = [("a".to_owned(), "real".to_owned())].into_iter().collect();
    assert!(validate_link_graph(["a", "a/file", "real"].into_iter(), &ancestor).is_err());
    assert!(validate_link_graph(["a", "real"].into_iter(), &ancestor).is_ok());
}

#[test]
fn git_sha1_authenticates_blob_header_and_empty_content() {
    assert_eq!(
        git_digest("blob", b""),
        "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
    );
    assert_eq!(
        git_digest("blob", b"hello\n"),
        "ce013625030ba8dba906f756967f9e9ca394464a"
    );
    assert_ne!(
        git_digest("tree", b"hello\n"),
        git_digest("blob", b"hello\n")
    );
}

fn simple_manifest() -> SourceManifest {
    SourceManifest {
        schema_version: 1,
        repository: REPOSITORY.to_owned(),
        commit: "1".repeat(40),
        tree: "2".repeat(40),
        entries: vec![SourceEntry {
            path: "file.rs".to_owned(),
            mode: "100644".to_owned(),
            git_blob: git_digest("blob", b"data"),
            bytes: 4,
            sha256: sha256(b"data"),
            link_target: None,
        }],
    }
}

#[test]
fn source_manifest_requires_supported_modes_nonempty_unique_sorted_bounded_content() {
    let manifest = simple_manifest();
    assert!(manifest.validate().is_ok());
    let mut unsupported = manifest.clone();
    unsupported.entries[0].mode = "160000".to_owned();
    assert!(unsupported.validate().is_err());
    let mut empty = manifest.clone();
    empty.entries.clear();
    assert!(empty.validate().is_err());
    let mut no_bytes = manifest.clone();
    no_bytes.entries[0].bytes = 0;
    assert!(no_bytes.validate().is_err());
    let mut repeated = manifest.clone();
    repeated.entries.push(repeated.entries[0].clone());
    assert!(repeated.validate().is_err());
    let mut oversized = manifest.clone();
    oversized.entries[0].bytes = MAX_OBJECT_BYTES + 1;
    assert!(oversized.validate().is_err());
    let mut malformed = manifest.clone();
    malformed.entries[0].git_blob = "0".repeat(39);
    assert!(malformed.validate().is_err());
    assert!(canonical(&manifest, 8).is_err());
}

#[test]
fn bounded_lines_reject_unterminated_growth_without_unbounded_allocation() {
    let mut input = std::io::Cursor::new(b"line\nend");
    assert_eq!(bounded_line(&mut input, 5).unwrap().unwrap(), b"line\n");
    assert_eq!(bounded_line(&mut input, 5).unwrap().unwrap(), b"end");
    assert!(bounded_line(&mut input, 5).unwrap().is_none());
    assert!(bounded_line(&mut std::io::Cursor::new(b"too long without newline"), 4).is_err());
}

#[cfg(target_os = "linux")]
pub(super) fn fixture() -> (tempfile::TempDir, SourceArgs) {
    use std::os::unix::fs::PermissionsExt as _;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("README.md"), b"original\r\nUnicode: \xce\xb4\n").unwrap();
    fs::write(root.join("src/δ.py"), b"print('hello')\n").unwrap();
    fs::write(root.join("empty"), b"").unwrap();
    fs::write(root.join("script"), b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(root.join("script"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("README.md", root.join("AGENTS.md")).unwrap();
    command(Some(root), "git", &["init", "-q"], 4096).unwrap();
    command(Some(root), "git", &["add", "--all"], 4096).unwrap();
    command(
        Some(root),
        "git",
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-q",
            "--no-gpg-sign",
            "-m",
            "fixture",
        ],
        4096,
    )
    .unwrap();
    let sha = String::from_utf8(command(Some(root), "git", &["rev-parse", "HEAD"], 1024).unwrap())
        .unwrap()
        .trim()
        .to_owned();
    let tree =
        String::from_utf8(command(Some(root), "git", &["rev-parse", "HEAD^{tree}"], 1024).unwrap())
            .unwrap()
            .trim()
            .to_owned();
    let source = SourceArgs {
        source: root.to_owned(),
        sha,
        tree,
    };
    (temporary, source)
}

#[test]
#[cfg(target_os = "linux")]
fn sparse_git_objects_and_real_checkout_have_identical_canonical_content_but_distinct_receipts() {
    let (_temporary, source) = fixture();
    let (objects, object_receipt) = observe(&source, Authentication::GitObjects).unwrap();
    let (physical_manifest, physical_receipt) =
        observe(&source, Authentication::Materialized).unwrap();
    assert_eq!(
        canonical(&objects, MAX_MANIFEST_BYTES).unwrap(),
        canonical(&physical_manifest, MAX_MANIFEST_BYTES).unwrap()
    );
    assert_eq!(
        object_receipt.canonical_manifest,
        physical_receipt.canonical_manifest
    );
    assert_eq!(object_receipt.physically_materialized_entries, 0);
    assert!(!object_receipt.full_physical_noatime_inverses_equal);
    assert!(object_receipt.sparse_files_were_not_materialized);
    assert_eq!(
        physical_receipt.physically_materialized_entries,
        objects.entries.len()
    );
    assert!(physical_receipt.full_physical_noatime_inverses_equal);
    assert_eq!(physical_receipt.runtime_test_or_reference_credit, 0);
    fs::remove_file(source.source.join("README.md")).unwrap();
    let (still_objects, receipt) = observe(&source, Authentication::GitObjects).unwrap();
    assert_eq!(still_objects, objects);
    assert_eq!(receipt.physically_materialized_entries, 0);
    assert!(observe(&source, Authentication::Materialized).is_err());
    assert!(!source.source.join("README.md").exists());
}

#[test]
#[cfg(target_os = "linux")]
fn physical_blob_mode_and_missing_source_mismatches_fail() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_temporary, source) = fixture();
    fs::write(source.source.join("README.md"), b"modified").unwrap();
    assert!(observe(&source, Authentication::Materialized).is_err());
    fs::write(
        source.source.join("README.md"),
        b"original\r\nUnicode: \xce\xb4\n",
    )
    .unwrap();
    fs::set_permissions(
        source.source.join("script"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(observe(&source, Authentication::Materialized).is_err());
}

#[test]
#[cfg(target_os = "linux")]
fn source_reads_preserve_regular_and_link_full13_including_atime() {
    let (_temporary, source) = fixture();
    let root = open_noatime(&source.source, true).unwrap();
    let before_regular = physical(
        &open_beneath(&root, "README.md", false)
            .unwrap()
            .metadata()
            .unwrap(),
    )
    .unwrap();
    let before_link = physical(
        &open_beneath(&root, "AGENTS.md", true)
            .unwrap()
            .metadata()
            .unwrap(),
    )
    .unwrap();
    observe(&source, Authentication::Materialized).unwrap();
    let after_regular = physical(
        &open_beneath(&root, "README.md", false)
            .unwrap()
            .metadata()
            .unwrap(),
    )
    .unwrap();
    let after_link = physical(
        &open_beneath(&root, "AGENTS.md", true)
            .unwrap()
            .metadata()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(before_regular, after_regular);
    assert_eq!(before_link, after_link);
}

#[test]
#[cfg(target_os = "linux")]
fn confined_reads_refuse_symlink_ancestors_and_leaf_following() {
    let temporary = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("file"), b"secret").unwrap();
    std::os::unix::fs::symlink(outside.path(), temporary.path().join("alias")).unwrap();
    let root = open_noatime(temporary.path(), true).unwrap();
    assert!(open_beneath(&root, "alias/file", false).is_err());
    assert!(open_beneath(&root, "alias", false).is_err());
}

fn metadata() -> package::ArtifactMetadata {
    let request = request();
    let digest = || FileDigest {
        bytes: 1,
        sha256: "5".repeat(64),
    };
    package::ArtifactMetadata {
        schema_version: 1,
        repository: REPOSITORY.to_owned(),
        source_sha: request.source_sha,
        source_tree: request.source_tree,
        source_manifest_sha256: request.source_manifest_sha256,
        source_owner_receipt_sha256: request.source_owner_receipt_sha256,
        controller_sha: "6".repeat(40),
        run_id: "1".to_owned(),
        run_attempt: "1".to_owned(),
        runtime_archive: digest(),
        payload_manifest: digest(),
        build_evidence: digest(),
        piece_manifest: digest(),
        pieces: vec![package::Piece {
            order: 0,
            file: "000000.part".to_owned(),
            bytes: 1,
            sha256: "5".repeat(64),
        }],
        unpacked_regular_bytes: 1024,
        directory_count: 3,
        symlink_count: 1,
        regular_file_count: 2,
    }
}

#[test]
fn pieces_require_exact_order_names_nonzero_bounded_sizes_and_total() {
    let metadata = metadata();
    assert!(metadata.validate(&request()).is_ok());
    let mut pieces = metadata.pieces.clone();
    pieces[0].file = "../other".to_owned();
    assert!(package::validate_pieces(&pieces, &metadata.runtime_archive).is_err());
    pieces = metadata.pieces.clone();
    pieces[0].order = 1;
    assert!(package::validate_pieces(&pieces, &metadata.runtime_archive).is_err());
    pieces = metadata.pieces.clone();
    pieces[0].bytes = 0;
    assert!(package::validate_pieces(&pieces, &metadata.runtime_archive).is_err());
    pieces = metadata.pieces.clone();
    pieces[0].bytes = PIECE_BYTES + 1;
    assert!(package::validate_pieces(&pieces, &metadata.runtime_archive).is_err());
    assert!(package::validate_pieces(&[], &metadata.runtime_archive).is_err());
    let wrong_total = FileDigest {
        bytes: 2,
        sha256: metadata.runtime_archive.sha256.clone(),
    };
    assert!(package::validate_pieces(&metadata.pieces, &wrong_total).is_err());
}

#[test]
fn disk_plan_keeps_all_zip_raw_archive_unpacked_padding_scratch_floor_and_inodes() {
    let metadata = metadata();
    let expected = 10 + 2 + 1024 + 41 * 4096 + 64 * 1024 * 1024 + ORIGINAL_FLOOR;
    let plan = disk_plan(
        &metadata,
        10,
        4096,
        expected,
        41,
        ORIGINAL_FLOOR,
        64 * 1024 * 1024,
    )
    .unwrap();
    assert_eq!(plan.required_bytes, expected);
    assert_eq!(plan.required_inodes, 41);
    assert!(plan.fits);
    assert!(
        !disk_plan(
            &metadata,
            10,
            4096,
            expected - 1,
            41,
            ORIGINAL_FLOOR,
            64 * 1024 * 1024
        )
        .unwrap()
        .fits
    );
    assert!(
        !disk_plan(
            &metadata,
            10,
            4096,
            expected,
            40,
            ORIGINAL_FLOOR,
            64 * 1024 * 1024
        )
        .unwrap()
        .fits
    );
    assert!(
        disk_plan(
            &metadata,
            10,
            4096,
            u64::MAX,
            u64::MAX,
            ORIGINAL_FLOOR - 1,
            64 * 1024 * 1024
        )
        .is_err()
    );
    assert!(disk_plan(&metadata, 10, 4096, u64::MAX, u64::MAX, ORIGINAL_FLOOR, 0).is_err());
    assert!(
        disk_plan(
            &metadata,
            0,
            4096,
            u64::MAX,
            u64::MAX,
            ORIGINAL_FLOOR,
            64 * 1024 * 1024
        )
        .is_err()
    );
    assert!(
        disk_plan(
            &metadata,
            u64::MAX,
            4096,
            u64::MAX,
            u64::MAX,
            ORIGINAL_FLOOR,
            64 * 1024 * 1024
        )
        .is_err()
    );
}

#[test]
fn metadata_refuses_zero_payload_and_another_source_owner_or_run() {
    let mut metadata = metadata();
    metadata.unpacked_regular_bytes = 0;
    assert!(metadata.validate(&request()).is_err());
    metadata = super::tests::metadata();
    metadata.source_owner_receipt_sha256 = "9".repeat(64);
    assert!(metadata.validate(&request()).is_err());
    metadata = super::tests::metadata();
    metadata.run_id = "0".to_owned();
    assert!(metadata.validate(&request()).is_err());
}
