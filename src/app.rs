//! The app window. Engine, hotkeys and tray live in other modules; this file
//! is the UI plus the glue that keeps it in step with them. The footer with
//! the auto-update toggle and version is required on every factory app.

use crate::control::{self, GLOBAL_ID, Shared};
use crate::engine::{self, Button, Config, Mode, Reason, Runner, Step, StepAction};
use crate::keys::{self, Hotkey, MOD_ALT, MOD_CONTROL, MOD_SHIFT};
use crate::profiles::{self, Profile};
use crate::shots::{self, Shots};
use crate::theme::{self, Icon, Motif, Palette};
use crate::{license, winsys};
use eframe::egui;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;

const WIDTH: f32 = 500.0;
const HEIGHT: f32 = 766.0;
/// Scenes set up by `App::show_scene` for the Store screenshots.
const SHOT_SCENES: usize = 4;
/// Width of the main button.
const PRIMARY_WIDTH: f32 = 160.0;

pub fn run(args: Vec<String>) -> eframe::Result<()> {
    let shots_mode = Shots::from_args(&args).is_some();
    let settings = if shots_mode { crate::settings::Settings::default() } else { crate::settings::load() };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Auto Clicker - Mouse & Key Presser")
        .with_inner_size([WIDTH, HEIGHT])
        .with_min_inner_size([460.0, 560.0]);
    if settings.always_on_top {
        viewport = viewport.with_always_on_top();
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Auto Clicker - Mouse & Key Presser",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, settings, &args)))),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Clicker,
    Sequence,
    Profiles,
    Settings,
}

const TABS: [(Tab, &str); 4] = [
    (Tab::Clicker, "Clicker"),
    (Tab::Sequence, "Sequence"),
    (Tab::Profiles, "Profiles"),
    (Tab::Settings, "Settings"),
];

/// Which hotkey the next key press should rebind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Capture {
    Global,
    Profile(usize),
}

/// Where a picked screen point should go.
#[derive(Clone, Copy)]
enum PickFor {
    Single,
    Step(usize),
}

/// Where a run is, as the window shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Ready,
    Waiting,
    Running,
    Done(Reason),
}

#[derive(Clone, Copy)]
struct RunView {
    phase: Phase,
    count: u64,
    /// Seconds since the first action of the run.
    elapsed: f64,
}

