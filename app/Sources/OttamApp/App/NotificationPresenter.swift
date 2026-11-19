import AppKit
import Foundation
import Observation
import OSLog
import OttamBackendClient
@preconcurrency import UserNotifications

private let notificationLogger = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
    category: "Notifications"
)

/// A banner as the presenter builds it, before it becomes system content.
struct NotificationBanner: Equatable, Sendable {
    var identifier: String
    var title: String
    var subtitle: String
    var body: String
    var threadIdentifier: String
    var workspaceID: WorkspaceID
    var conversationID: ConversationID
}

/// What the presenter needs from the system notification center. The live
/// center is reachable only from a real app bundle, so tests drive a fake.
@MainActor
protocol NotificationCenterCapability: AnyObject {
    /// Called with a clicked banner's conversation.
    var onOpen: ((WorkspaceID, ConversationID) -> Void)? { get set }
    func authorizationStatus() async -> UNAuthorizationStatus
    func requestAuthorization() async -> Bool
    func add(_ banner: NotificationBanner) async
    func remove(identifiers: [String])
    func deliveredIdentifiers() async -> [String]
}

/// The system notification center, and its delegate.
@MainActor
final class LiveNotificationCenter: NSObject, NotificationCenterCapability, UNUserNotificationCenterDelegate {
    var onOpen: ((WorkspaceID, ConversationID) -> Void)?
    private let center: UNUserNotificationCenter

    /// Nil without a bundle identifier, which `UNUserNotificationCenter`
    /// requires, and under UI tests, whose runs must not prompt or post.
    static func makeIfAvailable(
        bundle: Bundle = .main,
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> LiveNotificationCenter? {
        guard bundle.bundleIdentifier != nil, bundle.bundleURL.pathExtension == "app",
              environment["OTTAM_UI_TEST_MODE"] != "1"
        else { return nil }
        return LiveNotificationCenter(center: .current())
    }

    private init(center: UNUserNotificationCenter) {
        self.center = center
        super.init()
    }

    /// Before launch finishes, so a click that launches Ottam is delivered.
    /// The center holds its delegate weakly; the runtime keeps this alive.
    func installAsDelegate() {
        center.delegate = self
    }

    func authorizationStatus() async -> UNAuthorizationStatus {
        await center.notificationSettings().authorizationStatus
    }

    func requestAuthorization() async -> Bool {
        do {
            return try await center.requestAuthorization(options: [.alert, .sound, .badge])
        } catch {
            notificationLogger.error("notification authorization failed")
            return false
        }
    }

    func add(_ banner: NotificationBanner) async {
        let content = UNMutableNotificationContent()
        content.title = banner.title
        content.subtitle = banner.subtitle
        content.body = banner.body
        content.threadIdentifier = banner.threadIdentifier
        content.sound = .default
        content.userInfo = [
            Self.workspaceKey: banner.workspaceID.rawValue,
            Self.conversationKey: banner.conversationID.rawValue,
        ]
        let request = UNNotificationRequest(identifier: banner.identifier, content: content, trigger: nil)
        do {
            try await center.add(request)
        } catch {
            notificationLogger.error(
                "failed to post a banner conversationID=\(banner.conversationID.rawValue, privacy: .public)"
            )
        }
    }

    func remove(identifiers: [String]) {
        center.removeDeliveredNotifications(withIdentifiers: identifiers)
        center.removePendingNotificationRequests(withIdentifiers: identifiers)
    }

    func deliveredIdentifiers() async -> [String] {
        await center.deliveredNotifications().map(\.request.identifier)
    }

    private nonisolated static let workspaceKey = "workspaceID"
    private nonisolated static let conversationKey = "conversationID"

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .list, .sound]
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        guard response.actionIdentifier == UNNotificationDefaultActionIdentifier else { return }
        let info = response.notification.request.content.userInfo
        guard let workspace = info[Self.workspaceKey] as? String,
              let conversation = info[Self.conversationKey] as? String
        else { return }
        await MainActor.run {
            self.onOpen?(WorkspaceID(workspace), ConversationID(conversation))
        }
    }
}

/// Which banners the user wants. Gates banners only: the sidebar dot and the
/// Dock badge show either way.
@MainActor
@Observable
final class NotificationSettings {
    static let shared = NotificationSettings()

