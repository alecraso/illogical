// S26 B2 on macOS: the terminal view (AppKit, CoreText) over the Rust
// core's libghostty-vt terminal (ffi/, through s26.h), and the bench.
// main.swift (s26-mac) and tabs.swift (s26-tabs) are the two apps.
//
// Each row is a CALayer whose contents are drawn with CoreText when the
// render state marks the row dirty; the compositor does the rest. Keys go
// through libghostty's encoder; text and IME through NSTextInputClient.

import AppKit
import CoreText
import QuartzCore

let args = CommandLine.arguments
func arg(_ k: String) -> String? {
    guard let i = args.firstIndex(of: k), i + 1 < args.count else { return nil }
    return args[i + 1]
}
let t0 = CACurrentMediaTime()
func now() -> Double { (CACurrentMediaTime() - t0) * 1000 }
let pad: CGFloat = 6

/// A view's layer tree as a PNG (no Screen Recording permission needed).
func saveLayers(of view: NSView, to path: String) {
    guard let layer = view.layer else { return }
    let scale = view.window?.backingScaleFactor ?? 2
    let w = Int(view.bounds.width * scale), h = Int(view.bounds.height * scale)
    guard let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue) else { return }
    ctx.scaleBy(x: scale, y: scale)
    if view.isFlipped {
        ctx.translateBy(x: 0, y: view.bounds.height)
        ctx.scaleBy(x: 1, y: -1)
    }
    layer.render(in: ctx)
    guard let img = ctx.makeImage() else { return }
    try? NSBitmapImageRep(cgImage: img).representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: path))
    print("s26: saved \(path)")
}

func color(_ rgb: UInt32) -> CGColor {
    CGColor(srgbRed: CGFloat((rgb >> 16) & 0xff) / 255, green: CGFloat((rgb >> 8) & 0xff) / 255, blue: CGFloat(rgb & 0xff) / 255, alpha: 1)
}

final class TermView: NSView, NSTextInputClient {
    let term: OpaquePointer
    var conn: OpaquePointer?
    let font: CTFont
    let bold: CTFont
    let italic: CTFont
    let cellW: CGFloat
    let cellH: CGFloat
    let descent: CGFloat
    var grid: (UInt16, UInt16) = (160, 48)
    var rowLayers: [CALayer] = []
    let cursor = CALayer()
    let preeditLayer = CATextLayer()
    var marked = ""
    var bg: UInt32 = 0
    var rowsDrawn = 0
    var pending = false
    /// Called after each render that changed something, with its time.
    var onRender: (() -> Void)?

    init(frame: NSRect, cols: UInt16, rows: UInt16) {
        term = s26_term_new(cols, rows)!
        let name = NSFont(name: "JetBrains Mono", size: 13) != nil ? "JetBrains Mono" : "Menlo"
        font = CTFontCreateWithName(name as CFString, 13, nil)
        bold = CTFontCreateCopyWithSymbolicTraits(font, 13, nil, .traitBold, .traitBold) ?? font
        italic = CTFontCreateCopyWithSymbolicTraits(font, 13, nil, .traitItalic, .traitItalic) ?? font
        var glyph = CGGlyph(0)
        var ch = UniChar(77)  // M
        CTFontGetGlyphsForCharacters(font, &ch, &glyph, 1)
        var adv = CGSize.zero
        CTFontGetAdvancesForGlyphs(font, .horizontal, &glyph, &adv, 1)
        cellW = adv.width
        cellH = ceil(CTFontGetAscent(font) + CTFontGetDescent(font) + CTFontGetLeading(font))
        descent = CTFontGetDescent(font)
        grid = (cols, rows)
        super.init(frame: frame)
        wantsLayer = true
        layer!.backgroundColor = color(0)
        cursor.backgroundColor = CGColor(gray: 0.8, alpha: 0.6)
        cursor.actions = ["position": NSNull(), "bounds": NSNull(), "hidden": NSNull()]
        layer!.addSublayer(cursor)
        preeditLayer.font = font
        preeditLayer.fontSize = 13
        preeditLayer.contentsScale = 2
        preeditLayer.isHidden = true
        layer!.addSublayer(preeditLayer)
    }

    required init?(coder: NSCoder) { fatalError() }
    override var acceptsFirstResponder: Bool { true }
    override var isFlipped: Bool { true }

    func layerFor(row: Int) -> CALayer {
        while rowLayers.count <= row {
            let l = CALayer()
            l.actions = ["contents": NSNull(), "position": NSNull(), "bounds": NSNull()]
            l.contentsScale = window?.backingScaleFactor ?? 2
            l.anchorPoint = .zero
            layer!.insertSublayer(l, below: cursor)
            rowLayers.append(l)
        }
        return rowLayers[row]
    }

