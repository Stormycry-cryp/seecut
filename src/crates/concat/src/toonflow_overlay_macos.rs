//! Clip only the native assistant's footprint out of the child WebView.
//! The Flow view keeps its original size and live viewport; hit-testing falls
//! through to the Slint parent inside the same footprint.
use std::cell::Cell;
use objc2::{define_class, msg_send, rc::Retained, DefinedClass, MainThreadOnly};
use objc2_app_kit::NSView;
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
use objc2_core_graphics::CGMutablePath;
use objc2_quartz_core::{CAShapeLayer, kCAFillRuleEvenOdd};
use wry::WebViewExtMacOS;

#[derive(Default)]
pub struct Ivars { hole: Cell<Option<NSRect>> }
define_class!(
    #[unsafe(super(NSView))]
    #[ivars = Ivars]
    pub struct FlowContainer;
    impl FlowContainer {
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            let local = self.convertPoint_fromView(point, unsafe { self.superview() }.as_deref());
            if self.ivars().hole.get().is_some_and(|r| local.x >= r.origin.x && local.y >= r.origin.y && local.x <= r.origin.x+r.size.width && local.y <= r.origin.y+r.size.height) { return std::ptr::null_mut(); }
            let hit: *mut NSView = unsafe { msg_send![super(self), hitTest: point] };
            // Empty parts of the wrapper, including the sidebar and every page
            // while Flow is hidden, belong to the native Slint parent.
            if std::ptr::eq(hit, self as *const Self as *const NSView) { std::ptr::null_mut() } else { hit }
        }
    }
);
impl FlowContainer {
    pub(crate) fn attach(webview: &wry::WebView) -> Option<Retained<Self>> {
        let mtm=MainThreadMarker::new()?;
        let web=webview.webview(); let parent=unsafe { web.superview() }?;
        let allocated=Self::alloc(mtm).set_ivars(Ivars::default());
        let container: Retained<Self>=unsafe { msg_send![super(allocated), initWithFrame: parent.bounds()] };
        container.setWantsLayer(true);
        web.removeFromSuperview(); parent.addSubview(&container); container.addSubview(&web);
        Some(container)
    }
    pub(crate) fn update(&self, hole: Option<[f64;4]>) {
        let Some(parent)=(unsafe { self.superview() }) else {return};
        self.setFrame(parent.bounds());
        // Slint coordinates start at top-left; AppKit's content view is bottom-left.
        let rect=hole.map(|v|NSRect::new(NSPoint::new(v[0],self.bounds().size.height-v[1]-v[3]),NSSize::new(v[2],v[3])));
        self.ivars().hole.set(rect);
        let Some(layer)=self.layer() else {return};
        if let Some(rect)=rect {
            let path=CGMutablePath::new();
            unsafe { CGMutablePath::add_rect(Some(&path),std::ptr::null(),self.bounds()); CGMutablePath::add_rect(Some(&path),std::ptr::null(),rect); }
            let mask=CAShapeLayer::new(); mask.setFrame(self.bounds()); mask.setPath(Some(&path));
            unsafe { mask.setFillRule(kCAFillRuleEvenOdd); }
            unsafe { layer.setMask(Some(&mask)); }
        } else { unsafe { layer.setMask(None); } }
    }
}
