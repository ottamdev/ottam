import Testing
@testable import OttamApp

/// The contract behind `engine.changed`: a change announced while a refresh is
/// running is answered by exactly one more refresh, no matter which caller
/// started the one in flight. Sign-in, "Check Again" and failed-run recovery
/// all call `refreshEngines` directly, so a drain that lived only in the
/// subscription path left those callers holding a pre-probe catalog.
@Suite("Chat engine refresh coalescing")
@MainActor
struct ChatEngineRefreshCoalescingTests {
    @Test("A quiet refresh runs one pass and carries its refreshRuntime")
    func quietRefreshRunsOnce() async {
        let model = ChatWorkspaceModel()
        var passes: [Bool] = []

        await model.coalescedEngineRefresh(refreshRuntime: true) { runtime in
            passes.append(runtime)
        }

        #expect(passes == [true])
        #expect(model.isRefreshingEngines == false)
    }

    @Test("A change announced mid-refresh is answered by one plain drain pass")
    func announcementDuringRefreshDrains() async {
        let model = ChatWorkspaceModel()
        var passes: [Bool] = []

        await model.coalescedEngineRefresh(refreshRuntime: true) { runtime in
            passes.append(runtime)
            if passes.count == 1 {
                // What the subscription does while a direct refresh is in
                // flight. `true` means it was recorded rather than starting a
                // competing refresh.
                #expect(model.noteEngineChangeIfRefreshing())
            }
        }

        // The drain is plain: only the initiating request re-probes runtimes,
        // so "Check Again" stays the only thing that carries `refreshRuntime`.
        #expect(passes == [true, false])
    }

    @Test("A burst announced during one pass collapses into a single drain pass")
    func burstCollapses() async {
        let model = ChatWorkspaceModel()
        var passes = 0

        await model.coalescedEngineRefresh(refreshRuntime: false) { _ in
            passes += 1
            if passes == 1 {
                // One probe lands the models, the sign-ins and the auth verdict.
                #expect(model.noteEngineChangeIfRefreshing())
                #expect(model.noteEngineChangeIfRefreshing())
                #expect(model.noteEngineChangeIfRefreshing())
            }
        }

        #expect(passes == 2)
    }

    @Test("A change announced between drain passes joins the loop")
    func announcementBetweenPassesJoinsLoop() async {
        let model = ChatWorkspaceModel()
        var passes = 0

        await model.coalescedEngineRefresh(refreshRuntime: false) { _ in
            passes += 1
            // The flag spans the drain, so the second pass is still "refreshing"
            // and a change arriving inside it extends the loop rather than
            // racing a second refresh.
            if passes <= 2 {
                #expect(model.noteEngineChangeIfRefreshing())
            }
        }

        #expect(passes == 3)
    }

    @Test("Nothing is recorded when no refresh is running")
    func noteIsIgnoredWhenIdle() {
        let model = ChatWorkspaceModel()

        // The caller owes a refresh of its own; nothing is pending to drain.
        #expect(model.noteEngineChangeIfRefreshing() == false)
    }

    @Test("A recorded change does not leak into the next refresh")
    func drainedFlagDoesNotLeak() async {
        let model = ChatWorkspaceModel()
        var passes = 0

        await model.coalescedEngineRefresh(refreshRuntime: false) { _ in
            passes += 1
            if passes == 1 {
                #expect(model.noteEngineChangeIfRefreshing())
            }
        }
        #expect(passes == 2)

        // A second refresh with nothing announced must not inherit the drain.
        var laterPasses = 0
        await model.coalescedEngineRefresh(refreshRuntime: false) { _ in
            laterPasses += 1
        }
        #expect(laterPasses == 1)
    }
}