pub struct App {
    settings: crate::settings::Settings,
    version: String,
    config: Config,
    profiles: Vec<Profile>,
    shared: Arc<Shared>,
    tab: Tab,
    capture: Option<Capture>,
    picking: Option<(PickFor, Receiver<Option<(i32, i32)>>)>,
    profile_name: String,
    message: Option<String>,
    licensed_email: Option<String>,
    key_input: String,
    dirty: bool,
    profiles_dirty: bool,
    last_hotkeys: Vec<(i32, Hotkey)>,
    /// "More options" is open.
    more_open: bool,
    run_started: Option<Instant>,
    last_elapsed: f64,
    /// Screenshot mode: nothing real runs and nothing is saved.
    shots: Option<Shots>,
    /// A made-up run state shown instead of the real one in screenshot mode.
    demo: Option<RunView>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, settings: crate::settings::Settings, args: &[String]) -> Self {
        let ctx = cc.egui_ctx.clone();
        theme::install_fonts(&ctx);
        theme::apply(&ctx);
        if let Some(t) = shots::theme_from_args(args) {
            ctx.set_theme(t);
        }
        let shots = Shots::from_args(args);
        let live = shots.is_none();
        let repaint_ctx = ctx.clone();
        let on_change: Arc<dyn Fn(bool) + Send + Sync> = Arc::new(move |running| {
            winsys::set_tray_running(running);
            repaint_ctx.request_repaint();
        });
        let runner = Runner::new(Arc::new(winsys::SystemInput::new()), on_change);
        let licensed_email = license::verify(&settings.license_key);
        let config = settings.config.clone();
        let profiles = if live { profiles::load() } else { Vec::new() };
        let shared = Arc::new(Shared {
            runner,
            config: std::sync::Mutex::new(config.clone()),
            profiles: std::sync::Mutex::new(profiles.clone()),
            external_change: Default::default(),
            licensed: std::sync::atomic::AtomicBool::new(licensed_email.is_some()),
            error: Default::default(),
        });
        if live {
            let handler_shared = shared.clone();
            let handler_ctx = ctx.clone();
            winsys::start(move |ev| {
                handler_shared.handle(ev);
                handler_ctx.request_repaint();
            });
        }
        let tab = match config.mode {
            Mode::Single => Tab::Clicker,
            Mode::Sequence => Tab::Sequence,
        };
        Self {
            version: crate::update::current_version(),
            key_input: settings.license_key.clone(),
            settings,
            config,
            profiles,
            shared,
            tab,
            capture: None,
            picking: None,
            profile_name: String::new(),
            message: None,
            licensed_email,
            dirty: false,
            profiles_dirty: false,
            last_hotkeys: Vec::new(),
            more_open: false,
            run_started: None,
            last_elapsed: 0.0,
            shots,
            demo: None,
        }
    }

    fn licensed(&self) -> bool {
        self.licensed_email.is_some()
    }

    fn live(&self) -> bool {
        self.shots.is_none()
    }

    fn save_settings(&self) {
        if self.live() {
            crate::settings::save(&self.settings);
        }
    }

    /// Made-up but believable states for the Store screenshots.
    fn show_scene(&mut self, scene: usize) {
        let clicker = Config {
            mode: Mode::Single,
            click_enabled: true,
            button: Button::Left,
            use_point: true,
            x: 1280,
            y: 642,
            key_enabled: true,
            key: 'E' as u32,
            interval_ms: 100,
            interval_variation_pct: 10,
            stop_after_clicks: Some(5000),
            ..Config::default()
        };
        let click = |x, y, delay_ms| Step {
            action: StepAction::Click { button: Button::Left, double: false, x, y },
            delay_ms,
        };
        let sequence = Config {
            mode: Mode::Sequence,
            steps: vec![
                click(1280, 642, 400),
                click(1516, 880, 250),
                Step { action: StepAction::Key { vk: 'E' as u32 }, delay_ms: 600 },
                Step {
                    action: StepAction::Click { button: Button::Right, double: true, x: 962, y: 540 },
                    delay_ms: 1200,
                },
            ],
            stop_after_clicks: None,
            stop_after_secs: Some(600),
            start_delay_ms: 3000,
            ..clicker.clone()
        };
        let ctrl = |vk| Some(Hotkey { mods: MOD_CONTROL, vk });
        self.profiles = vec![
            Profile { name: "Farm run".into(), config: clicker.clone(), hotkey: ctrl(0x70) },
            Profile { name: "Crafting loop".into(), config: sequence.clone(), hotkey: ctrl(0x71) },
            Profile {
                name: "Hold to mine".into(),
                config: Config {
                    click_enabled: true,
                    use_point: false,
                    key_enabled: true,
                    key: keys::VK_SPACE,
                    key_hold: true,
                    interval_ms: 250,
                    stop_after_clicks: None,
                    stop_after_secs: Some(1800),
                    ..clicker.clone()
                },
                hotkey: None,
            },
            Profile {
                name: "Fishing, right click".into(),
                config: Config {
                    button: Button::Right,
                    use_point: false,
                    key_enabled: false,
                    interval_ms: 2500,
                    stop_after_clicks: None,
                    ..clicker.clone()
                },
                hotkey: ctrl(0x72),
            },
        ];
        self.more_open = false;
        self.message = None;
        let view = |phase, count, elapsed| Some(RunView { phase, count, elapsed });
        match scene {
            0 => {
                self.config = clicker;
                self.tab = Tab::Clicker;
                self.demo = view(Phase::Ready, 0, 0.0);
            }
            1 => {
                self.config = clicker;
                self.tab = Tab::Clicker;
                self.demo = view(Phase::Running, 1284, 128.0);
            }
            2 => {
                self.config = sequence;
                self.tab = Tab::Sequence;
                self.demo = view(Phase::Running, 37, 222.0);
            }
            _ => {
                self.config = clicker;
                self.tab = Tab::Profiles;
                self.demo = view(Phase::Done(Reason::CountReached), 5000, 500.0);
            }
        }
    }

    /// Keep the hotkey thread's copy of the config in step with the window.
    fn sync(&mut self, ctx: &egui::Context) {
        let shared = self.shared.clone();
        if shared.external_change.swap(false, Ordering::SeqCst) {
            self.config = shared.config.lock().unwrap().clone();
            self.tab = match self.config.mode {
                Mode::Single if self.tab == Tab::Sequence => Tab::Clicker,
                Mode::Sequence if self.tab == Tab::Clicker => Tab::Sequence,
                _ => self.tab,
            };
            self.dirty = true;
        } else {
            let mut shared_cfg = shared.config.lock().unwrap();
            if *shared_cfg != self.config {
                *shared_cfg = self.config.clone();
                self.dirty = true;
            }
        }
        if self.profiles_dirty {
            *shared.profiles.lock().unwrap() = self.profiles.clone();
        }
        if !self.live() {
            self.dirty = false;
            self.profiles_dirty = false;
            return;
        }
        let wanted = control::hotkey_list(self.settings.hotkey, &self.profiles);
        if wanted != self.last_hotkeys {
            winsys::set_hotkeys(wanted.clone());
            self.last_hotkeys = wanted;
        }
        if self.dirty && !ctx.input(|i| i.pointer.any_down()) {
            self.settings.config = self.config.clone();
            crate::settings::save(&self.settings);
            self.dirty = false;
        }
        if self.profiles_dirty {
            profiles::save(&self.profiles);
            self.profiles_dirty = false;
        }
    }

    fn poll_picker(&mut self, ctx: &egui::Context) {
        let Some((target, rx)) = &self.picking else { return };
        match rx.try_recv() {
            Ok(point) => {
                let target = *target;
                self.picking = None;
                if let Some((x, y)) = point {
                    match target {
                        PickFor::Single => {
                            self.config.x = x;
                            self.config.y = y;
                            self.config.use_point = true;
                        }
                        PickFor::Step(i) => {
                            if let Some(Step { action: StepAction::Click { x: sx, y: sy, .. }, .. }) =
                                self.config.steps.get_mut(i)
                            {
                                *sx = x;
                                *sy = y;
                            }
                        }
                    }
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            Err(TryRecvError::Disconnected) => self.picking = None,
        }
    }

    fn start_picker(&mut self, target: PickFor) {
        if self.picking.is_some() || !self.live() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(winsys::pick_point());
        });
        self.picking = Some((target, rx));
    }

    /// While a rebind is pending, take the next key press as the new hotkey.
    fn poll_capture(&mut self, ctx: &egui::Context) {
        let Some(which) = self.capture else { return };
        let events = ctx.input(|i| i.events.clone());
        for ev in events {
            let egui::Event::Key { key, pressed: true, modifiers, .. } = ev else { continue };
            if key == egui::Key::Escape {
                self.capture = None;
                self.message = None;
                return;
            }
            if let Capture::Profile(i) = which
                && matches!(key, egui::Key::Backspace | egui::Key::Delete)
                && modifiers.is_none()
            {
                if let Some(p) = self.profiles.get_mut(i) {
                    p.hotkey = None;
                    self.profiles_dirty = true;
                }
                self.capture = None;
                self.message = None;
                return;
            }
            let Some(vk) = egui_key_to_vk(key) else { continue };
            let mut mods = 0;
            if modifiers.ctrl {
                mods |= MOD_CONTROL;
            }
            if modifiers.alt {
                mods |= MOD_ALT;
            }
            if modifiers.shift {
                mods |= MOD_SHIFT;
            }
            if modifiers.command && !modifiers.ctrl {
                mods |= MOD_CONTROL;
            }
            let hk = Hotkey { mods, vk };
            if !hk.is_acceptable() {
                self.message = Some(format!(
                    "{} on its own would stop that key working everywhere. Add Ctrl or Alt, or use a function key.",
                    hk.label()
                ));
                continue;
            }
            match which {
                Capture::Global => self.settings.hotkey = hk,
                Capture::Profile(i) => {
                    if let Some(p) = self.profiles.get_mut(i) {
                        p.hotkey = Some(hk);
                        self.profiles_dirty = true;
                    }
                }
            }
            self.save_settings();
            self.capture = None;
            self.message = None;
            return;
        }
    }

    fn start_or_stop(&mut self) {
        if !self.live() {
            return;
        }
        if self.shared.runner.is_running() {
            self.shared.runner.stop();
            return;
        }
        match engine::validate(&self.config, self.licensed()) {
            Ok(()) => {
                self.message = None;
                *self.shared.error.lock().unwrap() = None;
                self.shared.runner.start(self.config.clone());
            }
            Err(e) => self.message = Some(e),
        }
    }

    /// The run as the window should show it this frame.
    fn run_view(&mut self) -> RunView {
        if let Some(demo) = self.demo {
            return demo;
        }
        let stats = &self.shared.runner.stats;
        let count = stats.count();
        if stats.running() {
            let now = Instant::now();
            if stats.waiting() || self.run_started.is_none() {
                self.run_started = Some(now);
            }
            self.last_elapsed = self.run_started.map_or(0.0, |t| (now - t).as_secs_f64());
            let phase = if stats.waiting() { Phase::Waiting } else { Phase::Running };
            return RunView { phase, count, elapsed: self.last_elapsed };
        }
        self.run_started = None;
        let phase = match stats.reason() {
            Reason::None => Phase::Ready,
            Reason::Manual if count == 0 => Phase::Ready,
            r => Phase::Done(r),
        };
        RunView { phase, count, elapsed: self.last_elapsed }
    }

    /// What stops the window from starting, if anything.
    fn problem(&self) -> Option<String> {
        if let Some(m) = &self.message {
            return Some(m.clone());
        }
        if let Some(e) = self.shared.error.lock().unwrap().clone() {
            return Some(e);
        }
        if self.live() && winsys::failed_hotkeys().contains(&GLOBAL_ID) {
            return Some(format!(
                "{} is already used by another program. Choose a different hotkey in Settings.",
                self.settings.hotkey.label()
            ));
        }
        None
    }

    /// Status, the run's numbers and the one main button, at the top.
    fn run_card(&mut self, ui: &mut egui::Ui, p: Palette) {
        let view = self.run_view();
        let hk = self.settings.hotkey.label();
        let cfg = &self.config;
        let n = view.count;
        let noun = count_noun(cfg, n);
        let active = matches!(view.phase, Phase::Waiting | Phase::Running);
        let problem = if active { None } else { self.problem() };
        let (pill, fg, bg) = match (view.phase, &problem) {
            (_, Some(_)) => ("Can't start", p.error, p.error_soft),
            (Phase::Ready, _) => ("Ready", p.muted, p.track),
            (Phase::Waiting, _) => ("Starting soon", p.warn, p.warn_soft),
            (Phase::Running, _) => ("Running", p.success, p.success_soft),
            (Phase::Done(Reason::Manual), _) => ("Stopped", p.muted, p.track),
            (Phase::Done(_), _) => ("Done", p.accent_text, p.accent_soft),
        };
        let title = match view.phase {
            Phase::Ready => describe_action(cfg),
            Phase::Waiting => "Waiting for the start delay".to_string(),
            Phase::Running => format!("{} {noun}", group_digits(n)),
            Phase::Done(Reason::TimeLimit) => {
                format!("Stopped after {}", describe_secs(cfg.stop_after_secs.unwrap_or(0)))
            }
            Phase::Done(_) => format!("Stopped after {} {noun}", group_digits(n)),
        };
        let detail = match (view.phase, &problem) {
            (_, Some(e)) => e.clone(),
            (Phase::Ready, _) => format!("{}. Press {hk} to start.", describe_timing(cfg)),
            (Phase::Waiting, _) => format!("Press {hk} to cancel."),
            (Phase::Running, _) => match describe_limit(cfg) {
                Some(limit) => format!("{limit}. Press {hk} to stop."),
                None => format!("Runs until you press {hk}."),
            },
            (Phase::Done(Reason::CountReached), _) => format!("Reached the limit. Press {hk} to run it again."),
            (Phase::Done(Reason::TimeLimit), _) => {
                format!("{} {noun} in the time limit. Press {hk} to run it again.", group_digits(n))
            }
            (Phase::Done(_), _) => format!("Press {hk} to start again."),
        };
        let detail_color = if problem.is_some() { p.error } else { p.muted };
        let bar: Option<Option<f32>> = match view.phase {
            Phase::Waiting => Some(None),
            Phase::Running => Some(progress_fraction(cfg, n, view.elapsed)),
            Phase::Done(Reason::CountReached | Reason::TimeLimit) => Some(Some(1.0)),
            _ => None,
        };
        let button = primary_label(cfg, active);
        let mut clicked = false;
        theme::card_ui(ui, p, |ui| {
            ui.horizontal(|ui| {
                let left_w = ui.available_width() - PRIMARY_WIDTH - 12.0;
                ui.allocate_ui_with_layout(
                    egui::vec2(left_w, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        theme::pill(ui, pill, fg, bg);
                        ui.add_space(2.0);
                        ui.add(
                            egui::Label::new(egui::RichText::new(title).font(theme::title()).color(p.text))
                                .truncate(),
                        );
                        ui.add(
                            egui::Label::new(egui::RichText::new(detail).font(theme::small()).color(detail_color))
                                .wrap(),
                        );
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(theme::primary_button(p, button, true, PRIMARY_WIDTH)).clicked() {
                        clicked = true;
                    }
                });
            });
            if let Some(fraction) = bar {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::hover());
                let time = ui.input(|i| i.time);
                theme::progress_track(ui.painter(), rect, fraction, time, p);
                if fraction.is_none() {
                    ui.ctx().request_repaint();
                }
            }
        });
        if clicked {
            self.start_or_stop();
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui, p: Palette) {
        let labels: Vec<&str> = TABS.iter().map(|(_, l)| *l).collect();
        let selected = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
        if let Some(i) = theme::tabs(ui, &labels, selected, p) {
            self.tab = TABS[i].0;
            match self.tab {
                Tab::Clicker => self.config.mode = Mode::Single,
                Tab::Sequence => self.config.mode = Mode::Sequence,
                _ => {}
            }
        }
    }

    fn clicker_tab(&mut self, ui: &mut egui::Ui, p: Palette) {
        let picking = self.picking.is_some();
        let mut pick = false;
        let cfg = &mut self.config;
        theme::card_ui(ui, p, |ui| {
            theme::checkbox(ui, &mut cfg.click_enabled, theme::card_title("Click the mouse", p));
            ui.add_enabled_ui(cfg.click_enabled, |ui| {
                let w = ui.available_width() - theme::LABEL_WIDTH - 8.0;
                theme::row(ui, "Button", p, |ui| {
                    let buttons = [Button::Left, Button::Right, Button::Middle];
                    let at = buttons.iter().position(|b| *b == cfg.button).unwrap_or(0);
                    if let Some(i) = theme::choice(ui, egui::Id::new("button"), &["Left", "Right", "Middle"], at, w, p) {
                        cfg.button = buttons[i];
                    }
                });
                theme::row(ui, "Click", p, |ui| {
                    let at = usize::from(cfg.double_click);
                    if let Some(i) = theme::choice(ui, egui::Id::new("double"), &["Single", "Double"], at, w, p) {
                        cfg.double_click = i == 1;
                    }
                });
                theme::row(ui, "Where", p, |ui| {
                    let at = usize::from(cfg.use_point);
                    let labels = ["At the cursor", "At a fixed point"];
                    if let Some(i) = theme::choice(ui, egui::Id::new("where"), &labels, at, w, p) {
                        cfg.use_point = i == 1;
                    }
                });
                if cfg.use_point {
                    theme::row(ui, "Point", p, |ui| {
                        theme::unit_label(ui, "x", p);
                        theme::number(ui, &mut cfg.x, -100_000..=100_000, 1.0, "", p);
                        theme::unit_label(ui, "y", p);
                        theme::number(ui, &mut cfg.y, -100_000..=100_000, 1.0, "", p);
                        ui.add_enabled_ui(!picking, |ui| {
                            let text = if picking { "Picking..." } else { "Pick on screen" };
                            if theme::icon_text_button(ui, Icon::Crosshair, text, p).clicked() {
                                pick = true;
                            }
                        });
                    });
                }
            });
        });

        theme::card_ui(ui, p, |ui| {
            theme::checkbox(ui, &mut cfg.key_enabled, theme::card_title("Press a key", p));
            ui.add_enabled_ui(cfg.key_enabled, |ui| {
                theme::row(ui, "Key", p, |ui| {
                    key_picker(ui, "single_key", &mut cfg.key);
                    let w = ui.available_width();
                    let at = usize::from(cfg.key_hold);
                    if let Some(i) = theme::choice(ui, egui::Id::new("hold"), &["Tap it", "Hold it down"], at, w, p) {
                        cfg.key_hold = i == 1;
                    }
                });
                if cfg.key_hold {
                    theme::note(ui, "The key stays down until the run stops.", p);
                }
            });
        });

        let hint = if cfg.rate_per_second {
            format!("One every {}", describe_interval(cfg.interval_ms))
        } else {
            describe_rate(cfg.interval_ms)
        };
        theme::card_ui(ui, p, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::card_title("Speed", p));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    theme::note(ui, &hint, p);
                });
            });
            theme::row(ui, if cfg.rate_per_second { "Rate" } else { "Interval" }, p, |ui| {
                if cfg.rate_per_second {
                    let mut cps = engine::ms_to_cps(cfg.interval_ms);
                    if theme::number(ui, &mut cps, 0.01..=1000.0, 0.1, "per second", p).changed() {
                        cfg.interval_ms = engine::cps_to_ms(cps);
                    }
                } else {
                    theme::number(ui, &mut cfg.interval_ms, 1..=3_600_000, 1.0, "ms", p);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let at = usize::from(cfg.rate_per_second);
                    if let Some(i) = theme::choice(ui, egui::Id::new("unit"), &["ms", "Per second"], at, 160.0, p) {
                        cfg.rate_per_second = i == 1;
                    }
                });
            });
        });
        if pick {
            self.start_picker(PickFor::Single);
        }
        self.more_options(ui, p);
    }

    /// Start delay, stop conditions and random variation, shared by both
    /// modes and folded away until wanted.
    fn more_options(&mut self, ui: &mut egui::Ui, p: Palette) {
        let summary = describe_options(&self.config, &self.settings.hotkey.label());
        let mut open = self.more_open;
        let cfg = &mut self.config;
        theme::card_ui(ui, p, |ui| {
            if theme::disclosure(ui, "More options", &summary, open, p).clicked() {
                open = !open;
            }
            if !open {
                return;
            }
            ui.add_space(4.0);
            // Each row has a lead slot (a switch, a word or nothing) so the
            // number boxes line up.
            let lead = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
                ui.allocate_ui_with_layout(
                    egui::vec2(theme::LEAD_WIDTH, theme::CONTROL_HEIGHT),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_min_width(theme::LEAD_WIDTH);
                        add(ui)
                    },
                );
            };
            theme::row(ui, "Start delay", p, |ui| {
                lead(ui, &mut |_| {});
                let mut secs = cfg.start_delay_ms as f64 / 1000.0;
                if theme::number(ui, &mut secs, 0.0..=3600.0, 0.1, "seconds", p).changed() {
                    cfg.start_delay_ms = (secs * 1000.0).round() as u32;
                }
            });
            let noun = count_noun(cfg, 2);
            theme::row(ui, "Stop after", p, |ui| {
                let mut on = cfg.stop_after_clicks.is_some();
                let mut n = cfg.stop_after_clicks.unwrap_or(100);
                lead(ui, &mut |ui| {
                    theme::checkbox(ui, &mut on, "");
                });
                ui.add_enabled_ui(on, |ui| {
                    theme::number(ui, &mut n, 1..=u32::MAX as u64, 1.0, noun, p);
                });
                cfg.stop_after_clicks = on.then_some(n);
            });
            theme::row(ui, "Time limit", p, |ui| {
                let mut on = cfg.stop_after_secs.is_some();
                let mut n = cfg.stop_after_secs.unwrap_or(60);
                lead(ui, &mut |ui| {
                    theme::checkbox(ui, &mut on, "");
                });
                ui.add_enabled_ui(on, |ui| {
                    theme::number(ui, &mut n, 1..=86_400u32, 1.0, "seconds", p);
                });
                cfg.stop_after_secs = on.then_some(n);
            });
            theme::row(ui, "Vary timing", p, |ui| {
                lead(ui, &mut |ui| theme::unit_label(ui, "up to", p));
                theme::number(ui, &mut cfg.interval_variation_pct, 0..=100, 1.0, "% either way", p);
            });
            theme::row(ui, "Vary position", p, |ui| {
                lead(ui, &mut |ui| theme::unit_label(ui, "up to", p));
                theme::number(ui, &mut cfg.position_variation_px, 0..=500, 1.0, "pixels either way", p);
            });
        });
        self.more_open = open;
    }

    fn sequence_tab(&mut self, ui: &mut egui::Ui, p: Palette) {
        let max = license::max_steps(self.licensed());
        let picking = self.picking.is_some();
        let mut pick: Option<usize> = None;
        let mut remove: Option<usize> = None;
        let mut swap: Option<(usize, usize)> = None;
        let mut add: Option<StepAction> = None;
        let count = self.config.steps.len();
        let can_add = count < max;
        let last_point = self
            .config
            .steps
            .iter()
            .rev()
            .find_map(|s| match s.action {
                StepAction::Click { x, y, .. } => Some((x, y)),
                _ => None,
            })
            .unwrap_or((self.config.x, self.config.y));
        theme::card_ui(ui, p, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::card_title("Steps", p));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(can_add, |ui| {
                        if theme::icon_text_button(ui, Icon::Key, "Add a key press", p).clicked() {
                            add = Some(StepAction::Key { vk: keys::VK_SPACE });
                        }
                        if theme::icon_text_button(ui, Icon::Cursor, "Add a click", p).clicked() {
                            add = Some(StepAction::Click {
                                button: Button::Left,
                                double: false,
                                x: last_point.0,
                                y: last_point.1,
                            });
                        }
                    });
                });
            });
            ui.spacing_mut().item_spacing.y = 6.0;
            if count == 0 {
                theme::motif(ui, Motif::Sequence, p);
                ui.vertical_centered(|ui| {
                    theme::note(ui, "Add clicks at points and key presses. Each waits before the next.", p);
                });
            }
            for (i, step) in self.config.steps.iter_mut().enumerate() {
                if i > 0 {
                    theme::rule(ui, p);
                }
                ui.horizontal(|ui| {
                    theme::badge(ui, i + 1, p);
                    let is_key = matches!(step.action, StepAction::Key { .. });
                    if let Some(k) = theme::choice(ui, egui::Id::new(("kind", i)), &["Click", "Key"], usize::from(is_key), 112.0, p) {
                        step.action = if k == 1 {
                            StepAction::Key { vk: keys::VK_SPACE }
                        } else {
                            StepAction::Click { button: Button::Left, double: false, x: last_point.0, y: last_point.1 }
                        };
                    }
                    match &mut step.action {
                        StepAction::Click { button, double, .. } => {
                            egui::ComboBox::from_id_salt(("button", i))
                                .width(84.0)
                                .selected_text(button.label())
                                .show_ui(ui, |ui| {
                                    for b in [Button::Left, Button::Right, Button::Middle] {
                                        ui.selectable_value(button, b, b.label());
                                    }
                                });
                            theme::checkbox(ui, double, "Double");
                        }
                        StepAction::Key { vk } => key_picker(ui, ("step_key", i), vk),
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::icon_button(ui, Icon::Close, p).on_hover_text("Remove this step").clicked() {
                            remove = Some(i);
                        }
                        ui.add_enabled_ui(i + 1 < count, |ui| {
                            if theme::icon_button(ui, Icon::Down, p).on_hover_text("Move down").clicked() {
                                swap = Some((i, i + 1));
                            }
                        });
                        ui.add_enabled_ui(i > 0, |ui| {
                            if theme::icon_button(ui, Icon::Up, p).on_hover_text("Move up").clicked() {
                                swap = Some((i, i - 1));
                            }
                        });
                    });
                });
                ui.horizontal(|ui| {
                    ui.add_space(32.0);
                    if let StepAction::Click { x, y, .. } = &mut step.action {
                        theme::unit_label(ui, "x", p);
                        theme::number(ui, x, -100_000..=100_000, 1.0, "", p);
                        theme::unit_label(ui, "y", p);
                        theme::number(ui, y, -100_000..=100_000, 1.0, "", p);
                        ui.add_enabled_ui(!picking, |ui| {
                            if theme::icon_button(ui, Icon::Crosshair, p).on_hover_text("Pick the point on screen").clicked() {
                                pick = Some(i);
                            }
                        });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        theme::unit_label(ui, "ms", p);
                        ui.add_sized(
                            [theme::NUMBER_WIDTH, theme::CONTROL_HEIGHT],
                            egui::DragValue::new(&mut step.delay_ms).range(0..=3_600_000).speed(5.0),
                        );
                        theme::unit_label(ui, "then wait", p);
                    });
                });
            }
            if count > 0 {
                theme::rule(ui, p);
                theme::note(ui, "The steps run in order, then start again from step 1.", p);
            }
            if !can_add {
                theme::note(ui, &format!("The free version allows up to {max} steps."), p);
            }
        });
        if let Some(i) = remove {
            self.config.steps.remove(i);
        }
        if let Some((a, b)) = swap {
            self.config.steps.swap(a, b);
        }
        if let Some(action) = add {
            self.config.steps.push(Step { action, delay_ms: 500 });
        }
        if let Some(i) = pick {
            self.start_picker(PickFor::Step(i));
        }
        self.more_options(ui, p);
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui, p: Palette) {
        let capturing = self.capture == Some(Capture::Global);
        theme::card_ui(ui, p, |ui| {
            ui.label(theme::card_title("Start and stop", p));
            theme::row(ui, "Hotkey", p, |ui| {
                let text = if capturing { "Press a key".to_string() } else { self.settings.hotkey.label() };
                if theme::keycap(ui, &text, capturing, p)
                    .on_hover_text("Click, then press the new key or combination. Esc cancels.")
                    .clicked()
                {
                    self.capture = if capturing { None } else { Some(Capture::Global) };
                    self.message = None;
                }
                if self.settings.hotkey != keys::DEFAULT_HOTKEY
                    && theme::link(ui, &format!("Reset to {}", keys::DEFAULT_HOTKEY.label()), p).clicked()
                {
                    self.settings.hotkey = keys::DEFAULT_HOTKEY;
                    self.save_settings();
                }
            });
            theme::note(ui, "Works in any app, even while this window is minimised.", p);
        });
        theme::card_ui(ui, p, |ui| {
            ui.label(theme::card_title("Window", p));
            let mut on_top = self.settings.always_on_top;
            if theme::checkbox(ui, &mut on_top, "Keep this window on top").changed() {
                self.settings.always_on_top = on_top;
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::WindowLevel(if on_top {
                    egui::WindowLevel::AlwaysOnTop
                } else {
                    egui::WindowLevel::Normal
                }));
                self.save_settings();
            }
            theme::note(
                ui,
                "The tray icon by the clock turns green during a run. Click it to bring this window back.",
                p,
            );
        });
        theme::card_ui(ui, p, |ui| {
            theme::icon_note(
                ui,
                Icon::Info,
                "Windows does not let a normal app click or type into windows that run as administrator, so this app cannot click inside them.",
                p.muted,
                p,
            );
        });
        if license::paid_tier_enabled() {
            theme::card_ui(ui, p, |ui| self.license_ui(ui, p));
        }
    }

    fn license_ui(&mut self, ui: &mut egui::Ui, p: Palette) {
        ui.label(theme::card_title("Unlock", p));
        if let Some(email) = &self.licensed_email {
            theme::note(ui, &format!("Unlocked for {email}. Thank you."), p);
            return;
        }
        theme::note(
            ui,
            &format!(
                "Free: sequences of up to {} steps and {} saved profile. A one-off unlock removes both limits.",
                license::FREE_STEPS,
                license::FREE_PROFILES
            ),
            p,
        );
        ui.horizontal(|ui| {
            ui.add_sized(
                [ui.available_width() - 100.0, theme::CONTROL_HEIGHT],
                egui::TextEdit::singleline(&mut self.key_input).hint_text("Licence key").vertical_align(egui::Align::Center),
            );
            if ui.add(theme::secondary_button(p, "Unlock")).clicked() {
                match license::verify(&self.key_input) {
                    Some(email) => {
                        self.licensed_email = Some(email);
                        self.shared.licensed.store(true, Ordering::SeqCst);
                        self.settings.license_key = self.key_input.trim().to_string();
                        self.save_settings();
                        self.message = None;
                    }
                    None => self.message = Some("That licence key is not valid. Paste the whole key from your email.".into()),
                }
            }
        });
        if theme::link(ui, "Buy an unlock", p).clicked() {
            let _ = std::process::Command::new("explorer").arg(license::BUY_URL).spawn();
        }
    }

    fn profiles_tab(&mut self, ui: &mut egui::Ui, p: Palette) {
        let max = license::max_profiles(self.licensed());
        theme::card_ui(ui, p, |ui| {
            ui.label(theme::card_title("Save the current set-up", p));
            ui.horizontal(|ui| {
                ui.add_sized(
                    [ui.available_width() - 140.0, theme::CONTROL_HEIGHT],
                    egui::TextEdit::singleline(&mut self.profile_name)
                        .hint_text("Name, such as Farm run")
                        .vertical_align(egui::Align::Center),
                );
                ui.add_enabled_ui(!self.profile_name.trim().is_empty(), |ui| {
                    if theme::icon_text_button(ui, Icon::Plus, "Save profile", p).clicked() {
                        if profiles::upsert(&mut self.profiles, &self.profile_name, &self.config, max) {
                            self.profiles_dirty = true;
                            self.profile_name.clear();
                            self.message = None;
                        } else {
                            self.message = Some(format!("The free version keeps up to {max} saved profile."));
                        }
                    }
                });
            });
        });

        let mut load: Option<usize> = None;
        let mut delete: Option<usize> = None;
        let failed = if self.live() { winsys::failed_hotkeys() } else { Vec::new() };
        theme::card_ui(ui, p, |ui| {
            ui.label(theme::card_title("Saved profiles", p));
            if self.profiles.is_empty() {
                theme::motif(ui, Motif::Profiles, p);
                ui.vertical_centered(|ui| {
                    theme::note(ui, "Save a set-up above to load it later, or start it with its own hotkey.", p);
                });
                return;
            }
            for (i, prof) in self.profiles.iter_mut().enumerate() {
                if i > 0 {
                    theme::rule(ui, p);
                }
                ui.horizontal(|ui| {
                    let right_w = 196.0;
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width() - right_w, 40.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.add(egui::Label::new(theme::card_title(&prof.name, p)).truncate());
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(describe_setup(&prof.config)).font(theme::small()).color(p.muted),
                                )
                                .truncate(),
                            );
                        },
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::icon_button(ui, Icon::Close, p).on_hover_text("Delete this profile").clicked() {
                            delete = Some(i);
                        }
                        let capturing = self.capture == Some(Capture::Profile(i));
                        let response = match (capturing, prof.hotkey) {
                            (true, _) => theme::keycap(ui, "Press keys", true, p),
                            (false, Some(h)) => theme::keycap(ui, &h.label(), false, p),
                            (false, None) => theme::link(ui, "Set hotkey", p),
                        };
                        if response
                            .on_hover_text("Click, then press a key combination. Backspace removes it, Esc cancels.")
                            .clicked()
                        {
                            self.capture = if capturing { None } else { Some(Capture::Profile(i)) };
                            self.message = None;
                        }
                        if ui.add(theme::secondary_button(p, "Load")).clicked() {
                            load = Some(i);
                        }
                    });
                });
                if failed.contains(&(control::PROFILE_BASE + i as i32)) {
                    theme::icon_note(
                        ui,
                        Icon::Info,
                        "That hotkey is already used by another program or profile. Pick another.",
                        p.error,
                        p,
                    );
                }
            }
            if self.profiles.iter().any(|p| p.hotkey.is_some()) {
                ui.add_space(2.0);
                theme::note(ui, "A profile's hotkey loads it and starts it. Press it again to stop.", p);
            }
        });
        if let Some(i) = load {
            self.config = self.profiles[i].config.clone();
            self.tab = match self.config.mode {
                Mode::Single => Tab::Clicker,
                Mode::Sequence => Tab::Sequence,
            };
        }
        if let Some(i) = delete {
            self.profiles.remove(i);
            self.profiles_dirty = true;
            self.capture = None;
        }
    }

    fn footer(&mut self, ui: &mut egui::Ui, p: Palette) {
        ui.horizontal(|ui| {
            let mut auto_update = self.settings.auto_update;
            if theme::checkbox(ui, &mut auto_update, egui::RichText::new("Automatically update").font(theme::small()).color(p.muted))
                .on_hover_text("Checks for new versions about once a day and installs them in the background.")
                .changed()
            {
                self.settings.auto_update = auto_update;
                self.save_settings();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(format!("v{}", self.version)).font(theme::small()).color(p.muted));
            });
        });
    }
}

