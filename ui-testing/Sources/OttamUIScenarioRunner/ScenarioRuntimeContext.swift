import Darwin
import Foundation
import MacAutomationKit
import ScenarioKit

final class ScenarioRuntimeContext: UIScenarioContext {
    let client: DriverClient
    let launched: LaunchedApplication
    let paths: RunPaths
    let actions: ActionRecorder

    private(set) var artifactRecords: [[String: Any]] = []
    private(set) var intervals: [[String: Any]] = []
    var observedChildren: Set<ProcessIdentity> = []
    private var coreIdentityBeforeExecutableMove: ProcessIdentity?

    init(
        client: DriverClient,
        launched: LaunchedApplication,
        paths: RunPaths,
        actions: ActionRecorder
    ) {
        self.client = client
        self.launched = launched
        self.paths = paths
        self.actions = actions
        observedChildren.formUnion(launched.childProcesses)
    }

    func element(id: String) -> UIElementReference {
        UIElementReference(identifier: id)
    }

    func waitFor(_ condition: UICondition, timeout: TimeInterval) async throws {
        let started = Date()
        let deadline = started.addingTimeInterval(timeout)
        while Date() < deadline {
            if try evaluate(condition) {
                let ended = Date()
                intervals.append(
                    scenarioInterval(
                        "semantic",
                        "wait for \(describe(condition))",
                        started,
                        ended
                    )
                )
                try actions.append(
                    event: "condition_satisfied",
                    fields: [
                        "condition": describe(condition),
                        "durationMilliseconds": ended.timeIntervalSince(started) * 1_000,
                    ]
                )
                return
            }
            try ensureApplicationIsAlive()
            try await Task.sleep(for: .milliseconds(100))
        }
        throw ScenarioRuntimeError.waitTimedOut(describe(condition))
    }

    func perform(
        _ action: UISemanticAction,
        on element: UIElementReference,
        value: String?
    ) async throws {
        let started = Date()
        var parameters: [String: Any] = [
            "pid": launched.processIdentity.pid,
            "action": action.rawValue,
            "identifier": element.identifier,
            "timeout": 3,
        ]
        if let value {
            parameters["value"] = value
        }
        let response = try client.invoke(
            command: "action",
            parameters: parameters
        )
        guard response.status == "passed" else {
            throw scenarioDriverFailure(
                response,
                fallback: "\(action.rawValue) failed for \(element.identifier)"
            )
        }
        let ended = Date()
        intervals.append(
            scenarioInterval(
                "semantic",
                "\(action.rawValue) \(element.identifier)",
                started,
                ended
            )
        )
        try actions.append(
            event: "semantic_action",
            fields: [
                "action": action.rawValue,
                "identifier": element.identifier,
            ]
        )
        refreshChildren()
    }

    func assert(_ condition: UICondition) async throws {
        guard try evaluate(condition) else {
            throw UIScenarioFailure.assertion(
                "condition is not satisfied: \(describe(condition))"
            )
        }
        try actions.append(
            event: "assertion_passed",
            fields: ["condition": describe(condition)]
        )
    }

    func selectListRow(named name: String, waitForText: String) async throws {
        try performNamedCommand("select-list-row", name: name, waitForText: waitForText)
    }

    func pressControl(named name: String, waitForText: String) async throws {
        try performNamedCommand("press-control", name: name, waitForText: waitForText)
    }

    private func performNamedCommand(
        _ command: String,
        name: String,
        waitForText: String
    ) throws {
        let response = try client.invoke(
            command: command,
            parameters: [
                "pid": launched.processIdentity.pid,
                "name": name,
                "waitForText": waitForText,
                "timeout": 8,
            ]
        )
        guard response.status == "passed" else {
            throw scenarioDriverFailure(
                response,
                fallback: "\(command) failed for \(name)"
            )
        }
        try actions.append(
            event: "semantic_action",
            fields: ["action": command, "name": name, "waitForText": waitForText]
        )
    }

    func recordActivity(_ name: String, fields: [String: String]) throws {
        try actions.append(event: name, fields: fields)
    }

