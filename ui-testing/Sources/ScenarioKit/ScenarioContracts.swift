import Foundation
import MacAutomationKit

public enum RunClassification: String, Codable, CaseIterable, Sendable {
    case passed
    case failedAssertion = "failed_assertion"
    case fixtureFailed = "fixture_failed"
    case timedOut = "timed_out"
    case environmentUnavailable = "environment_unavailable"
    case appCrashed = "app_crashed"
    case backendFailed = "backend_failed"
    case cleanupFailed = "cleanup_failed"
    case isolationFailed = "isolation_failed"
    case buildFailed = "build_failed"
    case queueTimedOut = "queue_timed_out"
    case evidenceFailed = "evidence_failed"
    case cancelled
}

public struct RunFailure: Codable, Equatable, Sendable {
    public let classification: RunClassification
    public let phase: String
    public let message: String

    public init(
        classification: RunClassification,
        phase: String,
        message: String
    ) {
        self.classification = classification
        self.phase = phase
        self.message = message
    }
}

public struct RunFailureState: Codable, Equatable, Sendable {
    public private(set) var primaryFailure: RunFailure?
    public private(set) var secondaryFailures: [RunFailure]

    public init(
        primaryFailure: RunFailure? = nil,
        secondaryFailures: [RunFailure] = []
    ) {
        self.primaryFailure = primaryFailure
        self.secondaryFailures = secondaryFailures
    }

    public mutating func recordPrimary(_ failure: RunFailure) {
        if primaryFailure == nil {
            primaryFailure = failure
        } else {
            secondaryFailures.append(failure)
        }
    }

    public mutating func recordSecondary(_ failure: RunFailure) {
        secondaryFailures.append(failure)
    }

    public var classification: RunClassification {
        if secondaryFailures.contains(where: { $0.classification == .cleanupFailed }) {
            return .cleanupFailed
        }
        if secondaryFailures.contains(where: { $0.classification == .isolationFailed }) {
            return .isolationFailed
        }
        return primaryFailure?.classification
            ?? secondaryFailures.first?.classification
            ?? .passed
    }
}

public struct SemanticSnapshotPolicy: Codable, Equatable, Sendable {
    public let name: String
    public let maxDepth: Int
    public let maxNodes: Int
    public let maxValueLength: Int
    public let timeoutSeconds: Double
    public let maxDurationMilliseconds: Double
    public let allowsTruncation: Bool

    public init(
        name: String,
        maxDepth: Int,
        maxNodes: Int,
        maxValueLength: Int,
        timeoutSeconds: Double,
        maxDurationMilliseconds: Double,
        allowsTruncation: Bool
    ) {
        self.name = name
        self.maxDepth = maxDepth
        self.maxNodes = maxNodes
        self.maxValueLength = maxValueLength
        self.timeoutSeconds = timeoutSeconds
        self.maxDurationMilliseconds = maxDurationMilliseconds
        self.allowsTruncation = allowsTruncation
    }

    public static let route = SemanticSnapshotPolicy(
        name: "route",
        maxDepth: 12,
        maxNodes: 500,
        maxValueLength: 2_000,
        timeoutSeconds: 5,
        maxDurationMilliseconds: 5_000,
        allowsTruncation: false
    )

    /// A route snapshot taken while the product is deliberately presenting a
    /// continuously animated transition. Hosted macOS AX can need longer than
    /// the normal route budget to traverse that live hierarchy, but the
    /// capture must remain bounded and fail closed.
    public static let animatedRoute = SemanticSnapshotPolicy(
        name: "animated-route",
        maxDepth: 12,
        maxNodes: 500,
        maxValueLength: 2_000,
        timeoutSeconds: 8,
        maxDurationMilliseconds: 8_000,
        allowsTruncation: false
    )

    /// A route snapshot with a sheet over it: the sheet's table is the
    /// evidence, so the tree carries both the page and every row, and a
    /// truncated table would hide exactly the rows the audit is for.
    public static let sheet = SemanticSnapshotPolicy(
        name: "sheet",
        maxDepth: 14,
        maxNodes: 2_000,
        maxValueLength: 2_000,
        timeoutSeconds: 5,
        maxDurationMilliseconds: 5_000,
        allowsTruncation: false
    )

    public static let wholeApplicationDiagnostic = SemanticSnapshotPolicy(
        name: "whole-application-diagnostic",
        maxDepth: 12,
        maxNodes: 500,
        maxValueLength: 2_000,
        timeoutSeconds: 3,
        maxDurationMilliseconds: 3_000,
        allowsTruncation: true
    )

