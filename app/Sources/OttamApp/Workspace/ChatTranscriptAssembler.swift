import OttamBackendClient
import OttamInstrumentation
import OttamPresentationLogic
import OttamWorkspaceLogic
import CryptoKit
import Foundation

enum ChatTranscriptAssembler {
    /// A message the parent sent a subagent after its task, in its tab.
    static func fromParent(_ message: String) -> String {
        "From the parent: " + message
    }

    /// Render Core's immutable Conversation positions; only adjacent activity groups collapse.
    static func plan(_ snapshot: ChatTranscriptSnapshot) -> [TranscriptItem] {
        // Renderer-neutral by construction: this is structural reconciliation,
        // not drawing. The native collection renderer consumes the resulting
        // values without leaking its AppKit objects into model assembly.
        let signpost = TranscriptSignpost.beginAssembly()
        var entries: [TranscriptItem] = []
        defer { TranscriptSignpost.endAssembly(signpost, itemCount: entries.count) }
        let messageByID = Dictionary(snapshot.messages.map { ($0.message.messageID.rawValue, $0) }, uniquingKeysWith: { _, latest in latest })
        var positioned = snapshot.normalizedItems.map { ($0.position, $0.itemID) }
        let normalizedIDs = Set(positioned.map(\.1))
        positioned.append(contentsOf: snapshot.messages.filter { !normalizedIDs.contains($0.message.messageID.rawValue) }.map { ($0.message.position, $0.message.messageID.rawValue) })
        let itemByID = Dictionary(snapshot.normalizedItems.map { ($0.itemID, $0) }, uniquingKeysWith: { _, latest in latest })
        let runItems = Dictionary(grouping: snapshot.normalizedItems.filter { $0.runID != nil && $0.completion != .suppressed }, by: { $0.runID! })
        // A message that takes over from a follow-up lands before that run's
        // boundary, so the trailer follows the run's last output instead and
        // keeps the boundary's status and time.
        let lastByRun = runItems.mapValues { items in
            (items.filter { $0.kind != .runBoundary }.max { $0.position < $1.position }
                ?? items.max { $0.position < $1.position }!).itemID
        }
        let boundaryByRun = runItems.compactMapValues { items in
            items.filter { $0.kind == .runBoundary }.max { $0.position < $1.position }
        }
        let lastPositionedID = positioned.max(by: { $0.0 < $1.0 })?.1
        // Superseded too: a retry replaces the failed input, and the row then
        // speaks for the retry, but the refused run still has nothing to trail.
        let failedInputRunIDs = Set(snapshot.messages.lazy.flatMap(\.runInputs)
            .filter { $0.status == .failed || $0.status == .superseded }.compactMap(\.targetRunID))
        var captureGapByID: [String: TranscriptCoverage] = [:]
        for (runID, gaps) in snapshot.captureGaps {
            for gap in gaps {
                let id = "capture-gap/\(runID.rawValue)/\(gap.startPosition)"
                captureGapByID[id] = gap
                positioned.append((gap.startPosition, id))
            }
        }
        var activities: [ChatLiveActivity] = []
        var delegateCalls: [String: Int] = [:]
        // Children by the item their card follows when no `delegate` call
        // shows them, and those already drawn.
        let childrenByRun = snapshot.subagentThreadScope == nil ? Self.childrenByRun(snapshot) : [:]
        let childAnchors = Self.childAnchors(childrenByRun, runItems: runItems) { messageByID[$0.itemID] == nil }
        var shownChildren: Set<ConversationID> = []
        var continuedRuns: Set<ConversationRunID> = []
        var shownCaptureProblems: Set<ConversationRunID> = []
        func appendCaptureProblem(_ runID: ConversationRunID) {
            guard let problem = snapshot.captureProblems[runID], shownCaptureProblems.insert(runID).inserted else { return }
            // Item descriptors and positioned gaps already explain ordinary capture
            // loss. Do not turn their Run aggregate into another apparent tool.
            let attributed = (runItems[runID] ?? []).contains { item in
                (item.payloads + (item.tool?.payloads ?? [])).contains { $0.locallyTruncated }
            } || !(snapshot.captureGaps[runID] ?? []).isEmpty
            let aggregate = ["Some tool details could not be saved completely", "Some transcript output was not saved", "Capture may be incomplete"]
            guard !attributed || !aggregate.contains(problem) else { return }
            let id = "capture/" + runID.rawValue
            entries.append(.init(id: id, content: .markdown("⚠︎ " + problem, syntax: .full, container: nil)))
        }
        func flushActivities() {
            guard let first = activities.first else { return }
            // The trailing activity burst stays live between completed calls.
            // Only moving past the group or ending the Run settles its summary.
            let active = activities.last.map { activity in
                guard let item = itemByID[activity.id], let runID = item.runID else { return false }
                return snapshot.liveRun?.runID == runID && activity.id == lastPositionedID
            } ?? false
            entries.append(.init(id: first.id + "/group", content: .card(.activityGroup(activities, isActive: active))))
            activities.removeAll()
        }
        func appendUnclaimedChildren(after id: String) {
            for child in childAnchors[id] ?? [] where shownChildren.insert(child.conversationID).inserted {
                flushActivities()
                entries.append(.init(
                    id: childCardID(child),
                    content: .card(.delegation(delegationPresentation(agent: child.agentName, failed: false, child: child, snapshot: snapshot)))
                ))
            }
        }
        // A call whose arguments match no child's key takes the first child
        // its turn started after it, rather than leaving that child a card of
        // its own beside an unlinked call.
        func adoptedChild(_ item: ConversationTranscriptItem) -> ChatDelegatedChild? {
            guard item.tool?.status != "failed", let runID = item.runID,
                  let called = ChatTimestampFormatter.date(from: item.createdAt) else { return nil }
            return childrenByRun[runID]?.first { child in
                guard !shownChildren.contains(child.conversationID),
                      let started = child.createdAt.flatMap(ChatTimestampFormatter.date(from:)) else { return false }
                return started >= called
            }
        }
        func toolActivity(_ item: ConversationTranscriptItem) -> ChatLiveActivity? {
            guard let tool = item.tool else { return nil }
            let id = item.itemID
            let body = snapshot.normalizedBodies[id] ?? item.bodyPreview
            let arguments = body.data(using: .utf8).flatMap { try? JSONDecoder().decode(JSONValue.self, from: $0) }
            let supplied = tool.title ?? tool.name ?? "Tool"
            let bare = tool.title == nil || tool.title == tool.name
                || ["bash", "execute", "read", "read_file", "write", "edit", "search", "tool"].contains(supplied.lowercased())
            let ottamTitle = ottamToolTitle(tool, arguments: arguments, snapshot: snapshot)
            let title = ottamTitle ?? toolCaption(tool, arguments: arguments)
                ?? (bare ? arguments.flatMap { chatDerivedToolCallLabel(payload: .object(["rawInput": $0]), kind: tool.kind) } ?? supplied : supplied)
            // An MCP call names its tool beside a title of its own.
            let mcpTool = ottamTitle == nil && title != tool.name ? tool.name.flatMap(mcpToolLabel) : nil
            let argumentPath: String? = {
                guard case .object(let fields) = arguments else { return nil }
                if case .string(let path) = fields["path"] { return path }
                if case .string(let path) = fields["file_path"] { return path }
                return nil
            }()
            let rawPath = argumentPath ?? tool.locations.first?.path
            let path = tool.kind == "read" ? readFilePath(rawPath.map { "Read file " + $0 } ?? title, root: snapshot.resourceRoot) : nil
            let reading = tool.status == "in_progress" || tool.status == "pending"
            return ChatLiveActivity(id: id, label: path.map { (reading ? "Reading " : "Read ") + URL(fileURLWithPath: $0).lastPathComponent } ?? title,
                subtitle: path == nil ? mcpTool : nil, detail: toolDetail(tool, arguments: body), kind: tool.kind, toolCallID: tool.providerToolCallID, filePath: path,
                payloads: tool.payloads, revision: item.revision, bodyByteCount: item.bodyByteCount,
                runID: tool.backgroundTask == nil ? nil : item.runID, backgroundTask: tool.backgroundTask,
                question: tool.question)
        }
        // Subagent threads render as one card at the call that launched them,
        // wherever Core positioned their rows: a child can report while the
        // parent writes, or before its launch is loaded. The card opens the
        // Subagent tab, whose snapshot is scoped to one thread: there, that
        // thread's rows render as a run's do, and nested threads as cards.
        let scope = snapshot.subagentThreadScope
        let threadedKinds: Set<TranscriptItemKind> = [.tool, .reasoning, .plan, .image, .permission, .assistantText]
        let anchorByThread = Dictionary(snapshot.normalizedItems.compactMap { item in
            item.tool?.subagentThread.map { ($0.id, item) }
        }, uniquingKeysWith: { first, _ in first })
        let membersByThread = Dictionary(grouping: snapshot.normalizedItems.filter {
            $0.subagentThreadID != nil && $0.completion != .suppressed && threadedKinds.contains($0.kind)
        }, by: { $0.subagentThreadID! })
        func memberActivity(_ item: ConversationTranscriptItem, open: Set<String>) -> ChatLiveActivity? {
            if let thread = item.tool?.subagentThread {
                // A launch already open above this row is a malformed cycle.
                return open.contains(thread.id) ? nil : threadActivity(item, open: open)
            }
            switch item.kind {
            case .tool: return toolActivity(item)
            case .reasoning:
                var activity = reasoningActivity(id: item.itemID, body: snapshot.normalizedBodies[item.itemID] ?? item.bodyPreview)
                activity.revision = item.revision
                activity.bodyByteCount = item.bodyByteCount
                return activity
            case .plan:
                return ChatLiveActivity(id: item.itemID, label: "Plan", detail: item.plan?.map { "\($0.status): \($0.content)" }.joined(separator: "\n"), kind: "plan")
            case .image:
                return ChatLiveActivity(id: item.itemID, label: "Image", kind: "image", payloads: item.payloads, revision: item.revision)
            case .assistantText:
                // The card's newest line while the subagent writes.
                let body = snapshot.normalizedBodies[item.itemID] ?? item.bodyPreview
                let line = body.split(whereSeparator: \.isNewline).last { !$0.allSatisfy(\.isWhitespace) }
                guard let line else { return nil }
                return ChatLiveActivity(id: item.itemID, label: line.trimmingCharacters(in: .whitespaces), kind: "subagentText")
            default: return nil
            }
        }
        func members(of threadID: String, open: Set<String>) -> [ChatLiveActivity] {
            (membersByThread[threadID] ?? []).compactMap { memberActivity($0, open: open.union([threadID])) }
        }
        func threadActivity(_ anchor: ConversationTranscriptItem, open: Set<String> = []) -> ChatLiveActivity? {
            guard var activity = toolActivity(anchor), let thread = anchor.tool?.subagentThread else { return nil }
            activity.kind = "subagent"
            // A native subagent's name is the parent's short description and
            // its task the whole prompt, which opens its tab: the card's
            // headline is the name, with no agent type beside it.
            let native = thread.source == .nativeSession
            activity.thread = ChatSubagentThread(
                id: thread.id, name: native ? nil : thread.name, task: native ? thread.name ?? thread.task : thread.task,
                state: thread.state, members: members(of: thread.id, open: open), hasFinalAnswer: !thread.asynchronous
            )
            return activity
        }
        /// A row inside a thread whose launch is loaded renders there, not here,
        /// unless the launches above it loop back: then nothing would show it.
        func rendersInThread(_ item: ConversationTranscriptItem) -> Bool {
            guard var threadID = item.subagentThreadID, threadID != scope, threadedKinds.contains(item.kind),
                  anchorByThread[threadID] != nil else { return false }
            var seen: Set<String> = item.tool?.subagentThread.map { [$0.id] } ?? []
            while let anchor = anchorByThread[threadID] {
                guard seen.insert(threadID).inserted else { return false }
                guard let parent = anchor.subagentThreadID, parent != scope else { return true }
                threadID = parent
            }
            return true
        }
        var shownOrphanThreads: Set<String> = []
        // A retained prefix can share a loss position. Show its marker after
        // that item, before any later output, with deterministic tie ordering.
        for (_, id) in positioned.sorted(by: {
            if $0.0 != $1.0 { return $0.0 < $1.0 }
            let leftGap = captureGapByID[$0.1] != nil
            let rightGap = captureGapByID[$1.1] != nil
            if leftGap != rightGap { return !leftGap }
            return $0.1 < $1.1
        }) {
            if captureGapByID[id] != nil {
                flushActivities()
                let detail = "Some output in this part of the conversation could not be saved completely."
                entries.append(.init(id: id, content: .markdown("⚠︎ " + detail, syntax: .full, container: nil)))
                continue
            }
            if let message = messageByID[id] {
                if scope == nil, message.message.role == .system, let note = message.message.note {
                    // A child's report reads as an event of the turn, grouped
                    // with the tool calls around it.
                    var activity = receivedActivity(message.message, note: note, snapshot: snapshot)
                    activity.noteAgent = snapshot.agentName
                    activities.append(activity)
                    continue
                }
                if message.message.role == .user, message.message.source == .noteDelivery {
                    // The prompt core wrote to carry the reports above.
                    continue
                }
                flushActivities()
                append(message, snapshot: snapshot, to: &entries)
                continue
            }
            guard let item = itemByID[id] else { continue }
            guard item.completion != .suppressed else { continue }
            // The tab's header names the thread. Each run of the subagent
            // opens with what the parent asked: its task, then the message
            // that resumed it. A resumed native run's anchor is not the
            // thread's own call.
            if let scope, let thread = item.tool?.subagentThread, thread.id == scope {
                let resumed = thread.source == .nativeSession && item.tool?.providerToolCallID != thread.id
                let asked = resumed ? thread.prompt : thread.prompt ?? thread.task
                if resumed, asked?.hasPrefix("<task-notification>") == true {
                    // Claude resumed it when its background task finished;
                    // the parent sent nothing.
                    flushActivities()
                    entries.append(.init(id: id + "/prompt", content: .markdown(
                        "_Resumed when its background task finished._", syntax: .full, container: nil
                    )))
                } else if let asked, !asked.allSatisfy(\.isWhitespace) {
                    flushActivities()
                    entries.append(.init(id: id + "/prompt", content: .markdown(
                        resumed ? Self.fromParent(asked) : asked, syntax: .inline, container: .bubble
                    )))
                }
                continue
            }
            // The parent's message to the subagent while it ran.
            if let scope, let followUp = item.tool?.subagentFollowUp, followUp.threadID == scope {
                flushActivities()
                entries.append(.init(id: id + "/follow-up", content: .markdown(
                    Self.fromParent(followUp.message), syntax: .inline, container: .bubble
                )))
                continue
            }
            if scope == nil, let runID = item.runID, item.kind != .runBoundary,
               snapshot.engineSwitches[runID] != nil || snapshot.autonomousRunIDs.contains(runID),
               continuedRuns.insert(runID).inserted {
                flushActivities()
                if let divider = snapshot.engineSwitches[runID] {
                    entries.append(.init(id: "run/\(runID.rawValue)/switched", content: .card(.engineSwitched(divider))))
                }
                if snapshot.autonomousRunIDs.contains(runID) {
                    entries.append(.init(
                        id: "run/\(runID.rawValue)/continued", content: .card(.continuedAutomatically(runID))
                    ))
                }
            }
            if let runID = item.runID, !shownCaptureProblems.contains(runID), snapshot.captureProblems[runID] != nil {
                flushActivities()
                appendCaptureProblem(runID)
            }
            let body = snapshot.normalizedBodies[id] ?? item.bodyPreview
            if let threadID = item.subagentThreadID, threadID != scope, threadedKinds.contains(item.kind),
               anchorByThread[threadID] == nil {
                // Its launch is not loaded: the thread's rows gather where the
                // first of them is, under a headline that cannot say more.
                if shownOrphanThreads.insert(threadID).inserted {
                    flushActivities()
                    let thread = ChatSubagentThread(id: threadID, state: nil, members: members(of: threadID, open: []))
                    entries.append(.init(id: id + "/group", content: .card(.activityGroup([
                        ChatLiveActivity(id: "thread/" + threadID, label: "Subagent", kind: "subagent", thread: thread)
                    ], isActive: false))))
                }
            } else if rendersInThread(item) {
                // Drawn inside its thread's card.
            } else if item.tool?.subagentThread != nil, let activity = threadActivity(item) {
                flushActivities()
                entries.append(.init(id: id + "/group", content: .card(.activityGroup([activity], isActive: false))))
            } else {
            switch item.kind {
            case .assistantText where scope == nil && Self.isDelegationAck(body, streaming: item.completion == .open):
                // The parent read the notes above and had nothing to add:
                // the ones it was given say so, and its reply shows as
                // nothing. Notes still held, or delivered later, were not
                // read here.
                guard Self.isDelegationAck(body) else { break }
                for index in activities.indices {
                    guard activities[index].note != nil,
                          let delivered = messageByID[activities[index].id]?.message.noteDeliveredAt,
                          delivered <= item.updatedAt else { continue }
                    activities[index].noteAcknowledged = true
                }
            case .assistantText:
                flushActivities()
                entries.append(.init(id: id, content: .markdown(body, syntax: .full, container: nil)))
            case .reasoning:
                var activity = reasoningActivity(id: id, body: body)
                activity.revision = item.revision
                activity.bodyByteCount = item.bodyByteCount
                activities.append(activity)
            case .image:
                flushActivities()
                entries.append(.init(id: id, content: .card(.activityGroup([
                    ChatLiveActivity(id: id, label: "Image", kind: "image", payloads: item.payloads, revision: item.revision)
                ], isActive: false))))
            case .tool:
                if var delegation = delegationCall(item, snapshot: snapshot, calls: &delegateCalls) {
                    if case .delegated(let agent, let failed, let child, _) = delegation.kind, child == nil, !failed,
                       let adopted = adoptedChild(item) {
                        delegation = delegationPresentation(agent: agent, failed: false, child: adopted, snapshot: snapshot)
                    }
                    if case .delegated(_, _, let child?, _) = delegation.kind,
                       !shownChildren.insert(child.conversationID).inserted {
                        // Drawn already where its turn's positions ran ahead
                        // of their start times; the call's card replaces it.
                        entries.removeAll { $0.id == childCardID(child) }
                    }
                    flushActivities()
                    entries.append(.init(id: id + "/delegation", content: .card(.delegation(delegation))))
                    appendUnclaimedChildren(after: id)
                    continue
                }
                guard let tool = item.tool, let activity = toolActivity(item) else { break }
                if tool.backgroundTask != nil {
                    // Its own row: a collapsed group must not hide work that
                    // is still running, or its Stop.
                    flushActivities()
                    entries.append(.init(id: id + "/group", content: .card(.activityGroup([activity], isActive: false))))
                } else {
                    activities.append(activity)
                }
            case .plan:
                activities.append(ChatLiveActivity(id: id, label: "Plan", detail: item.plan?.map { "\($0.status): \($0.content)" }.joined(separator: "\n"), kind: "plan"))
            case .permission:
                flushActivities()
                if let permission = item.permission {
                    entries.append(.init(id: id, content: .card(.permissionRecord(ChatPermissionRecord(title: permission.title, outcomeLabel: permission.outcomeLabel, wasAllowed: permission.allowed, plan: permission.plan)))))
                }
            case .gap:
                flushActivities()
                let diagnostic = item.gap.map { "Missed \($0.droppedUpdates) updates. \($0.reason)" }
                entries.append(.init(id: id, content: .markdown("⚠︎ Incomplete transcript. " + (diagnostic ?? ""), syntax: .full, container: nil)))
            case .message, .runBoundary:
                break
            }
            }
            appendUnclaimedChildren(after: id)
            if let error = snapshot.bodyErrors[id] {
                flushActivities()
                entries.append(.init(id: id + "/body-error", content: .markdown("⚠︎ Could not load saved text. " + error, syntax: .full, container: nil)))
            }
            if scope == nil, let runID = item.runID, lastByRun[runID] == id,
               let status = boundaryByRun[runID]?.runStatus ?? item.runStatus ?? snapshot.runStatuses[runID], status != .running && status != .queued {
                // A run refused before the agent took the turn leaves only its
                // boundary. The input's row already says Failed, and why (or
                // its retry's state); a trailer here would be a bare timestamp
                // under nothing.
                if status == .failed, failedInputRunIDs.contains(runID),
                   !(runItems[runID] ?? []).contains(where: { $0.kind != .runBoundary }) {
                    continue
                }
                flushActivities()
                // The parent's answer, never a subagent's.
                let finalMessage = (runItems[runID] ?? []).filter { $0.kind == .assistantText && $0.subagentThreadID == nil }
                    .max { $0.position < $1.position }
                let body = finalMessage.flatMap { message -> String? in
                    let text = snapshot.normalizedBodies[message.itemID] ?? message.bodyPreview
                    guard UInt64(text.utf8.count) == message.bodyByteCount,
                          snapshot.bodyErrors[message.itemID] == nil,
                          !Self.isDelegationAck(text) else { return nil }
                    return text
                } ?? ""
                // A turn that ends on a plan the user turned down ends by design
                // (Claude interrupts the turn to wait for feedback); the plan's
                // record already says how, so a Stopped chip would only repeat it.
                // A Stop while the plan waited is still a Stop.
                let lastItem = (runItems[runID] ?? []).filter { $0.kind != .runBoundary && $0.subagentThreadID == nil }
                    .max { $0.position < $1.position }
                let endedOnPlan = lastItem?.permission.map { $0.plan != nil && !$0.allowed && $0.outcomeLabel != "Cancelled" } ?? false
                let turnOutcome = endedOnPlan && status == .canceled ? .complete : outcome(of: status, reason: snapshot.runFailures[runID])
                entries.append(.init(id: trailerID(runID: runID), content: .card(.turnTrailer(.persisted(runID: runID, body: body, createdAt: boundaryByRun[runID]?.updatedAt ?? item.updatedAt, outcome: turnOutcome, backgroundTasks: snapshot.backgroundTasks[runID] ?? 0, usage: snapshot.runUsage[runID], sendsToParent: runID == snapshot.deferredReportRunID))), relation: .attachesToPrevious))
                if case .failed(let reason) = turnOutcome {
                    entries.append(.init(
                        id: "run/\(runID.rawValue)/failure",
                        content: .card(.runFailure(.init(
                            runID: runID, reason: reason,
                            canRetry: snapshot.retryableFailedRunID == runID,
                            isMutating: snapshot.isMutating
                        ))),
                        relation: .attachesToPrevious
                    ))
                }
            }
        }
        flushActivities()
        // A finished subagent with no rows of its own: the agent reported its
        // start and end and none of its work. Say so, rather than leave the
        // tab looking unloaded under the prompt.
        if let scope,
           let anchor = snapshot.normalizedItems.filter({ $0.tool?.subagentThread?.id == scope })
               .max(by: { $0.position < $1.position }),
           anchor.tool?.subagentThread?.state != .running,
           !snapshot.normalizedItems.contains(where: { $0.subagentThreadID == scope && $0.completion != .suppressed }) {
            entries.append(.init(id: anchor.itemID + "/uncaptured", content: .markdown(
                "_No activity was captured for this subagent._", syntax: .full, container: nil
            )))
        }
        for runID in snapshot.captureProblems.keys.sorted(by: { $0.rawValue < $1.rawValue }) {
            appendCaptureProblem(runID)
        }
        if scope == nil, snapshot.liveRun == nil {
            // Reports no turn has carried, while idle.
            let waiting = snapshot.messages.compactMap { entry in
                entry.message.noteDeliveredAt == nil ? entry.message.note : nil
            }
            if !waiting.isEmpty {
                entries.append(.init(id: "delegation/waiting", content: .card(.delegation(ChatDelegationPresentation(
                    kind: .waiting(count: waiting.count, messages: !waiting.contains { $0.kind == .report }),
                    isMutating: snapshot.isMutating
                )))))
            }
        }
        if let run = snapshot.liveRun { appendLiveStatus(run, items: snapshot.normalizedItems, activity: snapshot.pendingPermissions.isEmpty ? snapshot.sessionStates[run.runID] : nil, backgroundTasks: snapshot.backgroundTasks[run.runID] ?? 0, subagents: snapshot.subagentThreads[run.runID] ?? 0, to: &entries) }
        for permission in snapshot.pendingPermissions {
            entries.append(.init(id: "permission/\(permission.id.uuidString)", content: .card(.pendingPermission(permission))))
        }
        for signIn in snapshot.mcpSignIns {
            entries.append(.init(id: "mcp-sign-in/\(signIn.serverID.rawValue)", content: .card(.mcpSignIn(signIn))))
        }
        return entries
    }

