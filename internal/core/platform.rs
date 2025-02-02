// Copyright © SixtyFPS GmbH <info@slint-ui.com>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-commercial

/*!
The backend is the abstraction for crates that need to do the actual drawing and event loop
*/

#![warn(missing_docs)]

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;

#[cfg(all(not(feature = "std"), feature = "unsafe-single-threaded"))]
use crate::unsafe_single_threaded::{thread_local, OnceCell};
#[cfg(feature = "std")]
use once_cell::sync::OnceCell;

pub use crate::items::{InputType, MouseCursor};
pub use crate::lengths::{PhysicalLength, PhysicalPoint};
pub use crate::renderer::Renderer;
#[cfg(feature = "swrenderer")]
pub use crate::swrenderer;
pub use crate::window::PlatformWindow;

#[derive(Copy, Clone)]
/// Behavior describing how the event loop should terminate.
pub enum EventLoopQuitBehavior {
    /// Terminate the event loop when the last window was closed.
    QuitOnLastWindowClosed,
    /// Keep the event loop running until [`Backend::quit_event_loop()`] is called.
    QuitOnlyExplicitly,
}

/// Interface implemented by back-ends
pub trait PlatformAbstraction {
    /// Instantiate a window for a component.
    fn create_window(&self) -> Rc<dyn PlatformWindow>;

    /// Spins an event loop and renders the visible windows.
    fn run_event_loop(&self, _behavior: EventLoopQuitBehavior) {
        unimplemented!("The backend does not implement running an eventloop")
    }

    /// Return an [`EventLoopProxy`] that can be used to send event to the event loop
    ///
    /// If this function returns `None` (the default implementation), then it will
    /// not be possible to send event to the event loop and the function
    /// [`slint::invoke_from_event_loop()`](crate::api::invoke_from_event_loop) and
    /// [`slint::quit_event_loop()`](crate::api::quit_event_loop) will panic
    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        None
    }

    /// Returns the current time as a monotonic duration since the start of the program
    ///
    /// This is used by the animations and timer to compute the elapsed time.
    ///
    /// When the `std` feature is enabled, this function is implemented in terms of
    /// [`std::time::Instant::now()`], but on `#![no_std]` platform, this funciton must
    /// be implemented.
    fn duration_since_start(&self) -> core::time::Duration {
        #[cfg(feature = "std")]
        {
            let the_beginning = *INITIAL_INSTANT.get_or_init(instant::Instant::now);
            instant::Instant::now() - the_beginning
        }
        #[cfg(not(feature = "std"))]
        unimplemented!("The platform abstraction must implement `duration_since_start`")
    }

    /// Sends the given text into the system clipboard
    fn set_clipboard_text(&self, _text: &str) {}
    /// Returns a copy of text stored in the system clipboard, if any.
    fn clipboard_text(&self) -> Option<String> {
        None
    }
}

/// Trait that is returned by the [`PlatformAbstraction::new_event_loop_proxy`]
///
/// This are the implementation details for the function that may need to
/// communicate with the eventloop from different thread
pub trait EventLoopProxy: Send + Sync {
    /// Exits the event loop.
    ///
    /// This is what is called by [`slint::quit_event_loop()`](crate::api::quit_event_loop)
    fn quit_event_loop(&self);

    /// Invoke the function from the event loop.
    ///
    /// This is what is called by [`slint::invoke_from_event_loop()`](crate::api::invoke_from_event_loop)
    fn invoke_from_event_loop(&self, event: Box<dyn FnOnce() + Send>);
}

#[cfg(feature = "std")]
static INITIAL_INSTANT: once_cell::sync::OnceCell<instant::Instant> =
    once_cell::sync::OnceCell::new();

#[cfg(feature = "std")]
impl std::convert::From<crate::animations::Instant> for instant::Instant {
    fn from(our_instant: crate::animations::Instant) -> Self {
        let the_beginning = *INITIAL_INSTANT.get_or_init(instant::Instant::now);
        the_beginning + core::time::Duration::from_millis(our_instant.0)
    }
}

thread_local! {
    pub(crate) static PLAFTORM_ABSTRACTION_INSTANCE : once_cell::unsync::OnceCell<Box<dyn PlatformAbstraction>>
        = once_cell::unsync::OnceCell::new()
}
static EVENTLOOP_PROXY: OnceCell<Box<dyn EventLoopProxy + 'static>> = OnceCell::new();

pub(crate) fn event_loop_proxy() -> Option<&'static dyn EventLoopProxy> {
    EVENTLOOP_PROXY.get().map(core::ops::Deref::deref)
}

/// Set the slint platform abstraction.
///
/// If the platform abastraction was already set this will return `Err`
pub fn set_platform_abstraction(
    platform: Box<dyn PlatformAbstraction + 'static>,
) -> Result<(), ()> {
    PLAFTORM_ABSTRACTION_INSTANCE.with(|instance| {
        if instance.get().is_some() {
            return Err(());
        }
        if let Some(proxy) = platform.new_event_loop_proxy() {
            EVENTLOOP_PROXY.set(proxy).map_err(drop)?
        }
        instance.set(platform.into()).map_err(drop).unwrap();
        Ok(())
    })
}

/// Fire timer events and update animations
///
/// This function should be called before rendering or processing input event.
/// It should basically be called on every iteration of the event loop.
pub fn update_timers_and_animations() {
    crate::timers::TimerList::maybe_activate_timers();
    crate::animations::update_animations();
}

/// Return the duration before the next timer should be activated. This is basically the
/// maximum time before calling [`upate_timers_and_animation()`].
///
/// That is typically called by the implementation of the event loop to know how long the
/// thread can go to sleep before the next event.
///
/// Note: this does not include animations. [`Window::has_active_animation()`](crate::api::Window::has_active_animation())
/// can be called to know if a window has running animation
pub fn duration_until_next_timer_update() -> Option<core::time::Duration> {
    crate::timers::TimerList::next_timeout().map(|timeout| {
        let duration_since_start = crate::platform::PLAFTORM_ABSTRACTION_INSTANCE
            .with(|p| p.get().map(|p| p.duration_since_start()))
            .unwrap_or_default();
        core::time::Duration::from_millis(
            timeout.0.saturating_sub(duration_since_start.as_millis() as u64),
        )
    })
}
