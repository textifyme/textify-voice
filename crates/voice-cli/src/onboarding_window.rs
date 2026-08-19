//! First-run permissions checklist — one window, live status, no modal chain.
//!
//! The first version of this flow was a sequence of `NSAlert`s, one per
//! permission, and it was the wrong shape for a reason worth writing down:
//! macOS has **two kinds of permission**, and modal alerts make them look
//! identical when they behave nothing alike.
//!
//! * **In-app grantable** (Microphone, Camera): calling `requestAccess` shows
//!   the system consent dialog and the user never leaves the app. One click.
//! * **Settings-only** (Accessibility, Input Monitoring, Screen Recording):
//!   there is no API that grants these. The user MUST open System Settings,
//!   find the app in a list, and flip a switch.
//!
//! Presenting both as "an alert with a Try Again button" implies the app can
//! retry something it cannot, and that is exactly how the alert version dead-
//! ended: a Try Again button that could never succeed, next to a sentence
//! mentioning a second permission the user could not see the state of.
//!
//! So: a checklist. Every permission visible at once with its own live status,
//! its own affordance, and no button that lies about what it does. The window
//! polls, so a permission granted in System Settings turns green by itself —
//! the absence of a "Try Again" button is the point, not an omission.
//!
//! NOT VERIFIED: nobody in an automated environment can see this window or
//! click it. Construction and the pure state logic are tested; appearance is not.

use std::sync::mpsc::{self, Receiver, Sender};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSButton, NSColor, NSFont,
    NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use crate::onboarding::{open_deep_link, OnboardingStep};

const WIN_W: f64 = 580.0;
const WIN_H: f64 = 430.0;
const ROW_H: f64 = 84.0;
const MARGIN: f64 = 26.0;

/// Which checklist row a click came from.
const TAG_MIC: isize = 1;
const TAG_AX: isize = 2;
const TAG_MODEL: isize = 3;
const TAG_CONTINUE: isize = 10;
const TAG_QUIT: isize = 11;
const TAG_RELAUNCH: isize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecklistEvent {
    GrantMicrophone,
    OpenAccessibilitySettings,
    DownloadModel,
    Continue,
    Quit,
    Relaunch,
}

fn event_for_tag(tag: isize) -> Option<ChecklistEvent> {
    match tag {
        TAG_MIC => Some(ChecklistEvent::GrantMicrophone),
        TAG_AX => Some(ChecklistEvent::OpenAccessibilitySettings),
        TAG_MODEL => Some(ChecklistEvent::DownloadModel),
        TAG_CONTINUE => Some(ChecklistEvent::Continue),
        TAG_QUIT => Some(ChecklistEvent::Quit),
        TAG_RELAUNCH => Some(ChecklistEvent::Relaunch),
        _ => None,
    }
}

/// How a single row currently reads. Kept separate from the AppKit objects so
/// the interesting logic — what the row says, and whether Continue is allowed —
/// is testable without a window server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    /// Not granted, and the user has not been sent anywhere yet.
    Pending,
    /// Sent to System Settings; waiting for the switch to flip.
    Waiting,
    Granted,
    /// Granted in System Settings, but this process cannot see it until it
    /// restarts. Accessibility only.
    NeedsRelaunch,
}

impl RowState {
    pub fn badge(self) -> &'static str {
        match self {
            RowState::Granted => "✓",
            RowState::NeedsRelaunch => "↻",
            RowState::Waiting => "…",
            RowState::Pending => "○",
        }
    }

    pub fn is_satisfied(self) -> bool {
        matches!(self, RowState::Granted)
    }
}

/// The action button's label for a row. Deliberately different per permission
/// class: "Allow" promises an in-app dialog, "Open Settings…" promises a trip.
/// A single shared label would be the alert version's mistake again.
pub fn action_title(step: OnboardingStep, state: RowState) -> &'static str {
    match (step, state) {
        (_, RowState::Granted) => "Granted",
        (OnboardingStep::Microphone, _) => "Allow…",
        (OnboardingStep::Accessibility, RowState::NeedsRelaunch) => "Quit & Reopen",
        (OnboardingStep::Accessibility, _) => "Open Settings…",
        (OnboardingStep::ModelDownload, _) => "Download",
        _ => "Continue",
    }
}

