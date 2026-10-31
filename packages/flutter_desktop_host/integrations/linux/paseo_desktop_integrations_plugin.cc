#include "include/paseo_desktop_integrations/paseo_desktop_integrations_plugin.h"
#include <gdk/gdkx.h>
#include <dlfcn.h>
#include <cstring>
#include <cstdlib>
#include <glib-unix.h>
#if __has_include(<X11/SM/SMlib.h>)
#include <X11/SM/SMlib.h>
#define PASEO_HAS_XSMP 1
#else
#define PASEO_HAS_XSMP 0
#endif

// Public libXss ABI, kept here so the optional idle diagnostic does not require
// libxss-dev at build time. The runtime library remains an explicit prerequisite.
struct ScreenSaverInfo {
  Window window;
  int state;
  int kind;
  unsigned long til_or_since;
  unsigned long idle;
  unsigned long event_mask;
};
using QueryScreenSaver = Status (*)(Display*, Drawable, ScreenSaverInfo*);
struct _PaseoDesktopIntegrationsPlugin {
  GObject parent_instance;
  FlMethodChannel* channel;
  GDBusConnection* session_bus;
  GDBusConnection* system_bus;
  guint shutdown_subscription;
  void* screensaver_library;
  gchar* gnome_client;
  guint gnome_subscription;
#if PASEO_HAS_XSMP
  void* sm_library;
  void* ice_library;
  SmcConn sm_connection;
  IceConn ice_connection;
  guint ice_source;
  decltype(&SmcCloseConnection) sm_close;
  decltype(&SmcSaveYourselfDone) sm_saved;
  decltype(&IceProcessMessages) ice_process;
#endif
};
G_DECLARE_FINAL_TYPE(PaseoDesktopIntegrationsPlugin, paseo_desktop_integrations_plugin,
                    PASEO, DESKTOP_INTEGRATIONS_PLUGIN, GObject)
G_DEFINE_TYPE(PaseoDesktopIntegrationsPlugin, paseo_desktop_integrations_plugin, g_object_get_type())

static void RespondError(FlMethodCall* call, const char* code, const char* message) {
  fl_method_call_respond_error(call, code, message, nullptr, nullptr);
}

static void EmitLifecycle(PaseoDesktopIntegrationsPlugin* self, const char* name,
                          const char* error = nullptr) {
  g_autoptr(FlValue) value = error ? fl_value_new_string(error) : nullptr;
  if (self->channel) fl_method_channel_invoke_method(self->channel, name, value,
                                                     nullptr, nullptr, nullptr);
}

static void StopLifecycle(PaseoDesktopIntegrationsPlugin* self) {
  if (self->system_bus && self->shutdown_subscription) {
    g_dbus_connection_signal_unsubscribe(self->system_bus, self->shutdown_subscription);
    self->shutdown_subscription = 0;
  }
  g_clear_object(&self->system_bus);
  if (self->session_bus && self->gnome_subscription) {
    g_dbus_connection_signal_unsubscribe(self->session_bus, self->gnome_subscription);
    self->gnome_subscription = 0;
  }
  if (self->session_bus && self->gnome_client) {
    g_dbus_connection_call(self->session_bus, "org.gnome.SessionManager",
      "/org/gnome/SessionManager", "org.gnome.SessionManager", "UnregisterClient",
      g_variant_new("(o)", self->gnome_client), nullptr, G_DBUS_CALL_FLAGS_NONE,
      2000, nullptr, nullptr, nullptr);
  }
  g_clear_pointer(&self->gnome_client, g_free);
#if PASEO_HAS_XSMP
  if (self->ice_source) {
    g_source_remove(self->ice_source);
    self->ice_source = 0;
  }
  if (self->sm_connection) {
    self->sm_close(self->sm_connection, 0, nullptr);
    self->sm_connection = nullptr;
    self->ice_connection = nullptr;
  }
  if (self->sm_library) { dlclose(self->sm_library); self->sm_library = nullptr; }
  if (self->ice_library) { dlclose(self->ice_library); self->ice_library = nullptr; }
#endif
}

