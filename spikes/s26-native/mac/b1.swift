// S26 B1 on macOS: Ghostty's own surface (GhosttyKit, built from the
// daemon's pinned Ghostty) in a bare AppKit window, running
// `illogical attach` against a daemon: Ghostty draws, the daemon owns the
// shell. Embedding has one I/O backend, exec, so the surface can only be fed
// through a command in its own PTY.
//
//     s26-b1 --socket DEV_SOCK --pane 1 [--cli PATH]

import AppKit

let args = CommandLine.arguments
func arg(_ k: String) -> String? {
    guard let i = args.firstIndex(of: k), i + 1 < args.count else { return nil }
    return args[i + 1]
}
let t0 = CACurrentMediaTime()

final class SurfaceView: NSView {
    var surface: ghostty_surface_t?
    override var acceptsFirstResponder: Bool { true }

    override func setFrameSize(_ s: NSSize) {
        super.setFrameSize(s)
        guard let sf = surface else { return }
        let scale = window?.backingScaleFactor ?? 2
        ghostty_surface_set_content_scale(sf, scale, scale)
        ghostty_surface_set_size(sf, UInt32(s.width * scale), UInt32(s.height * scale))
    }

    func mods(_ f: NSEvent.ModifierFlags) -> ghostty_input_mods_e {
        var m: UInt32 = 0
        if f.contains(.shift) { m |= GHOSTTY_MODS_SHIFT.rawValue }
        if f.contains(.control) { m |= GHOSTTY_MODS_CTRL.rawValue }
        if f.contains(.option) { m |= GHOSTTY_MODS_ALT.rawValue }
        if f.contains(.command) { m |= GHOSTTY_MODS_SUPER.rawValue }
        return ghostty_input_mods_e(rawValue: m)
    }

    func key(_ e: NSEvent, _ action: ghostty_input_action_e) {
        guard let sf = surface else { return }
        var k = ghostty_input_key_s()
        k.action = action
        k.mods = mods(e.modifierFlags)
        k.consumed_mods = ghostty_input_mods_e(rawValue: 0)
        k.keycode = UInt32(e.keyCode)
        k.composing = false
        k.unshifted_codepoint = e.charactersIgnoringModifiers?.unicodeScalars.first?.value ?? 0
        let text = action == GHOSTTY_ACTION_RELEASE ? nil : e.characters.flatMap { s in
            s.unicodeScalars.allSatisfy { $0.value >= 0x20 && $0.value < 0xF700 } ? s : nil
        }
        if let t = text {
            t.withCString { k.text = $0; _ = ghostty_surface_key(sf, k) }
        } else {
            k.text = nil
            _ = ghostty_surface_key(sf, k)
        }
    }

    override func keyDown(with e: NSEvent) { key(e, e.isARepeat ? GHOSTTY_ACTION_REPEAT : GHOSTTY_ACTION_PRESS) }
    override func keyUp(with e: NSEvent) { key(e, GHOSTTY_ACTION_RELEASE) }
    override func becomeFirstResponder() -> Bool {
        if let sf = surface { ghostty_surface_set_focus(sf, true) }
        return true
    }
}

final class Delegate: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    var view: SurfaceView!
    var app: ghostty_app_t?
    var command: UnsafeMutablePointer<CChar>?

    func applicationDidFinishLaunching(_ n: Notification) {
        guard let sock = arg("--socket") else { print("--socket DEV_SOCK"); exit(2) }
        let pane = arg("--pane") ?? "1"
        let cli = arg("--cli") ?? (NSHomeDirectory() + "/.local/bin/illogical")

        let config = ghostty_config_new()
        ghostty_config_finalize(config)
        var rt = ghostty_runtime_config_s()
        rt.userdata = Unmanaged.passUnretained(self).toOpaque()
        rt.supports_selection_clipboard = false
        rt.wakeup_cb = { ud in
            let d = Unmanaged<Delegate>.fromOpaque(ud!).takeUnretainedValue()
            DispatchQueue.main.async { if let a = d.app { ghostty_app_tick(a) } }
        }
        rt.action_cb = { _, _, _ in false }
        rt.read_clipboard_cb = { _, _, _ in false }
        rt.confirm_read_clipboard_cb = { _, _, _, _ in }
        rt.write_clipboard_cb = { _, _, _, _, _ in }
        rt.close_surface_cb = { _, _ in DispatchQueue.main.async { NSApp.terminate(nil) } }
        app = ghostty_app_new(&rt, config)

        let size = NSSize(width: 1280, height: 820)
        view = SurfaceView(frame: NSRect(origin: .zero, size: size))
        window = NSWindow(contentRect: NSRect(origin: .zero, size: size), styleMask: [.titled, .closable, .resizable, .miniaturizable], backing: .buffered, defer: false)
        window.title = "illogical (S26 B1, GhosttyKit)"
        window.contentView = view
        window.center()
        window.makeKeyAndOrderFront(nil)

        var sc = ghostty_surface_config_new()
        sc.platform_tag = GHOSTTY_PLATFORM_MACOS
        sc.platform = ghostty_platform_u(macos: ghostty_platform_macos_s(nsview: Unmanaged.passUnretained(view).toOpaque()))
        sc.userdata = Unmanaged.passUnretained(view).toOpaque()
        sc.scale_factor = window.backingScaleFactor
        command = strdup("\(cli) --socket \(sock) attach %\(pane)")
        sc.command = UnsafePointer(command)
        view.surface = ghostty_surface_new(app, &sc)
        view.setFrameSize(size)
        window.makeFirstResponder(view)
        ghostty_app_set_focus(app, true)
        NSApp.activate(ignoringOtherApps: true)
        print(String(format: "s26-b1: surface up at %.0f ms", (CACurrentMediaTime() - t0) * 1000))

        // For `screencapture -l ID` from outside (the surface draws into an
        // IOSurface layer, which the view's own cacheDisplay can't see).
        print("s26-b1: window \(window.windowNumber)")
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ s: NSApplication) -> Bool { true }
}

setvbuf(stdout, nil, _IOLBF, 0)  // lines reach a log file at once
if ghostty_init(UInt(CommandLine.argc), CommandLine.unsafeArgv) != 0 { print("ghostty_init failed"); exit(1) }
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let delegate = Delegate()
app.delegate = delegate
app.run()
