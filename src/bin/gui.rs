#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[path = "gui/model.rs"]
mod model;

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, FontId, RichText, Vec2};
use model::{BANDS, MAX_LENGTH, MAX_WORDS, NumberInput, Settings, Suggestion};
use ui_observation_egui_031::{
    ContextObservationExt, ResponseObservationExt, ScrollAreaObservationExt,
};
use ui_observation_protocol::{
    Content, ControlMetadata, ElementKey, ElementSpec, InteractionKind, RegionSpec, Role,
};
use ui_observation_runtime::ObservationRuntime;

const DEBOUNCE: Duration = Duration::from_millis(140);

struct Request {
    revision: u64,
    settings: Settings,
}
struct Reply {
    revision: u64,
    result: Result<Suggestion, String>,
}

struct PasswordApp {
    context: egui::Context,
    settings: Settings,
    length: NumberInput,
    words: NumberInput,
    active: Option<Settings>,
    revision: u64,
    scheduled: Option<Instant>,
    working: bool,
    requests: Sender<Request>,
    replies: Receiver<Reply>,
    suggestion: Option<Suggestion>,
    error: Option<String>,
    copied_until: Option<Instant>,
}

impl PasswordApp {
    fn new(ctx: &egui::Context) -> Self {
        ctx.install_observation_runtime(ObservationRuntime::from_inherited_v1());
        ctx.set_theme(egui::ThemePreference::System);
        ctx.all_styles_mut(|style| {
            style
                .text_styles
                .insert(egui::TextStyle::Body, FontId::proportional(21.0));
            style
                .text_styles
                .insert(egui::TextStyle::Button, FontId::proportional(22.0));
            style
                .text_styles
                .insert(egui::TextStyle::Heading, FontId::proportional(32.0));
            style
                .text_styles
                .insert(egui::TextStyle::Small, FontId::proportional(17.0));
            style
                .text_styles
                .insert(egui::TextStyle::Monospace, FontId::monospace(23.0));
            style.spacing.item_spacing = Vec2::new(14.0, 12.0);
            style.spacing.interact_size = Vec2::new(48.0, 38.0);
            style.spacing.slider_width = 380.0;
            style.visuals.selection.bg_fill = Color32::from_rgb(25, 66, 150);
            style.visuals.selection.stroke.color = Color32::WHITE;
        });
        let (requests, work) = mpsc::channel::<Request>();
        let (results, replies) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            while let Ok(mut request) = work.recv() {
                // Slider movements may queue work; only calculate the newest settings.
                for newer in work.try_iter() {
                    request = newer;
                }
                let reply = Reply {
                    revision: request.revision,
                    result: request.settings.generate(),
                };
                if results.send(reply).is_err() {
                    break;
                }
                repaint.request_repaint();
            }
        });
        let settings = Settings::default();
        Self {
            context: ctx.clone(),
            length: NumberInput::new(settings.max_length, 4, MAX_LENGTH),
            words: NumberInput::new(settings.max_words, 1, MAX_WORDS),
            settings,
            active: None,
            revision: 0,
            scheduled: None,
            working: false,
            requests,
            replies,
            suggestion: None,
            error: None,
            copied_until: None,
        }
    }

    fn valid_settings(&self) -> Option<Settings> {
        let mut settings = self.settings.clone();
        settings.max_length = self.length.parsed()?;
        if settings.limit_words {
            settings.max_words = self.words.parsed()?;
        }
        if !settings.languages.iter().any(|enabled| *enabled) {
            return None;
        }
        Some(settings)
    }

    fn refresh_settings(&mut self) {
        let next = self.valid_settings();
        if next != self.active {
            self.revision += 1;
            self.active = next;
            self.suggestion = None;
            self.error = None;
            self.copied_until = None;
            self.working = false;
            self.scheduled = self.active.as_ref().map(|_| Instant::now());
        }
    }

    fn send_request(&mut self) {
        if let Some(settings) = self.active.clone() {
            self.revision += 1;
            self.suggestion = None;
            self.error = None;
            self.copied_until = None;
            self.scheduled = None;
            match self.requests.send(Request {
                revision: self.revision,
                settings,
            }) {
                Ok(()) => self.working = true,
                Err(_) => self.error = Some("Please close and reopen Four Words.".into()),
            }
        }
    }

    fn accept_replies(&mut self) {
        for reply in self.replies.try_iter() {
            if reply.revision != self.revision {
                continue;
            }
            self.working = false;
            match reply.result {
                Ok(suggestion) => {
                    self.suggestion = Some(suggestion);
                    self.error = None;
                }
                Err(error) => {
                    self.suggestion = None;
                    self.error = Some(error);
                }
            }
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Password settings").strong());
        ui.label("Maximum characters");
        number_control(ui, "character_limit", &mut self.length);
        ui.add_space(2.0);
        let _ = ui
            .checkbox(&mut self.settings.limit_words, "Limit the number of words")
            .observe(
                spec(
                    "settings.words.enabled",
                    Role::Button,
                    "Limit the number of words",
                )
                .with_selected(self.settings.limit_words),
            );
        ui.add_enabled_ui(self.settings.limit_words, |ui| {
            number_control(ui, "word_limit", &mut self.words);
        });
        ui.add_space(2.0);
        ui.label("Word languages");
        ui.horizontal_wrapped(|ui| {
            for (enabled, (key, name)) in self.settings.languages.iter_mut().zip([
                ("english", "English"),
                ("french", "French"),
                ("german", "German"),
            ]) {
                let _ = ui.checkbox(enabled, name).observe(
                    spec(&format!("settings.language.{key}"), Role::Button, name)
                        .with_selected(*enabled),
                );
            }
        });
        if !self.settings.languages.iter().any(|enabled| *enabled) {
            ui.colored_label(color(0), "Choose at least one language.");
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Between words:");
            let _ = ui
                .radio_value(&mut self.settings.spaces, false, "Hyphen (-)")
                .observe(
                    spec("settings.separator.hyphen", Role::Button, "Hyphen (-)")
                        .with_selected(!self.settings.spaces),
                );
            let _ = ui
                .radio_value(&mut self.settings.spaces, true, "Space")
                .observe(
                    spec("settings.separator.space", Role::Button, "Space")
                        .with_selected(self.settings.spaces),
                );
        });
    }

    fn entropy(&self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Entropy with these settings").strong());
        if let Some(suggestion) = &self.suggestion {
            let index = model::band_index(suggestion.bits);
            let [r, g, b] = model::entropy_rgb(suggestion.bits);
            let achieved_color = Color32::from_rgb(r, g, b);
            ui.horizontal_wrapped(|ui| {
                let bits = format!("{:.2} bits", suggestion.bits);
                let _ = ui.label(RichText::new(&bits).size(36.0).strong()).observe(
                    spec("result.entropy", Role::Status, "Entropy").with_content(public(&bits)),
                );
                egui::Frame::new()
                    .fill(achieved_color)
                    .corner_radius(8)
                    .inner_margin(9)
                    .show(ui, |ui| {
                        let text_color = readable_text([r, g, b]);
                        let _ = ui
                            .label(RichText::new(BANDS[index].label).color(text_color).strong())
                            .observe(
                                spec("result.classification", Role::Status, "Classification")
                                    .with_content(public(BANDS[index].label)),
                            )
                            .observe(
                                spec("result.entropy.color", Role::Status, "Entropy color")
                                    .with_content(public(&format!("#{r:02x}{g:02x}{b:02x}"))),
                            );
                    });
            });
            if index == 4 {
                ui.label(RichText::new("What James uses").small());
            }
        } else {
            ui.label(RichText::new("— bits").size(36.0).strong());
        }
    }

    fn password(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Your password").strong());
        let password = self
            .suggestion
            .as_ref()
            .map_or("", |suggestion| suggestion.password.as_str());
        let mut selectable = password;
        let _ = ui
            .add(
                egui::TextEdit::multiline(&mut selectable)
                    .id_salt("password")
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .desired_rows(5)
                    .hint_text(if self.working || self.scheduled.is_some() {
                        "Making your password…"
                    } else {
                        "Choose your settings above."
                    }),
            )
            .observe(
                spec("result.password", Role::Label, "Your password")
                    .with_content(Content::private_omitted(password.len() as u64)),
            );
        if let Some(suggestion) = &self.suggestion {
            let counts = format!(
                "{} characters  ·  {} words",
                suggestion.password.len(),
                suggestion.words
            );
            let _ = ui.label(&counts).observe(
                spec("result.length", Role::Status, "Password length")
                    .with_content(public(&counts)),
            );
        }
        if let Some(error) = &self.error {
            ui.colored_label(color(0), error);
        }
        ui.horizontal_wrapped(|ui| {
            let can_generate = self.active.is_some() && !self.working && self.scheduled.is_none();
            if ui
                .add_enabled(
                    can_generate,
                    egui::Button::new("New password").min_size(Vec2::new(210.0, 54.0)),
                )
                .observe(spec("action.generate", Role::Button, "New password"))
                .clicked()
            {
                self.send_request();
            }
            let copied = self
                .copied_until
                .is_some_and(|until| Instant::now() < until);
            let caption = if copied { "Copied!" } else { "Copy password" };
            if ui
                .add_enabled(
                    self.suggestion.is_some(),
                    egui::Button::new(RichText::new(caption).color(Color32::WHITE))
                        .fill(Color32::from_rgb(25, 66, 150))
                        .min_size(Vec2::new(200.0, 54.0)),
                )
                .observe(
                    spec("action.copy", Role::Button, "Copy password")
                        .with_content(public(caption)),
                )
                .clicked()
                && let Some(suggestion) = &self.suggestion
            {
                ui.ctx().copy_text(suggestion.password.clone());
                self.copied_until = Some(Instant::now() + Duration::from_secs(3));
                ui.ctx().request_repaint_after(Duration::from_secs(3));
            }
        });
    }

    fn show(&mut self, ctx: &egui::Context) {
        ctx.observe_window("Four Words");
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(24))
            .show(ctx, |ui| {
                let _ = egui::ScrollArea::vertical()
                    .show(ui, |ui| {
                        ui.heading("Four Words");
                        ui.label("Passwords made from words you can type.");
                        ui.add_space(8.0);
                        if ui.available_width() >= 850.0 {
                            ui.columns(2, |columns| {
                                self.controls(&mut columns[0]);
                                self.refresh_settings();
                                self.accept_replies();
                                self.entropy(&mut columns[1]);
                                columns[1].separator();
                                self.password(&mut columns[1]);
                            });
                        } else {
                            self.controls(ui);
                            self.refresh_settings();
                            // Invalidate obsolete results before they can appear beside new controls.
                            self.accept_replies();
                            ui.separator();
                            self.entropy(ui);
                            ui.separator();
                            self.password(ui);
                        }
                    })
                    .observe_region(
                        RegionSpec::new("layout.scroll", Role::ScrollView)
                            .expect("static observation key"),
                    );
            });
        if let Some(changed) = self.scheduled {
            let elapsed = changed.elapsed();
            if elapsed >= DEBOUNCE {
                self.send_request();
            } else {
                ctx.request_repaint_after(DEBOUNCE - elapsed);
            }
        }
    }
}