/// Dropdown of every pressable key.
fn key_picker(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, vk: &mut u32) {
    egui::ComboBox::from_id_salt(id)
        .width(96.0)
        .height(320.0)
        .selected_text(keys::key_name(*vk))
        .show_ui(ui, |ui| {
            for (name, code) in keys::key_table() {
                ui.selectable_value(vk, *code, name);
            }
        });
}

pub fn describe_rate(interval_ms: u32) -> String {
    let cps = engine::ms_to_cps(interval_ms);
    if cps >= 10.0 {
        format!("About {:.0} per second", cps)
    } else if cps >= 1.0 {
        format!("About {:.1} per second", cps)
    } else {
        format!("One every {:.1} seconds", interval_ms as f64 / 1000.0)
    }
}

/// An interval in the unit people would say it in: "100 ms", "1.5 s", "2 min".
pub fn describe_interval(ms: u32) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms >= 60_000 && ms % 60_000 == 0 {
        format!("{} min", ms / 60_000)
    } else {
        let s = ms as f64 / 1000.0;
        if ms % 1000 == 0 { format!("{s:.0} s") } else { format!("{s:.1} s") }
    }
}

/// Seconds as "45 s", "2 min", "1 h 30 min".
pub fn describe_secs(s: u32) -> String {
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    match (h, m, sec) {
        (0, 0, s) => format!("{s} s"),
        (0, m, 0) => format!("{m} min"),
        (0, m, s) => format!("{m} min {s} s"),
        (h, 0, _) => format!("{h} h"),
        (h, m, _) => format!("{h} h {m} min"),
    }
}

