import AppKit
import ApplicationServices
import CoreGraphics
import Foundation
@_exported import UIHarnessCore

public typealias ProcessIdentity = HarnessProcessIdentity
public typealias LaunchedApplication = HarnessLaunchedApplication
public typealias ApplicationLifecycle = HarnessApplicationLifecycle
public typealias AutomationError = HarnessAutomationError

public struct AutomationPermissionState: Codable, Sendable {
    public let accessibilityAllowed: Bool
    public let screenRecordingAllowed: Bool
    public let driverBundleIdentifier: String
    public let driverBundlePath: String
    public let driverPID: Int32
    public let screenLocked: Bool
    public let onConsole: Bool

    public init() {
        let session = CGSessionCopyCurrentDictionary() as? [String: Any]
        accessibilityAllowed = AXIsProcessTrusted()
        screenRecordingAllowed = CGPreflightScreenCaptureAccess()
        driverBundleIdentifier = Bundle.main.bundleIdentifier ?? "unknown"
        driverBundlePath = Bundle.main.bundlePath
        driverPID = ProcessInfo.processInfo.processIdentifier
        screenLocked = session?["CGSSessionScreenIsLocked"] as? Bool ?? false
        onConsole = session?[kCGSessionOnConsoleKey as String] as? Bool ?? true
    }

    public static func requestAccessibility() -> Bool {
        let options = [
            kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true,
        ] as CFDictionary
        return AXIsProcessTrustedWithOptions(options)
    }

    public static func requestScreenRecording() -> Bool {
        CGRequestScreenCaptureAccess()
    }
}

public struct AXFrame: Codable, Equatable, Sendable {
    public let x: Double
    public let y: Double
    public let width: Double
    public let height: Double
}

public struct NormalizedAXNode: Codable, Sendable {
    public let role: String?
    public let subrole: String?
    public let identifier: String?
    public let label: String?
    public let title: String?
    public let value: String?
    public let placeholder: String?
    public let frame: AXFrame?
    public let enabled: Bool?
    public let selected: Bool?
    public let focused: Bool?
    public let actions: [String]?
    public let children: [NormalizedAXNode]

    public init(
        role: String?,
        subrole: String?,
        identifier: String?,
        label: String?,
        title: String?,
        value: String?,
        placeholder: String?,
        frame: AXFrame?,
        enabled: Bool?,
        selected: Bool?,
        focused: Bool?,
        actions: [String]?,
        children: [NormalizedAXNode]
    ) {
        self.role = role
        self.subrole = subrole
        self.identifier = identifier
        self.label = label
        self.title = title
        self.value = value
        self.placeholder = placeholder
        self.frame = frame
        self.enabled = enabled
        self.selected = selected
        self.focused = focused
        self.actions = actions
        self.children = children
    }
}

public struct AXTreeCapture: Codable, Sendable {
    public let schemaVersion: Int
    public let targetPID: Int32
    public let scopeIdentifier: String?
    public let maxDepth: Int
    public let maxNodes: Int
    public let capturedNodes: Int
    public let truncatedNodes: Int
    public let omittedValues: Int
    public let durationMilliseconds: Double
    public let root: NormalizedAXNode

    public init(
        schemaVersion: Int,
        targetPID: Int32,
        scopeIdentifier: String?,
        maxDepth: Int,
        maxNodes: Int,
        capturedNodes: Int,
        truncatedNodes: Int,
        omittedValues: Int,
        durationMilliseconds: Double,
        root: NormalizedAXNode
    ) {
        self.schemaVersion = schemaVersion
        self.targetPID = targetPID
        self.scopeIdentifier = scopeIdentifier
        self.maxDepth = maxDepth
        self.maxNodes = maxNodes
        self.capturedNodes = capturedNodes
        self.truncatedNodes = truncatedNodes
        self.omittedValues = omittedValues
        self.durationMilliseconds = durationMilliseconds
        self.root = root
    }
}

public struct AXCaptureLimits: Sendable {
    public var maxDepth: Int
    public var maxNodes: Int
    public var maxValueLength: Int
    public var timeout: TimeInterval

