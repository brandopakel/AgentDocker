//! The menu-bar item: the mark, and beside it the number the dock badge
//! shows, nothing at zero.
//!
//! The dock badge reaches the person while the window is behind another;
//! the menu bar reaches them while the window is closed. The item is the
//! app's one presence then: with it on, closing the window keeps the app
//! running and a click on the item brings the window back. The count is
//! text beside the mark, never a colour alone.
//!
//! Only macOS has a status bar here. Elsewhere every function is a
//! no-op, so the app's update is the same on every platform.
//!
//! A click arrives in the app as a message (`Message::StatusItemClicked`,
//! through the wake that every other native callback uses), so what the
//! click does is the app's to decide: it opens the panel under the item,
//! placed from [`anchor`], the item's button frame in the window
//! coordinates Iced takes.

/// The item's tooltip: the app's name, and the count in words when there
/// is one, so the number beside the mark is explained on hover and read
/// by assistive technology.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn tooltip(count: usize) -> String {
    match crate::dock::label(count) {
        None => "AgentDocker".to_owned(),
        Some(label) => format!("AgentDocker · {label} waiting on you"),
    }
}

/// Whether this platform draws a menu-bar item at all.
pub(crate) const fn offered() -> bool {
    cfg!(target_os = "macos")
}

/// Show the item with `count` beside the mark, or take it down with
/// `None`. AppKit's status bar belongs to the main thread, where the
/// app's update runs; called anywhere else it does nothing rather than
/// touch it from the wrong thread.
#[cfg(target_os = "macos")]
pub(crate) fn sync(shown: Option<usize>) {
    native::sync(shown);
}

/// Other platforms have no menu-bar item here.
#[cfg(not(target_os = "macos"))]
pub(crate) fn sync(_shown: Option<usize>) {}

/// Where the item's button stands, for a panel to hang under it: in
/// logical points with the origin at the top left of the main display,
/// as Iced positions a window.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct Anchor {
    /// The horizontal middle of the item's button.
    pub middle: f32,
    /// The bottom edge of the menu bar: where a panel's top goes.
    pub bottom: f32,
    /// The left and right edges of the screen the item is on; a panel
    /// stays between them.
    pub screen: (f32, f32),
}

impl Anchor {
    /// The top-left corner of a panel `width` wide centred under the
    /// item, kept inside the screen's edges.
    pub(crate) fn panel_origin(self, width: f32) -> iced::Point {
        let (left, right) = self.screen;
        let x = (self.middle - width / 2.0)
            .min(right - width)
            .max(left);
        iced::Point::new(x, self.bottom)
    }
}

/// The item's anchor while it is shown, on the main thread; `None` with
/// no item, elsewhere than macOS, or off the main thread.
#[cfg(target_os = "macos")]
pub(crate) fn anchor() -> Option<Anchor> {
    native::anchor()
}

/// Other platforms have no menu-bar item here.
#[cfg(not(target_os = "macos"))]
pub(crate) fn anchor() -> Option<Anchor> {
    None
}

/// Whether the item was clicked since the last call. The click sets a
/// flag and wakes the app, whose next tick asks here and turns it into a
/// message; polling keeps the native callback free of app state.
pub(crate) fn take_click() -> bool {
    CLICKED.swap(false, std::sync::atomic::Ordering::AcqRel)
}

static CLICKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "macos")]
mod native {
    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{AnyThread, ClassType, MainThreadMarker, define_class, msg_send, sel};
    use objc2_app_kit::{NSCellImagePosition, NSImage, NSScreen, NSStatusBar, NSStatusItem};
    use objc2_foundation::{NSData, NSObject, NSObjectProtocol, NSSize, NSString};

    define_class!(
        // SAFETY: NSObject has no additional subclass requirements. The
        // action only sets an atomic flag and wakes the app; no UI state
        // is touched from the callback.
        #[unsafe(super = NSObject)]
        pub struct Target;

        unsafe impl NSObjectProtocol for Target {}

        impl Target {
            #[unsafe(method(clicked:))]
            fn clicked(&self, _sender: Option<&AnyObject>) {
                super::CLICKED.store(true, std::sync::atomic::Ordering::Release);
                crate::wake::Wake::default().request_repaint();
            }
        }
    );

    /// The item and its action target, held for as long as it is shown:
    /// AppKit keeps only weak references to both.
    struct Shown {
        item: Retained<NSStatusItem>,
        _target: Retained<Target>,
    }

