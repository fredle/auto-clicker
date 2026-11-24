//! The clicking engine: configuration, timing maths and the job loop. The
//! loop talks to the system through the `Input` trait so it can be tested
//! with a recording stand-in.

use crate::keys::VK_SPACE;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Button {
    #[default]
    Left,
    Right,
    Middle,
}

impl Button {
    pub fn label(self) -> &'static str {
        match self {
            Button::Left => "Left",
            Button::Right => "Right",
            Button::Middle => "Middle",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    Single,
    Sequence,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepAction {
    Click { button: Button, double: bool, x: i32, y: i32 },
    Key { vk: u32 },
}

/// One step of a sequence: do the action, then wait `delay_ms`.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Step {
    pub action: StepAction,
    pub delay_ms: u32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(default)]
pub struct Config {
    pub mode: Mode,
    pub click_enabled: bool,
    pub button: Button,
    pub double_click: bool,
    /// Click at the fixed point below instead of where the cursor is.
    pub use_point: bool,
    pub x: i32,
    pub y: i32,
    pub key_enabled: bool,
    pub key: u32,
    /// Keep the key held down for the whole run instead of tapping it.
    pub key_hold: bool,
    pub interval_ms: u32,
    /// Show the rate as clicks per second instead of milliseconds.
    pub rate_per_second: bool,
    /// Random variation of the interval, percent either way.
    pub interval_variation_pct: u32,
    /// Random variation of the click position, pixels either way.
    pub position_variation_px: u32,
    pub start_delay_ms: u32,
    pub stop_after_clicks: Option<u64>,
    pub stop_after_secs: Option<u32>,
    pub steps: Vec<Step>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Single,
            click_enabled: true,
            button: Button::Left,
            double_click: false,
            use_point: false,
            x: 0,
            y: 0,
            key_enabled: false,
            key: VK_SPACE,
            key_hold: false,
            interval_ms: 100,
            rate_per_second: false,
            interval_variation_pct: 0,
            position_variation_px: 0,
            start_delay_ms: 0,
            stop_after_clicks: None,
            stop_after_secs: None,
            steps: Vec::new(),
        }
    }
}

/// Why a config cannot start, or `Ok` if it can.
pub fn validate(config: &Config, licensed: bool) -> Result<(), String> {
    match config.mode {
        Mode::Single => {
            if !config.click_enabled && !config.key_enabled {
                return Err("Turn on clicking or a key press first.".into());
            }
        }
        Mode::Sequence => {
            if config.steps.is_empty() {
                return Err("Add at least one step to the sequence.".into());
            }
            let max = crate::license::max_steps(licensed);
            if config.steps.len() > max {
                return Err(format!("The free version allows up to {max} steps."));
            }
        }
    }
    Ok(())
}

pub fn cps_to_ms(cps: f64) -> u32 {
    if !cps.is_finite() || cps <= 0.0 {
        return 1000;
    }
    (1000.0 / cps).round().clamp(1.0, 3_600_000.0) as u32
}

pub fn ms_to_cps(ms: u32) -> f64 {
    1000.0 / ms.max(1) as f64
}

/// Small xorshift generator; the jitter does not need to be secure.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    pub fn from_clock() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        Rng::new(nanos)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        if hi <= lo {
            return lo;
        }
        lo + (self.next_u64() % (hi - lo + 1) as u64) as i64
    }
}

/// `base_ms` varied by up to `pct` percent either way.
pub fn jitter_ms(base_ms: u32, pct: u32, rng: &mut Rng) -> u64 {
    let base = base_ms as i64;
    let delta = base * pct.min(100) as i64 / 100;
    rng.range(base - delta, base + delta).max(0) as u64
}

/// A point moved by up to `radius` pixels in each direction.
pub fn jitter_point(x: i32, y: i32, radius: u32, rng: &mut Rng) -> (i32, i32) {
    let r = radius as i64;
    (
        (x as i64 + rng.range(-r, r)) as i32,
        (y as i64 + rng.range(-r, r)) as i32,
    )
}

/// What the engine needs from the system.
pub trait Input: Send + Sync {
    fn cursor(&self) -> (i32, i32);
    fn move_to(&self, x: i32, y: i32);
    fn click(&self, button: Button, double: bool);
    fn key_down(&self, vk: u32);
    fn key_up(&self, vk: u32);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    None,
    Manual,
    CountReached,
    TimeLimit,
}

