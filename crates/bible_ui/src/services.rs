//! Глобальный доступ к сервисам приложения из UI (composition root, §5.2).
//!
//! `AppServices` живёт в shell и не знает о GPUI; здесь — только тонкая
//! регистрация структуры как глобала GPUI, чтобы панели могли её читать.
//! Внутри — Arc: тяжёлые сценарии (поиск, построение индекса) уносят
//! клон на фоновый поток.

use std::sync::Arc;

use gpui_kit::{App, Global};
use shell::services::AppServices;

pub struct Services(pub Arc<AppServices>);

impl Global for Services {}

pub fn app_services(cx: &App) -> &AppServices {
    &cx.global::<Services>().0
}

/// Arc-клон для фоновых задач (поиск выполняется вне UI-потока).
pub fn app_services_arc(cx: &App) -> Arc<AppServices> {
    cx.global::<Services>().0.clone()
}
