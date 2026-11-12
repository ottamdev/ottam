import Foundation

/// Where Ottam keeps state and logs: `OTTAM_HOME` (default
/// `~/.ottam`) and `OTTAM_LOG_DIR` (default `<home>/logs`), the same
/// resolution core uses.
public enum OttamPaths {
    public static func home(environment: [String: String] = ProcessInfo.processInfo.environment) -> URL {
        if let configured = nonEmpty(environment["OTTAM_HOME"]) {
            return URL(fileURLWithPath: configured).standardizedFileURL
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".ottam")
            .standardizedFileURL
    }

    public static func logsDirectory(environment: [String: String] = ProcessInfo.processInfo.environment) -> URL {
        if let configured = nonEmpty(environment["OTTAM_LOG_DIR"]) {
            return URL(fileURLWithPath: configured).standardizedFileURL
        }
        return home(environment: environment).appendingPathComponent("logs").standardizedFileURL
    }

    private static func nonEmpty(_ value: String?) -> String? {
        guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines), !trimmed.isEmpty else {
            return nil
        }
        return trimmed
    }
}
