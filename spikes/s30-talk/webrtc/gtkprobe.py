#!/usr/bin/env python3
"""Load probe.html in WebKitGTK (WebKit2 4.1, the API Tauri 2 uses on Linux)
and print its result. --enable sets enable-webrtc and enable-media-stream,
as the desktop app would have to; without it, WebKit's defaults apply.
--mock uses WebKit's mock capture devices (geek has no microphone)."""
import sys, json, gi
gi.require_version("Gtk", "3.0"); gi.require_version("WebKit2", "4.1")
from gi.repository import Gtk, WebKit2, GLib

url = sys.argv[1]
enable = "--enable" in sys.argv
mock = "--mock" in sys.argv
s = WebKit2.Settings()
if enable:
    s.set_property("enable-webrtc", True)
    s.set_property("enable-media-stream", True)
if mock:
    s.set_property("enable-mock-capture-devices", True)
ucm = WebKit2.UserContentManager()
ucm.register_script_message_handler("probe")
def got(_m, res):
    v = res.get_js_value().to_string()
    print(json.dumps({"webkit": "%d.%d.%d" % (WebKit2.get_major_version(), WebKit2.get_minor_version(), WebKit2.get_micro_version()),
                      "enable": enable, "mock": mock, "result": json.loads(v)}, indent=1))
    Gtk.main_quit()
ucm.connect("script-message-received::probe", got)
wv = WebKit2.WebView.new_with_user_content_manager(ucm)
wv.set_settings(s)
def perm(_wv, req):
    print("permission-request:", type(req).__name__, file=sys.stderr)
    req.allow(); return True
wv.connect("permission-request", perm)
w = Gtk.Window(); w.set_default_size(700, 800); w.add(wv); w.show_all()
wv.load_uri(url)
GLib.timeout_add_seconds(60, lambda: (print("TIMEOUT"), Gtk.main_quit()))
Gtk.main()
