//! Slice 4: two-save transient loading. The diff entry points that load from
//! disk and drop the second tree, so the editor never holds two full saves.

use std::path::Path;
use tise::IgnoreRules;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn example(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(name)
}

/// `diff_paths` must yield the same SaveDiff as loading both saves manually and
/// calling `diff_saves`, while only returning the (small) delta.
#[test]
fn diff_paths_matches_in_memory_diff() -> Result<()> {
    let a = example("PrunedGame.json");
    let b = example("PrunedGameMore.json");
    if !a.exists() || !b.exists() {
        return Err("missing example fixtures".into());
    }

    let ignore = IgnoreRules::new();

    // Reference: load both, diff in memory.
    let before = tise::LoadedSave::load_path(&a)?;
    let after = tise::LoadedSave::load_path(&b)?;
    let reference = tise::diff_saves(&before, &after, &ignore);

    // Path-based: loads transiently, drops trees, returns only the diff.
    let via_paths = tise::diff_paths(&a, &b, &ignore)?;

    assert_eq!(
        via_paths, reference,
        "diff_paths must equal the in-memory diff"
    );
    // Sanity: these two distinct saves actually differ, so the test has teeth.
    assert!(
        !reference.is_empty(),
        "fixtures must differ for this test to mean anything"
    );
    Ok(())
}

/// `diff_against_path` keeps `before` resident and loads only `after`.
#[test]
fn diff_against_path_keeps_before_resident() -> Result<()> {
    let a = example("PrunedGame.json");
    let b = example("PrunedGameMore.json");
    if !a.exists() || !b.exists() {
        return Err("missing example fixtures".into());
    }
    let before = tise::LoadedSave::load_path(&a)?;
    let diff = tise::diff_against_path(&before, &b, &IgnoreRules::new())?;

    // `before` is still usable after the call (not consumed/dropped).
    assert!(before.game_id().is_some(), "before save must remain usable");
    assert_eq!(diff, tise::diff_paths(&a, &b, &IgnoreRules::new())?);
    Ok(())
}

/// A missing `after` path is a clean Err, not a panic.
#[test]
fn diff_against_path_missing_file_is_err() -> Result<()> {
    let a = example("PrunedGame.json");
    if !a.exists() {
        return Err("missing example fixture".into());
    }
    let before = tise::LoadedSave::load_path(&a)?;
    let res = tise::diff_against_path(
        &before,
        Path::new("/nonexistent/does-not-exist.json"),
        &IgnoreRules::new(),
    );
    assert!(res.is_err(), "missing after-file must return Err");
    Ok(())
}
