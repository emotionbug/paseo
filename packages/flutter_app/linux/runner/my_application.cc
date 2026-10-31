#include "my_application.h"

#include <flutter_linux/flutter_linux.h>
#include <desktop_multi_window/desktop_multi_window_plugin.h>
#ifdef GDK_WINDOWING_X11
#include <gdk/gdkx.h>
#endif

#include "flutter/generated_plugin_registrant.h"

struct _MyApplication {
  GtkApplication parent_instance;
  GtkWindow* primary_window;
  FlMethodChannel* host_channel;
  FlValue* pending_activations;
  gboolean host_ready;
  gboolean quit_pending;
};

G_DEFINE_TYPE(MyApplication, my_application, GTK_TYPE_APPLICATION)

static void send_activation(MyApplication* self, FlValue* arguments) {
  if (self->host_ready) {
    fl_method_channel_invoke_method(self->host_channel, "activate", arguments,
                                   nullptr, nullptr, nullptr);
  } else {
    fl_value_append(self->pending_activations, arguments);
  }
}

static void request_quit(GSimpleAction*, GVariant*, gpointer user_data) {
  MyApplication* self = MY_APPLICATION(user_data);
  if (!self->host_ready) {
    self->quit_pending = TRUE;
    return;
  }
  fl_method_channel_invoke_method(self->host_channel, "quit", nullptr,
                                 nullptr, nullptr, nullptr);
}

static void host_method_call(FlMethodChannel*, FlMethodCall* call,
                             gpointer user_data) {
  MyApplication* self = MY_APPLICATION(user_data);
  const gchar* method = fl_method_call_get_name(call);
  if (g_strcmp0(method, "ready") == 0) {
    self->host_ready = TRUE;
    fl_method_call_respond_success(call, self->pending_activations, nullptr);
    fl_value_unref(self->pending_activations);
    self->pending_activations = fl_value_new_list();
    if (self->quit_pending) {
      self->quit_pending = FALSE;
      request_quit(nullptr, nullptr, self);
    }
  } else if (g_strcmp0(method, "allowTermination") == 0) {
    // Dart has drained services and guarded user windows. End the process even
    // if a secondary engine is still initializing and cannot yet accept close.
    fl_method_call_respond_success(call, nullptr, nullptr);
    g_idle_add_full(
        G_PRIORITY_DEFAULT_IDLE,
        [](gpointer application) -> gboolean {
          g_application_quit(G_APPLICATION(application));
          return G_SOURCE_REMOVE;
        },
        g_object_ref(self), g_object_unref);
  } else {
    fl_method_call_respond_not_implemented(call, nullptr);
  }
}

// Implements GApplication::activate.
static void my_application_activate(GApplication* application) {
  MyApplication* self = MY_APPLICATION(application);
  if (self->primary_window != nullptr) {
    g_autoptr(FlValue) activation = fl_value_new_map();
    send_activation(self, activation);
    return;
  }
  GtkWindow* window =
      GTK_WINDOW(gtk_application_window_new(GTK_APPLICATION(application)));
  self->primary_window = window;
  g_object_add_weak_pointer(G_OBJECT(window),
                           reinterpret_cast<gpointer*>(&self->primary_window));
  gtk_window_set_icon_name(window, "sh.paseo.Paseo");

  // Use a header bar when running in GNOME as this is the common style used
  // by applications and is the setup most users will be using (e.g. Ubuntu
  // desktop).
  // If running on X and not using GNOME then just use a traditional title bar
  // in case the window manager does more exotic layout, e.g. tiling.
  // If running on Wayland assume the header bar will work (may need changing
  // if future cases occur).
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
    gtk_header_bar_set_title(header_bar, "Paseo Sessions");
    gtk_header_bar_set_show_close_button(header_bar, TRUE);
    gtk_window_set_titlebar(window, GTK_WIDGET(header_bar));
  } else {
    gtk_window_set_title(window, "Paseo Sessions");
  }

  gtk_window_set_default_size(window, 1280, 720);

  g_autoptr(FlDartProject) project = fl_dart_project_new();

  FlView* view = fl_view_new(project);
  GdkRGBA background_color;
  // Background defaults to black, override it here if necessary, e.g. #00000000
  // for transparent.
  gdk_rgba_parse(&background_color, "#000000");
  fl_view_set_background_color(view, &background_color);
  gtk_widget_show(GTK_WIDGET(view));
  gtk_container_add(GTK_CONTAINER(window), GTK_WIDGET(view));

  // The initial engine is a hidden daemon/tray coordinator. Dart creates all
  // user-visible windows through desktop_multi_window.
  gtk_widget_realize(GTK_WIDGET(view));

  desktop_multi_window_plugin_set_window_created_callback(
      [](FlPluginRegistry* registry) { fl_register_plugins(registry); });
  fl_register_plugins(FL_PLUGIN_REGISTRY(view));
  g_autoptr(FlStandardMethodCodec) codec = fl_standard_method_codec_new();
  self->host_channel = fl_method_channel_new(
      fl_engine_get_binary_messenger(fl_view_get_engine(view)),
      "paseo/desktop_host_events", FL_METHOD_CODEC(codec));
  fl_method_channel_set_method_call_handler(self->host_channel, host_method_call,
                                            self, nullptr);

  gtk_widget_grab_focus(GTK_WIDGET(view));
}

