import MacAutomationKit
import ScenarioKit
import OttamAccessibility
import OttamUIScenarios
import XCTest

final class ScenarioContractsTests: XCTestCase {
    func testIdentityAcceptsCompleteUnicodeDisplayNames() throws {
        for name in ["Ottam Test · 1234567890👩‍💻…", "Ottam Test · e\u{301}e\u{301}"] {
            let identity = try IsolationIdentity(
                worktreeURL: URL(fileURLWithPath: "/tmp/naming-validation"),
                displayName: name
            )
            XCTAssertEqual(identity.displayName, name)
        }
    }

    func testIdentityRejectsMalformedDisplayNames() throws {
        for name in [
            "", "Ottam Dev", "Ottam Testing", "Ottam Test · ",
            "diagnostic\nOttam Test", "Ottam Test\ndiagnostic",
            "Ottam Test · before\rafter", "Ottam Test · before\u{2028}after",
            "Ottam Test · before\u{7}after",
        ] {
            XCTAssertThrowsError(try IsolationIdentity(
                worktreeURL: URL(fileURLWithPath: "/tmp/naming-validation"),
                displayName: name
            )) { error in
                guard case ScenarioKitError.invalidDisplayName = error else {
                    return XCTFail("Unexpected error: \(error)")
                }
            }
        }
    }

    func testMergedShellDiagnosticCannotBecomeDisplayName() throws {
        let output = try Command.output([
            "/bin/bash", "-c",
            "printf 'shell startup diagnostic\\n' >&2; printf 'Ottam Test · textkit2\\n'",
        ], timeout: 5).trimmingCharacters(in: .whitespacesAndNewlines)
        XCTAssertTrue(output.contains("shell startup diagnostic"))
        XCTAssertTrue(output.contains("Ottam Test · textkit2"))
        XCTAssertThrowsError(try IsolationIdentity(
            worktreeURL: URL(fileURLWithPath: "/tmp/naming-validation"),
            displayName: output
        )) { error in
            guard case ScenarioKitError.invalidDisplayName = error else {
                return XCTFail("Unexpected error: \(error)")
            }
        }
    }

    func testDisplayNameDoesNotChangeIsolationIdentity() throws {
        let worktree = URL(fileURLWithPath: "/tmp/ottam-display-identity")
        let runID = "r00000000000000000000000000000000"
        let first = try IsolationIdentity(worktreeURL: worktree, runID: runID)
        let renamed = try IsolationIdentity(
            worktreeURL: worktree, runID: runID, displayName: "Ottam Test · textkit2"
        )
        XCTAssertEqual(first.worktreeID, renamed.worktreeID)
        XCTAssertEqual(first.bundleIdentifier, renamed.bundleIdentifier)
        XCTAssertEqual(first.applicationName, renamed.applicationName)
        XCTAssertEqual(first.executableName, renamed.executableName)
        XCTAssertEqual(renamed.displayName, "Ottam Test · textkit2")

        let nextRun = try IsolationIdentity(
            worktreeURL: worktree,
            runID: "r00000000000000000000000000000001",
            displayName: renamed.displayName
        )
        XCTAssertEqual(renamed.displayName, nextRun.displayName)
        XCTAssertNotEqual(renamed.bundleIdentifier, nextRun.bundleIdentifier)
        XCTAssertNotEqual(renamed.applicationName, nextRun.applicationName)
        XCTAssertNotEqual(renamed.executableName, nextRun.executableName)
    }