impl eframe::App for PasswordApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.show(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Drain and release the writer while normal threads are still alive,
        // before Windows begins process-wide thread-local destruction.
        self.context.finish_observation();
        self.context.remove_observation_runtime();
    }
}

fn color(index: usize) -> Color32 {
    let [r, g, b] = BANDS[index].rgb;
    Color32::from_rgb(r, g, b)
}

fn readable_text(rgb: [u8; 3]) -> Color32 {
    let linear = rgb.map(|channel| {
        let channel = f64::from(channel) / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    });
    let brightness = linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722;
    if brightness > 0.179 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

fn public(text: &str) -> Content {
    Content::public(text).expect("bounded nonempty public UI label")
}

fn spec(key: &str, role: Role, name: &str) -> ElementSpec {
    let mut spec = ElementSpec::new(
        ElementKey::parse(key).expect("static observation key"),
        role,
        public(name),
    );
    if role.is_actionable() {
        spec = spec.with_control(ControlMetadata {
            interactions: if role == Role::TextInput {
                vec![InteractionKind::ReplaceText]
            } else {
                vec![InteractionKind::Activate]
            },
            scope: Some(
                Content::stable_identifier("four-words.main").expect("static observation scope"),
            ),
            reveals: vec![],
        });
    }
    spec
}

fn number_control(ui: &mut egui::Ui, id: &str, input: &mut NumberInput) {
    ui.push_id(id, |ui| {
        // Support the public Windows accessibility editor action. This is
        // independent of observation; screen readers use the same input route.
        let editor_id = ui.make_persistent_id("value");
        if ui.is_enabled() {
            let replacement = ui.input(|state| {
                state
                    .accesskit_action_requests(editor_id, egui::accesskit::Action::SetValue)
                    .filter_map(|request| match &request.data {
                        Some(egui::accesskit::ActionData::Value(value)) => Some(value.to_string()),
                        _ => None,
                    })
                    .last()
            });
            if let Some(value) = replacement {
                input.text = value;
                if let Some(value) = input.parsed() {
                    input.value = value;
                }
            }
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = (ui.available_width() - 100.0).max(100.0);
            let slider = ui
                .add(egui::Slider::new(&mut input.value, input.min..=input.max).show_value(false));
            if slider
                .observe(
                    spec(
                        &format!("settings.{id}.slider"),
                        Role::Button,
                        &id.replace('_', " "),
                    )
                    .with_content(public(&input.value.to_string())),
                )
                .changed()
            {
                input.text = input.value.to_string();
            }
            let response = ui
                .add_sized(
                    [80.0, 38.0],
                    egui::TextEdit::singleline(&mut input.text)
                        .id(editor_id)
                        .horizontal_align(egui::Align::Center),
                )
                .observe_text_digest(
                    spec(
                        &format!("settings.{id}.value"),
                        Role::TextInput,
                        &id.replace('_', " "),
                    ),
                    &input.text,
                );
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::TextEdit,
                    ui.is_enabled(),
                    id.replace('_', " "),
                )
            });
            ui.ctx().accesskit_node_builder(response.id, |node| {
                node.add_action(egui::accesskit::Action::SetValue);
                node.set_value(input.text.clone());
            });
            if response.changed()
                && let Some(value) = input.parsed()
            {
                input.value = value;
            }
        });
        if input.parsed().is_none() && ui.is_enabled() {
            ui.colored_label(
                color(0),
                format!("Enter a whole number from {} to {}.", input.min, input.max),
            );
        }
    });
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 740.0])
            .with_min_inner_size([620.0, 600.0]),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "Four Words",
        options,
        Box::new(|cc| Ok(Box::new(PasswordApp::new(&cc.egui_ctx)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use ui_observation_protocol::{Observation, ObservationBody};
    use ui_observation_runtime::{
        MemoryLimits, MemoryObserver, MemorySnapshot, ObservationSink, RecordStatus,
    };

    #[derive(Default)]
    struct Capture(Mutex<Vec<Observation>>);

    impl ObservationSink for Capture {
        fn try_record(&self, observation: Observation) -> RecordStatus {
            self.0.lock().unwrap().push(observation);
            RecordStatus::Recorded
        }
    }

    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    fn observed_frame(
        app: &mut PasswordApp,
        ctx: &egui::Context,
        capture: &Capture,
        size: Vec2,
        events: Vec<egui::Event>,
    ) -> (MemorySnapshot, egui::FullOutput) {
        let output = ui_observation_egui_031::run_observed(
            ctx,
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                events,
                ..Default::default()
            },
            |ctx| app.show(ctx),
        );
        let mut observer = MemoryObserver::new(NONCE, MemoryLimits::default()).unwrap();
        for record in capture.0.lock().unwrap().iter() {
            observer.push(record.clone()).unwrap();
        }
        (observer.snapshot().unwrap(), output)
    }

    fn element<'a>(
        snapshot: &'a MemorySnapshot,
        key: &str,
    ) -> &'a ui_observation_protocol::ElementObservation {
        match &snapshot.latest.get(key).unwrap().record {
            ObservationBody::Element(element) => element,
            _ => panic!("expected an element"),
        }
    }

    #[test]
    fn measured_default_layout_has_visible_controls_and_preserves_private_password() {
        let ctx = egui::Context::default();
        let mut app = PasswordApp::new(&ctx);
        app.refresh_settings();
        app.scheduled = None;
        app.suggestion = Some(app.settings.generate().unwrap());
        let capture = Arc::new(Capture::default());
        ctx.install_observation_runtime(ObservationRuntime::with_sink(NONCE, capture.clone()));
        // A second layout pass settles wrapped rows before measuring.
        let _ = observed_frame(&mut app, &ctx, &capture, Vec2::new(1000.0, 740.0), vec![]);
        let (snapshot, output) =
            observed_frame(&mut app, &ctx, &capture, Vec2::new(1000.0, 740.0), vec![]);
        assert!(snapshot.completed_frame.is_some());
        assert!(
            snapshot
                .latest
                .contains_key("revision.ui.render.text.scan-complete")
        );
        for key in [
            "settings.character_limit.slider",
            "settings.character_limit.value",
            "settings.words.enabled",
            "settings.word_limit.slider",
            "settings.word_limit.value",
            "settings.language.english",
            "settings.language.french",
            "settings.language.german",
            "settings.separator.hyphen",
            "settings.separator.space",
            "result.entropy",
            "result.classification",
            "result.password",
            "result.length",
            "action.generate",
            "action.copy",
        ] {
            let control = element(&snapshot, key);
            assert_eq!(
                control.visible_fraction.fixed().millionths(),
                1_000_000,
                "clipped: {key}: {:?}",
                control.layout_rect
            );
            control.validate().unwrap();
        }
        for key in ["action.generate", "action.copy"] {
            let rect = element(&snapshot, key).layout_rect;
            assert!(rect.max_y.millionths() - rect.min_y.millionths() >= 48_000_000);
        }
        let left = element(&snapshot, "settings.character_limit.slider").layout_rect;
        let right = element(&snapshot, "result.password").layout_rect;
        assert!(left.max_x <= right.min_x, "columns overlap");
        let password = app.suggestion.as_ref().unwrap();
        assert_eq!(
            element(&snapshot, "result.password").content,
            Some(Content::private_omitted(password.password.len() as u64))
        );
        assert!(!element(&snapshot, "result.password").states.editable);
        assert_eq!(
            element(&snapshot, "result.entropy")
                .content
                .as_ref()
                .unwrap()
                .public_value(),
            Some(format!("{:.2} bits", password.bits).as_str())
        );
        let digest = Content::digest_only(password.password.as_bytes());
        assert!(snapshot.latest.values().any(|record| matches!(&record.record,
            ObservationBody::Element(element) if element.key.as_str().starts_with("ui.render.text.")
                && element.content.as_ref() == Some(&digest) && element.visible_fraction == ui_observation_protocol::Fraction::ONE)),
            "The complete password text was not rendered within its visible bounds");
        let [r, g, b] = model::entropy_rgb(password.bits);
        let achieved_color = Color32::from_rgb(r, g, b);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Rect(rect) if rect.fill == achieved_color)));
        assert_eq!(
            element(&snapshot, "result.entropy.color")
                .content
                .as_ref()
                .unwrap()
                .public_value(),
            Some(format!("#{r:02x}{g:02x}{b:02x}").as_str())
        );
        println!(
            "default_layout: controls=16 fully_visible=16 minimum_button_height=54 columns_overlap=false password_plaintext_omitted=true achieved_entropy_color_verified=true"
        );
    }

    #[test]
    fn measured_checkbox_and_slider_interactions_update_actual_values() {
        let ctx = egui::Context::default();
        let mut app = PasswordApp::new(&ctx);
        let capture = Arc::new(Capture::default());
        ctx.install_observation_runtime(ObservationRuntime::with_sink(NONCE, capture.clone()));
        let size = Vec2::new(1000.0, 740.0);
        let (snapshot, _) = observed_frame(&mut app, &ctx, &capture, size, vec![]);
        let rect = element(&snapshot, "settings.words.enabled").layout_rect;
        let point = egui::pos2(
            (rect.min_x.millionths() + 12_000_000) as f32 / 1e6,
            ((rect.min_y.millionths() + rect.max_y.millionths()) / 2) as f32 / 1e6,
        );
        let click = |point, pressed| egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = observed_frame(
            &mut app,
            &ctx,
            &capture,
            size,
            vec![egui::Event::PointerMoved(point), click(point, true)],
        );
        let (snapshot, _) =
            observed_frame(&mut app, &ctx, &capture, size, vec![click(point, false)]);
        assert!(element(&snapshot, "settings.words.enabled").states.selected);
        assert!(
            element(&snapshot, "settings.word_limit.value")
                .states
                .enabled
        );
        let rect = element(&snapshot, "settings.character_limit.slider").layout_rect;
        let point = egui::pos2(
            ((rect.min_x.millionths() + rect.max_x.millionths()) / 2) as f32 / 1e6,
            ((rect.min_y.millionths() + rect.max_y.millionths()) / 2) as f32 / 1e6,
        );
        let _ = observed_frame(
            &mut app,
            &ctx,
            &capture,
            size,
            vec![egui::Event::PointerMoved(point), click(point, true)],
        );
        let (snapshot, _) =
            observed_frame(&mut app, &ctx, &capture, size, vec![click(point, false)]);
        assert!(app.length.value < 128);
        assert_eq!(app.length.parsed(), Some(app.length.value));
        assert_eq!(
            element(&snapshot, "settings.character_limit.value").text_value,
            Some(Content::digest_only(app.length.text.as_bytes()))
        );
        println!(
            "input_metrics: word_cap_selected=true word_editor_enabled=true slider_and_editor_synchronized=true"
        );
    }

    #[test]
    fn exit_handler_finishes_the_observation_stream_before_context_destruction() {
        let ctx = egui::Context::default();
        let mut app = PasswordApp::new(&ctx);
        let capture = Arc::new(Capture::default());
        ctx.install_observation_runtime(ObservationRuntime::with_sink(NONCE, capture.clone()));
        let _ = observed_frame(&mut app, &ctx, &capture, Vec2::new(1000.0, 740.0), vec![]);
        eframe::App::on_exit(&mut app, None);
        let mut observer = MemoryObserver::new(NONCE, MemoryLimits::default()).unwrap();
        for record in capture.0.lock().unwrap().iter() {
            observer.push(record.clone()).unwrap();
        }
        assert!(observer.finish().unwrap().stream_complete);
        let records = capture.0.lock().unwrap().len();
        drop(app);
        drop(ctx);
        assert_eq!(capture.0.lock().unwrap().len(), records);
    }

    #[test]
    fn follows_system_theme_changes_and_keeps_large_text_in_both_modes() {
        let ctx = egui::Context::default();
        let mut app = PasswordApp::new(&ctx);
        for theme in [egui::Theme::Light, egui::Theme::Dark, egui::Theme::Light] {
            let output = ui_observation_egui_031::run_observed(
                &ctx,
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1000.0, 740.0),
                    )),
                    system_theme: Some(theme),
                    ..Default::default()
                },
                |ctx| app.show(ctx),
            );
            assert_eq!(ctx.theme(), theme);
            assert_eq!(
                ctx.options(|options| options.theme_preference),
                egui::ThemePreference::System
            );
            let style = ctx.style();
            assert_eq!(style.text_styles[&egui::TextStyle::Body].size, 21.0);
            assert_eq!(style.text_styles[&egui::TextStyle::Monospace].size, 23.0);
            assert!(style.visuals.override_text_color.is_none());
            let panel = theme.default_visuals().panel_fill;
            assert_eq!(style.visuals.panel_fill, panel);
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Rect(rect) if rect.fill == panel && rect.rect.width() >= 990.0)));
        }
        eframe::App::on_exit(&mut app, None);
    }

    #[test]
    fn changed_or_invalid_values_clear_password_and_ignore_obsolete_results() {
        let ctx = egui::Context::default();
        let mut app = PasswordApp::new(&ctx);
        app.refresh_settings();
        app.suggestion = Some(app.settings.generate().unwrap());
        let old_revision = app.revision;
        app.length.text = "63".into();
        app.refresh_settings();
        assert!(app.suggestion.is_none());
        assert!(app.revision > old_revision);
        assert_eq!(app.active.as_ref().unwrap().max_length, 63);
        app.length.text.clear();
        app.refresh_settings();
        assert!(app.active.is_none());
        assert!(app.scheduled.is_none());
        assert!(app.suggestion.is_none());
    }

    #[test]
    fn window_renders_and_copy_exports_only_the_password() {
        let ctx = egui::Context::default();
        let mut app = PasswordApp::new(&ctx);
        app.refresh_settings();
        app.scheduled = None;
        app.suggestion = Some(app.settings.generate().unwrap());
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(900.0, 880.0),
                )),
                ..Default::default()
            },
            |ctx| app.show(ctx),
        );
        assert!(!output.shapes.is_empty());
        let expected = app.suggestion.as_ref().unwrap().password.clone();
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            ctx.copy_text(expected.clone())
        });
        assert!(output.platform_output.commands.iter().any(
            |command| matches!(command, egui::OutputCommand::CopyText(text) if text == &expected)
        ));
    }
}
