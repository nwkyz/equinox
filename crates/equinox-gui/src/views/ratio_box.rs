//! Shared aspect-ratio / fixed-size container used by the wallpaper preview,
//! gallery and history grids.
//!
//! Two modes:
//! - **Ratio mode** ([`ScreenRatioBox::new`]): height is always `width ×
//!   ratio`; measuring ignores the child, so a huge photo's natural size never
//!   balloons the layout. Used where the parent does height-for-width
//!   measuring (Clamp chains, e.g. the wallpaper preview).
//! - **Fixed mode** ([`ScreenRatioBox::with_fixed_size`]): reports a CONSTANT
//!   (w, h) for both measure orientations. Required inside GridView, which
//!   measures children unconstrained (-1) and would otherwise distort tiles
//!   as the window resizes.

mod imp {
    use std::cell::Cell;

    use glib::subclass::prelude::*;
    use gtk4::prelude::WidgetExt;
    use gtk4::subclass::prelude::*;

    #[derive(Default)]
    pub struct ScreenRatioBox {
        pub ratio: Cell<f64>,
        /// Fixed mode: when both > 0, measure reports exactly this size.
        pub fixed_w: Cell<i32>,
        pub fixed_h: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ScreenRatioBox {
        const NAME: &'static str = "EquinoxScreenRatioBox";
        type Type = super::ScreenRatioBox;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for ScreenRatioBox {}

    impl WidgetImpl for ScreenRatioBox {
        fn measure(&self, orientation: gtk4::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (fw, fh) = (self.fixed_w.get(), self.fixed_h.get());
            if fw > 0 && fh > 0 {
                // Fixed mode: constant size, independent of window/parent.
                return match orientation {
                    gtk4::Orientation::Horizontal => (fw, fw, -1, -1),
                    _ => (fh, fh, -1, -1),
                };
            }
            let ratio = self.ratio.get();
            match orientation {
                gtk4::Orientation::Vertical if ratio > 0.0 => {
                    // Unconstrained measure (for_size = -1) assumes a 502 px content width.
                    let w = if for_size >= 0 { for_size } else { 502 };
                    let h = (w as f64 * ratio).round() as i32;
                    (h, h, -1, -1) // min == natural, layout cannot compress
                }
                _ => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if let Some(child) = self.obj().first_child() {
                child.allocate(width, height, -1, None);
            }
        }
    }
}

use glib::prelude::*;
use glib::subclass::prelude::*;
use gtk4::prelude::*;

glib::wrapper! {
    pub struct ScreenRatioBox(ObjectSubclass<imp::ScreenRatioBox>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl ScreenRatioBox {
    pub fn new(ratio: f64) -> Self {
        let b: Self = glib::Object::builder().build();
        b.imp().ratio.set(ratio);
        b
    }

    /// Fixed-pixel mode: the tile always measures exactly `w × h`.
    pub fn with_fixed_size(w: i32, h: i32) -> Self {
        let b: Self = glib::Object::builder().build();
        b.imp().fixed_w.set(w);
        b.imp().fixed_h.set(h);
        b
    }

    /// Change the aspect ratio at runtime (ratio mode only).
    pub fn set_ratio(&self, ratio: f64) {
        self.imp().ratio.set(ratio);
        self.queue_resize();
    }

    /// Switch to a different fixed size at runtime (fixed mode only).
    pub fn set_fixed_size(&self, w: i32, h: i32) {
        self.imp().fixed_w.set(w);
        self.imp().fixed_h.set(h);
        self.queue_resize();
    }

    /// Unparent the child (call before dropping/removing the frame, otherwise
    /// GTK warns "finalized but still has children").
    pub fn take_child(&self) {
        if let Some(c) = self.first_child() {
            c.unparent();
        }
    }

    pub fn set_child(&self, child: Option<&impl IsA<gtk4::Widget>>) {
        if let Some(c) = self.first_child() {
            c.unparent();
        }
        if let Some(c) = child {
            c.set_parent(self);
        }
    }
}
