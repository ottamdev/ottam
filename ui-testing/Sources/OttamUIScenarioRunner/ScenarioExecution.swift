import AppKit
import OttamAccessibility
import OttamUIScenarios
import Foundation
import MacAutomationKit
import ScenarioKit
final class ScenarioExecution {
    let arguments: RunnerArguments
    private let evidenceRoot: URL?

    init(arguments: RunnerArguments, evidenceRoot: URL? = nil) {
        self.arguments = arguments
        self.evidenceRoot = evidenceRoot
    }

    func run(_ scenario: UIScenarioDefinition) async -> ExecutionResult {
        let worktreeURL = URL(fileURLWithPath: arguments.root)
            .resolvingSymlinksInPath()
            .standardizedFileURL
        let identity: IsolationIdentity
        let paths: RunPaths
        do {
            let displayName = try Command.output([
                "/bin/bash",
                worktreeURL.appendingPathComponent("scripts/app-display-name.sh").path,
                "--root", worktreeURL.path,
                "--role", "test",
            ], timeout: 5).trimmingCharacters(in: .whitespacesAndNewlines)
            identity = try IsolationIdentity(worktreeURL: worktreeURL, displayName: displayName)
            paths = try RunPaths(
                worktreeURL: worktreeURL,
                identity: identity,
                evidenceRoot: evidenceRoot
            )
        } catch {
            FileHandle.standardError.write(Data("isolation failed: \(error)\n".utf8))
            return ExecutionResult(
                runDirectory: evidenceRoot?.path ?? arguments.root,
                exitCode: 1,
                classification: .isolationFailed,
                cleanupVerified: true
            )
        }

        let actions = ActionRecorder(url: paths.actions)
        let client = DriverClient(
            driverBundleURL: URL(fileURLWithPath: arguments.driverApp),
            requestsDirectory: paths.control,
            topology: arguments.driverTopology,
            actionRecorder: actions
        )
        let started = Date()
        let frontmostBefore = frontmostApplication()
        let canonicalBefore = canonicalApplicationIdentities()
        var staged: StagedApplication?
        var launched: LaunchedApplication?
        var coordinatorLease: ScenarioCoordinatorLease?
        var launchAttempted = false
        var observedChildren: Set<ProcessIdentity> = []
        var cleanupDetails: [String: Any] = [:]
        var artifactRecords: [[String: Any]] = []
        var intervals: [[String: Any]] = []
        var failures = RunFailureState()
        var failureMessage: String?
        var cleanupVerified = false
        var stagedApplicationRetained = true
        var permissionState: [String: Any] = [:]
        var runtimeContext: ScenarioRuntimeContext?
        var runtimeEvidenceMerged = false
        let retainInternals =
            ProcessInfo.processInfo.environment["OTTAM_UI_TEST_RETAIN_INTERNALS"] == "1"

        do {
            try actions.append(
                event: "run_started",
                fields: ["scenario": scenario.descriptor.id]
            )
            try EvidenceWriter.writeJSON(
                [
                    "schemaVersion": 1,
                    "id": scenario.descriptor.id,
                    "description": scenario.descriptor.description,
                    "implementation": "Swift ScenarioKit registry",
                    "tags": scenario.descriptor.tags,
                    "requirements": scenario.descriptor.requirements,
                ],
                to: paths.root.appendingPathComponent("scenario.json")
            )
            let preflight = try client.invoke(command: "preflight")
            permissionState = preflight.result
            guard preflight.status == "passed",
                  preflight.result["accessibilityAllowed"] as? Bool == true,
                  preflight.result["screenRecordingAllowed"] as? Bool == true,
                  preflight.result["screenLocked"] as? Bool == false,
                  preflight.result["onConsole"] as? Bool == true
            else {
                throw RunnerError.environmentUnavailable(
                    preflight.message
                        ?? "The UI test requires an unlocked console session and driver permissions."
                )
            }

            if scenario.launchConfiguration.executionMode == .exclusiveForeground {
                coordinatorLease = try ScenarioCoordinatorLease.acquire(
                    executable: arguments.coordinator,
                    identity: identity,
                    scenarioID: scenario.descriptor.id,
                    evidenceURL: paths.root
                )
                try actions.append(
                    event: "ui_automation_queue_acquired",
                    fields: ["scenario": scenario.descriptor.id]
                )
            }

            let stageStarted = Date()
            let sourceBundleURL = URL(fileURLWithPath: arguments.sourceApp)
            staged = try StagedApplication.stage(
                sourceBundleURL: sourceBundleURL,
                identity: identity,
                paths: paths
            )
            // Recording the source bundle keeps evidence traceable to the app
            // it actually came from, so a launcher that stages the wrong
            // bundle is visible in the record instead of only in the pixels.
            try actions.append(
                event: "application_staged",
                fields: [
                    "bundleIdentifier": staged!.bundleIdentifier,
                    "bundleURL": staged!.bundleURL.path,
                    "sourceBundleURL": sourceBundleURL.standardizedFileURL.path,
                    "executableSHA256": staged!.executableSHA256,
                ]
            )
            try coordinatorLease?.stage(staged!)
            intervals.append(interval("quiet", "stage isolated application", stageStarted, Date()))
            try prepareFixtures(
                scenario: scenario,
                worktreeURL: worktreeURL,
                paths: paths
            )

            let launchStarted = Date()
            let environment = launchEnvironment(
                scenario: scenario,
                identity: identity,
                paths: paths
            )
            launchAttempted = true
            let launch = try client.invoke(
                command: "launch",
                parameters: [
                    "bundleURL": staged!.bundleURL.path,
                    "environment": environment,
                    "arguments": [],
                    "activates":
                        scenario.launchConfiguration.executionMode == .exclusiveForeground,
                ]
            )
            guard launch.status == "passed" else {
                throw scenarioDriverFailure(launch, fallback: "launch failed")
            }
            launched = try decodeLaunch(launch.result)
            try coordinatorLease?.attach(launched!, application: staged!)
            observedChildren.formUnion(launched!.childProcesses)
            intervals.append(interval("quiet", "background application launch", launchStarted, Date()))
            try actions.append(
                event: "application_launched",
                fields: [
                    "pid": launched!.processIdentity.pid,
                    "processIdentity": launched!.processIdentity.stableValue,
                ]
            )

            let context = ScenarioRuntimeContext(
                client: client,
                launched: launched!,
                paths: paths,
                actions: actions
            )
            runtimeContext = context
            try await scenario.run(in: context)
            artifactRecords = context.artifactRecords
            intervals.append(contentsOf: context.intervals)
            observedChildren.formUnion(context.observedChildren)
            runtimeEvidenceMerged = true

            if let launched {
                observedChildren.formUnion(
                    ProcessIdentity.descendants(of: launched.processIdentity.pid)
                )
                if let coordinatorLease {
                    cleanupDetails = try coordinatorLease.cleanupApplication()
                    mergeObservedChildren(from: cleanupDetails, into: &observedChildren)
                    guard cleanupDetails["exited"] as? Bool == true,
                          cleanupDetails["childrenExited"] as? Bool == true
                    else {
                        throw ScenarioRuntimeError.cleanupFailed(
                            "coordinator could not clean the isolated application"
                        )
                    }
                } else {
                    let terminate = try client.invoke(
                        command: "terminate",
                        parameters: identityParameters(
                            launched.processIdentity,
                            knownDescendants: observedChildren
                        )
                    )
                    cleanupDetails = terminate.result
                    mergeObservedChildren(from: terminate.result, into: &observedChildren)
                    guard terminate.status == "passed",
                          terminate.result["exited"] as? Bool == true,
                          terminate.result["childrenExited"] as? Bool == true
                    else {
                        throw ScenarioRuntimeError.cleanupFailed(
                            terminate.message
                                ?? "driver could not clean the isolated application"
                        )
                    }
                }
                cleanupVerified =
                    ProcessIdentity.forPID(launched.processIdentity.pid)
                    != launched.processIdentity
                    && observedChildren.allSatisfy {
                        ProcessIdentity.forPID($0.pid) != $0
                    }
            }

            let frontmostAfter = frontmostApplication()
            if !arguments.isHosted,
               let frontmostBefore,
               frontmostBefore != frontmostAfter
            {
                throw RunnerError.foregroundChanged(frontmostBefore, frontmostAfter)
            }
            if canonicalBefore != canonicalApplicationIdentities() {
                throw RunnerError.canonicalChanged
            }
        } catch {
            if let runtimeContext, !runtimeEvidenceMerged {
                artifactRecords = runtimeContext.artifactRecords
                intervals.append(contentsOf: runtimeContext.intervals)
                observedChildren.formUnion(runtimeContext.observedChildren)
                runtimeEvidenceMerged = true
            }
            failureMessage = String(describing: error)
            failures.recordPrimary(runFailure(for: error, phase: "execution"))
            do {
                try actions.append(
                    event: "run_failed",
                    fields: [
                        "classification": failures.classification.rawValue,
                        "message": failureMessage ?? "",
                    ]
                )
            } catch {
                recordEvidenceFailure(
                    &failures,
                    phase: "action-log",
                    error: error
                )
            }
            if let launched {
                let appIsRunning =
                    ProcessIdentity.forPID(launched.processIdentity.pid)
                    == launched.processIdentity
                if appIsRunning {
                    artifactRecords.append(
                        captureFailureEvidence(
                            client: client,
                            launched: launched,
                            paths: paths
                        )
                    )
                } else {
                    artifactRecords.append([
                        "captureID": "failure-postmortem",
                        "screenshotStatus": "unavailable",
                        "semanticTreeStatus": "unavailable",
                        "reason": "the isolated application was no longer running",
                    ])
                }
                if let coordinatorLease {
                    cleanupDetails = (try? coordinatorLease.cleanupApplication()) ?? [:]
                    mergeObservedChildren(from: cleanupDetails, into: &observedChildren)
                    cleanupVerified = cleanupDetails["exited"] as? Bool == true
                        && cleanupDetails["childrenExited"] as? Bool == true
                } else {
                    let terminate = try? client.invoke(
                        command: "terminate",
                        parameters: identityParameters(
                            launched.processIdentity,
                            knownDescendants: observedChildren
                        )
                    )
                    cleanupDetails = terminate?.result ?? [:]
                    if let terminate {
                        mergeObservedChildren(from: terminate.result, into: &observedChildren)
                    }
                    cleanupVerified = terminate?.status == "passed"
                        && terminate?.result["exited"] as? Bool == true
                        && terminate?.result["childrenExited"] as? Bool == true
                }
                cleanupVerified = cleanupVerified
                    && ProcessIdentity.forPID(launched.processIdentity.pid)
                    != launched.processIdentity
                    && observedChildren.allSatisfy {
                        ProcessIdentity.forPID($0.pid) != $0
                    }
                if !cleanupVerified {
                    failures.recordSecondary(
                        RunFailure(
                            classification: .cleanupFailed,
                            phase: "cleanup",
                            message: "exact application or child cleanup was not verified"
                        )
                    )
                }
            } else if launchAttempted, let staged {
                if let coordinatorLease {
                    cleanupDetails = (try? coordinatorLease.cleanupApplication()) ?? [:]
                    cleanupVerified = cleanupDetails["exited"] as? Bool == true
                        && cleanupDetails["childrenExited"] as? Bool == true
                } else {
                    let exactMatches = ProcessIdentity.running(
                        executableURL: staged.executableURL
                    )
                    if exactMatches.count == 1 {
                        let recovered = exactMatches[0]
                        let children = Set(ProcessIdentity.descendants(of: recovered.pid))
                        let terminated = try? await ApplicationLifecycle.terminate(
                            identity: recovered,
                            knownDescendants: Array(children)
                        )
                        observedChildren.formUnion(children)
                        observedChildren.formUnion(terminated ?? [])
                        cleanupVerified = ProcessIdentity.forPID(recovered.pid) != recovered
                            && observedChildren.allSatisfy {
                                ProcessIdentity.forPID($0.pid) != $0
                            }
                        cleanupDetails = [
                            "status": cleanupVerified
                                ? "recovered_launch_cleaned"
                                : "recovered_launch_cleanup_failed",
                            "pid": recovered.pid,
                        ]
                    } else {
                        cleanupVerified = exactMatches.isEmpty
                        cleanupDetails = [
                            "status": exactMatches.isEmpty
                                ? "launch_not_observed"
                                : "multiple_exact_launch_matches",
                            "matchCount": exactMatches.count,
                        ]
                    }
                }
                if !cleanupVerified {
                    failures.recordSecondary(
                        RunFailure(
                            classification: .cleanupFailed,
                            phase: "cleanup",
                            message:
                                "launch was attempted but exact application cleanup was not verified"
                        )
                    )
                }
            }
            try? Data((failureMessage ?? "unknown failure").utf8).write(
                to: paths.diagnostics.appendingPathComponent("failure.txt"),
                options: .atomic
            )
        }

        if !launchAttempted {
            cleanupVerified = true
        }
        if let coordinatorLease {
            do {
                try coordinatorLease.release()
                try actions.append(
                    event: "ui_automation_queue_released",
                    fields: ["scenario": scenario.descriptor.id]
                )
            } catch {
                failures.recordSecondary(
                    RunFailure(
                        classification: .cleanupFailed,
                        phase: "queue-release",
                        message: String(describing: error)
                    )
                )
                cleanupVerified = false
            }
        }
        let ended = Date()
        collectLogs(launched: launched, paths: paths)
        let canonicalAfter = canonicalApplicationIdentities()
        let frontmostAfter = frontmostApplication()
        if canonicalBefore != canonicalAfter {
            failures.recordSecondary(
                RunFailure(
                    classification: .isolationFailed,
                    phase: "postflight",
                    message: String(describing: RunnerError.canonicalChanged)
                )
            )
            failureMessage = [
                failureMessage,
                String(describing: RunnerError.canonicalChanged),
            ].compactMap { $0 }.joined(separator: "; ")
        } else if !arguments.isHosted,
                  let frontmostBefore,
                  frontmostBefore != frontmostAfter
        {
            failures.recordSecondary(
                RunFailure(
                    classification: .failedAssertion,
                    phase: "postflight",
                    message: String(
                        describing: RunnerError.foregroundChanged(
                            frontmostBefore,
                            frontmostAfter
                        )
                    )
                )
            )
            failureMessage = [
                failureMessage,
                String(
                    describing: RunnerError.foregroundChanged(
                        frontmostBefore,
                        frontmostAfter
                    )
                ),
            ].compactMap { $0 }.joined(separator: "; ")
        }
        if cleanupVerified, let staged, !retainInternals {
            do {
                try staged.removeAfterVerifiedCleanup(paths: paths)
                stagedApplicationRetained = false
            } catch {
                failures.recordSecondary(
                    RunFailure(
                        classification: .cleanupFailed,
                        phase: "staged-application-cleanup",
                        message: String(describing: error)
                    )
                )
                cleanupVerified = false
                failureMessage = [
                    failureMessage,
                    "staged application cleanup failed: \(error)",
                ].compactMap { $0 }.joined(separator: "; ")
            }
        }
        if !retainInternals {
            for directory in [
                RunPaths.TransientDirectory.temporary,
                .derivedData,
            ] {
                do {
                    guard try paths.removeTransientDirectory(directory) else {
                        throw RunnerError.evidenceCompactionFailed(directory.rawValue)
                    }
                } catch {
                    failures.recordSecondary(
                        RunFailure(
                            classification: .cleanupFailed,
                            phase: "evidence-compaction",
                            message: String(describing: error)
                        )
                    )
                    cleanupVerified = false
                    failureMessage = [
                        failureMessage,
                        "transient evidence cleanup failed: \(error)",
                    ].compactMap { $0 }.joined(separator: "; ")
                }
            }
            if staged == nil || !stagedApplicationRetained {
                do {
                    guard try paths.removeTransientDirectory(
                        .stagedApplications,
                        onlyIfEmpty: true
                    ) else {
                        throw RunnerError.evidenceCompactionFailed(
                            RunPaths.TransientDirectory.stagedApplications.rawValue
                        )
                    }
                } catch {
                    failures.recordSecondary(
                        RunFailure(
                            classification: .cleanupFailed,
                            phase: "evidence-compaction",
                            message: String(describing: error)
                        )
                    )
                    cleanupVerified = false
                    failureMessage = [
                        failureMessage,
                        "staged evidence directory cleanup failed: \(error)",
                    ].compactMap { $0 }.joined(separator: "; ")
                }
            }
            do {
                _ = try paths.removeTransientDirectory(
                    .diagnostics,
                    onlyIfEmpty: true
                )
            } catch {
                failures.recordSecondary(
                    RunFailure(
                        classification: .cleanupFailed,
                        phase: "evidence-compaction",
                        message: String(describing: error)
                    )
                )
                cleanupVerified = false
                failureMessage = [
                    failureMessage,
                    "empty diagnostics cleanup failed: \(error)",
                ].compactMap { $0 }.joined(separator: "; ")
            }
        }
        let evidenceCompaction: [String: Any] = [
            "retainInternalsRequested": retainInternals,
            "temporaryRetainedAfterCleanup": paths.transientDirectoryExists(.temporary),
            "stagedDirectoryRetainedAfterCleanup":
                paths.transientDirectoryExists(.stagedApplications),
            "derivedDataRetainedAfterCleanup":
                paths.transientDirectoryExists(.derivedData),
            "diagnosticsRetainedAfterCleanup":
                paths.transientDirectoryExists(.diagnostics),
        ]
        do {
            try actions.append(
                event: "run_finished",
                fields: [
                    "classification": failures.classification.rawValue,
                    "cleanupVerified": cleanupVerified,
                ]
            )
        } catch {
            recordEvidenceFailure(
                &failures,
                phase: "action-log",
                error: error
            )
        }

        func resultValue() -> [String: Any] {
            [
                "schemaVersion": 1,
                "classification": failures.classification.rawValue,
                "message": failureMessage ?? NSNull(),
                "primaryFailure": failureEvidence(failures.primaryFailure),
                "secondaryFailures": failures.secondaryFailures.map(failureEvidence),
                "startedAt": iso(started),
                "endedAt": iso(ended),
                "durationMilliseconds": ended.timeIntervalSince(started) * 1_000,
                "cleanupVerified": cleanupVerified,
            ]
        }

        func finalManifest() -> [String: Any] {
            manifestValue(
                scenario: scenario,
                identity: identity,
                paths: paths,
                staged: staged,
                launched: launched,
                started: started,
                ended: ended,
                canonicalBefore: canonicalBefore,
                canonicalAfter: canonicalAfter,
                frontmostBefore: frontmostBefore,
                frontmostAfter: frontmostAfter,
                artifacts: artifactRecords,
                intervals: intervals,
                cleanupVerified: cleanupVerified,
                cleanupDetails: cleanupDetails,
                observedChildren: observedChildren,
                permissions: permissionState,
                stagedApplicationRetained: stagedApplicationRetained,
                evidenceCompaction: evidenceCompaction,
                failures: failures
            )
        }

        do {
            try EvidenceWriter.writeJSON(
                finalManifest(),
                to: paths.root.appendingPathComponent("manifest.json")
            )
            try EvidenceWriter.writeJSON(
                resultValue(),
                to: paths.root.appendingPathComponent("result.json")
            )
        } catch {
            recordEvidenceFailure(
                &failures,
                phase: "finalization",
                error: error
            )
            try? EvidenceWriter.writeJSON(
                finalManifest(),
                to: paths.root.appendingPathComponent("manifest.json")
            )
            try? EvidenceWriter.writeJSON(
                resultValue(),
                to: paths.root.appendingPathComponent("result.json")
            )
        }
        let classification = failures.classification
        return ExecutionResult(
            runDirectory: paths.root.path,
            exitCode: classification == .passed ? 0 : 1,
            classification: classification,
            cleanupVerified: cleanupVerified
        )
    }
}
