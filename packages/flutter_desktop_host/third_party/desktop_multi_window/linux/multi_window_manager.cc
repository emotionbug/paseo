#include "multi_window_manager.h"

#include <iomanip>
#include <random>
#include <sstream>
#include <epoxy/egl.h>

#include "desktop_multi_window_plugin_internal.h"
#include "flutter_window.h"
#include "include/desktop_multi_window/desktop_multi_window_plugin.h"
#include "window_configuration.h"
#ifdef GDK_WINDOWING_X11
#include <gdk/gdkx.h>
#endif

namespace {

// Creation can be dispatched while GTK is painting an existing window. A new
// Flutter engine must not steal that window's GLX/EGL binding.
class GraphicsBindingScope {
 public:
  GraphicsBindingScope()
      : gtk_context_(gdk_gl_context_get_current()),
        display_(eglGetCurrentDisplay()),
        context_(eglGetCurrentContext()),
        draw_(eglGetCurrentSurface(EGL_DRAW)),
        read_(eglGetCurrentSurface(EGL_READ)),
        api_(eglQueryAPI()) {
    if (gtk_context_ != nullptr) g_object_ref(gtk_context_);
    ClearCurrent();
    // GTK on Wayland can leave the desktop OpenGL API selected; Flutter's
    // engine creates OpenGL ES contexts. Restore GTK's API in the destructor.
    eglBindAPI(EGL_OPENGL_ES_API);
  }

  ~GraphicsBindingScope() {
    ClearCurrent();
    eglBindAPI(api_);
    if (context_ != EGL_NO_CONTEXT) {
      if (!eglMakeCurrent(display_, draw_, read_, context_)) {
        g_warning("Failed to restore the creating window's EGL context");
      }
    } else if (gtk_context_ != nullptr) {
      gdk_gl_context_make_current(gtk_context_);
    }
    g_clear_object(&gtk_context_);
  }

  GraphicsBindingScope(const GraphicsBindingScope&) = delete;
  GraphicsBindingScope& operator=(const GraphicsBindingScope&) = delete;

  static void ClearCurrent() {
    const EGLDisplay display = eglGetCurrentDisplay();
    if (display != EGL_NO_DISPLAY) {
      eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    }
    gdk_gl_context_clear_current();
  }

 private:
  GdkGLContext* gtk_context_;
  EGLDisplay display_;
  EGLContext context_;
  EGLSurface draw_;
  EGLSurface read_;
  EGLenum api_;
};

std::string GenerateWindowId() {
  std::random_device rd;
  std::mt19937_64 gen(rd());
  std::uniform_int_distribution<uint64_t> dis;

  uint64_t part1 = dis(gen);
  uint64_t part2 = dis(gen);

  part1 = (part1 & 0xFFFFFFFFFFFF0FFFULL) | 0x0000000000004000ULL;
  part2 = (part2 & 0x3FFFFFFFFFFFFFFFULL) | 0x8000000000000000ULL;

  char uuid_str[37];
  snprintf(uuid_str, sizeof(uuid_str), "%08x-%04x-%04x-%04x-%012llx",
           static_cast<uint32_t>(part1 >> 32),
           static_cast<uint16_t>(part1 >> 16), static_cast<uint16_t>(part1),
           static_cast<uint16_t>(part2 >> 48), part2 & 0xFFFFFFFFFFFFULL);

  return std::string(uuid_str);
}

WindowCreatedCallback _g_window_created_callback = nullptr;

}  // namespace

// static
MultiWindowManager* MultiWindowManager::Instance() {
  static auto manager = std::make_shared<MultiWindowManager>();
  return manager.get();
}

MultiWindowManager::MultiWindowManager() : windows_() {}

MultiWindowManager::~MultiWindowManager() = default;

