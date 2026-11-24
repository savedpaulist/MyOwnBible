//! Полноценная панель настроек приложения.
//!
//! Настройки сгруппированы по задачам: каталоги и базовые параметры,
//! оформление, отдельная страница типографики, чтение и переходы.
//! Изменения интерфейса применяются сразу; каталоги — по явной кнопке.

use std::path::PathBuf;
use std::sync::Mutex as StdMutex;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::setting::{
    SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, TitleBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use bible_domain::ports::{ModuleCatalog as _, ModuleKind};

use crate::navigation;
use crate::services::app_services;

#[derive(Clone, Copy)]
enum FontPreviewTarget {
    Interface,
    Bible,
    SidePanels,
}

static LAST_SETTINGS_SAVE_STATUS: StdMutex<Option<String>> = StdMutex::new(None);

pub(crate) fn set_save_status(message: String) {
    if let Ok(mut status) = LAST_SETTINGS_SAVE_STATUS.lock() {
        *status = Some(message);
    }
}

fn last_save_status() -> Option<String> {
    LAST_SETTINGS_SAVE_STATUS
        .lock()
        .ok()
        .and_then(|status| status.clone())
}

pub struct SettingsPanel {
    focus_handle: FocusHandle,
    modules_dir_input: Entity<InputState>,
    notes_dir_input: Entity<InputState>,
    status: Option<String>,
    pages: Option<Vec<SettingPage>>,
    system_fonts: Option<Vec<String>>,
    _subscriptions: Vec<Subscription>,
}

/// Root of the dedicated settings window. Keeping this separate from
/// `SettingsPanel` means the same settings UI remains testable as a normal
/// entity while it is no longer part of the workspace dock/layout.
pub struct SettingsWindow {
    panel: Entity<SettingsPanel>,
}

impl SettingsWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            panel: cx.new(|cx| SettingsPanel::new(window, cx)),
        }
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scale = app_services(cx)
            .settings
            .read()
            .interface_scale
            .clamp(0.8, 1.6);
        window.set_rem_size(px(16.0 * scale));
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                TitleBar::new().child(
                    h_flex()
                        .h_full()
                        .px_3()
                        .items_center()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(crate::i18n::tr(cx, "Настройки")),
                ),
            )
            .child(div().flex_1().min_h_0().child(self.panel.clone()))
    }
}

/// Open or focus the single settings window, following Zed's additional
/// window pattern. A second click never creates a duplicate window.
pub fn open_settings_window(cx: &mut App) {
    if let Some(existing) = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<SettingsWindow>())
    {
        let _ = existing.update(cx, |_, window, _| window.activate_window());
        return;
    }

    cx.defer(|cx| {
        let mut window_size = size(px(980.0), px(720.0));
        if let Some(display) = cx.primary_display() {
            let available = display.bounds().size;
            window_size.width = window_size.width.min(available.width * 0.9);
            window_size.height = window_size.height.min(available.height * 0.9);
        }
        let bounds = Bounds::centered(None, window_size, cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(720.0), px(480.0))),
            ..TitleBar::window_options()
        };
        if let Err(error) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| SettingsWindow::new(window, cx))
        }) {
            log::error!("failed to open settings window: {error:#}");
        }
    });
}

