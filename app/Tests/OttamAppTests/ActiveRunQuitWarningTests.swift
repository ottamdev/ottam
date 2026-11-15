import Foundation
import Testing
@testable import OttamApp
@testable import OttamBackendClient

struct ActiveRunQuitWarningTests {
    @Test func quitAsksNothingWithoutActiveRuns() {
        #expect(ActiveRunQuitWarning(activeRunConversations: 0) == nil)
    }

    @Test func quitNamesOneOrManyWorkingAgents() throws {
        let one = try #require(ActiveRunQuitWarning(activeRunConversations: 1))
        #expect(one.message == "Quit Ottam while an agent is working?")
        #expect(one.detail == "Quitting stops its turn. You can continue the chat later.")
        let two = try #require(ActiveRunQuitWarning(activeRunConversations: 2))
        #expect(two.message == "Quit Ottam while 2 agents are working?")
        #expect(two.detail == "Quitting stops their turns. You can continue the chats later.")
    }

    @Test func olderCoresReportNoActiveRuns() throws {
        let readiness = try JSONDecoder().decode(
            AgentShutdownReadinessResult.self,
            from: Data(#"{"ready":true,"waitingOn":[],"refreshLockPaths":[]}"#.utf8)
        )
        #expect(readiness.activeRunConversations == 0)
    }
}
