public enum CoreConnectionPhase: Equatable, Sendable {
    case starting
    case ready
    case recovering(attempt: Int)
    case failed(message: String)
}

public enum CoreRecoveryPresentation: Equatable, Sendable {
    public struct Failure: Equatable, Sendable {
        public let title: String
        public let retryTitle: String
        public let message: String
    }

    case progress(title: String)
    case content
    case failure(Failure)

    public init(phase: CoreConnectionPhase) {
        switch phase {
        case .starting:
            self = .progress(title: "Starting Core…")
        case .ready:
            self = .content
        case .recovering(let attempt):
            self = .progress(title: "Recovering Core (attempt \(attempt))…")
        case .failed(let message):
            self = .failure(
                Failure(
                    title: "Core is unavailable",
                    retryTitle: "Retry Core",
                    message: message
                )
            )
        }
    }
}