std::string MultiWindowManager::Create(FlValue* args, GError** error) {
  GraphicsBindingScope graphics_binding;
  g_autoptr(GdkDisplay) previous_display =
      GDK_DISPLAY(g_object_ref(gdk_display_get_default()));
  // GDK owns the initial reference until gdk_display_close().
  GdkDisplay* display = gdk_display_open(gdk_display_get_name(previous_display));
  if (display == nullptr) {
    g_set_error_literal(error, G_IO_ERROR, G_IO_ERROR_FAILED,
                       "Unable to open a graphics display for the new window");
    return {};
  }
  // Flutter 3.47 terminates its EGLDisplay when an engine is released. Give
  // each engine its own native connection so closing it cannot invalidate
  // contexts used by the coordinator or another user window.
  gdk_display_manager_set_default_display(gdk_display_manager_get(), display);
  WindowConfiguration config = WindowConfiguration::FromFlValue(args);
  std::string window_id = GenerateWindowId();

  // Create GTK window
  GtkApplication* app = GTK_APPLICATION(g_application_get_default());
  GtkWindow* window = GTK_WINDOW(gtk_application_window_new(app));
  gtk_window_set_screen(window, gdk_display_get_default_screen(display));
  gtk_application_add_window(app, window);

  gboolean use_header_bar = TRUE;
#ifdef GDK_WINDOWING_X11
  GdkScreen* screen = gtk_window_get_screen(window);
  if (GDK_IS_X11_SCREEN(screen)) {
    const gchar* wm_name = gdk_x11_screen_get_window_manager_name(screen);
    if (g_strcmp0(wm_name, "GNOME Shell") != 0) {
      use_header_bar = FALSE;
    }
  }
#endif
  if (use_header_bar) {
    GtkHeaderBar* header_bar = GTK_HEADER_BAR(gtk_header_bar_new());
    gtk_widget_show(GTK_WIDGET(header_bar));
    gtk_header_bar_set_title(header_bar, "");
    gtk_header_bar_set_show_close_button(header_bar, TRUE);
    gtk_window_set_titlebar(window, GTK_WIDGET(header_bar));
  } else {
    gtk_window_set_title(window, "");
  }

  gtk_window_set_default_size(window, 1280, 720);

  gtk_window_set_title(window, "");
  if (config.hidden_at_launch) {
    gtk_widget_realize(GTK_WIDGET(window));
  } else {
    gtk_widget_show(GTK_WIDGET(window));
  }

  // Create FlutterWindow instance
  auto w = std::make_unique<FlutterWindow>(window_id, config.arguments,
                                           GTK_WIDGET(window));
  windows_[window_id] = std::move(w);

  // Setup Flutter project
  g_autoptr(FlDartProject) project = fl_dart_project_new();
  const char* entrypoint_args[] = {"multi_window", window_id.c_str(),
                                   config.arguments.c_str(), nullptr};
  fl_dart_project_set_dart_entrypoint_arguments(
      project, const_cast<char**>(entrypoint_args));

  // Create Flutter view
  auto fl_view = fl_view_new(project);
  g_object_weak_ref(
      G_OBJECT(fl_view_get_engine(fl_view)),
      +[](gpointer data, GObject*) {
        // The engine may finish its raster teardown after GTK destroys the
        // window. Close its display only after that teardown, on the GTK loop.
        g_idle_add_full(
            G_PRIORITY_DEFAULT_IDLE,
            +[](gpointer owned_display) -> gboolean {
              gdk_display_close(GDK_DISPLAY(owned_display));
              return G_SOURCE_REMOVE;
            },
            data, nullptr);
      },
      display);
  gtk_widget_show(GTK_WIDGET(fl_view));
  gtk_container_add(GTK_CONTAINER(window), GTK_WIDGET(fl_view));

  // Issues from flutter/engine: https://github.com/flutter/engine/pull/40033
  // Prevent delete-event from flutter engine shell, which will quit the whole
  // appplication when the window is closed. this can be done by
  // [window_manager] plugin, but we need it here if user is not using that
  // plugin.
  guint handler_id = g_signal_handler_find(window, G_SIGNAL_MATCH_DATA, 0, 0,
                                           NULL, NULL, fl_view);
  if (handler_id > 0) {
    g_signal_handler_disconnect(window, handler_id);
  }

  // Call window created callback
  if (_g_window_created_callback) {
    _g_window_created_callback(FL_PLUGIN_REGISTRY(fl_view));
  }

  ObserveWindowClose(window_id, window);
  // Register plugin
  g_autoptr(FlPluginRegistrar) desktop_multi_window_registrar =
      fl_plugin_registry_get_registrar_for_plugin(FL_PLUGIN_REGISTRY(fl_view),
                                                  "DesktopMultiWindowPlugin");

  desktop_multi_window_plugin_register_with_registrar_internal(
      desktop_multi_window_registrar, windows_[window_id].get());

  gtk_widget_grab_focus(GTK_WIDGET(fl_view));

  // Notify all windows about the change
  NotifyWindowsChanged();

  gdk_display_manager_set_default_display(gdk_display_manager_get(),
                                         previous_display);
  return window_id;
}

