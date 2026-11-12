import OttamBackendClient

/// One row of the command card: a mode to switch to, or an agent command.
enum ChatCommandPickerItem: Equatable, Identifiable {
    case mode(EngineSelectOption, isCurrent: Bool)
    case command(ConversationSessionCommand)

    var id: String {
        switch self {
        case .mode(let mode, _): "mode/\(mode.id)"
        case .command(let command): "command/\(command.name)"
        }
    }
}

extension ConversationSessionCommand {
    /// ACP has no skill marker; Codex names its skills `$name`, the way they
    /// are mentioned in a request. Other agents' skills list as commands.
    var isSkill: Bool { name.hasPrefix("$") }

    /// The name without the character that invokes it.
    var bareName: Substring { isSkill ? name.dropFirst() : name[...] }
}

/// The command card's state while the composer holds `/` or `$` and a
/// query. Pure value logic: the composer owns it, routes keys to it, and
/// acts on picks.
struct ChatCommandPicker: Equatable {
    var modes: [ChatCommandPickerItem] = []
    var commands: [ChatCommandPickerItem] = []
    var skills: [ChatCommandPickerItem] = []
    var highlightedID: String?
    /// Esc closes the card and leaves the text; it stays closed until the
    /// draft stops being a command query.
    var isDismissed = false
    /// The agent has reported its commands for this conversation or engine.
    var hasCommands = false
    /// The draft is a command query.
    private(set) var isQuery = false
    /// ↑/↓ moved the highlight, so filtering keeps it where it is.
    private var isHighlightChosen = false

    var items: [ChatCommandPickerItem] { modes + commands + skills }
    var highlighted: ChatCommandPickerItem? { items.first { $0.id == highlightedID } }

    /// `/` lists everything; `$` lists only skills.
    enum Trigger: Character {
        case slash = "/"
        case dollar = "$"
    }

    /// The trigger and the text after it while the draft is a bare command
    /// being typed: it starts with `/` or `$` and holds no whitespace yet.
    /// Nil otherwise.
    static func query(_ draft: String) -> (trigger: Trigger, text: String)? {
        guard let first = draft.first, let trigger = Trigger(rawValue: first),
              !draft.contains(where: \.isWhitespace)
        else { return nil }
        return (trigger, String(draft.dropFirst()))
    }

    /// Recompute the rows for `draft`, keeping the highlight when its row
    /// survives the filter.
    mutating func update(
        draft: String,
        modes: [EngineSelectOption],
        currentModeID: String?,
        commands: [ConversationSessionCommand]?
    ) {
        let allSkills = (commands ?? []).filter(\.isSkill)
        // `$` on an agent without skills is just text.
        guard let query = Self.query(draft), query.trigger == .slash || !allSkills.isEmpty else {
            self = ChatCommandPicker()
            return
        }
        let needle = query.text.lowercased()
        isQuery = true
        hasCommands = commands != nil
        let onlySkills = query.trigger == .dollar
        self.modes = onlySkills ? [] : Self.ranked(modes, needle: needle) { [$0.displayName, $0.id] } secondary: { _ in nil }
            .map { .mode($0, isCurrent: $0.id == currentModeID) }
        self.commands = onlySkills ? [] : Self.ranked((commands ?? []).filter { !$0.isSkill }, needle: needle) {
            [String($0.bareName)]
        } secondary: { $0.description }
            .map { .command($0) }
        self.skills = Self.ranked(allSkills, needle: needle) { [String($0.bareName)] } secondary: { $0.description }
            .map { .command($0) }
        // The highlight follows the best row until ↑/↓ picks one: a name
        // typed out in full, else a name the query starts, else the top row.
        // Sections come before rank, so the top row can be a weaker match.
        if !isHighlightChosen || highlighted == nil {
            isHighlightChosen = false
            let best = items.first { Self.names($0).contains(needle) }
                ?? items.first { Self.names($0).contains { $0.hasPrefix(needle) } }
                ?? items.first
            highlightedID = best?.id
        }
    }

    private static func names(_ item: ChatCommandPickerItem) -> [String] {
        switch item {
        case .mode(let mode, _): [mode.displayName.lowercased(), mode.id.lowercased()]
        case .command(let command): [command.bareName.lowercased()]
        }
    }

    /// The card shows while the draft is a query, even with nothing to list,
    /// so `/` always says what is there or why nothing is.
    var isOpen: Bool { isQuery && !isDismissed }

    mutating func move(by offset: Int) {
        let ids = items.map(\.id)
        guard !ids.isEmpty else { return }
        let current = highlightedID.flatMap { ids.firstIndex(of: $0) } ?? 0
        highlightedID = ids[(current + offset + ids.count) % ids.count]
        isHighlightChosen = true
    }

    /// Prefix matches on a name first, then names containing the query, then
    /// descriptions; ties keep the agent's order.
    private static func ranked<T>(
        _ values: [T],
        needle: String,
        names: (T) -> [String],
        secondary: (T) -> String?
    ) -> [T] {
        guard !needle.isEmpty else { return values }
        func rank(_ value: T) -> Int? {
            let names = names(value).map { $0.lowercased() }
            if names.contains(where: { $0.hasPrefix(needle) }) { return 0 }
            if names.contains(where: { $0.contains(needle) }) { return 1 }
            if secondary(value)?.lowercased().contains(needle) == true { return 2 }
            return nil
        }
        return values.enumerated()
            .compactMap { offset, value in rank(value).map { (rank: $0, offset: offset, value: value) } }
            .sorted { ($0.rank, $0.offset) < ($1.rank, $1.offset) }
            .map(\.value)
    }
}
