//! The app window. Engine, hotkeys and tray live in other modules; this file
//! is the UI plus the glue that keeps it in step with them. The footer with
//! the auto-update toggle and version is required on every factory app.

use crate::control::{self, GLOBAL_ID, Shared};
use crate::engine::{self, Button, Config, Mode, Reason, Runner, Step, StepAction};
use crate::keys::{self, Hotkey, MOD_ALT, MOD_CONTROL, MOD_SHIFT};
use crate::profiles::{self, Profile};
use crate::{license, winsys};
use eframe::egui;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError};

const WIDTH: f32 = 520.0;
const HEIGHT: f32 = 640.0;

pub fn run(_args: Vec<String>) -> eframe::Result<()> {
    let settings = crate::settings::load();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Auto Clicker & Key Presser")
        .with_inner_size([WIDTH, HEIGHT])
        .with_min_inner_size([420.0, 480.0]);
    if settings.always_on_top {
        viewport = viewport.with_always_on_top();
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Auto Clicker & Key Presser",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, settings)))),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Single,
    Sequence,
    Options,
    Profiles,
}

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
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, settings: crate::settings::Settings) -> Self {
        let ctx = cc.egui_ctx.clone();
        let repaint_ctx = ctx.clone();
        let on_change: Arc<dyn Fn(bool) + Send + Sync> = Arc::new(move |running| {
            winsys::set_tray_running(running);
            repaint_ctx.request_repaint();
        });
        let runner = Runner::new(Arc::new(winsys::SystemInput::new()), on_change);
        let licensed_email = license::verify(&settings.license_key);
        let config = settings.config.clone();
        let profiles = profiles::load();
        let shared = Arc::new(Shared {
            runner,
            config: std::sync::Mutex::new(config.clone()),
            profiles: std::sync::Mutex::new(profiles.clone()),
            external_change: Default::default(),
            licensed: std::sync::atomic::AtomicBool::new(licensed_email.is_some()),
            error: Default::default(),
        });
        let handler_shared = shared.clone();
        let handler_ctx = ctx.clone();
        winsys::start(move |ev| {
            handler_shared.handle(ev);
            handler_ctx.request_repaint();
        });
        let tab = match config.mode {
            Mode::Single => Tab::Single,
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
        }
    }

    fn licensed(&self) -> bool {
        self.licensed_email.is_some()
    }

    /// Keep the hotkey thread's copy of the config in step with the window.
    fn sync(&mut self, ctx: &egui::Context) {
        let shared = self.shared.clone();
        if shared.external_change.swap(false, Ordering::SeqCst) {
            self.config = shared.config.lock().unwrap().clone();
            self.tab = match self.config.mode {
                Mode::Single if self.tab == Tab::Sequence => Tab::Single,
                Mode::Sequence if self.tab == Tab::Single => Tab::Sequence,
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
        if self.picking.is_some() {
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
            crate::settings::save(&self.settings);
            self.capture = None;
            self.message = None;
            return;
        }
    }

    fn start_or_stop(&mut self) {
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

    fn status_line(&self) -> String {
        let stats = &self.shared.runner.stats;
        if stats.running() {
            if stats.waiting() {
                return "Starting after the delay...".into();
            }
            let n = stats.count();
            return format!("Running - {n} {}", if n == 1 { "action" } else { "actions" });
        }
        let n = stats.count();
        match stats.reason() {
            Reason::CountReached => format!("Stopped after {n} actions."),
            Reason::TimeLimit => format!("Stopped at the time limit after {n} actions."),
            Reason::Manual if n > 0 => format!("Stopped after {n} actions."),
            _ => format!("Ready. Press {} to start or stop.", self.settings.hotkey.label()),
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let running = self.shared.runner.is_running();
        ui.horizontal(|ui| {
            let (text, fill) = if running {
                ("Stop", egui::Color32::from_rgb(0xB0, 0x3A, 0x3A))
            } else {
                ("Start", egui::Color32::from_rgb(0x2E, 0x8B, 0x4A))
            };
            let button = egui::Button::new(
                egui::RichText::new(format!("{text}  ({})", self.settings.hotkey.label()))
                    .size(18.0)
                    .color(egui::Color32::WHITE),
            )
            .fill(fill)
            .min_size(egui::vec2(180.0, 38.0));
            if ui.add(button).clicked() {
                self.start_or_stop();
            }
            ui.vertical(|ui| {
                ui.label(self.status_line());
                let hotkey_error = self.shared.error.lock().unwrap().clone();
                if let Some(e) = self.message.clone().or(hotkey_error) {
                    ui.colored_label(egui::Color32::from_rgb(0xD9, 0x6B, 0x4A), e);
                }
            });
        });
        if winsys::failed_hotkeys().contains(&GLOBAL_ID) {
            ui.colored_label(
                egui::Color32::from_rgb(0xD9, 0x6B, 0x4A),
                format!(
                    "{} is already used by another program. Choose a different hotkey in Options.",
                    self.settings.hotkey.label()
                ),
            );
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let mut tab = self.tab;
            for (t, label) in [
                (Tab::Single, "Single target"),
                (Tab::Sequence, "Sequence"),
                (Tab::Options, "Options"),
                (Tab::Profiles, "Profiles"),
            ] {
                ui.selectable_value(&mut tab, t, label);
            }
            if tab != self.tab {
                self.tab = tab;
                match tab {
                    Tab::Single => self.config.mode = Mode::Single,
                    Tab::Sequence => self.config.mode = Mode::Sequence,
                    _ => {}
                }
            }
        });
        ui.separator();
    }

    fn single_tab(&mut self, ui: &mut egui::Ui) {
        let picking = self.picking.is_some();
        let cfg = &mut self.config;
        ui.checkbox(&mut cfg.click_enabled, "Click the mouse");
        ui.add_enabled_ui(cfg.click_enabled, |ui| {
            egui::Grid::new("click_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Button");
                ui.horizontal(|ui| {
                    for b in [Button::Left, Button::Right, Button::Middle] {
                        ui.selectable_value(&mut cfg.button, b, b.label());
                    }
                });
                ui.end_row();
                ui.label("Click type");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut cfg.double_click, false, "Single");
                    ui.selectable_value(&mut cfg.double_click, true, "Double");
                });
                ui.end_row();
                ui.label("Where");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut cfg.use_point, false, "At the cursor");
                    ui.selectable_value(&mut cfg.use_point, true, "At a fixed point");
                });
                ui.end_row();
            });
            ui.add_enabled_ui(cfg.use_point, |ui| {
                ui.horizontal(|ui| {
                    ui.label("X");
                    ui.add(egui::DragValue::new(&mut cfg.x).speed(1.0));
                    ui.label("Y");
                    ui.add(egui::DragValue::new(&mut cfg.y).speed(1.0));
                });
            });
        });
        let mut pick_clicked = false;
        ui.add_enabled_ui(cfg.click_enabled && !picking, |ui| {
            if ui.button("Pick a point on screen...").clicked() {
                pick_clicked = true;
            }
        });
        ui.add_space(6.0);
        ui.separator();
        ui.checkbox(&mut cfg.key_enabled, "Press a key");
        ui.add_enabled_ui(cfg.key_enabled, |ui| {
            ui.horizontal(|ui| {
                key_picker(ui, "single_key", &mut cfg.key);
                ui.selectable_value(&mut cfg.key_hold, false, "Tap it");
                ui.selectable_value(&mut cfg.key_hold, true, "Hold it down");
            });
            if cfg.key_hold {
                ui.weak("The key stays down until you stop; it is released when the run ends.");
            }
        });
        ui.add_space(6.0);
        ui.separator();
        ui.strong("Speed");
        rate_editor(ui, cfg);
        if pick_clicked {
            self.start_picker(PickFor::Single);
        }
    }

    fn sequence_tab(&mut self, ui: &mut egui::Ui) {
        let max = license::max_steps(self.licensed());
        let picking = self.picking.is_some();
        let mut pick: Option<usize> = None;
        let mut remove: Option<usize> = None;
        let mut swap: Option<(usize, usize)> = None;
        let count = self.config.steps.len();
        if count == 0 {
            ui.label("No steps yet. Add clicks at points or key presses; they run in order and then repeat.");
        }
        for (i, step) in self.config.steps.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("{}.", i + 1));
                let mut is_key = matches!(step.action, StepAction::Key { .. });
                let was_key = is_key;
                egui::ComboBox::from_id_salt(("kind", i))
                    .width(70.0)
                    .selected_text(if is_key { "Key" } else { "Click" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut is_key, false, "Click");
                        ui.selectable_value(&mut is_key, true, "Key");
                    });
                if is_key != was_key {
                    step.action = if is_key {
                        StepAction::Key { vk: keys::VK_SPACE }
                    } else {
                        StepAction::Click { button: Button::Left, double: false, x: 0, y: 0 }
                    };
                }
                match &mut step.action {
                    StepAction::Click { button, double, x, y } => {
                        egui::ComboBox::from_id_salt(("button", i))
                            .width(64.0)
                            .selected_text(button.label())
                            .show_ui(ui, |ui| {
                                for b in [Button::Left, Button::Right, Button::Middle] {
                                    ui.selectable_value(button, b, b.label());
                                }
                            });
                        ui.checkbox(double, "x2").on_hover_text("Double click");
                        ui.add(egui::DragValue::new(x).prefix("x "));
                        ui.add(egui::DragValue::new(y).prefix("y "));
                        if ui.add_enabled(!picking, egui::Button::new("Pick")).clicked() {
                            pick = Some(i);
                        }
                    }
                    StepAction::Key { vk } => key_picker(ui, ("step_key", i), vk),
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                ui.label("then wait");
                ui.add(egui::DragValue::new(&mut step.delay_ms).suffix(" ms").range(0..=3_600_000).speed(5.0));
                if ui.add_enabled(i > 0, egui::Button::new("Up")).clicked() {
                    swap = Some((i, i - 1));
                }
                if ui.add_enabled(i + 1 < count, egui::Button::new("Down")).clicked() {
                    swap = Some((i, i + 1));
                }
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                }
            });
            ui.add_space(2.0);
        }
        if let Some(i) = remove {
            self.config.steps.remove(i);
        }
        if let Some((a, b)) = swap {
            self.config.steps.swap(a, b);
        }
        if let Some(i) = pick {
            self.start_picker(PickFor::Step(i));
        }
        let can_add = self.config.steps.len() < max;
        ui.horizontal(|ui| {
            if ui.add_enabled(can_add, egui::Button::new("Add click step")).clicked() {
                let (x, y) = match self.config.steps.iter().rev().find_map(|s| match s.action {
                    StepAction::Click { x, y, .. } => Some((x, y)),
                    _ => None,
                }) {
                    Some(p) => p,
                    None => (self.config.x, self.config.y),
                };
                self.config.steps.push(Step {
                    action: StepAction::Click { button: Button::Left, double: false, x, y },
                    delay_ms: 500,
                });
            }
            if ui.add_enabled(can_add, egui::Button::new("Add key step")).clicked() {
                self.config.steps.push(Step { action: StepAction::Key { vk: keys::VK_SPACE }, delay_ms: 500 });
            }
        });
        if !can_add {
            ui.weak(format!("The free version allows up to {max} steps."));
        }
        ui.add_space(4.0);
        ui.weak("The whole list repeats until you stop it or a stop condition in Options is met.");
    }

    fn options_tab(&mut self, ui: &mut egui::Ui) {
        let cfg = &mut self.config;
        ui.strong("Start and stop");
        egui::Grid::new("opts_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Start delay");
            let mut secs = cfg.start_delay_ms as f64 / 1000.0;
            if ui
                .add(egui::DragValue::new(&mut secs).suffix(" s").range(0.0..=3600.0).speed(0.1).max_decimals(1))
                .changed()
            {
                cfg.start_delay_ms = (secs * 1000.0).round() as u32;
            }
            ui.end_row();

            ui.label("Stop after");
            ui.horizontal(|ui| {
                let mut on = cfg.stop_after_clicks.is_some();
                if ui.checkbox(&mut on, "").changed() {
                    cfg.stop_after_clicks = on.then_some(100);
                }
                if let Some(n) = &mut cfg.stop_after_clicks {
                    ui.add(egui::DragValue::new(n).range(1..=u32::MAX as u64).speed(1.0));
                    ui.label("clicks / key presses");
                } else {
                    ui.weak("off");
                }
            });
            ui.end_row();

            ui.label("Time limit");
            ui.horizontal(|ui| {
                let mut on = cfg.stop_after_secs.is_some();
                if ui.checkbox(&mut on, "").changed() {
                    cfg.stop_after_secs = on.then_some(60);
                }
                if let Some(n) = &mut cfg.stop_after_secs {
                    ui.add(egui::DragValue::new(n).range(1..=86_400u32).suffix(" s").speed(1.0));
                } else {
                    ui.weak("off");
                }
            });
            ui.end_row();
        });
        ui.weak("The hotkey always stops a run too.");
        ui.add_space(6.0);
        ui.separator();
        ui.strong("Random variation");
        egui::Grid::new("var_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Timing");
            ui.add(egui::DragValue::new(&mut cfg.interval_variation_pct).range(0..=100).prefix("+/- ").suffix(" %"));
            ui.end_row();
            ui.label("Position");
            ui.add(egui::DragValue::new(&mut cfg.position_variation_px).range(0..=500).prefix("+/- ").suffix(" px"));
            ui.end_row();
        });
        ui.weak("Each click lands within this many pixels of the target. At the cursor, that is where the cursor was when the run started.");
        ui.add_space(6.0);
        ui.separator();
        ui.strong("Hotkey");
        ui.horizontal(|ui| {
            ui.label("Start / stop:");
            let capturing = self.capture == Some(Capture::Global);
            let text = if capturing { "Press a key...".to_string() } else { self.settings.hotkey.label() };
            if ui.button(text).on_hover_text("Click, then press the new key or combination. Esc cancels.").clicked() {
                self.capture = if capturing { None } else { Some(Capture::Global) };
                self.message = None;
            }
            if self.settings.hotkey != keys::DEFAULT_HOTKEY && ui.button("Reset").clicked() {
                self.settings.hotkey = keys::DEFAULT_HOTKEY;
                crate::settings::save(&self.settings);
            }
        });
        ui.add_space(6.0);
        ui.separator();
        ui.strong("Window");
        let mut on_top = self.settings.always_on_top;
        if ui.checkbox(&mut on_top, "Keep this window on top").changed() {
            self.settings.always_on_top = on_top;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::WindowLevel(if on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }));
            crate::settings::save(&self.settings);
        }
        ui.weak("The tray icon next to the clock turns green while a run is active. Click it to bring this window back.");
        ui.add_space(6.0);
        ui.separator();
        ui.strong("Good to know");
        ui.weak("Windows does not let a normal app click or type into windows that run as administrator, so this app cannot click inside them.");
        if license::paid_tier_enabled() {
            ui.add_space(6.0);
            ui.separator();
            self.license_ui(ui);
        }
    }

    fn license_ui(&mut self, ui: &mut egui::Ui) {
        ui.strong("Unlock");
        if let Some(email) = &self.licensed_email {
            ui.label(format!("Unlocked for {email}. Thank you!"));
            return;
        }
        ui.label(format!(
            "Free: sequences of up to {} steps and {} saved profile. A one-off unlock removes both limits.",
            license::FREE_STEPS,
            license::FREE_PROFILES
        ));
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.key_input);
            if ui.button("Unlock").clicked() {
                match license::verify(&self.key_input) {
                    Some(email) => {
                        self.licensed_email = Some(email);
                        self.shared.licensed.store(true, Ordering::SeqCst);
                        self.settings.license_key = self.key_input.trim().to_string();
                        crate::settings::save(&self.settings);
                        self.message = None;
                    }
                    None => self.message = Some("That licence key is not valid.".into()),
                }
            }
        });
        if ui.button("Buy an unlock").clicked() {
            let _ = std::process::Command::new("explorer").arg(license::BUY_URL).spawn();
        }
    }

    fn profiles_tab(&mut self, ui: &mut egui::Ui) {
        let max = license::max_profiles(self.licensed());
        ui.horizontal(|ui| {
            ui.label("Save current settings as");
            ui.add(egui::TextEdit::singleline(&mut self.profile_name).hint_text("Profile name").desired_width(160.0));
            if ui.add_enabled(!self.profile_name.trim().is_empty(), egui::Button::new("Save")).clicked() {
                if profiles::upsert(&mut self.profiles, &self.profile_name, &self.config, max) {
                    self.profiles_dirty = true;
                    self.profile_name.clear();
                    self.message = None;
                } else {
                    self.message = Some(format!("The free version keeps up to {max} saved profile."));
                }
            }
        });
        ui.add_space(6.0);
        if self.profiles.is_empty() {
            ui.weak("No saved profiles yet. A profile stores everything on the Single target, Sequence and Options tabs, and can have its own hotkey that loads it and starts it.");
            return;
        }
        let mut load: Option<usize> = None;
        let mut delete: Option<usize> = None;
        let failed = winsys::failed_hotkeys();
        for (i, p) in self.profiles.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.strong(&p.name);
                if ui.button("Load").clicked() {
                    load = Some(i);
                }
                let capturing = self.capture == Some(Capture::Profile(i));
                let text = if capturing {
                    "Press a key...".to_string()
                } else {
                    p.hotkey.map_or("Set hotkey".to_string(), |h| h.label())
                };
                if ui.button(text).on_hover_text("Click, then press a key combination. Esc cancels.").clicked() {
                    self.capture = if capturing { None } else { Some(Capture::Profile(i)) };
                    self.message = None;
                }
                if p.hotkey.is_some() && ui.button("Clear").clicked() {
                    p.hotkey = None;
                    self.profiles_dirty = true;
                }
                if ui.button("Delete").clicked() {
                    delete = Some(i);
                }
            });
            if failed.contains(&(control::PROFILE_BASE + i as i32)) {
                ui.colored_label(
                    egui::Color32::from_rgb(0xD9, 0x6B, 0x4A),
                    "That hotkey is already used by another program or profile.",
                );
            }
        }
        if let Some(i) = load {
            self.config = self.profiles[i].config.clone();
            self.tab = match self.config.mode {
                Mode::Single => Tab::Single,
                Mode::Sequence => Tab::Sequence,
            };
        }
        if let Some(i) = delete {
            self.profiles.remove(i);
            self.profiles_dirty = true;
            self.capture = None;
        }
        if self.profiles.iter().any(|p| p.hotkey.is_some()) {
            ui.add_space(4.0);
            ui.weak("Pressing a profile's hotkey loads it and starts it; pressing it again stops.");
        }
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let mut auto_update = self.settings.auto_update;
            if ui
                .checkbox(&mut auto_update, "Automatically update")
                .on_hover_text("Checks for new versions about once a day and installs them in the background.")
                .changed()
            {
                self.settings.auto_update = auto_update;
                crate::settings::save(&self.settings);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!("v{}", self.version));
            });
        });
    }
}