    func step(
        _ name: String,
        operation: () async throws -> Void
    ) async throws {
        let started = Date()
        try actions.append(event: "step_started", fields: ["name": name])
        do {
            try await operation()
            let ended = Date()
            intervals.append(
                scenarioInterval("semantic", name, started, ended)
            )
            try actions.append(event: "step_passed", fields: ["name": name])
        } catch {
            try? actions.append(
                event: "step_failed",
                fields: [
                    "name": name,
                    "message": String(describing: error),
                ]
            )
            throw error
        }
    }

    func capture(_ request: ScenarioCaptureRequest) async throws {
        let readinessStarted = Date()
        let windowID = try await waitForWindowID(
            request.controlName,
            timeout: 20
        )
        refreshChildren()
        let readinessAt = Date()

        let captureStarted = Date()
        let screenshotURL = paths.screenshots
            .appendingPathComponent("\(request.captureID).png")
        var screenshotCommand = "capture"
        var screenshotParameters: [String: Any] = [
            "pid": launched.processIdentity.pid,
            "windowID": windowID,
            "outputPath": screenshotURL.path,
        ]
        if let fixtureName = request.windowGroupFixtureName {
            try await assertFixture(fixtureName, timeout: 20)
            screenshotCommand = "capture-window-group"
            screenshotParameters["windowIDs"] = try fixtureWindowIDs(fixtureName)
            screenshotParameters.removeValue(forKey: "windowID")
        }
        let screenshot = try client.invoke(
            command: screenshotCommand,
            parameters: screenshotParameters
        )
        guard screenshot.status == "passed" else {
            throw scenarioDriverFailure(screenshot, fallback: "capture failed")
        }
        let screenshotAt = Date()

        let treeURL = paths.accessibility
            .appendingPathComponent("\(request.captureID).json")
        var treeParameters: [String: Any] = [
            "pid": launched.processIdentity.pid,
            "outputPath": treeURL.path,
            "maxDepth": request.snapshotPolicy.maxDepth,
            "maxNodes": request.snapshotPolicy.maxNodes,
            "maxValueLength": request.snapshotPolicy.maxValueLength,
            "timeout": request.snapshotPolicy.timeoutSeconds,
        ]
        if let scopeIdentifier = request.scopeIdentifier {
            treeParameters["scopeIdentifier"] = scopeIdentifier
        }
        let treeStarted = Date()
        let treeResponse = try client.invoke(
            command: "semantic-tree",
            parameters: treeParameters
        )
        guard treeResponse.status == "passed" else {
            throw scenarioDriverFailure(
                treeResponse,
                fallback: "semantic capture failed"
            )
        }
        let treeAt = Date()
        intervals.append(
            scenarioInterval(
                "semantic",
                "capture \(request.captureID) AX tree",
                treeStarted,
                treeAt
            )
        )

        let capture = try decode(AXTreeCapture.self, from: treeResponse.result)
        let policyViolations = request.snapshotPolicy.violations(for: capture)
        let audit = AXAccessibilityAudit.audit(capture, policy: request.auditPolicy)
        let auditURL = paths.accessibility
            .appendingPathComponent("\(request.captureID).audit.json")
        try EvidenceWriter.writeEncodable(audit, to: auditURL)
        try actions.append(
            event: "accessibility_audit",
            fields: [
                "captureID": request.captureID,
                "policy": request.auditPolicy.name,
                "blockingIssueCount": audit.blockingIssueCount,
                "exceptionCount": audit.exceptionCount,
            ]
        )

        let screenshotPath = scenarioRelative(screenshotURL, to: paths.root)
        let treePath = scenarioRelative(treeURL, to: paths.root)
        let auditPath = scenarioRelative(auditURL, to: paths.root)
        artifactRecords.append([
            "captureID": request.captureID,
            "sourceControlName": request.controlName,
            "screenshot": screenshotPath,
            "semanticTree": treePath,
            "accessibilityAudit": auditPath,
            "screenshotStatus": "authoritative",
            "semanticTreeStatus": "authoritative",
            "accessibilityAuditStatus":
                audit.blockingIssueCount == 0 ? "passed" : "failed",
            "captureOrder": ["screenshot", "semanticTree", "accessibilityAudit"],
            "screenshotTimestamp": scenarioISO(screenshotAt),
            "semanticTreeTimestamp": scenarioISO(treeAt),
            "observedSkewMilliseconds":
                treeAt.timeIntervalSince(screenshotAt) * 1_000,
            "readinessWaitMilliseconds":
                readinessAt.timeIntervalSince(readinessStarted) * 1_000,
            "window": screenshot.result,
            "tree": treeResponse.result,
            "snapshotPolicy": try EvidenceWriter.object(request.snapshotPolicy),
            "snapshotPolicyViolations": policyViolations,
            "auditPolicy": try EvidenceWriter.object(request.auditPolicy),
            "auditSummary": [
                "inspectedNodes": audit.inspectedNodes,
                "blockingIssueCount": audit.blockingIssueCount,
                "exceptionCount": audit.exceptionCount,
            ],
        ])
        intervals.append(
            scenarioInterval(
                "quiet",
                "ScreenCaptureKit \(request.captureID)",
                captureStarted,
                screenshotAt
            )
        )

        if !policyViolations.isEmpty {
            throw UIScenarioFailure.assertion(
                "snapshot policy \(request.snapshotPolicy.name) failed: "
                    + policyViolations.joined(separator: "; ")
            )
        }
        if audit.blockingIssueCount > 0 {
            throw UIScenarioFailure.assertion(
                "accessibility audit \(request.auditPolicy.name) found "
                    + "\(audit.blockingIssueCount) blocking issue(s)"
            )
        }

        refreshChildren()
        try Data().write(
            to: paths.control.appendingPathComponent("\(request.controlName).done"),
            options: .atomic
        )
    }