// GApplication forwards subsequent launches to this primary process over D-Bus.
static gint my_application_command_line(GApplication* application,
                                        GApplicationCommandLine* command_line) {
  MyApplication* self = MY_APPLICATION(application);
  const gboolean initial = self->primary_window == nullptr;
  gint argc = 0;
  g_auto(GStrv) argv =
      g_application_command_line_get_arguments(command_line, &argc);
  if (initial) g_application_activate(application);

  g_autoptr(FlValue) activation = fl_value_new_map();
  for (gint i = 1; i < argc; i++) {
    if (g_str_has_prefix(argv[i], "paseo:")) {
      // On later launches app_links' command-line hook delivers argv[1].
      if (initial || i != 1) {
        fl_value_set_string_take(activation, "url", fl_value_new_string(argv[i]));
      }
      break;
    }
    const gchar* path = argv[i];
    if (g_strcmp0(path, "--open-project") == 0 ||
        g_strcmp0(path, "--paseo-project") == 0) {
      if (++i >= argc) break;
      path = argv[i];
    } else if (path[0] == '-') {
      continue;
    }
    g_autofree gchar* absolute = g_canonicalize_filename(
        path, g_application_command_line_get_cwd(command_line));
    if (g_file_test(absolute, G_FILE_TEST_IS_DIR)) {
      fl_value_set_string_take(activation, "projectPath",
                               fl_value_new_string(absolute));
      if (initial) {
        fl_value_set_string_take(activation, "initial", fl_value_new_bool(TRUE));
      }
      break;
    }
  }
  send_activation(self, activation);
  return 0;
}

// Implements GApplication::startup.
static void my_application_startup(GApplication* application) {
  G_APPLICATION_CLASS(my_application_parent_class)->startup(application);
  gtk_window_set_default_icon_name("sh.paseo.Paseo");
  g_autoptr(GSimpleAction) quit = g_simple_action_new("quit", nullptr);
  g_signal_connect(quit, "activate", G_CALLBACK(request_quit), application);
  g_action_map_add_action(G_ACTION_MAP(application), G_ACTION(quit));
  g_autoptr(GMenu) menu = g_menu_new();
  g_menu_append(menu, "Quit Paseo Sessions", "app.quit");
  gtk_application_set_app_menu(GTK_APPLICATION(application), G_MENU_MODEL(menu));
  const gchar* quit_accels[] = {"<Primary>q", nullptr};
  gtk_application_set_accels_for_action(GTK_APPLICATION(application), "app.quit",
                                        quit_accels);
}

// Implements GObject::dispose.
static void my_application_dispose(GObject* object) {
  MyApplication* self = MY_APPLICATION(object);
  g_clear_object(&self->host_channel);
  g_clear_pointer(&self->pending_activations, fl_value_unref);
  G_OBJECT_CLASS(my_application_parent_class)->dispose(object);
}

static void my_application_class_init(MyApplicationClass* klass) {
  G_APPLICATION_CLASS(klass)->activate = my_application_activate;
  G_APPLICATION_CLASS(klass)->command_line = my_application_command_line;
  G_APPLICATION_CLASS(klass)->startup = my_application_startup;
  G_OBJECT_CLASS(klass)->dispose = my_application_dispose;
}

static void my_application_init(MyApplication* self) {
  self->pending_activations = fl_value_new_list();
}

MyApplication* my_application_new() {
  // Set the program name to the application ID, which helps various systems
  // like GTK and desktop environments map this running application to its
  // corresponding .desktop file. This ensures better integration by allowing
  // the application to be recognized beyond its binary name.
  g_set_prgname("sh.paseo.Paseo");
  g_set_application_name("Paseo Sessions");

  return MY_APPLICATION(g_object_new(my_application_get_type(),
                                     "application-id", APPLICATION_ID, "flags",
                                     G_APPLICATION_HANDLES_COMMAND_LINE, nullptr));
}
