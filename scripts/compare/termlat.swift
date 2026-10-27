// Window and key-to-pixel latency helper for scripts/compare/compare.sh.
//
// Build: swiftc -O -parse-as-library termlat.swift -o termlat
//
// Subcommands (all print one JSON object on stdout):
//   check                                   Accessibility / Screen Recording status, display
//   wait-window (--pid P | --exe-prefix PATH) [--timeout S]
//                                           first on-screen window of a process; poll ~2 ms
//   place --pid P --x X --y Y --w W --h H   activate, move and resize its first window (AX)
//   activate --pid P                        bring the app forward and wait until it is focused
//   latency --pid P [--samples N] [--min-gap-ms A] [--max-gap-ms B] [--timeout-ms T]
//                                           post key events and time the first frame that shows
//                                           the driver.py latency block toggling
//   font-archive [--name PS_NAME] [--size PT]  base64 keyed-archived NSFont (Terminal.app profile)
//
// Latency = SCStreamFrameInfo.displayTime of the first matching captured frame minus
// mach_absolute_time just before CGEvent posting. It includes event delivery, the
// terminal, the compositor and capture quantization at the display's refresh rate;
// the capture part is common to every terminal, so compare, don't read it as absolute.
// The host app needs Accessibility (posting keys, AX) and Screen Recording (capture).
import AppKit
import ApplicationServices
import CoreGraphics
import CoreMedia
import Darwin
import Foundation
import ScreenCaptureKit

// MARK: - small utilities

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(("termlat: " + msg + "\n").data(using: .utf8)!)
    exit(1)
}

func emit(_ obj: [String: Any]) {
    let data = try! JSONSerialization.data(withJSONObject: obj, options: [.sortedKeys])
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write("\n".data(using: .utf8)!)
}

var timebase: mach_timebase_info_data_t = {
    var tb = mach_timebase_info_data_t()
    mach_timebase_info(&tb)
    return tb
}()

func absToNs(_ t: UInt64) -> UInt64 { t * UInt64(timebase.numer) / UInt64(timebase.denom) }

struct Args {
    var values: [String: String] = [:]
    init(_ argv: ArraySlice<String>) {
        var it = argv.makeIterator()
        while let a = it.next() {
            guard a.hasPrefix("--") else { fail("unexpected argument \(a)") }
            guard let v = it.next() else { fail("\(a) needs a value") }
            values[String(a.dropFirst(2))] = v
        }
    }
    func str(_ k: String) -> String? { values[k] }
    func int(_ k: String, _ d: Int? = nil) -> Int {
        if let v = values[k] {
            guard let n = Int(v) else { fail("--\(k) must be an integer") }
            return n
        }
        guard let d = d else { fail("--\(k) is required") }
        return d
    }
    func double(_ k: String, _ d: Double) -> Double { values[k].flatMap(Double.init) ?? d }
}

func exePath(_ pid: pid_t) -> String? {
    var buf = [CChar](repeating: 0, count: 4096)
    let n = proc_pidpath(pid, &buf, UInt32(buf.count))
    return n > 0 ? String(cString: buf) : nil
}

struct Win {
    let id: CGWindowID
    let pid: pid_t
    let bounds: CGRect
    var json: [String: Any] {
        ["window_id": Int(id), "pid": Int(pid),
         "bounds": [bounds.origin.x, bounds.origin.y, bounds.width, bounds.height]]
    }
}

func onscreenWindows() -> [Win] {
    let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
    guard let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else {
        return []
    }
    return list.compactMap { w in
        guard (w[kCGWindowLayer as String] as? Int) == 0,
              let pid = w[kCGWindowOwnerPID as String] as? Int,
              let num = w[kCGWindowNumber as String] as? Int,
              let b = w[kCGWindowBounds as String] as? [String: CGFloat],
              let rect = CGRect(dictionaryRepresentation: b as CFDictionary),
              rect.width >= 200, rect.height >= 150
        else { return nil }
        return Win(id: CGWindowID(num), pid: pid_t(pid), bounds: rect)
    }
}

/// Largest on-screen layer-0 window of `pid` (tab bars and panels are smaller).
func mainWindow(_ pid: pid_t) -> Win? {
    onscreenWindows().filter { $0.pid == pid }.max { $0.bounds.width * $0.bounds.height < $1.bounds.width * $1.bounds.height }
}

// MARK: - Accessibility

