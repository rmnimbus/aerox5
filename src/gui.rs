//! egui front end. Device I/O runs on a worker thread so the window never blocks.

use crate::device::{find_devices, DeviceInfo, Mouse, Status};
use crate::profile::{Profile, Store};
use crate::protocol::{self, Action, Button, Rgb, ZONE_NAMES};
use crate::tables::{KEYS, MEDIA_KEYS};
use eframe::egui::{self, Color32, RichText, Stroke, Vec2};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

pub fn run() -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Aerox 5 Wireless")
            .with_app_id("aerox5")
            .with_inner_size([900.0, 640.0])
            .with_min_inner_size([720.0, 520.0]),
        // With vsync, eglSwapBuffers waits for the compositor's frame callback. Hyprland
        // sends none to windows on hidden workspaces, so (on NVIDIA) the UI thread blocked
        // there, stopped answering pings and Hyprland showed "not responding". egui only
        // repaints on input/requests, so running without vsync costs nothing.
        vsync: false,
        ..Default::default()
    };
    eframe::run_native("Aerox 5 Wireless", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
        .map_err(|e| anyhow::anyhow!("GUI failed: {e}"))
}

// ---------------------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------------------

enum Job {
    Refresh,
    Preview(Profile),
    Apply { profile: Profile, save: bool, name: Option<String> },
}

enum Event {
    Device(Option<DeviceInfo>, Status, Option<String>),
    Applied { saved: bool, name: Option<String> },
    Error(String),
}

fn spawn_worker(ctx: egui::Context) -> (Sender<Job>, Receiver<Event>) {
    let (job_tx, job_rx) = channel::<Job>();
    let (ev_tx, ev_rx) = channel::<Event>();
    thread::spawn(move || {
        let send = |e: Event| {
            let _ = ev_tx.send(e);
            ctx.request_repaint();
        };
        let refresh = || match Mouse::open_first() {
            Ok(mut m) => {
                let st = m.status();
                Event::Device(Some(m.info.clone()), st, None)
            }
            Err(e) => Event::Device(find_devices().into_iter().next(), Status::default(), Some(e.to_string())),
        };
        loop {
            let job = match job_rx.recv_timeout(Duration::from_secs(15)) {
                Ok(j) => j,
                Err(RecvTimeoutError::Timeout) => Job::Refresh,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            // Coalesce: while dragging a colour picker only the newest preview matters.
            let mut job = job;
            while let Ok(next) = job_rx.try_recv() {
                match (&job, &next) {
                    (Job::Preview(_), Job::Preview(_)) | (Job::Refresh, _) => job = next,
                    _ => {
                        run_job(job, &send, &refresh);
                        job = next;
                    }
                }
            }
            run_job(job, &send, &refresh);
        }
    });
    (job_tx, ev_rx)
}

fn run_job(job: Job, send: &dyn Fn(Event), refresh: &dyn Fn() -> Event) {
    match job {
        Job::Refresh => send(refresh()),
        Job::Preview(p) => {
            if let Err(e) = Mouse::open_first().and_then(|mut m| m.apply_lighting(&p)) {
                send(Event::Error(format!("preview failed: {e}")));
            }
        }
        Job::Apply { profile, save, name } => {
            match Mouse::open_first().and_then(|mut m| m.apply(&profile, save)) {
                Ok(()) => send(Event::Applied { saved: save, name }),
                Err(e) => send(Event::Error(format!("apply failed: {e:#}"))),
            }
            // The mouse doesn't answer queries for a moment after writing its flash.
            thread::sleep(Duration::from_millis(500));
            send(refresh());
        }
    }
}

// ---------------------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Buttons,
    Lighting,
    Sensitivity,
    Power,
    Profiles,
}