    func testStagingWritesDisplayNameAndPreservesUniquePaths() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("Source.app")
        let macOS = source.appendingPathComponent("Contents/MacOS")
        try FileManager.default.createDirectory(at: macOS, withIntermediateDirectories: true)
        try FileManager.default.copyItem(
            at: URL(fileURLWithPath: "/usr/bin/true"),
            to: macOS.appendingPathComponent("Ottam")
        )
        let sourceInfo: [String: Any] = [
            "CFBundleExecutable": "Ottam",
            "CFBundleIdentifier": "dev.ottam.Ottam",
            "CFBundleName": "Ottam",
            "CFBundlePackageType": "APPL",
        ]
        let info = try PropertyListSerialization.data(
            fromPropertyList: sourceInfo, format: .xml, options: 0
        )
        try info.write(to: source.appendingPathComponent("Contents/Info.plist"))
        let identity = try IsolationIdentity(
            worktreeURL: root, displayName: "Ottam Test · ééééééééééé…"
        )
        let paths = try RunPaths(worktreeURL: root, identity: identity)
        let staged = try StagedApplication.stage(
            sourceBundleURL: source, identity: identity, paths: paths
        )
        let stagedInfo = try XCTUnwrap(
            PropertyListSerialization.propertyList(
                from: Data(contentsOf: staged.bundleURL.appendingPathComponent("Contents/Info.plist")),
                options: [], format: nil
            ) as? [String: Any]
        )
        XCTAssertEqual(stagedInfo["CFBundleName"] as? String, identity.displayName)
        XCTAssertEqual(stagedInfo["CFBundleDisplayName"] as? String, identity.displayName)
        XCTAssertEqual(stagedInfo["CFBundleIdentifier"] as? String, identity.bundleIdentifier)
        XCTAssertEqual(stagedInfo["CFBundleExecutable"] as? String, identity.executableName)
        XCTAssertEqual(staged.bundleURL.lastPathComponent, identity.applicationName + ".app")
        XCTAssertEqual(staged.executableURL.lastPathComponent, identity.executableName)
        XCTAssertEqual(try Data(contentsOf: source.appendingPathComponent("Contents/Info.plist")), info)
        try staged.removeAfterVerifiedCleanup(paths: paths)
        XCTAssertFalse(FileManager.default.fileExists(atPath: staged.bundleURL.path))
    }

    func testRunClassificationVocabularyIsStable() {
        XCTAssertEqual(
            RunClassification.allCases.map(\.rawValue),
            [
                "passed",
                "failed_assertion",
                "fixture_failed",
                "timed_out",
                "environment_unavailable",
                "app_crashed",
                "backend_failed",
                "cleanup_failed",
                "isolation_failed",
                "build_failed",
                "queue_timed_out",
                "evidence_failed",
                "cancelled",
            ]
        )
    }

    func testTypedScenarioFailuresMapToSharedClassifications() {
        XCTAssertEqual(
            UIScenarioFailure.assertion("missing").classification,
            .failedAssertion
        )
        XCTAssertEqual(
            UIScenarioFailure.fixture("invalid").classification,
            .fixtureFailed
        )
        XCTAssertEqual(
            UIScenarioFailure.backend("exited").classification,
            .backendFailed
        )
    }

    func testRegistryHasUniqueDescriptorsAndExecutableDefinitions() async throws {
        let identifiers = ScenarioRegistry.scenarios.map(\.descriptor.id)
        XCTAssertEqual(Set(identifiers).count, identifiers.count)
        XCTAssertEqual(
            identifiers,
            [
                "workspace.core-replacement",
                "core.recovery-presentation",
                "core.chooser-recovery",
                "core.chooser-startup-error",
                "chat.interrupt-and-continue",
                "chat.native-steer-and-stop",
                "chat.queue-drain",
                "chat.tools-pane",
                "chat.delivery-retry",
                "chat.run-failure",
                "chat.transcript-history-recovery",
                "chat.switch-keeps-place",
                "chat.delivery-mark-delivered",
                "chat.pending-retry-resume",
                "chat.auth-recovery",
                "chat.permission-prompt",
                "chat.question-panel",
                "chat.no-usable-engine",
                "chat.engine-checking",
                "settings.agent-compatibility",
                "settings.agent-update",
                "settings.existing-setups",
                "settings.profiles",
                "settings.profile-delete-probe",
                "chat.transcript-live-light",
                "chat.transcript-live-dark",
                "chat.transcript-complete",
                "chat.transcript-streaming",
                "chat.transcript-normalized",
                "chat.transcript-payloads",
                "chat.session-signals",
                "chat.background-tasks",
                "chat.usage",
                "chat.subagent-threads",
                "chat.native-subagent-threads",
                "chat.transcript-capture-recovery",
                "chat.transcript-appkit-native",
                "chat.code-blocks",
                "chat.tables",
                "transcript.measure-multi-turn",
                "transcript.measure-single-reply",
                "transcript.measure-detached",
                "transcript.measure-idle-tool",
                "chat.measure-switch-cache-memory",
                "transcript.updating-tools",
                "capture.occlusion-proof",
                "driver.readiness-delay",
                "driver.forced-failure",
                "workspace.zero-project-shape-light",
                "workspace.zero-project-shape-dark",
                "workspace.single-project-shape-light",
                "workspace.single-project-shape-dark",
                "workspace.multi-project-shape-light",
                "workspace.multi-project-shape-dark",
                "workspace.two-window-identity-capture",
                "markdown-lab.kitchen-sink-light",
                "markdown-lab.kitchen-sink-dark",
                "markdown-lab.code-heavy-light",
                "markdown-lab.code-heavy-dark",
                "markdown-lab.table-heavy-light",
                "markdown-lab.table-heavy-dark",
                "markdown-lab.lists-and-quotes-light",
                "markdown-lab.lists-and-quotes-dark",
                "markdown-lab.images-light",
                "markdown-lab.images-dark",
                "markdown-lab.user-inline-light",
                "markdown-lab.user-inline-dark",
                "markdown-lab.cards",
            ]
        )
        XCTAssertEqual(
            ScenarioRegistry.smoke.launchConfiguration.executionMode,
            .backgroundIsolated
        )
        XCTAssertEqual(
            ScenarioRegistry.smoke.launchConfiguration.environment[
                "OTTAM_UI_TEST_APPEARANCE"
            ],
            "light"
        )
        XCTAssertEqual(
            ScenarioRegistry.smoke.launchConfiguration.fixtureRequirements,
            [.deterministicProject]
        )

        let context = RecordingScenarioContext()
        let fixture = UIScenarioDefinition(
            descriptor: ScenarioDescriptor(
                id: "fixture.net-new-surface",
                description: "Prove a new scenario needs no launcher plumbing.",
                tags: ["fixture"],
                requirements: []
            ),
            launchConfiguration: ScenarioLaunchConfiguration(
                validationProfile: "focused",
                routes: "chat",
                fixtures: ["fixture.net-new-surface"]
            ),
            preparation: { context in
                try context.recordActivity(
                    "fixture_prepared",
                    fields: ["fixture": "fixture.net-new-surface"]
                )
            }
        ) { context in
            let composer = context.element(id: "chat.composer")
            try await context.waitFor(.exists(composer), timeout: 1)
            try await context.focus(composer)
            try await context.setValue("hello", on: composer)
            try await context.assert(.valueEquals(composer, "hello"))
            try await context.capture(
                ScenarioCaptureRequest(
                    controlName: "route-chat",
                    captureID: "fixture",
                    scopeIdentifier: "workspace.detail",
                    snapshotPolicy: .route,
                    auditPolicy: .diagnostic
                )
            )
        }

        let fixtureRegistry = UIScenarioRegistry([fixture])
        let registeredFixture = try XCTUnwrap(
            fixtureRegistry.definition(id: "fixture.net-new-surface")
        )

        try await registeredFixture.run(in: context)
        XCTAssertEqual(context.captureIDs, ["fixture"])
        XCTAssertEqual(context.activities, ["fixture_prepared"])
        XCTAssertEqual(
            context.actions,
            [
                "focus:chat.composer",
                "setValue:chat.composer=hello",
            ]
        )
    }

    func testSmokeIsTheCompleteSingleLifetimePageInventory() async throws {
        let context = RecordingScenarioContext()

        try await UIScenarioDefinition.smoke.run(in: context)

        XCTAssertEqual(
            context.captureIDs,
            [
                "workspace-chooser",
                "route-chat",
                "settings-app",
                "settings-agents",
                "settings-agent-claude",
                "settings-mcp-servers",
                "settings-terminal",
                "settings-storage",
            ]
        )
        XCTAssertEqual(
            context.activities,
            [
                "select-list-row:Agents->Check Again",
                "press-control:Claude Code->Instances",
                "press-control:Ottam profile->Adapter",
                "select-list-row:MCP Servers->Add Server…",
                "select-list-row:Terminal->Ghostty Overlay",
                "select-list-row:Storage->Tool Result Cache",
            ]
        )
        let requiredByCapture = Dictionary(
            uniqueKeysWithValues: context.captureRequests.map {
                ($0.captureID, Set($0.auditPolicy.requiredIdentifiers))
            }
        )
        XCTAssertEqual(
            requiredByCapture["settings-app"],
            [AccessibilityID.settingsWindow]
        )
        XCTAssertEqual(
            requiredByCapture["settings-agents"],
            [AccessibilityID.settingsWindow]
        )
        XCTAssertEqual(
            requiredByCapture["settings-agent-claude"],
            [AccessibilityID.settingsWindow]
        )
        XCTAssertEqual(
            requiredByCapture["settings-terminal"],
            [AccessibilityID.settingsWindow]
        )
        XCTAssertEqual(requiredByCapture["settings-mcp-servers"], [AccessibilityID.settingsWindow])
        XCTAssertEqual(requiredByCapture["settings-storage"], [AccessibilityID.settingsWindow])
        let requiredTextByCapture = Dictionary(
            uniqueKeysWithValues: context.captureRequests.map {
                ($0.captureID, Set($0.auditPolicy.requiredTexts))
            }
        )
        XCTAssertEqual(requiredTextByCapture["settings-app"], ["Theme"])
        XCTAssertEqual(
            requiredTextByCapture["settings-agents"],
            ["Check Again", "Claude Code"]
        )
        XCTAssertEqual(
            requiredTextByCapture["settings-agent-claude"],
            ["Claude Code", "Adapter", "Compatibility", "Runtime", "Account"]
        )
        XCTAssertEqual(
            requiredTextByCapture["settings-terminal"],
            ["Ghostty Overlay", "Reload"]
        )
        XCTAssertEqual(
            requiredTextByCapture["settings-mcp-servers"],
            ["Servers", "Add Server…", "No MCP servers yet."]
        )
        XCTAssertEqual(requiredTextByCapture["settings-storage"], ["Tool Result Cache", "Cache limit", "Clear Cache…"])
    }

    func testWorkspaceReplacementDocumentsEveryIntentionalWindowDuplicate() async throws {
        let context = RecordingScenarioContext()

        try await UIScenarioDefinition.workspaceCoreReplacement.run(in: context)

        let expectedIdentifiers: Set<String> = [
            "workspace.root",
            "workspace.sidebar",
            "workspace.detail",
            "route.chat",
            "chat.composer",
            "chat.submit",
            "chat.engine-menu",
            "sidebar.identity-block",
        ]
        XCTAssertEqual(context.captureRequests.count, 2)
        for request in context.captureRequests {
            let exceptions = request.auditPolicy.exceptions
            XCTAssertEqual(Set(exceptions.compactMap(\.identifier)), expectedIdentifiers)
            XCTAssertTrue(
                exceptions.allSatisfy {
                    $0.rule == .duplicateIdentifier
                        && $0.isNarrowlyDocumented
                }
            )
        }
    }

    func testWorkspaceShapeScenariosSeedAndProveTheirSidebarSections() async throws {
        let single = UIScenarioDefinition.workspaceSingleProjectShapeLight
        XCTAssertEqual(
            single.launchConfiguration.environment["OTTAM_SCREENSHOT_CONVERSATION"],
            "1"
        )
        let singleContext = RecordingScenarioContext()
        try await single.run(in: singleContext)
        XCTAssertTrue(
            try XCTUnwrap(singleContext.captureRequests.first)
                .auditPolicy.requiredIdentifiers.contains("sidebar.history-section")
        )

        let multi = UIScenarioDefinition.workspaceMultiProjectShapeLight
        XCTAssertEqual(
            multi.launchConfiguration.environment["OTTAM_SCREENSHOT_CONVERSATION"],
            "1"
        )
        let multiContext = RecordingScenarioContext()
        try await multi.run(in: multiContext)
        XCTAssertTrue(
            try XCTUnwrap(multiContext.captureRequests.first)
                .auditPolicy.requiredIdentifiers.contains("sidebar.chats-section")
        )
    }

    func testTwoWindowIdentityScenarioRequiresDistinctLabelsAndGroupedPixels() async throws {
        let context = RecordingScenarioContext()
        try await UIScenarioDefinition.workspaceTwoWindowIdentityCapture.run(in: context)

        let identity = UIElementReference(identifier: "sidebar.identity-block")
        XCTAssertTrue(
            context.waitedConditions.contains(
                .identifierCountEquals(identity, 2)
            )
        )
        XCTAssertTrue(
            context.waitedConditions.contains(
                .distinctIdentifierLabelCountEquals(identity, 2)
            )
        )
        XCTAssertEqual(
            try XCTUnwrap(context.captureRequests.first).windowGroupFixtureName,
            "workspace-window-lifecycle"
        )
    }

    func testAuthRecoveryUsesSignInFixtureAndCoversStablePhases() async throws {
        let scenario = UIScenarioDefinition.chatAuthRecovery
        let context = RecordingScenarioContext()

        XCTAssertTrue(
            scenario.launchConfiguration.fixtureRequirements.contains(.testACPSignIn)
        )
        try await scenario.run(in: context)

        XCTAssertEqual(context.acpBehaviors, [.authRequired, .complete])
        XCTAssertTrue(context.activities.contains("acp-sign-in-completed"))
        XCTAssertEqual(
            context.waitedValues,
            [
                "chat.auth-recovery=signed out",
                "chat.auth-recovery=signing in",
                "chat.auth-recovery=signed in",
            ]
        )
        XCTAssertEqual(
            context.captureIDs,
            [
                "chat-auth-signed-out",
                "chat-auth-signing-in",
                "chat-auth-signed-in",
                "chat-auth-recovered",
            ]
        )
        XCTAssertEqual(
            context.captureRequests
                .first { $0.captureID == "chat-auth-signing-in" }?
                .scopeIdentifier,
            "chat.auth-recovery"
        )
        XCTAssertEqual(
            context.captureRequests
                .first { $0.captureID == "chat-auth-signing-in" }?
                .snapshotPolicy,
            .animatedRoute
        )
    }

    func testFailureStatePreservesPrimaryWhenCleanupOverridesVerdict() {
        var state = RunFailureState()
        state.recordPrimary(
            RunFailure(
                classification: .timedOut,
                phase: "scenario",
                message: "readiness timed out"
            )
        )
        state.recordSecondary(
            RunFailure(
                classification: .cleanupFailed,
                phase: "cleanup",
                message: "child remained alive"
            )
        )

        XCTAssertEqual(state.classification, .cleanupFailed)
        XCTAssertEqual(state.primaryFailure?.classification, .timedOut)
        XCTAssertEqual(state.secondaryFailures.map(\.classification), [.cleanupFailed])
    }

    func testIsolationOverridesPrimaryButNotCleanup() {
        var state = RunFailureState()
        state.recordPrimary(
            RunFailure(
                classification: .failedAssertion,
                phase: "scenario",
                message: "surface missing"
            )
        )
        state.recordSecondary(
            RunFailure(
                classification: .isolationFailed,
                phase: "postflight",
                message: "canonical identity changed"
            )
        )
        XCTAssertEqual(state.classification, .isolationFailed)

        state.recordSecondary(
            RunFailure(
                classification: .cleanupFailed,
                phase: "cleanup",
                message: "cleanup failed"
            )
        )
        XCTAssertEqual(state.classification, .cleanupFailed)
    }

    func testSnapshotPolicyRejectsTruncationAndExcessDuration() {
        let capture = makeCapture(
            capturedNodes: 500,
            truncatedNodes: 3,
            durationMilliseconds: 5_001
        )
        let violations = SemanticSnapshotPolicy.route.violations(for: capture)

        XCTAssertEqual(violations.count, 2)
        XCTAssertTrue(violations.contains { $0.contains("3 nodes") })
        XCTAssertTrue(violations.contains { $0.contains("5001.0 ms") })
        XCTAssertEqual(SemanticSnapshotPolicy.route.timeoutSeconds, 5)
        XCTAssertEqual(
            SemanticSnapshotPolicy.route.maxDurationMilliseconds,
            5_000
        )
        XCTAssertEqual(
            SemanticSnapshotPolicy.wholeApplicationDiagnostic.timeoutSeconds,
            3
        )
    }

    func testEvidenceWriterEncodesTypedClassification() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appendingPathComponent("failure.json")
        let failure = RunFailure(
            classification: .backendFailed,
            phase: "fixture",
            message: "core exited"
        )

        try EvidenceWriter.writeEncodable(failure, to: url)
        let object = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: Data(contentsOf: url))
                as? [String: Any]
        )
        XCTAssertEqual(object["classification"] as? String, "backend_failed")
        XCTAssertEqual(object["phase"] as? String, "fixture")
    }

    func testRunPathsRemoveOnlyKnownTransientDirectories() throws {
        let worktree = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(
            at: worktree,
            withIntermediateDirectories: true
        )
        defer { try? FileManager.default.removeItem(at: worktree) }
        let identity = try IsolationIdentity(
            worktreeURL: worktree,
            runID: "r00000000000000000000000000000000"
        )
        let paths = try RunPaths(worktreeURL: worktree, identity: identity)
        XCTAssertEqual(
            paths.root.standardizedFileURL,
            worktree
                .appendingPathComponent(".build/ui-tests/current")
                .standardizedFileURL
        )
        try Data("cache".utf8).write(
            to: paths.temporary.appendingPathComponent("node-cache")
        )
        try Data("evidence".utf8).write(
            to: paths.diagnostics.appendingPathComponent("failure.txt")
        )

        XCTAssertTrue(
            try paths.removeTransientDirectory(.temporary)
        )
        XCTAssertFalse(paths.transientDirectoryExists(.temporary))
        XCTAssertFalse(
            try paths.removeTransientDirectory(.diagnostics, onlyIfEmpty: true)
        )
        XCTAssertTrue(paths.transientDirectoryExists(.diagnostics))

        try FileManager.default.removeItem(
            at: paths.diagnostics.appendingPathComponent("failure.txt")
        )
        XCTAssertTrue(
            try paths.removeTransientDirectory(.diagnostics, onlyIfEmpty: true)
        )
        XCTAssertFalse(paths.transientDirectoryExists(.diagnostics))
        XCTAssertTrue(FileManager.default.fileExists(atPath: paths.home.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: paths.logs.path))
    }

    func testRunPathsCanPublishIndependentSuiteMembers() throws {
        let worktree = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(
            at: worktree,
            withIntermediateDirectories: true
        )
        defer { try? FileManager.default.removeItem(at: worktree) }
        let identity = try IsolationIdentity(
            worktreeURL: worktree,
            runID: "r00000000000000000000000000000000"
        )
        let memberRoot = worktree
            .appendingPathComponent(".build/ui-tests/current/scenarios/chat.auth-recovery")
        let paths = try RunPaths(
            worktreeURL: worktree,
            identity: identity,
            evidenceRoot: memberRoot
        )

        XCTAssertEqual(paths.root.standardizedFileURL, memberRoot.standardizedFileURL)
        XCTAssertTrue(paths.home.path.hasPrefix(memberRoot.path + "/"))
        XCTAssertTrue(paths.stagedApplications.path.hasPrefix(memberRoot.path + "/"))
    }
}