impl Reason {
    fn from_u8(v: u8) -> Reason {
        match v {
            1 => Reason::Manual,
            2 => Reason::CountReached,
            3 => Reason::TimeLimit,
            _ => Reason::None,
        }
    }
}

/// Live numbers a running job publishes for the UI.
#[derive(Default)]
pub struct Stats {
    running: AtomicBool,
    waiting: AtomicBool,
    count: AtomicU64,
    reason: AtomicU8,
}

impl Stats {
    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
    /// Running but still inside the start delay.
    pub fn waiting(&self) -> bool {
        self.waiting.load(Ordering::SeqCst)
    }
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::SeqCst)
    }
    pub fn reason(&self) -> Reason {
        Reason::from_u8(self.reason.load(Ordering::SeqCst))
    }
}

/// Sleep until `t`; false if `stop` was raised first.
fn sleep_until(t: Instant, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let now = Instant::now();
        if now >= t {
            return true;
        }
        let remaining = t - now;
        if remaining > Duration::from_millis(2) {
            std::thread::sleep((remaining - Duration::from_millis(1)).min(Duration::from_millis(20)));
        } else {
            std::thread::yield_now();
        }
    }
}

struct Job<'a> {
    cfg: &'a Config,
    input: &'a dyn Input,
    stop: &'a AtomicBool,
    stats: &'a Stats,
    rng: Rng,
    deadline: Option<Instant>,
    /// Where "click at the cursor" is anchored when position variation is on.
    base: (i32, i32),
}

impl Job<'_> {
    /// Wait until `until`, or return why the run must end instead.
    fn wait(&self, until: Instant) -> Option<Reason> {
        let (target, limited) = match self.deadline {
            Some(d) if d <= until => (d, true),
            _ => (until, false),
        };
        if !sleep_until(target, self.stop) {
            return Some(Reason::Manual);
        }
        limited.then_some(Reason::TimeLimit)
    }

    fn check(&self) -> Option<Reason> {
        if self.stop.load(Ordering::SeqCst) {
            Some(Reason::Manual)
        } else if self.deadline.is_some_and(|d| Instant::now() >= d) {
            Some(Reason::TimeLimit)
        } else {
            None
        }
    }

    /// Count one action; Some if that reaches the click limit.
    fn counted(&self) -> Option<Reason> {
        let n = self.stats.count.fetch_add(1, Ordering::SeqCst) + 1;
        match self.cfg.stop_after_clicks {
            Some(max) if n >= max => Some(Reason::CountReached),
            _ => None,
        }
    }

    fn tap_key(&self, vk: u32, hold_ms: u64) {
        self.input.key_down(vk);
        sleep_until(Instant::now() + Duration::from_millis(hold_ms), self.stop);
        self.input.key_up(vk);
    }

    fn single_tick(&mut self) {
        let cfg = self.cfg;
        if cfg.click_enabled {
            let anchor = if cfg.use_point { Some((cfg.x, cfg.y)) } else { None };
            let radius = cfg.position_variation_px;
            let target = match anchor {
                Some((x, y)) => Some(jitter_point(x, y, radius, &mut self.rng)),
                None if radius > 0 => {
                    Some(jitter_point(self.base.0, self.base.1, radius, &mut self.rng))
                }
                None => None,
            };
            if let Some((x, y)) = target {
                self.input.move_to(x, y);
            }
            self.input.click(cfg.button, cfg.double_click);
        }
        if cfg.key_enabled && !cfg.key_hold {
            self.tap_key(cfg.key, (cfg.interval_ms as u64 / 2).clamp(1, 25));
        }
    }

    fn run_single(&mut self) -> Reason {
        let mut next = Instant::now();
        loop {
            if let Some(r) = self.check() {
                return r;
            }
            self.single_tick();
            if let Some(r) = self.counted() {
                return r;
            }
            let delay = jitter_ms(self.cfg.interval_ms, self.cfg.interval_variation_pct, &mut self.rng).max(1);
            next += Duration::from_millis(delay);
            let now = Instant::now();
            if next < now {
                // Fell behind (slow target app): do not burst to catch up.
                next = now;
            }
            if let Some(r) = self.wait(next) {
                return r;
            }
        }
    }

    fn run_sequence(&mut self) -> Reason {
        let cfg = self.cfg;
        if cfg.steps.is_empty() {
            return Reason::Manual;
        }
        loop {
            for step in &cfg.steps {
                if let Some(r) = self.check() {
                    return r;
                }
                match step.action {
                    StepAction::Click { button, double, x, y } => {
                        let (x, y) = jitter_point(x, y, cfg.position_variation_px, &mut self.rng);
                        self.input.move_to(x, y);
                        self.input.click(button, double);
                    }
                    StepAction::Key { vk } => self.tap_key(vk, 15),
                }
                if let Some(r) = self.counted() {
                    return r;
                }
                let delay = jitter_ms(step.delay_ms, cfg.interval_variation_pct, &mut self.rng).max(1);
                if let Some(r) = self.wait(Instant::now() + Duration::from_millis(delay)) {
                    return r;
                }
            }
        }
    }
}

