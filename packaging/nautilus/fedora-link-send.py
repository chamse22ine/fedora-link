"""Menu clic droit Nautilus : « Envoyer vers <téléphone> » (Fedora Link).

L'extension ne fait que parler au daemon par D-Bus (SendFile) ; le daemon fait
le transfert. Si le daemon est arrêté ou qu'aucun téléphone n'est connecté,
le menu n'apparaît pas.
"""

import json
from urllib.parse import unquote, urlparse

import gi

# Nautilus déjà chargé par nautilus-python : on s'aligne sur sa version (4.0, 4.1…).
try:
    gi.require_version("Nautilus", "4.1")
except ValueError:
    gi.require_version("Nautilus", "4.0")
from gi.repository import Gio, GLib, GObject, Nautilus  # noqa: E402

BUS_NAME = "org.fedoralink.Daemon"
OBJECT_PATH = "/org/fedoralink/Daemon"
INTERFACE = "org.fedoralink.Daemon1"
# Nautilus attend la liste du menu : on ne bloque jamais plus longtemps.
LIST_TIMEOUT_MS = 400


def local_paths(files):
    """Chemins des vrais fichiers locaux (les dossiers et les emplacements distants sont ignorés)."""
    paths = []
    for f in files:
        if f.is_directory():
            continue
        uri = f.get_uri()
        if uri is None or urlparse(uri).scheme != "file":
            continue
        paths.append(unquote(urlparse(uri).path))
    return paths


def connected_devices(bus):
    """Téléphones appairés et connectés, sans jamais démarrer le daemon."""
    try:
        reply = bus.call_sync(
            BUS_NAME,
            OBJECT_PATH,
            INTERFACE,
            "ListDevices",
            None,
            GLib.VariantType("(s)"),
            Gio.DBusCallFlags.NO_AUTO_START,
            LIST_TIMEOUT_MS,
            None,
        )
        devices = json.loads(reply.unpack()[0])
    except (GLib.Error, ValueError):
        return []
    return [d for d in devices if d.get("paired") and d.get("connected")]


def send_file(bus, device_id, path):
    bus.call(
        BUS_NAME,
        OBJECT_PATH,
        INTERFACE,
        "SendFile",
        GLib.Variant("(ss)", (device_id, path)),
        None,
        Gio.DBusCallFlags.NO_AUTO_START,
        -1,
        None,
        None,
    )


class FedoraLinkSend(GObject.GObject, Nautilus.MenuProvider):
    def __init__(self):
        super().__init__()
        self._bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def get_file_items(self, files):
        paths = local_paths(files)
        if not paths:
            return []
        items = []
        for device in connected_devices(self._bus):
            item = Nautilus.MenuItem(
                name="FedoraLink::Send::" + device["id"],
                label="Envoyer vers " + device["name"],
                tip="Envoyer la sélection avec Fedora Link",
                icon="document-send-symbolic",
            )
            item.connect("activate", self._on_activate, device["id"], paths)
            items.append(item)
        return items

    def _on_activate(self, _item, device_id, paths):
        for path in paths:
            send_file(self._bus, device_id, path)
