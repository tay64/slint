// Copyright © SixtyFPS GmbH <info@slint-ui.com>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-commercial

use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

// glutin::WindowedContext tries to enforce being current or not. Since we need the WindowedContext's window() function
// in the GL renderer regardless whether we're current or not, we wrap the two states back into one type.
enum OpenGLContextState {
    #[cfg(not(target_arch = "wasm32"))]
    NotCurrent(glutin::WindowedContext<glutin::NotCurrent>),
    #[cfg(not(target_arch = "wasm32"))]
    Current(glutin::WindowedContext<glutin::PossiblyCurrent>),
    #[cfg(target_arch = "wasm32")]
    Current { window: Rc<winit::window::Window>, canvas: web_sys::HtmlCanvasElement },
}

pub struct OpenGLContext(RefCell<Option<OpenGLContextState>>);

impl OpenGLContext {
    pub fn window(&self) -> std::cell::Ref<winit::window::Window> {
        std::cell::Ref::map(self.0.borrow(), |state| match state.as_ref().unwrap() {
            #[cfg(not(target_arch = "wasm32"))]
            OpenGLContextState::NotCurrent(context) => context.window(),
            #[cfg(not(target_arch = "wasm32"))]
            OpenGLContextState::Current(context) => context.window(),
            #[cfg(target_arch = "wasm32")]
            OpenGLContextState::Current { window, .. } => window.as_ref(),
        })
    }

    #[cfg(target_arch = "wasm32")]
    pub fn html_canvas_element(&self) -> std::cell::Ref<web_sys::HtmlCanvasElement> {
        std::cell::Ref::map(self.0.borrow(), |state| match state.as_ref().unwrap() {
            OpenGLContextState::Current { canvas, .. } => canvas,
        })
    }

