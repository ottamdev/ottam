// swift-tools-version: 6.2

import PackageDescription

let package = Package(
    name: "ottam",
    platforms: [
        .macOS(.v26)
    ],
    products: [
        .executable(name: "OttamApp", targets: ["OttamApp"]),
        .executable(name: "ottam", targets: ["OttamCLI"]),
        .executable(name: "OttamBackendClientTests", targets: ["OttamBackendClientTests"]),
        .library(name: "OttamBackendClient", targets: ["OttamBackendClient"]),
        .library(name: "OttamAccessibility", targets: ["OttamAccessibility"]),
        .library(name: "OttamCore", targets: ["OttamCore"]),
        .library(name: "OttamGhostty", targets: ["OttamGhostty"]),
        .library(name: "OttamInstrumentation", targets: ["OttamInstrumentation"]),
        .library(name: "OttamMarkdownEngine", targets: ["OttamMarkdownEngine"]),
        .library(name: "OttamPresentationLogic", targets: ["OttamPresentationLogic"]),
        .library(name: "OttamTranscriptFixtures", targets: ["OttamTranscriptFixtures"]),
        .library(name: "OttamWorkspaceLogic", targets: ["OttamWorkspaceLogic"])
    ],
    targets: [
        .binaryTarget(
            name: "GhosttyKit",
            path: "Vendor/ottam-ghostty/GhosttyKit.xcframework"
        ),
        .executableTarget(
            name: "OttamApp",
            dependencies: [
                "OttamBackendClient",
                "OttamAccessibility",
                "OttamCore",
                "OttamGhostty",
                "OttamInstrumentation",
                "OttamMarkdownEngine",
                "OttamPresentationLogic",
                "OttamWorkspaceLogic"
            ],
            resources: [
                .process("../../Resources")
            ]
        ),
        .executableTarget(
            name: "OttamCLI"
        ),
        .target(
            name: "OttamBackendClient",
            dependencies: ["OttamCore"]
        ),
        .target(
            name: "OttamAccessibility"
        ),
        // Deterministic markdown corpora for the transcript-renderer
        // measurement gates. Foundation-only and dependency-free so the unit
        // benchmarks and the UI-test scenarios stream identical bytes — their
        // numbers are only comparable if the fixture is literally the same
        // code. The app does not link it.
        // Protocol: docs/operations/performance/transcript-renderer.md
        .target(
            name: "OttamTranscriptFixtures"
        ),
        // Signpost schema and body-evaluation counters for the
        // transcript-renderer gates. A leaf so both the app and the markdown
        // engine can instrument the stable seams used by the native renderer.
        // Protocol: docs/operations/performance/transcript-renderer.md
        .target(
            name: "OttamInstrumentation"
        ),
        .target(
            name: "OttamCore"
        ),
        // Pure, synchronous Workspace rules — sidebar grouping and working-root
        // resolution. Kept out of the app target so it stays testable and free
        // of SwiftUI; depends only on the generated protocol types.
        .target(
            name: "OttamWorkspaceLogic",
            dependencies: ["OttamBackendClient"]
        ),
        // Renderer-neutral Markdown parsing, styling, tokenization and image
        // loading. Ownership boundaries:
        // docs/architecture/app/chat/transcript/markdown-renderer.md.
        .target(
            name: "OttamMarkdownEngine",
            dependencies: ["OttamInstrumentation"]
        ),
        // Pure app-wide presentation decisions. Views consume these typed
        // values; native tests own their full state matrices without mounting
        // SwiftUI or launching Ottam.
        .target(
            name: "OttamPresentationLogic",
            dependencies: ["OttamBackendClient", "OttamWorkspaceLogic"]
        ),
        .target(
            name: "OttamGhostty",
            dependencies: ["GhosttyKit"],
            linkerSettings: [
                .linkedLibrary("c++"),
                .linkedFramework("Metal"),
                .linkedFramework("QuartzCore"),
                .linkedFramework("IOSurface"),
                .linkedFramework("UniformTypeIdentifiers"),
                .linkedFramework("Carbon")
            ]
        ),
        .executableTarget(
            name: "OttamBackendClientTests",
            dependencies: ["OttamBackendClient"],
            path: "Tests/OttamBackendClientTests"
        ),
        .testTarget(
            name: "OttamBackendTransportTests",
            dependencies: ["OttamBackendClient"],
            path: "Tests/OttamBackendTransportTests"
        ),
        .testTarget(
            name: "OttamWorkspaceLogicTests",
            dependencies: ["OttamWorkspaceLogic"],
            path: "Tests/OttamWorkspaceLogicTests"
        ),
        .testTarget(
            name: "OttamMarkdownEngineTests",
            dependencies: [
                "OttamInstrumentation",
                "OttamMarkdownEngine",
                "OttamTranscriptFixtures"
            ],
            path: "Tests/OttamMarkdownEngineTests"
        ),
        .testTarget(
            name: "OttamAppTests",
            dependencies: ["OttamApp"],
            path: "Tests/OttamAppTests"
        ),
        .testTarget(
            name: "OttamPresentationLogicTests",
            dependencies: ["OttamPresentationLogic"],
            path: "Tests/OttamPresentationLogicTests"
        )
    ],
    swiftLanguageModes: [.v5]
)
