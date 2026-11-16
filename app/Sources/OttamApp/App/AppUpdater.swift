import Foundation
import OSLog
import Sparkle

private let updaterLogger = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
    category: "Updater"
)

/// Sparkle's standard updater for the process lifetime. Only release
/// assembly writes `SUFeedURL`, so development, `run-app.sh` and UI-test
/// bundles never build one: no menu item, no feed request.
@MainActor
@Observable
final class AppUpdater {
    private(set) var canCheckForUpdates = false
    /// The update the sidebar's button offers (`docs/architecture/app/macos-ui.md`).
    private(set) var availableVersion: String?

    @ObservationIgnored private let controller: SPUStandardUpdaterController
    @ObservationIgnored private let events = UpdaterEventDelegate()
    @ObservationIgnored private let reminders = UpdateReminderDelegate()
    @ObservationIgnored private var observation: NSKeyValueObservation?

    static func makeIfConfigured(bundle: Bundle = .main) -> AppUpdater? {
        guard let feed = bundle.object(forInfoDictionaryKey: "SUFeedURL") as? String,
              !feed.isEmpty
        else { return nil }
        return AppUpdater()
    }

    private init() {
        controller = SPUStandardUpdaterController(
            startingUpdater: false,
            updaterDelegate: events,
            userDriverDelegate: reminders
        )
    }

    /// Starts Sparkle once every callback has somewhere to go. `canRemind`
    /// says whether a window shows the sidebar's update button; when none
    /// does, Sparkle shows its own window as if there were no reminders.
    func start(canRemind: @escaping @MainActor () -> Bool) {
        reminders.canRemind = canRemind
        events.onAvailabilityChange = { [weak self] version in
            self?.availableVersion = version
        }
        observation = controller.updater.observe(\.canCheckForUpdates, options: [.initial, .new]) { [weak self] updater, _ in
            // SPUUpdater is main-thread only, so its KVO fires there.
            MainActor.assumeIsolated {
                self?.canCheckForUpdates = updater.canCheckForUpdates
            }
        }
        controller.startUpdater()
        updaterLogger.notice("updater started")
    }

    /// Also brings a waiting update into focus: Sparkle shows the pending
    /// session's window, or the downloaded update ready to install, rather
    /// than downloading again.
    func checkForUpdates() {
        updaterLogger.info("manual update check")
        controller.checkForUpdates(nil)
    }
}

/// Sparkle's gentle reminders: a scheduled check that finds an update leaves
/// showing it to us, so the sidebar's button stands in for Sparkle's window.
/// Checks the user starts always show the window. Sparkle calls these on the
/// main thread.
final class UpdateReminderDelegate: NSObject, SPUStandardUserDriverDelegate {
    var canRemind: @MainActor () -> Bool = { false }

    var supportsGentleScheduledUpdateReminders: Bool { true }

    func standardUserDriverShouldHandleShowingScheduledUpdate(
        _ update: SUAppcastItem,
        andInImmediateFocus immediateFocus: Bool
    ) -> Bool {
        let canRemind = MainActor.assumeIsolated { canRemind() }
        return !Self.reminds(critical: update.isCriticalUpdate, canRemind: canRemind)
    }

    /// A critical update, or one with no sidebar on screen to show its
    /// button, gets Sparkle's window.
    static func reminds(critical: Bool, canRemind: Bool) -> Bool {
        !critical && canRemind
    }

    /// Sparkle requires this once the method above can return false. The
    /// button already shows: `UpdaterEventDelegate` saw the update found.
    func standardUserDriverWillHandleShowingUpdate(
        _ handleShowingUpdate: Bool,
        forUpdate update: SUAppcastItem,
        state: SPUUserUpdateState
    ) {
        guard !handleShowingUpdate else { return }
        updaterLogger.notice("reminding of update \(update.displayVersionString, privacy: .public)")
    }
}

/// Which update is available, and lifecycle logging (versions and error
/// codes, never feed contents or URLs). An update stays available through
/// Remind Me Later, a closed window, and a cancelled relaunch; only Skip This
/// Version or a check that finds nothing clears it. Installing quits the app.
final class UpdaterEventDelegate: NSObject, SPUUpdaterDelegate {
    var onAvailabilityChange: (@MainActor (String?) -> Void)?

    func updater(_ updater: SPUUpdater, didFindValidUpdate item: SUAppcastItem) {
        updaterLogger.notice("update available: \(item.displayVersionString, privacy: .public)")
        found(version: item.displayVersionString)
    }

    func updaterDidNotFindUpdate(_ updater: SPUUpdater, error: any Error) {
        let reason = (error as NSError).userInfo[SPUNoUpdateFoundReasonKey] as? Int ?? -1
        updaterLogger.info("no update available: reason \(reason, privacy: .public)")
        notFound()
    }

    func updater(
        _ updater: SPUUpdater,
        userDidMake choice: SPUUserUpdateChoice,
        forUpdate updateItem: SUAppcastItem,
        state: SPUUserUpdateState
    ) {
        chose(choice)
    }

    func updater(_ updater: SPUUpdater, willInstallUpdate item: SUAppcastItem) {
        updaterLogger.notice("installing update \(item.displayVersionString, privacy: .public)")
    }

    func updater(_ updater: SPUUpdater, didAbortWithError error: any Error) {
        let error = error as NSError
        // Sparkle ends every check that finds nothing here too; the call
        // above already logged it.
        guard !Self.isNoUpdate(error) else { return }
        updaterLogger.error("update aborted: \(error.domain, privacy: .public) \(error.code, privacy: .public)")
    }

    func found(version: String) {
        notify(version)
    }

    func notFound() {
        notify(nil)
    }

    func chose(_ choice: SPUUserUpdateChoice) {
        guard choice == .skip else { return }
        notify(nil)
    }

    static func isNoUpdate(_ error: NSError) -> Bool {
        error.domain == SUSparkleErrorDomain && error.code == Int(SUError.noUpdateError.rawValue)
    }

    private func notify(_ version: String?) {
        MainActor.assumeIsolated { onAvailabilityChange?(version) }
    }
}