    static func readFilePath(_ label: String, root: URL?) -> String? {
        let prefix = label.hasPrefix("Read file ") ? "Read file " : "Read "
        guard label.hasPrefix(prefix) else { return nil }
        var path = String(label.dropFirst(prefix.count)).trimmingCharacters(in: .whitespacesAndNewlines)
        if let first = path.first, (first == "'" || first == "\""), path.last == first, path.count >= 2 {
            path = String(path.dropFirst().dropLast())
        }
        guard !path.isEmpty else { return nil }
        if path.hasPrefix("/") { return path }
        guard let root, root.isFileURL else { return nil }
        return root.appendingPathComponent(path).standardizedFileURL.path
    }

    static let reasoningPreviewLength = 200

    static func reasoningActivity(id: String, body: String) -> ChatLiveActivity {
        let trimmed = body.trimmingCharacters(in: .whitespacesAndNewlines)
        let lines = trimmed.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
        let first = lines.first.map(String.init) ?? ""
        let title = (try? AttributedString(markdown: first,
            options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            .map { String($0.characters) } ?? first
        // The open row reads the full body on demand; carrying it here made
        // every streamed chunk cost the whole text in comparisons.
        guard !title.isEmpty, title.count <= 160 else {
            return ChatLiveActivity(id: id, label: "Reasoning shared by the agent", detail: preview(trimmed), kind: "think")
        }
        let remainder = lines.count > 1 ? String(lines[1]).trimmingCharacters(in: .whitespacesAndNewlines) : ""
        return ChatLiveActivity(id: id, label: title, detail: remainder.isEmpty ? nil : preview(remainder), kind: "think")
    }

    private static func preview(_ text: String) -> String {
        let head = text.prefix(reasoningPreviewLength + 1)
        return head.count <= reasoningPreviewLength ? text : String(head.dropLast()) + "…"
    }

    private static func toolDetail(_ tool: TranscriptTool, arguments: String) -> String {
        var fields: [String] = []
        if let name = tool.name { fields.append("Tool: \(name)") }
        if let kind = tool.kind { fields.append("Kind: \(kind)") }
        if let status = tool.status { fields.append("Status: \(status)") }
        if let id = tool.providerToolCallID { fields.append("Call: \(id)") }
        if let started = tool.startedAt { fields.append("Started: \(started)") }
        if let ended = tool.completedAt { fields.append("Finished: \(ended)") }
        if let code = tool.exitCode { fields.append("Exit code: \(code)") }
        fields += tool.locations.map { location in
            location.path + (location.line.map { ":\($0)" } ?? "")
        }
        if tool.partialMetadata { fields.append("Some tool metadata was not supplied.") }
        if !arguments.isEmpty && arguments != "null" { fields.append("Arguments: \(arguments)") }
        for payload in tool.payloads {
            let role = payload.role == "fileChangeInput" ? "Requested file content" : "Output"
            var detail = "\(role): \(payload.availability)"
            if let count = payload.byteCount { detail += " (\(count) bytes reported)" }
            if payload.upstreamTruncated == true { detail += "; truncated by the agent" }
            if payload.locallyTruncated { detail += "; truncated locally" }
            fields.append(detail)
        }
        return fields.joined(separator: "\n")
    }

    private static func appendLiveStatus(_ run: ChatLiveRunState, items: [ConversationTranscriptItem], activity: ConversationSessionState?, backgroundTasks: Int, subagents: Int, to entries: inout [TranscriptItem]) {
        // The durable boundary can arrive before the lifecycle notification
        // retires this live Run. Its trailer already owns this identity.
        guard !entries.contains(where: { $0.id == trailerID(runID: run.runID) }) else { return }
        entries.append(.init(
            id: trailerID(runID: run.runID),
            content: .card(.turnTrailer(.running(
                runID: run.runID,
                status: run.status,
                startedAt: run.startedAt,
                activityCount: items.filter { $0.runID == run.runID && [.tool, .reasoning, .plan].contains($0.kind) }.count,
                activityLabel: activity?.live == true && activity?.signal.activity == "retrying" ? "Agent is retrying…" : nil,
                backgroundTasks: backgroundTasks,
                subagents: subagents
            ))),
            relation: trailerRelation(hasOutput: items.contains { $0.runID == run.runID })
        ))
    }

    /// A call of Ottam's `delegate` tool, as the row that names the
    /// agent and opens the child it started. Each agent names the tool its
    /// own way (`mcp__ottam__delegate`, `ottam.delegate`, …).
    static func delegationCall(
        _ item: ConversationTranscriptItem,
        snapshot: ChatTranscriptSnapshot
    ) -> ChatDelegationPresentation? {
        var calls: [String: Int] = [:]
        return delegationCall(item, snapshot: snapshot, calls: &calls)
    }

    /// `calls` counts the earlier calls that started a child, by delegation
    /// key, so a repeated prompt's calls take its children in order.
    static func delegationCall(
        _ item: ConversationTranscriptItem,
        snapshot: ChatTranscriptSnapshot,
        calls: inout [String: Int]
    ) -> ChatDelegationPresentation? {
        guard let tool = item.tool else { return nil }
        let names = [tool.name, tool.title].compactMap { $0?.lowercased() }
        guard names.contains(where: { $0.contains("ottam") && $0.hasSuffix("delegate") }) else { return nil }
        // Claude's call carries its input as the item's body; others may
        // carry it as structured arguments.
        let body = (snapshot.normalizedBodies[item.itemID] ?? item.bodyPreview)
            .data(using: .utf8)
            .flatMap { try? JSONDecoder().decode(JSONValue.self, from: $0) }
        let input = [body, tool.arguments].compactMap { $0 }
        // A named agent, an installed agent ad hoc, or else the caller's own.
        let agent = ["agent", "engine"].lazy.compactMap { name in
            input.lazy.compactMap { ottamField(name, in: $0) }.first
        }.first ?? snapshot.agentName ?? "an agent"
        let key = input.lazy.compactMap { ottamField("prompt", in: $0) }.first.map(delegationKey)
        let child = key.flatMap { key -> ChatDelegatedChild? in
            // A refused call started none.
            guard tool.status != "failed" else { return nil }
            let index = calls[key, default: 0]
            calls[key] = index + 1
            return snapshot.delegatedChildren[key].flatMap { index < $0.count ? $0[index] : nil }
        }
        return delegationPresentation(agent: agent, failed: tool.status == "failed" && child == nil, child: child, snapshot: snapshot)
    }

    static func delegationPresentation(
        agent: String, failed: Bool, child: ChatDelegatedChild?, snapshot: ChatTranscriptSnapshot
    ) -> ChatDelegationPresentation {
        // A stopped child's card says why, as its latest report does.
        let stopCause = child.flatMap { child in
            snapshot.messages.last { $0.message.note?.childConversationID == child.conversationID }?.message.note?.stopCause
        }
        return ChatDelegationPresentation(
            kind: .delegated(agent: agent, failed: failed, child: child, stopCause: stopCause),
            agentName: snapshot.agentName
        )
    }

    static func childCardID(_ child: ChatDelegatedChild) -> String {
        "delegation/child/" + child.conversationID.rawValue
    }

    /// Children whose delegating turn is known, by that turn, oldest first.
    static func childrenByRun(_ snapshot: ChatTranscriptSnapshot) -> [ConversationRunID: [ChatDelegatedChild]] {
        Dictionary(grouping: snapshot.delegatedChildren.values.joined().filter { $0.fromRunID != nil }, by: { $0.fromRunID! })
            .mapValues { children in
                children.sorted { ($0.createdAt ?? "", $0.conversationID.rawValue) < ($1.createdAt ?? "", $1.conversationID.rawValue) }
            }
    }

    /// The item each child's card follows if no `delegate` call claims it:
    /// the latest of its turn's top-level tool calls that started before it,
    /// else the turn's last item. OpenCode, for one, calls MCP tools from
    /// inside a code-mode `execute`, so no call there is named `delegate`.
    /// `isDrawn` says whether the transcript draws an item in its own place.
    static func childAnchors(
        _ childrenByRun: [ConversationRunID: [ChatDelegatedChild]],
        runItems: [ConversationRunID: [ConversationTranscriptItem]],
        isDrawn: (ConversationTranscriptItem) -> Bool
    ) -> [String: [ChatDelegatedChild]] {
        var anchored: [String: [ChatDelegatedChild]] = [:]
        for (runID, children) in childrenByRun {
            let turn = (runItems[runID] ?? []).filter { $0.subagentThreadID == nil && isDrawn($0) }
                .sorted { ($0.position, $0.itemID) < ($1.position, $1.itemID) }
            let tools = turn.compactMap { item -> (Date, String)? in
                guard item.kind == .tool, let created = ChatTimestampFormatter.date(from: item.createdAt) else { return nil }
                return (created, item.itemID)
            }
            let last = turn.last { $0.kind != .runBoundary }?.itemID
            for child in children {
                let started = child.createdAt.flatMap(ChatTimestampFormatter.date(from:))
                let before = started.flatMap { started in tools.last { $0.0 <= started }?.1 }
                guard let anchor = before ?? last else { continue }
                anchored[anchor, default: []].append(child)
            }
        }
        return anchored
    }

    /// A string argument of a call, where the agent put it.
    static func ottamField(_ name: String, in value: JSONValue) -> String? {
        guard case .object(let fields) = value else { return nil }
        if case .string(let text) = fields[name] { return text }
        // Codex nests an MCP call's arguments.
        return fields["arguments"].flatMap { ottamField(name, in: $0) }
    }

    /// The model's own caption for a shell call: Claude's `description`. The
    /// command stays in the expanded Command section.
    static func toolCaption(_ tool: TranscriptTool, arguments: JSONValue?) -> String? {
        guard tool.kind == "execute", case .object(let fields) = arguments,
              case .string(let command) = fields["command"], case .string(let description) = fields["description"]
        else { return nil }
        let caption = description.trimmingCharacters(in: .whitespacesAndNewlines)
        return caption.isEmpty || caption == command ? nil : caption
    }

    /// "server · tool" for an MCP tool name, or nil when the name doesn't
    /// split. Claude spells it `mcp__server__tool`. Oh-My-Pi spells it
    /// `mcp__server_tool`, which splits unambiguously only for Ottam's
    /// own server, whose tool names never contain `__`; any other `__` is
    /// read as Claude's separator.
    static func mcpToolLabel(_ name: String) -> String? {
        guard name.hasPrefix("mcp__") else { return nil }
        let rest = name.dropFirst("mcp__".count)
        if rest.hasPrefix("ottam_") {
            let tool = rest.dropFirst("ottam_".count)
            if !tool.isEmpty, !tool.contains("__"), !tool.hasPrefix("_") {
                return "ottam · \(tool)"
            }
        }
        guard let separator = rest.range(of: "__") else { return nil }
        return "\(rest[..<separator.lowerBound]) · \(rest[separator.upperBound...])"
    }

    /// Ottam's tools past `delegate`, however the agent names them.
    static let ottamTools = [
        "list_children", "message_child", "stop_child", "message_parent",
        "get_session_messages", "get_session_toolcall_output",
    ]

    /// The bare name of a call of one of `ottamTools`.
    static func ottamTool(_ tool: TranscriptTool) -> String? {
        let names = [tool.name, tool.title].compactMap { $0?.lowercased() }.filter { $0.contains("ottam") }
        return ottamTools.first { bare in names.contains { $0.hasSuffix(bare) } }
    }

    /// An Ottam tool call's row: what the agent did, and to whom.
    static func ottamToolTitle(
        _ tool: TranscriptTool,
        arguments: JSONValue?,
        snapshot: ChatTranscriptSnapshot
    ) -> String? {
        guard let bare = ottamTool(tool) else { return nil }
        let input = [arguments, tool.arguments].compactMap { $0 }
        func argument(_ name: String) -> String? {
            input.lazy.compactMap { ottamField(name, in: $0) }.first
        }
        // A child as the call names it: by id, task title or agent name.
        func child(_ reference: String?) -> String? {
            guard let reference else { return nil }
            let children = snapshot.delegatedChildren.values.joined()
            let match = children.first { $0.conversationID.rawValue == reference }
                ?? children.first { $0.title.caseInsensitiveCompare(reference) == .orderedSame }
            return match?.agentName ?? reference
        }
        // A child's parent, by the agent that delegated to it.
        let parentAgent = snapshot.messages.lazy.compactMap { entry in
            entry.message.task?.startedByEngineID.flatMap { snapshot.engineNames[$0] }
        }.first
        // The history tools read the parent unless the call names another.
        let readTarget = argument("conversation").map { reference in
            reference == snapshot.delegationParent?.conversationID.rawValue ? parentAgent ?? "the parent" : child(reference) ?? reference
        } ?? (snapshot.delegationParent != nil ? parentAgent ?? "the parent" : nil)
        switch bare {
        case "list_children": return "Checked on delegated agents"
        case "message_child": return child(argument("child")).map { "Messaged \($0)" } ?? "Messaged a delegated agent"
        case "stop_child": return child(argument("child")).map { "Stopped \($0)" } ?? "Stopped a delegated agent"
        case "message_parent": return "Messaged \(parentAgent ?? "the delegating agent")"
        case "get_session_messages": return readTarget.map { "Read \($0)'s conversation" } ?? "Read a conversation"
        default: return readTarget.map { "Read a tool call's output from \($0)" } ?? "Read a tool call's output"
        }
    }

    /// Core's `DELEGATION_ACK_DIRECTIVE`: what a parent woken by its
    /// children's notes replies when none of them needs the user.
    static let delegationAckDirective = "::ottam-ack{}"

    /// An acknowledgement and nothing else, in the backticks or code fence
    /// an agent may quote it in, as core's `is_delegation_ack` reads it.
    /// While `streaming`, the start of one counts too.
    static func isDelegationAck(_ text: String, streaming: Bool = false) -> Bool {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
            .trimmingCharacters(in: CharacterSet(charactersIn: "`"))
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard streaming else { return text == delegationAckDirective }
        return text.hasPrefix("::") && delegationAckDirective.hasPrefix(text)
    }

    /// A note as a "Received message" row: the sender's title, and the
    /// message it sent to show when the row opens. A child names itself; a
    /// parent's message names the parent, as "Started by" does.
    static func receivedActivity(
        _ message: ConversationMessage,
        note: DelegationNote,
        snapshot: ChatTranscriptSnapshot
    ) -> ChatLiveActivity {
        if note.kind == .parentMessage {
            let parent = snapshot.delegationParent
            let title = parent?.title.cappedTitle() ?? ""
            return ChatLiveActivity(
                id: message.messageID.rawValue,
                label: title.isEmpty ? note.childName : title,
                detail: message.body, kind: "receivedMessage", note: note,
                noteSender: parent?.conversationID
            )
        }
        let child = snapshot.delegatedChildren.values.joined().first { $0.conversationID == note.childConversationID }
        return ChatLiveActivity(
            id: message.messageID.rawValue, label: child?.title ?? note.childName,
            detail: message.body, kind: "receivedMessage", note: note,
            noteSender: note.childConversationID, noteAvatar: child?.avatar
        )
    }

    /// Core's key for a delegated prompt: the first 16 hex digits of the
    /// SHA-256 of the trimmed prompt.
    static func delegationKey(_ prompt: String) -> String {
        let digest = SHA256.hash(data: Data(prompt.trimmingCharacters(in: .whitespacesAndNewlines).utf8))
        return digest.prefix(8).map { String(format: "%02x", $0) }.joined()
    }

    private static func append(
        _ entry: ChatTranscriptSnapshot.Message,
        snapshot: ChatTranscriptSnapshot,
        to entries: inout [TranscriptItem]
    ) {
        let message = entry.message
        let messageID = message.messageID.rawValue
        switch message.role {
        case .user:
            if message.source == .delegation, let task = message.task {
                // A delegated child opens on who started it and the task,
                // not on the prompt core assembled from them.
                let parent = snapshot.delegationParent
                entries.append(.init(id: "\(messageID)/task", content: .card(.delegation(ChatDelegationPresentation(
                    kind: .task(ChatDelegationTaskPresentation(
                        task: task,
                        startedBy: task.startedByEngineID.map { snapshot.engineNames[$0] ?? $0.rawValue },
                        parentConversationID: parent?.conversationID,
                        parentTitle: parent?.title,
                        avatar: snapshot.delegatedAvatar
                    ))
                )))))
            } else {
                // Images first, then the text — the block order the agent sees. An
                // image on its own is a complete message, so no empty bubble.
                if !message.attachments.isEmpty {
                    entries.append(.init(id: "\(messageID)/attachments", content: .card(.attachments(message.attachments))))
                }
                if !message.body.isEmpty {
                    entries.append(.init(id: messageID, content: .markdown(message.body, syntax: .inline, container: .bubble)))
                }
            }
            // One row trails every user message, the way one trails every
            // assistant turn: copy and timestamp in a predictable place rather
            // than a surface that exists only when something went wrong. The
            // latest attempt with a one-line state supplies the state slot;
            // recovery-class outcomes are too big for a row and keep their own
            // card beneath it.
            var stateInput: ConversationRunInput?
            var stateOffersResend = false
            var recoveries: [ConversationRunInput] = []
            for runInput in entry.runInputs {
                switch ChatDeliveryPresentation(input: runInput) {
                case .hidden: continue
                case .status(let status):
                    stateInput = runInput
                    // Only a state that names an action can offer one.
                    stateOffersResend = status.action != nil
                        && snapshot.allowsResend
                        && snapshot.cardOwnedInputID != runInput.runInputID
                        && !snapshot.failureOwnedInputIDs.contains(runInput.runInputID)
                        && !snapshot.unsendableInputIDs.contains(runInput.runInputID)
                case .recovery: recoveries.append(runInput)
                }
            }
            entries.append(.init(
                id: trailerID(messageID: message.messageID),
                content: .card(.userTrailer(ChatUserTrailerPresentation(
                    messageID: message.messageID,
                    body: message.body,
                    createdAt: message.createdAt,
                    input: stateInput,
                    failureReason: stateInput.flatMap { input in
                        input.status == .failed ? input.targetRunID.flatMap { snapshot.runFailures[$0] } : nil
                    },
                    showsResend: stateOffersResend,
                    isMutating: snapshot.isMutating,
                    // A Task card spans the column, not a bubble on the right.
                    isLeading: message.source == .delegation && message.task != nil
                ))),
                relation: .attachesToPrevious
            ))
            for runInput in recoveries {
                entries.append(.init(
                    id: "runinput/\(runInput.runInputID.rawValue)",
                    content: .card(.deliveryRecovery(runInput, isMutating: snapshot.isMutating)),
                    relation: .attachesToPrevious
                ))
            }
        case .system:
            entries.append(.init(id: messageID, content: .markdown(message.body, syntax: .inline, container: nil)))
        }
        if let recovery = entry.runtimeRecovery {
            entries.append(.init(id: "\(messageID)/runtime-recovery", content: .card(.runtimeRecovery(
                recovery, install: snapshot.runtimeInstall, isChecking: snapshot.isRefreshingEngines
            ))))
        }
        if let recovery = entry.selectionRecovery {
            entries.append(.init(id: "\(messageID)/selection-recovery", content: .card(.selectionRecovery(
                recovery, isMutating: snapshot.isMutating
            ))))
        }
        if let recovery = entry.authRecovery {
            entries.append(.init(id: "\(messageID)/auth-recovery", content: .card(.authRecovery(
                recovery,
                failure: entry.authSignInFailure,
                isTerminalLogin: entry.authTerminalLogin,
                isMutating: snapshot.isMutating
            ))))
        }
    }

    private static func trailerID(runID: ConversationRunID) -> String {
        "run/\(runID.rawValue)/trailer"
    }

    private static func trailerID(messageID: ConversationMessageID) -> String {
        "message/\(messageID.rawValue)/trailer"
    }

    /// A run that ended on its own reads as complete even when it failed: the
    /// output it did produce is the whole of what it meant to say. Only the two
    /// statuses that cut a turn short mid-sentence get a chip.
    private static func outcome(of status: ConversationRunStatus, reason: String?) -> ChatTurnOutcome {
        switch status {
        case .canceled: .stopped
        case .interrupted: .steered
        case .failed: .failed(reason: reason)
        default: .complete
        }
    }

    private static func trailerRelation(hasOutput: Bool) -> TranscriptItem.Relation {
        hasOutput ? .attachesToPrevious : .turn
    }
}