    #[cfg(all(
        feature = "renderer-skia",
        not(any(target_os = "macos", target_family = "windows", target_arch = "wasm32"))
    ))]
    pub fn glutin_context(
        &self,
    ) -> std::cell::Ref<glutin::WindowedContext<glutin::PossiblyCurrent>> {
        std::cell::Ref::map(self.0.borrow(), |state| match state.as_ref().unwrap() {
            OpenGLContextState::Current(gl_context) => gl_context,
            OpenGLContextState::NotCurrent(..) => {
                panic!("internal error: glutin_context() called without current context")
            }
        })
    }

    pub fn make_current(&self) {
        let mut ctx = self.0.borrow_mut();
        *ctx = Some(match ctx.take().unwrap() {
            #[cfg(not(target_arch = "wasm32"))]
            OpenGLContextState::NotCurrent(not_current_ctx) => {
                let current_ctx = unsafe { not_current_ctx.make_current().unwrap() };
                OpenGLContextState::Current(current_ctx)
            }
            state @ OpenGLContextState::Current { .. } => state,
        });
    }

    pub fn make_not_current(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut ctx = self.0.borrow_mut();
            *ctx = Some(match ctx.take().unwrap() {
                state @ OpenGLContextState::NotCurrent(_) => state,
                OpenGLContextState::Current(current_ctx_rc) => {
                    OpenGLContextState::NotCurrent(unsafe {
                        current_ctx_rc.make_not_current().unwrap()
                    })
                }
            });
        }
    }

    pub fn with_current_context<T>(&self, cb: impl FnOnce(&Self) -> T) -> T {
        if matches!(self.0.borrow().as_ref().unwrap(), OpenGLContextState::Current { .. }) {
            cb(self)
        } else {
            self.make_current();
            let result = cb(self);
            self.make_not_current();
            result
        }
    }

    pub fn swap_buffers(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        match &self.0.borrow().as_ref().unwrap() {
            OpenGLContextState::NotCurrent(_) => {}
            OpenGLContextState::Current(current_ctx) => {
                current_ctx.swap_buffers().unwrap();
            }
        }
    }

    pub fn ensure_resized(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut ctx = self.0.borrow_mut();
            *ctx = Some(match ctx.take().unwrap() {
                #[cfg(not(target_arch = "wasm32"))]
                OpenGLContextState::NotCurrent(not_current_ctx) => {
                    let current_ctx = unsafe { not_current_ctx.make_current().unwrap() };
                    current_ctx.resize(current_ctx.window().inner_size());
                    OpenGLContextState::NotCurrent(unsafe {
                        current_ctx.make_not_current().unwrap()
                    })
                }
                OpenGLContextState::Current(current) => {
                    current.resize(current.window().inner_size());
                    OpenGLContextState::Current(current)
                }
            });
        }
    }

    pub fn new_context(
        window_builder: winit::window::WindowBuilder,
        #[cfg(target_arch = "wasm32")] canvas_id: &str,
    ) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use glutin::ContextBuilder;
            let windowed_context = crate::event_loop::with_window_target(|event_loop| {
                let builder = ContextBuilder::new().with_vsync(true);
                // With latest Windows 10 and VmWare glutin's default for srgb produces surfaces that are always rendered black :(
                #[cfg(target_os = "windows")]
                let builder = builder.with_srgb(false);
                match builder.build_windowed(window_builder, event_loop.event_loop_target()) {
                    Ok(new_context) => new_context,
                    Err(creation_error) => {
                        panic!("Failed to create OpenGL context: {}", creation_error)
                    }
                }
            });
            let windowed_context = unsafe { windowed_context.make_current().unwrap() };

            #[cfg(target_os = "macos")]
            {
                use cocoa::appkit::NSView;
                use winit::platform::macos::WindowExtMacOS;
                let ns_view = windowed_context.window().ns_view();
                let view_id: cocoa::base::id = ns_view as *const _ as *mut _;
                unsafe {
                    NSView::setLayerContentsPlacement(view_id, cocoa::appkit::NSViewLayerContentsPlacement::NSViewLayerContentsPlacementTopLeft)
                }
            }

            Self(RefCell::new(Some(OpenGLContextState::Current(windowed_context))))
        }

        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::JsCast;

            let canvas = web_sys::window()
                .unwrap()
                .document()
                .unwrap()
                .get_element_by_id(canvas_id)
                .unwrap()
                .dyn_into::<web_sys::HtmlCanvasElement>()
                .unwrap();

            use winit::platform::web::WindowBuilderExtWebSys;

            let existing_canvas_size = winit::dpi::LogicalSize::new(
                canvas.client_width() as u32,
                canvas.client_height() as u32,
            );

            let window = Rc::new(crate::event_loop::with_window_target(|event_loop| {
                window_builder
                    .with_canvas(Some(canvas.clone()))
                    .build(&event_loop.event_loop_target())
                    .unwrap()
            }));

            // Try to maintain the existing size of the canvas element. A window created with winit
            // on the web will always have 1024x768 as size otherwise.

            let resize_canvas = {
                let window = window.clone();
                let canvas = canvas.clone();
                move |_: web_sys::Event| {
                    let existing_canvas_size = winit::dpi::LogicalSize::new(
                        canvas.client_width() as u32,
                        canvas.client_height() as u32,
                    );

                    window.set_inner_size(existing_canvas_size);
                    window.request_redraw();
                    crate::event_loop::with_window_target(|event_loop| {
                        event_loop
                            .event_loop_proxy()
                            .send_event(crate::event_loop::CustomEvent::RedrawAllWindows)
                            .ok();
                    })
                }
            };

            let resize_closure =
                wasm_bindgen::closure::Closure::wrap(Box::new(resize_canvas) as Box<dyn FnMut(_)>);
            web_sys::window()
                .unwrap()
                .add_event_listener_with_callback("resize", resize_closure.as_ref().unchecked_ref())
                .unwrap();
            resize_closure.forget();

            {
                let default_size = window.inner_size().to_logical(window.scale_factor());
                let new_size = winit::dpi::LogicalSize::new(
                    if existing_canvas_size.width > 0 {
                        existing_canvas_size.width
                    } else {
                        default_size.width
                    },
                    if existing_canvas_size.height > 0 {
                        existing_canvas_size.height
                    } else {
                        default_size.height
                    },
                );
                if new_size != default_size {
                    window.set_inner_size(new_size);
                }
            }

            Self(RefCell::new(Some(OpenGLContextState::Current { window, canvas })))
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn get_proc_address(&self, name: &str) -> *const std::ffi::c_void {
        match &self.0.borrow().as_ref().unwrap() {
            OpenGLContextState::NotCurrent(_) => std::ptr::null(),
            OpenGLContextState::Current(current_ctx) => current_ctx.get_proc_address(name),
        }
    }
}