    /// One row's runs into a bitmap for its layer.
    func draw(row: Int, runs: UnsafeBufferPointer<S26Run>) {
        let scale = window?.backingScaleFactor ?? 2
        let width = bounds.width - 2 * pad
        let l = layerFor(row: row)
        l.frame = CGRect(x: pad, y: pad + CGFloat(row) * cellH, width: width, height: cellH)
        guard width > 0, let ctx = CGContext(data: nil, width: Int(width * scale), height: Int(cellH * scale), bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue) else { return }
        ctx.scaleBy(x: scale, y: scale)
        for r in runs {
            let x = CGFloat(r.col) * cellW
            if r.bg != UInt32.max {
                ctx.setFillColor(color(r.bg))
                ctx.fill(CGRect(x: x, y: 0, width: CGFloat(r.cells) * cellW, height: cellH))
            }
            guard r.text_len > 0, let p = r.text else { continue }
            let s = String(decoding: UnsafeBufferPointer(start: p, count: r.text_len), as: UTF8.self)
            if s.allSatisfy({ $0 == " " }) { continue }
            let f = r.flags & 1 != 0 ? bold : (r.flags & 2 != 0 ? italic : font)
            let attr = NSAttributedString(string: s, attributes: [.font: f, .foregroundColor: NSColor(cgColor: color(r.fg))!])
            let line = CTLineCreateWithAttributedString(attr)
            ctx.textPosition = CGPoint(x: x, y: descent)
            CTLineDraw(line, ctx)
        }
        l.contents = ctx.makeImage()
        rowsDrawn += 1
    }

    /// Takes what the connection queued, then redraws dirty rows.
    func drain() {
        pending = false
        if let c = conn, s26_conn_drain(c, term) < 0 {
            NSApp.terminate(nil)
            return
        }
        render()
    }

    func render() {
        var frame = S26Frame()
        let me = Unmanaged.passUnretained(self).toOpaque()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        s26_term_render(term, me, { ctx, row, runs, n in
            let v = Unmanaged<TermView>.fromOpaque(ctx!).takeUnretainedValue()
            v.draw(row: Int(row), runs: UnsafeBufferPointer(start: runs, count: n))
        }, &frame)
        if frame.bg != bg {
            bg = frame.bg
            layer!.backgroundColor = color(bg)
        }
        cursor.isHidden = !frame.cursor_visible || !marked.isEmpty
        cursor.frame = CGRect(x: pad + CGFloat(frame.cursor_x) * cellW, y: pad + CGFloat(frame.cursor_y) * cellH, width: cellW, height: cellH)
        preeditLayer.isHidden = marked.isEmpty
        if !marked.isEmpty {
            preeditLayer.string = NSAttributedString(string: marked, attributes: [.font: font, .foregroundColor: NSColor.white, .underlineStyle: NSUnderlineStyle.single.rawValue])
            preeditLayer.frame = CGRect(x: cursor.frame.minX, y: cursor.frame.minY, width: CGFloat(marked.count * 2 + 1) * cellW, height: cellH)
            preeditLayer.backgroundColor = color(bg)
        }
        CATransaction.commit()
        if frame.dirty != 0 { onRender?() }
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        let cols = UInt16(max(2, Int((newSize.width - 2 * pad) / cellW)))
        let rows = UInt16(max(2, Int((newSize.height - 2 * pad) / cellH)))
        if (cols, rows) != grid {
            grid = (cols, rows)
            s26_term_resize(term, cols, rows, UInt32(cellW), UInt32(cellH))
            if let c = conn { s26_conn_view(c, cols, rows) }
            for l in rowLayers.dropFirst(Int(rows)) { l.contents = nil }
            render()
        }
    }

    // ---- input

    func send(_ bytes: [UInt8]) {
        guard let c = conn, !bytes.isEmpty else { return }
        bytes.withUnsafeBufferPointer { s26_conn_input(c, $0.baseAddress, $0.count) }
    }

    override func keyDown(with e: NSEvent) {
        let f = e.modifierFlags
        // Control and Command chords, and keys with no text, through
        // libghostty's encoder; typing (and IME) through the text system.
        let chord = f.contains(.control) || f.contains(.command)
        let special = (e.characters ?? "").unicodeScalars.first.map { $0.value >= 0xF700 || $0.value < 0x20 || $0.value == 0x7f } ?? true
        if !marked.isEmpty || (!chord && !special) {
            interpretKeyEvents([e])
            return
        }
        var mods: UInt32 = 0
        if f.contains(.shift) { mods |= 1 }
        if f.contains(.control) { mods |= 2 }
        if f.contains(.option) { mods |= 4 }
        if f.contains(.command) { mods |= 8 }
        var out = [UInt8](repeating: 0, count: 64)
        let text = e.charactersIgnoringModifiers ?? ""
        let n = text.withCString { s26_term_key(term, e.keyCode, mods, $0, &out, out.count) }
        send(Array(out[..<n]))
    }

