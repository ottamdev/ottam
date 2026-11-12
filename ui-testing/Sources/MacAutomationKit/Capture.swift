import AppKit
import CoreGraphics
import Foundation
import ImageIO
import ScreenCaptureKit
import UniformTypeIdentifiers

public struct WindowCaptureResult: Codable, Sendable {
    public let targetPID: Int32
    public let windowID: UInt32
    public let windowTitle: String?
    public let outputPath: String
    public let width: Int
    public let height: Int
    public let frame: AXFrame
}

public struct WindowGroupMember: Codable, Sendable {
    public let windowID: UInt32
    public let windowTitle: String?
    public let width: Int
    public let height: Int
    public let frame: AXFrame
}

public struct WindowGroupCaptureResult: Codable, Sendable {
    public let targetPID: Int32
    public let windowIDs: [UInt32]
    public let outputPath: String
    public let width: Int
    public let height: Int
    public let members: [WindowGroupMember]
}

public struct OcclusionProofResult: Codable, Sendable {
    public let baseline: WindowCaptureResult
    public let isolatedWithOverlay: WindowCaptureResult
    public let displayOutputPath: String
    public let overlayFrame: AXFrame
    public let overlayVisiblePixelCount: Int
    public let baselineMarkerPixelCount: Int
    public let isolatedMarkerPixelCount: Int
    public let exactTargetBoundsPreserved: Bool
    public let controlledOverlayExcluded: Bool
}