struct TargetIvars {
    tx: Sender<ChecklistEvent>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and this type has no
    // Drop impl that would conflict with objc's deallocation.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = TargetIvars]
    struct ChecklistTarget;

    unsafe impl NSObjectProtocol for ChecklistTarget {}

    impl ChecklistTarget {
        /// One selector for every button; the tag says which. Mirrors
        /// `menubar.rs`'s pattern, which is the one proven to actually
        /// dispatch rather than merely compile.
        #[unsafe(method(checklistAction:))]
        fn checklist_action(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            // SAFETY: every sender wired to this selector is one of this
            // module's own NSButtons, which responds to `tag`.
            let tag: isize = unsafe { msg_send![sender, tag] };
            if let Some(event) = event_for_tag(tag) {
                // A disconnected receiver just means nobody is listening —
                // never a reason to panic from inside an AppKit callback.
                let _ = self.ivars().tx.send(event);
            }
        }
    }
);

impl ChecklistTarget {
    fn new(mtm: MainThreadMarker, tx: Sender<ChecklistEvent>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TargetIvars { tx });
        // SAFETY: NSObject's `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }
}

fn label(
    mtm: MainThreadMarker,
    text: &str,
    frame: NSRect,
    size: f64,
    bold: bool,
    secondary: bool,
) -> Retained<NSTextField> {
    let tf = NSTextField::initWithFrame(NSTextField::alloc(mtm), frame);
    tf.setStringValue(&NSString::from_str(text));
    tf.setBezeled(false);
    tf.setDrawsBackground(false);
    tf.setEditable(false);
    tf.setSelectable(false);
    let font = if bold {
        NSFont::boldSystemFontOfSize(size)
    } else {
        NSFont::systemFontOfSize(size)
    };
    tf.setFont(Some(&font));
    if secondary {
        tf.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    tf
}

fn button(
    mtm: MainThreadMarker,
    title: &str,
    frame: NSRect,
    tag: isize,
    target: &ChecklistTarget,
) -> Retained<NSButton> {
    // SAFETY: the target responds to `checklistAction:` (see define_class!
    // above) and outlives every button, since Checklist owns both.
    let b = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            Some(target),
            Some(sel!(checklistAction:)),
            mtm,
        )
    };
    b.setFrame(frame);
    b.setTag(tag);
    b
}

/// One checklist row's AppKit objects, so the poll loop can update them in place.
struct Row {
    step: OnboardingStep,
    badge: Retained<NSTextField>,
    status: Retained<NSTextField>,
    action: Retained<NSButton>,
    state: RowState,
}

impl Row {
    fn apply(&mut self, state: RowState, status_text: &str) {
        if self.state == state {
            return;
        }
        self.state = state;
        self.badge.setStringValue(&NSString::from_str(state.badge()));
        self.status.setStringValue(&NSString::from_str(status_text));
        self.action
            .setTitle(&NSString::from_str(action_title(self.step, state)));
        self.action.setEnabled(!state.is_satisfied());
    }
}

/// Build the window. Returns the window, its rows, and the event receiver.
pub struct Checklist {
    window: Retained<NSWindow>,
    rows: Vec<Row>,
    rx: Receiver<ChecklistEvent>,
    _target: Retained<ChecklistTarget>,
    continue_button: Retained<NSButton>,
}

