//! The config file: `$XDG_CONFIG_HOME/xross/config.toml` (or `--config PATH`).
//!
//! Every setting is optional; see `resources/config.example.toml` for all of them with
//! their defaults. The file is re-read when it changes. Tuning values (springs, gaps,
//! sensitivity, colours) apply immediately; binds and touchpad settings are recompiled
//! and reapplied; startup commands only run at startup.
//!
//! Tuning values are read through [`current`], so the pure modules (springs, swipe
//! tracking, the snap combo) don't need the config threaded through every call.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use serde::Deserialize;

use crate::anim::spring::SpringParams;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub startup: Startup,
    pub output: OutputConfig,
    pub xwayland: Xwayland,
    pub layout: Layout,
    pub gestures: Gestures,
    pub animations: Animations,
    pub overview: Overview,
    pub touchpad: Touchpad,
    pub appearance: Appearance,
    /// Extra keybinds, `"Mod+Return" = "spawn foot"`. They take precedence over the
    /// built-in ones; `"none"` disables a built-in bind.
    pub binds: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    /// What Ctrl+` (and `spawn-terminal`) runs, through `sh -c`.
    pub terminal: String,
    /// "dev" (Alt-based) or "prod" (Super-based). Unset: dev nested, prod on a TTY.
    /// `--binds` on the command line wins.
    pub binds: Option<String>,
}