struct App {
    store: Option<Store>,
    names: Vec<String>,
    current: String,
    profile: Profile,
    saved_copy: Profile,
    tab: Tab,
    jobs: Sender<Job>,
    events: Receiver<Event>,
    device: Option<DeviceInfo>,
    status: Status,
    device_error: Option<String>,
    message: Option<(String, bool, Instant)>,
    live_preview: bool,
    busy: bool,
    new_name: String,
    rivalcfg_path: String,
    key_filter: String,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> App {
        cc.egui_ctx.set_zoom_factor(1.1);
        let (jobs, events) = spawn_worker(cc.egui_ctx.clone());
        let _ = jobs.send(Job::Refresh);
        let store = Store::open().ok();
        let (current, profile, msg) = match store.as_ref().map(|s| s.ensure_default().and_then(|n| Ok((s.load(&n)?, n)))) {
            Some(Ok((p, n))) => (n, p, None),
            Some(Err(e)) => ("default".into(), Profile::default(), Some(format!("could not load profile: {e:#}"))),
            None => ("default".into(), Profile::default(), Some("no config directory; profiles can't be saved".into())),
        };
        let names = store.as_ref().map(|s| s.list()).unwrap_or_default();
        let rivalcfg_path = dirs::config_dir()
            .map(|d| d.join("rivalcfg/1038_1854.device.json").display().to_string())
            .unwrap_or_default();
        App {
            store,
            names,
            current,
            saved_copy: profile.clone(),
            profile,
            tab: Tab::Buttons,
            jobs,
            events,
            device: None,
            status: Status::default(),
            device_error: None,
            message: msg.map(|m| (m, true, Instant::now())),
            live_preview: true,
            busy: false,
            new_name: String::new(),
            rivalcfg_path,
            key_filter: String::new(),
        }
    }

    fn info(&mut self, m: impl Into<String>) {
        self.message = Some((m.into(), false, Instant::now()));
    }
    fn error(&mut self, m: impl Into<String>) {
        self.message = Some((m.into(), true, Instant::now()));
    }

    fn dirty(&self) -> bool {
        self.profile != self.saved_copy
    }

    fn save_profile(&mut self) -> bool {
        let Some(store) = &self.store else { return false };
        match store.save(&self.current, &self.profile) {
            Ok(()) => {
                self.saved_copy = self.profile.clone();
                self.names = store.list();
                true
            }
            Err(e) => {
                let msg = format!("saving profile failed: {e:#}");
                self.error(msg);
                false
            }
        }
    }

    fn switch_to(&mut self, name: String) {
        let Some(store) = &self.store else { return };
        match store.load(&name) {
            Ok(p) => {
                self.profile = p.clone();
                self.saved_copy = p;
                self.current = name;
            }
            Err(e) => {
                let msg = format!("{e:#}");
                self.error(msg);
            }
        }
    }

    fn apply(&mut self, save: bool) {
        let errs = self.profile.validate();
        if !errs.is_empty() {
            self.error(format!("Can't apply: {}", errs.join("; ")));
            return;
        }
        if save && !self.save_profile() {
            return;
        }
        self.busy = true;
        let _ = self.jobs.send(Job::Apply {
            profile: self.profile.clone(),
            save,
            name: save.then(|| self.current.clone()),
        });
    }

