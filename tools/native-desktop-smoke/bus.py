import json, os, pathlib, signal, subprocess, sys, time
import dbus, dbus.service, dbus.mainloop.glib
import gi
gi.require_version('Gdk', '3.0')
from gi.repository import GLib, Gdk
Gdk.init([])

dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
proof = pathlib.Path(os.environ['PASEO_DESKTOP_SMOKE_PROOF'])
panel = os.environ.get('PASEO_SMOKE_PANEL') != '0'
items, clients, names = [], [], []
def record(phase, **values):
    with (proof / 'bus.jsonl').open('a') as stream: stream.write(json.dumps({'phase': phase, **values}, default=str) + '\n')
class Client(dbus.service.Object):
    @dbus.service.method('org.gnome.SessionManager.ClientPrivate', in_signature='bs', out_signature='')
    def EndSessionResponse(self, okay, reason): record('session-response', okay=bool(okay), reason=str(reason))
    @dbus.service.signal('org.gnome.SessionManager.ClientPrivate', signature='u')
    def EndSession(self, flags): pass
class Session(dbus.service.Object):
    @dbus.service.method('org.gnome.SessionManager', in_signature='ss', out_signature='o')
    def RegisterClient(self, app, startup):
        path = '/org/gnome/SessionManager/Client' + str(len(clients) + 1)
        clients.append(Client(bus, path)); record('session-register', app=str(app), path=path)
        return dbus.ObjectPath(path)
    @dbus.service.method('org.gnome.SessionManager', in_signature='o', out_signature='')
    def UnregisterClient(self, path): record('session-unregister', path=str(path))
class Notifications(dbus.service.Object):
    @dbus.service.method('org.freedesktop.Notifications', in_signature='', out_signature='ssss')
    def GetServerInformation(self): return ('Paseo smoke desktop', 'Paseo', '1', '1.2')
    @dbus.service.method('org.freedesktop.Notifications', in_signature='', out_signature='as')
    def GetCapabilities(self): return ['actions', 'body']
class Watcher(dbus.service.Object):
    @dbus.service.method('org.kde.StatusNotifierWatcher', in_signature='s', out_signature='', sender_keyword='sender')
    def RegisterStatusNotifierItem(self, service, sender=None):
        service = str(service)
        item = (str(sender), service) if service.startswith('/') else (service, '/StatusNotifierItem')
        if item not in items: items.append(item)
        self.StatusNotifierItemRegistered(item[0] + item[1]); record('tray-item-registered', service=item[0], path=item[1])
    @dbus.service.method('org.kde.StatusNotifierWatcher', in_signature='s', out_signature='')
    def RegisterStatusNotifierHost(self, service): record('tray-host-registered', service=str(service))
    @dbus.service.signal('org.kde.StatusNotifierWatcher', signature='s')
    def StatusNotifierItemRegistered(self, service): pass
    @dbus.service.method('org.freedesktop.DBus.Properties', in_signature='ss', out_signature='v')
    def Get(self, interface, name): return self.GetAll(interface)[name]
    @dbus.service.method('org.freedesktop.DBus.Properties', in_signature='s', out_signature='a{sv}')
    def GetAll(self, interface):
        return {'IsStatusNotifierHostRegistered': dbus.Boolean(panel), 'ProtocolVersion': dbus.Int32(0), 'RegisteredStatusNotifierItems': dbus.Array([service + path for service, path in items], signature='s')}
for name in ['org.gnome.SessionManager', 'org.freedesktop.Notifications', 'org.kde.StatusNotifierWatcher', 'org.kde.StatusNotifierHost-paseo-smoke']:
    names.append(dbus.service.BusName(name, bus))
services = [Session(bus, '/org/gnome/SessionManager'), Notifications(bus, '/org/freedesktop/Notifications'), Watcher(bus, '/StatusNotifierWatcher')]

def properties():
    if not items: raise RuntimeError('No real native tray item was registered')
    service, path = items[0]
    obj = bus.get_object(service, path, introspect=False)
    values = dbus.Interface(obj, 'org.freedesktop.DBus.Properties').GetAll('org.kde.StatusNotifierItem')
    return service, obj, values