    private enum Key {
        static let agentFinished = "dev.ottam.Ottam.notifications.agentFinished"
        static let agentNeedsInput = "dev.ottam.Ottam.notifications.agentNeedsInput"
    }

    @ObservationIgnored private let defaults: UserDefaults

    var agentFinished: Bool {
        didSet { defaults.set(agentFinished, forKey: Key.agentFinished) }
    }

    var agentNeedsInput: Bool {
        didSet { defaults.set(agentNeedsInput, forKey: Key.agentNeedsInput) }
    }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        agentFinished = defaults.object(forKey: Key.agentFinished) as? Bool ?? true
        agentNeedsInput = defaults.object(forKey: Key.agentNeedsInput) as? Bool ?? true
    }

    func allowsBanner(for reason: AttentionReason) -> Bool {
        switch reason {
        case .completed, .failed: agentFinished
        case .permission, .question, .plan: agentNeedsInput
        }
    }
}

/// Turns core's attention into banners: one per conversation and state, so a
/// newer one replaces it, grouped by workspace. Banners only open the
/// conversation; answering happens in the window.
@MainActor
@Observable
final class NotificationPresenter {
    /// Read by Settings, which links to System Settings once it is denied.
    private(set) var authorization: UNAuthorizationStatus = .notDetermined
    @ObservationIgnored private let center: any NotificationCenterCapability
    @ObservationIgnored private let settings: NotificationSettings
    @ObservationIgnored var workspaceName: (WorkspaceID) -> String? = { _ in nil }

    init(center: any NotificationCenterCapability, settings: NotificationSettings) {
        self.center = center
        self.settings = settings
    }

    var onOpen: ((WorkspaceID, ConversationID) -> Void)? {
        get { center.onOpen }
        set { center.onOpen = newValue }
    }

    func refreshAuthorization() async {
        authorization = await center.authorizationStatus()
    }

    /// Asks for permission the first time there is something to show, never
    /// at launch, and only while the user has not decided.
    func post(_ entry: AttentionEntry) async {
        guard settings.allowsBanner(for: entry.reason) else { return }
        var status = await center.authorizationStatus()
        if status == .notDetermined {
            _ = await center.requestAuthorization()
            status = await center.authorizationStatus()
        }
        authorization = status
        guard status == .authorized || status == .provisional else { return }
        await center.add(banner(for: entry))
        notificationLogger.info(
            "posted a banner conversationID=\(entry.conversationID.rawValue, privacy: .public)"
        )
    }

    func withdraw(_ conversationID: ConversationID) {
        center.remove(identifiers: [AttentionState.needsInput, .unseen].map {
            Self.identifier(conversationID, $0)
        })
    }

    /// Removes delivered banners whose conversation no longer holds that state.
    func reconcile(_ states: [ConversationID: AttentionState]) async {
        let keep = Set(states.map { Self.identifier($0.key, $0.value) })
        let stale = await center.deliveredIdentifiers().filter {
            $0.hasPrefix(Self.prefix) && !keep.contains($0)
        }
        if !stale.isEmpty {
            center.remove(identifiers: stale)
            notificationLogger.info("withdrew stale banners count=\(stale.count, privacy: .public)")
        }
    }

    func banner(for entry: AttentionEntry) -> NotificationBanner {
        NotificationBanner(
            identifier: Self.identifier(entry.conversationID, entry.state),
            title: entry.title.isEmpty ? "Untitled Conversation" : entry.title,
            subtitle: workspaceName(entry.workspaceID) ?? "",
            body: Self.body(for: entry),
            threadIdentifier: entry.workspaceID.rawValue,
            workspaceID: entry.workspaceID,
            conversationID: entry.conversationID
        )
    }

    static func body(for entry: AttentionEntry) -> String {
        switch entry.reason {
        case .completed: "Finished"
        case .failed: entry.detail.map { "Failed: \($0)" } ?? "Failed"
        case .permission: entry.detail.map { "Approval needed: \($0)" } ?? "Approval needed"
        case .plan: "Plan needs approval"
        case .question: entry.detail.map { "Question: \($0)" } ?? "Has a question"
        }
    }

    private static let prefix = "attention."

    static func identifier(_ conversationID: ConversationID, _ state: AttentionState) -> String {
        "\(prefix)\(state.rawValue).\(conversationID.rawValue)"
    }
}
