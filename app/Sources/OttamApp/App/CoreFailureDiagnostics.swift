import OttamBackendClient
import Foundation

/// Snapshot of the most recent ottam-core launch logs, gathered when the
/// backend enters the failed state so the error surface can show real output.
struct CoreFailureDiagnostics: Equatable {
    struct LogTail: Equatable {
        var url: URL
        var text: String
    }

    var message: String
    var logsDirectory: URL
    var traceTail: LogTail?
    var stderrTail: LogTail?

    var summaryLine: String {
        message.split(separator: "\n", omittingEmptySubsequences: true)
            .first.map(String.init) ?? message
    }

    var combinedLogText: String {
        var sections: [String] = []
        if let stderrTail, !stderrTail.text.isEmpty {
            sections.append(stderrTail.text)
        }
        if let traceTail, !traceTail.text.isEmpty {
            sections.append(traceTail.text)
        }
        return sections.joined(separator: "\n")
    }

    var clipboardText: String {
        var parts = ["Ottam core failure", "Error: \(message)"]
        if let traceTail {
            parts.append("Trace log: \(traceTail.url.path)\n\(traceTail.text)")
        }
        if let stderrTail, !stderrTail.text.isEmpty {
            parts.append("Stderr log: \(stderrTail.url.path)\n\(stderrTail.text)")
        }
        return parts.joined(separator: "\n\n")
    }

    static func capture(message: String) -> CoreFailureDiagnostics {
        let logsDirectory = OttamPaths.logsDirectory()
        var diagnostics = CoreFailureDiagnostics(message: message, logsDirectory: logsDirectory)
        guard let traceURL = newestTraceLog(in: logsDirectory) else {
            return diagnostics
        }
        diagnostics.traceTail = LogTail(url: traceURL, text: tail(of: traceURL))
        let stderrURL = URL(
            fileURLWithPath: traceURL.path.replacingOccurrences(of: ".log", with: ".stderr.log")
        )
        if FileManager.default.fileExists(atPath: stderrURL.path) {
            let text = tail(of: stderrURL)
            if !text.isEmpty {
                diagnostics.stderrTail = LogTail(url: stderrURL, text: text)
            }
        }
        return diagnostics
    }

    private static func newestTraceLog(in directory: URL) -> URL? {
        let contents = (try? FileManager.default.contentsOfDirectory(
            at: directory,
            includingPropertiesForKeys: [.contentModificationDateKey]
        )) ?? []
        return contents
            .filter { $0.lastPathComponent.hasPrefix("core-") && !$0.lastPathComponent.hasSuffix(".stderr.log") }
            .max { modificationDate($0) < modificationDate($1) }
    }

    private static func modificationDate(_ url: URL) -> Date {
        (try? url.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate)
            ?? .distantPast
    }

    private static func tail(of url: URL, maximumBytes: Int = 16_384, maximumLines: Int = 60) -> String {
        guard let data = try? Data(contentsOf: url), !data.isEmpty else { return "" }
        let suffix = data.suffix(maximumBytes)
        guard let text = String(data: suffix, encoding: .utf8) else {
            return "<\(suffix.count) bytes of non-UTF8 log data>"
        }
        var lines = text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        var truncated = suffix.count < data.count
        if lines.count > maximumLines {
            lines = Array(lines.suffix(maximumLines))
            truncated = true
        }
        let joined = lines.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
        return truncated ? "…\n\(joined)" : joined
    }
}