private final class RecordingScenarioContext: UIScenarioContext {
    var captureIDs: [String] = []
    var captureRequests: [ScenarioCaptureRequest] = []
    var activities: [String] = []
    var actions: [String] = []
    var values: [String: String] = [:]
    var waitedValues: [String] = []
    var waitedConditions: [UICondition] = []
    var acpBehaviors: [ACPPromptBehavior] = []

    func element(id: String) -> UIElementReference {
        UIElementReference(identifier: id)
    }

    func waitFor(_ condition: UICondition, timeout _: TimeInterval) async throws {
        waitedConditions.append(condition)
        if case let .exists(element) = condition {
            _ = values[element.identifier, default: ""]
        }
        if case let .valueEquals(element, expected) = condition {
            waitedValues.append("\(element.identifier)=\(expected)")
        }
    }

    func perform(
        _ action: UISemanticAction,
        on element: UIElementReference,
        value: String?
    ) async throws {
        if action == .setValue, let value {
            values[element.identifier] = value
            actions.append("\(action.rawValue):\(element.identifier)=\(value)")
        } else {
            actions.append("\(action.rawValue):\(element.identifier)")
        }
    }

    func assert(_ condition: UICondition) async throws {
        if case let .valueEquals(element, expected) = condition,
           values[element.identifier] != expected
        {
            throw UIScenarioFailure.assertion("unexpected value")
        }
    }

