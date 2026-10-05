// WKWebView probe for S30: the engine the desktop app uses on macOS.
// Loads probe.html, grants media capture the way the app would
// (WKUIDelegate), and prints the page's result.
import AppKit
import WebKit

final class Probe: NSObject, NSApplicationDelegate, WKScriptMessageHandler, WKUIDelegate {
    var window: NSWindow!
    var web: WKWebView!
    func applicationDidFinishLaunching(_ n: Notification) {
        let cfg = WKWebViewConfiguration()
        cfg.userContentController.add(self, name: "probe")
        cfg.mediaTypesRequiringUserActionForPlayback = []
        web = WKWebView(frame: NSRect(x: 0, y: 0, width: 700, height: 800), configuration: cfg)
        web.uiDelegate = self
        window = NSWindow(contentRect: web.frame, styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.contentView = web
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        web.load(URLRequest(url: URL(string: CommandLine.arguments[1])!))
        DispatchQueue.main.asyncAfter(deadline: .now() + 90) { print("TIMEOUT"); exit(2) }
    }
    func webView(_ w: WKWebView, requestMediaCapturePermissionFor origin: WKSecurityOrigin,
                 initiatedByFrame frame: WKFrameInfo, type: WKMediaCaptureType,
                 decisionHandler: @escaping (WKPermissionDecision) -> Void) {
        FileHandle.standardError.write("media capture requested: \(type.rawValue)\n".data(using: .utf8)!)
        decisionHandler(.grant)
    }
    func userContentController(_ c: WKUserContentController, didReceive m: WKScriptMessage) {
        print(m.body as? String ?? "?")
        exit(0)
    }
}
let app = NSApplication.shared
let d = Probe()
app.delegate = d
app.setActivationPolicy(.regular)
app.run()