func axFocusedPid() -> pid_t? {
    let sys = AXUIElementCreateSystemWide()
    var app: CFTypeRef?
    guard AXUIElementCopyAttributeValue(sys, kAXFocusedApplicationAttribute as CFString, &app) == .success,
          let el = app else { return nil }
    var pid: pid_t = 0
    return AXUIElementGetPid(el as! AXUIElement, &pid) == .success ? pid : nil
}

func activate(_ pid: pid_t, timeout: Double = 3) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    repeat {
        if axFocusedPid() == pid { return true }
        NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateAllWindows])
        let app = AXUIElementCreateApplication(pid)
        AXUIElementSetAttributeValue(app, kAXFrontmostAttribute as CFString, kCFBooleanTrue)
        usleep(100_000)
    } while Date() < deadline
    return axFocusedPid() == pid
}

func axFirstWindow(_ pid: pid_t) -> AXUIElement? {
    let app = AXUIElementCreateApplication(pid)
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &value) == .success,
          let wins = value as? [AXUIElement] else { return nil }
    return wins.first { w in
        var sub: CFTypeRef?
        AXUIElementCopyAttributeValue(w, kAXSubroleAttribute as CFString, &sub)
        return (sub as? String) == (kAXStandardWindowSubrole as String)
    } ?? wins.first
}

func place(_ pid: pid_t, _ rect: CGRect) -> Bool {
    guard let w = axFirstWindow(pid) else { return false }
    var origin = rect.origin
    var size = rect.size
    // Set size twice around the move: some apps clamp size to the old screen position.
    let s1 = AXValueCreate(.cgSize, &size)!
    let p = AXValueCreate(.cgPoint, &origin)!
    AXUIElementSetAttributeValue(w, kAXSizeAttribute as CFString, s1)
    AXUIElementSetAttributeValue(w, kAXPositionAttribute as CFString, p)
    AXUIElementSetAttributeValue(w, kAXSizeAttribute as CFString, s1)
    AXUIElementPerformAction(w, kAXRaiseAction as CFString)
    return true
}

// MARK: - display

func displayInfo() -> [String: Any] {
    let screen = NSScreen.main
    let mode = CGDisplayCopyDisplayMode(CGMainDisplayID())
    return [
        "max_fps": screen?.maximumFramesPerSecond ?? 0,
        "mode_refresh_hz": mode?.refreshRate ?? 0,
        "backing_scale": Double(screen?.backingScaleFactor ?? 0),
        "points": [Double(screen?.frame.width ?? 0), Double(screen?.frame.height ?? 0)],
    ]
}

// MARK: - capture

/// Downsampled luma grid of each complete frame, with its display time.
final class Capture: NSObject, SCStreamOutput, SCStreamDelegate {
    struct Frame { let displayAbs: UInt64; let luma: [UInt8] }
    private let cond = NSCondition()
    private var queue: [Frame] = []
    private(set) var latest: Frame?
    var stream: SCStream?
    var streamError: Error?

    func stream(_ s: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen,
              let atts = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let info = atts.first,
              let raw = info[.status] as? Int, SCFrameStatus(rawValue: raw) == .complete,
              let display = info[.displayTime] as? UInt64,
              let px = CMSampleBufferGetImageBuffer(sb) else { return }
        CVPixelBufferLockBaseAddress(px, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(px, .readOnly) }
        let w = CVPixelBufferGetWidth(px), h = CVPixelBufferGetHeight(px)
        let stride = CVPixelBufferGetBytesPerRow(px)
        guard let base = CVPixelBufferGetBaseAddress(px)?.assumingMemoryBound(to: UInt8.self) else { return }
        var luma: [UInt8] = []
        luma.reserveCapacity((w / 4) * (h / 4))
        for y in Swift.stride(from: 0, to: h, by: 4) {
            let row = base + y * stride
            for x in Swift.stride(from: 0, to: w, by: 4) {
                let p = row + x * 4 // BGRA
                luma.append(UInt8((UInt32(p[0]) * 29 + UInt32(p[1]) * 150 + UInt32(p[2]) * 77) >> 8))
            }
        }
        let f = Frame(displayAbs: display, luma: luma)
        cond.lock()
        latest = f
        queue.append(f)
        cond.signal()
        cond.unlock()
    }

    func stream(_ s: SCStream, didStopWithError error: Error) { streamError = error }

    func drain() { cond.lock(); queue.removeAll(); cond.unlock() }

    func latestFrame() -> Frame? { cond.lock(); defer { cond.unlock() }; return latest }