    func insertText(_ string: Any, replacementRange: NSRange) {
        let s = (string as? NSAttributedString)?.string ?? (string as? String) ?? ""
        marked = ""
        send(Array(s.utf8))
        render()
    }
    func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        marked = (string as? NSAttributedString)?.string ?? (string as? String) ?? ""
        render()
    }
    func unmarkText() { marked = ""; render() }
    func selectedRange() -> NSRange { NSRange(location: NSNotFound, length: 0) }
    func markedRange() -> NSRange { marked.isEmpty ? NSRange(location: NSNotFound, length: 0) : NSRange(location: 0, length: marked.utf16.count) }
    func hasMarkedText() -> Bool { !marked.isEmpty }
    func attributedSubstring(forProposedRange range: NSRange, actualRange: NSRangePointer?) -> NSAttributedString? { nil }
    func validAttributesForMarkedText() -> [NSAttributedString.Key] { [] }
    func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
        window?.convertToScreen(convert(cursor.frame, to: nil)) ?? .zero
    }
    func characterIndex(for point: NSPoint) -> Int { 0 }
    override func doCommand(by selector: Selector) {}
}

// ---- the bench: S25's scenarios fed straight into the terminal

final class Bench: NSObject {
    let view: TermView
    var ticks: [Double] = []
    var waiters: [() -> Void] = []
    var link: CADisplayLink?
    let out: String?

