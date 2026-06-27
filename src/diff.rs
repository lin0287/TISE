//! Structural comparison of two Terra Invicta save games.
//!
//! Saves are large (tens of MB, thousands of objects), so the diff does not
//! walk the two JSON trees positionally. Every gamestate object carries a
//! stable integer ID (`Key.value`); objects are aligned by `(group, id)` and
//! only the pairs that actually differ are walked field-by-field. Because
//! [`TiValue`] derives `PartialEq`, an unchanged object costs exactly one
//! whole-subtree compare and produces nothing, which is what keeps a 10k-object
//! save tractable.
//!
//! The core is intentionally UI-free so it can be unit-tested headlessly and
//! reused as a library self-test (prove that an edit perturbs *only* the
//! intended field).

use crate::save::LoadedSave;
use crate::value::TiValue;
use std::collections::BTreeSet;

/// Whether an object was added, removed, or changed between two saves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// Present in the "after" save, absent in "before".
    Added,
    /// Present in the "before" save, absent in "after".
    Removed,
    /// Present in both with differing `Value` subtrees.
    Changed,
}

/// A single leaf/subtree change inside a matched object, addressed by a
/// dotted/bracketed JSON path relative to the object's `Value`
/// (e.g. `resources.money`, `historyGDP[3]`).
#[derive(Debug, Clone, PartialEq)]
pub struct FieldChange {
    pub path: String,
    /// `None` when the field only exists in the "after" save (added).
    pub before: Option<TiValue>,
    /// `None` when the field only exists in the "before" save (removed).
    pub after: Option<TiValue>,
}

/// One object-level delta between two saves.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectDiff {
    pub group: String,
    pub id: i64,
    pub display_name: String,
    pub kind: DiffKind,
    /// Field-level changes for `Changed` objects; empty for `Added`/`Removed`.
    pub field_changes: Vec<FieldChange>,
}

/// The full result of comparing two saves.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SaveDiff {
    /// Groups present only in the "after" save.
    pub groups_added: Vec<String>,
    /// Groups present only in the "before" save.
    pub groups_removed: Vec<String>,
    /// All object-level deltas, in a stable order (by group then id).
    pub objects: Vec<ObjectDiff>,
}

impl SaveDiff {
    pub fn is_empty(&self) -> bool {
        self.groups_added.is_empty() && self.groups_removed.is_empty() && self.objects.is_empty()
    }

    pub fn count_kind(&self, kind: DiffKind) -> usize {
        self.objects.iter().filter(|o| o.kind == kind).count()
    }

    /// Return `(added, removed, changed)` object counts in one pass.
    pub fn summary_counts(&self) -> (usize, usize, usize) {
        let mut counts = (0, 0, 0);
        for o in &self.objects {
            match o.kind {
                DiffKind::Added => counts.0 += 1,
                DiffKind::Removed => counts.1 += 1,
                DiffKind::Changed => counts.2 += 1,
            }
        }
        counts
    }

    /// Non-destructively re-filter a previously computed full diff by `ignore`,
    /// returning a new `SaveDiff`. This is the cheap path the UI uses when the
    /// user toggles an ignore checkbox: compute the full diff once (the
    /// expensive tree walk), then re-derive the filtered view from the cached
    /// result without touching the save files again.
    ///
    /// Semantics match `diff_saves` with the same rules applied up front:
    /// - objects in an ignored group/id are dropped entirely;
    /// - for `Changed` objects, field changes on ignored paths are removed, and
    ///   if none survive the object itself is dropped;
    /// - `Added`/`Removed` objects carry no field changes, so they survive any
    ///   path-only ignore and are only removed by a group/id ignore.
    pub fn filtered(&self, ignore: &IgnoreRules) -> SaveDiff {
        let groups_added = self
            .groups_added
            .iter()
            .filter(|g| !ignore.skips_object(g, i64::MIN))
            .cloned()
            .collect();
        let groups_removed = self
            .groups_removed
            .iter()
            .filter(|g| !ignore.skips_object(g, i64::MIN))
            .cloned()
            .collect();

        let mut objects = Vec::new();
        for obj in &self.objects {
            if ignore.skips_object(&obj.group, obj.id) {
                continue;
            }
            match obj.kind {
                DiffKind::Added | DiffKind::Removed => objects.push(obj.clone()),
                DiffKind::Changed => {
                    let field_changes: Vec<FieldChange> = obj
                        .field_changes
                        .iter()
                        .filter(|c| !ignore.skips_path(&c.path))
                        .cloned()
                        .collect();
                    if field_changes.is_empty() {
                        continue;
                    }
                    objects.push(ObjectDiff {
                        field_changes,
                        ..obj.clone()
                    });
                }
            }
        }

        SaveDiff {
            groups_added,
            groups_removed,
            objects,
        }
    }

