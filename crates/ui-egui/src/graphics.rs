//! Graphics backend selection for the wgpu renderer.

use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup};
use eframe::wgpu::Backends;

/// The operating systems whose backend choices differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Os {
    Windows,
    Mac,
    Linux,
}

impl Os {
    pub(crate) const CURRENT: Self = if cfg!(windows) {
        Self::Windows
    } else if cfg!(target_os = "macos") {
        Self::Mac
    } else {
        Self::Linux
    };
}

/// The user's graphics backend choice, applied at the next start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum GraphicsBackend {
    #[default]
    Auto,
    Dx12,
    Vulkan,
    OpenGl,
    Metal,
}

/// Choices offered on an operating system.
///
/// macOS offers only Metal: wgpu's OpenGL backend there needs ANGLE, which this build lacks.
pub(crate) fn available_backends(os: Os) -> &'static [GraphicsBackend] {
    use GraphicsBackend::*;
    match os {
        Os::Windows => &[Auto, Dx12, Vulkan, OpenGl],
        Os::Mac => &[Auto, Metal],
        Os::Linux => &[Auto, Vulkan, OpenGl],
    }
}

impl GraphicsBackend {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Dx12 => "DX12",
            Self::Vulkan => "Vulkan",
            Self::OpenGl => "OpenGL",
            Self::Metal => "Metal",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dx12 => "dx12",
            Self::Vulkan => "vulkan",
            Self::OpenGl => "opengl",
            Self::Metal => "metal",
        }
    }

    /// A saved name that is not offered on `os` is treated as Auto so the app can always start.
    fn from_key(os: Os, key: &str) -> Self {
        available_backends(os)
            .iter()
            .copied()
            .find(|choice| choice.key() == key)
            .unwrap_or_default()
    }

    /// Backends to request. `None` keeps wgpu's own default.
    ///
    /// Auto selects DX12 on Windows: some Intel iGPU Vulkan drivers crash during
    /// initialisation. Other platforms keep wgpu's defaults. Any explicit choice on
    /// macOS uses Metal.
    fn backends(self, os: Os) -> Option<Backends> {
        match (self, os) {
            (Self::Auto, Os::Windows) => Some(Backends::DX12),
            (Self::Auto, _) => None,
            (_, Os::Mac) | (Self::Metal, _) => Some(Backends::METAL),
            (Self::Dx12, _) => Some(Backends::DX12),
            (Self::Vulkan, _) => Some(Backends::VULKAN),
            (Self::OpenGl, _) => Some(Backends::GL),
        }
    }

    fn path() -> Option<std::path::PathBuf> {
        Some(
            tradecraft_providers::settings::data_dir()?
                .join(build_files(cfg!(feature = "vibestrader-credential")).0),
        )
    }

    pub(crate) fn load() -> Self {
        Self::path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|data| serde_json::from_slice::<serde_json::Value>(&data).ok())
            .and_then(|value| {
                value["backend"]
                    .as_str()
                    .map(|key| Self::from_key(Os::CURRENT, key))
            })
            .unwrap_or_default()
    }

    pub(crate) fn save(self) -> Result<(), String> {
        let path = Self::path().ok_or("Settings folder is unavailable")?;
        let parent = path.parent().ok_or("Settings folder is unavailable")?;
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not save graphics backend: {e}"))?;
        let data = serde_json::json!({"backend": self.key()});
        crate::terminal_storage::write_atomic(&path, &data.to_string())
            .map_err(|e| format!("Could not save graphics backend: {e}"))
    }
}

/// The settings and crash-marker file names of a build, kept apart so that neither build
/// reads or clears the other's marker.
fn build_files(personal: bool) -> (&'static str, &'static str) {
    if personal {
        ("graphics-personal.json", "graphics-personal.marker")
    } else {
        ("graphics-community.json", "graphics-community.marker")
    }
}

/// Backends to request: `WGPU_BACKEND` first, then the saved choice.
fn requested_backends(env: Option<Backends>, choice: GraphicsBackend, os: Os) -> Option<Backends> {
    if env.is_some() {
        return None;
    }
    choice.backends(os)
}

