//! Self-test: prove the structural diff isolates exactly the field an edit
//! touched. This is the stronger successor to the line-count heuristic in
//! roundtrip.rs — instead of counting changed text lines, it asserts the
//! semantic [`SaveDiff`] contains precisely the intended `FieldChange`s.

use std::path::Path;
use tise::statics;
use tise::{DiffKind, IgnoreRules, TiValue};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Edit councilor 3896's three name fields, roundtrip through
/// serialize -> reload, then diff the reloaded save against the pristine load.
/// The diff must contain exactly one Changed object (the councilor) whose only
/// field changes are the three names we set — nothing else.
#[test]
fn diff_isolates_exactly_the_edited_fields() -> Result<()> {
    let input_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("LargeGame.json");
    if !input_path.exists() {
        return Err(format!("Missing test fixture: {input_path:?}").into());
    }

    // Pristine baseline.
    let before = tise::LoadedSave::load_path(&input_path)?;

    // Edited copy: load fresh, mutate three name fields, reserialize.
    let mut edited = tise::LoadedSave::load_path(&input_path)?;
    let Some(props) = edited.get_object_value_mut(statics::TI_GROUP_COUNCILOR_STATE, 3896) else {
        return Err("Could not locate TICouncilorState ID 3896".into());
    };
    props.insert(
        statics::TI_PROP_DISPLAY_NAME.to_string(),
        TiValue::String("Bob Ross".to_string()),
    );
    props.insert(
        statics::TI_PROP_FAMILY_NAME.to_string(),
        TiValue::String("Ross".to_string()),
    );
    props.insert(
        statics::TI_PROP_PERSONAL_NAME.to_string(),
        TiValue::String("Bob".to_string()),
    );
    edited.mark_dirty();

    // Roundtrip through bytes so we are diffing a genuinely reloaded save,
    // exercising parse + index rebuild on the edited side.
    let out_bytes = edited.save_bytes_for_format(tise::SaveFormat::Json5)?;
    let dir = tempfile::tempdir()?;
    let after_path = dir.path().join("edited.json");
    std::fs::write(&after_path, &out_bytes)?;
    let after = tise::LoadedSave::load_path(&after_path)?;

    let diff = tise::diff_saves(&before, &after, &IgnoreRules::new());

    // Exactly one object changed, and it is councilor 3896.
    assert_eq!(
        diff.objects.len(),
        1,
        "expected exactly one changed object, got {}: {:?}",
        diff.objects.len(),
        diff.objects
            .iter()
            .map(|o| (&o.group, o.id, o.kind))
            .collect::<Vec<_>>()
    );
    assert!(diff.groups_added.is_empty());
    assert!(diff.groups_removed.is_empty());

    let obj = &diff.objects[0];
    assert_eq!(obj.kind, DiffKind::Changed);
    assert_eq!(obj.id, 3896);
    assert_eq!(obj.group, statics::TI_GROUP_COUNCILOR_STATE);

    // Exactly the three fields we set, with the right after-values.
    let mut paths: Vec<&str> = obj.field_changes.iter().map(|c| c.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        vec!["displayName", "familyName", "personalName"],
        "diff touched unexpected fields"
    );

    let after_of = |path: &str| -> Option<&TiValue> {
        obj.field_changes
            .iter()
            .find(|c| c.path == path)
            .and_then(|c| c.after.as_ref())
    };
    assert_eq!(
        after_of("displayName"),
        Some(&TiValue::String("Bob Ross".into()))
    );
    assert_eq!(
        after_of("familyName"),
        Some(&TiValue::String("Ross".into()))
    );
    assert_eq!(
        after_of("personalName"),
        Some(&TiValue::String("Bob".into()))
    );

    Ok(())
}

/// A save diffed against itself is empty — the fundamental invariant.
#[test]
fn diff_of_identical_saves_is_empty() -> Result<()> {
    let input_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("PrunedGameMore.json");
    if !input_path.exists() {
        return Err(format!("Missing test fixture: {input_path:?}").into());
    }
    let a = tise::LoadedSave::load_path(&input_path)?;
    let b = tise::LoadedSave::load_path(&input_path)?;
    let diff = tise::diff_saves(&a, &b, &IgnoreRules::new());
    assert!(
        diff.is_empty(),
        "identical saves must produce an empty diff"
    );
    Ok(())
}
