import Foundation
import OttamBackendClient
import Testing
@preconcurrency import UserNotifications
@testable import OttamApp

/// Stands in for the system notification center, which a test process
/// cannot reach.
@MainActor
final class FakeNotificationCenter: NotificationCenterCapability {
    var onOpen: ((WorkspaceID, ConversationID) -> Void)?
    var status: UNAuthorizationStatus
    var grants: Bool
    private(set) var requests = 0
    private(set) var added: [NotificationBanner] = []
    private(set) var removed: [String] = []
    var delivered: [String] = []

    init(status: UNAuthorizationStatus, grants: Bool = true) {
        self.status = status
        self.grants = grants
    }

    func authorizationStatus() async -> UNAuthorizationStatus { status }

    func requestAuthorization() async -> Bool {
        requests += 1
        status = grants ? .authorized : .denied
        return grants
    }

    func add(_ banner: NotificationBanner) async {
        added.append(banner)
        delivered.append(banner.identifier)
    }

    func remove(identifiers: [String]) {
        removed.append(contentsOf: identifiers)
        delivered.removeAll { identifiers.contains($0) }
    }

    func deliveredIdentifiers() async -> [String] { delivered }
}

@MainActor
@Suite("Attention presenter")
struct AttentionPresenterTests {
    private let workspace = WorkspaceID("w")

    private func entry(
        _ conversation: String,
        _ state: AttentionState = .unseen,
        reason: AttentionReason = .completed,
        detail: String? = nil
    ) -> AttentionEntry {
        AttentionEntry(
            workspaceID: workspace,
            conversationID: ConversationID(conversation),
            state: state,
            reason: reason,
            title: "Title \(conversation)",
            detail: detail
        )
    }

    private func settings() -> NotificationSettings {
        let suite = "attention-presenter-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        return NotificationSettings(defaults: defaults)
    }

    private func presenter(_ center: FakeNotificationCenter, settings: NotificationSettings? = nil) -> NotificationPresenter {
        NotificationPresenter(center: center, settings: settings ?? self.settings())
    }

    @Test("the first banner asks for permission while undecided, and never again")
    func asksOnceWhileUndetermined() async {
        let center = FakeNotificationCenter(status: .notDetermined)
        let presenter = presenter(center)
        await presenter.post(entry("a"))
        await presenter.post(entry("b"))
        #expect(center.requests == 1)
        #expect(center.added.map(\.conversationID) == [ConversationID("a"), ConversationID("b")])
        #expect(presenter.authorization == .authorized)
    }

    @Test("a denied or refused center gets no banner and no second prompt")
    func deniedPostsNothing() async {
        let refused = FakeNotificationCenter(status: .notDetermined, grants: false)
        let refusedPresenter = presenter(refused)
        await refusedPresenter.post(entry("a"))
        await refusedPresenter.post(entry("b"))
        #expect(refused.requests == 1)
        #expect(refused.added.isEmpty)
        #expect(refusedPresenter.authorization == .denied)

        let denied = FakeNotificationCenter(status: .denied)
        await presenter(denied).post(entry("a"))
        #expect(denied.requests == 0)
        #expect(denied.added.isEmpty)
    }

    @Test("each toggle gates only its own banners, and on by default")
    func togglesGateBanners() async {
        let center = FakeNotificationCenter(status: .authorized)
        let settings = settings()
        #expect(settings.agentFinished && settings.agentNeedsInput)
        let presenter = presenter(center, settings: settings)
        settings.agentFinished = false
        await presenter.post(entry("done", reason: .completed))
        await presenter.post(entry("broke", reason: .failed))
        await presenter.post(entry("ask", .needsInput, reason: .permission))
        #expect(center.added.map(\.conversationID) == [ConversationID("ask")])
        settings.agentFinished = true
        settings.agentNeedsInput = false
        await presenter.post(entry("question", .needsInput, reason: .question))
        await presenter.post(entry("done", reason: .completed))
        #expect(center.added.map(\.conversationID) == [ConversationID("ask"), ConversationID("done")])
    }

    @Test("a banner names the conversation, reads as its last message, threads by workspace, and keys by state")
    func bannerContent() {
        let presenter = presenter(FakeNotificationCenter(status: .authorized))
        let finished = presenter.banner(for: entry("a", detail: "All 12 tests pass."))
        #expect(finished.title == "Title a")
        #expect(finished.body == "All 12 tests pass.")
        #expect(finished.threadIdentifier == "w")
        #expect(finished.identifier == "attention.unseen.a")
        #expect(presenter.banner(for: entry("a")).body == "Finished")
        let asking = presenter.banner(for: entry("a", .needsInput, reason: .permission, detail: "Run tests"))
        #expect(asking.body == "Approval needed: Run tests")
        #expect(asking.identifier == "attention.needsInput.a")
        #expect(NotificationPresenter.body(for: entry("a", reason: .failed, detail: "Out of tokens"))
            == "Failed: Out of tokens")
    }

    @Test("withdrawing clears both states; reconciling drops banners whose state is gone")
    func withdrawAndReconcile() async {
        let center = FakeNotificationCenter(status: .authorized)
        let presenter = presenter(center)
        presenter.withdraw(ConversationID("a"))
        #expect(center.removed == ["attention.needsInput.a", "attention.unseen.a"])

        center.delivered = ["attention.unseen.kept", "attention.unseen.gone", "attention.needsInput.kept", "other"]
        await presenter.reconcile([ConversationID("kept"): .unseen])
        #expect(center.delivered == ["attention.unseen.kept", "other"])
    }
}

@MainActor
@Suite("Attention store")
struct AttentionStoreTests {
    private func client() -> OttamCoreProcessClient {
        OttamCoreProcessClient(executableURL: URL(fileURLWithPath: "/nonexistent/core"))
    }

    private func change(
        _ revision: UInt64,
        _ conversation: String,
        _ state: AttentionState,
        notify: Bool
    ) -> AttentionChangedParams {
        AttentionChangedParams(
            revision: revision,
            entry: AttentionEntry(
                workspaceID: WorkspaceID("w"),
                conversationID: ConversationID(conversation),
                state: state,
                reason: .completed,
                title: conversation
            ),
            notify: notify
        )
    }

    @Test("changes drive the badge and banners even with notifications off")
    func badgeWithoutPresenter() {
        var badges: [Int] = []
        let store = AttentionStore(client: client(), presenter: nil) { badges.append($0) }
        store.receive(change(1, "a", .unseen, notify: true))
        store.receive(change(2, "b", .needsInput, notify: false))
        #expect(store.unseenConversationIDs(in: WorkspaceID("w")) == [ConversationID("a")])
        store.receive(change(3, "a", .none, notify: false))
        #expect(badges == [1, 2, 1])
        #expect(store.unseenConversationIDs(in: WorkspaceID("w")).isEmpty)
    }

    @Test("a notifying change posts and state none withdraws")
    func postsAndWithdraws() async {
        let center = FakeNotificationCenter(status: .authorized)
        let suite = "attention-store-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        let presenter = NotificationPresenter(center: center, settings: NotificationSettings(defaults: defaults))
        let store = AttentionStore(client: client(), presenter: presenter) { _ in }
        store.receive(change(1, "a", .unseen, notify: true))
        let deadline = ContinuousClock.now + .seconds(2)
        while center.added.isEmpty, ContinuousClock.now < deadline {
            await Task.yield()
        }
        #expect(center.added.map(\.identifier) == ["attention.unseen.a"])
        store.receive(change(2, "a", .none, notify: false))
        #expect(center.removed.contains("attention.unseen.a"))
    }
}
