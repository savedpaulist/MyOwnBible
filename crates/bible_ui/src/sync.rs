use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    scroll::ScrollableElement as _,
    setting::{SettingGroup, SettingItem, SettingPage},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use shell::sync::{Conflict, Kind, SyncService};

use crate::{navigation, services::app_services};

struct SyncUi {
    service: Arc<SyncService>,
    running: bool,
    message: Option<String>,
}

impl Global for SyncUi {}

pub(crate) fn init(cx: &mut App) {
    let data = app_services(cx);
    let service = SyncService::for_services(data);
    cx.set_global(SyncUi {
        service: service.clone(),
        running: true,
        message: None,
    });
    cx.spawn(async move |cx| {
        let initializer = service.clone();
        let result = cx
            .background_executor()
            .spawn(async move { initializer.initialize() })
            .await;
        cx.update(|cx| {
            if let Err(error) = result {
                service.report_error(&error);
            }
            cx.global_mut::<SyncUi>().running = false;
            sync_now(cx);
        });
        loop {
            cx.background_executor()
                .timer(Duration::from_secs(45))
                .await;
            cx.update(sync_now);
        }
    })
    .detach();
}

pub(crate) fn page() -> SettingPage {
    SettingPage::new(crate::i18n::tr_static("Синхронизация")).groups([
        SettingGroup::new().title("Google Drive").items([
            SettingItem::render(|_, _, cx| render_connection(cx)),
        ]),
        SettingGroup::new().title(crate::i18n::tr_static("Перенос данных")).items([
            SettingItem::render(|_, _, cx| {
                v_flex().gap_2()
                    .child(crate::i18n::tr_static("Экспорт создаёт папку с заметками Markdown и файлом bibleown.json. Для экспорта и импорта Google Drive не требуется."))
                    .child(h_flex().gap_2()
                        .child(path_button("sync-export", crate::i18n::tr_static("Экспортировать…"), PathAction::Export, cx))
                        .child(path_button("sync-import", crate::i18n::tr_static("Импортировать…"), PathAction::Import, cx)))
                    .when_some(cx.global::<SyncUi>().message.clone(), |this, message| this.child(message))
            }),
        ]),
    ])
}

fn render_connection(cx: &App) -> AnyElement {
    let ui = cx.global::<SyncUi>();
    let status = ui.service.status();
    let running = ui.running || status.busy;
    let state = if status.connected {
        crate::i18n::tr_static("Google Drive подключён")
    } else if running {
        crate::i18n::tr_static("Выполняется…")
    } else {
        crate::i18n::tr_static("Google Drive отключён")
    };
    let theme = cx.theme();
    v_flex().gap_2()
        .child(crate::i18n::tr_static("Заметки из выбранной папки и записи для изучения Библии синхронизируются напрямую с вашим Google Drive."))
        .child(div().text_color(theme.muted_foreground).child(state))
        .when_some(status.last_success.filter(|_| status.connected), |this, time| {
            let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |now| now.as_secs().saturating_sub(time));
            let label = match elapsed {
                0..=59 => "Последняя синхронизация только что".to_owned(),
                60..=3599 => format!("Минут с последней синхронизации: {}", elapsed / 60),
                _ => format!("Часов с последней синхронизации: {}", elapsed / 3600),
            };
            this.child(div().text_color(theme.muted_foreground).child(label))
        })
        .when_some(status.error, |this, error| this.child(div().text_color(theme.danger).child(error)))
        .child(h_flex().gap_2()
            .when(!status.connected, |this| this
                .child(Button::new("sync-sign-in").small().primary().label(crate::i18n::tr(cx, "Подключить Google Drive"))
                    .disabled(running || !status.configured)
                    .on_click(|_, _, cx| sign_in(cx)))
                .when(!status.bundled_client, |this| this
                    .child(path_button("sync-config", crate::i18n::tr_static("OAuth-клиент…"), PathAction::Configure, cx))))
            .when(status.connected, |this| this
                .child(Button::new("sync-now").small().primary().label(crate::i18n::tr(cx, "Синхронизировать сейчас"))
                    .disabled(running).on_click(|_, _, cx| sync_now(cx)))
                .child(Button::new("sync-sign-in-again").small().label(crate::i18n::tr(cx, "Переподключить…"))
                    .disabled(running).on_click(|_, _, cx| sign_in(cx)))
                .child(Button::new("sync-sign-out").small().ghost().label(crate::i18n::tr(cx, "Выйти из Google-аккаунта"))
                    .disabled(running).on_click(|_, _, cx| run(cx, |service, _| {
                        service.disconnect()?;
                        Ok(Outcome::default())
                    })))))
        .when(!status.configured, |this| this.child(
            div().text_color(theme.muted_foreground).child(crate::i18n::tr(cx, "Выберите JSON-файл OAuth-клиента Google типа Desktop app.")),
        ))
        .when(!status.conflicts.is_empty(), |this| this.child(
            Button::new("sync-conflicts").small()
                .label(format!("Конфликты ({})", status.conflicts.len()))
                .disabled(running).on_click(|_, window, cx| open_conflicts(window, cx)),
        )).into_any_element()
}

#[derive(Clone, Copy)]
enum PathAction {
    Configure,
    Export,
    Import,
}