    public init(
        maxDepth: Int = 12,
        maxNodes: Int = 500,
        maxValueLength: Int = 2_000,
        timeout: TimeInterval = 3
    ) {
        self.maxDepth = maxDepth
        self.maxNodes = maxNodes
        self.maxValueLength = maxValueLength
        self.timeout = timeout
    }
}

public enum AXAutomation {
    public static func captureTree(
        pid: pid_t,
        scopeIdentifier: String? = nil,
        limits: AXCaptureLimits = .init()
    ) throws -> AXTreeCapture {
        guard AXIsProcessTrusted() else {
            throw HarnessAutomationError.accessibilityPermissionMissing
        }
        let started = Date()
        let application = AXUIElementCreateApplication(pid)
        let root: AXUIElement
        if let scopeIdentifier {
            guard let found = try find(
                identifier: scopeIdentifier,
                from: application,
                // Locating a narrow scope and encoding that scope have different
                // budgets. App menus can precede the content window in the AX
                // graph, so a small output policy must not make valid elements
                // undiscoverable.
                maxNodes: max(limits.maxNodes, 1_000),
                deadline: started.addingTimeInterval(limits.timeout)
            ) else {
                throw HarnessAutomationError.elementNotFound(scopeIdentifier)
            }
            root = found
        } else {
            root = application
        }

        var state = CaptureState(limits: limits, deadline: started.addingTimeInterval(limits.timeout))
        let node = try normalize(root, depth: 0, state: &state)
        return AXTreeCapture(
            schemaVersion: 1,
            targetPID: pid,
            scopeIdentifier: scopeIdentifier,
            maxDepth: limits.maxDepth,
            maxNodes: limits.maxNodes,
            capturedNodes: state.capturedNodes,
            truncatedNodes: state.truncatedNodes,
            omittedValues: state.omittedValues,
            durationMilliseconds: Date().timeIntervalSince(started) * 1_000,
            root: node
        )
    }

    public static func perform(
        action: String,
        identifier: String,
        pid: pid_t,
        value: String? = nil,
        timeout: TimeInterval = 3
    ) throws {
        guard AXIsProcessTrusted() else {
            throw HarnessAutomationError.accessibilityPermissionMissing
        }
        let application = AXUIElementCreateApplication(pid)
        guard let element = try find(
            identifier: identifier,
            from: application,
            maxNodes: 1_000,
            deadline: Date().addingTimeInterval(timeout)
        ) else {
            throw HarnessAutomationError.elementNotFound(identifier)
        }
        let result: AXError
        switch action {
        case "press":
            result = AXUIElementPerformAction(element, kAXPressAction as CFString)
        case "focus":
            result = AXUIElementSetAttributeValue(
                element,
                kAXFocusedAttribute as CFString,
                kCFBooleanTrue
            )
        case "setValue":
            guard let value else { throw HarnessAutomationError.valueRequired }
            result = AXUIElementSetAttributeValue(
                element,
                kAXValueAttribute as CFString,
                value as CFTypeRef
            )
        default:
            throw HarnessAutomationError.unsupportedAction(action)
        }
        guard result == .success else {
            throw HarnessAutomationError.accessibilityFailure(result.rawValue)
        }
    }

    public static func selectListRow(
        named name: String,
        waitForText expectedText: String,
        pid: pid_t,
        timeout: TimeInterval = 3
    ) throws {
        guard AXIsProcessTrusted() else {
            throw HarnessAutomationError.accessibilityPermissionMissing
        }
        let application = AXUIElementCreateApplication(pid)
        let deadline = Date().addingTimeInterval(timeout)
        // Only a match inside a row counts: a workspace window can carry the
        // same label on a non-row element (the terminal surface is "Terminal").
        guard let text = try findExactText(
            text: name,
            from: application,
            maxNodes: 1_000,
            deadline: deadline,
            accepting: { ancestor(of: $0, role: kAXRowRole as String) != nil }
        ), let row = ancestor(of: text, role: kAXRowRole as String)
        else {
            throw HarnessAutomationError.elementNotFound(name)
        }
        let result = AXUIElementSetAttributeValue(
            row,
            kAXSelectedAttribute as CFString,
            kCFBooleanTrue
        )
        guard result == .success else {
            throw HarnessAutomationError.accessibilityFailure(result.rawValue)
        }
        try waitForExactText(expectedText, from: application, deadline: deadline)
    }