/// Run one job to completion on the calling thread.
pub fn run_job(cfg: &Config, input: &dyn Input, stop: &AtomicBool, stats: &Stats) -> Reason {
    stats.count.store(0, Ordering::SeqCst);
    stats.waiting.store(cfg.start_delay_ms > 0, Ordering::SeqCst);
    let started_wait = Instant::now() + Duration::from_millis(cfg.start_delay_ms as u64);
    if !sleep_until(started_wait, stop) {
        return Reason::Manual;
    }
    stats.waiting.store(false, Ordering::SeqCst);
    let deadline = cfg
        .stop_after_secs
        .map(|s| Instant::now() + Duration::from_secs(s as u64));
    let mut job = Job {
        cfg,
        input,
        stop,
        stats,
        rng: Rng::from_clock(),
        deadline,
        base: input.cursor(),
    };
    let hold = cfg.mode == Mode::Single && cfg.key_enabled && cfg.key_hold;
    if hold {
        input.key_down(cfg.key);
    }
    let reason = match cfg.mode {
        Mode::Single => job.run_single(),
        Mode::Sequence => job.run_sequence(),
    };
    if hold {
        input.key_up(cfg.key);
    }
    reason
}

/// Owns at most one running job thread.
pub struct Runner {
    input: Arc<dyn Input>,
    pub stats: Arc<Stats>,
    stop: Mutex<Arc<AtomicBool>>,
    /// Called with the new running state whenever a job starts or ends.
    on_change: Arc<dyn Fn(bool) + Send + Sync>,
}

impl Runner {
    pub fn new(input: Arc<dyn Input>, on_change: Arc<dyn Fn(bool) + Send + Sync>) -> Self {
        Runner {
            input,
            stats: Arc::new(Stats::default()),
            stop: Mutex::new(Arc::new(AtomicBool::new(false))),
            on_change,
        }
    }

    pub fn is_running(&self) -> bool {
        self.stats.running()
    }

    /// Start a job unless one is already running.
    pub fn start(&self, config: Config) {
        let mut stop_slot = self.stop.lock().unwrap();
        if self.stats.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        *stop_slot = stop.clone();
        self.stats.reason.store(0, Ordering::SeqCst);
        let input = self.input.clone();
        let stats = self.stats.clone();
        let on_change = self.on_change.clone();
        std::thread::spawn(move || {
            let reason = run_job(&config, input.as_ref(), &stop, &stats);
            stats.waiting.store(false, Ordering::SeqCst);
            stats.reason.store(reason as u8, Ordering::SeqCst);
            stats.running.store(false, Ordering::SeqCst);
            on_change(false);
        });
        drop(stop_slot);
        (self.on_change)(true);
    }

    pub fn stop(&self) {
        self.stop.lock().unwrap().store(true, Ordering::SeqCst);
    }

    /// Stop and wait (briefly) until held keys have been released.
    pub fn stop_and_wait(&self) {
        self.stop();
        let give_up = Instant::now() + Duration::from_millis(500);
        while self.is_running() && Instant::now() < give_up {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Clone)]
    enum Ev {
        Move(i32, i32),
        Click(Button, bool),
        Down(u32),
        Up(u32),
    }

    #[derive(Default)]
    struct Mock {
        events: Mutex<Vec<Ev>>,
    }

    impl Mock {
        fn log(&self) -> Vec<Ev> {
            self.events.lock().unwrap().clone()
        }
    }

