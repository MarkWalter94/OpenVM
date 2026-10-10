//! F1-16: automatic selection of the rendering backend with a visible
//! indication (ADR-0014).
//!
//! Default order (`--renderer auto`): **glow** → **wgpu on a hardware
//! adapter** (DX12 on Windows) → **wgpu on a software rasterizer** (WARP on
//! Windows, llvmpipe on Linux). An attempt fails when eframe returns an error
//! before creating the app (GL context, surface or adapter not available);
//! the loop in `main.rs` then moves on to the next attempt reusing the same
//! event loop (`NativeOptions::run_and_return`).
//!
//! The software backend is a **declared degraded mode**: high CPU and reduced
//! console frequency (`DEGRADED_CONSOLE_FPS`).
//!
//! This module contains only the pure logic (plan of the attempts, indicator
//! texts); window creation is in `main.rs`.

/// Maximum console updates per second in degraded mode.
pub const DEGRADED_CONSOLE_FPS: u32 = 5;

/// A rendering initialisation attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attempt {
    /// OpenGL through glow/glutin, hardware acceleration preferred.
    Glow,
    /// wgpu on a hardware adapter (DX12 on Windows; Vulkan/GL elsewhere).
    WgpuHardware,
    /// wgpu on a CPU-type adapter (WARP on Windows): degraded mode.
    WgpuSoftware,
}

impl Attempt {
    /// Order of the automatic selection.
    pub const AUTO: [Self; 3] = [Self::Glow, Self::WgpuHardware, Self::WgpuSoftware];

    /// Name used on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Glow => "glow",
            Self::WgpuHardware => "wgpu",
            Self::WgpuSoftware => "warp",
        }
    }

    /// Short label for the status bar.
    pub fn label(self) -> &'static str {
        match self {
            Self::Glow => "glow (OpenGL)",
            Self::WgpuHardware if cfg!(windows) => "wgpu DX12",
            Self::WgpuHardware => "wgpu (hardware)",
            Self::WgpuSoftware if cfg!(windows) => "wgpu WARP (software)",
            Self::WgpuSoftware => "wgpu software",
        }
    }

    /// The attempt uses a software rasterizer.
    pub fn degraded(self) -> bool {
        matches!(self, Self::WgpuSoftware)
    }

    /// The backend is compiled into this build.
    pub fn compiled(self) -> bool {
        match self {
            Self::Glow => cfg!(feature = "glow"),
            Self::WgpuHardware | Self::WgpuSoftware => cfg!(feature = "wgpu"),
        }
    }
}

/// Plan of the attempts chosen on the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Attempts in order.
    pub attempts: Vec<Attempt>,
    /// `true` if the user forced a single backend (no fallback).
    pub forced: bool,
}

impl Plan {
    /// Parses `--renderer`: `auto` or a comma-separated list of `glow`,
    /// `wgpu`, `warp` (for example `glow,warp`). A single name forces that
    /// backend without fallback. `wgpu_cpu` (`--wgpu-adapter cpu`, F0
    /// compatibility) replaces `wgpu` with `warp`.
    pub fn from_args(renderer: Option<&str>, wgpu_cpu: bool) -> Result<Self, String> {
        let spec = renderer.unwrap_or("auto");
        let (mut attempts, forced) = if spec == "auto" {
            (Attempt::AUTO.to_vec(), false)
        } else {
            let mut v = Vec::new();
            for part in spec.split(',') {
                let t = match part.trim() {
                    "glow" => Attempt::Glow,
                    "wgpu" => Attempt::WgpuHardware,
                    "warp" | "software" => Attempt::WgpuSoftware,
                    other => return Err(format!("unknown renderer: {other}")),
                };
                if v.contains(&t) {
                    return Err(format!("repeated renderer: {}", t.name()));
                }
                v.push(t);
            }
            if v.is_empty() {
                return Err("empty --renderer".into());
            }
            // A single name forces the backend; a list is a fallback order.
            let forced = v.len() == 1;
            (v, forced)
        };
        if wgpu_cpu {
            if renderer.is_none() {
                // F0: `--wgpu-adapter cpu` on its own forces WARP.
                attempts = vec![Attempt::WgpuSoftware];
                return Ok(Self {
                    attempts,
                    forced: true,
                });
            }
            for t in &mut attempts {
                if *t == Attempt::WgpuHardware {
                    *t = Attempt::WgpuSoftware;
                }
            }
            attempts.dedup();
        }
        let not_compiled: Vec<_> = attempts
            .iter()
            .filter(|t| !t.compiled())
            .map(|t| t.name())
            .collect();
        if spec != "auto" && !not_compiled.is_empty() {
            return Err(format!(
                "renderers not included in this build (cargo features): {}",
                not_compiled.join(", ")
            ));
        }
        attempts.retain(|t| t.compiled());
        if attempts.is_empty() {
            return Err("no rendering backend compiled".into());
        }
        Ok(Self { attempts, forced })
    }
}