/// 1284 as "1,284".
pub fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What a run counts: clicks, key presses or steps.
pub fn count_noun(cfg: &Config, n: u64) -> &'static str {
    let one = n == 1;
    match (cfg.mode, cfg.click_enabled) {
        (Mode::Sequence, _) => if one { "step" } else { "steps" },
        (Mode::Single, true) => if one { "click" } else { "clicks" },
        (Mode::Single, false) => if one { "key press" } else { "key presses" },
    }
}

/// The set-up in one plain sentence, such as "Left click every 100 ms".
pub fn describe_setup(cfg: &Config) -> String {
    if cfg.mode == Mode::Sequence {
        return match cfg.steps.len() {
            0 => "No steps yet".into(),
            1 => "1 step, repeated".into(),
            n => format!("{n} steps in a loop"),
        };
    }
    let every = format!("every {}", describe_interval(cfg.interval_ms));
    let key = keys::key_name(cfg.key);
    let click = || click_words(cfg);
    match (cfg.click_enabled, cfg.key_enabled, cfg.key_hold) {
        (false, false, _) => "Nothing to do yet".into(),
        (true, false, _) => format!("{} {every}", click()),
        (true, true, false) => format!("{} and press {key} {every}", click()),
        (true, true, true) => format!("{} {every}, holding {key}", click()),
        (false, true, false) => format!("Press {key} {every}"),
        (false, true, true) => format!("Hold {key} down"),
    }
}