impl Checklist {
    pub fn new(mtm: MainThreadMarker) -> anyhow::Result<Self> {
        let app = NSApplication::sharedApplication(mtm);
        // A window needs the app to activate, or it opens behind everything.
        // Accessory (not Regular) keeps this out of the Dock — it is a
        // first-run flow, not an application the user switches to.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIN_W, WIN_H));
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(ns_string!("Welcome to Textify Voice"));
        window.center();

        let content = NSView::initWithFrame(NSView::alloc(mtm), frame);

        let (tx, rx) = mpsc::channel();
        let target = ChecklistTarget::new(mtm, tx);

        let mut y = WIN_H - MARGIN - 34.0;
        let title = label(
            mtm,
            "Hold a key, speak, release.",
            NSRect::new(NSPoint::new(MARGIN, y), NSSize::new(WIN_W - MARGIN * 2.0, 28.0)),
            19.0,
            true,
            false,
        );
        content.addSubview(&title);
        y -= 24.0;
        let sub = label(
            mtm,
            "Your words land wherever you were typing. Two permissions make that possible.",
            NSRect::new(NSPoint::new(MARGIN, y), NSSize::new(WIN_W - MARGIN * 2.0, 20.0)),
            12.0,
            false,
            true,
        );
        content.addSubview(&sub);

        let specs = [
            (
                OnboardingStep::Microphone,
                "Microphone",
                "So Textify Voice can hear you. macOS will ask — just click Allow.",
                TAG_MIC,
            ),
            (
                OnboardingStep::Accessibility,
                "Accessibility",
                "To notice the hold key and paste for you. This one can only be switched on in System Settings.",
                TAG_AX,
            ),
            (
                OnboardingStep::ModelDownload,
                "Speech model",
                "About 150 MB, downloaded once. Everything runs on this Mac afterwards.",
                TAG_MODEL,
            ),
        ];

        let mut rows = Vec::new();
        y -= 22.0;
        for (step, name, why, tag) in specs {
            y -= ROW_H;
            let badge = label(
                mtm,
                RowState::Pending.badge(),
                NSRect::new(NSPoint::new(MARGIN, y + 38.0), NSSize::new(26.0, 24.0)),
                17.0,
                true,
                false,
            );
            let name_l = label(
                mtm,
                name,
                NSRect::new(NSPoint::new(MARGIN + 30.0, y + 40.0), NSSize::new(240.0, 20.0)),
                14.0,
                true,
                false,
            );
            let why_l = label(
                mtm,
                why,
                NSRect::new(NSPoint::new(MARGIN + 30.0, y + 2.0), NSSize::new(330.0, 36.0)),
                11.0,
                false,
                true,
            );
            let status = label(
                mtm,
                "Not yet granted",
                NSRect::new(NSPoint::new(MARGIN + 30.0, y + 24.0), NSSize::new(330.0, 16.0)),
                11.0,
                false,
                true,
            );
            let action = button(
                mtm,
                action_title(step, RowState::Pending),
                NSRect::new(NSPoint::new(WIN_W - MARGIN - 150.0, y + 30.0), NSSize::new(150.0, 30.0)),
                tag,
                &target,
            );
            content.addSubview(&badge);
            content.addSubview(&name_l);
            content.addSubview(&why_l);
            content.addSubview(&status);
            content.addSubview(&action);
            rows.push(Row { step, badge, status, action, state: RowState::Pending });
        }

        let quit = button(
            mtm,
            "Quit",
            NSRect::new(NSPoint::new(MARGIN, MARGIN - 8.0), NSSize::new(90.0, 32.0)),
            TAG_QUIT,
            &target,
        );
        let cont = button(
            mtm,
            "Start Dictating",
            NSRect::new(NSPoint::new(WIN_W - MARGIN - 160.0, MARGIN - 8.0), NSSize::new(160.0, 32.0)),
            TAG_CONTINUE,
            &target,
        );
        cont.setEnabled(false);
        content.addSubview(&quit);
        content.addSubview(&cont);

        window.setContentView(Some(&content));
        window.makeKeyAndOrderFront(None);
        app.activate();

        Ok(Self { window, rows, rx, _target: target, continue_button: cont })
    }

    /// Update every row from live state. Called on a timer, which is why there
    /// is no "Try Again" button: granting in System Settings turns the row
    /// green by itself.
    pub fn refresh(&mut self, mic: RowState, ax: RowState, model: RowState) {
        for row in &mut self.rows {
            let (state, text) = match row.step {
                OnboardingStep::Microphone => (mic, status_text(mic)),
                OnboardingStep::Accessibility => (ax, status_text(ax)),
                _ => (model, status_text(model)),
            };
            row.apply(state, text);
        }
        let ready = self.rows.iter().all(|r| r.state.is_satisfied());
        self.continue_button.setEnabled(ready);
        // Accessibility granted but invisible to this process is the one state
        // the user cannot resolve from here, so promote it to the primary action.
        if ax == RowState::NeedsRelaunch {
            self.continue_button.setTitle(ns_string!("Quit & Reopen"));
            self.continue_button.setTag(TAG_RELAUNCH);
            self.continue_button.setEnabled(true);
        }
    }