void MultiWindowManager::AttachMainWindow(GtkWidget* window_widget,
                                          FlPluginRegistrar* registrar) {
  // check window widget is in windows_
  for (const auto& pair : windows_) {
    if (pair.second->GetWindow() == GTK_WINDOW(window_widget)) {
      return;
    }
  }

  const std::string main_window_id = GenerateWindowId();
  auto window =
      std::make_unique<FlutterWindow>(main_window_id, "", window_widget);
  windows_[main_window_id] = std::move(window);

  ObserveWindowClose(main_window_id, GTK_WINDOW(window_widget));
  desktop_multi_window_plugin_register_with_registrar_internal(
      registrar, windows_[main_window_id].get());

  // Notify all windows about the change
  NotifyWindowsChanged();
}

void MultiWindowManager::ObserveWindowClose(const std::string& window_id,
                                            GtkWindow* window) {
  // A compositor can be finalized after its window's destroy callback, leaving
  // an EGL binding behind. GLVND rejects GTK's next GLX bind with BadAccess.
  // Hand the platform thread back to GTK before its frame-paint phase.
  auto observe_frame_clock = +[](GtkWidget* widget, gpointer) {
    g_signal_connect_object(
        gtk_widget_get_frame_clock(widget), "before-paint",
        G_CALLBACK(+[](GdkFrameClock*, gpointer) {
          if (eglGetCurrentContext() != EGL_NO_CONTEXT) {
            GraphicsBindingScope::ClearCurrent();
          }
        }),
        G_OBJECT(widget), static_cast<GConnectFlags>(0));
  };
  if (gtk_widget_get_realized(GTK_WIDGET(window))) {
    observe_frame_clock(GTK_WIDGET(window), nullptr);
  } else {
    g_signal_connect(window, "realize", G_CALLBACK(observe_frame_clock), nullptr);
  }
  g_signal_connect(
      GTK_WIDGET(window), "destroy",
      G_CALLBACK(+[](GtkWidget* widget, gpointer arg) {
        auto* window_id_ptr = static_cast<std::string*>(arg);

        GtkWidget* child = gtk_bin_get_child(GTK_BIN(widget));
        if (child && FL_IS_VIEW(child)) {
          gtk_container_remove(GTK_CONTAINER(widget), child);
        }
        // Flutter's compositor teardown leaves its EGL context current. Release
        // it before GTK binds another window's GLX context on this thread.
        GraphicsBindingScope::ClearCurrent();

        MultiWindowManager::Instance()->RemoveWindow(*window_id_ptr);
        delete window_id_ptr;
      }),
      new std::string(window_id));
}

FlutterWindow* MultiWindowManager::GetWindow(const std::string& window_id) {
  auto it = windows_.find(window_id);
  if (it != windows_.end()) {
    return it->second.get();
  }
  return nullptr;
}

FlValue* MultiWindowManager::GetAllWindows() {
  g_autoptr(FlValue) windows = fl_value_new_list();
  for (const auto& pair : windows_) {
    g_autoptr(FlValue) window_info = fl_value_new_map();
    fl_value_set_string_take(
        window_info, "windowId",
        fl_value_new_string(pair.second->GetWindowId().c_str()));
    fl_value_set_string_take(
        window_info, "windowArgument",
        fl_value_new_string(pair.second->GetWindowArgument().c_str()));
    fl_value_append_take(windows, fl_value_ref(window_info));
  }
  return fl_value_ref(windows);
}

std::vector<std::string> MultiWindowManager::GetAllWindowIds() {
  std::vector<std::string> window_ids;
  for (const auto& pair : windows_) {
    window_ids.push_back(pair.first);
  }
  return window_ids;
}

void MultiWindowManager::NotifyWindowsChanged() {
  auto window_ids = GetAllWindowIds();

  g_autoptr(FlValue) window_ids_list = fl_value_new_list();
  for (const auto& id : window_ids) {
    fl_value_append_take(window_ids_list, fl_value_new_string(id.c_str()));
  }

  g_autoptr(FlValue) data = fl_value_new_map();
  fl_value_set_string_take(data, "windowIds", fl_value_ref(window_ids_list));

  for (const auto& pair : windows_) {
    pair.second->NotifyWindowEvent("onWindowsChanged", data);
  }
}

void MultiWindowManager::RemoveWindow(const std::string& window_id) {
  g_warning("RemoveWindow: %s", window_id.c_str());
  windows_.erase(window_id);
  NotifyWindowsChanged();
}

void desktop_multi_window_plugin_set_window_created_callback(
    WindowCreatedCallback callback) {
  _g_window_created_callback = callback;
}