    func recordActivity(_ name: String, fields _: [String: String]) throws {
        activities.append(name)
    }

    func step(
        _ name: String,
        operation: () async throws -> Void
    ) async throws {
        activities.append(name)
        try await operation()
    }

    func capture(_ request: ScenarioCaptureRequest) async throws {
        captureIDs.append(request.captureID)
        captureRequests.append(request)
    }

    func selectListRow(named name: String, waitForText: String) async throws {
        activities.append("select-list-row:\(name)->\(waitForText)")
    }

    func pressControl(named name: String, waitForText: String) async throws {
        activities.append("press-control:\(name)->\(waitForText)")
    }

    func armACP(_ behavior: ACPPromptBehavior) throws {
        acpBehaviors.append(behavior)
    }

    func armACP(_ behavior: ACPPromptBehavior, reply: String) throws {
        acpBehaviors.append(behavior)
    }

    func armStreamedACP(reply: String, chunkBytes: Int, chunkDelayMillis: Int) throws {
        acpBehaviors.append(.stream)
    }

    func armUsageACP(_ behavior: ACPPromptBehavior, contextUsed: Int) throws {
        acpBehaviors.append(behavior)
    }

    func completeACPSignIn() throws {
        activities.append("acp-sign-in-completed")
    }

    func publishACPMarker(_ name: String) throws {
        activities.append("acp-marker:\(name)")
    }

