//! Inline Writing Tools (macOS 15.2+) for a window's focused text input,
//! through `NSWritingToolsCoordinator`. The coordinator asks for text,
//! replacements, selections and geometry; every answer comes from the
//! window's `PlatformInputHandler`, the same one IME uses. The app turns it
//! on and off (`Window::set_writing_tools`) as focus moves to and from a
//! text input that wants it.
//!
//! One context per request: the whole document, with the selection (or, with
//! nothing selected, all of it) as the range to work on. Ranges the
//! coordinator hands back are offsets into that document, in UTF-16.
//!
//! Animations are not drawn: preparing and finishing complete at once and no
//! preview is offered, so the coordinator falls back to showing the result.

use std::ffi::c_void;
use std::ops::Range;
use std::ptr::{self, NonNull};

use objc::runtime::Object;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBezierPath, NSTextPreview, NSView, NSWritingToolsBehavior, NSWritingToolsCoordinator,
    NSWritingToolsCoordinatorAnimationParameters, NSWritingToolsCoordinatorContext,
    NSWritingToolsCoordinatorContextScope, NSWritingToolsCoordinatorDelegate,
    NSWritingToolsCoordinatorTextAnimation, NSWritingToolsCoordinatorTextReplacementReason,
    NSWritingToolsResultOptions,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSObject, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize,
    NSString, NSValue,
};

use crate::window::with_input_handler;

/// A view's coordinator and its delegate, kept for as long as the window.
pub(crate) struct WritingTools {
    coordinator: Retained<NSWritingToolsCoordinator>,
    _delegate: Retained<Delegate>,
}

/// Attach (or detach) Writing Tools on `view`; `tools` holds what was made
/// the first time, so a focus change back is only a property set.
pub(crate) fn set(view: *mut c_void, enabled: bool, tools: &mut Option<WritingTools>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if !objc2::available!(macos = 15.2) {
        return;
    }
    // SAFETY: the window's own content view, alive with the window.
    let ns_view: &NSView = unsafe { &*(view as *const NSView) };
    if !enabled {
        ns_view.setWritingToolsCoordinator(None);
        return;
    }
    if !NSWritingToolsCoordinator::isWritingToolsAvailable(mtm) {
        return;
    }
    let tools = tools.get_or_insert_with(|| {
        let delegate = Delegate::new(view, mtm);
        let coordinator = NSWritingToolsCoordinator::initWithDelegate(
            NSWritingToolsCoordinator::alloc(mtm),
            Some(ProtocolObject::from_ref(&*delegate)),
        );
        coordinator.setPreferredBehavior(NSWritingToolsBehavior::Complete);
        coordinator.setPreferredResultOptions(NSWritingToolsResultOptions::PlainText);
        WritingTools { coordinator, _delegate: delegate }
    });
    ns_view.setWritingToolsCoordinator(Some(&tools.coordinator));
}

struct Ivars {
    view: *mut c_void,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and `Delegate` does
    // not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    #[name = "GPUIWritingToolsDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSWritingToolsCoordinatorDelegate for Delegate {
        #[unsafe(method(writingToolsCoordinator:requestsContextsForScope:completion:))]
        fn contexts(
            &self,
            _coordinator: &NSWritingToolsCoordinator,
            scope: NSWritingToolsCoordinatorContextScope,
            completion: &block2::DynBlock<dyn Fn(NonNull<NSArray<NSWritingToolsCoordinatorContext>>)>,
        ) {
            let whole = scope == NSWritingToolsCoordinatorContextScope::FullDocument;
            let found = self.input(|handler| {
                let length = handler.text_length_utf16().unwrap_or(u32::MAX as usize);
                let mut adjusted = None;
                let text = handler.text_for_range(0..length, &mut adjusted)?;
                let length = text.encode_utf16().count();
                let selected = handler.selected_text_range(false).map(|s| s.range).filter(|r| !r.is_empty());
                let range = if whole { 0..length } else { selected.unwrap_or(0..length) };
                Some((text, range))
            });
            let contexts = match found.flatten() {
                Some((text, range)) => {
                    let string = NSString::from_str(&text);
                    let attributed = NSAttributedString::initWithString(NSAttributedString::alloc(), &string);
                    let context = NSWritingToolsCoordinatorContext::initWithAttributedString_range(
                        NSWritingToolsCoordinatorContext::alloc(),
                        &attributed,
                        ns_range(range),
                    );
                    NSArray::from_retained_slice(&[context])
                }
                None => NSArray::new(),
            };
            completion.call((NonNull::from(&*contexts),));
        }

        #[unsafe(method(writingToolsCoordinator:replaceRange:inContext:proposedText:reason:animationParameters:completion:))]
        fn replace(
            &self,
            _coordinator: &NSWritingToolsCoordinator,
            range: NSRange,
            _context: &NSWritingToolsCoordinatorContext,
            replacement: &NSAttributedString,
            _reason: NSWritingToolsCoordinatorTextReplacementReason,
            _animation: Option<&NSWritingToolsCoordinatorAnimationParameters>,
            completion: &block2::DynBlock<dyn Fn(*mut NSAttributedString)>,
        ) {
            let text = replacement.string().to_string();
            let applied = self.input(|handler| handler.replace_text_in_range(Some(range_of(range)), &text));
            let result = if applied.is_some() { replacement as *const NSAttributedString as *mut NSAttributedString } else { ptr::null_mut() };
            completion.call((result,));
        }

