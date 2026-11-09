import Foundation
import AppKit
import ApplicationServices
import ScreenCaptureKit

enum DesktopError: LocalizedError {
    case unavailable(String)
    var errorDescription: String? { switch self { case .unavailable(let message): return message } }
}

@MainActor
final class DesktopHelper {
    var snapshotID = ""
    var elements: [String: AXUIElement] = [:]
    var screenshotSnapshot = false
    var screenshotOrigin = CGPoint.zero
    var screenshotSize = CGSize.zero
    let artifacts = ProcessInfo.processInfo.environment["HARNESS_ARTIFACT_DIR"] ?? NSTemporaryDirectory()

    func requireAccessibility() throws {
        let options = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
        if !AXIsProcessTrustedWithOptions(options) { throw DesktopError.unavailable("Accessibility permission is missing. Enable the Harness app/helper in System Settings → Privacy & Security → Accessibility, then restart the desktop session.") }
    }
    func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
        var value: CFTypeRef?
        return AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success ? value : nil
    }
    func text(_ element: AXUIElement, _ name: String) -> String? { attribute(element, name) as? String }

    func observe(_ params: [String: Any]) throws -> [String: Any] {
        try requireAccessibility()
        snapshotID = UUID().uuidString; elements.removeAll(); screenshotSnapshot = false
        let running = NSWorkspace.shared.runningApplications.filter { $0.activationPolicy == .regular && !$0.isTerminated }
        let requested = params["app"] as? String
        let app: NSRunningApplication?
        if let requested = requested {
            app = running.first { $0.bundleIdentifier == requested || $0.localizedName?.caseInsensitiveCompare(requested) == .orderedSame || String($0.processIdentifier) == requested }
            if app == nil { throw DesktopError.unavailable("Requested application is not running") }
        } else { app = NSWorkspace.shared.frontmostApplication }
        var nodes: [[String: Any]] = []
        let maxDepth = min(max(params["depth"] as? Int ?? 4, 1), 8)
        if let app = app {
            let root = AXUIElementCreateApplication(app.processIdentifier)
            AXUIElementSetMessagingTimeout(root, 2)
            func visit(_ element: AXUIElement, _ depth: Int) {
                if nodes.count >= 200 || depth > maxDepth { return }
                let ref = "e\(nodes.count + 1)"; elements[ref] = element
                let role = text(element, kAXRoleAttribute) ?? "unknown"
                var node: [String: Any] = ["ref": ref, "depth": depth, "role": role]
                for (key, attr) in [("title", kAXTitleAttribute), ("description", kAXDescriptionAttribute), ("help", kAXHelpAttribute)] {
                    if let value = text(element, attr), !value.isEmpty { node[key] = String(value.prefix(180)) }
                }
                if role == "AXSecureTextField" { node["value"] = "[password]" }
                else if let value = text(element, kAXValueAttribute) { node["value"] = String(value.prefix(300)) }
                if let enabled = attribute(element, kAXEnabledAttribute) as? Bool { node["enabled"] = enabled }
                if let position = attribute(element, kAXPositionAttribute), CFGetTypeID(position) == AXValueGetTypeID(),
                   let size = attribute(element, kAXSizeAttribute), CFGetTypeID(size) == AXValueGetTypeID() {
                    var point = CGPoint.zero; var extent = CGSize.zero
                    AXValueGetValue(unsafeBitCast(position, to: AXValue.self), .cgPoint, &point)
                    AXValueGetValue(unsafeBitCast(size, to: AXValue.self), .cgSize, &extent)
                    node["bounds"] = ["x": point.x, "y": point.y, "width": extent.width, "height": extent.height]
                }
                nodes.append(node)
                if let children = attribute(element, kAXChildrenAttribute) as? [AXUIElement] {
                    for child in children { visit(child, depth + 1); if nodes.count >= 200 { break } }
                }
            }
            visit(root, 0)
        }
        let windows = (CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []).filter { ($0[kCGWindowLayer as String] as? Int) == 0 }.prefix(80).map { window in
            ["window_id": String(describing: window[kCGWindowNumber as String] ?? ""), "title": window[kCGWindowName as String] ?? "", "owner": window[kCGWindowOwnerName as String] ?? "", "pid": window[kCGWindowOwnerPID as String] ?? 0] as [String: Any]
        }
        return ["snapshot_id": snapshotID, "platform": "macos", "app": app?.localizedName ?? "", "elements": nodes, "windows": windows,
                "truncated": nodes.count >= 200, "apps": running.map { ["pid": $0.processIdentifier, "name": $0.localizedName ?? "", "bundle_id": $0.bundleIdentifier ?? ""] as [String: Any] }]
    }

    func screenshot(_ params: [String: Any]) async throws -> [String: Any] {
        if !CGPreflightScreenCaptureAccess() && !CGRequestScreenCaptureAccess() { throw DesktopError.unavailable("Screen Recording permission is missing. Enable it for Harness/helper in System Settings → Privacy & Security → Screen & System Audio Recording, then restart.") }
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        let filter: SCContentFilter
        let config = SCStreamConfiguration()
        if let id = params["window_id"] as? String, let windowID = UInt32(id) {
            guard let window = content.windows.first(where: { $0.windowID == windowID }) else { throw DesktopError.unavailable("Window no longer exists") }
            filter = SCContentFilter(desktopIndependentWindow: window)
            config.width = max(Int(window.frame.width), 1); config.height = max(Int(window.frame.height), 1)
            screenshotOrigin = window.frame.origin; screenshotSize = window.frame.size
        } else {
            guard let display = content.displays.first(where: { $0.displayID == CGMainDisplayID() }) ?? content.displays.first else { throw DesktopError.unavailable("No display is available") }
            filter = SCContentFilter(display: display, excludingWindows: [])
            // Logical desktop points keep screenshot coordinates consistent with CGEvent.
            let bounds = CGDisplayBounds(display.displayID)
            config.width = max(Int(bounds.width), 1); config.height = max(Int(bounds.height), 1)
            screenshotOrigin = bounds.origin; screenshotSize = bounds.size
        }
        config.showsCursor = true
        let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
        guard let png = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:]) else { throw DesktopError.unavailable("PNG encoding failed") }
        let filename = "desktop-\(UUID().uuidString).png"
        let artifact = URL(fileURLWithPath: artifacts).appendingPathComponent(filename)
        try FileManager.default.createDirectory(at: artifact.deletingLastPathComponent(), withIntermediateDirectories: true)
        try png.write(to: artifact)
        snapshotID = UUID().uuidString; elements.removeAll(); screenshotSnapshot = true
        return ["snapshot_id": snapshotID, "artifact": artifact.path, "width": image.width, "height": image.height,
                "coordinate_space": "screenshot-relative pixels mapped to logical desktop points", "desktop_origin": ["x": screenshotOrigin.x, "y": screenshotOrigin.y],
                "content": [["type": "text", "text": "Real macOS ScreenCaptureKit screenshot"], ["type": "image", "data": png.base64EncodedString(), "mimeType": "image/png"]]]
    }

    func act(_ params: [String: Any]) async throws -> [String: Any] {
        try requireAccessibility()
        guard let action = params["action"] as? String else { throw DesktopError.unavailable("action is required") }
        if action == "launch" {
            guard let name = params["app"] as? String else { throw DesktopError.unavailable("app is required") }
            let url: URL?
            if name.hasPrefix("/") { url = URL(fileURLWithPath: name) }
            else { url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: name) ?? NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.\(name)") }
            guard let url = url else { throw DesktopError.unavailable("Launch requires a valid application bundle ID or absolute .app path") }
            let launched = try await NSWorkspace.shared.openApplication(at: url, configuration: NSWorkspace.OpenConfiguration())
            snapshotID = ""; elements.removeAll(); return ["launched": url.path, "pid": launched.processIdentifier]
        }
        guard params["snapshot_id"] as? String == snapshotID, !snapshotID.isEmpty else { throw DesktopError.unavailable("Stale snapshot_id; observe the desktop again") }
        let element = (params["ref"] as? String).flatMap { elements[$0] }
        if params["ref"] is String && element == nil { throw DesktopError.unavailable("Stale element ref; observe again") }
        switch action {
        case "click":
            if let element = element {
                let status = AXUIElementPerformAction(element, kAXPressAction as CFString)
                if status != .success { throw DesktopError.unavailable("Element does not support press or became stale (AX error \(status.rawValue)); capture a fresh screenshot for coordinate input") }
            } else if screenshotSnapshot, let x = params["x"] as? Double, let y = params["y"] as? Double {
                guard x >= 0, y >= 0, x < screenshotSize.width, y < screenshotSize.height else { throw DesktopError.unavailable("Coordinates are outside the screenshot") }
                let point = CGPoint(x: screenshotOrigin.x + x, y: screenshotOrigin.y + y)
                guard let down = CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown, mouseCursorPosition: point, mouseButton: .left),
                      let up = CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: point, mouseButton: .left) else { throw DesktopError.unavailable("Cannot create mouse event") }
                down.post(tap: .cghidEventTap); up.post(tap: .cghidEventTap)
            } else { throw DesktopError.unavailable("click requires a current element ref or screenshot coordinates") }
        case "fill":
            guard let element = element, let value = params["text"] as? String else { throw DesktopError.unavailable("fill requires ref and text") }
            let status = AXUIElementSetAttributeValue(element, kAXValueAttribute as CFString, value as CFString)
            if status != .success { throw DesktopError.unavailable("Value cannot be set or element is stale (AX error \(status.rawValue))") }
        case "key":
            guard let key = params["key"] as? String else { throw DesktopError.unavailable("key is required") }
            try sendKey(key)
        case "scroll":
            let delta = params["delta"] as? Int ?? -500
            guard let event = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1, wheel1: Int32(clamping: delta), wheel2: 0, wheel3: 0) else { throw DesktopError.unavailable("Cannot create scroll event") }
            event.post(tap: .cghidEventTap)
        default: throw DesktopError.unavailable("Unsupported desktop action: \(action)")
        }
        snapshotID = ""; elements.removeAll()
        return ["executed": true, "action": action, "requires_observation": true]
    }

    func sendKey(_ combination: String) throws {
        let keys: [String: CGKeyCode] = ["enter": 36, "return": 36, "tab": 48, "space": 49, "backspace": 51, "escape": 53, "delete": 117,
            "left": 123, "right": 124, "down": 125, "up": 126, "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5,
            "z": 6, "x": 7, "c": 8, "v": 9, "b": 11, "q": 12, "w": 13, "e": 14, "r": 15, "y": 16, "t": 17,
            "1": 18, "2": 19, "3": 20, "4": 21, "6": 22, "5": 23, "9": 25, "7": 26, "8": 28, "0": 29, "o": 31, "u": 32, "i": 34, "p": 35, "l": 37, "j": 38, "k": 40, "n": 45, "m": 46]
        let parts = combination.lowercased().split(separator: "+").map(String.init)
        guard let last = parts.last, let code = keys[last] else { throw DesktopError.unavailable("Unsupported key combination") }
        var flags: CGEventFlags = []
        for part in parts.dropLast() {
            switch part { case "cmd", "command", "meta": flags.insert(.maskCommand); case "ctrl", "control": flags.insert(.maskControl); case "alt", "option": flags.insert(.maskAlternate); case "shift": flags.insert(.maskShift); default: throw DesktopError.unavailable("Unknown modifier: \(part)") }
        }
        guard let down = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: true), let up = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: false) else { throw DesktopError.unavailable("Cannot create key events") }
        down.flags = flags; up.flags = flags; down.post(tap: .cghidEventTap); up.post(tap: .cghidEventTap)
    }

    func handle(_ request: [String: Any]) async throws -> [String: Any] {
        let params = request["params"] as? [String: Any] ?? [:]
        switch request["method"] as? String {
        case "observe": return try observe(params)
        case "screenshot": return try await screenshot(params)
        case "act": return try await act(params)
        case "cancel": snapshotID = ""; elements.removeAll(); return ["cancelled": true]
        case "capabilities": return ["platform": "macos", "accessibility": AXIsProcessTrusted(), "screen_capture": CGPreflightScreenCaptureAccess()]
        default: throw DesktopError.unavailable("Unknown native method")
        }
    }
}

Task { @MainActor in
    let helper = DesktopHelper()
    while let line = readLine() {
        var id: Any = NSNull()
        var response: [String: Any]
        do {
            guard let data = line.data(using: .utf8), let request = try JSONSerialization.jsonObject(with: data) as? [String: Any] else { throw DesktopError.unavailable("Invalid JSON request") }
            id = request["id"] ?? NSNull()
            response = ["id": id, "result": try await helper.handle(request)]
        } catch { response = ["id": id, "error": ["message": error.localizedDescription]] }
        do {
            let data = try JSONSerialization.data(withJSONObject: response, options: [.sortedKeys])
            FileHandle.standardOutput.write(data); FileHandle.standardOutput.write(Data([10]))
        } catch { FileHandle.standardError.write(Data("Cannot serialize desktop response\n".utf8)) }
    }
    exit(0)
}
RunLoop.main.run()
