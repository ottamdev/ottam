import OttamBackendClient
import Foundation

/// One conversation's rendering, kept while its conversation is cached.
/// Block identities come from the projector, so its heights, text artifacts
/// and expanded code blocks only hold for this conversation's projector:
/// they travel together.
@MainActor
final class TranscriptViewSession {
    /// Artifacts kept once out of view: about a screen's worth, the most
    /// recently drawn. The rest, and the images they hold, rebuild on demand.
    static let retainedArtifactCount = 32

    let projectionStore: TranscriptProjectionStore
    let textArtifactCache: TranscriptTextArtifactCache
    var heightCache: [TranscriptBlockID: MacAgentTranscriptViewController.HeightCacheEntry]
    var expandedCodeBlocks: Set<TranscriptBlockID>

    init(
        projectionStore: TranscriptProjectionStore? = nil,
        textArtifactCache: TranscriptTextArtifactCache? = nil,
        heightCache: [TranscriptBlockID: MacAgentTranscriptViewController.HeightCacheEntry] = [:],
        expandedCodeBlocks: Set<TranscriptBlockID> = []
    ) {
        self.projectionStore = projectionStore ?? TranscriptProjectionStore()
        self.textArtifactCache = textArtifactCache ?? TranscriptTextArtifactCache()
        self.heightCache = heightCache
        self.expandedCodeBlocks = expandedCodeBlocks
    }
}

/// Where the reader was, by content rather than block identity, so it
/// resolves in a rebuilt projection too.
enum TranscriptReadingPlace: Equatable {
    case bottom
    /// The block `ordinal` places into item `itemID`, `relativeY` below the
    /// top of the viewport.
    case block(itemID: String, ordinal: Int, relativeY: CGFloat)
}

/// Keeps the transcript's per-conversation state across conversation
/// switches and remounts.
@MainActor
protocol TranscriptViewSessionKeeping: AnyObject {
    func hasViewSession(for conversationID: ConversationID) -> Bool
    /// The conversation's kept rendering, if its conversation is cached. The
    /// keeper no longer holds it.
    func takeViewSession(for conversationID: ConversationID) -> TranscriptViewSession?
    /// Keeps a rendering and place the view is leaving. A nil place keeps
    /// the last one recorded.
    func keep(_ session: TranscriptViewSession, place: TranscriptReadingPlace?, for conversationID: ConversationID)
    func readingPlace(for conversationID: ConversationID) -> TranscriptReadingPlace?
    func uiState(for conversationID: ConversationID) -> ChatConversationUIState
}
