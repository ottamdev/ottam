import OttamBackendClient

/// Unified selection for the merged sidebar: the New Chat row and individual
/// conversations share one `List` selection. `.newChat` is the composer-first
/// draft.
///
/// The value is derived from `ChatWorkspaceModel.selectedConversationID` rather
/// than stored — the sidebar's binding maps writes back onto it. Future section
/// kinds (e.g. specs) extend this enum additively.
enum SidebarSelection: Hashable {
    case newChat
    case conversation(ConversationID)
}
