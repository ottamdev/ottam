import OttamBackendClient

/// What the app can see when it reports focus to core.
public struct FocusInputs: Equatable, Sendable {
    /// Ottam is the active app.
    public var appIsActive: Bool
    /// The selected conversation of the main window, when that window is a
    /// workspace window; nil for any other main window, or none selected.
    public var mainWorkspaceSelection: ConversationID?
    /// The main window is on screen: not occluded and not minimized.
    public var mainWindowIsVisible: Bool
    public var screensAreAwake: Bool
    /// The login session is the console's, not switched away from.
    public var sessionIsActive: Bool
    public var screenIsUnlocked: Bool

    public init(
        appIsActive: Bool,
        mainWorkspaceSelection: ConversationID?,
        mainWindowIsVisible: Bool,
        screensAreAwake: Bool,
        sessionIsActive: Bool,
        screenIsUnlocked: Bool
    ) {
        self.appIsActive = appIsActive
        self.mainWorkspaceSelection = mainWorkspaceSelection
        self.mainWindowIsVisible = mainWindowIsVisible
        self.screensAreAwake = screensAreAwake
        self.sessionIsActive = sessionIsActive
        self.screenIsUnlocked = screenIsUnlocked
    }
}

/// The one conversation the user can be looking at, which core also takes as
/// seen (`docs/architecture/app/macos-ui.md` § Attention).
public enum FocusResolver {
    public static func focus(_ inputs: FocusInputs) -> ConversationID? {
        guard inputs.appIsActive, inputs.mainWindowIsVisible, inputs.screensAreAwake,
              inputs.sessionIsActive, inputs.screenIsUnlocked
        else { return nil }
        return inputs.mainWorkspaceSelection
    }
}

/// What presenting attention takes, beyond the entries themselves.
public enum AttentionEffect: Equatable, Sendable {
    /// Show a banner for the entry, replacing that conversation's banner of
    /// the same state.
    case post(AttentionEntry)
    /// Remove every banner the conversation has.
    case withdraw(ConversationID)
    /// Remove delivered banners that no longer match these states, such as
    /// ones an earlier launch left.
    case reconcile([ConversationID: AttentionState])
    /// Conversations with any attention.
    case setBadge(Int)
}

/// Core's attention as the app holds it, built from `attention.list`
/// snapshots and `attention.changed` notifications that can land in either
/// order.
///
/// Core bumps one revision for every change, so a snapshot at N already holds
/// every change at or below N. Each conversation keeps the revision it was
/// last set at: a snapshot replaces only conversations stamped at or below
/// it, and any change at or below the newest snapshot, or its conversation's
/// stamp, is stale.
public struct AttentionReducer: Sendable {
    public private(set) var entries: [ConversationID: AttentionEntry] = [:]
    private var stamps: [ConversationID: UInt64] = [:]
    private var floor: UInt64?
    private var coreInstanceID: CoreInstanceID?

    public init() {}

    /// Conversations in `state` within one workspace.
    public func conversations(
        in workspaceID: WorkspaceID,
        state: AttentionState
    ) -> Set<ConversationID> {
        Set(entries.values.lazy
            .filter { $0.workspaceID == workspaceID && $0.state == state }
            .map(\.conversationID))
    }

    /// A new core numbers its revisions from zero again, so nothing the old
    /// one stamped may outrank it. Entries stay until its snapshot replaces
    /// them, so the badge does not flicker. Returns whether it reset.
    @discardableResult
    public mutating func connect(to instance: CoreInstanceID) -> Bool {
        guard instance != coreInstanceID else { return false }
        coreInstanceID = instance
        stamps.removeAll()
        floor = nil
        return true
    }

    public mutating func apply(_ change: AttentionChangedParams) -> [AttentionEffect] {
        let id = change.entry.conversationID
        if let floor, change.revision <= floor { return [] }
        if let stamp = stamps[id], change.revision <= stamp { return [] }
        stamps[id] = change.revision
        let count = entries.count
        let previous = entries[id]
        var effects: [AttentionEffect] = []
        if change.entry.state == .none {
            entries.removeValue(forKey: id)
            effects.append(.withdraw(id))
        } else {
            entries[id] = change.entry
            if let previous, previous.state != change.entry.state {
                effects.append(.withdraw(id))
            }
            if change.notify {
                effects.append(.post(change.entry))
            }
        }
        if entries.count != count {
            effects.append(.setBadge(entries.count))
        }
        return effects
    }

    public mutating func apply(snapshot: AttentionListResult) -> [AttentionEffect] {
        if let floor, snapshot.revision < floor { return [] }
        floor = snapshot.revision
        var listed: [ConversationID: AttentionEntry] = [:]
        for entry in snapshot.entries where entry.state != .none {
            listed[entry.conversationID] = entry
        }
        var effects: [AttentionEffect] = []
        for id in Set(entries.keys).union(listed.keys).sorted(by: { $0.rawValue < $1.rawValue }) {
            if let stamp = stamps[id], stamp > snapshot.revision { continue }
            stamps[id] = snapshot.revision
            let previous = entries[id]
            entries[id] = listed[id]
            if let previous, previous.state != listed[id]?.state {
                effects.append(.withdraw(id))
            }
        }
        effects.append(.reconcile(entries.mapValues(\.state)))
        effects.append(.setBadge(entries.count))
        return effects
    }
}
