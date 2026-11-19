import AppKit
import Foundation
import Observation
import OSLog
import OttamBackendClient
import OttamWorkspaceLogic

private let attentionLogger = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
    category: "Attention"
)

/// Core's attention, held for the sidebar, the Dock badge and banners. Core
/// decides; this only presents (`docs/architecture/app/macos-ui.md` § Attention).
@MainActor
@Observable
final class AttentionStore {
    private(set) var reducer = AttentionReducer()
    @ObservationIgnored private let client: OttamCoreProcessClient
    @ObservationIgnored let presenter: NotificationPresenter?
    @ObservationIgnored private let setBadge: @MainActor (Int) -> Void
    @ObservationIgnored private var subscription: AgentEventSubscriptionToken?
    @ObservationIgnored private var seeding: Task<Void, Never>?
    @ObservationIgnored private var presenting: Task<Void, Never>?
    /// Withdrawals asked for per conversation, so a post queued before one
    /// knows it was overtaken.
    @ObservationIgnored private var withdrawals: [ConversationID: Int] = [:]

    init(
        client: OttamCoreProcessClient,
        presenter: NotificationPresenter?,
        setBadge: @escaping @MainActor (Int) -> Void = AttentionStore.setDockBadge
    ) {
        self.client = client
        self.presenter = presenter
        self.setBadge = setBadge
    }

    static func setDockBadge(_ count: Int) {
        NSApplication.shared.dockTile.badgeLabel = count > 0 ? String(count) : nil
    }

    func unseenConversationIDs(in workspaceID: WorkspaceID) -> Set<ConversationID> {
        reducer.conversations(in: workspaceID, state: .unseen)
    }

    /// Subscribes before any snapshot is read, so no change falls between.
    func start() async {
        guard subscription == nil else { return }
        subscription = await client.subscribeToEvents(
            filter: AgentEventFilter(methods: ["attention.changed"])
        ) { [weak self] notification in
            let change = try? JSONDecoder().decode(
                ProtocolNotification<AttentionChangedParams>.self, from: notification.payload
            ).params
            guard let change else { return }
            await self?.receive(change)
        }
    }

    /// Core is up: a new one numbers revisions afresh, and either way its
    /// snapshot replaces what this holds.
    func connected(to instance: CoreInstanceID) {
        if reducer.connect(to: instance) {
            attentionLogger.info("attention reset for a new core")
        }
        seeding?.cancel()
        seeding = Task { [weak self, client] in
            do {
                let snapshot = try await client.listAttention()
                guard !Task.isCancelled, let self else { return }
                self.perform(self.reducer.apply(snapshot: snapshot))
                attentionLogger.info("attention seeded count=\(snapshot.entries.count, privacy: .public)")
            } catch {
                attentionLogger.error("failed to read attention")
            }
        }
    }

    func receive(_ change: AttentionChangedParams) {
        perform(reducer.apply(change))
    }

    /// Removes a conversation's banners after any banner work asked for
    /// before.
    func withdrawBanners(for conversationID: ConversationID) {
        withdrawals[conversationID, default: 0] += 1
        present { $0.withdraw(conversationID) }
    }

    /// Posts `entry` unless, by the time it can be added, the conversation's
    /// banners were withdrawn or its entry changed.
    private func post(_ entry: AttentionEntry) {
        let id = entry.conversationID
        let withdrawn = withdrawals[id, default: 0]
        present { [weak self] presenter in
            await presenter.post(entry) {
                guard let self else { return false }
                return self.withdrawals[id, default: 0] == withdrawn && self.reducer.entries[id] == entry
            }
        }
    }

    /// Banner work runs one piece at a time, in the order it was asked for:
    /// a post still waiting on authorization must not land after a later
    /// withdraw, nor a reconcile remove a banner posted after its snapshot.
    private func present(_ work: @escaping @MainActor (NotificationPresenter) async -> Void) {
        guard let presenter else { return }
        let previous = presenting
        presenting = Task {
            await previous?.value
            await work(presenter)
        }
    }

    private func perform(_ effects: [AttentionEffect]) {
        for effect in effects {
            switch effect {
            case .post(let entry):
                post(entry)
            case .withdraw(let conversationID):
                withdrawBanners(for: conversationID)
            case .reconcile(let states):
                present { await $0.reconcile(states) }
            case .setBadge(let count):
                setBadge(count)
                attentionLogger.info("dock badge count=\(count, privacy: .public)")
            }
        }
    }
}

/// Tells core which conversation the user is looking at: the selection of
/// the main workspace window while Ottam is active, that window is on screen,
/// and the screen is awake and unlocked. Anything else is no focus.
@MainActor
final class FocusReporter {
    private let client: OttamCoreProcessClient
    private let windows: WorkspaceWindowRegistry
    /// Called with each new focus, which seeing a conversation's banners
    /// in the window makes stale.
    var onFocus: ((ConversationID) -> Void)?
    private var screensAreAwake = true
    private var sessionIsActive = true
    private var screenIsUnlocked = true
    private var wanted: ConversationID?
    private var reported: ConversationID?
    private var hasReported = false
    private var sending = false
    private var observers: [(NotificationCenter, NSObjectProtocol)] = []
    private var started = false

