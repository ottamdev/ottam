use crate::storage::models::{ConversationProject, Project, ProjectSource, WorkspaceProject};
use ottam_protocol::{
    ProjectID, ProjectSourceID, WorkingRootAvailability, WorkingRootKind, WorkingRootSummary,
};
use std::path::Path;

pub(super) enum ResolvedWorkingRoot<'a> {
    WorkspaceFiles {
        canonical_path: &'a Path,
    },
    Project {
        primary_link: Option<&'a ConversationProject>,
        project: Option<&'a Project>,
        source: Option<&'a ProjectSource>,
        membership: Option<&'a WorkspaceProject>,
    },
}

pub(super) fn project_working_root(root: ResolvedWorkingRoot<'_>) -> WorkingRootSummary {
    match root {
        ResolvedWorkingRoot::WorkspaceFiles { canonical_path } => WorkingRootSummary {
            kind: WorkingRootKind::WorkspaceFiles,
            display_name: "Workspace files".to_string(),
            canonical_path: canonical_path.to_string_lossy().into_owned(),
            project_id: None,
            source_id: None,
            availability: WorkingRootAvailability::Available,
        },
        ResolvedWorkingRoot::Project {
            primary_link: None, ..
        } => WorkingRootSummary {
            kind: WorkingRootKind::Project,
            display_name: "Detached project".to_string(),
            canonical_path: String::new(),
            project_id: None,
            source_id: None,
            availability: WorkingRootAvailability::Detached,
        },
        ResolvedWorkingRoot::Project {
            primary_link: Some(link),
            project,
            source,
            membership,
        } => {
            let available = membership.is_some_and(|membership| membership.removed_at.is_none())
                && source.is_some_and(|source| source.archived_at.is_none());
            WorkingRootSummary {
                kind: WorkingRootKind::Project,
                display_name: project
                    .map(|project| project.display_name.clone())
                    .unwrap_or_else(|| "Detached project".to_string()),
                canonical_path: source
                    .map(|source| source.canonical_path.clone())
                    .unwrap_or_default(),
                project_id: Some(ProjectID(link.project_id.to_string())),
                source_id: Some(ProjectSourceID(link.source_id.to_string())),
                availability: if available {
                    WorkingRootAvailability::Available
                } else {
                    WorkingRootAvailability::Detached
                },
            }
        }
    }
}
