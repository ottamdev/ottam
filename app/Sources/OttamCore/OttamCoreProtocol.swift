import Foundation

public struct CoreDiffSnapshot: Codable, Equatable, Sendable {
    public var repoPath: String
    public var branch: String
    public var ahead: Int
    public var behind: Int
    public var additions: Int
    public var deletions: Int
    public var files: [CoreDiffFile]
    public var untrackedFiles: [String]
    public var omittedFiles: [CoreDiffOmittedFile]

    private enum CodingKeys: String, CodingKey {
        case repoPath
        case branch
        case ahead
        case behind
        case additions
        case deletions
        case files
        case untrackedFiles
        case omittedFiles
    }

    public init(
        repoPath: String,
        branch: String,
        ahead: Int,
        behind: Int,
        additions: Int,
        deletions: Int,
        files: [CoreDiffFile],
        untrackedFiles: [String],
        omittedFiles: [CoreDiffOmittedFile]
    ) {
        self.repoPath = repoPath
        self.branch = branch
        self.ahead = ahead
        self.behind = behind
        self.additions = additions
        self.deletions = deletions
        self.files = files
        self.untrackedFiles = untrackedFiles
        self.omittedFiles = omittedFiles
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        repoPath = try container.decode(String.self, forKey: .repoPath)
        branch = try container.decode(String.self, forKey: .branch)
        ahead = try container.decode(Int.self, forKey: .ahead)
        behind = try container.decode(Int.self, forKey: .behind)
        additions = try container.decode(Int.self, forKey: .additions)
        deletions = try container.decode(Int.self, forKey: .deletions)
        files = try container.decode([CoreDiffFile].self, forKey: .files)
        untrackedFiles = try container.decode([String].self, forKey: .untrackedFiles)
        omittedFiles = try container.decodeIfPresent([CoreDiffOmittedFile].self, forKey: .omittedFiles) ?? []
    }
}

public struct CoreDiffOmittedFile: Codable, Equatable, Sendable {
    public var path: String
    public var reason: String
    public var byteCount: Int?
}

public struct CoreDiffFile: Codable, Equatable, Sendable {
    public var path: String
    public var oldPath: String?
    public var status: String
    public var additions: Int
    public var deletions: Int
    public var hunks: [CoreDiffHunk]
}

public struct CoreDiffHunk: Codable, Equatable, Sendable {
    public var kind: CoreDiffHunkKind
    public var header: String
    public var oldStart: Int
    public var oldLines: Int
    public var newStart: Int
    public var newLines: Int
    public var lines: [CoreDiffLine]

    private enum CodingKeys: String, CodingKey {
        case kind
        case header
        case oldStart
        case oldLines
        case newStart
        case newLines
        case lines
    }

    public init(
        kind: CoreDiffHunkKind,
        header: String,
        oldStart: Int,
        oldLines: Int,
        newStart: Int,
        newLines: Int,
        lines: [CoreDiffLine]
    ) {
        self.kind = kind
        self.header = header
        self.oldStart = oldStart
        self.oldLines = oldLines
        self.newStart = newStart
        self.newLines = newLines
        self.lines = lines
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        kind = try container.decodeIfPresent(CoreDiffHunkKind.self, forKey: .kind) ?? .diff
        header = try container.decode(String.self, forKey: .header)
        oldStart = try container.decode(Int.self, forKey: .oldStart)
        oldLines = try container.decode(Int.self, forKey: .oldLines)
        newStart = try container.decode(Int.self, forKey: .newStart)
        newLines = try container.decode(Int.self, forKey: .newLines)
        lines = try container.decode([CoreDiffLine].self, forKey: .lines)
    }
}

public enum CoreDiffHunkKind: String, Codable, Equatable, Sendable {
    case diff
    case metadata
}

public struct CoreDiffLine: Codable, Equatable, Sendable {
    public var kind: CoreDiffLineKind
    public var oldLine: Int?
    public var newLine: Int?
    public var content: String
}

public enum CoreDiffLineKind: String, Codable, Equatable, Sendable {
    case context
    case addition
    case deletion
    case hunk
    case metadata
    case note
}
