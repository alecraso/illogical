# "Open in Arugula" in Nautilus's right-click menu (M47): on a folder, or
# on a folder's background, it opens a new tab there in the running app
# (arugula://open?cwd=DIR, which the app's .desktop file or the AppImage
# claims; a first launch starts the app). The .deb and .rpm put this in
# /usr/share/nautilus-python/extensions; Nautilus loads it when
# python3-nautilus (Fedora: nautilus-python) is installed. Works with
# Nautilus 3.0 (GTK 3, Ubuntu 22.04) and 4.0 (GTK 4).
import urllib.parse

import gi

for _v in ("4.0", "3.0"):
    try:
        gi.require_version("Nautilus", _v)
        break
    except ValueError:
        continue

from gi.repository import GObject, Gio, Nautilus  # noqa: E402


def _link(path):
    return "arugula://open?cwd=" + urllib.parse.quote(path, safe="")


def _open(path):
    Gio.AppInfo.launch_default_for_uri(_link(path), None)


def _folder(f):
    """The local path of a folder, else None."""
    if f.get_uri_scheme() != "file" or not f.is_directory():
        return None
    return f.get_location().get_path()


class ArugulaMenu(GObject.GObject, Nautilus.MenuProvider):
    def _item(self, name, paths):
        item = Nautilus.MenuItem(
            name="Arugula::" + name,
            label="Open in Arugula",
            tip="Open a new Arugula tab in this folder",
        )
        item.connect("activate", lambda _item: [_open(p) for p in paths])
        return item

    # Nautilus 3.0 passes (window, files); 4.0 passes (files).
    def get_file_items(self, *args):
        paths = [_folder(f) for f in args[-1]]
        if not paths or None in paths:
            return []
        return [self._item("OpenFolder", paths)]

    def get_background_items(self, *args):
        path = _folder(args[-1])
        return [self._item("OpenHere", [path])] if path else []