    thread_local! {
        static SHOWN: RefCell<Option<Shown>> = const { RefCell::new(None) };
    }

    /// The mark as the status button's image: the same PNG the window
    /// uses, told its point size so the menu bar draws it at a glance's
    /// height on any display.
    fn mark() -> Option<Retained<NSImage>> {
        let data = NSData::with_bytes(include_bytes!("mark.png"));
        let image = NSImage::initWithData(NSImage::alloc(), &data)?;
        image.setSize(NSSize::new(18.0, 18.0));
        Some(image)
    }

    /// The item's button frame, turned from AppKit's bottom-left screen
    /// coordinates into the top-left ones a window is placed by: the
    /// main display (the first screen, the one with the menu bar) is the
    /// reference, as it is for the window system's own flip.
    pub(super) fn anchor() -> Option<super::Anchor> {
        let main = MainThreadMarker::new()?;
        SHOWN.with(|slot| {
            let slot = slot.borrow();
            let window = slot.as_ref()?.item.button(main)?.window()?;
            let frame = window.frame();
            let height = NSScreen::screens(main).firstObject()?.frame().size.height;
            let screen = window
                .screen()
                .map(|screen| screen.frame())
                .unwrap_or(frame);
            Some(super::Anchor {
                middle: (frame.origin.x + frame.size.width / 2.0) as f32,
                bottom: (height - frame.origin.y) as f32,
                screen: (
                    screen.origin.x as f32,
                    (screen.origin.x + screen.size.width) as f32,
                ),
            })
        })
    }

    pub(super) fn sync(shown: Option<usize>) {
        let Some(main) = MainThreadMarker::new() else {
            return;
        };
        SHOWN.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(count) = shown else {
                if let Some(gone) = slot.take() {
                    NSStatusBar::systemStatusBar().removeStatusItem(&gone.item);
                }
                return;
            };
            if slot.is_none() {
                let item = NSStatusBar::systemStatusBar()
                    .statusItemWithLength(objc2_app_kit::NSVariableStatusItemLength);
                // SAFETY: NSObject initialization is valid for this
                // stateless subclass.
                let target: Retained<Target> = unsafe { msg_send![Target::class(), new] };
                if let Some(button) = item.button(main) {
                    button.setImage(mark().as_deref());
                    button.setImagePosition(NSCellImagePosition::ImageLeading);
                    let target_object: &AnyObject = &target;
                    // SAFETY: the target outlives the button (both are held
                    // in `Shown` and released together) and `clicked:` is
                    // the selector defined on it above.
                    unsafe {
                        button.setTarget(Some(target_object));
                        button.setAction(Some(sel!(clicked:)));
                    }
                }
                *slot = Some(Shown {
                    item,
                    _target: target,
                });
            }
            if let Some(shown) = slot.as_ref()
                && let Some(button) = shown.item.button(main)
            {
                let title = crate::dock::label(count).unwrap_or_default();
                button.setTitle(&NSString::from_str(&title));
                button.setToolTip(Some(&NSString::from_str(&super::tooltip(count))));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_tooltip_names_the_app_and_says_the_count_in_words() {
        assert_eq!(super::tooltip(0), "AgentDocker");
        assert_eq!(super::tooltip(1), "AgentDocker · 1 waiting on you");
        assert_eq!(super::tooltip(3), "AgentDocker · 3 waiting on you");
        assert_eq!(super::tooltip(250), "AgentDocker · 99+ waiting on you");
    }

    /// A panel is centred under the item and never off the screen: at
    /// the right edge it slides left to fit, at the left edge right.
    #[test]
    fn a_panel_hangs_centred_under_the_item_and_inside_the_screen() {
        let anchor = |middle: f32| super::Anchor {
            middle,
            bottom: 25.0,
            screen: (0.0, 1440.0),
        };
        assert_eq!(
            anchor(700.0).panel_origin(448.0),
            iced::Point::new(476.0, 25.0)
        );
        assert_eq!(
            anchor(1400.0).panel_origin(448.0),
            iced::Point::new(992.0, 25.0),
            "slid left to stay on the screen"
        );
        assert_eq!(anchor(40.0).panel_origin(448.0), iced::Point::new(0.0, 25.0));
        assert_eq!(super::anchor(), None, "no item is shown in a test");
    }

    #[test]
    fn a_click_is_taken_once() {
        super::CLICKED.store(true, std::sync::atomic::Ordering::Release);
        assert!(super::take_click());
        assert!(!super::take_click(), "a click is handed over once");
    }
}
