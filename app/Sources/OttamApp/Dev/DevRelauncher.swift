#if DEBUG
import Foundation
import Darwin
import OSLog

@MainActor
enum DevRelauncher {
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "dev.ottam.Ottam",
        category: "DevRelaunch"
    )

    private static let logRelativePath = ".build/dev-relaunch.log"
    private static var isRelaunching = false

    static func rebuildAndRelaunch(workspacePath: String, reloadState: DevReloadState) {
        guard reloadState.beginReloading() else {
            logger.debug("rebuild and relaunch ignored because reload is already active")
            return
        }

        guard let sourceRoot = resolveSourceRoot() else {
            logger.error("could not resolve source root for rebuild and relaunch")
            writeFallbackLog("Could not resolve Ottam source root.")
            reloadState.fail("Could not resolve Ottam source root.")
            return
        }

        let logURL = sourceRoot.appendingPathComponent(logRelativePath)
        guard !isRelaunching else {
            logger.error("rebuild and relaunch requested while already running")
            appendLog("Rebuild and relaunch is already running.", to: logURL)
            reloadState.fail("Rebuild and relaunch is already running.")
            return
        }
        isRelaunching = true

        appendLog("Rebuild and relaunch requested for workspace: \(workspacePath)", to: logURL)
        logger.info("rebuild and relaunch requested workspace=\(workspacePath, privacy: .private)")

        let scriptURL = sourceRoot.appendingPathComponent("scripts/run-app.sh")
        guard FileManager.default.isExecutableFile(atPath: scriptURL.path) else {
            logger.error("missing run-app script at \(scriptURL.path, privacy: .private)")
            appendLog("Missing executable run script: \(scriptURL.path)", to: logURL)
            isRelaunching = false
            reloadState.fail("Missing executable run script.")
            return
        }

        do {
            try FileManager.default.createDirectory(
                at: logURL.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )

            let outputHandle = try FileHandle(forWritingTo: logURL)
            try outputHandle.seekToEnd()
            appendLog("Launching \(scriptURL.path)", to: outputHandle)

            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/bin/bash")
            process.arguments = [scriptURL.path, workspacePath]
            process.currentDirectoryURL = sourceRoot
            var environment = ProcessInfo.processInfo.environment
            environment["OTTAM_RELAUNCH_OLD_PID"] = "\(getpid())"
            process.environment = environment
            process.standardOutput = outputHandle
            process.standardError = outputHandle
            process.terminationHandler = { process in
                guard process.terminationStatus != 0 else {
                    return
                }

                Task { @MainActor in
                    logger.error("relaunch script failed status=\(process.terminationStatus, privacy: .public)")
                    appendLog("Relaunch script exited with status \(process.terminationStatus).", to: logURL)
                    isRelaunching = false
                    reloadState.fail("Relaunch script failed with status \(process.terminationStatus).")
                }
            }
            try process.run()
            logger.info("relaunch script started")
        } catch {
            logger.error("failed to start relaunch script: \(error.localizedDescription, privacy: .private)")
            appendLog("Failed to start relaunch script: \(error)", to: logURL)
            isRelaunching = false
            reloadState.fail("Failed to start relaunch script.")
        }
    }

    private static func resolveSourceRoot() -> URL? {
        let fileManager = FileManager.default
        let bundleDerivedRoot = Bundle.main.bundleURL
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .standardizedFileURL

        if hasRunScript(in: bundleDerivedRoot) {
            return bundleDerivedRoot
        }

        if let currentDirectoryRoot = gitRoot(for: fileManager.currentDirectoryPath),
           hasRunScript(in: currentDirectoryRoot) {
            return currentDirectoryRoot
        }

        return nil
    }

    private static func hasRunScript(in rootURL: URL) -> Bool {
        FileManager.default.isExecutableFile(
            atPath: rootURL.appendingPathComponent("scripts/run-app.sh").path
        )
    }

    private static func gitRoot(for path: String) -> URL? {
        let process = Process()
        let outputPipe = Pipe()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/git")
        process.arguments = ["-C", path, "rev-parse", "--show-toplevel"]
        process.standardOutput = outputPipe
        process.standardError = Pipe()

        do {
            try process.run()
        } catch {
            return nil
        }

        process.waitUntilExit()
        guard process.terminationStatus == 0 else { return nil }

        let output = outputPipe.fileHandleForReading.readDataToEndOfFile()
        guard let root = String(data: output, encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines), !root.isEmpty else {
            return nil
        }
        return URL(fileURLWithPath: root).standardizedFileURL
    }

    private static func appendLog(_ message: String, to logURL: URL) {
        do {
            try FileManager.default.createDirectory(
                at: logURL.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            if !FileManager.default.fileExists(atPath: logURL.path) {
                FileManager.default.createFile(atPath: logURL.path, contents: nil)
            }
            let outputHandle = try FileHandle(forWritingTo: logURL)
            try outputHandle.seekToEnd()
            appendLog(message, to: outputHandle)
            try outputHandle.close()
        } catch {
            writeFallbackLog("\(message)\nFailed to write \(logURL.path): \(error)")
        }
    }

    private static func appendLog(_ message: String, to outputHandle: FileHandle) {
        let line = "[\(Date())] \(message)\n"
        if let data = line.data(using: .utf8) {
            outputHandle.write(data)
        }
    }

    private static func writeFallbackLog(_ message: String) {
        fputs("[Ottam] \(message)\n", stderr)
    }
}
#endif
