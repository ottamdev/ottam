use crate::storage::error::StorageError;
use crate::storage::models::WorkspaceId;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePaths {
    pub root: PathBuf,
    pub files: PathBuf,
    /// Bytes for Chat image attachments. A sibling of `files`, never inside it,
    /// so an agent listing its working root never sees attachment storage.
    pub attachments: PathBuf,
}

pub fn ensure_workspace_paths(
    ottam_home: &Path,
    workspace_id: &WorkspaceId,
) -> Result<WorkspacePaths, StorageError> {
    let home = ottam_home.canonicalize().map_err(|_| {
        StorageError::new(
            "workspaceFilesUnavailable",
            "failed to resolve OTTAM_HOME for Workspace files",
        )
        .recoverable(true)
    })?;
    let workspaces = ensure_plain_directory(&home.join("workspaces"))?;
    let root = ensure_plain_directory(&workspaces.join(workspace_id.to_string()))?;
    let files = ensure_plain_directory(&root.join("files"))?;
    let attachments = ensure_plain_directory(&root.join("attachments"))?;
    if !root.starts_with(&workspaces)
        || !files.starts_with(&root)
        || !attachments.starts_with(&root)
    {
        return Err(StorageError::new(
            "workspaceFilesUnsafe",
            "managed Workspace paths escape OTTAM_HOME",
        )
        .recoverable(true));
    }
    Ok(WorkspacePaths {
        root,
        files,
        attachments,
    })
}

fn ensure_plain_directory(path: &Path) -> Result<PathBuf, StorageError> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => {
            return Err(StorageError::new(
                "workspaceFilesUnavailable",
                "failed to create a managed Workspace directory",
            )
            .recoverable(true));
        }
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(StorageError::new(
                "workspaceFilesUnsafe",
                "managed Workspace paths must not be symbolic links",
            )
            .recoverable(true));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(StorageError::new(
                "workspaceFilesUnavailable",
                "a managed Workspace path is not a directory",
            )
            .recoverable(true));
        }
        Ok(_) => {}
        Err(_) => {
            return Err(StorageError::new(
                "workspaceFilesUnavailable",
                "failed to inspect a managed Workspace directory",
            )
            .recoverable(true));
        }
    }
    path.canonicalize().map_err(|_| {
        StorageError::new(
            "workspaceFilesUnavailable",
            "failed to resolve a managed Workspace directory",
        )
        .recoverable(true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::{Arc, Barrier};
    use tempfile::TempDir;

    #[test]
    fn provisions_one_stable_workspace_files_directory() {
        let home = TempDir::new().unwrap();
        let workspace_id = WorkspaceId::try_from("019b0000-0000-7000-8000-000000000001").unwrap();
        let first = ensure_workspace_paths(home.path(), &workspace_id).unwrap();
        let second = ensure_workspace_paths(home.path(), &workspace_id).unwrap();
        assert_eq!(first, second);
        assert!(first
            .files
            .ends_with("workspaces/019b0000-0000-7000-8000-000000000001/files"));
        assert!(first.files.is_dir());
        assert!(first
            .attachments
            .ends_with("workspaces/019b0000-0000-7000-8000-000000000001/attachments"));
        assert!(first.attachments.is_dir());
        assert!(!first.attachments.starts_with(&first.files));
        assert!(!first.root.join("terminals").exists());
    }

    #[test]
    fn provisions_workspace_paths_concurrently() {
        let home = Arc::new(TempDir::new().unwrap());
        let barrier = Arc::new(Barrier::new(16));
        let handles = (0..16)
            .map(|_| {
                let home = Arc::clone(&home);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let workspace_id =
                        WorkspaceId::try_from("019b0000-0000-7000-8000-000000000001").unwrap();
                    barrier.wait();
                    ensure_workspace_paths(home.path(), &workspace_id)
                })
            })
            .collect::<Vec<_>>();

        let paths = handles
            .into_iter()
            .map(|handle| handle.join().unwrap().unwrap())
            .collect::<Vec<_>>();
        assert!(paths.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn rejects_symlinked_workspace_directory() {
        let home = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        fs::create_dir(home.path().join("workspaces")).unwrap();
        symlink(
            outside.path(),
            home.path()
                .join("workspaces/019b0000-0000-7000-8000-000000000001"),
        )
        .unwrap();
        let workspace_id = WorkspaceId::try_from("019b0000-0000-7000-8000-000000000001").unwrap();
        let error = ensure_workspace_paths(home.path(), &workspace_id).unwrap_err();
        assert_eq!(error.kind, "workspaceFilesUnsafe");
    }
}
