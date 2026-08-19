//! The floating listening indicator — a small always-on-top waveform panel.
//!
//! COMMANDS-SPEC 3.5 calls the HUD load-bearing, and it is: without it you
//! cannot tell whether the mic is actually hearing you, so a failed dictation
//! is indistinguishable from a slow one. The bars here are driven by real RMS
//! from the capture callback, not a canned animation — if they do not move, the
//! audio genuinely is not arriving.
//!
//! **The panel must never take key focus.** Dictation ends by synthesizing ⌘V
//! into whatever the user was typing in; if this window ever becomes key, the
//! paste lands *here* instead and the text is lost. That is enforced three ways
//! — a `NonactivatingPanel` style mask, an `Accessory` activation policy (no
//! Dock icon, no menu bar, app never activates), and `orderFrontRegardless`
//! rather than `makeKeyAndOrderFront`. Do not "simplify" any of those away.
//!
//! NOT VERIFIED: drawing has never been observed. It needs a real login session
//! and a Microphone grant, neither available to an automated run.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_core_foundation::CFRetained;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSPanel, NSScreen,
    NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_graphics::CGColor;
use objc2_foundation::{NSPoint, NSRect, NSSize};
use objc2_quartz_core::{CALayer, CATransaction};

const BAR_COUNT: usize = 20;
const BAR_WIDTH: f64 = 3.0;
const BAR_GAP: f64 = 3.0;
const PANEL_H: f64 = 38.0;
const PAD_X: f64 = 12.0;
const MIN_BAR_H: f64 = 2.5;
/// Distance from the bottom of the screen. Low enough to be out of the way,
/// high enough to clear the Dock.
const BOTTOM_MARGIN: f64 = 96.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudState {
    Hidden,
    Listening,
    Transcribing,
}

pub struct Hud {
    panel: Retained<NSPanel>,
    bars: Vec<Retained<CALayer>>,
    /// Rolling RMS history, newest last. Always `BAR_COUNT` long.
    levels: Vec<f32>,
    state: HudState,
    frame: u64,
}

fn cg(r: f64, g: f64, b: f64, a: f64) -> CFRetained<CGColor> {
    CGColor::new_srgb(r, g, b, a)
}