fn path_button(id: &'static str, label: &'static str, action: PathAction, cx: &App) -> Button {
    Button::new(id).small().label(label).disabled(cx.global::<SyncUi>().running)
        .on_click(move |_, _, cx| {
            let config = matches!(action, PathAction::Configure);
            let prompt = match action {
                PathAction::Configure => "Выберите JSON-файл OAuth-клиента Google",
                PathAction::Export => "Выберите папку, в которой создать экспорт",
                PathAction::Import => "Выберите экспортированную папку BibleOWN",
            };
            let picker = cx.prompt_for_paths(PathPromptOptions {
                files: config, directories: !config, multiple: false, prompt: Some(prompt.into()),
            });
            cx.spawn(async move |cx| {
                match picker.await {
                    Ok(Ok(Some(paths))) => {
                        let Some(path) = paths.into_iter().next() else { return };
                        cx.update(|cx| run(cx, move |service, vault| match action {
                            PathAction::Configure => {
                                service.configure(&path)?;
                                Ok(Outcome::default())
                            }
                            PathAction::Export => {
                                let path = service.export(&path, vault)?;
                                Ok(Outcome { message: Some(format!("Данные экспортированы в {}", path.display())), ..Outcome::default() })
                            }
                            PathAction::Import => {
                                service.import(&path, vault)?;
                                Ok(Outcome { message: Some("Импорт завершён. Разные версии одной записи доступны в разделе «Конфликты».".into()), changed: true, ..Outcome::default() })
                            }
                        }));
                    }
                    Ok(Ok(None)) => {}
                    _ => cx.update(|cx| {
                        cx.global_mut::<SyncUi>().message = Some("Не удалось открыть выбор файла или папки.".into());
                        cx.refresh_windows();
                    }),
                }
            }).detach();
        })
}

#[derive(Default)]
struct Outcome {
    message: Option<String>,
    changed: bool,
    sync_after: bool,
}

fn run(
    cx: &mut App,
    action: impl FnOnce(Arc<SyncService>, Option<PathBuf>) -> anyhow::Result<Outcome> + Send + 'static,
) {
    let vault = app_services(cx).settings.read().notes_dir.clone();
    let ui = cx.global_mut::<SyncUi>();
    if ui.running {
        return;
    }
    ui.running = true;
    let service = ui.service.clone();
    cx.refresh_windows();
    cx.spawn(async move |cx| {
        let worker = service.clone();
        let result = cx
            .background_executor()
            .spawn(async move { action(worker, vault) })
            .await;
        cx.update(|cx| {
            let ui = cx.global_mut::<SyncUi>();
            ui.running = false;
            match result {
                Ok(outcome) => {
                    if let Some(message) = outcome.message {
                        ui.message = Some(message);
                    }
                    if outcome.changed {
                        navigation::notify_annotations_changed(cx);
                        navigation::notify_chains_changed(cx);
                        navigation::notify_tags_changed(cx);
                        navigation::bus(cx).update(cx, |_, cx| cx.emit(navigation::UserDataSynced));
                    }
                    if outcome.sync_after {
                        sync_now(cx);
                    }
                }
                Err(error) => service.report_error(&error),
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

fn sync_now(cx: &mut App) {
    if cx.global::<SyncUi>().service.status().connected {
        run(cx, |service, vault| {
            Ok(Outcome {
                changed: service.run(vault)?,
                ..Outcome::default()
            })
        });
    } else {
        cx.refresh_windows();
    }
}

fn sign_in(cx: &mut App) {
    let service = cx.global::<SyncUi>().service.clone();
    match service.start_login() {
        Ok(login) => {
            cx.open_url(login.url());
            run(cx, |service, _| {
                service.complete_login(login)?;
                Ok(Outcome {
                    sync_after: true,
                    ..Outcome::default()
                })
            });
        }
        Err(error) => {
            service.report_error(&error);
            cx.refresh_windows();
        }
    }
}

fn open_conflicts(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        let status = cx.global::<SyncUi>().service.status();
        dialog
            .title(crate::i18n::tr(cx, "Конфликты синхронизации"))
            .w(px(720.))
            .child(
                v_flex()
                    .gap_3()
                    .max_h(px(480.))
                    .overflow_y_scrollbar()
                    .children(status.conflicts.into_iter().map(render_conflict)),
            )
    });
}

fn render_conflict(conflict: Conflict) -> impl IntoElement {
    let title = match conflict.key.kind {
        Kind::Annotation => "Оформление стиха или слова",
        Kind::VerseComment => "Комментарий к стиху",
        Kind::Chain => "Цепочка",
        Kind::Tag => "Тег",
        Kind::Question => "Вопрос",
        Kind::CrossReference => "Параллельное место",
        Kind::Note => "Заметка",
    };
    let note = conflict.key.kind == Kind::Note;
    v_flex()
        .gap_2()
        .p_2()
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
        .when(note, |this| this.child(conflict.key.id.clone()))
        .children(conflict.variants.into_iter().map(|variant| {
            let key = conflict.key.clone();
            let id = variant.id;
            let preview = match variant.value {
                None => "Удаление записи".to_owned(),
                Some(value) if note => value.as_str().unwrap_or_default().to_owned(),
                Some(value) => serde_json::to_string_pretty(&value).unwrap_or_default(),
            };
            v_flex()
                .gap_1()
                .p_2()
                .border_1()
                .rounded_md()
                .child(
                    div()
                        .id(SharedString::from(format!("sync-preview-{id}")))
                        .text_sm()
                        .max_h(px(180.))
                        .overflow_y_scroll()
                        .child(preview),
                )
                .child(
                    Button::new(SharedString::from(format!("resolve-{id}")))
                        .small()
                        .label(crate::i18n::tr_static("Выбрать эту версию"))
                        .on_click(move |_, window, cx| {
                            let key = key.clone();
                            window.close_dialog(cx);
                            run(cx, move |service, vault| {
                                service.resolve(&key, id, vault)?;
                                Ok(Outcome {
                                    changed: true,
                                    ..Outcome::default()
                                })
                            });
                        }),
                )
        }))
}