    public func violations(for capture: AXTreeCapture) -> [String] {
        var violations: [String] = []
        if capture.capturedNodes > maxNodes {
            violations.append(
                "captured \(capture.capturedNodes) nodes above \(maxNodes)-node limit"
            )
        }
        if capture.durationMilliseconds > maxDurationMilliseconds {
            violations.append(
                "capture took \(capture.durationMilliseconds) ms above "
                    + "\(maxDurationMilliseconds) ms limit"
            )
        }
        if !allowsTruncation, capture.truncatedNodes > 0 {
            violations.append(
                "capture truncated \(capture.truncatedNodes) nodes under non-truncating policy"
            )
        }
        return violations
    }
}

public enum ScenarioExecutionMode: String, Codable, Equatable, Sendable {
    case backgroundIsolated = "background-isolated"
    case exclusiveForeground = "exclusive-foreground"
}

public struct ScenarioLaunchConfiguration: Equatable, Sendable {
    public let validationProfile: String
    public let executionMode: ScenarioExecutionMode
    public let routes: String
    public let capturesChooser: Bool
    public let capturesSettings: Bool
    public let environment: [String: String]
    public let fixtures: [String]
    public let fixtureEndpoints: [String]
    public let redactionWarnings: [String]
    public let fixtureRequirements: [ScenarioFixtureRequirement]

    public init(
        validationProfile: String,
        executionMode: ScenarioExecutionMode = .exclusiveForeground,
        routes: String,
        capturesChooser: Bool = false,
        capturesSettings: Bool = false,
        environment: [String: String] = [:],
        fixtures: [String] = [],
        fixtureEndpoints: [String] = [],
        redactionWarnings: [String] = [],
        fixtureRequirements: [ScenarioFixtureRequirement] = []
    ) {
        self.validationProfile = validationProfile
        self.executionMode = executionMode
        self.routes = routes
        self.capturesChooser = capturesChooser
        self.capturesSettings = capturesSettings
        self.environment = environment
        self.fixtures = fixtures
        self.fixtureEndpoints = fixtureEndpoints
        self.redactionWarnings = redactionWarnings
        self.fixtureRequirements = fixtureRequirements
    }
}

public enum ScenarioFixtureRequirement: String, Codable, Equatable, Sendable {
    case deterministicProject = "deterministic-project"
    case testACP = "test-acp"
    case testACPSignIn = "test-acp-sign-in"
    case nativeSteering = "native-steering"
    case coreLifecycle = "core-lifecycle"
    case coreFailure = "core-failure"
    case incompatibleStorage = "incompatible-storage"
    case workspaceWindows = "workspace-windows"
    /// A second Git-backed project, added onto the primary Workspace rather
    /// than into a Workspace of its own — the multi-project sidebar shape.
    /// Distinct from `workspaceWindows`, which seeds one project per
    /// Workspace across several windows.
    case additionalProject = "additional-project"
    /// A Claude adapter and a managed Node older than their pins, so launch
    /// starts both upgrades. The Node download fails at once; the adapter's
    /// npm holds until the scenario publishes the `release-npm` ACP marker,
    /// then fails, so Updating and Retry are each captured on purpose.
    case supersededAgentInstalls = "superseded-agent-installs"
    /// A login shell that holds Node resolution open until the scenario
    /// publishes the `release-login-shell` ACP marker, then answers with the
    /// test shim, so a runtime probe outruns the list budget on purpose.
    /// Requires `testACP`.
    case heldLoginShell = "held-login-shell"
}

public enum ACPPromptBehavior: String, Codable, Equatable, Sendable {
    case complete
    case holdUntilCancel
    case holdToolUntilCancel
    case changingTools
    case neverAcknowledge
    case authRequired
    /// Delivers the reply as many small deltas instead of one update, which is
    /// the only fixture shape that drives the per-chunk render path.
    case stream
    /// Stops mid-turn on a tool call and waits for the client's answer, so the
    /// approval card stays on screen for the whole capture.
    case requestPermission
    /// Asks one question through `elicitation/create` and waits for the
    /// answer, which it quotes before ending the turn.
    case askQuestion
    case normalizedTranscript
    case transcriptPayloads
    case transcriptHistory
    case captureLoss
    case sessionRetry
    case sessionLimit
    case sessionFailure
    case usageLimitFailure
    /// Backgrounds a command and ends the turn; on the `follow-up-complete`
    /// marker the task completes and the agent answers on its own.
    case asyncTasksFollowUp
    /// Claude's recorded subagent turn; it holds with a subagent running
    /// until the `subagents-complete` marker.
    case subagentThreads
    /// Claude's recorded native subagent turn: two subagents, one nested, and
    /// a subagent's permission request; it holds until `subagents-complete`.
    case nativeSubagents
    /// Claude's recorded run of one subagent, after a permission request.
    case nativeSubagentFirstRun
    /// Claude's recorded next turn: the parent messages the finished
    /// subagent of `nativeSubagentFirstRun`, which resumes.
    case nativeSubagentResumed
    /// Reports how full the context is, then ends the turn with its token
    /// usage.
    case usage
    /// Reports how full the context is and holds; Stop's cancelled answer
    /// carries the turn's usage and lands after the run is final.
    case usageCancelled
}