fn click_words(cfg: &Config) -> String {
    if cfg.double_click {
        format!("Double {} click", cfg.button.label().to_lowercase())
    } else {
        format!("{} click", cfg.button.label())
    }
}

/// What a run does, without the timing: "Left click and press E".
pub fn describe_action(cfg: &Config) -> String {
    if cfg.mode == Mode::Sequence {
        return describe_setup(cfg);
    }
    let key = keys::key_name(cfg.key);
    match (cfg.click_enabled, cfg.key_enabled, cfg.key_hold) {
        (false, false, _) => "Nothing to do yet".into(),
        (true, false, _) => click_words(cfg),
        (true, true, false) => format!("{} and press {key}", click_words(cfg)),
        (true, true, true) => format!("{} and hold {key}", click_words(cfg)),
        (false, true, false) => format!("Press {key}"),
        (false, true, true) => format!("Hold {key} down"),
    }
}

/// Where and how often, for the ready state: "Every 100 ms at 1280, 642".
pub fn describe_timing(cfg: &Config) -> String {
    if cfg.mode == Mode::Sequence {
        return "Each step waits its own time".into();
    }
    let mut s = format!("Every {}", describe_interval(cfg.interval_ms));
    if cfg.click_enabled && cfg.use_point {
        s.push_str(&format!(" at {}, {}", cfg.x, cfg.y));
    }
    s
}

