//! Вікно програми (egui).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

use crate::audio::{self, DeviceInfo, Engine, EngineConfig, Shared};
use crate::dsp::SpectralMode;
use crate::preset::{self, Preset, PresetStore};
use crate::settings::{Paths, Settings};

const CABLE_URL: &str = "https://vb-audio.com/Cable/";

pub struct HolosomatApp {
    shared: Arc<Shared>,
    engine: Option<Engine>,
    engine_error: Option<String>,

    inputs: Vec<DeviceInfo>,
    outputs: Vec<DeviceInfo>,

    paths: Paths,
    settings: Settings,
    settings_dirty: Option<Instant>,

    builtin: Vec<Preset>,
    store: PresetStore,
    user_presets: Vec<(PathBuf, Preset)>,

    /// Поточний голос (те, що зараз на повзунках).
    current: Preset,
    /// Пресет, з якого почали, — щоб показати «змінено» і вміти скинути.
    loaded_from: Option<Preset>,
    save_name: String,
    confirm_delete: Option<PathBuf>,

    toast: Option<(String, Instant)>,
    meter_in: f32,
    meter_out: f32,
}

impl HolosomatApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_zoom_factor(1.1);

        let paths = Paths::detect();
        let settings = Settings::load(&paths.settings_file);
        let builtin = preset::builtin();
        let current = settings
            .current
            .clone()
            .unwrap_or_else(|| builtin[1].clone());
        let shared = Arc::new(Shared::new(current.params));
        shared
            .effect_on
            .store(settings.effect_on.unwrap_or(true), Ordering::Relaxed);
        let store = PresetStore::new(paths.presets_dir.clone());

        let mut app = Self {
            shared,
            engine: None,
            engine_error: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            paths,
            settings,
            settings_dirty: None,
            builtin,
            user_presets: store.list(),
            store,
            save_name: current.name.clone(),
            loaded_from: Some(current.clone()),
            current,
            confirm_delete: None,
            toast: None,
            meter_in: 0.0,
            meter_out: 0.0,
        };
        app.refresh_devices();

        // Перший запуск: якщо знайшли віртуальний кабель — одразу виводимо в нього.
        if app.settings.output_id.is_none() {
            app.settings.output_id = audio::find_virtual_cable(&app.outputs).map(|d| d.id);
        }
        if app.settings.monitor_id.is_none() {
            app.settings.monitor_id = audio::default_output().map(|d| d.id);
        }
        if app.settings.output_id.is_some() {
            app.start_engine();
        }
        app
    }

    // ----------------------------------------------------------------- звук

    fn refresh_devices(&mut self) {
        self.inputs = audio::list_inputs();
        self.outputs = audio::list_outputs();
    }

    fn start_engine(&mut self) {
        self.engine = None; // спершу закриваємо старі потоки
        let cfg = EngineConfig {
            input: self.settings.input_id.clone(),
            output: self.settings.output_id.clone(),
            monitor: if self.settings.monitor_enabled {
                self.settings.monitor_id.clone()
            } else {
                None
            },
        };
        match audio::start(&cfg, self.shared.clone()) {
            Ok(engine) => {
                self.engine = Some(engine);
                self.engine_error = None;
            }
            Err(e) => self.engine_error = Some(e),
        }
    }

    fn restart_if_running(&mut self) {
        if self.engine.is_some() {
            self.start_engine();
        }
        self.mark_dirty();
    }

    // -------------------------------------------------------------- пресети

    fn load_preset(&mut self, p: &Preset) {
        self.current = p.clone();
        self.loaded_from = Some(p.clone());
        self.save_name = p.name.clone();
        self.confirm_delete = None;
        self.shared.set_params(p.params);
        self.mark_dirty();
    }

    fn save_current(&mut self) {
        let name = self.save_name.trim().to_string();
        if name.is_empty() {
            self.toast("Спершу придумай назву пресету");
            return;
        }
        self.current.name = name.clone();
        match self.store.save(&self.current) {
            Ok(_) => {
                self.user_presets = self.store.list();
                self.loaded_from = Some(self.current.clone());
                self.toast(&format!("Збережено «{name}»"));
                self.mark_dirty();
            }
            Err(e) => self.toast(&e),
        }
    }

    fn toast(&mut self, msg: &str) {
        self.toast = Some((msg.to_string(), Instant::now()));
    }

    fn mark_dirty(&mut self) {
        self.settings_dirty.get_or_insert_with(Instant::now);
    }

    fn save_settings(&mut self) {
        self.settings.current = Some(self.current.clone());
        self.settings.effect_on = Some(self.shared.effect_on.load(Ordering::Relaxed));
        self.settings.save(&self.paths.settings_file);
        self.settings_dirty = None;
    }

    // ---------------------------------------------------------------- вигляд

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading(RichText::new("🎤 Голосомат").size(26.0).strong());
            ui.add_space(16.0);

            let mut on = self.shared.effect_on.load(Ordering::Relaxed);
            let (text, color) = if on {
                ("🔊  Ефект УВІМКНЕНО", Color32::from_rgb(40, 150, 70))
            } else {
                ("🔇  Ефект вимкнено", Color32::from_rgb(120, 120, 120))
            };
            let button = egui::Button::new(RichText::new(text).size(18.0).color(Color32::WHITE))
                .fill(color)
                .min_size(egui::vec2(220.0, 36.0));
            if ui
                .add(button)
                .on_hover_text("Швидко вимкнути ефект і говорити своїм голосом")
                .clicked()
            {
                on = !on;
                self.shared.effect_on.store(on, Ordering::Relaxed);
                self.mark_dirty();
            }

            ui.add_space(16.0);
            ui.vertical(|ui| {
                meter(ui, "Мікрофон", self.meter_in);
                meter(ui, "Вихід", self.meter_out);
            });
        });

        if let Some(err) = &self.engine_error {
            ui.colored_label(Color32::from_rgb(220, 70, 60), format!("⚠ {err}"));
        } else if let Some(engine) = &self.engine {
            let latency = crate::dsp::pitch::LATENCY as f32 / 48.0 + self.shared.buffer_ms();
            ui.label(
                RichText::new(format!(
                    "{}   · затримка ≈ {:.0} мс",
                    engine.summary, latency
                ))
                .small()
                .weak(),
            );
        } else {
            ui.label(RichText::new("Звук зупинено").weak());
        }
        if let Some((msg, at)) = &self.toast
            && at.elapsed() < Duration::from_secs(3)
        {
            ui.colored_label(Color32::from_rgb(60, 140, 220), format!("✔ {msg}"));
        }
        ui.add_space(4.0);
    }

    fn presets_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.heading("Голоси");
        ui.add_space(4.0);

        let mut to_load: Option<Preset> = None;
        let mut to_delete: Option<PathBuf> = None;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(ui.available_height() - 130.0)
            .show(ui, |ui| {
                for p in &self.builtin {
                    let active = self.loaded_from.as_ref().is_some_and(|l| l == p);
                    let label = RichText::new(&p.name).size(16.0);
                    let r = ui.add_sized(
                        [ui.available_width(), 28.0],
                        egui::Button::selectable(active, label),
                    );
                    if r.on_hover_text(&p.description).clicked() {
                        to_load = Some(p.clone());
                    }
                }

                ui.add_space(8.0);
                ui.separator();
                ui.label(RichText::new("⭐ Мої голоси").strong());
                if self.user_presets.is_empty() {
                    ui.label(
                        RichText::new("Поки порожньо. Налаштуй голос і збережи внизу.")
                            .weak()
                            .small(),
                    );
                }
                for (path, p) in &self.user_presets {
                    ui.horizontal(|ui| {
                        if self.confirm_delete.as_ref() == Some(path) {
                            ui.label(format!("Видалити «{}»?", p.name));
                            if ui.button("Так").clicked() {
                                to_delete = Some(path.clone());
                            }
                            if ui.button("Ні").clicked() {
                                self.confirm_delete = None;
                            }
                            return;
                        }
                        let active = self.loaded_from.as_ref().is_some_and(|l| l == p);
                        let w = ui.available_width() - 34.0;
                        let r = ui.add_sized(
                            [w, 26.0],
                            egui::Button::selectable(active, RichText::new(&p.name).size(15.0)),
                        );
                        if r.clicked() {
                            to_load = Some(p.clone());
                        }
                        if ui.small_button("🗑").on_hover_text("Видалити").clicked() {
                            self.confirm_delete = Some(path.clone());
                        }
                    });
                }
            });

        if let Some(p) = to_load {
            self.load_preset(&p);
        }
        if let Some(path) = to_delete {
            match self.store.delete(&path) {
                Ok(()) => self.toast("Видалено"),
                Err(e) => self.toast(&e),
            }
            self.confirm_delete = None;
            self.user_presets = self.store.list();
        }

        ui.separator();
        ui.label("Зберегти поточний голос як:");
        let edit =
            ui.add(egui::TextEdit::singleline(&mut self.save_name).hint_text("Назва голосу"));
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.horizontal(|ui| {
            if ui.button("💾 Зберегти").clicked() || enter {
                self.save_current();
            }
            if ui
                .button("📂 Папка")
                .on_hover_text("Відкрити папку з пресетами (.json)")
                .clicked()
            {
                let _ = std::fs::create_dir_all(self.store.dir());
                open_folder(self.store.dir());
            }
            if ui
                .button("🔄")
                .on_hover_text("Перечитати папку з пресетами")
                .clicked()
            {
                self.user_presets = self.store.list();
            }
        });
    }

    fn devices_section(&mut self, ui: &mut egui::Ui) {
        let open = self.engine.is_none() || self.engine_error.is_some();
        egui::CollapsingHeader::new(RichText::new("🎧 Пристрої").size(17.0))
            .default_open(open)
            .show(ui, |ui| {
                let mut changed = false;

                egui::Grid::new("devices").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Мікрофон:");
                    changed |= device_combo(ui, "in", &mut self.settings.input_id, &self.inputs, "За замовчуванням");
                    ui.end_row();

                    ui.label("Куди йде голос:");
                    changed |= device_combo(ui, "out", &mut self.settings.output_id, &self.outputs, "— обери —");
                    ui.end_row();

                    let mut monitor = self.settings.monitor_enabled;
                    if ui.checkbox(&mut monitor, "Чути себе в:").changed() {
                        self.settings.monitor_enabled = monitor;
                        changed = true;
                    }
                    ui.add_enabled_ui(monitor, |ui| {
                        changed |= device_combo(ui, "mon", &mut self.settings.monitor_id, &self.outputs, "— обери —");
                    });
                    ui.end_row();
                });

                if changed {
                    self.restart_if_running();
                }

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if self.engine.is_some() {
                        if ui.button("⏹ Зупинити").clicked() {
                            self.engine = None;
                        }
                    } else if ui.button(RichText::new("▶ Запустити").strong()).clicked() {
                        self.start_engine();
                    }
                    if ui.button("🔄 Оновити список").clicked() {
                        self.refresh_devices();
                    }
                });

                ui.add_space(6.0);
                if audio::find_virtual_cable(&self.outputs).is_none() {
                    ui.colored_label(
                        Color32::from_rgb(230, 160, 40),
                        "⚠ Віртуальний кабель не знайдено. Без нього гра не почує зміненого голосу.",
                    );
                    ui.hyperlink_to("Завантажити VB-Audio Virtual Cable (безкоштовно)", CABLE_URL);
                }
                ui.label(
                    RichText::new(
                        "У Discord або грі обери мікрофоном «CABLE Output». \
                         Шумозаглушення (Krisp) краще вимкнути — воно з'їдає ефекти.",
                    )
                    .small()
                    .weak(),
                );
            });
    }

    fn voice_controls(&mut self, ui: &mut egui::Ui) {
        let before = self.current.params;
        let mut params = self.current.params;
        let p = &mut params;

        ui.horizontal(|ui| {
            ui.heading(&self.current.name);
            if self.loaded_from.as_ref().is_none_or(|l| l.params != *p) {
                ui.label(RichText::new("• змінено").italics().weak());
                if ui.small_button("↩ Скинути").clicked()
                    && let Some(orig) = &self.loaded_from
                {
                    *p = orig.params;
                }
            }
        });
        if !self.current.description.is_empty() {
            ui.label(RichText::new(&self.current.description).weak());
        }
        ui.add_space(6.0);

        section(ui, "👄 Голос", |ui| {
            slider(
                ui,
                "Висота",
                &mut p.pitch,
                -12.0..=12.0,
                " півт.",
                "Вище — пискляво, нижче — басом",
            );
            slider(
                ui,
                "Тембр",
                &mut p.formant,
                -12.0..=12.0,
                " півт.",
                "«Розмір горла»: менше — дитина, більше — велетень. Від'ємне = більший",
            );
            ui.horizontal(|ui| {
                ui.label("Режим:");
                ui.radio_value(&mut p.mode, SpectralMode::Normal, "Звичайний");
                ui.radio_value(&mut p.mode, SpectralMode::Robot, "Робот");
                ui.radio_value(&mut p.mode, SpectralMode::Whisper, "Шепіт");
            });
        });

        section(ui, "✨ Ефекти", |ui| {
            slider(
                ui,
                "Хрипіння",
                &mut p.drive,
                0.0..=1.0,
                "",
                "Перевантаження, як у старому динаміку",
            );
            slider(
                ui,
                "Метал",
                &mut p.ring_mix,
                0.0..=1.0,
                "",
                "Кільцевий модулятор — робот, інопланетянин",
            );
            ui.add_enabled_ui(p.ring_mix > 0.0, |ui| {
                log_slider(
                    ui,
                    "  частота металу",
                    &mut p.ring_freq,
                    20.0..=1000.0,
                    " Гц",
                );
            });
            log_slider(ui, "Зрізати низи до", &mut p.low_cut, 20.0..=2000.0, " Гц");
            log_slider(
                ui,
                "Зрізати верхи від",
                &mut p.high_cut,
                1000.0..=20000.0,
                " Гц",
            );
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut p.crush_bits, 2..=16).text("біт"))
                    .on_hover_text("Менше біт — звук старої приставки (16 = вимкнено)");
                ui.add(egui::Slider::new(&mut p.crush_rate, 1..=32).text("пікселізація"))
                    .on_hover_text("1 = вимкнено");
            });
        });

        section(ui, "🌄 Простір", |ui| {
            slider(ui, "Луна", &mut p.echo_mix, 0.0..=1.0, "", "Гучність луни");
            ui.add_enabled_ui(p.echo_mix > 0.0, |ui| {
                slider(ui, "  затримка", &mut p.echo_ms, 20.0..=1000.0, " мс", "");
                slider(ui, "  повтори", &mut p.echo_feedback, 0.0..=0.95, "", "");
            });
            slider(
                ui,
                "Реверберація",
                &mut p.reverb_mix,
                0.0..=1.0,
                "",
                "Як у великій залі або печері",
            );
            ui.add_enabled_ui(p.reverb_mix > 0.0, |ui| {
                slider(
                    ui,
                    "  розмір кімнати",
                    &mut p.reverb_size,
                    0.0..=1.0,
                    "",
                    "",
                );
            });
        });

        section(ui, "🔈 Гучність", |ui| {
            slider(ui, "Гучність", &mut p.volume_db, -20.0..=20.0, " дБ", "");
            ui.add(
                egui::Slider::new(&mut p.gate_db, -80.0..=-20.0)
                    .text("Глушити тишу")
                    .custom_formatter(|v, _| {
                        if v <= -79.0 {
                            "вимк".into()
                        } else {
                            format!("{v:.0} дБ")
                        }
                    }),
            )
            .on_hover_text(
                "Шумовий гейт: коли мовчиш, мікрофон не передає клавіатуру й вентилятор",
            );
        });

        if params != before {
            self.current.params = params;
            self.shared.set_params(params);
            self.mark_dirty();
        }
    }
}