public struct ScenarioCaptureRequest: Sendable {
    public let controlName: String
    public let captureID: String
    public let scopeIdentifier: String?
    public let windowGroupFixtureName: String?
    public let snapshotPolicy: SemanticSnapshotPolicy
    public let auditPolicy: AXAuditPolicy

    public init(
        controlName: String,
        captureID: String,
        scopeIdentifier: String?,
        windowGroupFixtureName: String? = nil,
        snapshotPolicy: SemanticSnapshotPolicy,
        auditPolicy: AXAuditPolicy
    ) {
        self.controlName = controlName
        self.captureID = captureID
        self.scopeIdentifier = scopeIdentifier
        self.windowGroupFixtureName = windowGroupFixtureName
        self.snapshotPolicy = snapshotPolicy
        self.auditPolicy = auditPolicy
    }
}

public struct UIElementReference: Equatable, Hashable, Sendable {
    public let identifier: String

    public init(identifier: String) {
        self.identifier = identifier
    }
}

public enum UICondition: Equatable, Sendable {
    case exists(UIElementReference)
    case notExists(UIElementReference)
    case identifierCountEquals(UIElementReference, Int)
    case distinctIdentifierLabelCountEquals(UIElementReference, Int)
    case enabled(UIElementReference, Bool)
    case focused(UIElementReference, Bool)
    case labelContains(UIElementReference, String)
    case valueEquals(UIElementReference, String)
    case valueContains(UIElementReference, String)

    public var element: UIElementReference {
        switch self {
        case let .exists(element),
             let .notExists(element),
             let .identifierCountEquals(element, _),
             let .distinctIdentifierLabelCountEquals(element, _),
             let .enabled(element, _),
             let .focused(element, _),
             let .labelContains(element, _),
             let .valueEquals(element, _),
             let .valueContains(element, _):
            element
        }
    }
}

public enum UISemanticAction: String, Codable, Sendable {
    case press
    case focus
    case setValue
}

public protocol UIScenarioContext: AnyObject {
    func element(id: String) -> UIElementReference
    func waitFor(_ condition: UICondition, timeout: TimeInterval) async throws
    func perform(
        _ action: UISemanticAction,
        on element: UIElementReference,
        value: String?
    ) async throws
    func assert(_ condition: UICondition) async throws
    func recordActivity(_ name: String, fields: [String: String]) throws
    func step(
        _ name: String,
        operation: () async throws -> Void
    ) async throws
    func capture(_ request: ScenarioCaptureRequest) async throws
    func selectListRow(named name: String, waitForText: String) async throws
    func pressControl(named name: String, waitForText: String) async throws
    func armACP(_ behavior: ACPPromptBehavior) throws
    func releaseACPInitialization() throws
    func holdACPInitialization() throws
    /// Arms the next prompt with its reply body too (the adapter's default is
    /// one short line), for captures that need the transcript taller than
    /// the viewport.
    func armACP(_ behavior: ACPPromptBehavior, reply: String) throws
    /// Arms `.stream`: `chunkBytes`-sized deltas paced `chunkDelayMillis`
    /// apart, so a capture (or a profiler) sees the transcript growing the way
    /// a real agent grows it rather than in one jump.
    func armStreamedACP(reply: String, chunkBytes: Int, chunkDelayMillis: Int) throws
    /// Arms `.usage` or `.usageCancelled`, reporting `contextUsed` of 200K
    /// context tokens.
    func armUsageACP(_ behavior: ACPPromptBehavior, contextUsed: Int) throws
    func completeACPSignIn() throws
    /// Releases a fake-agent step that waits on the named control marker.
    func publishACPMarker(_ name: String) throws
    func waitForACPPromptCount(_ expectedCount: Int, timeout: TimeInterval) async throws
    func assertACPPromptCount(_ expectedCount: Int) throws
    func assertFixture(_ name: String, timeout: TimeInterval) async throws
    func setACPExecutableAvailable(_ available: Bool) throws
    func terminateOwnedCore(expectReplacement: Bool) async throws
    func setBundledCoreAvailable(_ available: Bool) throws
    func proveOcclusionIsolation() async throws
    func forceAssertionFailure() async throws
}

