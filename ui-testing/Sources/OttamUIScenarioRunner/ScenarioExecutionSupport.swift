import AppKit
import OttamAccessibility
import OttamUIScenarios
import Foundation
import MacAutomationKit
import ScenarioKit

extension ScenarioExecution {
    func launchEnvironment(
        scenario: UIScenarioDefinition,
        identity: IsolationIdentity,
        paths: RunPaths
    ) -> [String: String] {
        var environment = [
            "OTTAM_UI_TEST_MODE": "1",
            "OTTAM_HOME": paths.home.path,
            "OTTAM_LOG_DIR": paths.logs.path,
            "OTTAM_WORKSPACE": identity.worktreePath,
            "OTTAM_INSTANCE_ID": identity.runID,
            "OTTAM_SCREENSHOT_ROUTES": scenario.launchConfiguration.routes,
            "OTTAM_SCREENSHOT_CHOOSER":
                scenario.launchConfiguration.capturesChooser ? "1" : "0",
            "OTTAM_UI_TEST_CAPTURE_SETTINGS":
                scenario.launchConfiguration.capturesSettings ? "1" : "0",
            // Scenario execution owns exact process cleanup. Keeping the staged app
            // alive also permits multiple actions and captures in one journey and
            // preserves post-failure evidence.
            "OTTAM_SCREENSHOT_EXIT": "0",
            "OTTAM_SCREENSHOT_EXTERNAL_DIR": paths.control.path,
            "OTTAM_SCREENSHOT_WINDOW_SIZE": "1100x720",
            "OTTAM_UI_TEST_QUIET":
                scenario.launchConfiguration.validationProfile == "smoke" ? "1" : "0",
            "OTTAM_MODEL_CATALOG_PROBE": "off",
            // The run home is wiped before every run, so no adapter is ever
            // installed in it — leaving the store on would put every scenario
            // in the no-usable-engine state, where the composer offers setup
            // instead of a send button. Off restores the pre-install launch
            // path, which resolves through the fixture npx shim. A scenario
            // that wants the uninstalled state turns it back on below.
            "OTTAM_ADAPTER_INSTALLS": "off",
            "OTTAM_LOG": "ottam_core=info,warn",
            "TMPDIR": paths.temporary.path + "/",
        ]
        for (key, value) in scenario.launchConfiguration.environment {
            environment[key] = value
        }
        let fixtureRequirements = Set(
            scenario.launchConfiguration.fixtureRequirements
        )
        if fixtureRequirements.contains(.testACP)
            || fixtureRequirements.contains(.nativeSteering)
        {
            let fixtureBin = paths.home.appendingPathComponent("fixture-bin")
            environment["OTTAM_TEST_ACP_CONTROL_DIR"] =
                paths.control.appendingPathComponent("acp").path
            environment["OTTAM_FAKE_ACP_LOG"] =
                paths.logs.appendingPathComponent("acp-requests.jsonl").path
            if fixtureRequirements.contains(.testACPSignIn) {
                let completionPath = paths.control
                    .appendingPathComponent("acp", isDirectory: true)
                    .appendingPathComponent("sign-in-complete")
                    .path
                environment["OTTAM_TEST_ACP_AUTH_TERMINAL"] =
                    "/bin/sh\t-c\twhile [ ! -f \"$1\" ]; do sleep 0.05; done"
                    + "\tottam-ui-auth\t\(completionPath)"
            } else {
                environment["OTTAM_TEST_ACP_AUTH_TERMINAL"] =
                    "/bin/sh\t-c\tsleep 1"
            }
            environment["OTTAM_MODEL_CATALOG_PROBE"] = "off"
            // NodeResolver intentionally prefers an explicit override over the
            // login shell and well-known installations. Point it at the test
            // shim so hosted runners cannot bypass the deterministic ACP fixture.
            environment["OTTAM_NODE"] =
                fixtureBin.appendingPathComponent("node").path
            environment["PATH"] = fixtureBin.path + ":/usr/bin:/bin:/usr/sbin:/sbin"
            if fixtureRequirements.contains(.heldLoginShell) {
                // The override answers without a search, so it has to go for
                // the probe to reach the login shell at all.
                environment.removeValue(forKey: "OTTAM_NODE")
                environment["SHELL"] =
                    fixtureBin.appendingPathComponent("held-login-shell").path
            }
        }
        if fixtureRequirements.contains(.nativeSteering) {
            environment["OTTAM_TEST_NATIVE_STEER_ENGINE"] = "1"
            environment["OTTAM_UI_TEST_PREFERRED_ENGINE"] = "test.native"
        }
        if fixtureRequirements.contains(.additionalProject) {
            environment["OTTAM_UI_TEST_PROJECT_PATHS"] =
                additionalProjectFixturePath(paths: paths)
        }
        if fixtureRequirements.contains(.deterministicProject) {
            environment["OTTAM_WORKSPACE"] =
                deterministicProjectFixturePath(paths: paths)
        }
        return environment
    }

