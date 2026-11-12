import OttamBackendClient
import Foundation
import Testing
@testable import OttamApp

@Suite("Chat usage")
@MainActor
struct ChatUsageTests {
    private let usage = ConversationRunUsage(
        scope: .mainAgent, inputTokens: 1_200, outputTokens: 300, totalTokens: 12_345,
        thoughtTokens: nil, cachedReadTokens: 800
    )

    private func run(
        _ id: String, status: ConversationRunStatus = .completed,
        usage: ConversationRunUsage? = nil, unavailable: Bool = false
    ) -> ConversationRunSummary {
        ConversationRunSummary(
            runID: .init(id), conversationID: .init("c1"),
            engineID: EngineID("claude"), status: status, startedAt: "2026-10-19T10:00:00Z",
            cost: .object([:]), usage: usage, usageUnavailable: unavailable, error: .object([:])
        )
    }

    @Test("token counts read as the gauge and trailer print them")
    func tokenCounts() {
        #expect(ChatTokenCount.format(999) == "999")
        #expect(ChatTokenCount.format(1_500) == "1.5K")
        #expect(ChatTokenCount.format(24_100) == "24.1K")
        #expect(ChatTokenCount.format(200_000) == "200K")
        #expect(ChatTokenCount.format(999_960) == "1M")
        #expect(ChatTokenCount.format(1_234_567) == "1.2M")
        #expect(ChatContextLevel.summary(.init(used: 24_100, size: 200_000)) == "24.1K of 200K tokens (12%)")
    }

    @Test("the gauge warns at 75, 90 and 95 percent")
    func thresholds() {
        func level(_ used: UInt64) -> ChatContextLevel { ChatContextLevel(.init(used: used, size: 100)) }
        #expect(level(74) == .normal)
        #expect(level(75) == .filling)
        #expect(level(90) == .nearlyFull)
        #expect(level(95) == .full)
        #expect(level(140) == .full, "an over-full report pins at full")
        #expect(ChatContextLevel(.init(used: 10, size: 0)) == .normal, "an unknown size never warns")
    }

    private func planUsage(
        _ percents: [Double], status: String? = nil, error: String? = nil
    ) -> AgentPlanUsage {
        let windows = zip(percents, [("5h", AgentPlanUsageWindowKind.fiveHour), ("wk", .weekly)])
            .map { percent, window in
                AgentPlanUsageWindow(kind: window.1, label: window.0, usedPercent: percent, resetsAt: 1_794_042_000)
            }
        return AgentPlanUsage(
            agentID: AgentID("claude"), windows: windows, limitStatus: status,
            source: .probe, observedAt: 1_793_935_258_763, error: error
        )
    }