    init(view: TermView, out: String?) {
        self.view = view
        self.out = out
        super.init()
        link = view.displayLink(target: self, selector: #selector(tick))
        link!.add(to: .main, forMode: .common)
    }

    @objc func tick(_ l: CADisplayLink) {
        ticks.append(now())
        let w = waiters
        waiters = []
        w.forEach { $0() }
    }

    func nextFrame(_ f: @escaping () -> Void) { waiters.append(f) }

    /// Write a marker, render, and wait for the next display-link frame (as
    /// S25 measured the browsers: write, render, the next rAF).
    func probe(_ done: @escaping (Double) -> Void) {
        let start = now()
        let m: [UInt8] = Array("\u{1b}7\u{1b}[1;150H#\u{1b}8".utf8)
        m.withUnsafeBufferPointer { s26_term_write(view.term, $0.baseAddress, $0.count) }
        view.render()
        nextFrame { done(now() - start) }
    }

    func pct(_ xs: [Double], _ p: Double) -> Double {
        let s = xs.sorted()
        return s.isEmpty ? 0 : s[min(s.count - 1, Int(p / 100 * Double(s.count)))]
    }
    func r1(_ x: Double) -> Double { (x * 10).rounded() / 10 }

    var renders: [Double] = []

    func summary(_ name: String, _ a: Double, _ b: Double, _ lat: [Double], _ extra: [String: Double]) -> [String: Any] {
        let inside = renders.filter { $0 >= a && $0 <= b }
        let gaps = zip(inside.dropFirst(), inside).map { $0 - $1 }
        var o: [String: Any] = [
            "name": name, "seconds": r1((b - a) / 1000), "fps": r1(Double(inside.count) / ((b - a) / 1000)),
            "frame_p95": r1(pct(gaps, 95)), "frame_max": r1(gaps.max() ?? 0), "frames_over_50ms": gaps.filter { $0 > 50 }.count,
            "latency_p50": r1(pct(lat, 50)), "latency_p95": r1(pct(lat, 95)), "latency_max": r1(lat.max() ?? 0), "latency_n": lat.count,
        ]
        for (k, v) in extra { o[k] = r1(v) }
        return o
    }

    /// A flood: chunks written as fast as the main loop allows, the screen
    /// redrawn once per display frame, a probe every 100 ms.
    func flood(_ name: String, _ chunk: @escaping () -> [UInt8], _ done: @escaping ([String: Any]) -> Void) {
        s26_term_reset(view.term)
        let a = now()
        var bytes = 0
        var lat: [Double] = []
        var nextProbe = a
        var probing = false
        var dirty = false
        func frame() {
            if dirty { view.render(); dirty = false }
            if now() - a < 5000 { nextFrame(frame) }
        }
        nextFrame(frame)
        func step() {
            if now() - a >= 5000 {
                let b = now()
                done(summary(name, a, b, lat, ["mbps": Double(bytes) / 1e6 / ((b - a) / 1000)]))
                return
            }
            if !probing {
                let c = chunk()
                bytes += c.count
                c.withUnsafeBufferPointer { s26_term_write(view.term, $0.baseAddress, $0.count) }
                dirty = true
                if now() >= nextProbe {
                    probing = true
                    probe { l in lat.append(l); probing = false; nextProbe = now() + 100 }
                }
            }
            DispatchQueue.main.async(execute: step)
        }
        step()
    }

    /// A full-screen program: one screen per display frame.
    func redraw(_ name: String, _ screen: @escaping (Int) -> [UInt8], _ done: @escaping ([String: Any]) -> Void) {
        s26_term_reset(view.term)
        let a = now()
        var i = 0
        var lat: [Double] = []
        func frame() {
            if now() - a >= 5000 {
                let b = now()
                done(summary(name, a, b, lat, ["screens_per_s": Double(i) / ((b - a) / 1000)]))
                return
            }
            let s = screen(i)
            i += 1
            s.withUnsafeBufferPointer { s26_term_write(view.term, $0.baseAddress, $0.count) }
            if i % 6 == 0 {
                let start = now()
                view.render()
                nextFrame { lat.append(now() - start); frame() }
            } else {
                view.render()
                nextFrame(frame)
            }
        }
        nextFrame(frame)
    }

    func run() {
        view.onRender = { [weak self] in self?.renders.append(now()) }
        var results: [[String: Any]] = []
        var lat: [Double] = []
        let words = ["the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "request", "handled", "cache", "miss", "retry", "ok", "warn"]
        var seq = 0
        let logChunk: () -> [UInt8] = {
            var s = ""
            for _ in 0..<400 {
                let lvl = ["\u{1b}[32mINFO\u{1b}[0m", "\u{1b}[33mWARN\u{1b}[0m", "\u{1b}[31mERROR\u{1b}[0m", "\u{1b}[2mDEBUG\u{1b}[0m"][seq % 4]
                s += "\u{1b}[90m2026-10-04T06:\(String(format: "%02d", seq % 60)):00.\(seq % 1000)Z\u{1b}[0m \(lvl) \u{1b}[36msvc::worker\(seq % 9)\u{1b}[0m "
                for w in 0..<12 { s += words[(seq * 7 + w) % words.count] + " " }
                s += "id=\(seq)\r\n"
                seq += 1
            }
            return Array(s.utf8)
        }
        let htop: (Int) -> [UInt8] = { n in
            var s = "\u{1b}[H"
            for row in 0..<48 {
                let p = (row * 13 + n * 7) % 100
                let bar = String(repeating: "|", count: p / 2).padding(toLength: 50, withPad: " ", startingAt: 0)
                let a = bar.prefix(30), b = bar.suffix(20)
                s += "\u{1b}[\(row + 1);1H\u{1b}[1;37m\(String(format: "%3d", row))\u{1b}[0m [\u{1b}[32m\(a)\u{1b}[31m\(b)\u{1b}[0m] \u{1b}[\(row % 2 == 1 ? 44 : 40)m \(String(format: "%3d", p))% \(1000 + ((n + row) * 37) % 9000) jake  20 0 \(words[(n + row) % words.count].padding(toLength: 60, withPad: " ", startingAt: 0))\u{1b}[0m"
            }
            return Array(s.utf8)
        }
        let vim: (Int) -> [UInt8] = { n in
            Array("\u{1b}[1;47r\u{1b}[47;1H\n\u{1b}[47;1H\u{1b}[33m\(String(format: "%5d", n))\u{1b}[0m fn handler_\(n)(req: Request) -> Result<Response> { let id = req.id(); \(words[n % words.count]) }\u{1b}[K\u{1b}[r\u{1b}[48;1H\u{1b}[7m src/main.rs  [+]  \(n),1  \(n % 100)% \u{1b}[K\u{1b}[0m".utf8)
        }
        let yes = Array(String(repeating: "y\r\n", count: 16384).utf8)
        func finish() {
            let hz = 1000 / pct(zip(ticks.dropFirst(), ticks).map { $0 - $1 }, 50)
            let o: [String: Any] = ["engine": "s26-mac (AppKit, CoreText, a CALayer per row)", "refresh_hz": r1(hz), "rows_drawn": view.rowsDrawn, "scenarios": results]
            let data = try! JSONSerialization.data(withJSONObject: o)
            let json = String(data: data, encoding: .utf8)!
            print("S26RESULT \(json)")
            if let p = out { try? json.write(toFile: p, atomically: true, encoding: .utf8) }
            NSApp.terminate(nil)
        }
        func idle(_ k: Int) {
            if k == 100 {
                results.append(["name": "idle", "latency_p50": r1(pct(lat, 50)), "latency_p95": r1(pct(lat, 95)), "latency_max": r1(lat.max() ?? 0), "latency_n": 100])
                flood("yes", { yes }) { results.append($0)
                    self.flood("log", logChunk) { results.append($0)
                        self.redraw("htop", htop) { results.append($0)
                            self.redraw("vim-scroll", vim) { results.append($0); finish() }
                        }
                    }
                }
                return
            }
            probe { l in
                lat.append(l)
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.03) { idle(k + 1) }
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.8) {
            s26_term_reset(self.view.term)
            idle(0)
        }
    }
}