    /// Deterministic so `launchEnvironment` (called before the app launches)
    /// and `prepareFixtures` (which creates the directory) agree on the path
    /// without threading it through as extra state.
    func additionalProjectFixturePath(paths: RunPaths) -> String {
        paths.home
            .appendingPathComponent("fixtures", isDirectory: true)
            .appendingPathComponent("additional-project", isDirectory: true)
            .path
    }

    func deterministicProjectFixturePath(paths: RunPaths) -> String {
        paths.home
            .appendingPathComponent("fixtures", isDirectory: true)
            .appendingPathComponent("smoke-project", isDirectory: true)
            .path
    }

    func prepareDeterministicProjectFixture(paths: RunPaths) throws {
        let path = deterministicProjectFixturePath(paths: paths)
        guard !FileManager.default.fileExists(atPath: "\(path)/.git") else { return }
        try FileManager.default.createDirectory(
            atPath: path,
            withIntermediateDirectories: true
        )
        let readme = "\(path)/README.md"
        try "Ottam visual smoke fixture.\n"
            .write(toFile: readme, atomically: true, encoding: .utf8)
        try Command.run(["/usr/bin/git", "-C", path, "init", "-q", "-b", "main"])
        try Command.run([
            "/usr/bin/git", "-C", path,
            "add", "README.md",
        ])
        try Command.run([
            "/usr/bin/git", "-C", path,
            "-c", "commit.gpgSign=false",
            "-c", "core.hooksPath=/dev/null",
            "-c", "user.name=Ottam UI Test",
            "-c", "user.email=ui-test@ottam.dev",
            "commit", "-q", "-m", "Initial smoke fixture",
        ])
        try "Ottam visual smoke fixture.\n\nA deterministic uncommitted change.\n"
            .write(toFile: readme, atomically: true, encoding: .utf8)
    }