        #[unsafe(method(writingToolsCoordinator:selectRanges:inContext:completion:))]
        fn select(
            &self,
            _coordinator: &NSWritingToolsCoordinator,
            ranges: &NSArray<NSValue>,
            _context: &NSWritingToolsCoordinatorContext,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            if let Some(first) = ranges.firstObject() {
                let range = range_of(first.get_range().unwrap_or(NSRange::new(0, 0)));
                self.input(|handler| handler.set_selected_text_range(range));
            }
            completion.call(());
        }

        #[unsafe(method(writingToolsCoordinator:requestsBoundingBezierPathsForRange:inContext:completion:))]
        fn bounding_paths(
            &self,
            _coordinator: &NSWritingToolsCoordinator,
            range: NSRange,
            _context: &NSWritingToolsCoordinatorContext,
            completion: &block2::DynBlock<dyn Fn(NonNull<NSArray<NSBezierPath>>)>,
        ) {
            let paths = self.paths(range, false);
            completion.call((NonNull::from(&*paths),));
        }

        #[unsafe(method(writingToolsCoordinator:requestsUnderlinePathsForRange:inContext:completion:))]
        fn underline_paths(
            &self,
            _coordinator: &NSWritingToolsCoordinator,
            range: NSRange,
            _context: &NSWritingToolsCoordinatorContext,
            completion: &block2::DynBlock<dyn Fn(NonNull<NSArray<NSBezierPath>>)>,
        ) {
            let paths = self.paths(range, true);
            completion.call((NonNull::from(&*paths),));
        }

        #[unsafe(method(writingToolsCoordinator:prepareForTextAnimation:forRange:inContext:completion:))]
        fn prepare_animation(
            &self,
            _coordinator: &NSWritingToolsCoordinator,
            _animation: NSWritingToolsCoordinatorTextAnimation,
            _range: NSRange,
            _context: &NSWritingToolsCoordinatorContext,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            completion.call(());
        }

        #[unsafe(method(writingToolsCoordinator:requestsPreviewForTextAnimation:ofRange:inContext:completion:))]
        fn animation_preview(
            &self,
            _coordinator: &NSWritingToolsCoordinator,
            _animation: NSWritingToolsCoordinatorTextAnimation,
            _range: NSRange,
            _context: &NSWritingToolsCoordinatorContext,
            completion: &block2::DynBlock<dyn Fn(*mut NSArray<NSTextPreview>)>,
        ) {
            completion.call((ptr::null_mut(),));
        }

        #[unsafe(method(writingToolsCoordinator:requestsPreviewForRect:inContext:completion:))]
        fn rect_preview(
            &self,
            _coordinator: &NSWritingToolsCoordinator,
            _rect: NSRect,
            _context: &NSWritingToolsCoordinatorContext,
            completion: &block2::DynBlock<dyn Fn(*mut NSTextPreview)>,
        ) {
            completion.call((ptr::null_mut(),));
        }

        #[unsafe(method(writingToolsCoordinator:finishTextAnimation:forRange:inContext:completion:))]
        fn finish_animation(
            &self,
            _coordinator: &NSWritingToolsCoordinator,
            _animation: NSWritingToolsCoordinatorTextAnimation,
            _range: NSRange,
            _context: &NSWritingToolsCoordinatorContext,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            completion.call(());
        }
    }
);

impl Delegate {
    fn new(view: *mut c_void, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { view });
        // SAFETY: `NSObject`'s designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    fn input<R>(&self, f: impl FnOnce(&mut gpui::PlatformInputHandler) -> R) -> Option<R> {
        // SAFETY: the view this delegate was made for; the view owns the
        // coordinator that calls us, so it is alive for every call.
        let view = unsafe { &*(self.ivars().view as *const Object) };
        with_input_handler(view, f)
    }

    /// `range`'s bounds as paths in the view's (unflipped) coordinates; an
    /// underline is the bottom edge.
    fn paths(&self, range: NSRange, underline: bool) -> Retained<NSArray<NSBezierPath>> {
        let bounds = self.input(|handler| handler.bounds_for_range(range_of(range))).flatten();
        let Some(bounds) = bounds else {
            return NSArray::new();
        };
        // SAFETY: as in `input`.
        let view: &NSView = unsafe { &*(self.ivars().view as *const NSView) };
        let height = view.bounds().size.height;
        let (x, y) = (f64::from(bounds.origin.x.as_f32()), f64::from(bounds.origin.y.as_f32()));
        let (w, h) = (f64::from(bounds.size.width.as_f32()), f64::from(bounds.size.height.as_f32()));
        let rect = if underline {
            NSRect::new(NSPoint::new(x, height - y - h), NSSize::new(w, 1.0))
        } else {
            NSRect::new(NSPoint::new(x, height - y - h), NSSize::new(w, h))
        };
        NSArray::from_retained_slice(&[NSBezierPath::bezierPathWithRect(rect)])
    }
}

fn range_of(range: NSRange) -> Range<usize> {
    range.location..range.location + range.length
}

fn ns_range(range: Range<usize>) -> NSRange {
    NSRange::new(range.start, range.end - range.start)
}
