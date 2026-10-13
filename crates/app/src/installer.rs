//! Installer / uninstaller screens. The same executable becomes the installer when it is started as
//! `SpaceTree-Setup.exe` (or with `--install`) and the uninstaller with `--uninstall`.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use eframe::egui::{self, Align, Id, Layout, RichText, Ui, vec2};

use crate::{App, Mode, theme, winint};

pub struct Install {
    desktop: bool,
    context_menu: bool,
    job: Option<Receiver<Result<PathBuf, String>>>,
    result: Option<Result<PathBuf, String>>,
}

impl Default for Install {
    fn default() -> Self {
        Install {
            desktop: true,
            context_menu: true,
            job: None,
            result: None,
        }
    }
}

#[derive(Default)]
pub struct Uninstall {
    job: Option<Receiver<Result<(), String>>>,
    result: Option<Result<(), String>>,
}

impl App {
    pub fn installer_ui(&mut self, ui: &mut Ui) {
        let icon = self.icon.clone();
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().show(ui, |ui| {
            let appear = ctx.animate_bool_with_time(Id::new("setup_in"), true, 0.3);
            ui.multiply_opacity(appear);
            ui.add_space(18.0 + (1.0 - appear) * 12.0);
            ui.vertical_centered(|ui| {
                if let Some(icon) = &icon {
                    ui.add(egui::Image::new(icon).fit_to_exact_size(vec2(64.0, 64.0)));
                }
                ui.add_space(6.0);
                ui.label(RichText::new("SpaceTree").size(24.0).strong());
                ui.label(RichText::new(format!("версия {}", env!("CARGO_PKG_VERSION"))).weak());
            });
            ui.add_space(14.0);
            match &mut self.mode {
                Mode::Install(s) => install_body(ui, s),
                Mode::Uninstall(s) => uninstall_body(ui, s),
                Mode::App => {}
            }
        });
    }
}

fn install_body(ui: &mut Ui, s: &mut Install) {
    if let Some(rx) = &s.job {
        if let Ok(r) = rx.try_recv() {
            s.result = Some(r);
            s.job = None;
        } else {
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.add(egui::Spinner::new().size(32.0).color(theme::ACCENT));
                ui.label("Установка…");
            });
            ui.ctx().request_repaint();
            return;
        }
    }
    match &s.result {
        Some(Ok(exe)) => {
            let exe = exe.clone();
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("✔ Готово").size(18.0).color(theme::SAFE).strong());
                ui.add_space(4.0);
                ui.label("SpaceTree установлен. Его можно найти через поиск в меню «Пуск»,");
                ui.label("а папку — просканировать правым кликом › «Сканировать в SpaceTree».");
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.add_space((ui.available_width() - 260.0).max(0.0) / 2.0);
                    if theme::primary_button(ui, "Запустить SpaceTree").clicked() {
                        let _ = std::process::Command::new(&exe).spawn();
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button("Закрыть").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        }
        Some(Err(e)) => {
            ui.label(RichText::new("Не удалось установить").strong().color(theme::REVIEW));
            ui.label(e.as_str());
            if ui.button("Попробовать ещё раз").clicked() {
                s.result = None;
            }
        }
        None => {
            let dir = winint::install_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_default();
            let upgrade = winint::installed_exe().is_some_and(|e| e.exists());
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Папка установки").weak());
                ui.label(dir);
                ui.add_space(6.0);
                ui.checkbox(&mut s.desktop, "Ярлык на рабочем столе");
                ui.checkbox(
                    &mut s.context_menu,
                    "Пункт «Сканировать в SpaceTree» в меню папок и дисков",
                );
                ui.label(
                    RichText::new("Ярлык в меню «Пуск» создаётся всегда. Права администратора не нужны.")
                        .weak()
                        .small(),
                );
            });
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Отмена").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                let label = if upgrade {
                    "Обновить"
                } else {
                    "Установить"
                };
                if theme::primary_button(ui, label).clicked() {
                    let (tx, rx) = channel();
                    let opts = winint::InstallOptions {
                        desktop_shortcut: s.desktop,
                        context_menu: s.context_menu,
                    };
                    let ctx = ui.ctx().clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(winint::install(opts));
                        ctx.request_repaint();
                    });
                    s.job = Some(rx);
                }
            });
        }
    }
}

fn uninstall_body(ui: &mut Ui, s: &mut Uninstall) {
    if let Some(rx) = &s.job {
        if let Ok(r) = rx.try_recv() {
            s.result = Some(r);
            s.job = None;
        } else {
            ui.vertical_centered(|ui| {
                ui.add(egui::Spinner::new().size(28.0).color(theme::ACCENT));
                ui.label("Удаление…");
            });
            ui.ctx().request_repaint();
            return;
        }
    }
    match &s.result {
        None => {
            ui.vertical_centered(|ui| {
                ui.label("Удалить SpaceTree с этого компьютера?");
                ui.label(RichText::new("Будут удалены программа, ярлыки, пункт меню папок и настройки.").weak());
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.add_space((ui.available_width() - 180.0).max(0.0) / 2.0);
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Удалить").color(egui::Color32::WHITE)).fill(theme::REVIEW),
                        )
                        .clicked()
                    {
                        let (tx, rx) = channel();
                        let ctx = ui.ctx().clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(winint::uninstall());
                            ctx.request_repaint();
                        });
                        s.job = Some(rx);
                    }
                    if ui.button("Отмена").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        }
        Some(Ok(())) => {
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("SpaceTree удалён").size(17.0).strong());
                ui.add_space(10.0);
                if ui.button("Закрыть").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        }
        Some(Err(e)) => {
            ui.label(RichText::new("Не удалось удалить").strong().color(theme::REVIEW));
            ui.label(e.as_str());
        }
    }
}