    public static func pressControl(
        named name: String,
        waitForText expectedText: String,
        pid: pid_t,
        timeout: TimeInterval = 3
    ) throws {
        guard AXIsProcessTrusted() else {
            throw HarnessAutomationError.accessibilityPermissionMissing
        }
        let application = AXUIElementCreateApplication(pid)
        let deadline = Date().addingTimeInterval(timeout)
        guard let control = try findNamedControl(
            text: name,
            from: application,
            maxNodes: 1_000,
            deadline: deadline
        ) else {
            throw HarnessAutomationError.elementNotFound(name)
        }
        var candidate: AXUIElement? = control
        while let element = candidate {
            if actionNames(element).contains(kAXPressAction as String) {
                let result = AXUIElementPerformAction(
                    element,
                    kAXPressAction as CFString
                )
                guard result == .success else {
                    throw HarnessAutomationError.accessibilityFailure(result.rawValue)
                }
                try waitForExactText(expectedText, from: application, deadline: deadline)
                return
            }
            candidate = elementAttribute(kAXParentAttribute, element: element)
        }
        throw HarnessAutomationError.unsupportedAction("press named \(name)")
    }

    private struct CaptureState {
        let limits: AXCaptureLimits
        let deadline: Date
        var capturedNodes = 0
        var truncatedNodes = 0
        var omittedValues = 0
        var visitedElements: [AXUIElement] = []
    }

    private static func normalize(
        _ element: AXUIElement,
        depth: Int,
        state: inout CaptureState
    ) throws -> NormalizedAXNode {
        guard Date() <= state.deadline else {
            throw HarnessAutomationError.semanticCaptureTimedOut
        }
        state.visitedElements.append(element)
        state.capturedNodes += 1
        let role = stringAttribute(kAXRoleAttribute, element: element, state: &state)
        let subrole = stringAttribute(kAXSubroleAttribute, element: element, state: &state)
        let identifier = stringAttribute(kAXIdentifierAttribute, element: element, state: &state)
        let label = stringAttribute(kAXDescriptionAttribute, element: element, state: &state)
        let title = stringAttribute(kAXTitleAttribute, element: element, state: &state)
        let value = stringAttribute(kAXValueAttribute, element: element, state: &state)
        let placeholder = stringAttribute(
            kAXPlaceholderValueAttribute,
            element: element,
            state: &state
        )

        var children: [NormalizedAXNode] = []
        let rawChildren = childElements(element).filter { child in
            !state.visitedElements.contains(where: { CFEqual($0, child) })
        }
        if depth >= state.limits.maxDepth || state.capturedNodes >= state.limits.maxNodes {
            state.truncatedNodes += rawChildren.count
        } else {
            for child in rawChildren {
                if state.capturedNodes >= state.limits.maxNodes {
                    state.truncatedNodes += 1
                    continue
                }
                children.append(try normalize(child, depth: depth + 1, state: &state))
            }
        }

        return NormalizedAXNode(
            role: role,
            subrole: subrole,
            identifier: identifier,
            label: label,
            title: title,
            value: value,
            placeholder: placeholder,
            frame: frame(element),
            enabled: boolAttribute(kAXEnabledAttribute, element: element),
            selected: boolAttribute(kAXSelectedAttribute, element: element),
            focused: boolAttribute(kAXFocusedAttribute, element: element),
            actions: actionNames(element),
            children: children
        )
    }

    private static func actionNames(_ element: AXUIElement) -> [String] {
        var names: CFArray?
        guard AXUIElementCopyActionNames(element, &names) == .success,
              let names = names as? [String]
        else {
            return []
        }
        return names.sorted()
    }