    impl Input for Mock {
        fn cursor(&self) -> (i32, i32) {
            (500, 400)
        }
        fn move_to(&self, x: i32, y: i32) {
            self.events.lock().unwrap().push(Ev::Move(x, y));
        }
        fn click(&self, b: Button, d: bool) {
            self.events.lock().unwrap().push(Ev::Click(b, d));
        }
        fn key_down(&self, vk: u32) {
            self.events.lock().unwrap().push(Ev::Down(vk));
        }
        fn key_up(&self, vk: u32) {
            self.events.lock().unwrap().push(Ev::Up(vk));
        }
    }

    fn quick(max: u64) -> Config {
        Config {
            interval_ms: 1,
            stop_after_clicks: Some(max),
            ..Config::default()
        }
    }

    fn run(cfg: &Config) -> (Reason, Vec<Ev>, u64) {
        let mock = Mock::default();
        let stats = Stats::default();
        let reason = run_job(cfg, &mock, &AtomicBool::new(false), &stats);
        (reason, mock.log(), stats.count())
    }

    #[test]
    fn clicks_in_place_until_the_count_is_reached() {
        let cfg = Config { button: Button::Right, double_click: true, ..quick(5) };
        let (reason, log, count) = run(&cfg);
        assert_eq!(reason, Reason::CountReached);
        assert_eq!(count, 5);
        assert_eq!(log, vec![Ev::Click(Button::Right, true); 5]);
    }

    #[test]
    fn fixed_point_moves_before_each_click() {
        let cfg = Config { use_point: true, x: 10, y: 20, ..quick(2) };
        let (_, log, _) = run(&cfg);
        assert_eq!(
            log,
            vec![
                Ev::Move(10, 20),
                Ev::Click(Button::Left, false),
                Ev::Move(10, 20),
                Ev::Click(Button::Left, false)
            ]
        );
    }

    #[test]
    fn position_variation_stays_within_the_radius() {
        let cfg = Config { use_point: true, x: 100, y: 100, position_variation_px: 3, ..quick(30) };
        let (_, log, _) = run(&cfg);
        for ev in log {
            if let Ev::Move(x, y) = ev {
                assert!((97..=103).contains(&x) && (97..=103).contains(&y));
            }
        }
        let cfg = Config { position_variation_px: 2, ..quick(10) };
        let (_, log, _) = run(&cfg);
        assert!(log.iter().any(|e| matches!(e, Ev::Move(x, _) if (498..=502).contains(x))));
    }

    #[test]
    fn key_can_be_pressed_alongside_the_click() {
        let cfg = Config { key_enabled: true, key: 0x45, ..quick(2) };
        let (_, log, _) = run(&cfg);
        assert_eq!(
            log,
            vec![
                Ev::Click(Button::Left, false),
                Ev::Down(0x45),
                Ev::Up(0x45),
                Ev::Click(Button::Left, false),
                Ev::Down(0x45),
                Ev::Up(0x45)
            ]
        );
    }

    #[test]
    fn held_key_goes_down_once_and_is_released_at_the_end() {
        let cfg = Config {
            click_enabled: false,
            key_enabled: true,
            key_hold: true,
            key: 0x57,
            ..quick(3)
        };
        let (reason, log, _) = run(&cfg);
        assert_eq!(reason, Reason::CountReached);
        assert_eq!(log, vec![Ev::Down(0x57), Ev::Up(0x57)]);
    }

    #[test]
    fn time_limit_ends_the_run() {
        let cfg = Config { stop_after_secs: Some(1), ..quick(u64::MAX) };
        let started = Instant::now();
        let (reason, log, _) = run(&cfg);
        assert_eq!(reason, Reason::TimeLimit);
        assert!(started.elapsed() < Duration::from_millis(1500));
        assert!(!log.is_empty());
    }