static void GnomeSessionSignal(GDBusConnection* connection, const gchar*, const gchar* path,
                              const gchar*, const gchar* signal, GVariant*, gpointer data) {
  auto* self = PASEO_DESKTOP_INTEGRATIONS_PLUGIN(data);
  if (strcmp(signal, "CancelEndSession") == 0) {
    EmitLifecycle(self, "sessionEndingCancelled");
    return;
  }
  if (strcmp(signal, "Stop") == 0) {
    EmitLifecycle(self, "sessionEnded");
    return;
  }
  const bool query = strcmp(signal, "QueryEndSession") == 0;
  if (!query && strcmp(signal, "EndSession") != 0) return;
  EmitLifecycle(self, query ? "sessionEnding" : "sessionEnded");
  g_autoptr(GError) error = nullptr;
  g_autoptr(GVariant) response = g_dbus_connection_call_sync(connection,
    "org.gnome.SessionManager", path, "org.gnome.SessionManager.ClientPrivate",
    "EndSessionResponse", g_variant_new("(bs)", TRUE, ""), nullptr,
    G_DBUS_CALL_FLAGS_NONE, 2000, nullptr, &error);
  if (!response) EmitLifecycle(self, "lifecycleError", error->message);
}

#if PASEO_HAS_XSMP
static void SaveSession(SmcConn connection, SmPointer data, int, Bool shutdown, int, Bool) {
  auto* self = static_cast<PaseoDesktopIntegrationsPlugin*>(data);
  if (shutdown) EmitLifecycle(self, "sessionEnding");
  self->sm_saved(connection, True);
}
static void EndSession(SmcConn, SmPointer data) {
  EmitLifecycle(static_cast<PaseoDesktopIntegrationsPlugin*>(data), "sessionEnded");
}
static void CancelSession(SmcConn, SmPointer data) {
  EmitLifecycle(static_cast<PaseoDesktopIntegrationsPlugin*>(data), "sessionEndingCancelled");
}
static gboolean ProcessIce(gint, GIOCondition condition, gpointer data) {
  auto* self = PASEO_DESKTOP_INTEGRATIONS_PLUGIN(data);
  if ((condition & (G_IO_ERR | G_IO_HUP)) != 0 ||
      self->ice_process(self->ice_connection, nullptr, nullptr) != IceProcessMessagesSuccess) {
    self->ice_source = 0;
    EmitLifecycle(self, "lifecycleError", "The XSMP session manager disconnected");
    return G_SOURCE_REMOVE;
  }
  return G_SOURCE_CONTINUE;
}
static bool StartXsmp(PaseoDesktopIntegrationsPlugin* self) {
  if (!g_getenv("SESSION_MANAGER")) return false;
  self->sm_library = dlopen("libSM.so.6", RTLD_NOW | RTLD_LOCAL);
  self->ice_library = dlopen("libICE.so.6", RTLD_NOW | RTLD_LOCAL);
  if (!self->sm_library || !self->ice_library) return false;
  auto open = reinterpret_cast<decltype(&SmcOpenConnection)>(dlsym(self->sm_library, "SmcOpenConnection"));
  auto ice = reinterpret_cast<decltype(&SmcGetIceConnection)>(dlsym(self->sm_library, "SmcGetIceConnection"));
  auto fd = reinterpret_cast<decltype(&IceConnectionNumber)>(dlsym(self->ice_library, "IceConnectionNumber"));
  self->sm_close = reinterpret_cast<decltype(&SmcCloseConnection)>(dlsym(self->sm_library, "SmcCloseConnection"));
  self->sm_saved = reinterpret_cast<decltype(&SmcSaveYourselfDone)>(dlsym(self->sm_library, "SmcSaveYourselfDone"));
  self->ice_process = reinterpret_cast<decltype(&IceProcessMessages)>(dlsym(self->ice_library, "IceProcessMessages"));
  if (!open || !ice || !fd || !self->sm_close || !self->sm_saved || !self->ice_process) return false;
  SmcCallbacks callbacks{};
  callbacks.save_yourself = {SaveSession, self};
  callbacks.die = {EndSession, self};
  callbacks.shutdown_cancelled = {CancelSession, self};
  char* client_id = nullptr;
  char error[256]{};
  self->sm_connection = open(nullptr, nullptr, SmProtoMajor, SmProtoMinor,
    SmcSaveYourselfProcMask | SmcDieProcMask | SmcShutdownCancelledProcMask,
    &callbacks, nullptr, &client_id, sizeof(error), error);
  free(client_id);
  if (!self->sm_connection) return false;
  self->ice_connection = ice(self->sm_connection);
  self->ice_source = g_unix_fd_add(fd(self->ice_connection),
    static_cast<GIOCondition>(G_IO_IN | G_IO_ERR | G_IO_HUP), ProcessIce, self);
  return self->ice_source != 0;
}
#endif