/// Effective backend and history of the attempts, shown in the status bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderingInfo {
    /// Successful attempt (`None` if not determined, for example in tests).
    pub active: Option<Attempt>,
    /// Description of the adapter or of the GL context.
    pub detail: String,
    /// The effective adapter is CPU-type (even if chosen as hardware).
    pub cpu_adapter: bool,
    /// Attempts that failed before the active one, with the reason.
    pub failed: Vec<(Attempt, String)>,
    /// Choice forced on the command line.
    pub forced: bool,
}

impl Default for RenderingInfo {
    fn default() -> Self {
        Self {
            active: None,
            detail: "not determined".into(),
            cpu_adapter: false,
            failed: Vec::new(),
            forced: false,
        }
    }
}

impl RenderingInfo {
    /// Degraded mode: software rasterizer.
    pub fn degraded(&self) -> bool {
        self.cpu_adapter || self.active.is_some_and(Attempt::degraded)
    }

    /// Main text of the indicator.
    pub fn label(&self) -> String {
        format!("Rendering: {}", self.backend())
    }

    /// Backend in use and how it was chosen, without prefix.
    pub fn backend(&self) -> String {
        let base = self.active.map_or("not determined", Attempt::label);
        let origin = match (self.active, self.forced) {
            (None, _) => "",
            (Some(_), true) => " · forced",
            (Some(_), false) if self.failed.is_empty() => " · automatic",
            (Some(_), false) => " · automatic fallback",
        };
        format!("{base}{origin}")
    }

    /// Full explanation (hover tooltip and log).
    pub fn explanation(&self) -> String {
        let mut s = self.detail.clone();
        for (t, reason) in &self.failed {
            s.push_str(&format!("\nattempt {} failed: {reason}", t.name()));
        }
        if self.degraded() {
            s.push_str(&format!(
                "\ndegraded mode: software rendering, high CPU, console limited to {DEGRADED_CONSOLE_FPS} updates/s"
            ));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(feature = "glow", feature = "wgpu"))]
    #[test]
    fn auto_follows_glow_wgpu_warp_order() {
        let p = Plan::from_args(None, false).unwrap();
        assert_eq!(p.attempts, Attempt::AUTO.to_vec());
        assert!(!p.forced);
        let p = Plan::from_args(Some("glow,warp"), false).unwrap();
        assert_eq!(p.attempts, vec![Attempt::Glow, Attempt::WgpuSoftware]);
        assert!(!p.forced, "a list is a fallback order, not a forcing");
        assert!(Plan::from_args(Some("glow"), false).unwrap().forced);
    }

    #[cfg(feature = "wgpu")]
    #[test]
    fn wgpu_adapter_cpu_forces_warp() {
        let p = Plan::from_args(None, true).unwrap();
        assert_eq!(p.attempts, vec![Attempt::WgpuSoftware]);
        assert!(p.forced);
        let p = Plan::from_args(Some("wgpu"), true).unwrap();
        assert_eq!(p.attempts, vec![Attempt::WgpuSoftware]);
    }

    #[test]
    fn invalid_names_rejected() {
        assert!(Plan::from_args(Some("vulkan"), false).is_err());
        assert!(Plan::from_args(Some("glow,glow"), false).is_err());
        assert!(Plan::from_args(Some(""), false).is_err());
    }

    #[test]
    fn indicator_reports_fallback_and_degradation() {
        let info = RenderingInfo {
            active: Some(Attempt::WgpuSoftware),
            detail: "adapter WARP".into(),
            cpu_adapter: true,
            failed: vec![(Attempt::Glow, "opengl 2.0+".into())],
            forced: false,
        };
        assert!(info.degraded());
        assert!(info.label().contains("automatic fallback"));
        assert!(info.explanation().contains("attempt glow failed"));
        assert!(info.explanation().contains("degraded mode"));
        assert!(!RenderingInfo::default().degraded());
    }
}