    pub fn poll(&self) -> Vec<ChecklistEvent> {
        self.rx.try_iter().collect()
    }

    pub fn close(&self) {
        self.window.close();
    }
}

pub fn status_text(state: RowState) -> &'static str {
    match state {
        RowState::Granted => "Granted",
        RowState::NeedsRelaunch => "Switched on — reopen Textify Voice to pick it up",
        RowState::Waiting => "Waiting for you in System Settings…",
        RowState::Pending => "Not yet granted",
    }
}

/// Open the Accessibility pane. Separate from the Microphone row on purpose:
/// there is no API that grants Accessibility, so this is the only affordance
/// that row can honestly offer.
pub fn open_accessibility_settings() {
    if let Some(url) = OnboardingStep::Accessibility.deep_link_url() {
        let _ = open_deep_link(url);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn every_tag_round_trips_and_unknown_tags_map_to_nothing() {
        for (tag, ev) in [
            (TAG_MIC, ChecklistEvent::GrantMicrophone),
            (TAG_AX, ChecklistEvent::OpenAccessibilitySettings),
            (TAG_MODEL, ChecklistEvent::DownloadModel),
            (TAG_CONTINUE, ChecklistEvent::Continue),
            (TAG_QUIT, ChecklistEvent::Quit),
            (TAG_RELAUNCH, ChecklistEvent::Relaunch),
        ] {
            assert_eq!(event_for_tag(tag), Some(ev));
        }
        assert_eq!(event_for_tag(0), None);
        assert_eq!(event_for_tag(999), None);
    }

    #[test]
    fn the_two_permission_classes_get_different_affordances() {
        // This is the whole lesson of the alert version: Microphone can be
        // granted in-app, Accessibility cannot, and a shared button label hides
        // that difference. If these ever converge, the flow regresses to
        // implying the app can retry something it cannot.
        assert_eq!(action_title(OnboardingStep::Microphone, RowState::Pending), "Allow…");
        assert_eq!(
            action_title(OnboardingStep::Accessibility, RowState::Pending),
            "Open Settings…"
        );
        assert_ne!(
            action_title(OnboardingStep::Microphone, RowState::Pending),
            action_title(OnboardingStep::Accessibility, RowState::Pending)
        );
    }

    #[test]
    fn a_granted_row_offers_no_action() {
        for step in [
            OnboardingStep::Microphone,
            OnboardingStep::Accessibility,
            OnboardingStep::ModelDownload,
        ] {
            assert_eq!(action_title(step, RowState::Granted), "Granted");
        }
        assert!(RowState::Granted.is_satisfied());
    }

    #[test]
    fn needs_relaunch_is_not_satisfied_and_says_what_to_do() {
        // The state that dead-ended the alert version: switched on, but
        // invisible to this process. It must NOT count as satisfied, and its
        // text must name the actual resolution rather than "try again".
        assert!(!RowState::NeedsRelaunch.is_satisfied());
        assert_eq!(
            action_title(OnboardingStep::Accessibility, RowState::NeedsRelaunch),
            "Quit & Reopen"
        );
        assert!(status_text(RowState::NeedsRelaunch).contains("reopen"));
    }

    #[test]
    fn every_state_has_a_distinct_badge() {
        let badges = [
            RowState::Pending.badge(),
            RowState::Waiting.badge(),
            RowState::Granted.badge(),
            RowState::NeedsRelaunch.badge(),
        ];
        for i in 0..badges.len() {
            for j in (i + 1)..badges.len() {
                assert_ne!(badges[i], badges[j], "two states share a badge");
            }
        }
    }
}


/// Re-open our own `.app` and exit, so the new process picks up a freshly
/// granted Accessibility trust.
///
/// macOS evaluates `AXIsProcessTrusted()` for a process essentially at launch,
/// so a process that was already running when the grant landed keeps reporting
/// untrusted. There is no way to refresh it in place — restarting is the
/// resolution, and the user has no way to know that unless we offer it.
pub fn relaunch_bundle() -> ! {
    if let Ok(crate::login_item::BundleContext::Bundled(app)) =
        crate::login_item::current_exe_bundle_context()
    {
        // `-n` forces a new instance rather than activating this dying one.
        let _ = std::process::Command::new("/usr/bin/open").arg("-n").arg(&app).spawn();
    }
    std::process::exit(0);
}

/// How the checklist ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Everything granted; the caller should arm dictation.
    Ready,
    /// The user quit.
    Quit,
    /// The user chose to relaunch so a fresh process picks up Accessibility.
    Relaunch,
}