/// The stop condition, if there is one: "Stops after 500 clicks".
pub fn describe_limit(cfg: &Config) -> Option<String> {
    let count = cfg.stop_after_clicks.map(|n| format!("{} {}", group_digits(n), count_noun(cfg, n)));
    let time = cfg.stop_after_secs.map(describe_secs);
    match (count, time) {
        (Some(c), Some(t)) => Some(format!("Stops after {c} or {t}")),
        (Some(c), None) => Some(format!("Stops after {c}")),
        (None, Some(t)) => Some(format!("Stops after {t}")),
        (None, None) => None,
    }
}

/// The folded "More options" in one line.
pub fn describe_options(cfg: &Config, hotkey: &str) -> String {
    let mut parts = vec![describe_limit(cfg).unwrap_or_else(|| format!("Runs until {hotkey}"))];
    if cfg.start_delay_ms > 0 {
        parts.push(format!("{} delay", describe_interval(cfg.start_delay_ms)));
    }
    if cfg.interval_variation_pct > 0 || cfg.position_variation_px > 0 {
        parts.push("varied".into());
    }
    parts.join(", ")
}

/// How far through its limit a run is, or `None` with no limit.
pub fn progress_fraction(cfg: &Config, count: u64, elapsed_secs: f64) -> Option<f32> {
    let by_count = cfg.stop_after_clicks.map(|max| count as f64 / max.max(1) as f64);
    let by_time = cfg.stop_after_secs.map(|max| elapsed_secs / max.max(1) as f64);
    let f = match (by_count, by_time) {
        (Some(a), Some(b)) => a.max(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return None,
    };
    Some(f.clamp(0.0, 1.0) as f32)
}

/// The main button's words.
pub fn primary_label(cfg: &Config, running: bool) -> &'static str {
    match (cfg.mode, cfg.click_enabled, running) {
        (Mode::Sequence, _, false) => "Start sequence",
        (Mode::Sequence, _, true) => "Stop sequence",
        (Mode::Single, true, false) => "Start clicking",
        (Mode::Single, true, true) => "Stop clicking",
        (Mode::Single, false, false) => "Start pressing",
        (Mode::Single, false, true) => "Stop pressing",
    }
}