impl Default for General {
    fn default() -> Self {
        Self { terminal: "foot".into(), binds: None }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Startup {
    /// Run once, through `sh -c`, after Xwayland is up (e.g. a wallpaper or a bar).
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    /// Scale factor, fractional allowed (1.25, 1.5, 2...). Unset: 1. `--scale` wins.
    pub scale: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Xwayland {
    /// Don't scale X11 apps: show their pixels 1:1 (sharp at fractional scales) and have
    /// them scale themselves via `Xft.dpi` (most toolkits do; very old apps look small).
    pub native_scaling: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Layout {
    /// Gap around and between snapped windows (logical px).
    pub gaps: i32,
    /// How quickly a second Mod+arrow must follow the first to make a quadrant.
    pub snap_combo_ms: u64,
}

impl Default for Layout {
    fn default() -> Self {
        Self { gaps: 0, snap_combo_ms: 500 }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Gestures {
    /// Finger travel (touchpad units) for one whole workspace; bigger is slower.
    pub swipe_px_per_workspace: f64,
}

impl Default for Gestures {
    fn default() -> Self {
        Self { swipe_px_per_workspace: 500.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Spring {
    pub stiffness: f64,
    /// 1.0 is critically damped; lower bounces, higher is sluggish.
    pub damping: f64,
}

impl Spring {
    const fn new(stiffness: f64) -> Self {
        Self { stiffness, damping: 1.0 }
    }
}

impl Default for Spring {
    fn default() -> Self {
        Self::new(800.0)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Animations {
    /// Off: everything snaps into place instantly.
    pub enabled: bool,
    /// Sliding between workspaces.
    pub workspace: Spring,
    /// Snapping, maximizing, restoring.
    pub window: Spring,
    /// New windows appearing.
    pub open: Spring,
    /// Opening and closing the overview.
    pub overview: Spring,
    /// Panning and settling inside the overview.
    pub pan: Spring,
}

impl Default for Animations {
    fn default() -> Self {
        Self {
            enabled: true,
            workspace: Spring::new(900.0),
            window: Spring::new(700.0),
            open: Spring::new(900.0),
            overview: Spring::new(800.0),
            pan: Spring::new(600.0),
        }
    }
}

impl Animations {
    /// Spring parameters, or an effectively instant spring when animations are off.
    pub fn params(&self, spring: Spring, epsilon: f64) -> SpringParams {
        if self.enabled {
            SpringParams::new(spring.damping.max(0.05), spring.stiffness.max(1.0), epsilon)
        } else {
            SpringParams::new(1.0, 1e9, epsilon)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Overview {
    /// Workspace size in the overview, relative to the screen.
    pub scale: f64,
}

impl Default for Overview {
    fn default() -> Self {
        Self { scale: 0.25 }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Touchpad {
    pub tap: bool,
    pub natural_scroll: bool,
}

impl Default for Touchpad {
    fn default() -> Self {
        Self { tap: true, natural_scroll: true }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    /// Behind everything (and between workspaces while they slide), as `#rrggbb`.
    pub background: String,
    /// For apps that can draw their own title bar or use ours: use ours. Apps that only
    /// do one or the other always get that.
    pub server_decorations: bool,
    /// Corner radius of windows with our title bar (logical px); 0 for square.
    pub corner_radius: f64,
    /// Soft shadows behind windows with our title bar.
    pub shadows: bool,
    /// Shadow colour, `#rrggbbaa`.
    pub shadow_color: String,
    /// How far shadows spread (logical px); bigger is softer.
    pub shadow_size: f64,
    /// Freedesktop icon theme for the title bar buttons.
    pub icon_theme: String,
    /// Font for title bars and the window switcher: a fontconfig name or pattern.
    pub font: String,
    /// Title text size (logical px).
    pub font_size: f64,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            background: "#141419".into(),
            server_decorations: true,
            corner_radius: 10.0,
            shadows: true,
            shadow_color: "#00000066".into(),
            shadow_size: 40.0,
            icon_theme: "Adwaita".into(),
            font: "NotoSans Nerd Font".into(),
            font_size: 12.5,
        }
    }
}

impl Config {
    pub fn snap_combo_timeout(&self) -> Duration {
        Duration::from_millis(self.layout.snap_combo_ms)
    }

    /// The background as RGBA, falling back to the default if it doesn't parse.
    pub fn background(&self) -> [f32; 4] {
        parse_color(&self.appearance.background).unwrap_or([0.08, 0.08, 0.10, 1.0])
    }

    /// Settings that would silently misbehave, reported as warnings.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if parse_color(&self.appearance.background).is_none() {
            out.push(format!("appearance.background: '{}' is not #rrggbb", self.appearance.background));
        }
        if parse_color(&self.appearance.shadow_color).is_none() {
            out.push(format!("appearance.shadow_color: '{}' is not #rrggbb(aa)", self.appearance.shadow_color));
        }
        if !(0.0..=64.0).contains(&self.appearance.corner_radius) {
            out.push(format!("appearance.corner_radius: {} is outside 0..64", self.appearance.corner_radius));
        }
        if !(0.05..=1.0).contains(&self.overview.scale) {
            out.push(format!("overview.scale: {} is outside 0.05..1.0", self.overview.scale));
        }
        if self.gestures.swipe_px_per_workspace <= 0.0 {
            out.push("gestures.swipe_px_per_workspace must be positive".into());
        }
        if let Some(s) = self.output.scale
            && !(0.5..=4.0).contains(&s)
        {
            out.push(format!("output.scale: {s} is outside 0.5..4.0"));
        }
        if self.layout.gaps < 0 {
            out.push("layout.gaps must not be negative".into());
        }
        if let Some(b) = &self.general.binds
            && b != "dev"
            && b != "prod"
        {
            out.push(format!("general.binds: '{b}' is not dev or prod"));
        }
        out
    }
}

/// `#rrggbb` or `#rrggbbaa`.
pub fn parse_color(s: &str) -> Option<[f32; 4]> {
    let hex = s.strip_prefix('#')?;
    if !matches!(hex.len(), 6 | 8) || !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(|v| v as f32 / 255.0);
    let a = if hex.len() == 8 { byte(6)? } else { 1.0 };
    Some([byte(0)?, byte(2)?, byte(4)?, a])
}

// --- Loading -------------------------------------------------------------------

/// Where the config lives unless `--config` says otherwise.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("xross").join("config.toml"))
}

/// Read and parse a config file. A missing file is the default config.
pub fn load(path: &Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

thread_local! {
    static CURRENT: RefCell<Rc<Config>> = RefCell::new(Rc::new(Config::default()));
}

/// The active config.
pub fn current() -> Rc<Config> {
    CURRENT.with(|c| c.borrow().clone())
}

pub fn set_current(config: Config) {
    CURRENT.with(|c| *c.borrow_mut() = Rc::new(config));
}

// --- Applying and watching -------------------------------------------------------

/// How often the config file is checked for changes.
const WATCH_INTERVAL: Duration = Duration::from_secs(1);

/// File identity for change detection: (modified time, length).
fn stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

impl crate::state::XrossState {
    /// Load the config at startup (falling back to defaults on errors) and start
    /// watching it for changes.
    pub fn init_config(&mut self, path: Option<PathBuf>) {
        let config = match path.as_deref().map(load) {
            Some(Ok(c)) => c,
            Some(Err(err)) => {
                tracing::warn!(%err, "config error; using defaults");
                Config::default()
            }
            None => Config::default(),
        };
        if let Some(p) = &path {
            tracing::info!(path = %p.display(), exists = p.exists(), "config");
        }
        self.config_stamp = path.as_deref().and_then(stamp);
        self.config_path = path;
        self.pending_startup_commands.extend(config.startup.commands.iter().cloned());
        self.apply_config(config);

        if self.config_path.is_some() {
            let res = self.loop_handle.insert_source(
                smithay::reexports::calloop::timer::Timer::from_duration(WATCH_INTERVAL),
                |_, _, state| {
                    state.check_config();
                    smithay::reexports::calloop::timer::TimeoutAction::ToDuration(WATCH_INTERVAL)
                },
            );
            if let Err(err) = res {
                tracing::warn!(%err, "can't watch the config file");
            }
        }
    }

    /// Re-read the config if the file changed. Errors keep the current config.
    fn check_config(&mut self) {
        let Some(path) = self.config_path.clone() else { return };
        let now = stamp(&path);
        if now == self.config_stamp {
            return;
        }
        self.config_stamp = now;
        match load(&path) {
            Ok(config) => {
                tracing::info!("config reloaded");
                self.apply_config(config);
            }
            Err(err) => tracing::warn!(%err, "config error; keeping the previous config"),
        }
    }

    /// The output scale: `--scale`, else the config's `output.scale`, else 1.
    pub fn output_scale(&self) -> f64 {
        let s = self.scale_override.or(current().output.scale).unwrap_or(1.0);
        if (0.5..=4.0).contains(&s) { s } else { 1.0 }
    }

    /// Bring every output to [`Self::output_scale`], telling surfaces their new
    /// preferred scale (fractional-scale and wl_surface.preferred_buffer_scale).
    pub fn apply_output_scale(&mut self) {
        use smithay::{
            desktop::layer_map_for_output,
            output::Scale as OutputScale,
            reexports::wayland_server::protocol::wl_surface::WlSurface,
            wayland::{
                compositor::{SurfaceData, send_surface_state},
                fractional_scale::with_fractional_scale,
            },
        };
        let scale = self.output_scale();
        let outputs: Vec<_> = self.space.outputs().cloned().collect();
        for output in outputs {
            if (output.current_scale().fractional_scale() - scale).abs() < 1e-9 {
                continue;
            }
            output.change_current_state(None, None, Some(OutputScale::Fractional(scale)), None);
            tracing::info!(scale, output = output.name(), "output scale");
            let transform = output.current_transform();
            let update = |surface: &WlSurface, data: &SurfaceData| {
                with_fractional_scale(data, |fs| fs.set_preferred_scale(scale));
                send_surface_state(surface, data, scale.ceil() as i32, transform);
            };
            for window in self.space.elements() {
                window.with_surfaces(update);
            }
            let mut layers = layer_map_for_output(&output);
            for layer in layers.layers() {
                layer.with_surfaces(update);
            }
            // Panels re-arrange for the new logical size; snapped windows follow on the
            // next frame (the usable area changed).
            layers.arrange();
        }
    }

    /// Make `config` current: profile, binds, touchpads, layout.
    pub fn apply_config(&mut self, config: Config) {
        for problem in config.problems() {
            tracing::warn!("config: {problem}");
        }
        let profile = self
            .binds_override
            .or_else(|| config.general.binds.as_deref().and_then(|b| b.parse().ok()))
            .unwrap_or(self.default_profile);
        if profile != self.bind_profile {
            tracing::info!(?profile, "bind profile changed");
        }
        self.bind_profile = profile;

        let (binds, errors) = crate::input::user_binds::compile(&config.binds, profile);
        for err in errors {
            tracing::warn!("config: {err}");
        }
        self.user_binds = binds;
        let touchpad = config.touchpad.clone();
        set_current(config);
        self.apply_output_scale();
        self.apply_xwayland_scale();

        if let Some(udev) = self.udev.as_ref() {
            udev.borrow_mut().reconfigure_input(&touchpad);
        }
        // Gaps may have changed.
        self.relayout();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../resources/config.example.toml");

    #[test]
    fn empty_file_is_the_default() {
        assert_eq!(toml::from_str::<Config>("").unwrap(), Config::default());
    }

    #[test]
    fn example_documents_the_defaults() {
        // Every value in the example is the default; only the binds are illustrative.
        let mut example: Config = toml::from_str(EXAMPLE).unwrap();
        assert!(!example.binds.is_empty(), "the example shows some binds");
        example.binds.clear();
        assert_eq!(example, Config::default());
    }

    #[test]
    fn partial_sections() {
        let c: Config = toml::from_str("[layout]\ngaps = 8\n[animations.window]\nstiffness = 300").unwrap();
        assert_eq!(c.layout.gaps, 8);
        assert_eq!(c.layout.snap_combo_ms, 500, "unset keys keep their defaults");
        assert_eq!(c.animations.window, Spring { stiffness: 300.0, damping: 1.0 });
    }

    #[test]
    fn unknown_keys_are_errors() {
        assert!(toml::from_str::<Config>("[layout]\ngap = 8").is_err(), "typos shouldn't pass silently");
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#ff0000"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_color("#00000080").map(|c| (c[3] * 255.0).round()), Some(128.0));
        assert_eq!(parse_color("ff0000"), None);
        assert_eq!(parse_color("#12345"), None);
    }

    #[test]
    fn problems_are_reported() {
        let c: Config = toml::from_str("[appearance]\nbackground = \"blue\"\n[overview]\nscale = 2.0").unwrap();
        assert_eq!(c.problems().len(), 2);
        assert!(Config::default().problems().is_empty());
    }

    #[test]
    fn disabled_animations_are_instant() {
        let a = Animations { enabled: false, ..Default::default() };
        assert!(a.params(a.window, 0.001).stiffness > 1e8);
    }
}
