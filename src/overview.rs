use atspi::accessible::AccessibleProxy;
use glow::HasContext;
use gtk::{gdk, prelude::*, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell, RefMut},
    num::NonZeroU32,
    rc::Rc,
};

glib::wrapper! {
    pub struct Overview(ObjectSubclass<OverviewImp>)
        @extends gtk::Widget, gtk::GLArea,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Overview {
    pub fn new() -> Self {
        glib::Object::new()
    }
    pub fn set_accessible(&self, proxy: AccessibleProxy<'static>) {
        self.clear();
        let overview = self.clone();
        let handle = super::spawn_fut(self, async move {
            let node = Node::new(proxy.clone()).await?;
            overview.imp().node.replace(Some(node));
            overview.queue_draw();
            for (dest, path) in proxy.get_children().await? {
                let proxy = atspi::accessible::AccessibleProxy::builder(proxy.connection())
                    .destination(dest)?
                    .path(path)?
                    .build()
                    .await?;
                let child = Rc::new(RefCell::new(Node::new(proxy.clone()).await?));
                overview
                    .imp()
                    .node
                    .borrow_mut()
                    .as_mut()
                    .unwrap()
                    .children
                    .push(child.clone());
                Node::fill_children(child, proxy).await?;
            }
            overview.imp().handle.replace(None);
            Ok(())
        });
        self.imp().handle.replace(Some(handle));
    }
    pub fn clear(&self) {
        if let Some(handle) = self.imp().handle.take() {
            handle.abort();
        }
        self.imp().node.replace(None);
        self.queue_draw();
    }
}

#[derive(Debug)]
struct Node {
    extents: gdk::Rectangle,
    name: String,
    role: atspi::accessible::Role,
    children: Vec<Rc<RefCell<Node>>>,
}

impl Node {
    async fn new(proxy: AccessibleProxy<'static>) -> anyhow::Result<Self> {
        let ifaces = proxy.get_interfaces().await?;
        let role = proxy.get_role().await?;
        let name = proxy.name().await?;
        let extents = if ifaces.contains(atspi::Interface::Component) {
            let component = atspi::component::ComponentProxy::builder(proxy.connection())
                .destination(proxy.destination())?
                .path(proxy.path())?
                .build()
                .await?;
            let (x, y, w, h) = component.get_extents(atspi::CoordType::Window).await?;
            gdk::Rectangle::new(x, y, w, h)
        } else {
            gdk::Rectangle::new(0, 0, 0, 0)
        };
        Ok(Self {
            extents,
            name,
            role,
            children: Vec::new(),
        })
    }
    #[async_recursion::async_recursion(?Send)]
    async fn fill_children(
        node: Rc<RefCell<Node>>,
        proxy: AccessibleProxy<'static>,
    ) -> anyhow::Result<()> {
        for (dest, path) in proxy.get_children().await? {
            let proxy = atspi::accessible::AccessibleProxy::builder(proxy.connection())
                .destination(dest)?
                .path(path)?
                .build()
                .await?;
            let child = Rc::new(RefCell::new(Node::new(proxy.clone()).await?));
            node.borrow_mut().children.push(child.clone());
            Self::fill_children(child, proxy).await?;
        }
        Ok(())
    }
    fn pick(&self, x: i32, y: i32, func: impl FnOnce(&Node)) {
        for child in &self.children {
            let child = child.borrow();
            if child.extents.contains_point(x, y) {
                child.pick(x, y, func);
                return;
            }
        }
        func(self);
    }
}

type Canvas = femtovg::Canvas<femtovg::renderer::OpenGl>;

#[derive(Default)]
pub struct OverviewImp {
    canvas: RefCell<Option<Canvas>>,
    handle: RefCell<Option<glib::JoinHandle<()>>>,
    node: RefCell<Option<Node>>,
    popover: Popover,
    picked: Cell<Option<gdk::Rectangle>>,
    hovered: Cell<Option<gdk::Rectangle>>,
}

struct Popover {
    popover: gtk::Popover,
    name: gtk::Label,
    role: gtk::Label,
}

impl Default for Popover {
    fn default() -> Self {
        let popover = gtk::Popover::new();
        let grid = gtk::Grid::builder()
            .row_spacing(8)
            .column_spacing(8)
            .build();
        let name = gtk::Label::builder().xalign(0.).selectable(true).build();
        let role = gtk::Label::builder().xalign(0.).selectable(true).build();
        grid.attach(
            &gtk::Label::builder().label("Name").xalign(1.).build(),
            0,
            0,
            1,
            1,
        );
        grid.attach(&name, 1, 0, 1, 1);
        grid.attach(
            &gtk::Label::builder().label("Role").xalign(1.).build(),
            0,
            1,
            1,
            1,
        );
        grid.attach(&role, 1, 1, 1, 1);
        popover.set_child(Some(&grid));
        Self {
            popover,
            name,
            role,
        }
    }
}

impl OverviewImp {
    fn ensure_canvas(&self) -> RefMut<Canvas> {
        let mut canvas = self.canvas.borrow_mut();
        if canvas.is_some() {
            return RefMut::map(canvas, |c| c.as_mut().unwrap());
        }
        self.obj().attach_buffers();

        static LOAD_FN: fn(&str) -> *const std::ffi::c_void =
            |s| epoxy::get_proc_addr(s) as *const _;
        let (mut renderer, fbo) = unsafe {
            let renderer = femtovg::renderer::OpenGl::new_from_function(LOAD_FN)
                .expect("Cannot create renderer");
            let ctx = glow::Context::from_loader_function(LOAD_FN);
            let id = NonZeroU32::new(ctx.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING) as u32)
                .expect("No GTK provided framebuffer binding");
            ctx.bind_framebuffer(glow::FRAMEBUFFER, None);
            (renderer, glow::NativeFramebuffer(id))
        };
        renderer.set_screen_target(Some(fbo));
        canvas.replace(femtovg::Canvas::new(renderer).expect("Cannot create canvas"));
        RefMut::map(canvas, |c| c.as_mut().unwrap())
    }
    fn draw(&self, node: &Node, canvas: &mut Canvas, fg: &femtovg::Paint) {
        for child in &node.children {
            self.draw(&*child.borrow(), canvas, fg);
        }
        let r = &node.extents;
        let mut path = femtovg::Path::new();
        path.rect(
            r.x() as f32,
            r.y() as f32,
            r.width() as f32,
            r.height() as f32,
        );
        canvas.stroke_path(&path, fg);
    }
    fn pick(&self, x: i32, y: i32, func: impl FnOnce(&Node)) -> bool {
        let node = self.node.borrow();
        if let Some(node) = node.as_ref() {
            if node.extents.contains_point(x, y) {
                node.pick(x, y, func);
                return true;
            }
        }
        false
    }
    fn scale(&self) -> f32 {
        let node = self.node.borrow();
        if let Some(node) = node.as_ref() {
            let overview = self.obj();
            let w = overview.width();
            let h = overview.height();
            let xscale = w as f32 / node.extents.width() as f32;
            let yscale = h as f32 / node.extents.height() as f32;
            xscale.min(yscale).min(1.)
        } else {
            1.
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for OverviewImp {
    const NAME: &'static str = "SpiOverview";
    type Type = Overview;
    type ParentType = gtk::GLArea;
    fn class_init(klass: &mut Self::Class) {
        klass.set_css_name("spioverview");
        klass.set_accessible_role(gtk::AccessibleRole::Presentation);
    }
}

impl ObjectImpl for OverviewImp {
    fn constructed(&self) {
        self.parent_constructed();
        self.popover.popover.set_parent(&*self.obj());
        let click = gtk::GestureClick::new();
        click.connect_pressed(|ctrl, _, x, y| {
            let overview = ctrl.widget().downcast::<Overview>().unwrap();
            let popover = &overview.imp().popover;
            let set_labels = |node: &Node| {
                popover.name.set_text(&node.name);
                popover.role.set_text(node.role.name());
                overview.imp().picked.set(Some(node.extents));
            };
            let scale = overview.imp().scale() as f64;
            let sx = x / scale;
            let sy = y / scale;
            if overview.imp().pick(sx as i32, sy as i32, set_labels) {
                popover
                    .popover
                    .set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                popover.popover.popup();
            } else {
                overview.imp().picked.set(None);
            }
            overview.queue_render();
        });
        self.popover.popover.connect_closed(|popover| {
            let overview = popover.parent().unwrap().downcast::<Overview>().unwrap();
            overview.imp().picked.set(None);
            overview.queue_render();
        });
        self.obj().add_controller(click);
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(|ctrl, x, y| {
            let overview = ctrl.widget().downcast::<Overview>().unwrap();
            let scale = overview.imp().scale() as f64;
            let sx = x / scale;
            let sy = y / scale;
            let mut hover_changed = false;
            let set_hover = |node: &Node| {
                let ext = Some(node.extents);
                let old = overview.imp().hovered.replace(ext);
                hover_changed = old != ext;
            };
            if !overview.imp().pick(sx as i32, sy as i32, set_hover) {
                hover_changed = overview.imp().hovered.replace(None).is_some();
            }
            if hover_changed {
                overview.queue_render();
            }
        });
        motion.connect_leave(|ctrl| {
            let overview = ctrl.widget().downcast::<Overview>().unwrap();
            if overview.imp().hovered.replace(None).is_some() {
                overview.queue_render();
            }
        });
        self.obj().add_controller(motion);
    }
}
impl WidgetImpl for OverviewImp {}
impl GLAreaImpl for OverviewImp {
    fn resize(&self, width: i32, height: i32) {
        let mut canvas = self.ensure_canvas();
        canvas.set_size(
            width as u32,
            height as u32,
            self.obj().scale_factor() as f32,
        );
    }
    fn render(&self, _context: &gdk::GLContext) -> bool {
        let overview = self.obj();
        let w = overview.width();
        let h = overview.height();
        let scale = self.scale();
        let mut canvas = self.ensure_canvas();
        canvas.reset_transform();
        canvas.clear_rect(0, 0, w as u32, h as u32, femtovg::Color::rgba(0, 0, 0, 0));

        let node = self.node.borrow();
        if let Some(node) = node.as_ref() {
            let fg = overview.color();
            let mut fg = femtovg::Paint::color(femtovg::Color::rgbaf(
                fg.red(),
                fg.green(),
                fg.blue(),
                fg.alpha(),
            ));
            fg.set_line_width(1.);
            canvas.scale(scale, scale);
            self.draw(node, &mut *canvas, &fg);
            if let Some(r) = self.picked.get() {
                let sel = femtovg::Paint::color(femtovg::Color::rgbaf(1., 0., 0., 0.5));
                let mut path = femtovg::Path::new();
                path.rect(
                    r.x() as f32,
                    r.y() as f32,
                    r.width() as f32,
                    r.height() as f32,
                );
                canvas.fill_path(&path, &sel);
            } else if let Some(r) = self.hovered.get() {
                let sel = femtovg::Paint::color(femtovg::Color::rgbaf(0., 0., 1., 0.5));
                let mut path = femtovg::Path::new();
                path.rect(
                    r.x() as f32,
                    r.y() as f32,
                    r.width() as f32,
                    r.height() as f32,
                );
                canvas.fill_path(&path, &sel);
            }
        }

        canvas.flush();

        if self.handle.borrow().is_some() {
            let overview = overview.clone();
            glib::idle_add_local_once(move || overview.queue_render());
        }
        true
    }
}