    /// Installs that predate their pins: Claude's adapter at 0.0.9 and a
    /// managed Node v24.0.0. Neither is real — the adapter is never launched
    /// in a UI run, and the Node shims only answer the `--version` probes and
    /// the upgrade's `npm install`, which waits for the scenario's
    /// `release-npm` marker (bounded, so a failed run cannot strand it) and
    /// then fails as an offline registry would.
    func prepareSupersededAgentInstallsFixture(paths: RunPaths) throws {
        let files = FileManager.default
        let adapter = paths.home
            .appendingPathComponent("agents/claude/adapter/0.0.9", isDirectory: true)
        let entrypoint = "node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js"
        let entrypointURL = adapter.appendingPathComponent(entrypoint)
        try files.createDirectory(
            at: entrypointURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try Data().write(to: entrypointURL)
        let manifest: [String: Any] = [
            "name": "@agentclientprotocol/claude-agent-acp",
            "version": "0.0.9",
            "entrypoint": entrypoint,
            "installedAtUnix": 0,
        ]
        try JSONSerialization.data(withJSONObject: manifest, options: [.sortedKeys])
            .write(to: adapter.appendingPathComponent("manifest.json"))

        let control = paths.control.appendingPathComponent("acp", isDirectory: true)
        try files.createDirectory(at: control, withIntermediateDirectories: true)
        let release = control.appendingPathComponent("release-npm").path
        let bin = paths.home
            .appendingPathComponent("runtime/node/v24.0.0/bin", isDirectory: true)
        try files.createDirectory(at: bin, withIntermediateDirectories: true)
        let shims = [
            "node": "echo v24.0.0\n",
            "npx": "echo 11.0.0\n",
            "npm": """
                if [ "$1" = "--version" ]; then echo 11.0.0; exit 0; fi
                waited=0
                while [ ! -f '\(release)' ] && [ "$waited" -lt 1800 ]; do
                  sleep 0.1; waited=$((waited + 1))
                done
                echo 'npm error code ECONNREFUSED' >&2
                echo 'npm error syscall connect' >&2
                exit 1

                """,
        ]
        for (name, body) in shims {
            let url = bin.appendingPathComponent(name)
            try ("#!/bin/sh\n" + body).write(to: url, atomically: true, encoding: .utf8)
            try files.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
        }
    }

    /// A minimal, real Git repository — `openProject`/`addProject` require a
    /// canonical Git-backed path, and the app has no business synthesizing
    /// fixture repositories itself. Idempotent: each run gets a fresh `paths.home`,
    /// but a scenario retry within one run should not re-`git init` a directory
    /// that already has one.
    func prepareAdditionalProjectFixture(paths: RunPaths) throws {
        let path = additionalProjectFixturePath(paths: paths)
        guard !FileManager.default.fileExists(atPath: "\(path)/.git") else {
            return
        }
        try FileManager.default.createDirectory(
            atPath: path,
            withIntermediateDirectories: true
        )
        let readme = "\(path)/README.md"
        try "Fixture project for the multi-project sidebar scenario.\n"
            .write(toFile: readme, atomically: true, encoding: .utf8)
        try Command.run(["/usr/bin/git", "-C", path, "init", "-q", "-b", "main"])
        try Command.run([
            "/usr/bin/git", "-C", path,
            "add", "README.md",
        ])
        try Command.run([
            "/usr/bin/git", "-C", path,
            "-c", "commit.gpgSign=false",
            "-c", "core.hooksPath=/dev/null",
            "-c", "user.name=Ottam UI Test",
            "-c", "user.email=ui-test@ottam.dev",
            "commit", "-q", "-m", "Initial commit",
        ])
    }

    func prepareFixtures(
        scenario: UIScenarioDefinition,
        worktreeURL: URL,
        paths: RunPaths
    ) throws {
        let requirements = Set(scenario.launchConfiguration.fixtureRequirements)
        if requirements.contains(.incompatibleStorage) {
            // A migration newer than this binary in the runner's disposable home.
            // No live app data is opened or reset by this fixture.
            try Command.run([
                "/usr/bin/sqlite3", paths.home.appendingPathComponent("ottam.sqlite").path,
                "CREATE TABLE __diesel_schema_migrations (version TEXT PRIMARY KEY NOT NULL, run_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP); INSERT INTO __diesel_schema_migrations (version) VALUES ('99999999999999');",
            ])
        }
        if requirements.contains(.additionalProject) {
            try prepareAdditionalProjectFixture(paths: paths)
        }
        if requirements.contains(.deterministicProject) {
            try prepareDeterministicProjectFixture(paths: paths)
        }
        if requirements.contains(.supersededAgentInstalls) {
            try prepareSupersededAgentInstallsFixture(paths: paths)
        }
        guard requirements.contains(.testACP)
                || requirements.contains(.nativeSteering)
        else {
            return
        }
        let executable = worktreeURL
            .appendingPathComponent("backend/target/debug/ottam-test-acp")
        guard FileManager.default.isExecutableFile(atPath: executable.path) else {
            throw UIScenarioFailure.fixture(
                "ottam-test-acp is missing; build the fixture before running this scenario"
            )
        }
        let bin = paths.home.appendingPathComponent("fixture-bin", isDirectory: true)
        try FileManager.default.createDirectory(
            at: bin,
            withIntermediateDirectories: true
        )
        for command in ["node", "npx"] {
            let link = bin.appendingPathComponent(command)
            try FileManager.default.createSymbolicLink(
                at: link,
                withDestinationURL: executable
            )
        }
        try FileManager.default.createDirectory(
            at: paths.control.appendingPathComponent("acp", isDirectory: true),
            withIntermediateDirectories: true
        )
        if requirements.contains(.heldLoginShell) {
            let release = paths.control
                .appendingPathComponent("acp", isDirectory: true)
                .appendingPathComponent("release-login-shell")
            let shell = bin.appendingPathComponent("held-login-shell")
            try """
            #!/bin/sh
            while [ ! -f "\(release.path)" ]; do sleep 0.05; done
            printf '__ottam_node__:%s\\n' "\(bin.appendingPathComponent("node").path)"

            """.write(to: shell, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes(
                [.posixPermissions: 0o755],
                ofItemAtPath: shell.path
            )
        }
    }

    func decodeLaunch(_ result: [String: Any]) throws -> LaunchedApplication {
        let data = try JSONSerialization.data(withJSONObject: result)
        return try JSONDecoder().decode(LaunchedApplication.self, from: data)
    }

    func captureFailureEvidence(
        client: DriverClient,
        launched: LaunchedApplication,
        paths: RunPaths
    ) -> [String: Any] {
        let screenshotURL = paths.screenshots.appendingPathComponent("failure.png")
        let treeURL = paths.accessibility.appendingPathComponent("failure.json")
        var screenshotStatus = "unavailable"
        var semanticTreeStatus = "unavailable"
        var reasons: [String] = []

        do {
            let response = try client.invoke(
                command: "capture",
                parameters: [
                    "pid": launched.processIdentity.pid,
                    "outputPath": screenshotURL.path,
                ]
            )
            if response.status == "passed",
               FileManager.default.fileExists(atPath: screenshotURL.path)
            {
                screenshotStatus = "authoritative"
            } else {
                reasons.append(response.message ?? "failure screenshot capture failed")
            }
        } catch {
            reasons.append("failure screenshot capture failed: \(error)")
        }

        do {
            let response = try client.invoke(
                command: "semantic-tree",
                parameters: [
                    "pid": launched.processIdentity.pid,
                    "outputPath": treeURL.path,
                    "maxDepth": SemanticSnapshotPolicy.wholeApplicationDiagnostic.maxDepth,
                    "maxNodes": SemanticSnapshotPolicy.wholeApplicationDiagnostic.maxNodes,
                    "maxValueLength":
                        SemanticSnapshotPolicy.wholeApplicationDiagnostic.maxValueLength,
                    "timeout":
                        SemanticSnapshotPolicy.wholeApplicationDiagnostic.timeoutSeconds,
                ]
            )
            if response.status == "passed",
               FileManager.default.fileExists(atPath: treeURL.path)
            {
                semanticTreeStatus = "authoritative"
            } else {
                reasons.append(response.message ?? "failure semantic capture failed")
            }
        } catch {
            reasons.append("failure semantic capture failed: \(error)")
        }

        return [
            "captureID": "failure-postmortem",
            "screenshot": "screenshots/failure.png",
            "semanticTree": "accessibility/failure.json",
            "screenshotStatus": screenshotStatus,
            "semanticTreeStatus": semanticTreeStatus,
            "snapshotPolicy": (try? EvidenceWriter.object(
                SemanticSnapshotPolicy.wholeApplicationDiagnostic
            )) ?? [:],
            "reasons": reasons,
        ]
    }

    func collectLogs(launched: LaunchedApplication?, paths: RunPaths) {
        guard let launched else { return }
        let swiftLog = paths.logs.appendingPathComponent("swift.log")
        if let output = try? Command.output([
            "/usr/bin/log", "show", "--style", "compact", "--last", "10m",
            "--predicate", "processID == \(launched.processIdentity.pid)",
        ], timeout: 5) {
            let bounded = output
                .split(separator: "\n", omittingEmptySubsequences: false)
                .suffix(2_000)
                .joined(separator: "\n")
            try? Data(bounded.utf8).write(to: swiftLog, options: .atomic)
        }
        let backendLog = paths.logs.appendingPathComponent("backend.log")
        if !FileManager.default.fileExists(atPath: backendLog.path) {
            let candidates = (try? FileManager.default.contentsOfDirectory(
                at: paths.logs,
                includingPropertiesForKeys: nil
            )) ?? []
            let content = candidates
                .filter { $0 != swiftLog && $0 != paths.actions }
                .compactMap { try? String(contentsOf: $0, encoding: .utf8) }
                .joined(separator: "\n")
            try? Data(content.utf8).write(to: backendLog, options: .atomic)
        }
    }

    func manifestValue(
        scenario: UIScenarioDefinition,
        identity: IsolationIdentity,
        paths: RunPaths,
        staged: StagedApplication?,
        launched: LaunchedApplication?,
        started: Date,
        ended: Date,
        canonicalBefore: [[String: String]],
        canonicalAfter: [[String: String]],
        frontmostBefore: ForegroundIdentity?,
        frontmostAfter: ForegroundIdentity?,
        artifacts: [[String: Any]],
        intervals: [[String: Any]],
        cleanupVerified: Bool,
        cleanupDetails: [String: Any],
        observedChildren: Set<ProcessIdentity>,
        permissions: [String: Any],
        stagedApplicationRetained: Bool,
        evidenceCompaction: [String: Any],
        failures: RunFailureState
    ) -> [String: Any] {
        var manifest: [String: Any] = [
            "schemaVersion": 1,
            "worktreeID": identity.worktreeID,
            "runID": identity.runID,
            "scenarioID": scenario.descriptor.id,
            "worktreePath": identity.worktreePath,
            "bundleIdentifier": identity.bundleIdentifier,
            "displayName": identity.displayName,
            "validationProfile": scenario.launchConfiguration.validationProfile,
            "executionMode": scenario.launchConfiguration.executionMode.rawValue,
            "driverTopology": arguments.driverTopology.rawValue,
            "driverAppURL": arguments.driverApp,
            "paths": [
                "state": paths.home.path,
                "logs": paths.logs.path,
                "temporary": paths.temporary.path,
                "derivedData": paths.derivedData.path,
                "control": paths.control.path,
                "evidence": paths.root.path,
            ],
            "canonicalOttamBefore": canonicalBefore,
            "canonicalOttamAfter": canonicalAfter,
            "frontmostBefore": frontmostBefore?.dictionary ?? NSNull(),
            "frontmostAfter": frontmostAfter?.dictionary ?? NSNull(),
            "hostedForegroundAssertion": arguments.isHosted ? "not_scored" : "scored",
            "artifacts": artifacts,
            "actionIntervals": intervals,
            "exclusiveInputIntervals": [],
            "permissions": permissions,
            "fixtures": scenario.launchConfiguration.fixtures,
            "fixtureRequirements":
                scenario.launchConfiguration.fixtureRequirements.map(\.rawValue),
            "fixtureEndpoints": scenario.launchConfiguration.fixtureEndpoints,
            "redactionWarnings": scenario.launchConfiguration.redactionWarnings,
            "startedAt": iso(started),
            "endedAt": iso(ended),
            "durationMilliseconds": ended.timeIntervalSince(started) * 1_000,
            "cleanupVerified": cleanupVerified,
            "cleanup": cleanupDetails,
            "evidenceCompaction": evidenceCompaction,
            "failureState": [
                "primaryFailure": failureEvidence(failures.primaryFailure),
                "secondaryFailures": failures.secondaryFailures.map(failureEvidence),
                "effectiveClassification": failures.classification.rawValue,
            ],
            "exactCommand": publicCommandArguments(),
            "macOSVersion": ProcessInfo.processInfo.operatingSystemVersionString,
            "xcodeVersion": (try? Command.output([
                "/usr/bin/xcodebuild", "-version",
            ])) ?? "unavailable",
        ]
        manifest.merge(
            EvidenceWriter.gitMetadata(worktreeURL: URL(fileURLWithPath: identity.worktreePath))
        ) { _, new in new }
        if let staged {
            manifest["stagedApplication"] = [
                "url": staged.bundleURL.path,
                "bundleIdentifier": staged.bundleIdentifier,
                "executablePath": staged.executableURL.path,
                "executableSHA256": staged.executableSHA256,
                "retainedAfterCleanup": stagedApplicationRetained,
            ]
        }
        if let launched,
           let data = try? JSONEncoder().encode(launched),
           var object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        {
            let children = Set(launched.childProcesses)
                .union(observedChildren)
                .sorted {
                    $0.pid == $1.pid
                        ? $0.startMicroseconds < $1.startMicroseconds
                        : $0.pid < $1.pid
                }
                .map {
                    [
                        "pid": $0.pid,
                        "startSeconds": $0.startSeconds,
                        "startMicroseconds": $0.startMicroseconds,
                    ] as [String: Any]
                }
            object["childProcesses"] = children
            manifest["launchedApplication"] = object
        }
        return manifest
    }

    func mergeObservedChildren(
        from result: [String: Any],
        into observedChildren: inout Set<ProcessIdentity>
    ) {
        guard let value = result["observedChildProcesses"]
                ?? result["observedChildren"],
              JSONSerialization.isValidJSONObject(value),
              let data = try? JSONSerialization.data(withJSONObject: value),
              let identities = try? JSONDecoder().decode([ProcessIdentity].self, from: data)
        else {
            return
        }
        observedChildren.formUnion(identities)
    }
}