impl SettingsPanel {
    pub const NAME: &'static str = "settings";

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = shell::settings::current(app_services(cx));
        let modules_dir_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.modules_dir.to_string_lossy().to_string())
        });
        let notes_dir_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::i18n::tr(cx, "Не выбрано — заметки отключены"))
                .default_value(
                    settings
                        .notes_dir
                        .as_ref()
                        .map(|dir| dir.to_string_lossy().to_string())
                        .unwrap_or_default(),
                )
        });
        let settings_bus = navigation::bus(cx);
        let settings_subscription = cx.subscribe(
            &settings_bus,
            |this, _, _: &navigation::SettingsChanged, cx| {
                this.pages = None;
                cx.notify();
            },
        );

        Self {
            focus_handle: cx.focus_handle(),
            modules_dir_input,
            notes_dir_input,
            status: None,
            pages: None,
            system_fonts: None,
            _subscriptions: vec![settings_subscription],
        }
    }

    /// Сохранить изменение до публикации события. Ошибки не проглатываются:
    /// если settings.json нельзя записать, новое значение не считается применённым.
    fn persist_settings(cx: &mut App, patch: impl FnOnce(&mut shell::settings::Settings)) -> bool {
        match shell::settings::update(app_services(cx), patch) {
            Ok(()) => {
                set_save_status(
                    "Последнее изменение успешно записано в settings.json.".to_string(),
                );
                true
            }
            Err(error) => {
                let message = format!("Не удалось сохранить settings.json: {error:#}");
                log::error!("{message}");
                set_save_status(message);
                // Notify even on failure so the settings page can display the
                // error and controls can revert to the last persisted value.
                navigation::notify_settings_changed(cx);
                false
            }
        }
    }

    fn apply_modules_dir(&mut self, cx: &mut Context<Self>) {
        let path = self.modules_dir_input.read(cx).value().trim().to_string();
        if path.is_empty() {
            self.status = Some("Укажите папку с модулями MyBible.".to_string());
            cx.notify();
            return;
        }
        let dir = PathBuf::from(&path);
        if !dir.is_dir() {
            self.status = Some(format!("Папка не найдена: {}", dir.display()));
            cx.notify();
            return;
        }
        if let Err(error) = shell::settings::update(app_services(cx), move |s| {
            s.modules_dir = dir;
        }) {
            self.status = Some(format!("Не удалось сохранить настройки: {error:#}"));
            cx.notify();
            return;
        }
        let count = shell::settings::rescan_modules(app_services(cx));
        let services = crate::services::app_services_arc(cx);
        cx.background_spawn(async move { shell::search::warm_indexes(&services) })
            .detach();
        self.status = Some(format!(
            "Каталог модулей применён. Найдено модулей: {count}."
        ));
        navigation::notify_settings_changed(cx);
        cx.notify();
    }

    fn apply_notes_dir(&mut self, cx: &mut Context<Self>) {
        let path = self.notes_dir_input.read(cx).value().trim().to_string();
        let dir = if path.is_empty() {
            None
        } else {
            let candidate = PathBuf::from(&path);
            if !candidate.is_dir() {
                self.status = Some(format!("Папка заметок не найдена: {}", candidate.display()));
                cx.notify();
                return;
            }
            Some(candidate)
        };
        if let Err(error) = shell::settings::update(app_services(cx), move |s| {
            s.notes_dir = dir;
        }) {
            self.status = Some(format!("Не удалось сохранить настройки: {error:#}"));
            cx.notify();
            return;
        }
        let enabled = shell::settings::reinit_notes(app_services(cx));
        self.status = Some(if enabled {
            "Папка заметок применена.".to_string()
        } else if path.is_empty() {
            "Хранилище заметок отключено.".to_string()
        } else {
            "Не удалось открыть хранилище заметок. Подробности — в журнале.".to_string()
        });
        navigation::notify_settings_changed(cx);
        cx.notify();
    }

    /// Поле пути: текстовый путь остаётся доступен для ручного ввода,
    /// кнопка «Обзор» открывает системный выбор папки.
    fn path_item(
        panel: &WeakEntity<Self>,
        input: &Entity<InputState>,
        is_notes: bool,
        apply_id: &'static str,
        apply: fn(&mut Self, &mut Context<Self>),
    ) -> SettingItem {
        let input = input.clone();
        let panel_for_apply = panel.clone();
        let panel_for_browse = panel.clone();
        SettingItem::render(move |_options, _window, _cx| {
            let panel = panel_for_apply.clone();
            let picker_panel = panel_for_browse.clone();
            let input = input.clone();
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(div().flex_1().min_w_0().child(Input::new(&input)))
                .child(
                    Button::new(if is_notes { "browse-notes-dir" } else { "browse-modules-dir" })
                        .xsmall()
                        .ghost()
                        .label(crate::i18n::tr(_cx, "Обзор…"))
                        .tooltip(crate::i18n::tr(_cx, "Выбрать папку в проводнике"))
                        .on_click(move |_, window, cx| {
                            let _ = picker_panel.update(cx, |this, cx| {
                                let prompt = if is_notes {
                                    "Выберите папку для хранилища заметок"
                                } else {
                                    "Выберите папку с модулями MyBible"
                                };
                                let receiver = cx.prompt_for_paths(PathPromptOptions {
                                    files: false,
                                    directories: true,
                                    multiple: false,
                                    prompt: Some(prompt.into()),
                                });
                                this.status = Some("Ожидаю выбор папки…".to_string());
                                cx.notify();
                                cx.spawn_in(window, async move |this, window| {
                                    let result = receiver.await;
                                    let _ = this.update_in(window, |this, window, cx| {
                                        match result {
                                            Ok(Ok(Some(paths))) => {
                                                if let Some(path) = paths.into_iter().next() {
                                                    let value = path.to_string_lossy().to_string();
                                                    let target = if is_notes {
                                                        &this.notes_dir_input
                                                    } else {
                                                        &this.modules_dir_input
                                                    };
                                                    target.update(cx, |state, cx| {
                                                        state.set_value(value, window, cx);
                                                    });
                                                    this.status = Some(
                                                        "Папка выбрана. Нажмите «Применить», чтобы сохранить её.".to_string(),
                                                    );
                                                } else {
                                                    this.status = Some("Папка не выбрана.".to_string());
                                                }
                                            }
                                            Ok(Ok(None)) => {
                                                this.status = Some("Выбор папки отменён.".to_string());
                                            }
                                            Ok(Err(error)) => {
                                                this.status = Some(format!(
                                                    "Не удалось открыть проводник: {error:#}"
                                                ));
                                            }
                                            Err(_) => {
                                                this.status = Some(
                                                    "Диалог выбора папки закрылся без результата.".to_string(),
                                                );
                                            }
                                        }
                                        cx.notify();
                                    });
                                }).detach();
                            });
                        }),
                )
                .child(
                    Button::new(apply_id)
                        .xsmall()
                        .label(crate::i18n::tr(_cx, "Применить"))
                        .on_click(move |_, _window, cx| {
                            let _ = panel.update(cx, |this, cx| apply(this, cx));
                        }),
                )
        })
    }

    fn font_options(fonts: &[String], selected: &str) -> Vec<(SharedString, SharedString)> {
        let mut options = vec![("".into(), crate::i18n::tr_static("По умолчанию").into())];
        if !selected.is_empty() && !fonts.iter().any(|font| font.eq_ignore_ascii_case(selected)) {
            options.push((
                selected.into(),
                format!("{selected} (не найден сейчас)").into(),
            ));
        }
        options.extend(fonts.iter().map(|font| {
            let font = SharedString::from(font.clone());
            (font.clone(), font)
        }));
        options
    }

    fn font_preview_item(target: FontPreviewTarget) -> SettingItem {
        SettingItem::render(move |_options, _window, cx| {
            let (family, font_size, profile_title) = {
                let settings = app_services(cx).settings.read();
                match target {
                    FontPreviewTarget::Interface => (
                        settings.ui_font_family.clone(),
                        settings.ui_font_size,
                        "Предпросмотр интерфейса",
                    ),
                    FontPreviewTarget::Bible => (
                        settings.reading_font_family.clone(),
                        if settings.reading_font_size > 0.0 {
                            settings.reading_font_size
                        } else {
                            settings.ui_font_size
                        },
                        "Предпросмотр текста Библии",
                    ),
                    FontPreviewTarget::SidePanels => (
                        settings.side_panel_font_family.clone(),
                        settings.side_panel_font_size,
                        "Предпросмотр боковых панелей",
                    ),
                }
            };
            let preview_size = if font_size.is_finite() {
                font_size.clamp(10.0, 32.0)
            } else {
                16.0
            };
            let muted = cx.theme().muted_foreground;
            let font_label = if family.is_empty() {
                "Системный шрифт (по умолчанию)".to_string()
            } else {
                family.clone()
            };
            let mut preview = v_flex()
                .w_full()
                .gap_2()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(muted)
                                .child(profile_title),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("{font_label} · {:.0} px", preview_size)),
                        ),
                );

            // Preview layouts deliberately mirror the real destination of
            // each typography profile instead of showing three copies of the
            // same generic sample card.
            let samples: [(&str, &str, bool); 4] = match target {
                FontPreviewTarget::Interface => {
                    preview = preview.child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(div().px_2().py_1().rounded_sm().bg(cx.theme().accent.opacity(0.14)).child(crate::i18n::tr_static("Библия")))
                            .child(div().px_2().py_1().child(crate::i18n::tr_static("Поиск")))
                            .child(div().px_2().py_1().child(crate::i18n::tr_static("Библиотека")))
                            .child(div().px_2().py_1().child(crate::i18n::tr_static("Настройки"))),
                    );
                    [
                        ("Ελληνικά", "Ἐν ἀρχῇ ἦν ὁ λόγος.", false),
                        ("English", "Search · Library · Settings", false),
                        ("Русский", "Поиск · Библиотека · Настройки", false),
                        ("עברית", "בְּרֵאשִׁית בָּרָא אֱלֹהִים", true),
                    ]
                }
                FontPreviewTarget::Bible => {
                    preview = preview.child(
                        v_flex()
                            .w_full()
                            .gap_1()
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(crate::i18n::tr_static("Бытие · Глава 1")))
                            .child(div().text_xs().text_color(muted).child(crate::i18n::tr_static("1:1 · Образец текста Писания"))),
                    );
                    [
                        ("Греческий", "Ἐν ἀρχῇ ἦν ὁ λόγος, καὶ ὁ λόγος ἦν πρὸς τὸν θεόν.", false),
                        ("English", "In the beginning was the Word, and the Word was with God.", false),
                        ("Русский", "В начале было Слово, и Слово было у Бога.", false),
                        ("עברית", "בְּרֵאשִׁית בָּרָא אֱלֹהִים אֵת הַשָּׁמַיִם וְאֵת הָאָרֶץ", true),
                    ]
                }
                FontPreviewTarget::SidePanels => {
                    preview = preview.child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(crate::i18n::tr_static("Словарь · Статья «Логос»")))
                            .child(div().text_xs().text_color(muted).child(crate::i18n::tr_static("Панель справа"))),
                    );
                    [
                        ("Ελληνικά", "Λόγος — значение и употребление", false),
                        ("English", "Dictionary article and commentary", false),
                        ("Русский", "Словарная статья и комментарий", false),
                        ("עברית", "בְּרֵאשִׁית בָּרָא אֱלֹהִים", true),
                    ]
                }
            };

            for (language, sample, rtl) in samples {
                let mut sample_text = div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(preview_size))
                    .child(sample.to_string());
                if !family.is_empty() {
                    sample_text = sample_text.font_family(family.clone());
                }
                if rtl {
                    sample_text = sample_text.text_right();
                }
                preview = preview.child(
                    h_flex()
                        .w_full()
                        .gap_3()
                        .items_center()
                        .child(
                            div()
                                .w(px(84.))
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(muted)
                                .child(language.to_string()),
                        )
                        .child(sample_text),
                );
            }
            preview
        })
        .description(crate::i18n::tr_static("Проверка шрифта на греческом, английском, русском и иврите в контексте выбранного профиля. Недостающие глифы могут отображаться системным fallback-шрифтом."))
    }

    fn font_size_options(include_auto: bool) -> Vec<(SharedString, SharedString)> {
        let mut values: Vec<(SharedString, SharedString)> = [
            ("10", "10"),
            ("11", "11"),
            ("12", "12"),
            ("13", "13"),
            ("14", "14"),
            ("15", "15"),
            ("16", "16"),
            ("17", "17"),
            ("18", "18"),
            ("20", "20"),
            ("22", "22"),
            ("24", "24"),
            ("26", "26"),
            ("28", "28"),
            ("30", "30"),
            ("32", "32"),
        ]
        .into_iter()
        // SettingField::dropdown принимает (сохраняемое значение, подпись).
        .map(|(label, value)| (value.into(), label.into()))
        .collect();
        if include_auto {
            values.insert(
                0,
                ("".into(), crate::i18n::tr_static("По умолчанию").into()),
            );
        }
        values
    }

    fn font_profile(
        title: SharedString,
        cx: &App,
        fonts: Vec<(SharedString, SharedString)>,
        family: fn(&shell::settings::Settings) -> &String,
        set_family: fn(&mut shell::settings::Settings, String),
        size: fn(&shell::settings::Settings) -> f32,
        set_size: fn(&mut shell::settings::Settings, f32),
    ) -> SettingGroup {
        SettingGroup::new().title(title).items([
            SettingItem::new(
                crate::i18n::tr(cx, "Семейство шрифта"),
                SettingField::scrollable_dropdown(
                    fonts,
                    move |cx| family(&app_services(cx).settings.read()).clone().into(),
                    move |value, cx| {
                        let value = value.to_string();
                        if Self::persist_settings(cx, move |settings| set_family(settings, value)) {
                            navigation::notify_settings_changed(cx);
                        }
                    },
                ),
            ),
            SettingItem::new(
                crate::i18n::tr(cx, "Размер шрифта"),
                SettingField::dropdown(
                    Self::font_size_options(true),
                    move |cx| {
                        let size = size(&app_services(cx).settings.read());
                        if size > 0. {
                            (size.round() as u32).to_string().into()
                        } else {
                            "".into()
                        }
                    },
                    move |value, cx| {
                        let value = value.parse::<f32>().unwrap_or(0.);
                        if Self::persist_settings(cx, move |settings| set_size(settings, value)) {
                            navigation::notify_settings_changed(cx);
                        }
                    },
                ),
            ),
        ])
    }

    fn build_pages(&mut self, cx: &mut Context<Self>) -> Vec<SettingPage> {
        let translations: Vec<(SharedString, SharedString)> = std::iter::once((
            SharedString::from(""),
            SharedString::from(crate::i18n::tr_static("Авто — первый доступный перевод")),
        ))
        .chain(std::iter::once((
            "current_module".into(),
            crate::i18n::tr_static("Текущий модуль").into(),
        )))
        .chain(
            app_services(cx)
                .registry
                .modules()
                .into_iter()
                .filter(|module| module.kind == ModuleKind::BibleTranslation)
                .map(|module| {
                    let label = module
                        .description
                        .clone()
                        .unwrap_or_else(|| module.id.to_string());
                    (
                        SharedString::from(module.id.to_string()),
                        SharedString::from(label),
                    )
                }),
        )
        .collect();

        let panel = cx.entity().downgrade();
        let current = shell::settings::current(app_services(cx));
        let fonts = self.system_fonts.get_or_insert_with(|| {
            let mut fonts = cx.text_system().all_font_names();
            fonts.sort_by_cached_key(|font| font.to_lowercase());
            fonts.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
            fonts
        });
        let count = fonts.len();
        let ui_fonts = Self::font_options(fonts, &current.ui_font_family);
        let bible_fonts = Self::font_options(fonts, &current.reading_font_family);
        let panel_fonts = Self::font_options(fonts, &current.side_panel_font_family);

        let left_fonts = Self::font_options(fonts, &current.left_panel_font_family);
        let dictionary_fonts = Self::font_options(fonts, &current.dictionary_font_family);
        vec![
                SettingPage::new(crate::i18n::tr(cx, "Общие")).groups([
                    SettingGroup::new().title(crate::i18n::tr(cx, "Язык")).items([
                        SettingItem::new(
                            crate::i18n::tr(cx, "Язык интерфейса"),
                            SettingField::dropdown(
                                vec![
                                    ("russian".into(), "Русский".into()),
                                    ("english".into(), "English".into()),
                                ],
                                |cx: &App| match app_services(cx)
                                    .settings
                                    .read()
                                    .interface_language
                                {
                                    shell::settings::InterfaceLanguage::Russian => "russian".into(),
                                    shell::settings::InterfaceLanguage::English => "english".into(),
                                },
                                |value, cx| {
                                    let language = if value.as_ref() == "english" {
                                        shell::settings::InterfaceLanguage::English
                                    } else {
                                        shell::settings::InterfaceLanguage::Russian
                                    };
                                    if Self::persist_settings(cx, move |settings| {
                                        settings.interface_language = language;
                                    }) {
                                        crate::i18n::sync(cx);
                                        navigation::notify_settings_changed(cx);
                                        cx.refresh_windows();
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Изменение языка применяется сразу во всех окнах приложения.")),
                    ]),
                    SettingGroup::new()
                        .title(crate::i18n::tr(cx, "Папки приложения"))
                        .items([
                            Self::path_item(
                                &panel,
                                &self.modules_dir_input,
                                false,
                                "apply-modules-dir",
                                Self::apply_modules_dir,
                            )
                            .description(
                                "Папка с базами модулей MyBible (*.sqlite3). Выберите её через проводник или вставьте путь вручную.",
                            ),
                            Self::path_item(
                                &panel,
                                &self.notes_dir_input,
                                true,
                                "apply-notes-dir",
                                Self::apply_notes_dir,
                            )
                            .description(
                                "Папка Markdown-хранилища (например, Obsidian). Пустое значение отключает заметки.",
                            ),
                        ]),
                    SettingGroup::new().title(crate::i18n::tr(cx, "Файл настроек")).items([
                        SettingItem::render(|_options, _window, _cx| {
                            let status = last_save_status()
                                .unwrap_or_else(|| "Ожидает первого изменения настроек.".to_string());
                            v_flex()
                                .w_full()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .child(status),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(_cx.theme().muted_foreground)
                                        .child(shell::settings::file_path().display().to_string()),
                                )
                        }),
                    ]),
                    SettingGroup::new().title(crate::i18n::tr(cx, "По умолчанию")).items([
                        SettingItem::new(
                            crate::i18n::tr(cx, "Перевод для новых вкладок"),
                            SettingField::dropdown(
                                translations,
                                |cx: &App| {
                                    let services = app_services(cx);
                                    let settings = services.settings.read();
                                    if settings.new_tabs_use_current_module {
                                        return "current_module".into();
                                    }
                                    let configured = settings.default_translation.clone();
                                    drop(settings);
                                    configured
                                        .filter(|value| services.registry
                                            .module(&bible_domain::ids::ModuleId(value.clone()))
                                            .is_some_and(|meta| meta.kind == ModuleKind::BibleTranslation))
                                        .unwrap_or_default()
                                        .into()
                                },
                                |value, cx| {
                                    let value = value.to_string();
                                    if Self::persist_settings(cx, move |s| {
                                        s.new_tabs_use_current_module = value == "current_module";
                                        if !s.new_tabs_use_current_module {
                                            s.default_translation = (!value.is_empty()).then_some(value);
                                        }
                                    }) {
                                        navigation::notify_settings_changed(cx);
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Перевод, который будет выбран при открытии нового окна чтения.")),
                    ]),
                ]),
                SettingPage::new(crate::i18n::tr(cx, "Внешний вид")).groups([
                    SettingGroup::new().title(crate::i18n::tr(cx, "Оформление окна")).items([
                        SettingItem::new(
                            crate::i18n::tr(cx, "Цветовая тема"),
                            SettingField::dropdown(
                                crate::theme::available(cx)
                                    .into_iter()
                                    .map(|name| {
                                        (SharedString::from(name.clone()), SharedString::from(name))
                                    })
                                    .collect(),
                                |cx: &App| app_services(cx).settings.read().theme.clone().into(),
                                |value, cx| {
                                    let theme = value.to_string();
                                    crate::theme::select(&theme, cx);
                                },
                            ),
                        )
                        .description(format!(
                            "Меняет фон, поверхности, контраст и акцентные цвета. Zed JSON темы автоматически подхватываются из папки {} и из пользовательской папки Zed; для новых файлов перезапустите приложение.",
                                    shell::themes::storage_dir().display()
                        )),
                        SettingItem::new(
                            crate::i18n::tr(cx, "Масштаб интерфейса"),
                            SettingField::dropdown(
                                vec![
                                    ("0.8".into(), "80 %".into()),
                                    ("0.9".into(), "90 %".into()),
                                    ("1.0".into(), "100 %".into()),
                                    ("1.1".into(), "110 %".into()),
                                    ("1.2".into(), "120 %".into()),
                                    ("1.3".into(), "130 %".into()),
                                    ("1.4".into(), "140 %".into()),
                                    ("1.5".into(), "150 %".into()),
                                    ("1.6".into(), "160 %".into()),
                                ],
                                |cx: &App| format!("{:.1}", app_services(cx).settings.read().interface_scale).into(),
                                |value, cx| {
                                    let scale = value.parse::<f32>().unwrap_or(1.0).clamp(0.8, 1.6);
                                    if Self::persist_settings(cx, move |s| {
                                        s.interface_scale = scale;
                                    }) {
                                        navigation::notify_settings_changed(cx);
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Увеличивает или уменьшает элементы и отступы во всех окнах.")),
                        SettingItem::new(
                            crate::i18n::tr(cx, "Размер вкладок"),
                            SettingField::dropdown(
                                vec![
                                    ("compact".into(), crate::i18n::tr_static("Компактный").into()),
                                    ("normal".into(), crate::i18n::tr_static("Обычный").into()),
                                    ("large".into(), crate::i18n::tr_static("Крупный").into()),
                                ],
                                |cx: &App| {
                                    use shell::settings::TabSize;
                                    match app_services(cx).settings.read().tab_size {
                                        TabSize::Compact => "compact",
                                        TabSize::Normal => "normal",
                                        TabSize::Large => "large",
                                    }.into()
                                },
                                |value, cx| {
                                    use shell::settings::TabSize;
                                    let size = match value.as_ref() {
                                        "compact" => TabSize::Compact,
                                        "large" => TabSize::Large,
                                        _ => TabSize::Normal,
                                    };
                                    if Self::persist_settings(cx, move |settings| settings.tab_size = size) {
                                        cx.refresh_windows();
                                    }
                                },
                            ),
                        ),
                        SettingItem::new(crate::i18n::tr(cx, "Размер иконок"), SettingField::dropdown(
                            [12, 14, 16, 18, 20, 22, 24].into_iter()
                                .map(|size| (size.to_string().into(), format!("{size} px").into())).collect(),
                            |cx| (app_services(cx).settings.read().icon_size.round() as u32).to_string().into(),
                            |value, cx| {
                                let size = value.parse::<f32>().unwrap_or(18.);
                                if Self::persist_settings(cx, move |settings| settings.icon_size = size) {
                                    navigation::notify_settings_changed(cx);
                                }
                            },
                        )),
                        SettingItem::new(
                            crate::i18n::tr(cx, "Ширина текста Библии"),
                            SettingField::dropdown(
                                vec![
                                    ("0".into(), crate::i18n::tr_static("Автоматически").into()),
                                    ("720".into(), "720 px — компактно".into()),
                                    ("880".into(), "880 px — стандарт".into()),
                                    ("1040".into(), "1040 px — широко".into()),
                                    ("1200".into(), "1200 px — очень широко".into()),
                                ],
                                |cx: &App| format!("{}", app_services(cx).settings.read().reading_column_width as u32).into(),
                                |value, cx| {
                                    let width = value.parse::<f32>().unwrap_or(0.0).clamp(0.0, 1600.0);
                                    if Self::persist_settings(cx, move |s| {
                                        s.reading_column_width = width;
                                    }) {
                                        navigation::notify_settings_changed(cx);
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Ограничивает ширину текстовой колонки. Полезно для чтения на широких мониторах.")),
                        SettingItem::new(
                            crate::i18n::tr(cx, "Интервал между стихами"),
                            SettingField::dropdown(
                                vec![
                                    ("0.70".into(), crate::i18n::tr_static("Плотно").into()),
                                    ("0.85".into(), crate::i18n::tr_static("Компактно").into()),
                                    ("1.00".into(), crate::i18n::tr_static("Обычно").into()),
                                    ("1.25".into(), crate::i18n::tr_static("Свободно").into()),
                                    ("1.50".into(), crate::i18n::tr_static("Очень свободно").into()),
                                ],
                                |cx: &App| format!("{:.2}", app_services(cx).settings.read().reading_line_spacing).into(),
                                |value, cx| {
                                    let spacing = value.parse::<f32>().unwrap_or(1.0).clamp(0.6, 1.8);
                                    if Self::persist_settings(cx, move |s| {
                                        s.reading_line_spacing = spacing;
                                    }) {
                                        navigation::notify_settings_changed(cx);
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Регулирует вертикальное расстояние между строками стихов.")),
                    ]),
                ]),
                SettingPage::new(crate::i18n::tr(cx, "Шрифты")).groups([
                    SettingGroup::new().title(crate::i18n::tr(cx, "Системные шрифты")).items([
                        SettingItem::render({
                            let panel = panel.clone();
                            move |_options, _window, cx| {
                                let panel = panel.clone();
                                h_flex()
                                    .w_full()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("Доступно семейств шрифтов: {count}")),
                                    )
                                    .child(
                                        Button::new("refresh-system-fonts")
                                            .xsmall()
                                            .ghost()
                                            .label(crate::i18n::tr(cx, "Обновить список"))
                                            .on_click(move |_, _, cx| {
                                                let _ = panel.update(cx, |this, cx| {
                                                    this.system_fonts = None;
                                                    this.pages = None;
                                                    cx.notify();
                                                });
                                            }),
                                    )
                            }
                        }),
                    ]),
                    SettingGroup::new().title(crate::i18n::tr(cx, "Интерфейс приложения")).items([
                        SettingItem::new(
                            crate::i18n::tr(cx, "Семейство шрифта"),
                            SettingField::scrollable_dropdown(
                                ui_fonts,
                                |cx: &App| app_services(cx).settings.read().ui_font_family.clone().into(),
                                |value, cx| {
                                    let family = value.to_string();
                                    if Self::persist_settings(cx, move |s| {
                                        s.ui_font_family = family;
                                    }) {
                                        crate::theme::refresh_typography(cx);
                                        navigation::notify_settings_changed(cx);
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Основное семейство для меню, кнопок, заголовков и всех рабочих окон. Список берётся из шрифтов, доступных GPUI в системе.")),
                        Self::font_preview_item(FontPreviewTarget::Interface),
                        SettingItem::new(
                            crate::i18n::tr(cx, "Размер шрифта интерфейса"),
                            SettingField::dropdown(
                                Self::font_size_options(false),
                                |cx: &App| format!("{}", app_services(cx).settings.read().ui_font_size.round() as u32).into(),
                                |value, cx| {
                                    let size = value.parse::<f32>().unwrap_or(16.0).clamp(10.0, 32.0);
                                    if Self::persist_settings(cx, move |s| s.ui_font_size = size) {
                                        crate::theme::refresh_typography(cx);
                                        navigation::notify_settings_changed(cx);
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Базовый кегль интерфейса. Масштаб окна применяется дополнительно.")),
                    ]),
                    SettingGroup::new().title(crate::i18n::tr(cx, "Текст Библии")).items([
                        SettingItem::new(
                            crate::i18n::tr(cx, "Семейство шрифта"),
                            SettingField::scrollable_dropdown(
                                bible_fonts,
                                |cx: &App| app_services(cx).settings.read().reading_font_family.clone().into(),
                                |value, cx| {
                                    let family = value.to_string();
                                    if Self::persist_settings(cx, move |s| s.reading_font_family = family) {
                                        navigation::notify_settings_changed(cx);
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Независимый шрифт текста Писания. Значение «По умолчанию» наследует шрифт интерфейса.")),
                        Self::font_preview_item(FontPreviewTarget::Bible),
                        SettingItem::new(
                            crate::i18n::tr(cx, "Размер текста Библии"),
                            SettingField::dropdown(
                                Self::font_size_options(true),
                                |cx: &App| {
                                    let size = app_services(cx).settings.read().reading_font_size;
                                    if size > 0.0 { format!("{}", size.round() as u32).into() } else { "".into() }
                                },
                                |value, cx| {
                                    let size = value.parse::<f32>().unwrap_or(0.0).clamp(0.0, 40.0);
                                    if Self::persist_settings(cx, move |s| s.reading_font_size = size) {
                                        navigation::notify_settings_changed(cx);
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Размер основного текста иврита, греческого и переводов Библии.")),
                    ]),
                    Self::font_profile(crate::i18n::tr(cx, "Левое меню"), cx, left_fonts,
                        |s| &s.left_panel_font_family, |s, family| s.left_panel_font_family = family,
                        |s| s.left_panel_font_size, |s, size| s.left_panel_font_size = size),
                    Self::font_profile(crate::i18n::tr(cx, "Словари"), cx, dictionary_fonts,
                        |s| &s.dictionary_font_family, |s, family| s.dictionary_font_family = family,
                        |s| s.dictionary_font_size, |s, size| s.dictionary_font_size = size),
                    SettingGroup::new().title(crate::i18n::tr(cx, "Правая панель")).items([
                        SettingItem::new(
                            crate::i18n::tr(cx, "Семейство шрифта"),
                            SettingField::scrollable_dropdown(
                                panel_fonts,
                                |cx: &App| app_services(cx).settings.read().side_panel_font_family.clone().into(),
                                |value, cx| {
                                    let family = value.to_string();
                                    if Self::persist_settings(cx, move |s| s.side_panel_font_family = family) {
                                        navigation::notify_settings_changed(cx);
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Шрифт правой панели и областей с общим профилем.")),
                        Self::font_preview_item(FontPreviewTarget::SidePanels),
                        SettingItem::new(
                            crate::i18n::tr(cx, "Размер текста правой панели"),
                            SettingField::dropdown(
                                Self::font_size_options(false),
                                |cx: &App| format!("{}", app_services(cx).settings.read().side_panel_font_size.round() as u32).into(),
                                |value, cx| {
                                    let size = value.parse::<f32>().unwrap_or(14.0).clamp(10.0, 32.0);
                                    if Self::persist_settings(cx, move |s| s.side_panel_font_size = size) {
                                        navigation::notify_settings_changed(cx);
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Базовый кегль боковых панелей. Относительные подписи сохраняют визуальную иерархию.")),
                        SettingItem::new(
                            crate::i18n::tr(cx, "Дополнительный масштаб панелей"),
                            SettingField::dropdown(
                                vec![
                                    ("0.8".into(), "80 %".into()), ("0.9".into(), "90 %".into()),
                                    ("1.0".into(), "100 %".into()), ("1.1".into(), "110 %".into()),
                                    ("1.2".into(), "120 %".into()), ("1.3".into(), "130 %".into()),
                                    ("1.4".into(), "140 %".into()), ("1.5".into(), "150 %".into()),
                                    ("1.6".into(), "160 %".into()),
                                ],
                                |cx: &App| format!("{:.1}", app_services(cx).settings.read().side_panel_font_scale).into(),
                                |value, cx| {
                                    let scale = value.parse::<f32>().unwrap_or(1.0).clamp(0.8, 1.6);
                                    if Self::persist_settings(cx, move |s| s.side_panel_font_scale = scale) {
                                        navigation::notify_settings_changed(cx);
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Тонкая подстройка поверх базового размера шрифта.")),
                    ]),
                ]),
                SettingPage::new(crate::i18n::tr(cx, "Чтение")).groups([
                    SettingGroup::new().title(crate::i18n::tr(cx, "Содержание текста")).items([
                        SettingItem::new(
                            crate::i18n::tr(cx, "Подсветка морфологии"),
                            SettingField::dropdown(
                                vec![("off".into(), crate::i18n::tr_static("Выкл").into()), ("parts_of_speech".into(), crate::i18n::tr_static("По частям речи").into())],
                                |cx: &App| {
                                    use shell::settings::MorphologyColorScheme::*;
                                    match app_services(cx).settings.read().morphology_color_scheme {
                                        Off => "off".into(),
                                        PartsOfSpeech => "parts_of_speech".into(),
                                    }
                                },
                                |value, cx| {
                                    use shell::settings::MorphologyColorScheme::*;
                                    let mode = if value.as_ref() == "parts_of_speech" { PartsOfSpeech } else { Off };
                                    if Self::persist_settings(cx, move |s| s.morphology_color_scheme = mode) {
                                        navigation::notify_settings_changed(cx);
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Цвета можно переопределить в разделе morphology_colors файла settings.json.")),
                        SettingItem::new(
                            crate::i18n::tr(cx, "Критический аппарат"),
                            SettingField::checkbox(
                                |cx: &App| app_services(cx).settings.read().show_critical_text,
                                |value, cx| {
                                    if Self::persist_settings(cx, move |s| {
                                        s.show_critical_text = value;
                                    }) {
                                        navigation::notify_settings_changed(cx);
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Показывать текстологические пометки ({…}, 「…」, <n>…) в переводе.")),
                        SettingItem::new(
                            crate::i18n::tr(cx, "Номера стихов"),
                            SettingField::checkbox(
                                |cx: &App| app_services(cx).settings.read().show_verse_numbers,
                                |value, cx| {
                                    if Self::persist_settings(cx, move |s| {
                                        s.show_verse_numbers = value;
                                    }) {
                                        navigation::notify_settings_changed(cx);
                                    }
                                },
                            ),
                        )
                        .description(crate::i18n::tr(cx, "Показывать номера стихов в отдельной колонке.")),
                    ]),
                ]),
                crate::sync::page(),
                SettingPage::new(crate::i18n::tr(cx, "Переходы")).groups([SettingGroup::new()
                    .title(crate::i18n::tr(cx, "Библейские ссылки"))
                    .items([SettingItem::new(
                        crate::i18n::tr(cx, "Открывать места Писания"),
                        SettingField::dropdown(
                            vec![
                                ("reuse_current".into(), crate::i18n::tr_static("В текущей вкладке").into()),
                                ("new_tab".into(), crate::i18n::tr_static("В новой вкладке").into()),
                            ],
                            |cx: &App| match app_services(cx).settings.read().link_open_mode {
                                shell::settings::LinkOpenMode::ReuseCurrent => "reuse_current".into(),
                                shell::settings::LinkOpenMode::NewTab => "new_tab".into(),
                            },
                            |value, cx| {
                                let mode = match value.as_ref() {
                                    "new_tab" => shell::settings::LinkOpenMode::NewTab,
                                    _ => shell::settings::LinkOpenMode::ReuseCurrent,
                                };
                                if Self::persist_settings(cx, move |s| {
                                    s.link_open_mode = mode;
                                }) {
                                    navigation::notify_settings_changed(cx);
                                }
                            },
                        ),
                    )
                    .description(crate::i18n::tr(cx, "Куда открываются перекрёстные ссылки, результаты поиска и переходы из статей."))])]),
        ]
    }
}

impl EventEmitter<PanelEvent> for SettingsPanel {}

impl Focusable for SettingsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for SettingsPanel {
    fn panel_name(&self) -> &'static str {
        Self::NAME
    }
}

impl Panel for SettingsPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        crate::i18n::tr_static("Настройки")
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status_color = cx.theme().muted_foreground;
        // The settings widget consumes its pages. Cloning the cached page
        // descriptors only clones the Rc handles to fields and their options.
        if self.pages.is_none() {
            self.pages = Some(self.build_pages(cx));
        }
        let settings =
            Settings::new("bibleown-settings").pages(self.pages.iter().flatten().cloned());
        v_flex()
            .size_full()
            .min_h_0()
            .when_some(self.status.clone(), |this, status| {
                this.child(
                    div()
                        .w_full()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(status_color)
                        .child(status),
                )
            })
            .child(div().flex_1().min_h_0().child(settings))
    }
}