public enum WindowCapture {
    @MainActor
    public static func capture(
        pid: pid_t,
        windowID: UInt32? = nil,
        outputURL: URL
    ) async throws -> WindowCaptureResult {
        guard CGPreflightScreenCaptureAccess() else {
            throw HarnessAutomationError.screenRecordingPermissionMissing
        }
        NSApplication.shared.setActivationPolicy(.accessory)
        let content = try await SCShareableContent.excludingDesktopWindows(
            false,
            onScreenWindowsOnly: false
        )
        let candidates = content.windows.filter { window in
            guard window.owningApplication?.processID == pid else { return false }
            return windowID == nil || window.windowID == windowID
        }
        guard let window = candidates.max(by: {
            $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height
        }) else {
            throw HarnessAutomationError.windowNotFound(pid)
        }

        let configuration = SCScreenshotConfiguration()
        configuration.setValue(UTType.png, forKey: "contentType")
        configuration.dynamicRange = .sdr
        configuration.displayIntent = .canonical
        configuration.includeChildWindows = true
        configuration.ignoreClipping = false
        configuration.ignoreShadows = true
        configuration.showsCursor = false
        let capture = try await SCScreenshotManager.captureScreenshot(
            contentFilter: SCContentFilter(desktopIndependentWindow: window),
            configuration: configuration
        )
        guard let image = capture.sdrImage else {
            throw HarnessAutomationError.missingCapturedImage
        }
        guard let destination = CGImageDestinationCreateWithURL(
            outputURL as CFURL,
            UTType.png.identifier as CFString,
            1,
            nil
        ) else {
            throw HarnessAutomationError.cannotWriteCapture(outputURL.path)
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else {
            throw HarnessAutomationError.cannotWriteCapture(outputURL.path)
        }
        return WindowCaptureResult(
            targetPID: pid,
            windowID: window.windowID,
            windowTitle: window.title,
            outputPath: outputURL.path,
            width: image.width,
            height: image.height,
            frame: AXFrame(
                x: window.frame.origin.x,
                y: window.frame.origin.y,
                width: window.frame.width,
                height: window.frame.height
            )
        )
    }

    /// Captures each requested window with the authoritative
    /// `desktopIndependentWindow` filter, then places those exact pixels into
    /// one contact sheet. This proves several windows without admitting desktop
    /// pixels or depending on their current overlap and z-order.
    @MainActor
    public static func captureGroup(
        pid: pid_t,
        windowIDs: [UInt32],
        outputURL: URL
    ) async throws -> WindowGroupCaptureResult {
        guard CGPreflightScreenCaptureAccess() else {
            throw HarnessAutomationError.screenRecordingPermissionMissing
        }
        var seenWindowIDs = Set<UInt32>()
        let uniqueWindowIDs = windowIDs.filter {
            seenWindowIDs.insert($0).inserted
        }
        guard uniqueWindowIDs.count >= 2 else {
            throw HarnessAutomationError.windowNotFound(pid)
        }
        NSApplication.shared.setActivationPolicy(.accessory)
        let content = try await SCShareableContent.excludingDesktopWindows(
            false,
            onScreenWindowsOnly: false
        )
        let candidates = content.windows.filter {
            $0.owningApplication?.processID == pid
        }
        let windows = try uniqueWindowIDs.map { windowID in
            guard let window = candidates.first(where: { $0.windowID == windowID }) else {
                throw HarnessAutomationError.windowNotFound(pid)
            }
            return window
        }

        var captures: [(window: SCWindow, image: CGImage)] = []
        for window in windows {
            let configuration = SCScreenshotConfiguration()
            configuration.setValue(UTType.png, forKey: "contentType")
            configuration.dynamicRange = .sdr
            configuration.displayIntent = .canonical
            configuration.includeChildWindows = true
            configuration.ignoreClipping = false
            configuration.ignoreShadows = true
            configuration.showsCursor = false
            let capture = try await SCScreenshotManager.captureScreenshot(
                contentFilter: SCContentFilter(desktopIndependentWindow: window),
                configuration: configuration
            )
            guard let image = capture.sdrImage else {
                throw HarnessAutomationError.missingCapturedImage
            }
            captures.append((window, image))
        }

        let gap = 16
        let width = captures.reduce(0) { $0 + $1.image.width }
            + gap * (captures.count - 1)
        let height = captures.map(\.image.height).max() ?? 0
        guard let context = CGContext(
            data: nil,
            width: width,
            height: height,
            bitsPerComponent: 8,
            bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else {
            throw HarnessAutomationError.cannotWriteCapture(outputURL.path)
        }
        context.setFillColor(CGColor(gray: 0.12, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        var x = 0
        for capture in captures {
            context.draw(
                capture.image,
                in: CGRect(
                    x: x,
                    y: height - capture.image.height,
                    width: capture.image.width,
                    height: capture.image.height
                )
            )
            x += capture.image.width + gap
        }
        guard let image = context.makeImage() else {
            throw HarnessAutomationError.missingCapturedImage
        }
        try writePNG(image, to: outputURL)

        return WindowGroupCaptureResult(
            targetPID: pid,
            windowIDs: uniqueWindowIDs,
            outputPath: outputURL.path,
            width: image.width,
            height: image.height,
            members: captures.map { capture in
                WindowGroupMember(
                    windowID: capture.window.windowID,
                    windowTitle: capture.window.title,
                    width: capture.image.width,
                    height: capture.image.height,
                    frame: AXFrame(
                        x: capture.window.frame.origin.x,
                        y: capture.window.frame.origin.y,
                        width: capture.window.frame.width,
                        height: capture.window.frame.height
                    )
                )
            }
        )
    }

    /// Proves the production capture contract instead of merely assuming the
    /// `desktopIndependentWindow` filter works. A driver-owned opaque marker is
    /// placed across the target window, a display capture proves that marker
    /// was visibly intersecting the window, and a second independent-window
    /// capture proves it was excluded without clipping the target.
    @MainActor
    public static func proveOcclusionIsolation(
        pid: pid_t,
        windowID: UInt32?,
        baselineURL: URL,
        displayURL: URL,
        isolatedURL: URL
    ) async throws -> OcclusionProofResult {
        guard CGPreflightScreenCaptureAccess() else {
            throw HarnessAutomationError.screenRecordingPermissionMissing
        }
        let baseline = try await capture(pid: pid, windowID: windowID, outputURL: baselineURL)
        let content = try await SCShareableContent.excludingDesktopWindows(
            false,
            onScreenWindowsOnly: false
        )
        guard let window = content.windows.first(where: {
            $0.owningApplication?.processID == pid && $0.windowID == baseline.windowID
        }) else {
            throw HarnessAutomationError.windowNotFound(pid)
        }
        let center = CGPoint(x: window.frame.midX, y: window.frame.midY)
        guard let display = content.displays.first(where: { $0.frame.contains(center) }),
              let screen = screen(for: display)
        else {
            throw HarnessAutomationError.displayNotFound
        }

        let markerWidth = min(220, max(80, window.frame.width * 0.35))
        let markerHeight = min(140, max(60, window.frame.height * 0.25))
        let markerCaptureFrame = CGRect(
            x: window.frame.midX - markerWidth / 2,
            y: window.frame.midY - markerHeight / 2,
            width: markerWidth,
            height: markerHeight
        )
        let markerAppKitFrame = CGRect(
            x: screen.frame.minX + markerCaptureFrame.minX - display.frame.minX,
            y: screen.frame.maxY - (markerCaptureFrame.maxY - display.frame.minY),
            width: markerCaptureFrame.width,
            height: markerCaptureFrame.height
        )
        let overlay = OcclusionMarkerOverlay(frame: markerAppKitFrame)
        overlay.show()
        defer { overlay.close() }
        try await Task.sleep(for: .milliseconds(250))
        try await captureDisplay(display, outputURL: displayURL)
        let isolated = try await capture(
            pid: pid,
            windowID: baseline.windowID,
            outputURL: isolatedURL
        )

        let displayPixels = try markerPixelCount(
            displayURL,
            markerFrame: markerCaptureFrame,
            displayFrame: display.frame
        )
        let baselinePixels = try markerPixelCount(baselineURL)
        let isolatedPixels = try markerPixelCount(isolatedURL)
        let boundsPreserved =
            baseline.windowID == isolated.windowID
            && baseline.width == isolated.width
            && baseline.height == isolated.height
            && baseline.frame == isolated.frame
        let overlayExcluded =
            displayPixels >= 500
            && isolatedPixels <= baselinePixels + 32
        guard displayPixels >= 500 else {
            throw HarnessAutomationError.occlusionMarkerNotVisible
        }
        guard boundsPreserved, overlayExcluded else {
            throw HarnessAutomationError.occlusionIsolationFailed
        }
        return OcclusionProofResult(
            baseline: baseline,
            isolatedWithOverlay: isolated,
            displayOutputPath: displayURL.path,
            overlayFrame: AXFrame(
                x: markerCaptureFrame.minX,
                y: markerCaptureFrame.minY,
                width: markerCaptureFrame.width,
                height: markerCaptureFrame.height
            ),
            overlayVisiblePixelCount: displayPixels,
            baselineMarkerPixelCount: baselinePixels,
            isolatedMarkerPixelCount: isolatedPixels,
            exactTargetBoundsPreserved: boundsPreserved,
            controlledOverlayExcluded: overlayExcluded
        )
    }

    @MainActor
    private static func captureDisplay(_ display: SCDisplay, outputURL: URL) async throws {
        let configuration = SCScreenshotConfiguration()
        configuration.setValue(UTType.png, forKey: "contentType")
        configuration.dynamicRange = .sdr
        configuration.displayIntent = .canonical
        configuration.showsCursor = false
        configuration.width = Int(display.width)
        configuration.height = Int(display.height)
        let capture = try await SCScreenshotManager.captureScreenshot(
            contentFilter: SCContentFilter(display: display, excludingWindows: []),
            configuration: configuration
        )
        guard let image = capture.sdrImage else {
            throw HarnessAutomationError.missingCapturedImage
        }
        guard let destination = CGImageDestinationCreateWithURL(
            outputURL as CFURL,
            UTType.png.identifier as CFString,
            1,
            nil
        ) else {
            throw HarnessAutomationError.cannotWriteCapture(outputURL.path)
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else {
            throw HarnessAutomationError.cannotWriteCapture(outputURL.path)
        }
    }

    private static func writePNG(_ image: CGImage, to outputURL: URL) throws {
        guard let destination = CGImageDestinationCreateWithURL(
            outputURL as CFURL,
            UTType.png.identifier as CFString,
            1,
            nil
        ) else {
            throw HarnessAutomationError.cannotWriteCapture(outputURL.path)
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else {
            throw HarnessAutomationError.cannotWriteCapture(outputURL.path)
        }
    }

    private static func screen(for display: SCDisplay) -> NSScreen? {
        NSScreen.screens.first { screen in
            guard let number = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")]
                as? NSNumber
            else {
                return false
            }
            return number.uint32Value == display.displayID
        }
    }

    private static func markerPixelCount(
        _ url: URL,
        markerFrame: CGRect? = nil,
        displayFrame: CGRect? = nil
    ) throws -> Int {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
              let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
        else {
            throw HarnessAutomationError.cannotReadCapture(url.path)
        }
        let bytesPerRow = image.width * 4
        var pixels = [UInt8](repeating: 0, count: bytesPerRow * image.height)
        guard let context = CGContext(
            data: &pixels,
            width: image.width,
            height: image.height,
            bitsPerComponent: 8,
            bytesPerRow: bytesPerRow,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else {
            throw HarnessAutomationError.cannotReadCapture(url.path)
        }
        context.draw(
            image,
            in: CGRect(x: 0, y: 0, width: image.width, height: image.height)
        )
        let regions: [CGRect]
        if let markerFrame, let displayFrame,
           displayFrame.width > 0, displayFrame.height > 0
        {
            let scaleX = CGFloat(image.width) / displayFrame.width
            let scaleY = CGFloat(image.height) / displayFrame.height
            let local = CGRect(
                x: (markerFrame.minX - displayFrame.minX) * scaleX,
                y: (markerFrame.minY - displayFrame.minY) * scaleY,
                width: markerFrame.width * scaleX,
                height: markerFrame.height * scaleY
            ).insetBy(dx: -4, dy: -4)
            let verticallyFlipped = CGRect(
                x: local.minX,
                y: CGFloat(image.height) - local.maxY,
                width: local.width,
                height: local.height
            )
            // ScreenCaptureKit and CoreGraphics use opposite vertical origins
            // on some display configurations. Restrict the proof to the
            // expected overlay rectangle under either origin convention.
            regions = [local, verticallyFlipped]
        } else {
            regions = [CGRect(x: 0, y: 0, width: image.width, height: image.height)]
        }
        var count = 0
        for y in 0..<image.height {
            let row = y * bytesPerRow
            for x in 0..<image.width {
                let point = CGPoint(x: x, y: y)
                guard regions.contains(where: { $0.contains(point) }) else { continue }
                let pixel = row + x * 4
                let components = pixels[pixel..<(pixel + 4)]
                // The CGContext byte order is platform-native. Opaque magenta
                // is three high components (red, blue, alpha) and one low
                // component (green) in either RGBA or ABGR memory order.
                if components.filter({ $0 > 220 }).count >= 3,
                   components.contains(where: { $0 < 100 }) {
                    count += 1
                }
            }
        }
        return count
    }
}