    /// All groups represented by object-level diffs, sorted for deterministic
    /// UI filter lists.
    pub fn changed_groups(&self) -> Vec<String> {
        self.objects
            .iter()
            .map(|o| o.group.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Non-destructively derive the Compare window's visible subset after the
    /// expensive full diff has already been computed and any noise ignore rules
    /// have already been applied.
    ///
    /// `visible_groups == None` means show every group. `Some(empty_set)` means
    /// show nothing, which powers the UI's "ignore all, then check what I want"
    /// flow. `query` is case-insensitive and matches object metadata (group, id,
    /// display name, kind) or field details (path, before value, after value).
    /// If object metadata matches, all of that object's surviving field changes
    /// remain visible; if only individual fields match, the object remains with
    /// just those matching fields.
    pub fn view_filtered(
        &self,
        visible_groups: Option<&BTreeSet<String>>,
        query: &str,
    ) -> SaveDiff {
        let query = query.trim().to_ascii_lowercase();
        let group_allowed =
            |group: &String| visible_groups.is_none_or(|groups| groups.contains(group));
        let group_matches = |group: &String| query.is_empty() || contains_query(group, &query);

        let groups_added = self
            .groups_added
            .iter()
            .filter(|g| group_allowed(g) && group_matches(g))
            .cloned()
            .collect();
        let groups_removed = self
            .groups_removed
            .iter()
            .filter(|g| group_allowed(g) && group_matches(g))
            .cloned()
            .collect();

        let mut objects = Vec::new();
        for obj in &self.objects {
            if !group_allowed(&obj.group) {
                continue;
            }
            if query.is_empty() || object_metadata_matches_query(obj, &query) {
                objects.push(obj.clone());
                continue;
            }
            if obj.kind != DiffKind::Changed {
                continue;
            }
            let field_changes: Vec<FieldChange> = obj
                .field_changes
                .iter()
                .filter(|c| field_change_matches_query(c, &query))
                .cloned()
                .collect();
            if !field_changes.is_empty() {
                objects.push(ObjectDiff {
                    field_changes,
                    ..obj.clone()
                });
            }
        }

        SaveDiff {
            groups_added,
            groups_removed,
            objects,
        }
    }
}

fn object_metadata_matches_query(obj: &ObjectDiff, query: &str) -> bool {
    contains_query(&obj.group, query)
        || contains_query(&obj.id.to_string(), query)
        || contains_query(&obj.display_name, query)
        || contains_query(
            match obj.kind {
                DiffKind::Added => "added",
                DiffKind::Removed => "removed",
                DiffKind::Changed => "changed",
            },
            query,
        )
}

fn field_change_matches_query(change: &FieldChange, query: &str) -> bool {
    contains_query(&change.path, query)
        || change
            .before
            .as_ref()
            .is_some_and(|v| contains_query(&v.to_json5_compact(), query))
        || change
            .after
            .as_ref()
            .is_some_and(|v| contains_query(&v.to_json5_compact(), query))
}

fn contains_query(haystack: &str, query: &str) -> bool {
    haystack.to_ascii_lowercase().contains(query)
}

/// Predicate-driven filter that suppresses noise from the diff. The UI builds
/// one of these from checkboxes; the diff core never knows the UI exists.
///
/// A field path matches an ignore prefix when it equals the prefix or begins
/// with `prefix` followed by `.` or `[` (so `historyGDP` ignores
/// `historyGDP[3]` but not `historyGDProjection`).
#[derive(Debug, Clone, Default)]
pub struct IgnoreRules {
    groups: Vec<String>,
    ids: Vec<i64>,
    path_prefixes: Vec<String>,
    /// First-segment name prefixes: a field is suppressed when the first
    /// segment of its path *starts with* one of these (e.g. `history` catches
    /// `historyGDP`, `historyWarStatus`, and any future `history*` series).
    field_namespaces: Vec<String>,
}

impl IgnoreRules {
    pub fn new() -> Self {
        Self::default()
    }

    /// Default preset suppressing the per-turn time-series and event-log churn
    /// that dominates a raw diff but carries no editing-relevant signal.
    /// Calibrated against two real continuing saves (history* arrays and the
    /// notification queue accounted for the overwhelming majority of changes).
    pub fn default_noise_preset() -> Self {
        Self::new()
            // Every `history*` field is a per-turn time series appended each
            // game tick. Match the whole namespace by first-segment prefix so
            // uncalibrated series (e.g. `historyWarStatus`, only present when a
            // nation is at war) are suppressed too, not just the ones observed
            // during calibration.
            .ignore_field_namespace("history")
            // Rolling event/notification log.
            .ignore_path_prefix("notificationSummaryQueue")
    }

    pub fn ignore_group(mut self, group: impl Into<String>) -> Self {
        self.groups.push(group.into());
        self
    }

    pub fn ignore_id(mut self, id: i64) -> Self {
        self.ids.push(id);
        self
    }

    pub fn ignore_path_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.path_prefixes.push(prefix.into());
        self
    }

    /// Suppress an entire field namespace: any field whose first path segment
    /// *starts with* `name` is dropped. Unlike `ignore_path_prefix` (which is
    /// an exact-segment match), this catches a whole family of related fields,
    /// e.g. `ignore_field_namespace("history")` suppresses `historyGDP`,
    /// `historyWarStatus`, and any other `history*` series without enumerating
    /// each one.
    pub fn ignore_field_namespace(mut self, name: impl Into<String>) -> Self {
        self.field_namespaces.push(name.into());
        self
    }

    /// True when an entire object (by group or id) should be skipped.
    pub fn skips_object(&self, group: &str, id: i64) -> bool {
        self.groups.iter().any(|g| g == group) || self.ids.contains(&id)
    }

    /// True when a field path (relative to the object's `Value`) is suppressed.
    pub fn skips_path(&self, path: &str) -> bool {
        self.path_prefixes
            .iter()
            .any(|prefix| path_has_prefix(path, prefix))
            || self
                .field_namespaces
                .iter()
                .any(|name| first_segment(path).starts_with(name.as_str()))
    }
}

/// The first path segment: everything up to the first `.` or `[`. For
/// `historyWarStatus[19]` this is `historyWarStatus`; for `publicOpinion.Resist`
/// it is `publicOpinion`.
fn first_segment(path: &str) -> &str {
    let end = path.find(['.', '[']).unwrap_or(path.len());
    &path[..end]
}

/// Match `path` against a segment-aware `prefix`: equal, or `prefix` followed
/// by a `.` (nested key) or `[` (array index).
fn path_has_prefix(path: &str, prefix: &str) -> bool {
    if !path.starts_with(prefix) {
        return false;
    }
    matches!(
        path.as_bytes().get(prefix.len()),
        None | Some(b'.') | Some(b'[')
    )
}

/// Compare two loaded saves, aligning objects by `(group, id)` and walking only
/// the pairs whose `Value` subtrees differ. `ignore` suppresses noisy
/// groups/ids/field-paths.
pub fn diff_saves(before: &LoadedSave, after: &LoadedSave, ignore: &IgnoreRules) -> SaveDiff {
    use std::collections::BTreeSet;

    let mut diff = SaveDiff::default();

    let before_groups: BTreeSet<&String> = before.index.groups.iter().collect();
    let after_groups: BTreeSet<&String> = after.index.groups.iter().collect();

    for g in after_groups.difference(&before_groups) {
        if !ignore.skips_object(g, i64::MIN) {
            diff.groups_added.push((*g).clone());
        }
    }
    for g in before_groups.difference(&after_groups) {
        if !ignore.skips_object(g, i64::MIN) {
            diff.groups_removed.push((*g).clone());
        }
    }

    // Object-level alignment by (group, id) across every group in either save.
    let all_groups: BTreeSet<&String> = before_groups.union(&after_groups).copied().collect();

    for group in all_groups {
        let before_objs = before.index.objects_by_group.get(group);
        let after_objs = after.index.objects_by_group.get(group);

        let before_ids: BTreeSet<i64> = before_objs
            .map(|v| v.iter().map(|o| o.id).collect())
            .unwrap_or_default();
        let after_ids: BTreeSet<i64> = after_objs
            .map(|v| v.iter().map(|o| o.id).collect())
            .unwrap_or_default();

        // Stable, sorted union of ids so output order is deterministic.
        for id in before_ids.union(&after_ids).copied() {
            if ignore.skips_object(group, id) {
                continue;
            }

            let in_before = before_ids.contains(&id);
            let in_after = after_ids.contains(&id);

            match (in_before, in_after) {
                (false, true) => diff.objects.push(ObjectDiff {
                    group: group.clone(),
                    id,
                    display_name: after
                        .index
                        .id_to_display_name
                        .get(&id)
                        .cloned()
                        .unwrap_or_default(),
                    kind: DiffKind::Added,
                    field_changes: Vec::new(),
                }),
                (true, false) => diff.objects.push(ObjectDiff {
                    group: group.clone(),
                    id,
                    display_name: before
                        .index
                        .id_to_display_name
                        .get(&id)
                        .cloned()
                        .unwrap_or_default(),
                    kind: DiffKind::Removed,
                    field_changes: Vec::new(),
                }),
                (true, true) => {
                    let (Some(a), Some(b)) = (
                        before.object_value_node(group, id),
                        after.object_value_node(group, id),
                    ) else {
                        continue;
                    };
                    // Whole-subtree short-circuit: the overwhelming majority of
                    // objects are unchanged and cost exactly one compare here.
                    if a == b {
                        continue;
                    }
                    let mut field_changes = Vec::new();
                    diff_values(a, b, "", ignore, &mut field_changes);
                    // If every concrete change was an ignored path, the object
                    // is effectively unchanged and must not appear.
                    if field_changes.is_empty() {
                        continue;
                    }
                    diff.objects.push(ObjectDiff {
                        group: group.clone(),
                        id,
                        display_name: after
                            .index
                            .id_to_display_name
                            .get(&id)
                            .cloned()
                            .unwrap_or_default(),
                        kind: DiffKind::Changed,
                        field_changes,
                    });
                }
                (false, false) => unreachable!("id came from the union of both sets"),
            }
        }
    }

    diff
}

/// Walk two matched `Value` subtrees and collect leaf/subtree changes as
/// `FieldChange`s, honoring `ignore` path-prefix suppression. `base_path` is
/// the accumulated path from the object root.
pub(crate) fn diff_values(
    before: &TiValue,
    after: &TiValue,
    base_path: &str,
    ignore: &IgnoreRules,
    out: &mut Vec<FieldChange>,
) {
    // Equal subtrees produce nothing; this is the cheap recursive short-circuit.
    if before == after {
        return;
    }
    if !base_path.is_empty() && ignore.skips_path(base_path) {
        return;
    }

    match (before, after) {
        (TiValue::Object(a), TiValue::Object(b)) => {
            // Changed/removed keys, preserving `before` key order.
            for (k, av) in a {
                let path = join_key(base_path, k);
                match b.get(k) {
                    Some(bv) => diff_values(av, bv, &path, ignore, out),
                    None => {
                        if !ignore.skips_path(&path) {
                            out.push(FieldChange {
                                path,
                                before: Some(av.clone()),
                                after: None,
                            });
                        }
                    }
                }
            }
            // Added keys, in `after` order.
            for (k, bv) in b {
                if a.contains_key(k) {
                    continue;
                }
                let path = join_key(base_path, k);
                if !ignore.skips_path(&path) {
                    out.push(FieldChange {
                        path,
                        before: None,
                        after: Some(bv.clone()),
                    });
                }
            }
        }
        (TiValue::Array(a), TiValue::Array(b)) => {
            let max = a.len().max(b.len());
            for i in 0..max {
                let path = format!("{base_path}[{i}]");
                match (a.get(i), b.get(i)) {
                    (Some(av), Some(bv)) => diff_values(av, bv, &path, ignore, out),
                    (Some(av), None) => {
                        if !ignore.skips_path(&path) {
                            out.push(FieldChange {
                                path,
                                before: Some(av.clone()),
                                after: None,
                            });
                        }
                    }
                    (None, Some(bv)) => {
                        if !ignore.skips_path(&path) {
                            out.push(FieldChange {
                                path,
                                before: None,
                                after: Some(bv.clone()),
                            });
                        }
                    }
                    (None, None) => unreachable!(),
                }
            }
        }
        // Type mismatch or differing primitives: a single leaf change.
        _ => out.push(FieldChange {
            path: base_path.to_string(),
            before: Some(before.clone()),
            after: Some(after.clone()),
        }),
    }
}

/// Join a base path with a child object key, using `.` only when nested.
fn join_key(base: &str, key: &str) -> String {
    if base.is_empty() {
        key.to_string()
    } else {
        format!("{base}.{key}")
    }
}

/// Load the `after` save from disk transiently, diff it against an
/// already-loaded `before`, and drop the `after` tree before returning. This
/// is the "Compare against..." entry point: the editor keeps `before` resident
/// and never holds two full save trees at once, only the (small) `SaveDiff`
/// survives.
pub fn diff_against_path(
    before: &LoadedSave,
    after_path: &std::path::Path,
    ignore: &IgnoreRules,
) -> anyhow::Result<SaveDiff> {
    let after = LoadedSave::load_path(after_path)?;
    let diff = diff_saves(before, &after, ignore);
    drop(after); // explicit: release the second tree before we return.
    Ok(diff)
}

/// Load both saves from disk, diff them, and drop both trees, returning only
/// the `SaveDiff`. Convenience for headless/library callers (and the self-test)
/// that have two paths and want just the delta.
pub fn diff_paths(
    before_path: &std::path::Path,
    after_path: &std::path::Path,
    ignore: &IgnoreRules,
) -> anyhow::Result<SaveDiff> {
    let before = LoadedSave::load_path(before_path)?;
    diff_against_path(&before, after_path, ignore)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statics;
    use crate::value::TiNumber;
    use indexmap::IndexMap;

    // --- helpers to build TI-shaped saves -----------------------------------

    fn num(n: i64) -> TiValue {
        TiValue::Number(TiNumber::I64(n))
    }

    fn obj(pairs: Vec<(&str, TiValue)>) -> TiValue {
        let mut m = IndexMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v);
        }
        TiValue::Object(m)
    }

    /// One `{ Key:{value:id}, Value:{..props} }` entry.
    fn entry(id: i64, props: Vec<(&str, TiValue)>) -> TiValue {
        obj(vec![
            (
                statics::TI_FIELD_KEY_CAP,
                obj(vec![(statics::TI_REF_FIELD_VALUE, num(id))]),
            ),
            (statics::TI_FIELD_VALUE_CAP, obj(props)),
        ])
    }

    /// A full save root: `{ gamestates: { group: [entries...] } }`.
    fn save_with(groups: Vec<(&str, Vec<TiValue>)>) -> LoadedSave {
        let mut gs = IndexMap::new();
        for (g, items) in groups {
            gs.insert(g.to_string(), TiValue::Array(items));
        }
        let root = obj(vec![(statics::TI_GAMESTATES, TiValue::Object(gs))]);
        LoadedSave::from_root_for_test(root)
    }

    const G: &str = "PavonisInteractive.TerraInvicta.TITest";

    // --- diff_values (recursive field walker) -------------------------------

    #[test]
    fn diff_values_reports_changed_primitive_leaf() {
        let before = obj(vec![
            ("money", num(100)),
            ("name", TiValue::String("a".into())),
        ]);
        let after = obj(vec![
            ("money", num(250)),
            ("name", TiValue::String("a".into())),
        ]);
        let mut out = Vec::new();
        diff_values(&before, &after, "", &IgnoreRules::new(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "money");
        assert_eq!(out[0].before, Some(num(100)));
        assert_eq!(out[0].after, Some(num(250)));
    }

    #[test]
    fn diff_values_reports_added_and_removed_keys() {
        let before = obj(vec![("keep", num(1)), ("gone", num(9))]);
        let after = obj(vec![("keep", num(1)), ("fresh", num(7))]);
        let mut out = Vec::new();
        diff_values(&before, &after, "", &IgnoreRules::new(), &mut out);
        let removed = out.iter().find(|c| c.path == "gone").unwrap();
        assert_eq!(removed.before, Some(num(9)));
        assert_eq!(removed.after, None);
        let added = out.iter().find(|c| c.path == "fresh").unwrap();
        assert_eq!(added.before, None);
        assert_eq!(added.after, Some(num(7)));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn diff_values_recurses_into_nested_objects_with_dotted_path() {
        let before = obj(vec![("res", obj(vec![("money", num(1))]))]);
        let after = obj(vec![("res", obj(vec![("money", num(2))]))]);
        let mut out = Vec::new();
        diff_values(&before, &after, "", &IgnoreRules::new(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "res.money");
    }

    #[test]
    fn diff_values_compares_arrays_positionally_with_bracket_path() {
        let before = obj(vec![("hist", TiValue::Array(vec![num(1), num(2)]))]);
        let after = obj(vec![("hist", TiValue::Array(vec![num(1), num(2), num(3)]))]);
        let mut out = Vec::new();
        diff_values(&before, &after, "", &IgnoreRules::new(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "hist[2]");
        assert_eq!(out[0].before, None);
        assert_eq!(out[0].after, Some(num(3)));
    }

    #[test]
    fn diff_values_honors_path_prefix_ignore() {
        let before = obj(vec![
            ("historyGDP", TiValue::Array(vec![num(1)])),
            ("money", num(1)),
        ]);
        let after = obj(vec![
            ("historyGDP", TiValue::Array(vec![num(1), num(2)])),
            ("money", num(5)),
        ]);
        let ignore = IgnoreRules::new().ignore_path_prefix("historyGDP");
        let mut out = Vec::new();
        diff_values(&before, &after, "", &ignore, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "money");
    }

    #[test]
    fn diff_values_equal_subtrees_produce_nothing() {
        let v = obj(vec![("a", num(1)), ("b", obj(vec![("c", num(2))]))]);
        let mut out = Vec::new();
        diff_values(&v, &v.clone(), "", &IgnoreRules::new(), &mut out);
        assert!(out.is_empty());
    }

    // --- diff_saves (object alignment) --------------------------------------

    #[test]
    fn diff_saves_detects_added_and_removed_objects_by_id() {
        let before = save_with(vec![(G, vec![entry(1, vec![("hp", num(1))])])]);
        let after = save_with(vec![(
            G,
            vec![
                entry(1, vec![("hp", num(1))]),
                entry(2, vec![("hp", num(5))]),
            ],
        )]);
        let diff = diff_saves(&before, &after, &IgnoreRules::new());
        assert_eq!(diff.count_kind(DiffKind::Added), 1);
        assert_eq!(diff.count_kind(DiffKind::Removed), 0);
        let added = diff
            .objects
            .iter()
            .find(|o| o.kind == DiffKind::Added)
            .unwrap();
        assert_eq!(added.id, 2);
        assert_eq!(added.group, G);
    }

    #[test]
    fn diff_saves_aligns_by_id_not_position() {
        // Same two objects, reversed array order: must produce zero diff.
        let before = save_with(vec![(
            G,
            vec![
                entry(1, vec![("hp", num(1))]),
                entry(2, vec![("hp", num(2))]),
            ],
        )]);
        let after = save_with(vec![(
            G,
            vec![
                entry(2, vec![("hp", num(2))]),
                entry(1, vec![("hp", num(1))]),
            ],
        )]);
        let diff = diff_saves(&before, &after, &IgnoreRules::new());
        assert!(
            diff.is_empty(),
            "reordering objects must not register as a change"
        );
    }

    #[test]
    fn diff_saves_collects_field_changes_for_changed_object() {
        let before = save_with(vec![(G, vec![entry(1, vec![("hp", num(10))])])]);
        let after = save_with(vec![(G, vec![entry(1, vec![("hp", num(99))])])]);
        let diff = diff_saves(&before, &after, &IgnoreRules::new());
        assert_eq!(diff.count_kind(DiffKind::Changed), 1);
        let changed = &diff.objects[0];
        assert_eq!(changed.id, 1);
        assert_eq!(changed.field_changes.len(), 1);
        assert_eq!(changed.field_changes[0].path, "hp");
    }

    #[test]
    fn diff_saves_reports_added_and_removed_groups() {
        let before = save_with(vec![(G, vec![entry(1, vec![("hp", num(1))])])]);
        let other = "PavonisInteractive.TerraInvicta.TIOther";
        let after = save_with(vec![
            (G, vec![entry(1, vec![("hp", num(1))])]),
            (other, vec![entry(2, vec![("hp", num(1))])]),
        ]);
        let diff = diff_saves(&before, &after, &IgnoreRules::new());
        assert_eq!(diff.groups_added, vec![other.to_string()]);
        assert!(diff.groups_removed.is_empty());
    }

    #[test]
    fn diff_saves_skips_ignored_group_entirely() {
        let before = save_with(vec![(G, vec![entry(1, vec![("hp", num(1))])])]);
        let after = save_with(vec![(G, vec![entry(1, vec![("hp", num(2))])])]);
        let ignore = IgnoreRules::new().ignore_group(G);
        let diff = diff_saves(&before, &after, &ignore);
        assert!(
            diff.is_empty(),
            "ignored group must produce no object diffs"
        );
    }

    #[test]
    fn diff_saves_drops_changed_object_when_only_ignored_paths_differ() {
        let before = save_with(vec![(
            G,
            vec![entry(1, vec![("historyGDP", TiValue::Array(vec![num(1)]))])],
        )]);
        let after = save_with(vec![(
            G,
            vec![entry(
                1,
                vec![("historyGDP", TiValue::Array(vec![num(1), num(2)]))],
            )],
        )]);
        let ignore = IgnoreRules::new().ignore_path_prefix("historyGDP");
        let diff = diff_saves(&before, &after, &ignore);
        assert!(
            diff.is_empty(),
            "object whose only changes are ignored paths must not appear as Changed"
        );
    }

    // --- IgnoreRules path matching ------------------------------------------

    #[test]
    fn ignore_path_prefix_is_segment_aware() {
        let r = IgnoreRules::new().ignore_path_prefix("historyGDP");
        assert!(r.skips_path("historyGDP"));
        assert!(r.skips_path("historyGDP[3]"));
        assert!(r.skips_path("historyGDP.foo"));
        // Must NOT match a different field that merely shares the prefix string.
        assert!(!r.skips_path("historyGDProjection"));
    }

    #[test]
    fn ignore_field_namespace_catches_any_history_field() {
        // The noise preset must suppress EVERY history* time series, including
        // ones not seen during calibration (e.g. historyWarStatus appears only
        // when a nation is at war). A namespace rule catches them all by the
        // first path segment's name prefix.
        let r = IgnoreRules::new().ignore_field_namespace("history");
        assert!(r.skips_path("historyGDP[3]"));
        assert!(r.skips_path("historyWarStatus[19]"));
        assert!(r.skips_path("historyArmyStrength"));
        // Nested under a history array is still history churn.
        assert!(r.skips_path("historyPublicOpinion[2].Resist"));
        // A non-history field is untouched.
        assert!(!r.skips_path("economyScore"));
        assert!(!r.skips_path("publicOpinion.Resist"));
    }

    #[test]
    fn noise_preset_suppresses_uncalibrated_history_war_status() {
        // Regression: historyWarStatus leaked through the noise preset because
        // the preset enumerated specific field names. The namespace rule fixes
        // it. Build an object whose only change is historyWarStatus and assert
        // the preset empties it out.
        let before = save_with(vec![(
            "TINationState",
            vec![entry(
                1,
                vec![("historyWarStatus", TiValue::Array(vec![num(299)]))],
            )],
        )]);
        let after = save_with(vec![(
            "TINationState",
            vec![entry(
                1,
                vec![("historyWarStatus", TiValue::Array(vec![num(299), num(0)]))],
            )],
        )]);
        let full = diff_saves(&before, &after, &IgnoreRules::new());
        assert_eq!(full.count_kind(DiffKind::Changed), 1);
        let filtered = full.filtered(&IgnoreRules::default_noise_preset());
        assert!(
            filtered.is_empty(),
            "noise preset must suppress historyWarStatus churn, got {:?}",
            filtered.objects
        );
    }

    // --- SaveDiff::filtered (live re-filter of a cached full diff) -----------

    #[test]
    fn filtered_drops_objects_in_ignored_group() {
        let before = save_with(vec![(G, vec![entry(1, vec![("hp", num(1))])])]);
        let after = save_with(vec![(G, vec![entry(1, vec![("hp", num(2))])])]);
        // Full diff with no ignore: one Changed object.
        let full = diff_saves(&before, &after, &IgnoreRules::new());
        assert_eq!(full.count_kind(DiffKind::Changed), 1);
        // Re-filter the cached diff by group: now empty.
        let filtered = full.filtered(&IgnoreRules::new().ignore_group(G));
        assert!(filtered.is_empty());
        // Original is untouched (filtering is non-destructive).
        assert_eq!(full.count_kind(DiffKind::Changed), 1);
    }

    #[test]
    fn filtered_drops_field_changes_and_empties_objects() {
        let before = save_with(vec![(
            G,
            vec![entry(
                1,
                vec![
                    ("historyGDP", TiValue::Array(vec![num(1)])),
                    ("money", num(1)),
                ],
            )],
        )]);
        let after = save_with(vec![(
            G,
            vec![entry(
                1,
                vec![
                    ("historyGDP", TiValue::Array(vec![num(1), num(2)])),
                    ("money", num(9)),
                ],
            )],
        )]);
        let full = diff_saves(&before, &after, &IgnoreRules::new());
        // Both money and historyGDP changed.
        assert_eq!(full.objects[0].field_changes.len(), 2);

        // Filter out historyGDP: object survives with only the money change.
        let f1 = full.filtered(&IgnoreRules::new().ignore_path_prefix("historyGDP"));
        assert_eq!(f1.objects.len(), 1);
        assert_eq!(f1.objects[0].field_changes.len(), 1);
        assert_eq!(f1.objects[0].field_changes[0].path, "money");

        // Filter out both paths: object has no surviving changes, so it drops.
        let f2 = full.filtered(
            &IgnoreRules::new()
                .ignore_path_prefix("historyGDP")
                .ignore_path_prefix("money"),
        );
        assert!(f2.is_empty());
    }

    #[test]
    fn filtered_keeps_added_removed_objects_unless_group_ignored() {
        let before = save_with(vec![(G, vec![entry(1, vec![("hp", num(1))])])]);
        let after = save_with(vec![(
            G,
            vec![
                entry(1, vec![("hp", num(1))]),
                entry(2, vec![("hp", num(5))]),
            ],
        )]);
        let full = diff_saves(&before, &after, &IgnoreRules::new());
        assert_eq!(full.count_kind(DiffKind::Added), 1);
        // Added object has no field changes, but must NOT be dropped by a
        // path-only ignore — only a group/id ignore removes it.
        let f1 = full.filtered(&IgnoreRules::new().ignore_path_prefix("hp"));
        assert_eq!(f1.count_kind(DiffKind::Added), 1);
        let f2 = full.filtered(&IgnoreRules::new().ignore_id(2));
        assert_eq!(f2.count_kind(DiffKind::Added), 0);
    }

    // --- SaveDiff::view_filtered (Compare window visible subset) -------------

    #[test]
    fn view_filtered_can_show_only_selected_groups() {
        let other_group = "PavonisInteractive.TerraInvicta.TIOther";
        let before = save_with(vec![
            (G, vec![entry(1, vec![("hp", num(1))])]),
            (other_group, vec![entry(2, vec![("hp", num(1))])]),
        ]);
        let after = save_with(vec![
            (G, vec![entry(1, vec![("hp", num(2))])]),
            (other_group, vec![entry(2, vec![("hp", num(2))])]),
        ]);
        let full = diff_saves(&before, &after, &IgnoreRules::new());
        assert_eq!(full.count_kind(DiffKind::Changed), 2);

        let selected = std::collections::BTreeSet::from([G.to_string()]);
        let view = full.view_filtered(Some(&selected), "");
        assert_eq!(view.objects.len(), 1);
        assert_eq!(view.objects[0].group, G);
    }

    #[test]
    fn view_filtered_empty_group_selection_hides_every_object() {
        let before = save_with(vec![(G, vec![entry(1, vec![("hp", num(1))])])]);
        let after = save_with(vec![(G, vec![entry(1, vec![("hp", num(2))])])]);
        let full = diff_saves(&before, &after, &IgnoreRules::new());
        let selected = std::collections::BTreeSet::new();
        let view = full.view_filtered(Some(&selected), "");
        assert!(view.is_empty());
    }

    #[test]
    fn view_filtered_search_matches_object_metadata_or_field_details() {
        let before = save_with(vec![
            (
                G,
                vec![entry(
                    1,
                    vec![
                        (
                            statics::TI_PROP_DISPLAY_NAME,
                            TiValue::String("Canada".into()),
                        ),
                        ("economyScore", num(12)),
                        ("cohesion", num(5)),
                    ],
                )],
            ),
            (
                "PavonisInteractive.TerraInvicta.TIFaction",
                vec![entry(
                    2,
                    vec![(
                        statics::TI_PROP_DISPLAY_NAME,
                        TiValue::String("Resistance".into()),
                    )],
                )],
            ),
        ]);
        let after = save_with(vec![
            (
                G,
                vec![entry(
                    1,
                    vec![
                        (
                            statics::TI_PROP_DISPLAY_NAME,
                            TiValue::String("Canada".into()),
                        ),
                        ("economyScore", num(13)),
                        ("cohesion", num(6)),
                    ],
                )],
            ),
            (
                "PavonisInteractive.TerraInvicta.TIFaction",
                vec![entry(
                    2,
                    vec![(
                        statics::TI_PROP_DISPLAY_NAME,
                        TiValue::String("Humanity First".into()),
                    )],
                )],
            ),
        ]);
        let full = diff_saves(&before, &after, &IgnoreRules::new());

        // Object metadata match: keep the whole matching object and all its fields.
        let canada = full.view_filtered(None, "canada");
        assert_eq!(canada.objects.len(), 1);
        assert_eq!(canada.objects[0].display_name, "Canada");
        assert_eq!(canada.objects[0].field_changes.len(), 2);

        // Field match: keep only matching fields inside otherwise-matching objects.
        let economy = full.view_filtered(None, "economy");
        assert_eq!(economy.objects.len(), 1);
        assert_eq!(economy.objects[0].field_changes.len(), 1);
        assert_eq!(economy.objects[0].field_changes[0].path, "economyScore");

        // Value match: searching the before or after value should find the faction rename.
        let resistance = full.view_filtered(None, "resistance");
        assert_eq!(resistance.objects.len(), 1);
        assert_eq!(resistance.objects[0].display_name, "Humanity First");
        let humanity = full.view_filtered(None, "humanity");
        assert_eq!(humanity.objects.len(), 1);
        assert_eq!(humanity.objects[0].display_name, "Humanity First");
    }
}