impl eframe::App for HolosomatApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Індикатори рівня: швидко вгору, плавно вниз.
        let (lin, lout) = self.shared.levels();
        self.meter_in = lin.max(self.meter_in * 0.85);
        self.meter_out = lout.max(self.meter_out * 0.85);

        if let Some(err) = self.shared.take_error() {
            self.engine = None;
            self.engine_error = Some(format!("{err}. Натисни «Запустити» ще раз."));
        }

        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        egui::Panel::left("presets")
            .resizable(false)
            .exact_size(250.0)
            .show(ui, |ui| self.presets_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.devices_section(ui);
                    ui.add_space(8.0);
                    self.voice_controls(ui);
                });
        });

        if self
            .settings_dirty
            .is_some_and(|t| t.elapsed() > Duration::from_secs(1))
        {
            self.save_settings();
        }
        ui.ctx().request_repaint_after(Duration::from_millis(40));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings();
    }
}

// ------------------------------------------------------------------ віджети

fn section(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(16.0).strong());
            ui.add_space(2.0);
            body(ui);
        });
    ui.add_space(6.0);
}

fn slider(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
    hint: &str,
) {
    let r = ui.add(
        egui::Slider::new(v, range)
            .text(label)
            .suffix(suffix)
            .max_decimals(2),
    );
    if !hint.is_empty() {
        r.on_hover_text(hint);
    }
}