    init(client: OttamCoreProcessClient, windows: WorkspaceWindowRegistry) {
        self.client = client
        self.windows = windows
    }

    func start() {
        guard !started else { return }
        started = true
        let local = NotificationCenter.default
        for name in [
            NSWindow.didBecomeMainNotification, NSWindow.didResignMainNotification,
            NSWindow.didChangeOcclusionStateNotification, NSWindow.didMiniaturizeNotification,
            NSWindow.didDeminiaturizeNotification, NSWindow.willCloseNotification,
            NSApplication.didBecomeActiveNotification, NSApplication.didResignActiveNotification,
            NSApplication.didHideNotification, NSApplication.didUnhideNotification,
        ] {
            observe(name, on: local) { _ in }
        }
        // Sleep, wake and fast user switching post only to the workspace's
        // center; screen lock only as a distributed notification.
        let workspace = NSWorkspace.shared.notificationCenter
        observe(NSWorkspace.screensDidSleepNotification, on: workspace) { $0.screensAreAwake = false }
        observe(NSWorkspace.screensDidWakeNotification, on: workspace) { $0.screensAreAwake = true }
        observe(NSWorkspace.sessionDidResignActiveNotification, on: workspace) { $0.sessionIsActive = false }
        observe(NSWorkspace.sessionDidBecomeActiveNotification, on: workspace) { $0.sessionIsActive = true }
        let distributed = DistributedNotificationCenter.default()
        observe(Notification.Name("com.apple.screenIsLocked"), on: distributed) { $0.screenIsUnlocked = false }
        observe(Notification.Name("com.apple.screenIsUnlocked"), on: distributed) { $0.screenIsUnlocked = true }
        update()
    }

    /// A core that just came up holds no focus for this app yet.
    func resend() {
        hasReported = false
        update()
    }

    private func observe(
        _ name: Notification.Name,
        on center: NotificationCenter,
        _ apply: @escaping @MainActor (FocusReporter) -> Void
    ) {
        let token = center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                apply(self)
                self.update()
                // Main moves to the next window only after a close finishes.
                if name == NSWindow.willCloseNotification {
                    DispatchQueue.main.async { [weak self] in self?.update() }
                }
            }
        }
        observers.append((center, token))
    }

    /// Recomputed from scratch on every signal, and again whenever the main
    /// window's selection changes.
    private func update() {
        let window = NSApplication.shared.mainWindow
        let session = window.flatMap { windows.session(owning: $0) }
        trackSelection(of: session)
        wanted = FocusResolver.focus(inputs(window: window, session: session))
        send()
    }

    private var trackedSession: ObjectIdentifier?
    private var trackingGeneration = 0
    private var selectionChanged = false

    /// Tracking fires once, so it is renewed after it fires and whenever
    /// another session becomes main; an older one firing later does nothing.
    private func trackSelection(of session: WorkspaceWindowSession?) {
        let owner = session.map(ObjectIdentifier.init)
        guard owner != trackedSession || selectionChanged else { return }
        trackedSession = owner
        selectionChanged = false
        trackingGeneration += 1
        guard let session else { return }
        let generation = trackingGeneration
        withObservationTracking {
            _ = session.chatWorkspace.selectedConversationID
        } onChange: { [weak self] in
            Task { @MainActor in
                guard let self, generation == self.trackingGeneration else { return }
                self.selectionChanged = true
                self.update()
            }
        }
    }

    private func inputs(window: NSWindow?, session: WorkspaceWindowSession?) -> FocusInputs {
        FocusInputs(
            appIsActive: NSApplication.shared.isActive,
            mainWorkspaceSelection: session?.chatWorkspace.selectedConversationID,
            mainWindowIsVisible: window.map {
                $0.isVisible && !$0.isMiniaturized && $0.occlusionState.contains(.visible)
            } ?? false,
            screensAreAwake: screensAreAwake,
            sessionIsActive: sessionIsActive,
            screenIsUnlocked: screenIsUnlocked
        )
    }


    /// One call in flight; a change meanwhile sends the latest after it, so
    /// core, which keeps the last focus it got, never ends on a stale one.
    /// A failed call is sent again shortly, since core still holds the focus
    /// before it.
    private func send() {
        guard !sending, !hasReported || wanted != reported else { return }
        let focus = wanted
        sending = true
        hasReported = true
        reported = focus
        if let focus { onFocus?(focus) }
        Task { [weak self, client] in
            var failed = false
            do {
                try await client.setAttentionFocusIfRunning(conversationID: focus)
            } catch {
                attentionLogger.error("failed to report focus")
                failed = true
                try? await Task.sleep(for: .seconds(1))
            }
            guard let self else { return }
            self.sending = false
            if failed { self.hasReported = false }
            self.send()
        }
    }
}