def menu_command(label):
    service, obj, values = properties()
    menu_path = str(values['Menu'])
    menu = dbus.Interface(bus.get_object(service, menu_path, introspect=False), 'com.canonical.dbusmenu')
    revision, layout = menu.GetLayout(dbus.Int32(0), dbus.Int32(-1), dbus.Array([], signature='s'))
    entries = []
    def walk(node):
        item_id, props, children = node
        if 'label' in props: entries.append((int(item_id), str(props['label'])))
        for child in children: walk(child)
    walk(layout)
    record('actual-native-tray-menu', entries=entries, status=str(values.get('Status')), icon=str(values.get('IconName', '')))
    assert any(text == 'Open Paseo' for _, text in entries), entries
    selected = next(item_id for item_id, text in entries if text == label)
    menu.Event(dbus.Int32(selected), 'clicked', dbus.Int32(0, variant_level=1), dbus.UInt32(0), reply_handler=lambda: record('menu-click-delivered', label=label), error_handler=lambda error: fail(str(error)))

loop = GLib.MainLoop()
log = (proof / 'app.log').open('w')
app = subprocess.Popen([sys.argv[1]], stdout=log, stderr=subprocess.STDOUT)
start = time.monotonic()
last_command = None
failed = False
captured = set()
before_hide_pixels = None

def fail(message):
    global failed
    failed = True; record('FAIL', error=message)
    app.terminate(); loop.quit()

def tick():
    global last_command, before_hide_pixels
    try:
        events_path = proof / 'events.jsonl'
        events = events_path.read_text() if events_path.exists() else ''
        for phase in ['native-vim-delete-undo-unicode-save', 'multiwindow-close-retains-daemon', 'last-window-hidden-to-tray', 'tray-activation-restores-same-engine']:
            if phase in events and phase not in captured:
                root = Gdk.get_default_root_window()
                pixels = Gdk.pixbuf_get_from_window(root, 0, 0, root.get_width(), root.get_height())
                assert pixels is not None, 'Private Xvfb screenshot unavailable'
                if phase == 'multiwindow-close-retains-daemon':
                    before_hide_pixels = bytes(pixels.get_pixels())
                if phase == 'tray-activation-restores-same-engine':
                    assert before_hide_pixels is not None, 'No pre-hide rendering baseline'
                    # GTK visibility can precede presentation. Compare the same
                    # window before/after hiding, without pinning theme pixels.
                    if bytes(pixels.get_pixels()) != before_hide_pixels:
                        continue
                    record('restored-pixels-match-before-hide')
                pixels.savev(str(proof / (phase + '.png')), 'png', [], [])
                captured.add(phase)
                (proof / ('captured-' + phase)).touch()
        if app.poll() is not None:
            events = (proof / 'events.jsonl').read_text() if (proof / 'events.jsonl').exists() else ''
            expected = 'expect-no-panel-quit' if not panel else ('expect-session-logout-quit' if os.environ.get('PASEO_SMOKE_LOGOUT') == '1' else 'expect-real-tray-menu-quit')
            if app.returncode != 0 or expected not in events or '"phase":"FAIL"' in events:
                fail('Desktop exited unexpectedly: ' + str(app.returncode)); return False
            record('native-app-exited', exitCode=app.returncode, expected=expected)
            loop.quit(); return False
        if time.monotonic() - start > 150:
            fail('Native desktop scenario exceeded 150 seconds'); return False
        command_path = proof / 'panel-command.json'
        if command_path.exists():
            command = json.loads(command_path.read_text())['action']
            if command != last_command:
                last_command = command
                if command == 'activate':
                    service, obj, values = properties()
                    record('actual-tray-item', service=service, title=str(values.get('Title')), status=str(values.get('Status')))
                    dbus.Interface(obj, 'org.kde.StatusNotifierItem').Activate(dbus.Int32(0), dbus.Int32(0), reply_handler=lambda: record('tray-activate-delivered'), error_handler=lambda error: fail(str(error)))
                elif command == 'quit': menu_command('Quit Paseo')
                elif command == 'logout':
                    record('emitting-session-end', clients=len(clients))
                    for client in clients: client.EndSession(dbus.UInt32(0))
    except Exception as error: fail(repr(error)); return False
    return True
GLib.timeout_add(100, tick)
try: loop.run()
finally:
    if app.poll() is None:
        app.terminate()
        try: app.wait(timeout=5)
        except subprocess.TimeoutExpired: app.kill(); app.wait()
    log.close()
sys.exit(1 if failed else 0)
