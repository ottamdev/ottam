import AppKit
import Foundation
import Testing
@testable import OttamApp
@testable import OttamBackendClient

struct WorkingAgentQuitWarningTests {
    @Test func quitAsksNothingWhileNoAgentWorks() {
        #expect(WorkingAgentQuitWarning(workingConversations: 0, signalled: false) == nil)
    }

    @Test func signalledQuitNeverAsks() {
        #expect(WorkingAgentQuitWarning(workingConversations: 3, signalled: true) == nil)
    }

    @Test func quitNamesOneOrManyWorkingAgents() throws {
        let one = try #require(WorkingAgentQuitWarning(workingConversations: 1, signalled: false))
        #expect(one.message == "Quit Ottam while an agent is working?")
        #expect(one.detail == "Quitting stops its work. You can continue the chat later.")
        let two = try #require(WorkingAgentQuitWarning(workingConversations: 2, signalled: false))
        #expect(two.message == "Quit Ottam while 2 agents are working?")
        #expect(two.detail == "Quitting stops their work. You can continue the chats later.")
    }

    @Test @MainActor func cancelIsTheDefaultButton() throws {
        let warning = try #require(WorkingAgentQuitWarning(workingConversations: 1, signalled: false))
        let buttons = warning.makeAlert().buttons
        #expect(buttons.map(\.title) == ["Cancel", "Quit"])
        #expect(buttons[0].keyEquivalent == "\r")
        #expect(buttons[1].keyEquivalent.isEmpty)
        #expect(buttons[1].hasDestructiveAction)
    }

    @Test func olderCoresReportNoWorkingAgents() throws {
        let readiness = try JSONDecoder().decode(
            AgentShutdownReadinessResult.self,
            from: Data(#"{"ready":true,"waitingOn":[],"refreshLockPaths":[]}"#.utf8)
        )
        #expect(readiness.workingConversations == 0)
    }
}