    fn poll_events(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                Event::Device(d, s, err) => {
                    self.device = d;
                    self.status = s;
                    self.device_error = err;
                }
                Event::Applied { saved, name } => {
                    self.busy = false;
                    if let (Some(n), Some(store)) = (&name, &self.store) {
                        let _ = store.set_active(n);
                    }
                    self.info(if saved {
                        "Applied and saved in the mouse — it keeps these settings without this app."
                    } else {
                        "Applied for this session (not stored in the mouse)."
                    });
                }
                Event::Error(e) => {
                    self.busy = false;
                    self.error(e);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------------------

fn to_c32(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

fn hue(h: f32) -> Color32 {
    let c: egui::ecolor::Hsva = egui::ecolor::Hsva::new(h.rem_euclid(1.0), 1.0, 1.0, 1.0);
    c.into()
}

/// A top-down sketch of the mouse with the three-LED strip in its current colours.
fn mouse_preview(ui: &mut egui::Ui, l: &crate::profile::Lighting, time: f64) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(170.0, 250.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    let body = rect.shrink2(Vec2::new(20.0, 6.0));
    let shell = ui.visuals().widgets.inactive.bg_fill;
    let line = ui.visuals().widgets.noninteractive.fg_stroke.color;
    p.rect(body, egui::CornerRadius::same(70), shell, Stroke::new(1.5_f32, line), egui::StrokeKind::Inside);
    // buttons split + wheel
    let mid = body.center().x;
    p.line_segment([egui::pos2(mid, body.top() + 4.0), egui::pos2(mid, body.top() + 95.0)], Stroke::new(1.5_f32, line));
    let wheel = egui::Rect::from_center_size(egui::pos2(mid, body.top() + 45.0), Vec2::new(12.0, 30.0));
    p.rect(wheel, egui::CornerRadius::same(6), ui.visuals().extreme_bg_color, Stroke::new(1.0_f32, line), egui::StrokeKind::Inside);
    // light strip: three segments under the palm
    for i in 0..3 {
        let y = body.top() + 120.0 + i as f32 * 30.0;
        let seg = egui::Rect::from_center_size(egui::pos2(mid, y), Vec2::new(60.0, 22.0));
        let col = if l.rainbow { hue((time * 0.25) as f32 + i as f32 * 0.12) } else { to_c32(l.zones[i]) };
        p.rect_filled(seg.expand(4.0), egui::CornerRadius::same(9), col.gamma_multiply(0.25));
        p.rect_filled(seg, egui::CornerRadius::same(6), col);
    }
}

fn action_category(a: &Action) -> usize {
    match a {
        Action::Mouse(_) => 0,
        Action::Disabled | Action::DpiCycle | Action::ScrollUp | Action::ScrollDown => 1,
        Action::Key(_) => 2,
        Action::Media(_) => 3,
    }
}
const CATEGORIES: [&str; 4] = ["Mouse button", "Special", "Keyboard key", "Media key"];

/// Two drop-downs (category, value). Returns true when the action changed.
fn action_editor(ui: &mut egui::Ui, id: Button, a: &mut Action, filter: &mut String) -> bool {
    let before = a.clone();
    let mut cat = action_category(a);
    egui::ComboBox::from_id_salt(("cat", id)).width(130.0).selected_text(CATEGORIES[cat]).show_ui(ui, |ui| {
        for (i, c) in CATEGORIES.iter().enumerate() {
            ui.selectable_value(&mut cat, i, *c);
        }
    });
    if cat != action_category(a) {
        *a = match cat {
            0 => Action::Mouse(1),
            1 => Action::Disabled,
            2 => Action::Key("A".into()),
            _ => Action::Media("PlayPause".into()),
        };
    }
    let selected = match &*a {
        Action::Key(k) | Action::Media(k) => k.clone(),
        other => other.describe(),
    };
    egui::ComboBox::from_id_salt(("val", id)).width(190.0).height(360.0).selected_text(selected).show_ui(
        ui,
        |ui| match cat {
            0 => {
                for n in 1..=5 {
                    let v = Action::Mouse(n);
                    let label = v.describe();
                    ui.selectable_value(a, v, label);
                }
            }
            1 => {
                for v in [Action::Disabled, Action::DpiCycle, Action::ScrollUp, Action::ScrollDown] {
                    let label = v.describe();
                    ui.selectable_value(a, v, label);
                }
            }
            2 => {
                ui.add(egui::TextEdit::singleline(filter).hint_text("filter…").desired_width(170.0));
                let f = filter.to_lowercase();
                for (k, _) in KEYS.iter().filter(|(k, _)| f.is_empty() || k.to_lowercase().contains(&f)) {
                    ui.selectable_value(a, Action::Key(k.to_string()), *k);
                }
            }
            _ => {
                for (k, _) in MEDIA_KEYS {
                    ui.selectable_value(a, Action::Media(k.to_string()), *k);
                }
            }
        },
    );
    *a != before
}

fn rgb_edit(ui: &mut egui::Ui, c: &mut Rgb) -> bool {
    let mut arr = [c.0, c.1, c.2];
    let changed = ui.color_edit_button_srgb(&mut arr).changed();
    let mut hex = c.hex();
    let hex_resp = ui.add(egui::TextEdit::singleline(&mut hex).desired_width(70.0).font(egui::TextStyle::Monospace));
    if changed {
        *c = Rgb(arr[0], arr[1], arr[2]);
        return true;
    }
    if hex_resp.changed() {
        if let Some(n) = Rgb::parse(&hex) {
            *c = n;
            return true;
        }
    }
    false
}

impl App {
    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Aerox 5 Wireless");
            ui.separator();
            match &self.device {
                Some(d) if self.device_error.is_none() => {
                    ui.label(RichText::new("●").color(Color32::from_rgb(80, 200, 120)));
                    ui.label(format!("Connected · {}", d.mode.label()));
                    if !d.edition.is_empty() {
                        ui.label(format!("· {} Edition", d.edition));
                    }
                    match self.status.battery {
                        Some((lvl, chg)) => {
                            ui.separator();
                            ui.add(egui::ProgressBar::new(lvl as f32 / 100.0).desired_width(80.0).text(format!("{lvl}%")));
                            if chg {
                                ui.label("charging");
                            }
                        }
                        None => {
                            ui.separator();
                            ui.weak("battery n/a");
                        }
                    }
                    if let Some(fw) = &self.status.firmware {
                        ui.separator();
                        ui.weak(format!("fw {fw}"));
                    }
                }
                _ => {
                    ui.label(RichText::new("●").color(Color32::from_rgb(220, 80, 80)));
                    ui.label(self.device_error.clone().unwrap_or_else(|| "Mouse not found".into()));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("⟳").on_hover_text("Re-detect the mouse").clicked() {
                    let _ = self.jobs.send(Job::Refresh);
                }
            });
        });
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Profile");
            let mut pick = None;
            egui::ComboBox::from_id_salt("profile").width(150.0).selected_text(&self.current).show_ui(ui, |ui| {
                for n in &self.names {
                    if ui.selectable_label(*n == self.current, n).clicked() {
                        pick = Some(n.clone());
                    }
                }
            });
            if let Some(n) = pick {
                if n != self.current {
                    self.switch_to(n);
                }
            }
            if self.dirty() {
                ui.label(RichText::new("● unsaved").color(Color32::from_rgb(230, 170, 60)));
                if ui.button("Revert").clicked() {
                    self.profile = self.saved_copy.clone();
                    if self.live_preview {
                        let _ = self.jobs.send(Job::Preview(self.profile.clone()));
                    }
                }
                if ui.button("Save profile").clicked() && self.save_profile() {
                    self.info(format!("Saved profile '{}' (not yet applied to the mouse).", self.current));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can = self.device.is_some() && !self.busy;
                let apply = ui
                    .add_enabled(can, egui::Button::new(RichText::new("Apply & save to mouse").strong()).min_size(Vec2::new(0.0, 28.0)))
                    .on_hover_text("Saves this profile, sends every setting and stores it in the mouse's memory");
                if apply.clicked() {
                    self.apply(true);
                }
                if ui
                    .add_enabled(can, egui::Button::new("Try without saving"))
                    .on_hover_text("Send every setting, but the mouse reverts on power-off")
                    .clicked()
                {
                    self.apply(false);
                }
                if self.busy {
                    ui.spinner();
                }
            });
        });
        if let Some((m, err, _)) = &self.message {
            let col = if *err { Color32::from_rgb(230, 90, 90) } else { ui.visuals().text_color() };
            ui.label(RichText::new(m).color(col));
        }
        ui.add_space(2.0);
    }

    fn tab_buttons(&mut self, ui: &mut egui::Ui) {
        ui.heading("Button mapping");
        ui.weak("Stored in the mouse, so the mapping works on any computer and before login.");
        ui.add_space(8.0);
        egui::Grid::new("buttons").num_columns(3).spacing([12.0, 8.0]).striped(true).show(ui, |ui| {
            for b in Button::ALL {
                ui.label(b.label());
                let mut a = self.profile.action(b);
                ui.horizontal(|ui| {
                    if action_editor(ui, b, &mut a, &mut self.key_filter) {
                        self.profile.set_action(b, a.clone());
                    }
                });
                if a != b.default_action() {
                    if ui.small_button("↺").on_hover_text(format!("Default: {}", b.default_action().describe())).clicked() {
                        self.profile.set_action(b, b.default_action());
                    }
                } else {
                    ui.label("");
                }
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        if !self.profile.button_array().contains(&Action::Mouse(1)) {
            ui.colored_label(Color32::from_rgb(230, 90, 90), "⚠ Nothing is mapped to left click — applying is blocked.");
        }
        ui.horizontal(|ui| {
            if ui.button("Reset all buttons").clicked() {
                for b in Button::ALL {
                    self.profile.set_action(b, b.default_action());
                }
            }
            ui.weak("Keyboard keys use US-QWERTY positions (like rivalcfg). One key per button; no combos or macros.");
        });
    }

    fn tab_lighting(&mut self, ui: &mut egui::Ui) {
        let time = ui.input(|i| i.time);
        let mut changed = false;
        ui.heading("Lighting");
        ui.add_space(6.0);
        ui.horizontal_top(|ui| {
            mouse_preview(ui, &self.profile.lighting, time);
            ui.add_space(16.0);
            ui.vertical(|ui| {
                let l = &mut self.profile.lighting;
                ui.label(RichText::new("Effect").strong());
                ui.horizontal(|ui| {
                    changed |= ui.radio_value(&mut l.rainbow, false, "Static colours").changed();
                    changed |= ui.radio_value(&mut l.rainbow, true, "Rainbow").changed();
                });
                ui.add_space(8.0);
                ui.add_enabled_ui(!l.rainbow, |ui| {
                    egui::Grid::new("zones").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        for (i, name) in ZONE_NAMES.iter().enumerate() {
                            ui.label(format!("{name} LED"));
                            ui.horizontal(|ui| changed |= rgb_edit(ui, &mut l.zones[i]));
                            ui.end_row();
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Use top colour for all").clicked() {
                            l.zones = [l.zones[0]; 3];
                            changed = true;
                        }
                        if ui.button("Lights off").clicked() {
                            l.zones = [Rgb(0, 0, 0); 3];
                            changed = true;
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.weak("Presets:");
                        let presets: [(&str, [Rgb; 3]); 6] = [
                            ("SteelSeries", [Rgb(255, 0, 0), Rgb(0, 255, 0), Rgb(0, 0, 255)]),
                            ("Sunset", [Rgb(255, 60, 0), Rgb(255, 0, 90), Rgb(120, 0, 255)]),
                            ("Ocean", [Rgb(0, 255, 200), Rgb(0, 140, 255), Rgb(0, 30, 255)]),
                            ("Forest", [Rgb(150, 255, 0), Rgb(0, 200, 60), Rgb(0, 120, 80)]),
                            ("Ice", [Rgb(255, 255, 255), Rgb(160, 220, 255), Rgb(60, 140, 255)]),
                            ("Ember", [Rgb(255, 180, 0), Rgb(255, 80, 0), Rgb(200, 0, 0)]),
                        ];
                        for (name, z) in presets {
                            if ui.small_button(name).clicked() {
                                l.zones = z;
                                changed = true;
                            }
                        }
                    });
                });
                ui.add_space(12.0);
                ui.label(RichText::new("Reactive").strong());
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(&mut l.reactive, "Flash on click").changed();
                    ui.add_enabled_ui(l.reactive, |ui| changed |= rgb_edit(ui, &mut l.reactive_color));
                });
                ui.add_space(12.0);
                ui.checkbox(&mut self.live_preview, "Live preview on the mouse")
                    .on_hover_text("Send lighting changes immediately (not stored in the mouse until you apply & save)");
            });
        });
        // Animate the rainbow preview only while the window has focus.
        if self.profile.lighting.rainbow && ui.input(|i| i.viewport().focused.unwrap_or(false)) {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }
        if changed && self.live_preview && self.device.is_some() {
            let _ = self.jobs.send(Job::Preview(self.profile.clone()));
        }
    }

    fn tab_sensitivity(&mut self, ui: &mut egui::Ui) {
        ui.heading("Sensitivity (CPI)");
        ui.weak("Up to 5 presets; the CPI button cycles through them. 100–18 000 in steps of 100.");
        ui.add_space(8.0);
        let s = &mut self.profile.sensitivity;
        let mut remove = None;
        egui::Grid::new("dpi").num_columns(4).spacing([10.0, 6.0]).show(ui, |ui| {
            for i in 0..s.presets.len() {
                ui.radio_value(&mut s.active, i, format!("Preset {}", i + 1)).on_hover_text("Active after applying");
                let mut v = s.presets[i];
                ui.add(
                    egui::Slider::new(&mut v, protocol::DPI_MIN..=protocol::DPI_MAX)
                        .step_by(protocol::DPI_STEP as f64)
                        .logarithmic(true)
                        .suffix(" CPI"),
                );
                s.presets[i] = protocol::snap_dpi(v);
                if s.presets.len() > 1 && ui.small_button("✖").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
        if let Some(i) = remove {
            s.presets.remove(i);
            if s.active >= s.presets.len() {
                s.active = s.presets.len() - 1;
            } else if s.active > i {
                s.active -= 1;
            }
        }
        if s.presets.len() < protocol::MAX_DPI_PRESETS && ui.button("+ Add preset").clicked() {
            let next = s.presets.last().map(|d| protocol::snap_dpi(d * 2)).unwrap_or(800);
            s.presets.push(next);
        }
        ui.add_space(16.0);
        ui.heading("Polling rate");
        ui.horizontal(|ui| {
            for r in protocol::POLLING_RATES {
                ui.radio_value(&mut s.polling_rate, *r, format!("{r} Hz"));
            }
        });
        ui.weak("Lower rates save battery in wireless mode.");
    }

    fn tab_power(&mut self, ui: &mut egui::Ui) {
        ui.heading("Power");
        ui.add_space(8.0);
        let p = &mut self.profile.power;
        egui::Grid::new("power").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
            ui.label("Sleep after idle");
            ui.add(egui::Slider::new(&mut p.sleep_minutes, 0..=protocol::SLEEP_MAX_MIN).suffix(" min"))
                .on_hover_text("0 = never sleep");
            ui.end_row();
            ui.label("Dim LEDs after idle");
            ui.add(egui::Slider::new(&mut p.dim_seconds, 0..=protocol::DIM_MAX_S).suffix(" s"))
                .on_hover_text("0 = never dim");
            ui.end_row();
        });
        ui.weak("0 disables the timer. Sleeping only happens on battery (2.4 GHz).");
    }

    fn tab_profiles(&mut self, ui: &mut egui::Ui) {
        ui.heading("Profiles");
        ui.weak("The mouse holds one configuration; profiles are saved here and applied on demand.");
        if let Some(store) = &self.store {
            ui.weak(format!("Folder: {}", store.dir.join("profiles").display()));
        }
        ui.add_space(8.0);
        let active = self.store.as_ref().and_then(|s| s.active());
        let mut open = None;
        let mut delete = None;
        egui::Grid::new("profiles").num_columns(3).spacing([12.0, 6.0]).striped(true).show(ui, |ui| {
            for n in &self.names {
                let mut label = RichText::new(n);
                if *n == self.current {
                    label = label.strong();
                }
                ui.label(label);
                ui.label(if Some(n) == active.as_ref() { "in mouse" } else { "" });
                ui.horizontal(|ui| {
                    if *n != self.current && ui.small_button("Edit").clicked() {
                        open = Some(n.clone());
                    }
                    if self.names.len() > 1 && ui.small_button("Delete").clicked() {
                        delete = Some(n.clone());
                    }
                });
                ui.end_row();
            }
        });
        if let Some(n) = open {
            self.switch_to(n);
        }
        if let (Some(n), Some(store)) = (delete, &self.store) {
            match store.delete(&n) {
                Ok(()) => {
                    self.names = store.list();
                    if n == self.current {
                        let first = self.names[0].clone();
                        self.switch_to(first);
                    }
                    self.info(format!("Deleted profile '{n}'."));
                }
                Err(e) => self.error(format!("{e:#}")),
            }
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.new_name).hint_text("new profile name").desired_width(180.0));
            let ok = Store::valid_name(self.new_name.trim()) && !self.names.contains(&self.new_name.trim().to_string());
            if ui.add_enabled(ok, egui::Button::new("Save current settings as new")).clicked() {
                self.current = self.new_name.trim().to_string();
                if self.save_profile() {
                    self.info(format!("Created profile '{}'.", self.current));
                    self.new_name.clear();
                }
            }
        });
        ui.add_space(12.0);
        ui.collapsing("Import from rivalcfg", |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.rivalcfg_path).desired_width(360.0));
                if ui.button("Load into editor").clicked() {
                    match Profile::from_rivalcfg(std::path::Path::new(self.rivalcfg_path.trim())) {
                        Ok(p) => {
                            self.profile = p;
                            self.info("Imported. Review, then save or apply.");
                        }
                        Err(e) => self.error(format!("{e:#}")),
                    }
                }
            });
        });
        ui.collapsing("Command line", |ui| {
            ui.monospace("aerox5 apply <profile>     # apply + save to the mouse");
            ui.monospace("aerox5 info                # battery / firmware");
            ui.monospace("aerox5 profiles | show | actions | reset");
            ui.weak("Bind `aerox5 apply <profile>` to a key in your compositor to switch profiles quickly.");
        });
        ui.add_space(12.0);
        if ui.button("Load factory defaults into editor").clicked() {
            self.profile = Profile::default();
            self.info("Factory defaults loaded — apply to send them to the mouse.");
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events();
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(6.0);
            self.header(ui);
            ui.add_space(6.0);
        });
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| self.footer(ui));
        egui::SidePanel::left("tabs").resizable(false).exact_width(150.0).show(ctx, |ui| {
            ui.add_space(10.0);
            for (t, name) in [
                (Tab::Buttons, "🖱  Buttons"),
                (Tab::Lighting, "💡  Lighting"),
                (Tab::Sensitivity, "🎯  Sensitivity"),
                (Tab::Power, "🔋  Power"),
                (Tab::Profiles, "🗂  Profiles"),
            ] {
                let r = ui.add_sized([ui.available_width(), 30.0], egui::SelectableLabel::new(self.tab == t, name));
                if r.clicked() {
                    self.tab = t;
                }
            }
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match self.tab {
                Tab::Buttons => self.tab_buttons(ui),
                Tab::Lighting => self.tab_lighting(ui),
                Tab::Sensitivity => self.tab_sensitivity(ui),
                Tab::Power => self.tab_power(ui),
                Tab::Profiles => self.tab_profiles(ui),
            });
        });
        // Drop the status message when it expires; wake up exactly once for that.
        if let Some((_, err, t)) = &self.message {
            let ttl = Duration::from_secs(if *err { 20 } else { 6 });
            match ttl.checked_sub(t.elapsed()) {
                Some(left) => ctx.request_repaint_after(left + Duration::from_millis(50)),
                None => self.message = None,
            }
        }
    }
}
