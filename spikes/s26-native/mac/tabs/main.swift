// S26 level A and the hybrid on macOS: the daemon's tabs as native window
// tabs. A tab that is one terminal gets the native view (term.swift, B2);
// any other gets a WKWebView of the client at its first pane, with the
// client's own tab bar hidden.
//
//     s26-tabs --socket DEV_SOCK --url http://127.0.0.1:PORT [--web-only]

import AppKit
import WebKit

struct Tab: Decodable {
    let name: String
    let panes: [UInt32]
    let kind: String
}

final class Delegate: NSObject, NSApplicationDelegate {
    var windows: [NSWindow] = []
    var views: [TermView] = []

    func web(_ url: String) -> WKWebView {
        let ucc = WKUserContentController()
        ucc.addUserScript(WKUserScript(source: "const s=document.createElement('style');s.textContent='.bar{display:none!important}';document.documentElement.appendChild(s);", injectionTime: .atDocumentEnd, forMainFrameOnly: true))
        let cfg = WKWebViewConfiguration()
        cfg.userContentController = ucc
        let v = WKWebView(frame: .zero, configuration: cfg)
        v.load(URLRequest(url: URL(string: url)!))
        return v
    }

    func term(_ sock: String, _ pane: UInt32) -> TermView {
        let v = TermView(frame: NSRect(x: 0, y: 0, width: 1200, height: 780), cols: 120, rows: 40)
        let me = Unmanaged.passUnretained(v).toOpaque()
        v.conn = s26_conn_attach(sock, pane, v.grid.0, v.grid.1, { ctx in
            let v = Unmanaged<TermView>.fromOpaque(ctx!).takeUnretainedValue()
            DispatchQueue.main.async { if !v.pending { v.pending = true; DispatchQueue.main.async { v.drain() } } }
        }, me)
        views.append(v)
        return v
    }

    func applicationDidFinishLaunching(_ n: Notification) {
        guard let sock = arg("--socket"), let url = arg("--url") else { print("--socket DEV_SOCK --url http://127.0.0.1:PORT"); exit(2) }
        let webOnly = args.contains("--web-only")
        guard let raw = s26_tabs_json(sock) else { print("no daemon at \(sock)"); exit(1) }
        let tabs = try! JSONDecoder().decode([Tab].self, from: Data(String(cString: raw).utf8))
        s26_string_free(raw)
        NSWindow.allowsAutomaticWindowTabbing = true
        for t in tabs {
            guard let first = t.panes.first else { continue }
            let native = !webOnly && t.panes.count == 1 && t.kind == "Terminal"
            let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1200, height: 780), styleMask: [.titled, .closable, .resizable, .miniaturizable], backing: .buffered, defer: false)
            w.tabbingMode = .preferred
            w.tabbingIdentifier = "illogical"
            w.title = native ? t.name : "\(t.name) (web)"
            w.contentView = native ? term(sock, first) : web("\(url)/#pane=\(first)")
            if let main = windows.first {
                main.addTabbedWindow(w, ordered: .above)
            } else {
                w.center()
            }
            w.makeKeyAndOrderFront(nil)
            windows.append(w)
            print("s26-tabs: \(t.name): \(t.panes.count) panes, \(t.kind), \(native ? "native" : "web")")
        }
        NSApp.activate(ignoringOtherApps: true)
        if let shot = arg("--shot"), let sel = arg("--select").flatMap(Int.init), sel < windows.count {
            let w = windows[sel]
            w.makeKeyAndOrderFront(nil)
            DispatchQueue.main.asyncAfter(deadline: .now() + 3) {
                if let wk = w.contentView as? WKWebView {
                    wk.takeSnapshot(with: nil) { img, _ in
                        if let img, let tiff = img.tiffRepresentation, let rep = NSBitmapImageRep(data: tiff) {
                            try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: shot))
                            print("s26-tabs: saved \(shot)")
                        }
                    }
                } else if let v = w.contentView {
                    saveLayers(of: v, to: shot)
                }
            }
        }
        print(String(format: "s26-tabs: windows at %.0f ms; window ids %@", now(), windows.map { String($0.windowNumber) }.joined(separator: ",")))
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ s: NSApplication) -> Bool { true }
}

setvbuf(stdout, nil, _IOLBF, 0)  // lines reach a log file at once
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let delegate = Delegate()
app.delegate = delegate
app.run()