fn backend_key(backends: Option<Backends>) -> &'static str {
    match backends {
        Some(b) if b == Backends::DX12 => "dx12",
        Some(b) if b == Backends::VULKAN => "vulkan",
        Some(b) if b == Backends::GL => "opengl",
        Some(b) if b == Backends::METAL => "metal",
        _ => "default",
    }
}

/// After a start that never finished initialising graphics, try DX12 and then OpenGL.
/// The backend that failed is never chosen again by this fallback. Windows only; `None`
/// means no fallback applies.
fn fallback_after(previous_attempt: &str) -> Option<Backends> {
    if !cfg!(windows) {
        return None;
    }
    match previous_attempt {
        "dx12" => Some(Backends::GL),
        _ => Some(Backends::DX12),
    }
}

/// The backends this start will request, and whether a crash fallback chose them.
fn plan(
    env: Option<Backends>,
    choice: GraphicsBackend,
    previous_attempt: Option<&str>,
) -> (Option<Backends>, bool) {
    if env.is_some() {
        return (None, false);
    }
    match previous_attempt.and_then(fallback_after) {
        Some(backends) => (Some(backends), true),
        None => (requested_backends(env, choice, Os::CURRENT), false),
    }
}

fn marker_path() -> Option<std::path::PathBuf> {
    Some(
        tradecraft_providers::settings::data_dir()?
            .join(build_files(cfg!(feature = "vibestrader-credential")).1),
    )
}

/// The renderer configuration, with the backend request for the log.
///
/// A marker file is written before graphics initialise and removed once a few frames
/// have rendered. If it is still there at the next start, the previous start crashed
/// during graphics initialisation, so a safer backend is tried.
pub(crate) fn startup(choice: GraphicsBackend) -> WgpuConfiguration {
    let env = Backends::from_env();
    let previous = marker_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim().to_owned());
    if let Some(previous) = &previous {
        crate::startup_log::log(format!(
            "previous start did not finish initialising graphics (backend {previous})"
        ));
    }
    let (backends, fell_back) = plan(env, choice, previous.as_deref());
    if fell_back {
        crate::startup_log::log("automatic graphics fallback for this start only");
    }
    crate::startup_log::log(format!(
        "graphics backend requested: setting={}, WGPU_BACKEND={}, effective={}",
        choice.label(),
        std::env::var("WGPU_BACKEND").unwrap_or_else(|_| "unset".into()),
        match (env, backends) {
            (Some(env), _) => format!("{env:?}"),
            (None, Some(backends)) => format!("{backends:?}"),
            (None, None) => "wgpu default".into(),
        }
    ));
    if env.is_none()
        && let Some(path) = marker_path()
    {
        let _ = std::fs::write(path, backend_key(backends));
    }
    let mut options = WgpuConfiguration::default();
    if let Some(backends) = backends
        && let WgpuSetup::CreateNew(setup) = &mut options.wgpu_setup
    {
        setup.instance_descriptor.backends = backends;
    }
    options
}

static FRAMES: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Call once per frame; clears the crash marker after the second frame.
pub(crate) fn frame_finished() {
    use std::sync::atomic::Ordering;
    if FRAMES.load(Ordering::Relaxed) >= 2 {
        return;
    }
    if FRAMES.fetch_add(1, Ordering::Relaxed) == 1 {
        if let Some(path) = marker_path() {
            let _ = std::fs::remove_file(path);
        }
        crate::startup_log::log("graphics initialised: first frames rendered");
    }
}