    #[test]
    fn stop_flag_ends_a_run_and_releases_held_keys() {
        let cfg = Config {
            click_enabled: false,
            key_enabled: true,
            key_hold: true,
            ..Config::default()
        };
        let mock = Mock::default();
        let stats = Stats::default();
        let stop = AtomicBool::new(false);
        let reason = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(40));
                stop.store(true, Ordering::SeqCst);
            });
            run_job(&cfg, &mock, &stop, &stats)
        });
        assert_eq!(reason, Reason::Manual);
        assert_eq!(mock.log().last(), Some(&Ev::Up(VK_SPACE)));
    }

    #[test]
    fn start_delay_postpones_the_first_click() {
        let cfg = Config { start_delay_ms: 80, ..quick(1) };
        let started = Instant::now();
        let (_, log, _) = run(&cfg);
        assert!(started.elapsed() >= Duration::from_millis(75));
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn sequence_repeats_its_steps_in_order() {
        let cfg = Config {
            mode: Mode::Sequence,
            steps: vec![
                Step { action: StepAction::Click { button: Button::Left, double: false, x: 1, y: 2 }, delay_ms: 1 },
                Step { action: StepAction::Key { vk: 0x41 }, delay_ms: 1 },
            ],
            stop_after_clicks: Some(5),
            ..Config::default()
        };
        let (reason, log, count) = run(&cfg);
        assert_eq!((reason, count), (Reason::CountReached, 5));
        assert_eq!(
            log,
            vec![
                Ev::Move(1, 2),
                Ev::Click(Button::Left, false),
                Ev::Down(0x41),
                Ev::Up(0x41),
                Ev::Move(1, 2),
                Ev::Click(Button::Left, false),
                Ev::Down(0x41),
                Ev::Up(0x41),
                Ev::Move(1, 2),
                Ev::Click(Button::Left, false),
            ]
        );
    }

    #[test]
    fn jitter_stays_in_bounds_and_is_off_at_zero() {
        let mut rng = Rng::new(12345);
        for _ in 0..500 {
            let v = jitter_ms(100, 20, &mut rng);
            assert!((80..=120).contains(&v), "{v}");
            assert_eq!(jitter_ms(100, 0, &mut rng), 100);
            assert!(jitter_ms(100, 500, &mut rng) <= 200);
            assert_eq!(jitter_ms(0, 50, &mut rng), 0);
        }
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            seen.insert(jitter_ms(100, 20, &mut rng));
        }
        assert!(seen.len() > 10);
    }

    #[test]
    fn rate_conversions_round_trip_and_clamp() {
        assert_eq!(cps_to_ms(10.0), 100);
        assert_eq!(cps_to_ms(1000.0), 1);
        assert_eq!(cps_to_ms(100_000.0), 1);
        assert_eq!(cps_to_ms(0.0), 1000);
        assert_eq!(cps_to_ms(f64::NAN), 1000);
        assert_eq!(ms_to_cps(250), 4.0);
        assert_eq!(ms_to_cps(0), 1000.0);
    }

    #[test]
    fn validation_explains_why_a_config_cannot_start() {
        let mut cfg = Config::default();
        assert!(validate(&cfg, false).is_ok());
        cfg.click_enabled = false;
        assert!(validate(&cfg, false).is_err());
        cfg.mode = Mode::Sequence;
        assert!(validate(&cfg, false).is_err());
        cfg.steps = vec![
            Step { action: StepAction::Key { vk: 0x41 }, delay_ms: 10 };
            crate::license::FREE_STEPS + 1
        ];
        let expected_ok = !crate::license::paid_tier_enabled();
        assert_eq!(validate(&cfg, false).is_ok(), expected_ok);
        assert!(validate(&cfg, true).is_ok());
    }

    #[test]
    fn runner_starts_once_and_stops() {
        let changes = Arc::new(AtomicU64::new(0));
        let c = changes.clone();
        let runner = Runner::new(
            Arc::new(Mock::default()),
            Arc::new(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        );
        runner.start(Config { interval_ms: 5, ..Config::default() });
        assert!(runner.is_running());
        runner.start(Config::default()); // ignored while running
        std::thread::sleep(Duration::from_millis(30));
        assert!(runner.stats.count() > 0);
        runner.stop_and_wait();
        assert!(!runner.is_running());
        assert_eq!(runner.stats.reason(), Reason::Manual);
        assert!(changes.load(Ordering::SeqCst) >= 2);
        runner.start(quick(1));
        while runner.is_running() {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(runner.stats.reason(), Reason::CountReached);
    }

    #[test]
    fn config_loads_from_partial_json() {
        let cfg: Config = serde_json::from_str(r#"{"interval_ms": 7}"#).unwrap();
        assert_eq!(cfg.interval_ms, 7);
        assert!(cfg.click_enabled);
    }
}