/// Map an egui key to a Windows virtual-key code through the key table.
pub fn egui_key_to_vk(key: egui::Key) -> Option<u32> {
    let name = key.name();
    let name = name.strip_prefix("Arrow").unwrap_or(name);
    let name = match name {
        "Minus" => "-",
        "Equals" => "=",
        "Comma" => ",",
        "Period" => ".",
        "Slash" => "/",
        "Semicolon" => ";",
        "Backtick" => "`",
        "Backslash" => "\\",
        "OpenBracket" => "[",
        "CloseBracket" => "]",
        "Quote" => "'",
        "PageUp" => "Page Up",
        "PageDown" => "Page Down",
        other => other,
    };
    keys::key_table()
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, vk)| *vk)
}

impl eframe::App for App {
    fn ui(&mut self, ui_root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui_root.ctx().clone();
        if let Some(mut shots) = self.shots.take() {
            if let Some(scene) = shots.drive(&ctx, SHOT_SCENES) {
                self.show_scene(scene);
            }
            self.shots = Some(shots);
        }
        self.sync(&ctx);
        self.poll_picker(&ctx);
        self.poll_capture(&ctx);
        if self.shared.runner.is_running() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let p = theme::palette(&ctx);
        egui::Panel::bottom("footer")
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin { left: 16, right: 16, top: 4, bottom: 10 }))
            .show(ui_root, |ui| self.footer(ui, p));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin { left: 16, right: 16, top: 16, bottom: 4 }))
            .show(ui_root, |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                self.run_card(ui, p);
                self.tab_bar(ui, p);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 12.0;
                    match self.tab {
                        Tab::Clicker => self.clicker_tab(ui, p),
                        Tab::Sequence => self.sequence_tab(ui, p),
                        Tab::Profiles => self.profiles_tab(ui, p),
                        Tab::Settings => self.settings_tab(ui, p),
                    }
                });
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shared.runner.stop_and_wait();
        self.settings.config = self.config.clone();
        self.save_settings();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egui_keys_map_to_virtual_keys() {
        assert_eq!(egui_key_to_vk(egui::Key::A), Some('A' as u32));
        assert_eq!(egui_key_to_vk(egui::Key::F6), Some(0x75));
        assert_eq!(egui_key_to_vk(egui::Key::Num5), Some('5' as u32));
        assert_eq!(egui_key_to_vk(egui::Key::Space), Some(0x20));
        assert_eq!(egui_key_to_vk(egui::Key::ArrowLeft), Some(0x25));
        assert_eq!(egui_key_to_vk(egui::Key::PageDown), Some(0x22));
        assert_eq!(egui_key_to_vk(egui::Key::Minus), Some(0xBD));
    }

    #[test]
    fn rate_is_described_in_plain_words() {
        assert_eq!(describe_rate(100), "About 10 per second");
        assert_eq!(describe_rate(1), "About 1000 per second");
        assert_eq!(describe_rate(500), "About 2.0 per second");
        assert_eq!(describe_rate(4000), "One every 4.0 seconds");
    }

    #[test]
    fn intervals_and_durations_read_naturally() {
        assert_eq!(describe_interval(100), "100 ms");
        assert_eq!(describe_interval(1500), "1.5 s");
        assert_eq!(describe_interval(3000), "3 s");
        assert_eq!(describe_interval(120_000), "2 min");
        assert_eq!(describe_secs(45), "45 s");
        assert_eq!(describe_secs(120), "2 min");
        assert_eq!(describe_secs(90), "1 min 30 s");
        assert_eq!(describe_secs(3600), "1 h");
        assert_eq!(describe_secs(5400), "1 h 30 min");
    }

    #[test]
    fn digits_are_grouped() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1284), "1,284");
        assert_eq!(group_digits(1_000_000), "1,000,000");
    }

    #[test]
    fn setup_is_one_plain_sentence() {
        let mut cfg = Config::default();
        assert_eq!(describe_setup(&cfg), "Left click every 100 ms");
        cfg.double_click = true;
        cfg.button = Button::Right;
        cfg.use_point = true;
        cfg.x = 10;
        cfg.y = 20;
        assert_eq!(describe_setup(&cfg), "Double right click every 100 ms");
        assert_eq!(describe_action(&cfg), "Double right click");
        assert_eq!(describe_timing(&cfg), "Every 100 ms at 10, 20");
        cfg.key_enabled = true;
        cfg.key = 'E' as u32;
        assert_eq!(describe_action(&cfg), "Double right click and press E");
        cfg.key = keys::VK_SPACE;
        cfg.click_enabled = false;
        cfg.key_enabled = true;
        assert_eq!(describe_setup(&cfg), "Press Space every 100 ms");
        cfg.key_hold = true;
        assert_eq!(describe_setup(&cfg), "Hold Space down");
        cfg.mode = Mode::Sequence;
        assert_eq!(describe_setup(&cfg), "No steps yet");
    }

    #[test]
    fn limits_and_options_are_summarised() {
        let mut cfg = Config::default();
        assert_eq!(describe_limit(&cfg), None);
        assert_eq!(describe_options(&cfg, "F6"), "Runs until F6");
        cfg.stop_after_clicks = Some(5000);
        assert_eq!(describe_limit(&cfg).as_deref(), Some("Stops after 5,000 clicks"));
        cfg.stop_after_secs = Some(120);
        cfg.start_delay_ms = 3000;
        cfg.interval_variation_pct = 10;
        assert_eq!(describe_options(&cfg, "F6"), "Stops after 5,000 clicks or 2 min, 3 s delay, varied");
    }

    #[test]
    fn progress_follows_the_nearest_limit() {
        let mut cfg = Config::default();
        assert_eq!(progress_fraction(&cfg, 10, 1.0), None);
        cfg.stop_after_clicks = Some(100);
        assert_eq!(progress_fraction(&cfg, 25, 0.0), Some(0.25));
        cfg.stop_after_secs = Some(10);
        assert_eq!(progress_fraction(&cfg, 25, 5.0), Some(0.5));
        assert_eq!(progress_fraction(&cfg, 500, 0.0), Some(1.0));
    }

    #[test]
    fn button_and_count_words_follow_the_mode() {
        let mut cfg = Config::default();
        assert_eq!(primary_label(&cfg, false), "Start clicking");
        assert_eq!(count_noun(&cfg, 1), "click");
        cfg.click_enabled = false;
        assert_eq!(primary_label(&cfg, true), "Stop pressing");
        assert_eq!(count_noun(&cfg, 3), "key presses");
        cfg.mode = Mode::Sequence;
        assert_eq!(primary_label(&cfg, false), "Start sequence");
        assert_eq!(count_noun(&cfg, 2), "steps");
    }
}
