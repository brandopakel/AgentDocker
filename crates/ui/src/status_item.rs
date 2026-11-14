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
//! click does is the app's to decide: today it raises the window; a panel
//! attached to the item would hang off the same message, anchored to the
//! item's button, without this module changing shape.

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
    use objc2_app_kit::{NSCellImagePosition, NSImage, NSStatusBar, NSStatusItem};
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

    #[test]
    fn a_click_is_taken_once() {
        super::CLICKED.store(true, std::sync::atomic::Ordering::Release);
        assert!(super::take_click());
        assert!(!super::take_click(), "a click is handed over once");
    }
}
