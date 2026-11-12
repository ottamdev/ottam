import OttamBackendClient
import OttamCore
import Foundation

@main
struct OttamBackendClientTestRunner {
    static func main() async {
        do {
            try ProtocolFixtureChecks().run()
            try FixtureGateChecks().run()
            try ChatAttachmentIntakeChecks().run()
            try await AsyncJSONLConnectionChecks().run()
            try await immediateProcessExitPreservesStructuredError()
            try await RustProcessIntegrationChecks().runIfRequested()
            print("OttamBackendClientTests passed")
        } catch {
            fputs("OttamBackendClientTests failed: \(error)\n", stderr)
            exit(EXIT_FAILURE)
        }
    }
}

private func immediateProcessExitPreservesStructuredError() async throws {
    let directory = FileManager.default.temporaryDirectory
        .appendingPathComponent("ottam-exit-error-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: directory) }
    let executable = directory.appendingPathComponent("core-exit-fixture")
    let script = #"""
    #!/bin/sh
    printf '%s\n' '{"method":"core.exitError","params":{"kind":"storageMigrationFailed","message":"Incompatible database","details":"Back up first."}}'
    exit 1
    """#
    try Data(script.utf8).write(to: executable)
    try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: executable.path)
    try await withEnvironment("OTTAM_LOG_DIR", value: directory.path) {
        // Repeated short-lived children exercise termination-vs-stdout ordering.
        for _ in 0..<10 {
            let connection = LocalProcessConnection(executableURL: executable)
            do {
                _ = try await connection.initializeResult()
                throw TestFailure("Expected startup process failure")
            } catch AgentConnectionError.coreExited(let error) {
                try expect(error.kind == "storageMigrationFailed", "process exit kind lost")
                try expect(error.details == "Back up first.", "process exit details lost")
            }
            await connection.close()
        }
    }
}

private struct RustProcessIntegrationChecks {
    func runIfRequested() async throws {
        guard let corePath = ProcessInfo.processInfo.environment["OTTAM_BACKEND_CLIENT_INTEGRATION_CORE"],
              !corePath.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return
        }

        let coreURL = URL(fileURLWithPath: corePath).standardized
        let repoRoot = try repositoryRoot()
        let ottamHome = FileManager.default.temporaryDirectory
            .appendingPathComponent("ottam-backend-client-tests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: ottamHome, withIntermediateDirectories: true)
        defer {
            try? FileManager.default.removeItem(at: ottamHome)
        }

        try await withEnvironment("OTTAM_HOME", value: ottamHome.path) {
            let client = OttamCoreProcessClient(executableURL: coreURL)
            let openedProject = try await client.openProject(path: repoRoot.path)
            try expect(openedProject.source.canonicalPath == repoRoot.path, "opened project root mismatch")
            let initialized = try await client.initializeResult()
            let workspaces = try await client.listWorkspaces(includeArchived: true)
            try expect(
                workspaces.workspaces.contains {
                    $0.workspace.workspaceID == openedProject.createdWorkspace?.workspaceID
                        && $0.projectPaths == [openedProject.source.canonicalPath]
                },
                "real process Workspace list should contain the opened Project's default Workspace and its path"
            )
            let repeatedInitialize = try await client.initializeResult()
            try expect(
                repeatedInitialize.coreInstanceID == initialized.coreInstanceID,
                "one client should retain one Core instance generation"
            )

            async let first = client.diffSnapshot(projectSourceID: openedProject.source.projectSourceID)
            async let second = client.diffSnapshot(projectSourceID: openedProject.source.projectSourceID)
            let snapshots = try await [first, second]
            try expect(snapshots.count == 2, "real process should return both concurrent snapshots")
            try expect(snapshots.allSatisfy { $0.repoPath == repoRoot.path }, "snapshot repo path mismatch")

            let agents = try await client.listAgents()
            try expect(Set(agents.agents.map(\.agentID.rawValue)) == Set(["claude", "codex", "opencode", "omp"]), "real process agent list mismatch")
        }

        // Cold-start concurrency: simultaneous first requests must share one core
        // spawn. A second spawn would lose the OTTAM_HOME lock race and orphan
        // the winner, wedging every later spawn with coreAlreadyRunning.
        let raceHome = FileManager.default.temporaryDirectory
            .appendingPathComponent("ottam-backend-client-race-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: raceHome, withIntermediateDirectories: true)
        defer {
            try? FileManager.default.removeItem(at: raceHome)
        }

        // Spawns are counted by trace-log files. The client puts those under
        // OTTAM_LOG_DIR when it is set (an app-hosted shell inherits the
        // app instance's), so pin it to the race home's own logs directory
        // rather than assuming the OTTAM_HOME fallback.
        let logsDirectory = raceHome.appendingPathComponent("logs")
        try await withEnvironment("OTTAM_HOME", value: raceHome.path) {
            try await withEnvironment("OTTAM_LOG_DIR", value: logsDirectory.path) {
                let raceClient = OttamCoreProcessClient(executableURL: coreURL)
                async let first = raceClient.listWorkspaces(includeArchived: true)
                async let second = raceClient.listWorkspaces(includeArchived: true)
                async let third = raceClient.listAgents()
                _ = try await (first, second, third)
                let initialized = try await raceClient.initializeResult()
                _ = initialized

                let coreLogs = ((try? FileManager.default.contentsOfDirectory(atPath: logsDirectory.path)) ?? [])
                    .filter { $0.hasPrefix("core-") && !$0.contains("stderr") }
                try expect(
                    coreLogs.count == 1,
                    "concurrent cold-start requests should spawn exactly one core, spawned \(coreLogs.count)"
                )
            }
        }
    }
}

private struct ProtocolFixtureChecks {
    private let decoder = JSONDecoder()

    func run() throws {
        try initializeResponseFixtureDecodesWithoutJSONRPC()
        try initializedNotificationFixtureDecodes()
        try gitDiffSnapshotResponseFixtureDecodesIntoCoreModel()
        try agentFixturesDecode()
        try conversationRunFixturesDecode()
        try modelSelectionRoundTrips()
        try responseIDVariantsDecode()
        try responseRejectsBothResultAndError()
        try responseRejectsMissingResultAndError()
    }

    private func conversationRunFixturesDecode() throws {
        let start = try decoder.decode(
            ProtocolResponse<ConversationStartRunResult>.self,
            from: fixtureData("conversation-start-run-response.json")
        )
        try expect(start.result?.run.status == .running, "conversation run status mismatch")
        try expect(
            start.result?.input.status == .sending,
            "conversation start input status mismatch"
        )

        let cancel = try decoder.decode(
            ProtocolResponse<ConversationCancelRunResult>.self,
            from: fixtureData("conversation-cancel-run-response.json")
        )
        try expect(cancel.result?.run.runID == ConversationRunID("conversation_run_fixture"), "conversation cancel run mismatch")

        let deleted = try decoder.decode(
            ProtocolResponse<ConversationDeleteResult>.self,
            from: fixtureData("conversation-delete-response.json")
        )
        try expect(
            deleted.result?.harnessSessionCleanup.first?.status == .deleted,
            "conversation harness cleanup mismatch"
        )

        let event = try decoder.decode(
            ProtocolNotification<ConversationEventDeltaParams>.self,
            from: fixtureData("conversation-event-delta-notification.json")
        )
        try expect(
            event.params.projectID == ProjectID("019b0000-0000-7000-8000-000000000001"),
            "conversation event project mismatch"
        )
        try expect(event.params.runID == ConversationRunID("conversation_run_fixture"), "conversation event run mismatch")
        try expect(event.params.eventKind == .runStatus, "conversation event kind mismatch")
        let transcript = try decoder.decode(ProtocolResponse<ConversationListTranscriptResult>.self,
            from: fixtureData("conversation-list-transcript-response.json"))
        let delta = try decoder.decode(ProtocolNotification<ConversationTranscriptDeltaParams>.self,
            from: fixtureData("conversation-transcript-delta-notification.json"))
        let body = try decoder.decode(ProtocolResponse<ConversationReadTranscriptBodyResult>.self,
            from: fixtureData("conversation-read-transcript-body-response.json"))
        try expect(transcript.result?.items == delta.params.items, "snapshot and delta item identity mismatch")
        try expect(body.result?.revision == delta.params.items.first?.revision, "body revision mismatch")
    }

    private func modelSelectionRoundTrips() throws {
        // Nil modelID must omit the key on the wire: older core binaries decode
        // startRun params with deny_unknown_fields and reject a null there.
        let params = ConversationStartRunParams(
            workspaceID: WorkspaceID("workspace_fixture"),
            conversationID: ConversationID("conversation_fixture"),
            engineID: EngineID("claude"),
            runInputID: ConversationRunInputID("run_input_fixture")
        )
        let encoded = try JSONEncoder().encode(params)
        let object = try JSONSerialization.jsonObject(with: encoded) as? [String: Any]
        try expect(object?["modelID"] == nil, "nil modelID should be omitted from encoded params")

        var withModel = params
        withModel.modelID = "haiku"
        let encodedWithModel = try JSONEncoder().encode(withModel)
        let objectWithModel = try JSONSerialization.jsonObject(with: encodedWithModel) as? [String: Any]
        try expect(objectWithModel?["modelID"] as? String == "haiku", "modelID should encode when set")

        let fixtureEngines = try decoder.decode(
            ProtocolResponse<EngineListResult>.self,
            from: fixtureData("engine-list-response.json")
        )
        let fixtureEngine = fixtureEngines.result?.engines.first
        try expect(fixtureEngine?.models.isEmpty == false, "engine fixture should carry model options")
        try expect(fixtureEngine?.models.first?.modelID == "gpt-5", "engine fixture model id mismatch")
        try expect(fixtureEngine?.defaultModelID == "gpt-5", "engine fixture default model mismatch")
        try expect(
            fixtureEngine?.authMethods.first?.methodID == "chat-gpt",
            "engine fixture should carry the sign-in the picker offers"
        )
    }

    private func agentFixturesDecode() throws {
        let list = try decoder.decode(
            ProtocolResponse<AgentListResult>.self,
            from: fixtureData("agent-list-response.json")
        )
        try expect(list.result?.agents.first?.agentID == AgentID("codex"), "agent list fixture mismatch")
        try expect(list.result?.agents.first?.accountKind == .subscription, "agent account kind mismatch")
        try expect(list.result?.agents.first?.brand == "openai", "agent brand mismatch")

        let permissionRequest = try decoder.decode(
            AgentPermissionFixtureRequest.self,
            from: fixtureData("agent-request-permission-request.json")
        )
        try expect(permissionRequest.params.runID == ConversationRunID("conversation_run_fixture"), "permission run context mismatch")

        let permission = try decoder.decode(
            ProtocolResponse<AgentPermissionOutcome>.self,
            from: fixtureData("agent-request-permission-response.json")
        )
        try expect(permission.result == .selected(optionID: "allow_once"), "permission outcome mismatch")

        let questionRequest = try decoder.decode(
            AgentQuestionFixtureRequest.self,
            from: fixtureData("agent-ask-question-request.json")
        )
        try expect(questionRequest.params.questions.count == 2, "question count mismatch")
        try expect(
            questionRequest.params.questions.last?.kind == .text(secure: true, default: nil),
            "secure question kind mismatch"
        )
        let answer = try decoder.decode(
            ProtocolResponse<AgentQuestionOutcome>.self,
            from: fixtureData("agent-ask-question-response.json")
        )
        guard case .submitted(let answers)? = answer.result else {
            throw TestFailure("question outcome should be submitted")
        }
        // The hand-built wire value and Codable agree on every outcome.
        for outcome in [answer.result!, .skipped, .cancelled] {
            let encoded = try JSONEncoder().encode(outcome.jsonValue)
            try expect(
                try decoder.decode(AgentQuestionOutcome.self, from: encoded) == outcome,
                "question outcome wire value mismatch"
            )
        }
        try expect(!answers.isEmpty, "question answers missing")
    }

    private func initializeResponseFixtureDecodesWithoutJSONRPC() throws {
        let response = try decoder.decode(
            ProtocolResponse<InitializeResult>.self,
            from: fixtureData("initialize-response.json")
        )

        try expect(response.jsonrpc == nil, "initialize response should omit jsonrpc")
        try expect(response.id == .number(1), "initialize response id should be numeric")
        try expect(response.result?.serverInfo.name == "ottam-core", "server name mismatch")
    }

    private func initializedNotificationFixtureDecodes() throws {
        let notification = try decoder.decode(
            ProtocolNotification<InitializedParams>.self,
            from: fixtureData("initialized-notification.json")
        )

        try expect(notification.jsonrpc == nil, "initialized notification should omit jsonrpc")
        try expect(notification.method == "initialized", "initialized method mismatch")
    }

    private func gitDiffSnapshotResponseFixtureDecodesIntoCoreModel() throws {
        let response = try decoder.decode(
            ProtocolResponse<CoreDiffSnapshot>.self,
            from: fixtureData("git-diff-snapshot-response.json")
        )

        let snapshot = try require(response.result, "git diff fixture should contain result")
        try expect(response.id == .number(3), "git diff response id mismatch")
        try expect(snapshot.repoPath == "/Users/example/Developer/ottam", "repo path mismatch")
        try expect(snapshot.files.first?.path == "README.md", "fixture file path mismatch")
        try expect(snapshot.files.first?.hunks.first?.lines.count == 2, "fixture line count mismatch")
    }

    private func responseIDVariantsDecode() throws {
        let numeric = try decoder.decode(
            ProtocolResponse<TestPayload>.self,
            from: Data(#"{"id":7,"result":{"value":"number"}}"#.utf8)
        )
        let string = try decoder.decode(
            ProtocolResponse<TestPayload>.self,
            from: Data(#"{"id":"abc","result":{"value":"string"}}"#.utf8)
        )
        let null = try decoder.decode(
            ProtocolResponse<TestPayload>.self,
            from: Data(#"{"id":null,"error":{"code":-32600,"message":"bad"}}"#.utf8)
        )

        try expect(numeric.id == .number(7), "numeric id did not decode")
        try expect(string.id == .string("abc"), "string id did not decode")
        try expect(null.id == .null, "null id did not decode")
        try expect(null.error?.message == "bad", "null-id error did not decode")
    }

    private func responseRejectsBothResultAndError() throws {
        try expectThrows("response with both result and error should fail") {
            _ = try decoder.decode(
                ProtocolResponse<TestPayload>.self,
                from: Data(#"{"id":1,"result":{"value":"ok"},"error":{"code":-32000,"message":"bad"}}"#.utf8)
            )
        }
    }

    private func responseRejectsMissingResultAndError() throws {
        try expectThrows("response without result or error should fail") {
            _ = try decoder.decode(
                ProtocolResponse<TestPayload>.self,
                from: Data(#"{"id":1}"#.utf8)
            )
        }
    }

    private func fixtureData(_ name: String) throws -> Data {
        let repoRoot = try repositoryRoot()
        let fixtureURL = repoRoot
            .appendingPathComponent("backend/crates/ottam-protocol/fixtures")
            .appendingPathComponent(name)
        return try Data(contentsOf: fixtureURL)
    }
}

private struct AgentQuestionFixtureRequest: Decodable {
    var params: AgentAskQuestionParams
}

private struct AgentPermissionFixtureRequest: Decodable {
    var params: AgentRequestPermissionParams
}

private struct AsyncJSONLConnectionChecks {
    func run() async throws {
        try await routesOutOfOrderResponsesByRequestID()
        try await openProjectRequestEncodesPath()
        try await openProjectFailureDoesNotIssueDiffRequest()
        try await projectSourceIDDiffConvenienceRoutesOutOfOrderResponses()
        try await terminalRequestsEncodeExpectedMethods()
        try await terminalNotificationsRouteToHandler()
        try await notificationsRemainOrderedAcrossAsyncHandlers()
        try await coreOriginatedRequestDoesNotBlockPendingResponse()
        try await coreOriginatedPermissionDefaultsToCancellation()
        try await coreWithdrawalCancelsTheQuestionHandler()
        try await concurrentConversationStartAndCancelRouteIndependently()
        try await conversationEventFiltersUseProjectConversationAndRun()
        try await clientResponseFailureClosesConnectionAndFailsPendingRequests()
        try terminalOutputDeliveryMetadataDecodes()
        try terminalReplayMetadataDecodes()
        try await rpcErrorDataDecodesAndPreservesMessage()
        try await oversizedResponseFailsPendingRequest()
        try await transportTerminationFailsPendingRequest()
        try await structuredExitErrorSurvivesEOF()
        try await malformedExitErrorFallsBackToTermination()
        try await cancelledRequestReturnsWithoutClosingLiveTransport()
    }

    private func cancelledRequestReturnsWithoutClosingLiveTransport() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport)
        let pending = Task<TestPayload, Error> {
            try await connection.request(method: "agent.prepareShutdown", params: EmptyParams())
        }
        _ = await transport.waitForWriteCount(1)
        pending.cancel()
        do {
            _ = try await pending.value
            throw TestFailure("A cancelled request must return without a core response")
        } catch is CancellationError {}
        let active = await connection.isActive
        try expect(active, "Cancellation must retain the live transport")
        await transport.enqueueLine(#"{"id":1,"result":{"value":"late"}}"#)
        let next = Task<TestPayload, Error> {
            try await connection.request(method: "test.afterCancel", params: EmptyParams())
        }
        _ = await transport.waitForWriteCount(2)
        await transport.enqueueLine(#"{"id":2,"result":{"value":"ready"}}"#)
        let result = try await next.value
        try expect(result.value == "ready", "A late reply must not consume the next request")
        await connection.close()
    }

    private func coreOriginatedRequestDoesNotBlockPendingResponse() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 4_096)
        await connection.setClientRequestHandler { request in
            guard request.method == "agent.requestPermission" else {
                return .failure(AgentProtocolError(code: -32601, message: "method not found"))
            }
            return .success(.object([
                "outcome": .string("selected"),
                "optionID": .string("allow_once")
            ]))
        }

        let pending = Task<TestPayload, Error> {
            try await connection.request(method: "test.pending", params: EmptyParams())
        }
        let firstWrite = await transport.waitForWriteCount(1)
        let request = try JSONDecoder().decode(RequestEnvelope.self, from: trimmed(firstWrite[0]))
        await transport.enqueueLine(
            #"{"id":"core/req/1","method":"agent.requestPermission","params":{"agentID":"codex","sessionID":"session_fixture","toolCall":{},"options":[{"optionID":"allow_once","name":"Allow once","kind":"allowOnce"}]}}"#
        )

        let writes = await transport.waitForWriteCount(2)
        let callback = try JSONDecoder().decode(
            ProtocolResponse<AgentPermissionOutcome>.self,
            from: trimmed(writes[1])
        )
        try expect(callback.id == .string("core/req/1"), "core request response id mismatch")
        try expect(callback.result == .selected(optionID: "allow_once"), "core request response result mismatch")

        await transport.enqueueLine(responseLine(id: request.id, value: "ok"))
        let pendingValue = try await pending.value
        try expect(pendingValue == TestPayload(value: "ok"), "pending response was blocked")
        await connection.close()
    }

    private func coreOriginatedPermissionDefaultsToCancellation() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 4_096)
        let pending = Task<TestPayload, Error> {
            try await connection.request(method: "test.pending", params: EmptyParams())
        }
        let writes = await transport.waitForWriteCount(1)
        let request = try JSONDecoder().decode(RequestEnvelope.self, from: trimmed(writes[0]))
        await transport.enqueueLine(
            #"{"id":"core/req/default","method":"agent.requestPermission","params":{"agentID":"codex","sessionID":"session_fixture","toolCall":{},"options":[{"optionID":"allow_once","name":"Allow once","kind":"allowOnce"}]}}"#
        )

        let responses = await transport.waitForWriteCount(2)
        let callback = try JSONDecoder().decode(
            ProtocolResponse<AgentPermissionOutcome>.self,
            from: trimmed(responses[1])
        )
        try expect(callback.result == .cancelled, "permission without a handler should cancel")

        await transport.enqueueLine(responseLine(id: request.id, value: "ok"))
        _ = try await pending.value
        await connection.close()
    }

    /// `$/cancelRequest` cancels the handler's task, which answers nothing:
    /// core no longer waits on it.
    private func coreWithdrawalCancelsTheQuestionHandler() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 4_096)
        let started = AsyncStream.makeStream(of: Void.self)
        let withdrawn = AsyncStream.makeStream(of: Void.self)
        await connection.setClientRequestHandler { _ in
            started.continuation.yield()
            await withTaskCancellationHandler {
                while !Task.isCancelled {
                    try? await Task.sleep(for: .milliseconds(5))
                }
            } onCancel: {
                withdrawn.continuation.yield()
            }
            return .success(AgentQuestionOutcome.cancelled.jsonValue)
        }
        let pending = Task<TestPayload, Error> {
            try await connection.request(method: "test.pending", params: EmptyParams())
        }
        let writes = await transport.waitForWriteCount(1)
        let request = try JSONDecoder().decode(RequestEnvelope.self, from: trimmed(writes[0]))
        await transport.enqueueLine(
            try fixtureLine("agent-ask-question-request.json")
        )
        var startedIterator = started.stream.makeAsyncIterator()
        _ = await startedIterator.next()
        await transport.enqueueLine(
            try fixtureLine("cancel-request-notification.json")
        )
        var withdrawnIterator = withdrawn.stream.makeAsyncIterator()
        _ = await withdrawnIterator.next()

        await transport.enqueueLine(responseLine(id: request.id, value: "ok"))
        _ = try await pending.value
        try await Task.sleep(for: .milliseconds(50))
        let written = await transport.writtenLineCount()
        try expect(written == 1, "a withdrawn question must not be answered")
        await connection.close()
    }

    private func concurrentConversationStartAndCancelRouteIndependently() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 64 * 1_024)
        let workspaceID = WorkspaceID("019b0000-0000-7000-8000-000000000010")
        let conversationID = ConversationID("conversation_fixture")
        let runID = ConversationRunID("conversation_run_fixture")
        let start = Task<ConversationStartRunResult, Error> {
            try await connection.startConversationRun(
                workspaceID: workspaceID,
                conversationID: conversationID,
                engineID: EngineID("codex"),
                runInputID: ConversationRunInputID("run_input_fixture")
            )
        }
        let cancel = Task<ConversationCancelRunResult, Error> {
            try await connection.cancelConversationRun(
                workspaceID: workspaceID,
                conversationID: conversationID,
                runID: runID
            )
        }

        let writes = await transport.waitForWriteCount(2)
        let envelopes = try writes.map { try JSONDecoder().decode(RequestEnvelope.self, from: trimmed($0)) }
        let byMethod = Dictionary(uniqueKeysWithValues: envelopes.map { ($0.method, $0.id) })
        let startID = try require(byMethod["conversation.startRun"], "missing start run request")
        let cancelID = try require(byMethod["conversation.cancelRun"], "missing cancel run request")
        await transport.enqueueLine(try fixtureResponseLine("conversation-cancel-run-response.json", id: cancelID))
        await transport.enqueueLine(try fixtureResponseLine("conversation-start-run-response.json", id: startID))

        let cancelResult = try await cancel.value
        let startResult = try await start.value
        try expect(cancelResult.run.runID == runID, "cancel response routed incorrectly")
        try expect(startResult.run.runID == runID, "start response routed incorrectly")
        await connection.close()
    }

    /// A fixture as one JSONL line.
    private func fixtureLine(_ name: String) throws -> String {
        let url = try repositoryRoot()
            .appendingPathComponent("backend/crates/ottam-protocol/fixtures")
            .appendingPathComponent(name)
        let object = try JSONSerialization.jsonObject(with: Data(contentsOf: url))
        return String(decoding: try JSONSerialization.data(withJSONObject: object), as: UTF8.self)
    }

    private func fixtureResponseLine(_ name: String, id: RequestID) throws -> String {
        let url = try repositoryRoot()
            .appendingPathComponent("backend/crates/ottam-protocol/fixtures")
            .appendingPathComponent(name)
        var object = try require(
            try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any],
            "fixture response should be an object"
        )
        switch id {
        case .null: object["id"] = NSNull()
        case .number(let value): object["id"] = value
        case .string(let value): object["id"] = value
        }
        let data = try JSONSerialization.data(withJSONObject: object)
        return try require(String(data: data, encoding: .utf8), "fixture response should be UTF-8")
    }

    private func conversationEventFiltersUseProjectConversationAndRun() async throws {
        let hub = AgentEventHub()
        let recorder = NotificationRecorder()
        _ = await hub.subscribe(
            filter: AgentEventFilter(
                methods: ["conversation.eventDelta"],
                workspaceID: WorkspaceID("workspace-1"),
                projectID: ProjectID("project-1"),
                conversationID: ConversationID("conversation-1"),
                conversationRunID: ConversationRunID("run-1")
            )
        ) { notification in
            Task { await recorder.record(notification) }
        }
        let mismatch = Data(
            #"{"method":"conversation.eventDelta","params":{"workspaceID":"workspace-1","projectID":"project-1","conversationID":"conversation-1","runID":"run-2"}}"#.utf8
        )
        await hub.publish(AgentNotification(method: "conversation.eventDelta", payload: mismatch))
        await Task.yield()
        let mismatchCount = await recorder.count()
        try expect(mismatchCount == 0, "run filter accepted a mismatched event")

        let matching = Data(
            #"{"method":"conversation.eventDelta","params":{"workspaceID":"workspace-1","projectID":"project-1","conversationID":"conversation-1","runID":"run-1"}}"#.utf8
        )
        await hub.publish(AgentNotification(method: "conversation.eventDelta", payload: matching))
        let notification = try await recorder.waitForNotification()
        try expect(notification.method == "conversation.eventDelta", "matching Chat event was not delivered")
    }

    private func clientResponseFailureClosesConnectionAndFailsPendingRequests() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 1_024)
        await connection.setClientRequestHandler { _ in
            .success(.string(String(repeating: "x", count: 2_048)))
        }

        let pending = Task<TestPayload, Error> {
            try await connection.request(method: "test.pending", params: EmptyParams())
        }
        _ = await transport.waitForWriteCount(1)
        await transport.enqueueLine(
            #"{"id":"core/req/oversize","method":"agent.requestPermission","params":{}}"#
        )

        do {
            _ = try await pending.value
            throw TestFailure("Expected client response failure to fail pending requests")
        } catch AgentConnectionError.responseTooLarge(let limit) {
            try expect(limit == 1_024, "client response failure limit mismatch")
        }
        let closeCallCount = await transport.closeCallCount()
        let isActive = await connection.isActive
        try expect(closeCallCount == 1, "client response failure should close transport")
        try expect(isActive == false, "client response failure should close connection")
    }

    private func routesOutOfOrderResponsesByRequestID() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 1_024)

        let first = Task<TestPayload, Error> {
            try await connection.request(method: "test.first", params: EmptyParams())
        }
        let second = Task<TestPayload, Error> {
            try await connection.request(method: "test.second", params: EmptyParams())
        }

        let writes = await transport.waitForWriteCount(2)
        let firstRequest = try JSONDecoder().decode(RequestEnvelope.self, from: trimmed(writes[0]))
        let secondRequest = try JSONDecoder().decode(RequestEnvelope.self, from: trimmed(writes[1]))
        let idsByMethod = Dictionary(
            uniqueKeysWithValues: [
                (firstRequest.method, firstRequest.id),
                (secondRequest.method, secondRequest.id)
            ]
        )
        let firstID = try require(idsByMethod["test.first"], "missing first request")
        let secondID = try require(idsByMethod["test.second"], "missing second request")
        try expect(Set(idsByMethod.values) == Set([.number(1), .number(2)]), "request ids should be sequential")

        await transport.enqueueLine(responseLine(id: secondID, value: "second"))
        await transport.enqueueLine(responseLine(id: firstID, value: "first"))

        let secondValue = try await second.value
        let firstValue = try await first.value
        try expect(secondValue == TestPayload(value: "second"), "second response routed incorrectly")
        try expect(firstValue == TestPayload(value: "first"), "first response routed incorrectly")
        await connection.close()
    }

    private func openProjectRequestEncodesPath() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 4_096)

        let open = Task<ProjectOpenPathResult, Error> {
            try await connection.openProject(path: "/tmp/ottam-repo")
        }

        let writes = await transport.waitForWriteCount(1)
        let request = try JSONDecoder().decode(ProjectOpenRequestEnvelope.self, from: trimmed(writes[0]))
        try expect(request.method == "project.openPath", "project open method mismatch")
        try expect(request.params.path == "/tmp/ottam-repo", "project open path mismatch")

        await transport.enqueueLine(projectOpenResponseLine(id: request.id, root: "/tmp/ottam-repo"))
        let result = try await open.value
        try expect(result.source.canonicalPath == "/tmp/ottam-repo", "project open response root mismatch")
        await connection.close()
    }

    private func openProjectFailureDoesNotIssueDiffRequest() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 4_096)

        let open = Task<ProjectOpenPathResult, Error> {
            try await connection.openProject(path: "/tmp/not-a-repo")
        }

        let writes = await transport.waitForWriteCount(1)
        let request = try JSONDecoder().decode(ProjectOpenRequestEnvelope.self, from: trimmed(writes[0]))
        await transport.enqueueLine(errorResponseLine(
            id: request.id,
            code: -32000,
            message: "path is not a git repository: /tmp/not-a-repo"
        ))

        do {
            _ = try await open.value
            throw TestFailure("Expected project open failure")
        } catch AgentConnectionError.rpcError(let code, let message, let data) {
            try expect(code == -32000, "project open error code mismatch")
            try expect(message.contains("not a git repository"), "project open error message mismatch")
            try expect(data == nil, "plain error response should not have structured data")
        }

        let writeCount = await transport.writtenLineCount()
        try expect(writeCount == 1, "project open failure should not issue a diff request")
        await connection.close()
    }

    private func projectSourceIDDiffConvenienceRoutesOutOfOrderResponses() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 4_096)
        let projectSourceID = ProjectSourceID("019b0000-0000-7000-8000-000000000002")

        let first = Task<CoreDiffSnapshot, Error> {
            try await connection.diffSnapshot(projectSourceID: projectSourceID)
        }
        _ = await transport.waitForWriteCount(1)
        let second = Task<CoreDiffSnapshot, Error> {
            try await connection.diffSnapshot(projectSourceID: projectSourceID)
        }

        let writes = await transport.waitForWriteCount(2)
        let firstRequest = try JSONDecoder().decode(DiffRequestEnvelope.self, from: trimmed(writes[0]))
        let secondRequest = try JSONDecoder().decode(DiffRequestEnvelope.self, from: trimmed(writes[1]))
        try expect(firstRequest.method == "git.diffSnapshot", "first diff method mismatch")
        try expect(secondRequest.method == "git.diffSnapshot", "second diff method mismatch")
        try expect(firstRequest.params.projectSourceID == projectSourceID, "first request should use projectSourceID")
        try expect(secondRequest.params.projectSourceID == projectSourceID, "second request should use projectSourceID")

        await transport.enqueueLine(diffSnapshotResponseLine(id: secondRequest.id, repoPath: "/tmp/second"))
        await transport.enqueueLine(diffSnapshotResponseLine(id: firstRequest.id, repoPath: "/tmp/first"))

        let secondValue = try await second.value
        let firstValue = try await first.value
        try expect(secondValue.repoPath == "/tmp/second", "second diff response routed incorrectly")
        try expect(firstValue.repoPath == "/tmp/first", "first diff response routed incorrectly")
        await connection.close()
    }

    private func rpcErrorDataDecodesAndPreservesMessage() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 4_096)

        let request = Task<CoreDiffSnapshot, Error> {
            try await connection.diffSnapshot(
                projectSourceID: ProjectSourceID("019b0000-0000-7000-8000-000000000099")
            )
        }
        let writes = await transport.waitForWriteCount(1)
        let envelope = try JSONDecoder().decode(DiffRequestEnvelope.self, from: trimmed(writes[0]))
        await transport.enqueueLine(errorResponseLine(
            id: envelope.id,
            code: -32000,
            message: "unknown projectID: 019b0000-0000-7000-8000-000000000099",
            data: #"{"kind":"unknownProjectID","projectID":"019b0000-0000-7000-8000-000000000099","recoverable":true}"#
        ))

        do {
            _ = try await request.value
            throw TestFailure("Expected diff failure")
        } catch AgentConnectionError.rpcError(let code, let message, let data) {
            try expect(code == -32000, "rpc error code mismatch")
            try expect(message == "unknown projectID: 019b0000-0000-7000-8000-000000000099", "rpc error message mismatch")
            guard case .object(let object) = data else {
                throw TestFailure("structured error data should decode as object")
            }
            try expect(object["kind"] == .string("unknownProjectID"), "structured error kind mismatch")
            try expect(object["projectID"] == .string("019b0000-0000-7000-8000-000000000099"), "structured projectID mismatch")
            try expect(object["recoverable"] == .bool(true), "structured recoverable mismatch")
        }
        await connection.close()
    }

    private func terminalRequestsEncodeExpectedMethods() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 4_096)
        let projectID = ProjectID("019b0000-0000-7000-8000-000000000001")
        let sourceID = ProjectSourceID("019b0000-0000-7000-8000-000000000002")
        let workspaceID = WorkspaceID("019b0000-0000-7000-8000-000000000010")

        let open = Task<TerminalSessionSummary, Error> {
            try await connection.openTerminal(
                workspaceID: workspaceID,
                workingRoot: .project(projectID: projectID, sourceID: sourceID),
                rows: 24,
                cols: 80
            )
        }
        let openWrite = await transport.waitForWriteCount(1)
        let openRequest = try JSONDecoder().decode(TerminalOpenRequestEnvelope.self, from: trimmed(openWrite[0]))
        try expect(openRequest.method == "terminal.open", "terminal open method mismatch")
        try expect(openRequest.params.workspaceID == workspaceID, "terminal open workspaceID mismatch")
        try expect(
            openRequest.params.workingRoot == .project(projectID: projectID, sourceID: sourceID),
            "terminal open working root mismatch"
        )
        try expect(openRequest.params.rows == 24, "terminal open rows mismatch")
        await transport.enqueueLine(terminalOpenResponseLine(id: openRequest.id))
        let summary = try await open.value
        try expect(summary.sessionID == TerminalSessionID("terminal_local_test"), "terminal session id mismatch")

        let write = Task<Void, Error> {
            try await connection.writeTerminal(
                sessionID: TerminalSessionID("terminal_local_test"),
                data: Data("hello".utf8)
            )
        }
        let writeLines = await transport.waitForWriteCount(2)
        let writeRequest = try JSONDecoder().decode(TerminalWriteRequestEnvelope.self, from: trimmed(writeLines[1]))
        try expect(writeRequest.method == "terminal.write", "terminal write method mismatch")
        try expect(writeRequest.params.dataBase64 == Data("hello".utf8).base64EncodedString(), "terminal write data mismatch")
        await transport.enqueueLine(emptyResponseLine(id: writeRequest.id))
        try await write.value

        let resize = Task<Void, Error> {
            try await connection.resizeTerminal(
                sessionID: TerminalSessionID("terminal_local_test"),
                rows: 33,
                cols: 101
            )
        }
        let resizeLines = await transport.waitForWriteCount(3)
        let resizeRequest = try JSONDecoder().decode(TerminalResizeRequestEnvelope.self, from: trimmed(resizeLines[2]))
        try expect(resizeRequest.method == "terminal.resize", "terminal resize method mismatch")
        try expect(resizeRequest.params.rows == 33, "terminal resize rows mismatch")
        try expect(resizeRequest.params.cols == 101, "terminal resize cols mismatch")
        await transport.enqueueLine(emptyResponseLine(id: resizeRequest.id))
        try await resize.value

        let close = Task<Void, Error> {
            try await connection.closeTerminal(sessionID: TerminalSessionID("terminal_local_test"))
        }
        let closeLines = await transport.waitForWriteCount(4)
        let closeRequest = try JSONDecoder().decode(TerminalCloseRequestEnvelope.self, from: trimmed(closeLines[3]))
        try expect(closeRequest.method == "terminal.close", "terminal close method mismatch")
        await transport.enqueueLine(emptyResponseLine(id: closeRequest.id))
        try await close.value
        await connection.close()
    }

    private func terminalNotificationsRouteToHandler() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 4_096)
        let received = NotificationRecorder()
        await connection.setNotificationHandler { notification in
            await received.record(notification)
        }

        let request = Task<TestPayload, Error> {
            try await connection.request(method: "test.pending", params: EmptyParams())
        }
        let writes = await transport.waitForWriteCount(1)
        let pending = try JSONDecoder().decode(RequestEnvelope.self, from: trimmed(writes[0]))

        await transport.enqueueLine(#"{"method":"terminal.outputDelta","params":{"sessionID":"terminal_local_test","workspaceID":"workspace_fixture","sequence":1,"dataBase64":"aGk=","capReached":false}}"#)
        await transport.enqueueLine(responseLine(id: pending.id, value: "ok"))

        let value = try await request.value
        try expect(value.value == "ok", "pending response did not complete after notification")
        let notification = try await received.waitForNotification()
        try expect(notification.method == "terminal.outputDelta", "notification method mismatch")
        let typed = try JSONDecoder().decode(
            ProtocolNotification<TerminalOutputDeltaParams>.self,
            from: notification.payload
        )
        try expect(typed.params.sessionID == TerminalSessionID("terminal_local_test"), "terminal notification session mismatch")
        try expect(typed.params.sequence == 1, "terminal notification sequence mismatch")
        try expect(typed.params.deliveryState == nil, "minimal terminal notification should keep delivery state absent")
        try expect(typed.params.replaySuggested == false, "minimal terminal notification should default replaySuggested")
        await connection.close()
    }

    private func notificationsRemainOrderedAcrossAsyncHandlers() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 4_096)
        let received = NotificationRecorder()
        await connection.setNotificationHandler { notification in
            let event = try? JSONDecoder().decode(
                ProtocolNotification<TerminalOutputDeltaParams>.self,
                from: notification.payload
            )
            if event?.params.sequence == 1 {
                try? await Task.sleep(for: .milliseconds(20))
            }
            await received.record(notification)
        }

        let request = Task<TestPayload, Error> {
            try await connection.request(method: "test.pending", params: EmptyParams())
        }
        let writes = await transport.waitForWriteCount(1)
        let pending = try JSONDecoder().decode(RequestEnvelope.self, from: trimmed(writes[0]))
        await transport.enqueueLine(#"{"method":"terminal.outputDelta","params":{"sessionID":"terminal_local_test","workspaceID":"workspace_fixture","sequence":1,"dataBase64":"YQ==","capReached":false}}"#)
        await transport.enqueueLine(#"{"method":"terminal.outputDelta","params":{"sessionID":"terminal_local_test","workspaceID":"workspace_fixture","sequence":2,"dataBase64":"Yg==","capReached":false}}"#)
        await transport.enqueueLine(responseLine(id: pending.id, value: "ok"))

        let first = try await received.waitForNotification()
        let second = try await received.waitForNotification()
        let decoder = JSONDecoder()
        let firstEvent = try decoder.decode(ProtocolNotification<TerminalOutputDeltaParams>.self, from: first.payload)
        let secondEvent = try decoder.decode(ProtocolNotification<TerminalOutputDeltaParams>.self, from: second.payload)
        try expect(firstEvent.params.sequence == 1, "first async notification was reordered")
        try expect(secondEvent.params.sequence == 2, "second async notification was reordered")
        _ = try await request.value
        await connection.close()
    }

    private func terminalOutputDeliveryMetadataDecodes() throws {
        let notification = try JSONDecoder().decode(
            ProtocolNotification<TerminalOutputDeltaParams>.self,
            from: Data(
                #"""
                {"method":"terminal.outputDelta","params":{"sessionID":"terminal_local_test","workspaceID":"workspace_fixture","sequence":4,"dataBase64":"aGk=","capReached":false,"deliveryState":"degraded","droppedBytes":4096,"droppedChunks":3,"replaySuggested":true}}
                """#.utf8
            )
        )

        try expect(notification.params.deliveryState == "degraded", "delivery state did not decode")
        try expect(notification.params.droppedBytes == 4096, "dropped bytes did not decode")
        try expect(notification.params.droppedChunks == 3, "dropped chunks did not decode")
        try expect(notification.params.replaySuggested == true, "replaySuggested did not decode")
    }

    private func terminalReplayMetadataDecodes() throws {
        let minimal = try JSONDecoder().decode(
            ProtocolResponse<TerminalReplayResult>.self,
            from: Data(
                #"""
                {"id":1,"result":{"terminalID":"terminal_record_test","chunks":[],"truncated":false}}
                """#.utf8
            )
        )
        let minimalResult = try require(minimal.result, "minimal replay response result")
        try expect(minimalResult.responseTruncated == false, "minimal replay should default responseTruncated")
        try expect(minimalResult.retentionTruncated == false, "minimal replay should default retentionTruncated")
        try expect(minimalResult.retainedByteStart == nil, "minimal replay should keep retained byte start absent")

        let full = try JSONDecoder().decode(
            ProtocolResponse<TerminalReplayResult>.self,
            from: Data(
                #"""
                {"id":2,"result":{"terminalID":"terminal_record_test","chunks":[],"truncated":true,"responseTruncated":false,"retentionTruncated":true,"retainedByteStart":4096,"retainedByteEnd":8192,"retainedSequenceStart":3,"retainedSequenceEnd":4}}
                """#.utf8
            )
        )
        let fullResult = try require(full.result, "full replay response result")
        try expect(fullResult.truncated == true, "replay truncated did not decode")
        try expect(fullResult.responseTruncated == false, "replay responseTruncated did not decode")
        try expect(fullResult.retentionTruncated == true, "replay retentionTruncated did not decode")
        try expect(fullResult.retainedByteStart == 4096, "replay retainedByteStart did not decode")
        try expect(fullResult.retainedByteEnd == 8192, "replay retainedByteEnd did not decode")
        try expect(fullResult.retainedSequenceStart == 3, "replay retainedSequenceStart did not decode")
        try expect(fullResult.retainedSequenceEnd == 4, "replay retainedSequenceEnd did not decode")
    }

    private func oversizedResponseFailsPendingRequest() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 1_024)

        let request = Task<TestPayload, Error> {
            try await connection.request(method: "test.oversize", params: EmptyParams())
        }
        _ = await transport.waitForWriteCount(1)

        await transport.enqueueError(AgentConnectionError.responseTooLarge(1_024))

        do {
            _ = try await request.value
            throw TestFailure("Expected oversized response to fail")
        } catch AgentConnectionError.responseTooLarge(let limit) {
            try expect(limit == 1_024, "oversize limit mismatch")
        }
        await connection.close()
    }

    private func structuredExitErrorSurvivesEOF() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport)
        let request = Task<TestPayload, Error> {
            try await connection.request(method: "initialize", params: EmptyParams())
        }
        _ = await transport.waitForWriteCount(1)
        await transport.enqueueLine(#"{"method":"core.exitError","params":{"kind":"storageMigrationFailed","message":"Incompatible database","details":"Back up the data first."}}"#)
        await transport.finish()
        do {
            _ = try await request.value
            throw TestFailure("Expected structured exit error")
        } catch AgentConnectionError.coreExited(let error) {
            try expect(error.kind == "storageMigrationFailed", "exit error kind lost")
            try expect(error.message == "Incompatible database", "exit message lost")
            try expect(error.details == "Back up the data first.", "exit details lost")
        }
        do {
            let _: TestPayload = try await connection.request(method: "test.afterExit", params: EmptyParams())
            throw TestFailure("Expected retained exit error after EOF")
        } catch AgentConnectionError.coreExited {
            // A later caller must receive the same structured reason.
        }
        await connection.close()
    }

    private func malformedExitErrorFallsBackToTermination() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport)
        let request = Task<TestPayload, Error> {
            try await connection.request(method: "initialize", params: EmptyParams())
        }
        _ = await transport.waitForWriteCount(1)
        await transport.enqueueLine(#"{"method":"core.exitError","params":{"details":42}}"#)
        await transport.finish()
        do {
            _ = try await request.value
            throw TestFailure("Expected generic exit error for malformed diagnostics")
        } catch AgentConnectionError.processTerminated {
            // Logs remain the fallback for old, crashed, or malformed peers.
        }
        await connection.close()
    }

    private func transportTerminationFailsPendingRequest() async throws {
        let transport = FakeTransport()
        let connection = AsyncJSONLConnection(transport: transport, maximumMessageBytes: 1_024)

        let request = Task<TestPayload, Error> {
            try await connection.request(method: "test.termination", params: EmptyParams())
        }
        _ = await transport.waitForWriteCount(1)
        await transport.finish()

        do {
            _ = try await request.value
            throw TestFailure("Expected terminated transport to fail")
        } catch AgentConnectionError.processTerminated {
            // Expected.
        }
        await connection.close()
    }
}

private actor FakeTransport: JSONLTransport {
    private var writtenLines: [Data] = []
    private var writeWaiters: [(Int, CheckedContinuation<[Data], Never>)] = []
    private var queuedReads: [Result<Data?, Error>] = []
    private var readWaiters: [CheckedContinuation<Data?, Error>] = []
    private var closeCalls = 0

    func writeLine(_ line: Data) async throws {
        writtenLines.append(line)
        resumeSatisfiedWriteWaiters()
    }

    func readLine(maximumBytes: Int) async throws -> Data? {
        if !queuedReads.isEmpty {
            return try queuedReads.removeFirst().get()
        }

        return try await withCheckedThrowingContinuation { continuation in
            readWaiters.append(continuation)
        }
    }

    func close() {
        closeCalls += 1
        finish()
    }

    func closeCallCount() -> Int {
        closeCalls
    }

    func waitForWriteCount(_ count: Int) async -> [Data] {
        if writtenLines.count >= count {
            return Array(writtenLines.prefix(count))
        }

        return await withCheckedContinuation { continuation in
            writeWaiters.append((count, continuation))
        }
    }

    func writtenLineCount() -> Int {
        writtenLines.count
    }

    func enqueueLine(_ line: String) {
        enqueue(.success(Data(line.utf8)))
    }

    func enqueueError(_ error: Error) {
        enqueue(.failure(error))
    }

    func finish() {
        enqueue(.success(nil))
    }

    private func enqueue(_ result: Result<Data?, Error>) {
        if !readWaiters.isEmpty {
            let waiter = readWaiters.removeFirst()
            switch result {
            case .success(let data):
                waiter.resume(returning: data)
            case .failure(let error):
                waiter.resume(throwing: error)
            }
        } else {
            queuedReads.append(result)
        }
    }

    private func resumeSatisfiedWriteWaiters() {
        var remaining: [(Int, CheckedContinuation<[Data], Never>)] = []
        for waiter in writeWaiters {
            if writtenLines.count >= waiter.0 {
                waiter.1.resume(returning: Array(writtenLines.prefix(waiter.0)))
            } else {
                remaining.append(waiter)
            }
        }
        writeWaiters = remaining
    }
}

private actor NotificationRecorder {
    private var notifications: [AgentNotification] = []
    private var waiters: [CheckedContinuation<AgentNotification, Never>] = []

    func record(_ notification: AgentNotification) {
        if waiters.isEmpty {
            notifications.append(notification)
        } else {
            waiters.removeFirst().resume(returning: notification)
        }
    }

    func waitForNotification() async throws -> AgentNotification {
        if !notifications.isEmpty {
            return notifications.removeFirst()
        }
        return await withCheckedContinuation { continuation in
            waiters.append(continuation)
        }
    }

    func count() -> Int {
        notifications.count
    }
}

private struct EmptyParams: Codable, Sendable {}

private struct TestPayload: Codable, Equatable, Sendable {
    var value: String
}

private struct RequestEnvelope: Decodable {
    var id: RequestID
    var method: String
}

private struct ProjectOpenRequestEnvelope: Decodable {
    var id: RequestID
    var method: String
    var params: ProjectOpenPathParams
}

private struct DiffRequestEnvelope: Decodable {
    var id: RequestID
    var method: String
    var params: GitDiffSnapshotParams
}

private struct TerminalOpenRequestEnvelope: Decodable {
    var id: RequestID
    var method: String
    var params: TerminalOpenParams
}

private struct TerminalWriteRequestEnvelope: Decodable {
    var id: RequestID
    var method: String
    var params: TerminalWriteParams
}

private struct TerminalResizeRequestEnvelope: Decodable {
    var id: RequestID
    var method: String
    var params: TerminalResizeParams
}

private struct TerminalCloseRequestEnvelope: Decodable {
    var id: RequestID
    var method: String
    var params: TerminalCloseParams
}

private struct TestFailure: Error, CustomStringConvertible {
    var description: String

    init(_ description: String) {
        self.description = description
    }
}

private func expect(_ condition: @autoclosure () throws -> Bool, _ message: String) throws {
    guard try condition() else {
        throw TestFailure(message)
    }
}

private func require<T>(_ value: T?, _ message: String) throws -> T {
    guard let value else {
        throw TestFailure(message)
    }
    return value
}

private func expectThrows(_ message: String, _ body: () throws -> Void) throws {
    do {
        try body()
    } catch {
        return
    }
    throw TestFailure(message)
}

private func trimmed(_ data: Data) -> Data {
    guard data.last == UInt8(ascii: "\n") else {
        return data
    }
    return Data(data.dropLast())
}

private func responseLine(id: RequestID, value: String) -> String {
    let encodedID: String
    switch id {
    case .number(let number):
        encodedID = String(number)
    case .string(let string):
        encodedID = #""\#(string)""#
    case .null:
        encodedID = "null"
    }
    return #"{"id":\#(encodedID),"result":{"value":"\#(value)"}}"#
}

private func diffSnapshotResponseLine(id: RequestID, repoPath: String) -> String {
    let encodedID: String
    switch id {
    case .number(let number):
        encodedID = String(number)
    case .string(let string):
        encodedID = #""\#(string)""#
    case .null:
        encodedID = "null"
    }
    return #"{"id":\#(encodedID),"result":{"repoPath":"\#(repoPath)","branch":"main","ahead":0,"behind":0,"additions":0,"deletions":0,"files":[],"untrackedFiles":[],"omittedFiles":[]}}"#
}

private func projectOpenResponseLine(id: RequestID, root: String) -> String {
    let encodedID = encodedRequestID(id)
    return #"{"id":\#(encodedID),"result":{"project":{"projectID":"019b0000-0000-7000-8000-000000000001","displayName":"ottam-repo","createdAt":1750636800000,"updatedAt":1750636800000,"lastOpenedAt":1750636800000},"source":{"projectSourceID":"019b0000-0000-7000-8000-000000000002","projectID":"019b0000-0000-7000-8000-000000000001","kind":"localGit","displayName":"ottam-repo","canonicalPath":"\#(root)","createdAt":1750636800000,"updatedAt":1750636800000,"currentBranch":"main"},"workspaces":[]}}"#
}

private func terminalOpenResponseLine(id: RequestID) -> String {
    let encodedID = encodedRequestID(id)
    return #"{"id":\#(encodedID),"result":{"terminalID":"019b0000-0000-7000-8000-000000000004","sessionID":"terminal_local_test","workspaceID":"019b0000-0000-7000-8000-000000000010","workingRoot":{"kind":"project","displayName":"ottam-repo","canonicalPath":"/tmp/ottam-repo","projectID":"019b0000-0000-7000-8000-000000000001","sourceID":"019b0000-0000-7000-8000-000000000002","availability":"available"},"cwd":"/tmp/ottam-repo","shell":"/bin/zsh","rows":24,"cols":80}}"#
}

private func emptyResponseLine(id: RequestID) -> String {
    let encodedID = encodedRequestID(id)
    return #"{"id":\#(encodedID),"result":{}}"#
}

private func errorResponseLine(
    id: RequestID,
    code: Int,
    message: String,
    data: String? = nil
) -> String {
    let encodedID = encodedRequestID(id)
    let dataField = data.map { #","data":\#($0)"# } ?? ""
    return #"{"id":\#(encodedID),"error":{"code":\#(code),"message":"\#(message)"\#(dataField)}}"#
}

private func encodedRequestID(_ id: RequestID) -> String {
    switch id {
    case .number(let number):
        return String(number)
    case .string(let string):
        return #""\#(string)""#
    case .null:
        return "null"
    }
}

private func withEnvironment<T>(
    _ key: String,
    value: String,
    operation: () async throws -> T
) async throws -> T {
    let oldValue = getenv(key).map { String(cString: $0) }
    setenv(key, value, 1)
    defer {
        if let oldValue {
            setenv(key, oldValue, 1)
        } else {
            unsetenv(key)
        }
    }
    return try await operation()
}

private func repositoryRoot() throws -> URL {
    if let configured = ProcessInfo.processInfo.environment["OTTAM_REPO_ROOT"],
       !configured.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
        return URL(fileURLWithPath: configured).standardized
    }

    var candidate = URL(fileURLWithPath: FileManager.default.currentDirectoryPath).standardized
    while true {
        let package = candidate.appendingPathComponent("app/Package.swift")
        let backend = candidate.appendingPathComponent("backend/Cargo.toml")
        if FileManager.default.fileExists(atPath: package.path),
           FileManager.default.fileExists(atPath: backend.path) {
            return candidate
        }

        let parent = candidate.deletingLastPathComponent()
        if parent.path == candidate.path {
            throw TestFailure("could not locate repository root")
        }
        candidate = parent
    }
}