    @Test("the plan meter prints each window and warns from the fullest")
    func planUsageMeter() {
        #expect(ChatPlanUsageLevel.compact(planUsage([12.4, 15.6])) == "5h 12% · wk 16%")
        #expect(ChatPlanUsageLevel.spoken(planUsage([12, 15])) == "5h 12% used, wk 15% used")
        #expect(ChatPlanUsageLevel(planUsage([12, 74])) == .normal)
        #expect(ChatPlanUsageLevel(planUsage([80, 10])) == .filling)
        #expect(ChatPlanUsageLevel(planUsage([10, 92])) == .nearlyFull)
        #expect(ChatPlanUsageLevel(planUsage([100, 40])) == .full)
        #expect(ChatPlanUsageLevel(planUsage([10, 10], status: "allowed_warning")) == .filling,
                "the vendor's warning shows even when the numbers look low")
        #expect(ChatPlanUsageLevel(planUsage([10, 10], status: "rejected")) == .full)
    }

    @Test("a model's own weekly cap shows in the popover, not the bars")
    func modelWindowIsPopoverOnly() {
        var usage = planUsage([12, 15])
        let fable = AgentPlanUsageWindow(
            kind: .weekly, label: "Fable", usedPercent: 2, resetsAt: nil, model: "Fable"
        )
        usage.windows.append(fable)
        #expect(ChatPlanUsageLevel.barWindows(usage).map(\.label) == ["5h", "wk"])
        #expect(ChatPlanUsageLevel.compact(usage) == "5h 12% · wk 15%")
        #expect(ChatPlanUsageLevel.name(fable) == "Fable")
        #expect(ChatPlanUsageLevel.name(usage.windows[1]) == "Weekly")
    }

    @Test("the plan meter's popover says when a refresh failed")
    func planUsageFooter() {
        let now = Date(timeIntervalSince1970: 1_793_935_258.763 + 120)
        #expect(ChatPlanUsageLevel.footer(planUsage([1]), now: now).hasPrefix("Updated "))
        #expect(ChatPlanUsageLevel.footer(planUsage([1], error: "timed out"), now: now)
            .hasPrefix("Couldn't refresh. Updated "))
        let window = planUsage([1]).windows[0]
        let resets = Date(timeIntervalSince1970: 1_794_042_000)
        let english = Locale(identifier: "en_US")
        func remaining(_ seconds: TimeInterval) -> String {
            ChatPlanUsageLevel.resets(window, now: resets.addingTimeInterval(-seconds), locale: english)
        }
        #expect(remaining(4 * 86_400 + 10 * 3_600 + 5 * 60) == "Resets in 4d 10h")
        #expect(remaining(6 * 3_600 + 12 * 60) == "Resets in 6h 12m")
        #expect(remaining(17 * 60) == "Resets in 17m")
        #expect(remaining(20) == "Resets in 1m", "the last minute still shows a duration")
        #expect(ChatPlanUsageLevel.resets(window, now: .distantFuture) == "Resets now")
    }

    @Test("only the probe's own answer ends a refresh")
    func refreshSettles() {
        func change(_ usage: AgentPlanUsage?) -> AgentPlanUsageChangedParams {
            .init(agentID: AgentID("claude"), usage: usage)
        }
        var live = planUsage([12])
        live.source = .session
        #expect(!ChatWorkspaceModel.settlesProbe(change(live)), "a turn's update keeps waiting")
        #expect(ChatWorkspaceModel.settlesProbe(change(planUsage([12]))))
        live.error = "timed out"
        #expect(ChatWorkspaceModel.settlesProbe(change(live)), "a failure ends it")
        #expect(ChatWorkspaceModel.settlesProbe(change(nil)), "a clear ends it")
    }

    @Test("plan usage is per agent, not per chat")
    func planUsageFollowsTheAgent() throws {
        let defaults = try #require(UserDefaults(suiteName: "ChatUsageTests.\(UUID().uuidString)"))
        let model = ChatWorkspaceModel(defaults: defaults)
        model.recordPlanUsage(planUsage([12, 15]), engineID: EngineID("claude"))
        model.selectedEngineID = EngineID("claude")
        #expect(model.selectedPlanUsage?.windows.count == 2)
        model.selectedEngineID = EngineID("codex")
        #expect(model.selectedPlanUsage == nil)
        model.selectedEngineID = EngineID("claude")
        model.applyPlanUsageChange(.init(agentID: AgentID("claude"), usage: nil))
        #expect(model.selectedPlanUsage == nil, "a cleared snapshot hides the meter")
    }

    @Test("the trailer's tooltip lists the reported categories and the scope")
    func breakdown() {
        #expect(ChatTokenCount.breakdown(usage)
            == "1.2K input · 300 output · 800 cache read\nCounts the main agent's model calls, not its subagents'.")
        var codex = usage
        codex.scope = .lastRequest
        #expect(ChatTokenCount.breakdown(codex).hasSuffix("Counts the turn's last model request only."))
    }

    @Test("a run's trailer shows tokens, says unavailable, or shows nothing for older runs")
    func trailerUsage() {
        #expect(ChatTurnUsage(run("r1", usage: usage)) == .tokens(usage))
        #expect(ChatTurnUsage(run("r2", status: .canceled, unavailable: true)) == .unavailable)
        #expect(ChatTurnUsage(run("r3")) == nil, "a run from before usage was recorded")
    }

    @Test("a stopped run's late usage replaces its unavailable state")
    func lateUsage() {
        var runs = [run("r1", status: .canceled, unavailable: true), run("r2")]
        ChatWorkspaceModel.applyRunUsage(usage, runID: .init("r1"), to: &runs)
        #expect(ChatTurnUsage(runs[0]) == .tokens(usage))
        #expect(ChatTurnUsage(runs[1]) == nil)
        ChatWorkspaceModel.applyRunUsage(usage, runID: .init("missing"), to: &runs)
        #expect(runs.count == 2)
    }

    @Test("each chat keeps its own gauge; a draft has none")
    func perChatGauge() throws {
        let defaults = try #require(UserDefaults(suiteName: "ChatUsageTests.\(UUID().uuidString)"))
        let model = ChatWorkspaceModel(defaults: defaults)
        let first = ConversationID("c1")
        model.recordContextUsage(.init(used: 24_100, size: 200_000), conversationID: first)
        #expect(model.selectedContextUsage == nil, "a draft has no gauge")
        model.selectedConversationID = first
        #expect(model.selectedContextUsage == .init(used: 24_100, size: 200_000))
        model.recordContextUsage(nil, conversationID: first)
        #expect(model.selectedContextUsage == nil, "a new session that hasn't reported clears the gauge")
        model.selectedConversationID = ConversationID("c2")
        #expect(model.selectedContextUsage == nil)
    }
}
