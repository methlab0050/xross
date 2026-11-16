//! Pointer grabs for interactive move and resize. Both work for Wayland and X11 windows.

/// Forwards the pointer-grab methods we don't customize to the inner handle.
macro_rules! forward_pointer_grab {
    () => {
        fn relative_motion(
            &mut self,
            data: &mut XrossState,
            handle: &mut PointerInnerHandle<'_, XrossState>,
            focus: Option<(WlSurface, Point<f64, Logical>)>,
            event: &smithay::input::pointer::RelativeMotionEvent,
        ) {
            handle.relative_motion(data, focus, event);
        }
        fn axis(&mut self, data: &mut XrossState, handle: &mut PointerInnerHandle<'_, XrossState>, details: smithay::input::pointer::AxisFrame) {
            handle.axis(data, details)
        }
        fn frame(&mut self, data: &mut XrossState, handle: &mut PointerInnerHandle<'_, XrossState>) {
            handle.frame(data);
        }
        fn gesture_swipe_begin(&mut self, data: &mut XrossState, handle: &mut PointerInnerHandle<'_, XrossState>, event: &smithay::input::pointer::GestureSwipeBeginEvent) {
            handle.gesture_swipe_begin(data, event)
        }
        fn gesture_swipe_update(&mut self, data: &mut XrossState, handle: &mut PointerInnerHandle<'_, XrossState>, event: &smithay::input::pointer::GestureSwipeUpdateEvent) {
            handle.gesture_swipe_update(data, event)
        }
        fn gesture_swipe_end(&mut self, data: &mut XrossState, handle: &mut PointerInnerHandle<'_, XrossState>, event: &smithay::input::pointer::GestureSwipeEndEvent) {
            handle.gesture_swipe_end(data, event)
        }
        fn gesture_pinch_begin(&mut self, data: &mut XrossState, handle: &mut PointerInnerHandle<'_, XrossState>, event: &smithay::input::pointer::GesturePinchBeginEvent) {
            handle.gesture_pinch_begin(data, event)
        }
        fn gesture_pinch_update(&mut self, data: &mut XrossState, handle: &mut PointerInnerHandle<'_, XrossState>, event: &smithay::input::pointer::GesturePinchUpdateEvent) {
            handle.gesture_pinch_update(data, event)
        }
        fn gesture_pinch_end(&mut self, data: &mut XrossState, handle: &mut PointerInnerHandle<'_, XrossState>, event: &smithay::input::pointer::GesturePinchEndEvent) {
            handle.gesture_pinch_end(data, event)
        }
        fn gesture_hold_begin(&mut self, data: &mut XrossState, handle: &mut PointerInnerHandle<'_, XrossState>, event: &smithay::input::pointer::GestureHoldBeginEvent) {
            handle.gesture_hold_begin(data, event)
        }
        fn gesture_hold_end(&mut self, data: &mut XrossState, handle: &mut PointerInnerHandle<'_, XrossState>, event: &smithay::input::pointer::GestureHoldEndEvent) {
            handle.gesture_hold_end(data, event)
        }
        fn start_data(&self) -> &PointerGrabStartData<XrossState> {
            &self.start_data
        }
    };
}

pub mod move_grab;
pub mod resize_grab;

pub use move_grab::MoveSurfaceGrab;
pub use resize_grab::{ResizeEdge, ResizeSurfaceGrab};