fn log_slider(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
) {
    ui.add(
        egui::Slider::new(v, range)
            .logarithmic(true)
            .text(label)
            .suffix(suffix)
            .max_decimals(0),
    );
}

fn meter(ui: &mut egui::Ui, label: &str, level: f32) {
    // Шкала в децибелах: -60 дБ … 0 дБ.
    let db = 20.0 * level.max(1e-6).log10();
    let fill = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let color = if level > 0.95 {
        Color32::from_rgb(220, 60, 50)
    } else if db > -12.0 {
        Color32::from_rgb(230, 180, 40)
    } else {
        Color32::from_rgb(60, 170, 90)
    };
    ui.horizontal(|ui| {
        ui.add_sized([70.0, 14.0], egui::Label::new(RichText::new(label).small()));
        ui.add(
            egui::ProgressBar::new(fill)
                .desired_width(220.0)
                .desired_height(12.0)
                .fill(color),
        );
    });
}

fn device_combo(
    ui: &mut egui::Ui,
    id: &str,
    value: &mut Option<String>,
    devices: &[DeviceInfo],
    none_label: &str,
) -> bool {
    let selected = value
        .as_ref()
        .and_then(|v| devices.iter().find(|d| &d.id == v))
        .map(|d| d.name.clone())
        .unwrap_or_else(|| {
            if value.is_some() {
                "(пристрій відключено)".into()
            } else {
                none_label.into()
            }
        });
    let before = value.clone();
    egui::ComboBox::from_id_salt(id)
        .selected_text(selected)
        .width(360.0)
        .show_ui(ui, |ui| {
            if id == "in" {
                ui.selectable_value(value, None, none_label);
            }
            for d in devices {
                ui.selectable_value(value, Some(d.id.clone()), &d.name);
            }
        });
    *value != before
}

fn open_folder(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let cmd = "xdg-open";
    let _ = std::process::Command::new(cmd).arg(path).spawn();
}
