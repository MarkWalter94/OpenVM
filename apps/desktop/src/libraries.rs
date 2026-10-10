//! F1-14: Linux libraries loaded with `dlopen` and turning the
//! `xkbcommon-dl` panic (D-17) into a usable error.
//!
//! winit, glutin and wgpu do not link X11, Wayland, xkbcommon, EGL/GL and
//! Vulkan into the binary: they open them at runtime. If a library is missing,
//! `xkbcommon-dl` panics (`Library libxkbcommon-x11.so could not be loaded.`)
//! instead of returning an error.
//!
//! Two defences, without `unsafe` code:
//!
//! 1. **preflight check** before creating the window: looks for the library
//!    names in the dynamic linker cache (`/etc/ld.so.cache`), in the
//!    `LD_LIBRARY_PATH` directories and in the default directories, that is
//!    in the same places `dlopen` looks;
//! 2. **`catch_unwind`** around the eframe start in `main.rs`: a panic during
//!    the creation of the event loop becomes a message listing the missing
//!    libraries and the packages to install.
//!
//! The analysis logic is pure and is tested on every platform; reading the
//! file system is used only on Linux.

/// Graphical session detected from the environment variables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    /// `WAYLAND_DISPLAY` set (winit prefers Wayland).
    Wayland,
    /// Only `DISPLAY` set.
    X11,
    /// No graphical session.
    NoSession,
}

impl Session {
    /// Detects the session the way winit does (Wayland before X11).
    pub fn detect(wayland_display: Option<&str>, display: Option<&str>) -> Self {
        let set = |v: Option<&str>| v.is_some_and(|s| !s.is_empty());
        if set(wayland_display) {
            Self::Wayland
        } else if set(display) {
            Self::X11
        } else {
            Self::NoSession
        }
    }
}

/// Library loaded at runtime, with the Debian/Ubuntu package providing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Library {
    /// Name of the file opened with `dlopen`.
    pub file: &'static str,
    /// Debian/Ubuntu package.
    pub package: &'static str,
    /// What it is used for.
    pub purpose: &'static str,
}

const XKB: Library = Library {
    file: "libxkbcommon.so.0",
    package: "libxkbcommon0",
    purpose: "keyboard (xkbcommon)",
};
const XKB_X11: Library = Library {
    file: "libxkbcommon-x11.so.0",
    package: "libxkbcommon-x11-0",
    purpose: "keyboard on X11",
};
const X11: Library = Library {
    file: "libX11.so.6",
    package: "libx11-6",
    purpose: "X11 window",
};
const X11_XCB: Library = Library {
    file: "libX11-xcb.so.1",
    package: "libx11-xcb1",
    purpose: "X11 window (xcb)",
};
const XCURSOR: Library = Library {
    file: "libXcursor.so.1",
    package: "libxcursor1",
    purpose: "X11 cursors",
};
const XI: Library = Library {
    file: "libXi.so.6",
    package: "libxi6",
    purpose: "X11 input",
};
const WAYLAND: Library = Library {
    file: "libwayland-client.so.0",
    package: "libwayland-client0",
    purpose: "Wayland window",
};
const EGL: Library = Library {
    file: "libEGL.so.1",
    package: "libegl1",
    purpose: "OpenGL/EGL (glow, wgpu GL)",
};
const GL: Library = Library {
    file: "libGL.so.1",
    package: "libgl1",
    purpose: "OpenGL/GLX (glow on X11)",
};
const VULKAN: Library = Library {
    file: "libvulkan.so.1",
    package: "libvulkan1",
    purpose: "Vulkan (wgpu)",
};

/// Libraries required for the session, grouped: each group is satisfied if
/// **at least one** of its libraries is present.
pub fn required_groups(session: Session) -> Vec<Vec<Library>> {
    let mut g = vec![vec![XKB]];
    match session {
        Session::Wayland => g.push(vec![WAYLAND]),
        Session::X11 => {
            for l in [XKB_X11, X11, X11_XCB, XCURSOR, XI] {
                g.push(vec![l]);
            }
        }
        Session::NoSession => {}
    }
    // At least one graphics API: EGL or GL for glow, Vulkan (or EGL) for wgpu.
    g.push(vec![EGL, GL, VULKAN]);
    g
}

/// Unsatisfied groups, given a presence predicate.
pub fn missing(session: Session, present: impl Fn(&str) -> bool) -> Vec<Vec<Library>> {
    required_groups(session)
        .into_iter()
        .filter(|g| !g.iter().any(|l| present(l.file)))
        .collect()
}