    func waitForACPPromptCount(
        _ expectedCount: Int,
        timeout _: TimeInterval
    ) async throws {
        activities.append("acp-prompt-count-reached:\(expectedCount)")
    }

    func assertACPPromptCount(_ expectedCount: Int) throws {
        activities.append("acp-prompt-count:\(expectedCount)")
    }

    func assertFixture(_ name: String, timeout _: TimeInterval) async throws {
        activities.append("fixture:\(name)")
    }

    func terminateOwnedCore(expectReplacement: Bool) async throws {
        activities.append("core-replacement:\(expectReplacement)")
    }

    func proveOcclusionIsolation() async throws {}

    func forceAssertionFailure() async throws {
        throw UIScenarioFailure.assertion("forced")
    }
}

private func makeCapture(
    capturedNodes: Int,
    truncatedNodes: Int,
    durationMilliseconds: Double
) -> AXTreeCapture {
    AXTreeCapture(
        schemaVersion: 1,
        targetPID: 1,
        scopeIdentifier: nil,
        maxDepth: 12,
        maxNodes: 500,
        capturedNodes: capturedNodes,
        truncatedNodes: truncatedNodes,
        omittedValues: 0,
        durationMilliseconds: durationMilliseconds,
        root: makeNode()
    )
}

private func makeNode(
    role: String? = "AXGroup",
    identifier: String? = nil,
    label: String? = nil,
    title: String? = nil,
    focused: Bool? = false,
    actions: [String] = [],
    children: [NormalizedAXNode] = []
) -> NormalizedAXNode {
    NormalizedAXNode(
        role: role,
        subrole: nil,
        identifier: identifier,
        label: label,
        title: title,
        value: nil,
        placeholder: nil,
        frame: nil,
        enabled: true,
        selected: false,
        focused: focused,
        actions: actions,
        children: children
    )
}
