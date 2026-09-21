"""PV-2.3-M-001: narrow hosted qualification fixes, asserted before writes."""
from pathlib import Path

changes = {}
def replace(path, old, new):
    value = changes.get(path, Path(path).read_text())
    if new in value:
        return
    if value.count(old) != 1:
        raise SystemExit(f'Unexpected source anchor in {path}: {old[:100]}')
    changes[path] = value.replace(old, new, 1)

replace('tests/persistent_retrieval.rs', 'error.contains("offset"),', 'error.contains("offset") || (error.contains("line ") && error.contains("column ")),')
replace('tests/persistent_retrieval_corruption.rs', '#![cfg(all(feature = "full-text", feature = "vector-search")))]', '#![cfg(all(feature = "full-text", feature = "vector-search"))]')
replace('tests/persistent_retrieval_corruption.rs', 'root.join("pv_manifest.json")', 'root.join(picovolt::MANIFEST_FILE)')

replace('src/db.rs', '    commit_sequence: Option<u64>,\n    page_count:', '    commit_sequence: Option<u64>,\n    /// Version 8 separates format capability from workspace logging state.\n    #[serde(default, skip_serializing_if = "Option::is_none")]\n    logged_workspace: Option<bool>,\n    page_count:')
replace('src/db.rs', '    persistent::validate_manifest(m)?;', '''    persistent::validate_manifest(m)?;
    if m.format_version >= crate::FORMAT_VERSION_RETRIEVAL
        && m.logged_workspace != Some(m.commit_sequence.is_some()) {
        return Err(PvError::Corruption("format-8 logging marker/sequence anchor mismatch".into()));
    }''')
replace('src/db.rs', '            retrieval_indexes: self.retrieval_descriptors()?,', '''            retrieval_indexes: self.retrieval_descriptors()?,
            logged_workspace: (format_version >= crate::FORMAT_VERSION_RETRIEVAL)
                .then_some(matches!(plan, IndexPlan::Definitions)),''')
replace('src/journal.rs', '    commit_sequence: Option<u64>,\n    clock:', '    commit_sequence: Option<u64>,\n    #[serde(default)]\n    logged_workspace: Option<bool>,\n    clock:')
replace('src/journal.rs', '    let surviving_head = commits.last().copied().unwrap_or(0).max(floor);', '''    let surviving_head = commits.last().copied().unwrap_or(0).max(floor);
    if let Some(meta) = manifest.as_ref().filter(|m| m.format_version >= crate::FORMAT_VERSION_RETRIEVAL) {
        if meta.logged_workspace != Some(meta.commit_sequence.is_some())
            || (meta.logged_workspace == Some(false) && surviving_head != 0) {
            return Err(PvError::Corruption("format-8 logging marker/sequence anchor mismatch".into()));
        }
    }''')
replace('src/journal.rs', '.is_some_and(|m| m.format_version >= crate::FORMAT_VERSION_COMMIT_LOG)', '''.is_some_and(|m| m.format_version >= crate::FORMAT_VERSION_COMMIT_LOG
                        && !(m.format_version >= crate::FORMAT_VERSION_RETRIEVAL && m.logged_workspace == Some(false)))''')

for path, content in changes.items():
    Path(path).write_text(content)
    print('PATCHED', path)