/// Dropdown of every pressable key.
fn key_picker(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, vk: &mut u32) {
    egui::ComboBox::from_id_salt(id)
        .width(110.0)
        .selected_text(keys::key_name(*vk))
        .show_ui(ui, |ui| {
            for (name, code) in keys::key_table() {
                ui.selectable_value(vk, *code, name);
            }
        });
}

/// Interval editor in milliseconds or clicks per second.
fn rate_editor(ui: &mut egui::Ui, cfg: &mut Config) {
    ui.horizontal(|ui| {
        ui.selectable_value(&mut cfg.rate_per_second, false, "Milliseconds between");
        ui.selectable_value(&mut cfg.rate_per_second, true, "Per second");
    });
    if cfg.rate_per_second {
        let mut cps = engine::ms_to_cps(cfg.interval_ms);
        let r = ui.add(egui::DragValue::new(&mut cps).range(0.01..=1000.0).speed(0.1).suffix(" per second").max_decimals(2));
        if r.changed() {
            cfg.interval_ms = engine::cps_to_ms(cps);
        }
    } else {
        ui.add(egui::DragValue::new(&mut cfg.interval_ms).range(1..=3_600_000).speed(1.0).suffix(" ms"));
    }
    ui.weak(format!(
        "{} - between 1 ms and 1 hour. Sequences use each step's own wait instead.",
        describe_rate(cfg.interval_ms)
    ));
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
        self.sync(&ctx);
        self.poll_picker(&ctx);
        self.poll_capture(&ctx);
        if self.shared.runner.is_running() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        egui::Panel::bottom("footer").show(ui_root, |ui| self.footer(ui));
        egui::CentralPanel::default().show(ui_root, |ui| {
            self.header(ui);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
                Tab::Single => self.single_tab(ui),
                Tab::Sequence => self.sequence_tab(ui),
                Tab::Options => self.options_tab(ui),
                Tab::Profiles => self.profiles_tab(ui),
            });
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shared.runner.stop_and_wait();
        self.settings.config = self.config.clone();
        crate::settings::save(&self.settings);
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
}
