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

    @ObservationIgnored private let controller: SPUStandardUpdaterController
    @ObservationIgnored private let delegate = UpdaterLogDelegate()
    @ObservationIgnored private var observation: NSKeyValueObservation?

    static func makeIfConfigured(bundle: Bundle = .main) -> AppUpdater? {
        guard let feed = bundle.object(forInfoDictionaryKey: "SUFeedURL") as? String,
              !feed.isEmpty
        else { return nil }
        return AppUpdater()
    }

    private init() {
        controller = SPUStandardUpdaterController(
            startingUpdater: true,
            updaterDelegate: delegate,
            userDriverDelegate: nil
        )
        observation = controller.updater.observe(\.canCheckForUpdates, options: [.initial, .new]) { [weak self] updater, _ in
            // SPUUpdater is main-thread only, so its KVO fires there.
            MainActor.assumeIsolated {
                self?.canCheckForUpdates = updater.canCheckForUpdates
            }
        }
        updaterLogger.notice("updater started")
    }

    func checkForUpdates() {
        updaterLogger.info("manual update check")
        controller.checkForUpdates(nil)
    }
}

/// Lifecycle logging only: versions and error codes, never feed contents or
/// URLs.
final class UpdaterLogDelegate: NSObject, SPUUpdaterDelegate {
    func updater(_ updater: SPUUpdater, didFindValidUpdate item: SUAppcastItem) {
        updaterLogger.notice("update available: \(item.displayVersionString, privacy: .public)")
    }

    func updaterDidNotFindUpdate(_ updater: SPUUpdater, error: any Error) {
        let reason = (error as NSError).userInfo[SPUNoUpdateFoundReasonKey] as? Int ?? -1
        updaterLogger.info("no update available: reason \(reason, privacy: .public)")
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

    static func isNoUpdate(_ error: NSError) -> Bool {
        error.domain == SUSparkleErrorDomain && error.code == Int(SUError.noUpdateError.rawValue)
    }
}
