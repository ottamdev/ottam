import OttamBackendClient

/// The composer's read on the whole engine list: can anything take a message,
/// and if not, is there one shared reason worth naming?
public enum EngineSetupState: Equatable, Sendable {
    /// At least one engine can take a message; the composer behaves normally.
    case usable
    /// Engines exist but none is usable. `dominant` is the runtime problem
    /// every engine shares, if they agree — specific copy beats generic — and
    /// nil when they disagree or the blockers are not runtime problems at all.
    case noneUsable(dominant: EngineRuntimeProblem?)
    /// No engines in the list. Built-in definitions make this near-impossible
    /// today, but the case keeps the composer honest if that ever changes.
    case noEngines
}

public func engineSetupState(engines: [EngineSummary]) -> EngineSetupState {
    guard !engines.isEmpty else { return .noEngines }
    guard !engines.contains(where: \.isUsable) else { return .usable }
    let problems = engines.map(EngineRuntimeProblem.init(engine:))
    guard let first = problems.first ?? nil,
        problems.allSatisfy({ $0 == first })
    else {
        return .noneUsable(dominant: nil)
    }
    return .noneUsable(dominant: first)
}

/// Whether the composer should replace Send with an agent-setup action.
/// A failed or unfinished initial load has not established that setup is the
/// blocker, and an active run remains steerable even if no engine is usable.
public func shouldOfferAgentSetup(
    didCompleteInitialLoad: Bool,
    hasLiveRun: Bool,
    engines: [EngineSummary]
) -> Bool {
    guard didCompleteInitialLoad, !hasLiveRun else { return false }
    if case .usable = engineSetupState(engines: engines) { return false }
    return true
}