static bool StartLogoutLifecycle(PaseoDesktopIntegrationsPlugin* self, FlMethodCall* call) {
  g_autoptr(GError) error = nullptr;
  if (!self->session_bus) self->session_bus = g_bus_get_sync(G_BUS_TYPE_SESSION, nullptr, &error);
  if (!self->session_bus) {
    RespondError(call, "session_bus_unavailable", error->message);
    return false;
  }
  const char* startup_id = g_getenv("DESKTOP_AUTOSTART_ID");
  g_autoptr(GVariant) response = g_dbus_connection_call_sync(self->session_bus,
    "org.gnome.SessionManager", "/org/gnome/SessionManager", "org.gnome.SessionManager",
    "RegisterClient", g_variant_new("(ss)", "sh.paseo.Paseo", startup_id ? startup_id : ""),
    G_VARIANT_TYPE("(o)"), G_DBUS_CALL_FLAGS_NO_AUTO_START, 2000, nullptr, &error);
  if (response) {
    g_variant_get(response, "(o)", &self->gnome_client);
    self->gnome_subscription = g_dbus_connection_signal_subscribe(self->session_bus,
      "org.gnome.SessionManager", "org.gnome.SessionManager.ClientPrivate", nullptr,
      self->gnome_client, nullptr, G_DBUS_SIGNAL_FLAGS_NONE, GnomeSessionSignal, self, nullptr);
    return true;
  }
#if PASEO_HAS_XSMP
  if (StartXsmp(self)) return true;
#endif
  RespondError(call, "logout_unavailable",
    "No GNOME SessionManager or usable XSMP session manager is available. "
    "Close-to-tray must stay disabled to avoid intercepting logout. "
    "XSMP needs libSM/libICE runtime libraries, SM headers at build time and SESSION_MANAGER.");
  return false;
}

static void ShutdownSignal(GDBusConnection*, const gchar*, const gchar*,
                          const gchar*, const gchar*, GVariant* parameters, gpointer data) {
  gboolean shutting_down = FALSE;
  g_variant_get(parameters, "(b)", &shutting_down);
  auto* self = PASEO_DESKTOP_INTEGRATIONS_PLUGIN(data);
  EmitLifecycle(self, shutting_down ? "sessionEnded" : "sessionEndingCancelled");
}