impl Hud {
    /// Build the panel. Must be called on the main thread, before the run loop
    /// starts pumping.
    pub fn new() -> anyhow::Result<Self> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow::anyhow!("the HUD must be created on the main thread"))?;

        // Accessory: no Dock icon, no menu bar, and critically the app never
        // becomes active, so focus stays wherever the user is typing.
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let panel_w = PAD_X * 2.0 + (BAR_COUNT as f64) * BAR_WIDTH + ((BAR_COUNT - 1) as f64) * BAR_GAP;

        let screen_frame = NSScreen::mainScreen(mtm)
            .map(|s| s.frame())
            .unwrap_or(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0)));
        let origin = NSPoint::new(
            screen_frame.origin.x + (screen_frame.size.width - panel_w) / 2.0,
            screen_frame.origin.y + BOTTOM_MARGIN,
        );
        let content = NSRect::new(origin, NSSize::new(panel_w, PANEL_H));

        // NonactivatingPanel is the load-bearing bit — see the module docs.
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;

        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
                NSPanel::alloc(mtm),
                content,
                style,
            NSBackingStoreType::Buffered,
            false,
        );

        {
            panel.setOpaque(false);
            panel.setBackgroundColor(Some(&NSColor::clearColor()));
            panel.setHasShadow(true);
            panel.setIgnoresMouseEvents(true);
            // Float above normal windows and follow the user across Spaces and
            // into fullscreen apps — dictation happens wherever they already are.
            panel.setLevel(25); // NSStatusWindowLevel
            panel.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary
                    | NSWindowCollectionBehavior::Stationary,
            );
            panel.setHidesOnDeactivate(false);
        }

        let view = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(panel_w, PANEL_H)),
        );
        view.setWantsLayer(true);
        panel.setContentView(Some(&view));

        let root = view
            .layer()
            .ok_or_else(|| anyhow::anyhow!("the HUD content view has no backing layer"))?;
        {
            root.setBackgroundColor(Some(&cg(0.07, 0.07, 0.09, 0.86)));
            root.setCornerRadius(10.0);
        }

        let mut bars = Vec::with_capacity(BAR_COUNT);
        for i in 0..BAR_COUNT {
            let bar = CALayer::layer();
            let x = PAD_X + (i as f64) * (BAR_WIDTH + BAR_GAP);
            {
                bar.setFrame(NSRect::new(
                    NSPoint::new(x, (PANEL_H - MIN_BAR_H) / 2.0),
                    NSSize::new(BAR_WIDTH, MIN_BAR_H),
                ));
                bar.setCornerRadius(BAR_WIDTH / 2.0);
                bar.setBackgroundColor(Some(&cg(0.42, 0.72, 1.0, 1.0)));
                root.addSublayer(&bar);
            }
            bars.push(bar);
        }

        Ok(Self {
            panel,
            bars,
            levels: vec![0.0; BAR_COUNT],
            state: HudState::Hidden,
            frame: 0,
        })
    }

    pub fn show_listening(&mut self) {
        self.levels.iter_mut().for_each(|l| *l = 0.0);
        self.frame = 0;
        self.set_tint(0.42, 0.72, 1.0);
        self.state = HudState::Listening;
        // orderFrontRegardless, never makeKeyAndOrderFront — see module docs.
        self.panel.orderFrontRegardless();
    }

    pub fn show_transcribing(&mut self) {
        self.state = HudState::Transcribing;
        self.set_tint(1.0, 0.78, 0.35);
        self.panel.orderFrontRegardless();
    }

    pub fn hide(&mut self) {
        self.state = HudState::Hidden;
        self.panel.orderOut(None);
    }

    fn set_tint(&self, r: f64, g: f64, b: f64) {
        for bar in &self.bars {
            bar.setBackgroundColor(Some(&cg(r, g, b, 1.0)));
        }
    }

    /// Advance one animation frame. `level` is the current capture RMS in
    /// 0.0..=1.0; it is ignored when not listening.
    pub fn tick(&mut self, level: f32) {
        if self.state == HudState::Hidden {
            return;
        }
        self.frame = self.frame.wrapping_add(1);

        match self.state {
            HudState::Listening => {
                self.levels.remove(0);
                self.levels.push(level.clamp(0.0, 1.0));
            }
            HudState::Transcribing => {
                // A travelling shimmer, so "working" is visibly different from
                // "hearing you" rather than just a different colour.
                for (i, slot) in self.levels.iter_mut().enumerate() {
                    let phase = (self.frame as f64) * 0.18 - (i as f64) * 0.45;
                    *slot = (0.20 + 0.16 * phase.sin()) as f32;
                }
            }
            HudState::Hidden => return,
        }

        // Implicit CALayer animations would smear every frame over ~0.25 s,
        // turning a live level meter into laggy mush. The explicit transaction
        // also guarantees the frame changes commit even though this process
        // pumps its own run loop rather than running NSApp's event loop.
        CATransaction::begin();
        CATransaction::setDisableActions(true);

        let usable = PANEL_H - 12.0;
        for (i, bar) in self.bars.iter().enumerate() {
            // sqrt gives quiet speech visible movement; a linear map makes
            // normal talking look like almost nothing.
            let amp = (self.levels[i].max(0.0) as f64).sqrt();
            let h = (MIN_BAR_H + amp * usable).min(usable).max(MIN_BAR_H);
            let x = PAD_X + (i as f64) * (BAR_WIDTH + BAR_GAP);
            bar.setFrame(NSRect::new(
                NSPoint::new(x, (PANEL_H - h) / 2.0),
                NSSize::new(BAR_WIDTH, h),
            ));
        }

        CATransaction::commit();
    }
}

/// Convert a block of i16 PCM to a 0.0..=1.0 RMS level suitable for `tick`.
///
/// Pulled out of the HUD so it is testable without a window server, and so the
/// capture callback can call it on the audio thread without touching AppKit.
pub fn rms_level(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f64 = pcm.iter().map(|&s| { let v = s as f64 / 32768.0; v * v }).sum();
    let rms = (sum / pcm.len() as f64).sqrt();
    // Speech RMS sits well below 1.0; scale so normal talking lands mid-range
    // instead of hugging the floor.
    ((rms * 4.0) as f32).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn silence_is_zero_and_full_scale_saturates() {
        assert_eq!(rms_level(&[]), 0.0);
        assert_eq!(rms_level(&[0; 512]), 0.0);
        assert_eq!(rms_level(&[i16::MAX; 512]), 1.0);
    }

    #[test]
    fn speech_level_audio_lands_in_a_visible_mid_range() {
        // The real fixtures sit around RMS 5400-5900 out of 32768. If the
        // scaling regresses, the bars flatline during normal speech and the
        // HUD silently stops meaning anything.
        let pcm = vec![5900i16; 1024];
        let level = rms_level(&pcm);
        assert!(level > 0.4 && level < 0.95, "speech-level RMS mapped to {level}, expected mid-range");
    }

    #[test]
    fn level_is_monotonic_in_amplitude() {
        let quiet = rms_level(&[800i16; 512]);
        let normal = rms_level(&[5900i16; 512]);
        let loud = rms_level(&[20000i16; 512]);
        assert!(quiet < normal, "{quiet} !< {normal}");
        assert!(normal < loud, "{normal} !< {loud}");
    }

    #[test]
    fn a_dc_offset_block_still_reports_energy() {
        // RMS, not peak-to-peak: a constant non-zero block is energy, and
        // returning 0 here would mean a stuck mic reads as silence.
        assert!(rms_level(&[3000i16; 256]) > 0.0);
    }
}
