//! On-screen window and input, via AppKit (macOS).
//!
//! Phase 0 presents a CPU framebuffer in a native window and surfaces the input
//! events the browser process forwards into content. We bind Apple's frameworks
//! directly (the `objc2` family) rather than a cross-platform abstraction, keeping
//! this the thinnest real OS binding. Other platforms get their own backend behind
//! this same API later.
//!
//! All AppKit objects are main-thread-only, so [`Window::open`] must be called on
//! the main thread (it asserts via [`MainThreadMarker`]).

use argus_geometry::Size;
use objc2::rc::Retained;
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBitmapImageRep,
    NSDeviceRGBColorSpace, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSImage,
    NSImageScaling, NSImageView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize};

/// An input event delivered by the window, normalized for the browser process.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// A primary-button press at the given content pixel (top-left origin).
    MouseDown { x: u32, y: u32 },
    /// A scroll-wheel movement; `dy` is the vertical delta in pixels (positive =
    /// content moves up, i.e. scroll down).
    Scroll { dy: i32 },
    /// A typed character (Unicode scalar; `0x08` = backspace).
    KeyChar { ch: u32 },
    /// Navigate back (Cmd+`[`).
    Back,
    /// Navigate forward (Cmd+`]`).
    Forward,
    /// Open a new tab (Cmd+T).
    NewTab,
    /// Close the active tab (Cmd+W).
    CloseTab,
    /// Activate the next tab (Cmd+Shift+`]`).
    NextTab,
    /// Activate the previous tab (Cmd+Shift+`[`).
    PrevTab,
    /// Activate the tab at this 0-based index (Cmd+1…9; 9 = last tab).
    SwitchTab { index: usize },
    /// Reload the active tab (Cmd+R).
    Reload,
    /// The user asked to close the window.
    CloseRequested,
}

/// A native window presenting an RGBA8 framebuffer.
pub struct Window {
    app: Retained<NSApplication>,
    window: Retained<NSWindow>,
    image_view: Retained<NSImageView>,
    size: Size,
}

impl Window {
    /// Open a window of `size` (interpreted as both points and framebuffer
    /// pixels in Phase 0). Must be called on the main thread.
    pub fn open(title: &str, size: Size) -> Window {
        let mtm = MainThreadMarker::new().expect("Window::open must run on the main thread");

        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

        let content_rect = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSSize {
                width: size.width as f64,
                height: size.height as f64,
            },
        };
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;

        // SAFETY: standard NSWindow designated initializer with a valid rect.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                content_rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(&objc2_foundation::NSString::from_str(title));

        let image_view = NSImageView::new(mtm);
        // Let the image fill the view as it resizes.
        image_view.setImageScaling(NSImageScaling::ScaleAxesIndependently);
        window.setContentView(Some(&image_view));

        // AppKit startup dance on the main thread.
        app.finishLaunching();
        window.center();
        window.makeKeyAndOrderFront(None);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);

        Window {
            app,
            window,
            image_view,
            size,
        }
    }

    /// The window's framebuffer size.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Present `pixels` (RGBA8, `size.area() * 4` bytes) into the window.
    /// Set the window's title bar text (the current page's `<title>`).
    pub fn set_title(&self, title: &str) {
        self.window
            .setTitle(&objc2_foundation::NSString::from_str(title));
    }

    pub fn present(&self, pixels: &[u8], size: Size) {
        let expected = size.area() * 4;
        assert_eq!(pixels.len(), expected, "framebuffer byte length mismatch");

        // Allocate a bitmap rep (AppKit owns the buffer) and copy our pixels in.
        // SAFETY: null `planes` asks AppKit to allocate; the geometry matches the
        // copy below.
        let rep = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                size.width as isize,
                size.height as isize,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                size.width as isize * 4,
                32,
            )
        }
        .expect("NSBitmapImageRep allocation failed");

        let dst = rep.bitmapData();
        if !dst.is_null() {
            // SAFETY: AppKit allocated `size.width*4 * height` bytes for `dst`,
            // exactly `expected` bytes, which is `pixels.len()`.
            unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), dst, expected) };
        }

        let image = NSImage::new();
        image.addRepresentation(&rep);
        self.image_view.setImage(Some(&image));
    }

    /// Block until the next meaningful [`Event`], pumping AppKit in the meantime.
    pub fn next_event(&self) -> Event {
        loop {
            // SAFETY: distantFuture blocks until an event arrives.
            let event = unsafe {
                self.app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    Some(&NSDate::distantFuture()),
                    NSDefaultRunLoopMode,
                    true,
                )
            };

            if let Some(event) = event {
                let kind = event.r#type();
                // Forward the event so the window keeps behaving normally.
                self.app.sendEvent(&event);

                if kind == NSEventType::LeftMouseDown {
                    if let Some(mapped) = self.map_mouse_down(&event) {
                        return mapped;
                    }
                } else if kind == NSEventType::ScrollWheel {
                    // scrollingDeltaY: positive = content should move down (wheel up).
                    let dy = event.scrollingDeltaY();
                    if dy != 0.0 {
                        // A small multiplier makes wheel/trackpad scrolling feel right.
                        return Event::Scroll {
                            dy: (dy * 3.0) as i32,
                        };
                    }
                } else if kind == NSEventType::KeyDown {
                    if let Some(mapped) = self.map_key_down(&event) {
                        return mapped;
                    }
                }
            }

            // A closed window means the user is done.
            if !self.window.isVisible() {
                return Event::CloseRequested;
            }
        }
    }

    fn map_mouse_down(&self, event: &NSEvent) -> Option<Event> {
        let loc = event.locationInWindow();
        // locationInWindow is in points from the bottom-left; flip Y to a
        // top-left pixel origin. Points map 1:1 to pixels in Phase 0.
        let x = loc.x;
        let y = self.size.height as f64 - loc.y;
        if x < 0.0 || y < 0.0 || x >= self.size.width as f64 || y >= self.size.height as f64 {
            return None; // a click in the title bar / outside content
        }
        Some(Event::MouseDown {
            x: x as u32,
            y: y as u32,
        })
    }

    /// Map a key-down event to a typed character, or a Cmd+`[`/`]` back/forward.
    fn map_key_down(&self, event: &NSEvent) -> Option<Event> {
        let chars = event.characters()?;
        let s = chars.to_string();
        let ch = s.chars().next()? as u32;
        // Cmd+[ / Cmd+] navigate history (the macOS browser convention).
        let cmd = event
            .modifierFlags()
            .contains(NSEventModifierFlags::Command);
        if cmd {
            return match ch {
                0x5B => Some(Event::Back),     // Cmd+'['
                0x5D => Some(Event::Forward),  // Cmd+']'
                0x74 | 0x54 => Some(Event::NewTab),   // Cmd+T
                0x77 | 0x57 => Some(Event::CloseTab), // Cmd+W
                0x72 | 0x52 => Some(Event::Reload),   // Cmd+R
                0x7D => Some(Event::NextTab),  // Cmd+Shift+']' → '}'
                0x7B => Some(Event::PrevTab),  // Cmd+Shift+'[' → '{'
                // Cmd+1..9 jump to a tab (9 = the last tab, per browser convention).
                0x31..=0x39 => Some(Event::SwitchTab {
                    index: (ch - 0x31) as usize,
                }),
                _ => None,                     // other Cmd shortcuts aren't ours
            };
        }
        // macOS sends DEL (0x7F) for the backspace key; normalize to BS (0x08).
        let ch = if ch == 0x7F { 0x08 } else { ch };
        Some(Event::KeyChar { ch })
    }
}
