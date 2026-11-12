import OttamBackendClient
import Testing
@testable import OttamApp

/// Core decides which requests are plans (`plan_detection_matches_codex_and_claude_shapes`
/// in ottam-core); these cover what the app does with `plan`.
@Suite("Plan approval requests")
@MainActor
struct ChatPlanTests {
    private func request(
        _ toolCall: JSONValue = .object(["toolCallId": .string("call-1"), "title": .string("Implement this plan?")]),
        plan: String?,
        options: [AgentPermissionOption]
    ) -> AgentRequestPermissionParams {
        AgentRequestPermissionParams(
            agentID: AgentID("codex"),
            sessionID: AgentSessionID("s1"),
            plan: plan,
            toolCall: toolCall,
            options: options
        )
    }

    /// codex-acp 2.1.1's plan review (`permissions/plan-review.ts`).
    private let codexOptions = [
        AgentPermissionOption(optionID: "implement_plan", name: "Yes, implement this plan", kind: .allowOnce),
        AgentPermissionOption(optionID: "revise_plan", name: "No, and tell Codex what to do differently", kind: .rejectOnce),
    ]

    @Test("a request core marks with a plan is a plan, titled by the request")
    func readsPlan() {
        let plan = request(plan: "## Add notes\n\n- one\n- two", options: codexOptions).chatPlan
        #expect(plan?.title == "Implement this plan?")
        #expect(plan?.markdown == "## Add notes\n\n- one\n- two")
        #expect(plan?.summary == "Add notes")
        #expect(request(plan: nil, options: codexOptions).chatPlan == nil)
        #expect(request(.object([:]), plan: "x", options: codexOptions).chatPlan?.title == "Approve this plan?")
    }

    /// claude-agent-acp 0.85.1 offers four ways ahead plus keep planning.
    @Test("Claude's options: keep planning is Revise, the four ways ahead are rows")
    func claudeOptions() {
        let options = [
            AgentPermissionOption(optionID: "exit-plan-clear-auto", name: "Yes, clear context and use auto mode", kind: .allowAlways),
            AgentPermissionOption(optionID: "exit-plan-auto", name: "Yes, and use auto mode", kind: .allowAlways),
            AgentPermissionOption(optionID: "exit-plan-bypass", name: "Yes, and bypass permissions", kind: .allowAlways),
            AgentPermissionOption(optionID: "exit-plan-default", name: "Yes, manually approve edits", kind: .allowOnce),
            AgentPermissionOption(optionID: "reject", name: "No, keep planning", kind: .rejectOnce),
        ]
        let call: JSONValue = .object(["toolCallId": .string("toolu_1"), "title": .string("Approve Plan")])
        let permission = ChatPendingPermission(previewRequest: request(call, plan: "# Add TODO.md\n\n1. Create it", options: options))
        #expect(permission.plan?.title == "Approve Plan")
        #expect(permission.plan?.summary == "Add TODO.md")
        #expect(permission.reviseOption?.optionID == "reject")
        #expect(permission.planRows.count == 4)
    }

    @Test("the summary drops ordered-list markers and keeps inline code")
    func summaryMarkers() {
        let plan = ChatPlan(title: "Implement this plan?", markdown: "\n1. Create `NOTES.md` at the repo root.\n2. Next")
        #expect(plan.summary == "Create `NOTES.md` at the repo root.")
        #expect(String(plan.attributedSummary.characters) == "Create NOTES.md at the repo root.")
    }

    @Test("the summary keeps a bold first line's emphasis")
    func summaryKeepsBold() {
        #expect(ChatPlan(title: "", markdown: "**Goal:** add notes").summary == "**Goal:** add notes")
        #expect(ChatPlan(title: "", markdown: "> - **Goal:** add notes").summary == "**Goal:** add notes")
        #expect(ChatPlan(title: "", markdown: "## Plan\n").summary == "Plan")
    }

    @Test("Revise takes the option that keeps planning; the rest are rows")
    func splitsOptions() {
        let permission = ChatPendingPermission(previewRequest: request(plan: "Plan", options: codexOptions))
        #expect(permission.reviseOption?.optionID == "revise_plan")
        #expect(permission.planRows.map(\.optionID) == ["implement_plan"])
    }

    @Test("with no option to keep planning, every option is a row")
    func noReviseOption() {
        let options = [
            AgentPermissionOption(optionID: "a", name: "Auto-accept edits", kind: .allowAlways),
            AgentPermissionOption(optionID: "b", name: "Approve edits", kind: .allowOnce),
        ]
        let permission = ChatPendingPermission(previewRequest: request(plan: "Plan", options: options))
        #expect(permission.reviseOption == nil)
        #expect(permission.planRows.map(\.optionID) == ["a", "b"])
    }

    @Test("a plan's record names the decision, not the request")
    func recordWording() {
        func record(_ outcome: String, allowed: Bool, plan: String? = "# Plan") -> ChatPermissionRecord {
            ChatPermissionRecord(title: "Approve Plan", outcomeLabel: outcome, wasAllowed: allowed, plan: plan)
        }
        let approved = record("Yes, manually approve edits", allowed: true)
        #expect(approved.headline == "Plan approved")
        #expect(approved.subject == "Yes, manually approve edits")
        #expect(approved.chatPlan == ChatPlan(title: "Approve Plan", markdown: "# Plan"))
        let cancelled = record("Cancelled", allowed: false)
        #expect(cancelled.headline == "Plan cancelled")
        #expect(cancelled.subject == nil)
        #expect(record("No, keep planning", allowed: false).headline == "Plan sent back for feedback")
        let tool = record("Allow once", allowed: true, plan: nil)
        #expect(tool.headline == "Allow once")
        #expect(tool.subject == "Approve Plan")
    }
}