/// Error message for the user.
pub fn message(session: Session, missing: &[Vec<Library>], cause: Option<&str>) -> String {
    let mut s = String::from(
        "LightVM cannot open the window: system libraries loaded at runtime are missing.\n",
    );
    s.push_str(&format!("Detected graphical session: {session:?}.\n"));
    if missing.is_empty() {
        s.push_str("The preflight check found no missing libraries; check anyway:\n");
        for g in required_groups(session) {
            s.push_str(&group_line(&g));
        }
    } else {
        s.push_str("Missing libraries:\n");
        for g in missing {
            s.push_str(&group_line(g));
        }
        let packages: Vec<&str> = missing
            .iter()
            .filter_map(|g| g.first().map(|l| l.package))
            .collect();
        s.push_str(&format!(
            "On Debian/Ubuntu: sudo apt install {}\n",
            packages.join(" ")
        ));
    }
    if session == Session::NoSession {
        s.push_str("No graphical session: neither WAYLAND_DISPLAY nor DISPLAY is set.\n");
    }
    if let Some(c) = cause {
        s.push_str(&format!("Technical detail: {c}\n"));
    }
    s
}

fn group_line(g: &[Library]) -> String {
    let alternatives: Vec<String> = g
        .iter()
        .map(|l| format!("{} (package {}, {})", l.file, l.package, l.purpose))
        .collect();
    format!("  - {}\n", alternatives.join(" or "))
}

/// The panic message concerns a library loaded at runtime.
pub fn is_library_panic(msg: &str) -> bool {
    msg.contains("could not be loaded")
        || msg.contains("xkbcommon")
        || msg.contains("cannot open shared object")
        || msg.contains("Failed to open")
}

/// Directories where `dlopen` looks besides the cache (ld.so(8)).
#[cfg(target_os = "linux")]
const DEFAULT_DIRS: &[&str] = &[
    "/lib",
    "/usr/lib",
    "/lib64",
    "/usr/lib64",
    "/lib/x86_64-linux-gnu",
    "/usr/lib/x86_64-linux-gnu",
    "/usr/local/lib",
];

/// Presence of a library on the current system (Linux only).
#[cfg(target_os = "linux")]
pub fn present_on_system(cache: &[u8], file: &str) -> bool {
    use std::path::Path;
    // The ld.so cache contains the names as NUL-terminated strings.
    let mut needle = file.as_bytes().to_vec();
    needle.push(0);
    let in_cache = cache.windows(needle.len()).any(|w| w == needle.as_slice());
    if in_cache {
        return true;
    }
    let ld = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
    ld.split(':')
        .filter(|d| !d.is_empty())
        .chain(DEFAULT_DIRS.iter().copied())
        .any(|d| Path::new(d).join(file).exists())
}

/// Preflight check before creating the window (Linux only).
/// `Err` with the message for the user if something essential is missing.
#[cfg(target_os = "linux")]
pub fn preflight_check() -> Result<Session, String> {
    let session = Session::detect(
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        std::env::var("DISPLAY").ok().as_deref(),
    );
    let cache = std::fs::read("/etc/ld.so.cache").unwrap_or_default();
    let m = missing(session, |f| present_on_system(&cache, f));
    if m.is_empty() && session != Session::NoSession {
        Ok(session)
    } else {
        Err(message(session, &m, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_session_wins_over_x11() {
        assert_eq!(
            Session::detect(Some("wayland-0"), Some(":0")),
            Session::Wayland
        );
        assert_eq!(Session::detect(Some(""), Some(":0")), Session::X11);
        assert_eq!(Session::detect(None, None), Session::NoSession);
    }

    #[test]
    fn x11_without_xkbcommon_x11_lists_the_package() {
        let present = [
            "libxkbcommon.so.0",
            "libX11.so.6",
            "libX11-xcb.so.1",
            "libXcursor.so.1",
            "libXi.so.6",
            "libvulkan.so.1",
        ];
        let m = missing(Session::X11, |f| present.contains(&f));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0][0].file, "libxkbcommon-x11.so.0");
        let msg = message(Session::X11, &m, Some("panic"));
        assert!(msg.contains("libxkbcommon-x11-0"));
        assert!(msg.contains("sudo apt install libxkbcommon-x11-0"));
        assert!(msg.contains("Technical detail: panic"));
    }

    #[test]
    fn without_any_library_lists_those_required_by_the_roadmap() {
        let m = missing(Session::Wayland, |_| false);
        let msg = message(Session::Wayland, &m, None);
        for p in [
            "libxkbcommon0",
            "libwayland-client0",
            "libegl1",
            "libgl1",
            "libvulkan1",
        ] {
            assert!(msg.contains(p), "{p} missing from the message");
        }
    }

    #[test]
    fn one_graphics_api_is_enough() {
        let present = ["libxkbcommon.so.0", "libwayland-client.so.0", "libEGL.so.1"];
        assert!(missing(Session::Wayland, |f| present.contains(&f)).is_empty());
    }

    #[test]
    fn recognises_the_xkbcommon_dl_panic() {
        assert!(is_library_panic(
            "Library libxkbcommon-x11.so could not be loaded."
        ));
        assert!(!is_library_panic("index out of bounds"));
    }
}