    private static func find(
        identifier: String,
        from root: AXUIElement,
        maxNodes: Int,
        deadline: Date
    ) throws -> AXUIElement? {
        // Application children place several duplicated menu graphs before
        // content windows on recent macOS releases. Search windows first so
        // product identifiers remain discoverable under a bounded budget.
        var queue = [
            elementAttribute(kAXFocusedWindowAttribute, element: root),
            elementAttribute(kAXMainWindowAttribute, element: root),
        ].compactMap { $0 }
            + elementsAttribute(kAXWindowsAttribute, element: root)
            + [root]
        var nextIndex = 0
        var visited = 0
        var visitedElements: [AXUIElement] = []
        while nextIndex < queue.count, visited < maxNodes, Date() <= deadline {
            let element = queue[nextIndex]
            nextIndex += 1
            guard !visitedElements.contains(where: { CFEqual($0, element) }) else {
                continue
            }
            visitedElements.append(element)
            visited += 1
            if rawStringAttribute(kAXIdentifierAttribute, element: element) == identifier {
                return element
            }
            queue.append(contentsOf: childElements(element))
        }
        if Date() > deadline {
            throw HarnessAutomationError.elementSearchTimedOut(identifier)
        }
        return nil
    }

    /// Breadth-first across every window, so the shallowest match wins. Callers
    /// that need a particular kind of match pass `accepting`; otherwise an
    /// unrelated element with the same label in another window can shadow it.
    private static func findExactText(
        text: String,
        from root: AXUIElement,
        maxNodes: Int,
        deadline: Date,
        accepting accepts: (AXUIElement) -> Bool = { _ in true }
    ) throws -> AXUIElement? {
        for scope in namedSearchScopes(root) {
            if let element = firstElement(from: scope, maxNodes: maxNodes, deadline: deadline, where: { element in
                let values = [
                    rawStringAttribute(kAXDescriptionAttribute, element: element),
                    rawStringAttribute(kAXTitleAttribute, element: element),
                    rawStringAttribute(kAXValueAttribute, element: element),
                ]
                return values.contains(text) && accepts(element)
            }) {
                return element
            }
        }
        if Date() > deadline {
            throw HarnessAutomationError.elementSearchTimedOut(text)
        }
        return nil
    }

    private static func findNamedControl(
        text: String,
        from root: AXUIElement,
        maxNodes: Int,
        deadline: Date
    ) throws -> AXUIElement? {
        for scope in namedSearchScopes(root) {
            if let element = firstElement(from: scope, maxNodes: maxNodes, deadline: deadline, where: { element in
                let values = [
                    rawStringAttribute(kAXDescriptionAttribute, element: element),
                    rawStringAttribute(kAXTitleAttribute, element: element),
                    rawStringAttribute(kAXValueAttribute, element: element),
                ].compactMap { $0 }
                return values.contains(text) || values.contains(where: { value in
                    value.split(separator: ",").contains { component in
                        component.trimmingCharacters(in: .whitespacesAndNewlines) == text
                    }
                })
            }) {
                return element
            }
        }
        if Date() > deadline {
            throw HarnessAutomationError.elementSearchTimedOut(text)
        }
        return nil
    }

    /// Labels are not unique across windows (each workspace window has its
    /// own New Chat row), and a breadth-first walk of every window
    /// returns whichever match sits shallowest. Named searches cover the
    /// focused window on its own first; the whole application is the fallback.
    private static func namedSearchScopes(_ root: AXUIElement) -> [[AXUIElement]] {
        let everything = elementsAttribute(kAXWindowsAttribute, element: root) + [root]
        guard let focused = elementAttribute(kAXFocusedWindowAttribute, element: root) else {
            return [everything]
        }
        return [[focused], everything]
    }