/// Logs the adapter chosen by wgpu.
pub(crate) fn log_adapter(state: Option<&eframe::egui_wgpu::RenderState>) {
    let Some(state) = state else {
        crate::startup_log::log("graphics adapter: unavailable (no wgpu render state)");
        return;
    };
    let info = state.adapter.get_info();
    crate::startup_log::log(format!(
        "graphics adapter: {} | backend {:?} | type {:?} | driver {} {}",
        info.name, info.backend, info.device_type, info.driver, info.driver_info
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use GraphicsBackend::*;

    #[test]
    fn each_build_has_its_own_settings_and_marker() {
        let (personal_settings, personal_marker) = build_files(true);
        let (community_settings, community_marker) = build_files(false);
        assert_ne!(personal_settings, community_settings);
        assert_ne!(personal_marker, community_marker);
        assert!(personal_marker.starts_with("graphics-personal"));
        assert!(community_marker.starts_with("graphics-community"));
    }

    #[test]
    fn available_backends_depend_on_the_operating_system() {
        assert_eq!(
            available_backends(Os::Windows),
            &[Auto, Dx12, Vulkan, OpenGl]
        );
        assert_eq!(available_backends(Os::Mac), &[Auto, Metal]);
        assert_eq!(available_backends(Os::Linux), &[Auto, Vulkan, OpenGl]);
    }

    #[test]
    fn explicit_choices_use_metal_on_macos() {
        for choice in [Dx12, Vulkan, OpenGl, Metal] {
            assert_eq!(choice.backends(Os::Mac), Some(Backends::METAL));
        }
        assert_eq!(Auto.backends(Os::Mac), None);
    }

    #[test]
    fn saved_opengl_is_ignored_on_macos() {
        assert_eq!(GraphicsBackend::from_key(Os::Mac, "opengl"), Auto);
        assert_eq!(GraphicsBackend::from_key(Os::Mac, "vulkan"), Auto);
        assert_eq!(GraphicsBackend::from_key(Os::Mac, "metal"), Metal);
        assert_eq!(GraphicsBackend::from_key(Os::Windows, "opengl"), OpenGl);
        assert_eq!(GraphicsBackend::from_key(Os::Linux, "dx12"), Auto);
    }

    #[test]
    fn environment_variable_overrides_setting_and_default() {
        let os = Os::Windows;
        assert_eq!(requested_backends(Some(Backends::VULKAN), OpenGl, os), None);
        assert_eq!(requested_backends(None, Auto, os), Some(Backends::DX12));
        assert_eq!(requested_backends(None, Vulkan, os), Some(Backends::VULKAN));
        assert_eq!(requested_backends(None, OpenGl, os), Some(Backends::GL));
        assert_eq!(requested_backends(None, Auto, Os::Linux), None);
    }

    #[test]
    fn saved_names_round_trip_and_unknown_names_fall_back_to_auto() {
        for os in [Os::Windows, Os::Mac, Os::Linux] {
            for choice in available_backends(os) {
                assert_eq!(GraphicsBackend::from_key(os, choice.key()), *choice);
            }
            assert_eq!(GraphicsBackend::from_key(os, "nonsense"), Auto);
        }
    }

    #[test]
    fn fallback_never_repeats_the_backend_that_failed() {
        if !cfg!(windows) {
            return;
        }
        // The saved setting is the backend that failed: the fallback must not reuse it.
        assert_eq!(
            plan(None, OpenGl, Some("opengl")),
            (Some(Backends::DX12), true)
        );
        assert_eq!(plan(None, Dx12, Some("dx12")), (Some(Backends::GL), true));
        assert_eq!(
            plan(None, Vulkan, Some("vulkan")),
            (Some(Backends::DX12), true)
        );
    }

    #[test]
    fn crash_fallback_tries_dx12_then_opengl_on_windows() {
        let auto = Auto;
        assert_eq!(plan(None, auto, None), (auto.backends(Os::CURRENT), false));
        if cfg!(windows) {
            assert_eq!(
                plan(None, auto, Some("vulkan")),
                (Some(Backends::DX12), true)
            );
            assert_eq!(plan(None, auto, Some("dx12")), (Some(Backends::GL), true));
        } else {
            assert_eq!(
                plan(None, auto, Some("dx12")),
                (auto.backends(Os::CURRENT), false)
            );
        }
        assert_eq!(
            plan(Some(Backends::VULKAN), auto, Some("dx12")),
            (None, false)
        );
    }
}