static void HandleMethod(FlMethodChannel*, FlMethodCall* call, gpointer data) {
  auto* self = PASEO_DESKTOP_INTEGRATIONS_PLUGIN(data);
  const char* method = fl_method_call_get_name(call);
  if (strcmp(method, "initializeLifecycle") == 0) {
    if (!self->system_bus) {
      g_autoptr(GError) error = nullptr;
      self->system_bus = g_bus_get_sync(G_BUS_TYPE_SYSTEM, nullptr, &error);
      if (!self->system_bus) {
        RespondError(call, "system_bus_unavailable", error->message);
        return;
      }
      self->shutdown_subscription = g_dbus_connection_signal_subscribe(
        self->system_bus, "org.freedesktop.login1", "org.freedesktop.login1.Manager",
        "PrepareForShutdown", "/org/freedesktop/login1", nullptr,
        G_DBUS_SIGNAL_FLAGS_NONE, ShutdownSignal, self, nullptr);
      if (!StartLogoutLifecycle(self, call)) {
        StopLifecycle(self);
        return;
      }
    }
    fl_method_call_respond_success(call, nullptr, nullptr);
  } else if (strcmp(method, "disposeLifecycle") == 0) {
    StopLifecycle(self);
    fl_method_call_respond_success(call, nullptr, nullptr);
  } else if (strcmp(method, "idleMilliseconds") == 0) {
    auto* display = gdk_display_get_default();
    if (!display || !GDK_IS_X11_DISPLAY(display) ||
        g_strcmp0(g_getenv("XDG_SESSION_TYPE"), "wayland") == 0) {
      RespondError(call, "idle_unsupported", "Global idle time requires an X11 session; Wayland provides no portable global idle API");
      return;
    }
    Display* xdisplay = gdk_x11_display_get_xdisplay(display);
    int opcode, event_base, error_base;
    if (!XQueryExtension(xdisplay, "MIT-SCREEN-SAVER", &opcode, &event_base, &error_base)) {
      RespondError(call, "idle_unavailable", "The XScreenSaver extension is unavailable");
      return;
    }
    if (!self->screensaver_library) {
      self->screensaver_library = dlopen("libXss.so.1", RTLD_NOW | RTLD_LOCAL);
    }
    if (!self->screensaver_library) {
      RespondError(call, "idle_unavailable", "System idle time requires the optional libXss.so.1 runtime library");
      return;
    }
    auto query = reinterpret_cast<QueryScreenSaver>(
      dlsym(self->screensaver_library, "XScreenSaverQueryInfo"));
    if (!query) {
      RespondError(call, "idle_unavailable", "libXss does not export XScreenSaverQueryInfo");
      return;
    }
    ScreenSaverInfo info{};
    const Status status = query(xdisplay, DefaultRootWindow(xdisplay), &info);
    const auto idle = static_cast<int64_t>(info.idle);
    if (!status) {
      RespondError(call, "idle_failed", "XScreenSaverQueryInfo failed");
      return;
    }
    g_autoptr(FlValue) value = fl_value_new_int(idle);
    fl_method_call_respond_success(call, value, nullptr);
  } else if (strcmp(method, "setBadgeCount") == 0) {
    FlValue* args = fl_method_call_get_args(call);
    FlValue* count = args && fl_value_get_type(args) == FL_VALUE_TYPE_MAP
      ? fl_value_lookup_string(args, "count") : nullptr;
    if (!count || fl_value_get_type(count) != FL_VALUE_TYPE_INT || fl_value_get_int(count) < 0) {
      RespondError(call, "invalid_count", "count must be a nonnegative integer");
      return;
    }
    g_autoptr(GError) error = nullptr;
    if (!self->session_bus) self->session_bus = g_bus_get_sync(G_BUS_TYPE_SESSION, nullptr, &error);
    if (!self->session_bus) {
      RespondError(call, "session_bus_unavailable", error->message);
      return;
    }
    // This standard launcher protocol is implemented by Unity, KDE and several
    // GNOME docks. Delivery is not a guarantee that the user's shell shows badges.
    GVariantBuilder properties;
    g_variant_builder_init(&properties, G_VARIANT_TYPE("a{sv}"));
    const gint64 number = fl_value_get_int(count);
    g_variant_builder_add(&properties, "{sv}", "count", g_variant_new_int64(number));
    g_variant_builder_add(&properties, "{sv}", "count-visible", g_variant_new_boolean(number > 0));
    if (!g_dbus_connection_emit_signal(self->session_bus, nullptr,
        "/sh/paseo/Paseo", "com.canonical.Unity.LauncherEntry", "Update",
        g_variant_new("(sa{sv})", "application://sh.paseo.Paseo.desktop", &properties), &error)) {
      RespondError(call, "badge_failed", error->message);
      return;
    }
    fl_method_call_respond_success(call, nullptr, nullptr);
  } else {
    fl_method_call_respond_not_implemented(call, nullptr);
  }
}

static void paseo_desktop_integrations_plugin_dispose(GObject* object) {
  auto* self = PASEO_DESKTOP_INTEGRATIONS_PLUGIN(object);
  StopLifecycle(self);
  if (self->screensaver_library) {
    dlclose(self->screensaver_library);
    self->screensaver_library = nullptr;
  }
  g_clear_object(&self->session_bus);
  if (self->channel) {
    g_object_remove_weak_pointer(G_OBJECT(self->channel),
      reinterpret_cast<gpointer*>(&self->channel));
    self->channel = nullptr;
  }
  G_OBJECT_CLASS(paseo_desktop_integrations_plugin_parent_class)->dispose(object);
}
static void paseo_desktop_integrations_plugin_class_init(PaseoDesktopIntegrationsPluginClass* klass) {
  G_OBJECT_CLASS(klass)->dispose = paseo_desktop_integrations_plugin_dispose;
}
static void paseo_desktop_integrations_plugin_init(PaseoDesktopIntegrationsPlugin*) {}

void paseo_desktop_integrations_plugin_register_with_registrar(FlPluginRegistrar* registrar) {
  auto* self = PASEO_DESKTOP_INTEGRATIONS_PLUGIN(
    g_object_new(paseo_desktop_integrations_plugin_get_type(), nullptr));
  g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
  self->channel = fl_method_channel_new(fl_plugin_registrar_get_messenger(registrar),
    "paseo/desktop_integrations", FL_METHOD_CODEC(codec));
  // The binary messenger retains the channel and its handler retains this
  // plugin. Keep only a weak channel pointer to avoid a reference cycle.
  g_object_add_weak_pointer(G_OBJECT(self->channel),
    reinterpret_cast<gpointer*>(&self->channel));
  fl_method_channel_set_method_call_handler(self->channel, HandleMethod,
    g_object_ref(self), g_object_unref);
  g_object_unref(self->channel);
  g_object_unref(self);
}