    private static func firstElement(
        from roots: [AXUIElement],
        maxNodes: Int,
        deadline: Date,
        where matches: (AXUIElement) -> Bool
    ) -> AXUIElement? {
        var queue = roots
        var nextIndex = 0
        var visited = 0
        var visitedElements: [AXUIElement] = []
        while nextIndex < queue.count, visited < maxNodes, Date() <= deadline {
            let element = queue[nextIndex]
            nextIndex += 1
            guard !visitedElements.contains(where: { CFEqual($0, element) }) else {
                continue
            }
            visitedElements.append(element)
            visited += 1
            if matches(element) {
                return element
            }
            queue.append(contentsOf: childElements(element))
        }
        return nil
    }

    private static func waitForExactText(
        _ text: String,
        from root: AXUIElement,
        deadline: Date
    ) throws {
        while Date() <= deadline {
            if try findExactText(
                text: text,
                from: root,
                maxNodes: 1_000,
                deadline: deadline
            ) != nil {
                return
            }
            Thread.sleep(forTimeInterval: 0.05)
        }
        throw HarnessAutomationError.elementSearchTimedOut(text)
    }

    private static func ancestor(
        of element: AXUIElement,
        role: String
    ) -> AXUIElement? {
        var candidate: AXUIElement? = element
        while let current = candidate {
            if rawStringAttribute(kAXRoleAttribute, element: current) == role {
                return current
            }
            candidate = elementAttribute(kAXParentAttribute, element: current)
        }
        return nil
    }

    private static func childElements(_ element: AXUIElement) -> [AXUIElement] {
        elementsAttribute(kAXChildrenAttribute, element: element)
    }

    private static func elementAttribute(
        _ name: String,
        element: AXUIElement
    ) -> AXUIElement? {
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(
            element,
            name as CFString,
            &value
        ) == .success,
        let value,
        CFGetTypeID(value) == AXUIElementGetTypeID()
        else {
            return nil
        }
        return (value as! AXUIElement)
    }

    private static func elementsAttribute(
        _ name: String,
        element: AXUIElement
    ) -> [AXUIElement] {
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(
            element,
            name as CFString,
            &value
        ) == .success else {
            return []
        }
        return value as? [AXUIElement] ?? []
    }

    private static func rawStringAttribute(
        _ name: String,
        element: AXUIElement
    ) -> String? {
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success,
              let value
        else {
            return nil
        }
        if let string = value as? String {
            return string
        }
        if CFGetTypeID(value) == CFBooleanGetTypeID() {
            return CFBooleanGetValue((value as! CFBoolean)) ? "true" : "false"
        }
        if let number = value as? NSNumber {
            return number.stringValue
        }
        return nil
    }

    private static func stringAttribute(
        _ name: String,
        element: AXUIElement,
        state: inout CaptureState
    ) -> String? {
        guard let value = rawStringAttribute(name, element: element) else { return nil }
        if value.count <= state.limits.maxValueLength {
            return value
        }
        state.omittedValues += 1
        return String(value.prefix(state.limits.maxValueLength)) + "…"
    }

    private static func boolAttribute(_ name: String, element: AXUIElement) -> Bool? {
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success,
              let value,
              CFGetTypeID(value) == CFBooleanGetTypeID()
        else {
            return nil
        }
        return CFBooleanGetValue((value as! CFBoolean))
    }

    private static func frame(_ element: AXUIElement) -> AXFrame? {
        var positionValue: CFTypeRef?
        var sizeValue: CFTypeRef?
        guard AXUIElementCopyAttributeValue(
            element,
            kAXPositionAttribute as CFString,
            &positionValue
        ) == .success,
        AXUIElementCopyAttributeValue(
            element,
            kAXSizeAttribute as CFString,
            &sizeValue
        ) == .success,
        let positionValue,
        let sizeValue,
        CFGetTypeID(positionValue) == AXValueGetTypeID(),
        CFGetTypeID(sizeValue) == AXValueGetTypeID()
        else {
            return nil
        }
        var point = CGPoint.zero
        var size = CGSize.zero
        guard AXValueGetValue(positionValue as! AXValue, .cgPoint, &point),
              AXValueGetValue(sizeValue as! AXValue, .cgSize, &size)
        else {
            return nil
        }
        return AXFrame(
            x: point.x,
            y: point.y,
            width: size.width,
            height: size.height
        )
    }
}
