use crate::storage::error::StorageError;
use crate::storage::models::WorkingRootKind;
use sha2::{Digest, Sha256};

/// One working root participating in an agent run's source scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRoot<'a> {
    pub kind: WorkingRootKind,
    pub source_id: Option<&'a str>,
    pub canonical_path: &'a str,
}

/// Normative source-scope fingerprint from the persistence redesign spec:
/// one immutable descriptor per root, bytewise-sorted, duplicates rejected,
/// joined with a single `\n`, lowercase hex SHA-256 of the UTF-8 bytes.
pub fn source_scope_fingerprint(roots: &[ScopeRoot<'_>]) -> Result<String, StorageError> {
    let mut descriptors = roots
        .iter()
        .map(|root| match (root.kind, root.source_id) {
            (WorkingRootKind::WorkspaceFiles, _) => {
                Ok(format!("workspaceFiles\0{}", root.canonical_path))
            }
            (WorkingRootKind::Project, Some(source_id)) => {
                Ok(format!("project\0{}\0{}", source_id, root.canonical_path))
            }
            (WorkingRootKind::Project, None) => Err(StorageError::new(
                "storageConstraint",
                "a Project scope root requires a ProjectSource",
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    descriptors.sort();
    if descriptors.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(StorageError::new(
            "storageConstraint",
            "scope roots contain duplicate descriptors",
        ));
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(descriptors.join("\n").as_bytes())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_files_root_has_pinned_digest() {
        let fingerprint = source_scope_fingerprint(&[ScopeRoot {
            kind: WorkingRootKind::WorkspaceFiles,
            source_id: None,
            canonical_path: "/tmp/workspace",
        }])
        .expect("fingerprint");
        // SHA-256 of "workspaceFiles\0/tmp/workspace".
        assert_eq!(
            fingerprint,
            "c3140d13b064c644aad268b97c03c362a1a19d4591d115f4c51453016bc54b18"
        );
    }

    #[test]
    fn multi_root_fingerprint_is_order_independent_with_pinned_digest() {
        let project = ScopeRoot {
            kind: WorkingRootKind::Project,
            source_id: Some("source-1"),
            canonical_path: "/tmp/project",
        };
        let workspace = ScopeRoot {
            kind: WorkingRootKind::WorkspaceFiles,
            source_id: None,
            canonical_path: "/tmp/workspace",
        };
        let forward =
            source_scope_fingerprint(&[project.clone(), workspace.clone()]).expect("fingerprint");
        let reversed = source_scope_fingerprint(&[workspace, project]).expect("fingerprint");
        assert_eq!(forward, reversed);
        // SHA-256 of "project\0source-1\0/tmp/project\nworkspaceFiles\0/tmp/workspace".
        assert_eq!(
            forward,
            "1c2c7a0f0371ad28f8cd74a7c3411853d97023e6df41b43bfca7e22f42ed88c5"
        );
    }

    #[test]
    fn duplicate_roots_are_rejected() {
        let root = ScopeRoot {
            kind: WorkingRootKind::WorkspaceFiles,
            source_id: None,
            canonical_path: "/tmp/workspace",
        };
        let error = source_scope_fingerprint(&[root.clone(), root]).expect_err("duplicate");
        assert_eq!(error.kind, "storageConstraint");
    }

    #[test]
    fn project_root_without_source_is_rejected() {
        let error = source_scope_fingerprint(&[ScopeRoot {
            kind: WorkingRootKind::Project,
            source_id: None,
            canonical_path: "/tmp/project",
        }])
        .expect_err("missing source");
        assert_eq!(error.kind, "storageConstraint");
    }
}
