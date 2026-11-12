import Foundation

let arguments = Array(CommandLine.arguments.dropFirst())

switch arguments.first {
case nil, "--help", "-h":
    printHelp()
case "version":
    print("ottam 0.1.0")
case "init":
    let path = arguments.dropFirst().first ?? FileManager.default.currentDirectoryPath
    do {
        try ProjectInitializer().initializeProject(at: URL(fileURLWithPath: path))
        print("Initialized Ottam project at \(path)")
    } catch {
        fputs("error: \(error.localizedDescription)\n", stderr)
        exit(1)
    }
default:
    fputs("error: unknown command '\(arguments[0])'\n\n", stderr)
    printHelp()
    exit(64)
}

private func printHelp() {
    print(
        """
        ottam

        Usage:
          ottam init [path]    Create .ottam starter files for a repo
          ottam version        Print version
          ottam --help         Show help
        """
    )
}

private struct ProjectInitializer {
    func initializeProject(at rootURL: URL) throws {
        let ottamURL = rootURL.appendingPathComponent(".ottam", isDirectory: true)
        try FileManager.default.createDirectory(
            at: ottamURL,
            withIntermediateDirectories: true
        )
    }
}