    /// Next queued frame, waiting until `deadline`.
    func next(until deadline: Date) -> Frame? {
        cond.lock()
        defer { cond.unlock() }
        while queue.isEmpty {
            if !cond.wait(until: deadline) { return nil }
        }
        return queue.removeFirst()
    }

    func start(window: Win, fps: Int) {
        let sem = DispatchSemaphore(value: 0)
        var content: SCShareableContent?
        var err: Error?
        SCShareableContent.getExcludingDesktopWindows(true, onScreenWindowsOnly: true) { c, e in
            content = c; err = e; sem.signal()
        }
        sem.wait()
        guard let c = content else { fail("SCShareableContent failed (Screen Recording permission?): \(String(describing: err))") }
        guard let scw = c.windows.first(where: { $0.windowID == window.id }) else {
            fail("window \(window.id) not shareable")
        }
        let filter = SCContentFilter(desktopIndependentWindow: scw)
        let cfg = SCStreamConfiguration()
        // One captured pixel per point: plenty for a block of many cells.
        cfg.width = Int(window.bounds.width)
        cfg.height = Int(window.bounds.height)
        cfg.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(max(fps, 60) * 2))
        cfg.pixelFormat = kCVPixelFormatType_32BGRA
        cfg.showsCursor = false
        cfg.queueDepth = 8
        let s = SCStream(filter: filter, configuration: cfg, delegate: self)
        do {
            try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: DispatchQueue(label: "termlat.capture"))
        } catch { fail("addStreamOutput: \(error)") }
        var startErr: Error?
        s.startCapture { e in startErr = e; sem.signal() }
        sem.wait()
        if let e = startErr { fail("startCapture: \(e)") }
        stream = s
    }

    func stop() {
        let sem = DispatchSemaphore(value: 0)
        stream?.stopCapture { _ in sem.signal() }
        _ = sem.wait(timeout: .now() + 2)
    }
}

func diffCount(_ a: [UInt8], _ b: [UInt8]) -> Int {
    var n = 0
    for i in 0..<min(a.count, b.count) where abs(Int(a[i]) - Int(b[i])) > 40 { n += 1 }
    return n
}

func postKey() -> UInt64 {
    // kVK_ANSI_S: a letter on QWERTY/AZERTY/QWERTZ alike; driver.py toggles on any key.
    let src = CGEventSource(stateID: .hidSystemState)
    let down = CGEvent(keyboardEventSource: src, virtualKey: 1, keyDown: true)!
    let up = CGEvent(keyboardEventSource: src, virtualKey: 1, keyDown: false)!
    let t = mach_absolute_time()
    down.post(tap: .cghidEventTap)
    up.post(tap: .cghidEventTap)
    return t
}

func latency(_ a: Args) {
    guard AXIsProcessTrusted() else { fail("host app lacks Accessibility permission") }
    guard CGPreflightScreenCaptureAccess() else { fail("host app lacks Screen Recording permission") }
    let pid = pid_t(a.int("pid"))
    let samples = a.int("samples", 200)
    let minGap = a.double("min-gap-ms", 50), maxGap = a.double("max-gap-ms", 150)
    let timeout = a.double("timeout-ms", 1000) / 1000
    guard let win = mainWindow(pid) else { fail("no on-screen window for pid \(pid)") }
    guard activate(pid) else { fail("could not focus pid \(pid)") }
    let fps = NSScreen.main?.maximumFramesPerSecond ?? 60
    let cap = Capture()
    cap.start(window: win, fps: fps)
    defer { cap.stop() }

    func settle(_ s: Double) -> Capture.Frame? { usleep(UInt32(s * 1e6)); return cap.latestFrame() }
    // Calibrate: frame before a key (`erased`), frame after it (`drawn`); the
    // threshold is half their distance. Retried because the first key can race
    // the terminal's first present after focus. `erased` is whichever state the
    // block starts in: only toggles are timed, so its polarity does not matter.
    // SCStream only delivers frames on change, so the first frame comes promptly.
    var calibrationKeys = 0
    var erased: Capture.Frame?
    var span = 0
    for _ in 0..<5 {
        guard let before = settle(0.5) else { fail("no captured frames (window occluded or not updating?)") }
        _ = postKey()
        calibrationKeys += 1
        guard let after = settle(0.8) else { fail("no frame after calibration key") }
        span = diffCount(after.luma, before.luma)
        if span >= 20 { erased = before; break }
        _ = activate(pid)
    }
    guard let erased = erased else {
        fail("latency block not visible (span \(span)); is the driver in latency mode and focused?")
    }
    _ = postKey()
    calibrationKeys += 1
    var isDrawn = diffCount(settle(0.8)!.luma, erased.luma) > span / 2
    if isDrawn { fail("calibration toggle back did not take effect") }

    var out: [[String: Any]] = []
    var misses = 0, refocus = 0
    for i in 0..<samples {
        usleep(UInt32(Double.random(in: minGap...maxGap) * 1000))
        if axFocusedPid() != pid {
            refocus += 1
            if !activate(pid) { fail("lost focus at sample \(i)") }
            usleep(200_000)
        }
        cap.drain()
        let want = !isDrawn
        let t0 = postKey()
        let deadline = Date().addingTimeInterval(timeout)
        var hit: Capture.Frame?
        while let f = cap.next(until: deadline) {
            if f.displayAbs <= t0 { continue }
            if (diffCount(f.luma, erased.luma) > span / 2) == want { hit = f; break }
        }
        if let f = hit {
            isDrawn = want
            out.append(["i": i, "post_ns": absToNs(t0), "display_ns": absToNs(f.displayAbs),
                        "ms": Double(absToNs(f.displayAbs - t0)) / 1e6])
        } else {
            misses += 1
            out.append(["i": i, "post_ns": absToNs(t0), "miss": true])
            // Resync parity from what is on screen now.
            if let f = settle(0.5) { isDrawn = diffCount(f.luma, erased.luma) > span / 2 }
        }
    }
    var result = displayInfo()
    result["pid"] = Int(pid)
    result["window"] = win.json
    result["calibration_span"] = span
    result["calibration_keys"] = calibrationKeys
    result["misses"] = misses
    result["refocus"] = refocus
    result["samples"] = out
    emit(result)
}