public extension UIScenarioContext {
    func press(_ element: UIElementReference) async throws {
        try await perform(.press, on: element, value: nil)
    }

    func focus(_ element: UIElementReference) async throws {
        try await perform(.focus, on: element, value: nil)
    }

    func setValue(_ value: String, on element: UIElementReference) async throws {
        try await perform(.setValue, on: element, value: value)
    }

    func armACP(_ behavior: ACPPromptBehavior) throws {
        throw UIScenarioFailure.fixture(
            "ACP fixture control is unavailable for \(behavior.rawValue)"
        )
    }

    func releaseACPInitialization() throws {
        throw UIScenarioFailure.assertion("ACP initialization control is unavailable")
    }

    func holdACPInitialization() throws {
        throw UIScenarioFailure.fixture("ACP initialization control is unavailable")
    }

    func armACP(_ behavior: ACPPromptBehavior, reply: String) throws {
        throw UIScenarioFailure.fixture(
            "ACP fixture control is unavailable for \(behavior.rawValue)"
        )
    }

    func armStreamedACP(reply: String, chunkBytes: Int, chunkDelayMillis: Int) throws {
        throw UIScenarioFailure.fixture(
            "ACP fixture control is unavailable for \(ACPPromptBehavior.stream.rawValue)"
        )
    }

    func armUsageACP(_ behavior: ACPPromptBehavior, contextUsed: Int) throws {
        throw UIScenarioFailure.fixture(
            "ACP fixture control is unavailable for \(behavior.rawValue)"
        )
    }

    func completeACPSignIn() throws {
        throw UIScenarioFailure.fixture(
            "ACP sign-in fixture control is unavailable"
        )
    }

    func publishACPMarker(_ name: String) throws {
        throw UIScenarioFailure.fixture(
            "ACP fixture control is unavailable for marker \(name)"
        )
    }

    func waitForACPPromptCount(
        _ expectedCount: Int,
        timeout: TimeInterval
    ) async throws {
        throw UIScenarioFailure.fixture(
            "ACP prompt evidence is unavailable for count \(expectedCount)"
        )
    }

    func assertACPPromptCount(_ expectedCount: Int) throws {
        throw UIScenarioFailure.fixture(
            "ACP prompt evidence is unavailable for exact count \(expectedCount)"
        )
    }

    func assertFixture(_ name: String, timeout: TimeInterval) async throws {
        throw UIScenarioFailure.fixture(
            "fixture evidence \(name) is unavailable after \(timeout)s"
        )
    }

    func setACPExecutableAvailable(_ available: Bool) throws {
        throw UIScenarioFailure.fixture(
            "ACP executable control is unavailable for available=\(available)"
        )
    }

    func terminateOwnedCore(expectReplacement: Bool) async throws {
        throw UIScenarioFailure.fixture("Core lifecycle control is unavailable")
    }

    func setBundledCoreAvailable(_ available: Bool) throws {
        throw UIScenarioFailure.fixture(
            "Core executable control is unavailable for available=\(available)"
        )
    }
}

public struct UIScenarioDefinition {
    public let descriptor: ScenarioDescriptor
    public let launchConfiguration: ScenarioLaunchConfiguration
    private let preparation: ((any UIScenarioContext) async throws -> Void)?
    private let body: (any UIScenarioContext) async throws -> Void

    public init(
        descriptor: ScenarioDescriptor,
        launchConfiguration: ScenarioLaunchConfiguration,
        preparation: ((any UIScenarioContext) async throws -> Void)? = nil,
        body: @escaping (any UIScenarioContext) async throws -> Void
    ) {
        self.descriptor = descriptor
        self.launchConfiguration = launchConfiguration
        self.preparation = preparation
        self.body = body
    }

    public func run(in context: any UIScenarioContext) async throws {
        try await preparation?(context)
        try await body(context)
    }
}

public enum UIScenarioFailure: Error, CustomStringConvertible {
    case assertion(String)
    case fixture(String)
    case backend(String)

    public var classification: RunClassification {
        switch self {
        case .assertion:
            .failedAssertion
        case .fixture:
            .fixtureFailed
        case .backend:
            .backendFailed
        }
    }

    public var description: String {
        switch self {
        case let .assertion(message):
            "assertion failed: \(message)"
        case let .fixture(message):
            "fixture failed: \(message)"
        case let .backend(message):
            "backend failed: \(message)"
        }
    }
}