/// Seconds after sending the user to System Settings before we start
/// suggesting a relaunch.
///
/// There is no API that distinguishes "not granted" from "granted but this
/// process cannot see it", so this is a heuristic rather than a reading. It is
/// deliberately generous: suggesting a relaunch to someone who simply has not
/// flipped the switch yet is noise, and the row still says plainly which one it
/// means.
const RELAUNCH_HINT_AFTER: f64 = 12.0;

/// Run the checklist until the user leaves it. Polls live state ~5x/sec, which
/// is what removes the need for a "Try Again" button.
pub fn run(mtm: MainThreadMarker) -> anyhow::Result<Outcome> {
    use objc2_app_kit::NSEventMask;
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};
    use std::time::Instant;

    let mut ui = Checklist::new(mtm)?;
    let app = NSApplication::sharedApplication(mtm);

    let mut sent_to_settings_at: Option<Instant> = None;
    let mut asked_for_mic = false;

    loop {
        let mic_granted = matches!(
            voice_audio::microphone_permission_status(),
            voice_audio::MicPermission::Authorized
        );
        let ax_granted = crate::permissions::accessibility_granted();
        let model_ready = crate::onboarding::model_is_cached();

        let mic = if mic_granted {
            RowState::Granted
        } else if asked_for_mic {
            RowState::Waiting
        } else {
            RowState::Pending
        };
        let ax = if ax_granted {
            RowState::Granted
        } else {
            match sent_to_settings_at {
                Some(t) if t.elapsed().as_secs_f64() > RELAUNCH_HINT_AFTER => {
                    RowState::NeedsRelaunch
                }
                Some(_) => RowState::Waiting,
                None => RowState::Pending,
            }
        };
        let model = if model_ready { RowState::Granted } else { RowState::Pending };

        ui.refresh(mic, ax, model);

        for event in ui.poll() {
            match event {
                ChecklistEvent::GrantMicrophone => {
                    asked_for_mic = true;
                    voice_audio::request_microphone_access();
                }
                ChecklistEvent::OpenAccessibilitySettings => {
                    // Prompting also registers us in the Accessibility list, so
                    // there is a row to switch on when the pane opens.
                    let _ = crate::permissions::prompt_for_accessibility();
                    open_accessibility_settings();
                    sent_to_settings_at = Some(Instant::now());
                }
                ChecklistEvent::DownloadModel => {
                    if let Err(e) = crate::onboarding::download_gate_model() {
                        eprintln!("[model download failed: {e:#}]");
                    }
                }
                ChecklistEvent::Continue => {
                    ui.close();
                    return Ok(Outcome::Ready);
                }
                ChecklistEvent::Relaunch => {
                    ui.close();
                    return Ok(Outcome::Relaunch);
                }
                ChecklistEvent::Quit => {
                    ui.close();
                    return Ok(Outcome::Quit);
                }
            }
        }

        // PUMP APPKIT, NOT JUST CFRunLoop.
        //
        // The HUD panel elsewhere in this crate gets away with a bare
        // CFRunLoop pump because it is click-through, non-activating and drawn
        // entirely from CALayer frames. An INTERACTIVE window does not: clicks,
        // redraw and window dragging all arrive as NSEvents that only
        // NSApplication dequeues. Pumping CFRunLoop alone leaves them queued
        // forever, the window never repaints or responds, and macOS marks the
        // process "Not Responding" — which is exactly what the first version of
        // this window did.
        //
        // untilDate 0.1s ahead means this blocks (rather than spins) when there
        // is nothing to do, while still refreshing row state ~10x/sec.
        let until = NSDate::dateWithTimeIntervalSinceNow(0.1);
        while let Some(event) = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&until),
                NSDefaultRunLoopMode,
                true,
            )
        } {
            app.sendEvent(&event);
        }
        app.updateWindows();
    }
}