// MARK: - main

@main
struct TermLat {
    static func main() {
        let argv = CommandLine.arguments
        guard argv.count >= 2 else { fail("usage: termlat check|wait-window|place|activate|latency ...") }
        let a = Args(argv.dropFirst(2))
        // AppKit/AX calls want a run loop on the main thread; do the work elsewhere.
        Thread {
            run(argv[1], a)
            exit(0)
        }.start()
        RunLoop.main.run()
    }

    static func run(_ cmd: String, _ a: Args) {
        switch cmd {
        case "check":
            var r = displayInfo()
            r["accessibility"] = AXIsProcessTrusted()
            r["screen_recording"] = CGPreflightScreenCaptureAccess()
            emit(r)
        case "wait-window":
            let deadline = Date().addingTimeInterval(a.double("timeout", 30))
            let pid = a.str("pid").flatMap { pid_t($0) }
            let prefix = a.str("exe-prefix")
            if pid == nil && prefix == nil { fail("wait-window needs --pid or --exe-prefix") }
            var cache: [pid_t: String] = [:]
            while Date() < deadline {
                for w in onscreenWindows() {
                    let ok: Bool
                    if let p = pid { ok = w.pid == p } else {
                        if cache[w.pid] == nil { cache[w.pid] = exePath(w.pid) ?? "" }
                        ok = cache[w.pid]!.hasPrefix(prefix!)
                    }
                    if ok {
                        var r = w.json
                        r["wall"] = Date().timeIntervalSince1970
                        emit(r)
                        return
                    }
                }
                usleep(2000)
            }
            fail("timed out waiting for a window")
        case "place":
            let pid = pid_t(a.int("pid"))
            let rect = CGRect(x: a.int("x"), y: a.int("y"), width: a.int("w"), height: a.int("h"))
            _ = activate(pid)
            if !place(pid, rect) { fail("no AX window for pid \(pid) (Accessibility permission?)") }
            usleep(300_000)
            var r: [String: Any] = ["pid": Int(pid), "focused": axFocusedPid() == pid]
            if let w = mainWindow(pid) { r["window"] = w.json }
            emit(r)
        case "activate":
            let pid = pid_t(a.int("pid"))
            emit(["pid": Int(pid), "focused": activate(pid)])
        case "latency":
            latency(a)
        case "font-archive":
            // Terminal.app profiles store their font as a keyed-archived NSFont.
            let name = a.str("name") ?? "Menlo-Regular"
            guard let font = NSFont(name: name, size: CGFloat(a.double("size", 14))) else {
                fail("font \(name) not found")
            }
            guard let data = try? NSKeyedArchiver.archivedData(withRootObject: font, requiringSecureCoding: false) else {
                fail("could not archive font")
            }
            emit(["name": name, "base64": data.base64EncodedString()])
        default:
            fail("unknown subcommand \(cmd)")
        }
    }
}
