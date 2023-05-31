use adw::prelude::*;
use atspi::accessible::AccessibleProxy;

mod overview;

struct Root {
    name: String,
    proxy: AccessibleProxy<'static>,
}

fn build_ui(app: &adw::Application) {
    if !app.windows().is_empty() {
        return;
    }
    let css = gtk::CssProvider::new();
    css.load_from_data("spioverview { padding: 4px; }");
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().unwrap(),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let win = adw::ApplicationWindow::new(app);
    win.set_default_size(600, 400);
    win.set_title(Some("SPInspector"));
    let spinner = gtk::Spinner::builder().spinning(true).build();
    win.set_content(Some(&spinner));
    win.present();
    glib::MainContext::default().spawn_local(async move {
        let bus = match atspi::AccessibilityConnection::open().await {
            Ok(bus) => bus,
            Err(e) => {
                let status = adw::StatusPage::builder()
                    .icon_name("dialog-warning-symbolic")
                    .title("Failed to connect to accessibility bus")
                    .description(&e.to_string())
                    .build();
                win.set_content(Some(&status));
                return;
            }
        };
        let overlay = adw::ToastOverlay::new();
        let leaflet = adw::Leaflet::new();
        let model = gio::ListStore::new(glib::BoxedAnyObject::static_type());
        let select = gtk::SingleSelection::new(Some(model.clone()));
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_bind(|_, obj| {
            let item = obj.downcast_ref::<gtk::ListItem>().unwrap();
            let obj = item
                .item()
                .unwrap()
                .downcast::<glib::BoxedAnyObject>()
                .unwrap();
            let root = obj.borrow::<Root>();
            item.set_child(Some(
                &gtk::Label::builder()
                    .label(&root.name)
                    .xalign(0.)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .build(),
            ));
            item.set_activatable(false);
        });
        let list = gtk::ListView::new(Some(select.clone()), Some(factory));
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .width_request(150)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .build();
        let header = adw::HeaderBar::new();
        let reload_button = gtk::Button::from_icon_name("view-refresh-symbolic");
        {
            let bus = bus.connection().clone();
            let model = model.clone();
            let overlay = overlay.clone();
            reload_button.connect_clicked(move |_| {
                spawn_fut(&overlay, reload(bus.clone(), model.clone()));
            });
        }
        header.pack_end(&reload_button);
        vbox.append(&header);
        vbox.append(&scroll);
        let scroll_page = leaflet.append(&vbox);
        scroll_page.set_name(Some("list"));

        header.set_title_widget(Some(&gtk::Label::new(Some("Windows"))));
        leaflet
            .bind_property("folded", &header, "show-end-title-buttons")
            .sync_create()
            .build();

        let vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .vexpand(true)
            .build();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&gtk::Label::new(Some("SPInspector"))));
        let back_button = gtk::Button::from_icon_name("go-previous-symbolic");
        header.pack_start(&back_button);
        leaflet
            .bind_property("folded", &back_button, "visible")
            .sync_create()
            .build();
        leaflet
            .bind_property("folded", &header, "show-start-title-buttons")
            .sync_create()
            .build();

        {
            let leaflet = leaflet.clone();
            back_button.connect_clicked(move |_| {
                leaflet.set_visible_child_name("list");
            });
        }

        vbox.append(&header);
        let overview = overview::Overview::new();
        overview.set_hexpand(true);
        overview.set_vexpand(true);
        vbox.append(&overview);
        let main_page = leaflet.append(&vbox);
        main_page.set_name(Some("overview"));

        overlay.set_child(Some(&leaflet));

        select.connect_selected_item_notify(move |model| {
            let label = header
                .title_widget()
                .unwrap()
                .downcast::<gtk::Label>()
                .unwrap();
            if let Some(obj) = model.selected_item() {
                let obj = obj.downcast::<glib::BoxedAnyObject>().unwrap();
                let root = obj.borrow::<Root>();
                label.set_label(&root.name);
                leaflet.set_visible_child_name("overview");
                overview.set_accessible(root.proxy.clone());
            } else {
                label.set_label("SPInspector");
                leaflet.set_visible_child_name("list");
                overview.clear();
            }
        });

        spawn_fut(&overlay, reload(bus.connection().clone(), model));
        win.set_content(Some(&overlay));
    });
}

async fn reload(bus: atspi::zbus::Connection, model: gio::ListStore) -> anyhow::Result<()> {
    model.remove_all();
    let registry = atspi::registry::RegistryProxy::new(&bus).await?;
    let acc = atspi::accessible::AccessibleProxy::builder(&bus)
        .destination(registry.destination())?
        .path("/org/a11y/atspi/accessible/root")?
        .build()
        .await?;
    for (dest, path) in acc.get_children().await? {
        let app = atspi::accessible::AccessibleProxy::builder(&bus)
            .destination(dest)?
            .path(path)?
            .build()
            .await?;
        let app_name = app.name().await?;
        for (dest, path) in app.get_children().await? {
            let proxy = atspi::accessible::AccessibleProxy::builder(&bus)
                .destination(dest)?
                .path(path)?
                .build()
                .await?;
            let name = proxy.name().await?;
            let name = if name.is_empty() {
                app_name.clone()
            } else {
                name
            };
            model.append(&glib::BoxedAnyObject::new(Root { name, proxy }));
        }
    }
    Ok(())
}

#[inline]
fn spawn_fut(
    widget: &impl glib::IsA<gtk::Widget>,
    fut: impl std::future::Future<Output = anyhow::Result<()>> + 'static,
) -> glib::JoinHandle<()> {
    let overlay = widget
        .ancestor(adw::ToastOverlay::static_type())
        .unwrap()
        .downcast::<adw::ToastOverlay>()
        .unwrap();
    glib::MainContext::default().spawn_local(async move {
        if let Err(err) = fut.await {
            let toast = adw::Toast::new(&err.to_string());
            overlay.add_toast(toast);
        }
    })
}

fn main() -> glib::ExitCode {
    static LOGGER: glib::GlibLogger = glib::GlibLogger::new(
        glib::GlibLoggerFormat::Plain,
        glib::GlibLoggerDomain::CrateTarget,
    );

    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Debug);

    {
        #[cfg(target_os = "macos")]
        let library = unsafe { libloading::os::unix::Library::new("libepoxy.0.dylib") }.unwrap();
        #[cfg(all(unix, not(target_os = "macos")))]
        let library = unsafe { libloading::os::unix::Library::new("libepoxy.so.0") }.unwrap();
        #[cfg(windows)]
        let library = libloading::os::windows::Library::open_already_loaded("libepoxy-0.dll")
            .or_else(|_| libloading::os::windows::Library::open_already_loaded("epoxy-0.dll"))
            .unwrap();

        epoxy::load_with(|name| {
            unsafe { library.get::<_>(name.as_bytes()) }
                .map(|symbol| *symbol)
                .unwrap_or(std::ptr::null())
        });
    }

    let app = adw::Application::new(
        Some("com.github.jf2048.SPInspector"),
        gio::ApplicationFlags::FLAGS_NONE,
    );
    app.connect_activate(build_ui);
    app.run()
}
