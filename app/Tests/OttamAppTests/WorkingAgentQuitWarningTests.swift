import Foundation
import Testing
@testable import OttamApp
@testable import OttamBackendClient

struct WorkingAgentQuitWarningTests {
    @Test func quitAsksNothingWhileNoAgentWorks() {
        #expect(WorkingAgentQuitWarning(workingConversations: 0) == nil)
    }

    @Test func quitNamesOneOrManyWorkingAgents() throws {
        let one = try #require(WorkingAgentQuitWarning(workingConversations: 1))
        #expect(one.message == "Quit Ottam while an agent is working?")
        #expect(one.detail == "Quitting stops its work. You can continue the chat later.")
        let two = try #require(WorkingAgentQuitWarning(workingConversations: 2))
        #expect(two.message == "Quit Ottam while 2 agents are working?")
        #expect(two.detail == "Quitting stops their work. You can continue the chats later.")
    }

    @Test func olderCoresReportNoWorkingAgents() throws {
        let readiness = try JSONDecoder().decode(
            AgentShutdownReadinessResult.self,
            from: Data(#"{"ready":true,"waitingOn":[],"refreshLockPaths":[]}"#.utf8)
        )
        #expect(readiness.workingConversations == 0)
    }
}
