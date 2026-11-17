pub mod udev;
pub mod winit;

use smithay::backend::renderer::gles::GlesRenderer;

use crate::state::XrossState;

impl XrossState {
    /// Run `f` with the renderer, from outside the backend's render loop (e.g. to
    /// snapshot a closing window). `None` if there's no renderer or it's busy.
    pub fn with_renderer<T>(&self, f: impl FnOnce(&mut GlesRenderer) -> T) -> Option<T> {
        if let Some(backend) = &self.winit {
            let mut backend = backend.try_borrow_mut().ok()?;
            return Some(f(backend.renderer()));
        }
        let mut udev = self.udev.as_ref()?.try_borrow_mut().ok()?;
        udev.renderer.as_mut().map(f)
    }
}
