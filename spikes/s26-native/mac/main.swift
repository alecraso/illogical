// S26 B2 on macOS: one pane drawn natively (term.swift).
//
//     s26-mac --socket DEV_SOCK --pane 1     # a dev daemon's pane
//     s26-mac --bench [--out results.json]   # S25's bench, no daemon

import AppKit

// ---- the app

final class Delegate: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    var view: TermView!
    var bench: Bench?

    func applicationDidFinishLaunching(_ n: Notification) {
        let probe = TermView(frame: .zero, cols: 160, rows: 48)
        let size = NSSize(width: 160 * probe.cellW + 2 * pad, height: 48 * probe.cellH + 2 * pad)
        view = TermView(frame: NSRect(origin: .zero, size: size), cols: 160, rows: 48)
        window = NSWindow(contentRect: NSRect(origin: .zero, size: size), styleMask: [.titled, .closable, .resizable, .miniaturizable], backing: .buffered, defer: false)
        window.title = "illogical (S26, AppKit)"
        window.contentView = view
        window.center()
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(view)
        NSApp.activate(ignoringOtherApps: true)
        print(String(format: "s26: window at %.0f ms", now()))

        if args.contains("--bench") {
            bench = Bench(view: view, out: arg("--out"))
            bench!.run()
            return
        }
        guard let sock = arg("--socket") else {
            print("--socket DEV_SOCK (a dev daemon's, never the daily one) or --bench")
            NSApp.terminate(nil)
            return
        }
        let pane = UInt32(arg("--pane")?.trimmingCharacters(in: CharacterSet(charactersIn: "%")) ?? "1") ?? 1
        let me = Unmanaged.passUnretained(view).toOpaque()
        view.conn = s26_conn_attach(sock, pane, view.grid.0, view.grid.1, { ctx in
            let v = Unmanaged<TermView>.fromOpaque(ctx!).takeUnretainedValue()
            DispatchQueue.main.async {
                if !v.pending { v.pending = true; DispatchQueue.main.async { v.drain() } }
            }
        }, me)
        if view.conn == nil { NSApp.terminate(nil) }
        if let shot = arg("--shot") {
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.5) {
                saveLayers(of: self.view, to: shot)
            }
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ s: NSApplication) -> Bool { true }
}

setvbuf(stdout, nil, _IOLBF, 0)  // lines reach a log file at once
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let delegate = Delegate()
app.delegate = delegate
app.run()