    private func fixtureWindowIDs(_ name: String) throws -> [UInt32] {
        let url = paths.control
            .appendingPathComponent("fixtures", isDirectory: true)
            .appendingPathComponent("\(name).json")
        let data = try Data(contentsOf: url)
        guard let object = try JSONSerialization.jsonObject(with: data)
                as? [String: Any],
              let windows = object["initialWindows"] as? [[String: Any]]
        else {
            throw UIScenarioFailure.fixture(
                "fixture \(name) has no initial window evidence"
            )
        }
        let windowIDs = windows.compactMap {
            ($0["windowNumber"] as? NSNumber)?.uint32Value
        }
        guard windowIDs.count == windows.count, windowIDs.count >= 2 else {
            throw UIScenarioFailure.fixture(
                "fixture \(name) has invalid window identifiers"
            )
        }
        return windowIDs
    }

    func armACP(_ behavior: ACPPromptBehavior) throws {
        try armACP(behavior, plan: ["behavior": behavior.rawValue])
    }

    func releaseACPInitialization() throws {
        let directory = paths.control.appendingPathComponent("acp", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try FileManager.default.removeItem(at: directory.appendingPathComponent("initialize-hold"))
        try Data("release\n".utf8).write(to: directory.appendingPathComponent("initialize-release"), options: .atomic)
        try actions.append(event: "acp_initialize_released", fields: [:])
    }

    func holdACPInitialization() throws {
        let directory = paths.control.appendingPathComponent("acp", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try Data("hold\n".utf8).write(to: directory.appendingPathComponent("initialize-hold"), options: .atomic)
        try actions.append(event: "acp_initialize_held", fields: [:])
    }

    func armACP(_ behavior: ACPPromptBehavior, reply: String) throws {
        try armACP(behavior, plan: ["behavior": behavior.rawValue, "reply": reply])
    }

    func armStreamedACP(reply: String, chunkBytes: Int, chunkDelayMillis: Int) throws {
        try armACP(.stream, plan: [
            "behavior": ACPPromptBehavior.stream.rawValue,
            "reply": reply,
            "chunkBytes": chunkBytes,
            "chunkDelayMillis": chunkDelayMillis,
        ])
    }

    func armUsageACP(_ behavior: ACPPromptBehavior, contextUsed: Int) throws {
        try armACP(behavior, plan: ["behavior": behavior.rawValue, "contextUsed": contextUsed])
    }

    private func armACP(_ behavior: ACPPromptBehavior, plan: [String: Any]) throws {
        let directory = paths.control.appendingPathComponent("acp", isDirectory: true)
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        try EvidenceWriter.writeJSON(
            plan,
            to: directory.appendingPathComponent("next-prompt.json")
        )
        try appendFixtureEvent(
            "acp_armed",
            fields: ["behavior": behavior.rawValue]
        )
    }

    /// Releases a fake-agent step that waits on `name` in its control
    /// directory, such as `follow-up-complete`.
    func publishACPMarker(_ name: String) throws {
        let directory = paths.control.appendingPathComponent("acp", isDirectory: true)
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        try Data().write(
            to: directory.appendingPathComponent(name),
            options: .atomic
        )
        try appendFixtureEvent("acp_marker_published", fields: ["marker": name])
    }

    func completeACPSignIn() throws {
        let directory = paths.control.appendingPathComponent("acp", isDirectory: true)
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        try Data().write(
            to: directory.appendingPathComponent("sign-in-complete"),
            options: .atomic
        )
        try appendFixtureEvent("acp_sign_in_completed", fields: [:])
    }

    func waitForACPPromptCount(
        _ expectedCount: Int,
        timeout: TimeInterval
    ) async throws {
        let started = Date()
        let deadline = started.addingTimeInterval(timeout)
        while Date() < deadline {
            if acpPromptCount() >= expectedCount {
                try appendFixtureEvent(
                    "acp_prompt_count_reached",
                    fields: ["count": acpPromptCount()]
                )
                intervals.append(
                    scenarioInterval(
                        "fixture",
                        "wait for ACP prompt count \(expectedCount)",
                        started,
                        Date()
                    )
                )
                return
            }
            try ensureApplicationIsAlive()
            try await Task.sleep(for: .milliseconds(100))
        }
        throw ScenarioRuntimeError.waitTimedOut(
            "ACP prompt count \(expectedCount)"
        )
    }

    func assertACPPromptCount(_ expectedCount: Int) throws {
        let actualCount = acpPromptCount()
        guard actualCount == expectedCount else {
            throw UIScenarioFailure.fixture(
                "expected exactly \(expectedCount) ACP prompts, observed \(actualCount)"
            )
        }
        try appendFixtureEvent(
            "acp_prompt_count_asserted",
            fields: ["count": actualCount]
        )
    }

    func assertFixture(_ name: String, timeout: TimeInterval) async throws {
        let url = paths.control
            .appendingPathComponent("fixtures", isDirectory: true)
            .appendingPathComponent("\(name).json")
        let started = Date()
        let deadline = started.addingTimeInterval(timeout)
        while Date() < deadline {
            if let data = try? Data(contentsOf: url),
               let object = try? JSONSerialization.jsonObject(with: data)
                    as? [String: Any]
            {
                guard object["status"] as? String == "passed" else {
                    throw UIScenarioFailure.fixture(
                        object["message"] as? String
                            ?? "fixture \(name) reported failure"
                    )
                }
                try appendFixtureEvent(
                    "fixture_asserted",
                    fields: [
                        "name": name,
                        "evidence": scenarioRelative(url, to: paths.root),
                    ]
                )
                intervals.append(
                    scenarioInterval(
                        "fixture",
                        "assert fixture \(name)",
                        started,
                        Date()
                    )
                )
                return
            }
            try ensureApplicationIsAlive()
            try await Task.sleep(for: .milliseconds(100))
        }
        throw ScenarioRuntimeError.waitTimedOut("fixture evidence \(name)")
    }

    func setACPExecutableAvailable(_ available: Bool) throws {
        let fixtureBin = paths.home.appendingPathComponent(
            "fixture-bin",
            isDirectory: true
        )
        let moves = ["node", "npx"].map { name in
            let executable = fixtureBin.appendingPathComponent(name)
            let unavailable = fixtureBin.appendingPathComponent(
                "\(name).unavailable"
            )
            return (
                source: available ? unavailable : executable,
                destination: available ? executable : unavailable
            )
        }
        for move in moves {
            guard FileManager.default.fileExists(atPath: move.source.path) else {
                throw UIScenarioFailure.fixture(
                    "ACP fixture \(move.source.lastPathComponent) is missing"
                )
            }
        }
        for move in moves {
            if FileManager.default.fileExists(atPath: move.destination.path) {
                try FileManager.default.removeItem(at: move.destination)
            }
            try FileManager.default.moveItem(
                at: move.source,
                to: move.destination
            )
        }
        try appendFixtureEvent(
            "acp_executable_availability",
            fields: ["available": available]
        )
    }

    func terminateOwnedCore(expectReplacement: Bool) async throws {
        refreshChildren()
        let candidates = ProcessIdentity.descendants(
            of: launched.processIdentity.pid
        ).filter {
            $0.executablePath?.hasSuffix("/ottam-core") == true
        }
        let core = candidates.count == 1
            ? candidates.first
            : coreIdentityBeforeExecutableMove
        guard let core else {
            throw UIScenarioFailure.fixture(
                "expected exactly one recorded ottam-core child, found \(candidates.count)"
            )
        }
        coreIdentityBeforeExecutableMove = nil
        let started = Date()
        guard kill(core.pid, SIGTERM) == 0 else {
            throw UIScenarioFailure.fixture(
                "could not terminate exact Core identity \(core.stableValue)"
            )
        }
        let exitDeadline = Date().addingTimeInterval(5)
        while ProcessIdentity.forPID(core.pid) == core, Date() < exitDeadline {
            try await Task.sleep(for: .milliseconds(50))
        }
        guard ProcessIdentity.forPID(core.pid) != core else {
            throw UIScenarioFailure.backend(
                "Core identity \(core.stableValue) did not exit"
            )
        }
        observedChildren.insert(core)
        try appendFixtureEvent(
            "core_terminated",
            fields: ["processIdentity": core.stableValue]
        )
        guard expectReplacement else { return }

        let replacementDeadline = Date().addingTimeInterval(15)
        while Date() < replacementDeadline {
            let replacements = ProcessIdentity.descendants(
                of: launched.processIdentity.pid
            ).filter {
                $0.executablePath?.hasSuffix("/ottam-core") == true
                    && $0 != core
            }
            if replacements.count == 1, let replacement = replacements.first {
                observedChildren.insert(replacement)
                try appendFixtureEvent(
                    "core_replaced",
                    fields: [
                        "previousIdentity": core.stableValue,
                        "replacementIdentity": replacement.stableValue,
                    ]
                )
                intervals.append(
                    scenarioInterval(
                        "fixture",
                        "replace exact Core child",
                        started,
                        Date()
                    )
                )
                return
            }
            try ensureApplicationIsAlive()
            try await Task.sleep(for: .milliseconds(100))
        }
        throw UIScenarioFailure.backend(
            "Core did not recover with exactly one replacement identity"
        )
    }

    func setBundledCoreAvailable(_ available: Bool) throws {
        if !available {
            refreshChildren()
            let candidates = ProcessIdentity.descendants(
                of: launched.processIdentity.pid
            ).filter {
                $0.executablePath?.hasSuffix("/ottam-core") == true
            }
            guard candidates.count == 1 else {
                throw UIScenarioFailure.fixture(
                    "expected exactly one Core identity before moving its executable"
                )
            }
            coreIdentityBeforeExecutableMove = candidates[0]
        }
        let executable = URL(fileURLWithPath: launched.bundleURL)
            .appendingPathComponent("Contents/MacOS/ottam-core")
        let unavailable = executable.appendingPathExtension("unavailable")
        let source = available ? unavailable : executable
        let destination = available ? executable : unavailable
        guard FileManager.default.fileExists(atPath: source.path) else {
            throw UIScenarioFailure.fixture(
                "Core fixture source is missing: \(source.path)"
            )
        }
        if FileManager.default.fileExists(atPath: destination.path) {
            try FileManager.default.removeItem(at: destination)
        }
        try FileManager.default.moveItem(at: source, to: destination)
        try appendFixtureEvent(
            "core_executable_availability",
            fields: ["available": available]
        )
    }

    func forceAssertionFailure() async throws {
        _ = try client.invoke(
            command: "semantic-tree",
            parameters: [
                "pid": launched.processIdentity.pid,
                "outputPath": paths.accessibility
                    .appendingPathComponent("deliberately-missing.json").path,
                "scopeIdentifier": "deliberately.missing.element",
                "timeout": 1,
            ]
        )
        throw UIScenarioFailure.assertion("deliberate driver assertion failure")
    }

    func proveOcclusionIsolation() async throws {
        let windowID = try await waitForWindowID("route-chat", timeout: 20)
        refreshChildren()
        let baselineURL = paths.screenshots
            .appendingPathComponent("occlusion-baseline.png")
        let displayURL = paths.screenshots
            .appendingPathComponent("occlusion-display-proof.png")
        let candidateURL = paths.screenshots
            .appendingPathComponent("occlusion-isolated.png")
        let proof = try client.invoke(
            command: "occlusion-proof",
            parameters: [
                "pid": launched.processIdentity.pid,
                "windowID": windowID,
                "baselineOutputPath": baselineURL.path,
                "displayOutputPath": displayURL.path,
                "isolatedOutputPath": candidateURL.path,
            ]
        )
        guard proof.status == "passed" else {
            throw scenarioDriverFailure(proof, fallback: "occlusion proof failed")
        }
        let baselineHash = try fileSHA256(baselineURL)
        let candidateHash = try fileSHA256(candidateURL)
        let displayHash = try fileSHA256(displayURL)
        guard proof.result["exactTargetBoundsPreserved"] as? Bool == true,
              proof.result["controlledOverlayExcluded"] as? Bool == true
        else {
            throw UIScenarioFailure.assertion(
                "controlled occlusion capture did not preserve complete target bounds"
            )
        }
        try actions.append(
            event: "occlusion_proof",
            fields: [
                "baselineSHA256": baselineHash,
                "displayProofSHA256": displayHash,
                "isolatedSHA256": candidateHash,
                "completeTargetBoundsPreserved": true,
                "controlledOverlayFrameIntersectsTarget": true,
                "overlayVisiblePixelCount":
                    proof.result["overlayVisiblePixelCount"] as? Int ?? 0,
                "isolatedMarkerPixelCount":
                    proof.result["isolatedMarkerPixelCount"] as? Int ?? 0,
            ]
        )
        refreshChildren()
        try Data().write(
            to: paths.control.appendingPathComponent("route-chat.done"),
            options: .atomic
        )
        intervals.append([
            "tier": "quiet",
            "name": "controlled overlay ScreenCaptureKit proof",
            "baselineSHA256": baselineHash,
            "displayProofSHA256": displayHash,
            "isolatedSHA256": candidateHash,
        ])
        artifactRecords.append([
            "captureID": "occlusion-proof",
            "baseline": scenarioRelative(baselineURL, to: paths.root),
            "displayProof": scenarioRelative(displayURL, to: paths.root),
            "isolatedWithOverlay": scenarioRelative(candidateURL, to: paths.root),
            "baselineSHA256": baselineHash,
            "displayProofSHA256": displayHash,
            "isolatedWithOverlaySHA256": candidateHash,
            "completeTargetBoundsPreserved": true,
            "controlledOverlayExcluded": true,
            "proofBasis":
                "a display capture contains the opaque driver marker over the target, "
                + "while desktopIndependentWindow capture excludes its marker pixels "
                + "and retains the exact target window ID, frame, and pixel dimensions",
            "proofMetrics": proof.result,
        ])
    }

}
