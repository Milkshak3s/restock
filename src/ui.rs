use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use eframe::egui::{self, Color32, RichText};

use crate::catalog::{AppEntry, Catalog};
use crate::source::{self, Release};
use crate::state::State;
use crate::{detect, install, version};

const GOOD: Color32 = Color32::from_rgb(90, 180, 110);

enum Msg {
    Detected(Vec<Option<String>>),
    Latest(usize, Result<Release, String>),
    Status(usize, Status),
    InstallFinished,
}

#[derive(Clone, PartialEq)]
enum Status {
    Idle,
    Working(String),
    Done(String),
    Failed(String),
}

#[derive(PartialEq)]
enum Action {
    Install,
    Update,
    UpToDate,
}

struct Row {
    app: AppEntry,
    installed: Option<String>,
    latest: Option<Result<Release, String>>,
    status: Status,
    selected: bool,
}

impl Row {
    fn release(&self) -> Option<&Release> {
        self.latest.as_ref()?.as_ref().ok()
    }

    fn action(&self) -> Option<Action> {
        let latest = self.release()?;
        Some(match &self.installed {
            None => Action::Install,
            Some(current) if version::is_newer(&latest.version, current) => Action::Update,
            Some(_) => Action::UpToDate,
        })
    }
}

/// Sends a message to the UI thread and wakes it to redraw.
#[derive(Clone)]
struct Notifier {
    tx: Sender<Msg>,
    ctx: egui::Context,
}

impl Notifier {
    fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }
}

pub struct RestockApp {
    rows: Vec<Row>,
    notifier: Notifier,
    rx: Receiver<Msg>,
    installing: bool,
}

impl RestockApp {
    pub fn new(cc: &eframe::CreationContext<'_>, catalog: Catalog) -> Self {
        let (tx, rx) = channel();
        let rows = catalog
            .apps
            .into_iter()
            .map(|app| Row {
                app,
                installed: None,
                latest: None,
                status: Status::Idle,
                selected: false,
            })
            .collect();
        let mut app = Self {
            rows,
            notifier: Notifier {
                tx,
                ctx: cc.egui_ctx.clone(),
            },
            rx,
            installing: false,
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        self.spawn_detect();
        let client = match source::client() {
            Ok(c) => c,
            Err(e) => {
                for row in &mut self.rows {
                    row.latest = Some(Err(e.to_string()));
                }
                return;
            }
        };
        for (i, row) in self.rows.iter_mut().enumerate() {
            row.latest = None;
            row.status = Status::Idle;
            let (client, source, notifier) = (
                client.clone(),
                row.app.source.clone(),
                self.notifier.clone(),
            );
            thread::spawn(move || {
                let result = source::resolve(&client, &source).map_err(|e| format!("{e:#}"));
                notifier.send(Msg::Latest(i, result));
            });
        }
    }

    fn spawn_detect(&self) {
        let apps: Vec<AppEntry> = self.rows.iter().map(|r| r.app.clone()).collect();
        let notifier = self.notifier.clone();
        thread::spawn(move || notifier.send(Msg::Detected(detect_all(&apps))));
    }

    fn install_selected(&mut self) {
        let jobs: Vec<(usize, AppEntry, Release)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.selected)
            .filter_map(|(i, r)| Some((i, r.app.clone(), r.release()?.clone())))
            .collect();
        if jobs.is_empty() {
            return;
        }
        self.installing = true;

        let notifier = self.notifier.clone();
        let apps: Vec<AppEntry> = self.rows.iter().map(|r| r.app.clone()).collect();
        thread::spawn(move || {
            let client = source::client();
            // Installers run one at a time: MSI holds a global mutex and many
            // EXE installers misbehave when run concurrently.
            for (i, app, release) in jobs {
                let status = |s: Status| notifier.send(Msg::Status(i, s));
                let result = client
                    .as_ref()
                    .map_err(|e| anyhow::anyhow!("{e}"))
                    .and_then(|client| {
                        let dir = std::env::temp_dir().join("restock").join(&app.id);
                        let file = install::download(client, &release, &dir, |p| {
                            status(Status::Working(p))
                        })?;
                        status(Status::Working("Installing…".into()));
                        install::install(&app.installer, &file)?;
                        let _ = std::fs::remove_dir_all(&dir);
                        let mut state = State::load();
                        state
                            .installed
                            .insert(app.id.clone(), release.version.clone());
                        state.save()
                    });
                status(match result {
                    Ok(()) => Status::Done(format!("Installed {}", release.version)),
                    Err(e) => Status::Failed(format!("{e:#}")),
                });
            }
            notifier.send(Msg::Detected(detect_all(&apps)));
            notifier.send(Msg::InstallFinished);
        });
    }

    fn drain_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Detected(versions) => {
                    for (row, installed) in self.rows.iter_mut().zip(versions) {
                        row.installed = installed;
                    }
                }
                Msg::Latest(i, result) => self.rows[i].latest = Some(result),
                Msg::Status(i, status) => {
                    if matches!(status, Status::Done(_)) {
                        self.rows[i].selected = false;
                    }
                    self.rows[i].status = status;
                }
                Msg::InstallFinished => self.installing = false,
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Restock");
            ui.separator();
            ui.add_enabled_ui(!self.installing, |ui| {
                if ui.button("⟳ Refresh").clicked() {
                    self.refresh();
                }
                if ui
                    .button("Select essentials")
                    .on_hover_text("Missing or outdated apps marked essential")
                    .clicked()
                {
                    for row in &mut self.rows {
                        row.selected = row.app.essential
                            && matches!(row.action(), Some(Action::Install | Action::Update));
                    }
                }
                if ui.button("Select updates").clicked() {
                    for row in &mut self.rows {
                        row.selected = row.action() == Some(Action::Update);
                    }
                }
                if ui.button("Clear").clicked() {
                    for row in &mut self.rows {
                        row.selected = false;
                    }
                }
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let count = self.rows.iter().filter(|r| r.selected).count();
                let label = if self.installing {
                    "Working…".to_owned()
                } else {
                    format!("Install / update selected ({count})")
                };
                if ui
                    .add_enabled(!self.installing && count > 0, egui::Button::new(label))
                    .clicked()
                {
                    self.install_selected();
                }
            });
        });
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        let installing = self.installing;
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("apps")
                .num_columns(5)
                .striped(true)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label("");
                    ui.strong("App");
                    ui.strong("Installed");
                    ui.strong("Latest");
                    ui.strong("Status");
                    ui.end_row();

                    for row in &mut self.rows {
                        let can_select = !installing && row.release().is_some();
                        ui.add_enabled(can_select, egui::Checkbox::without_text(&mut row.selected));

                        let name = ui
                            .horizontal(|ui| {
                                ui.label(RichText::new(&row.app.name).strong());
                                if row.app.essential {
                                    ui.label(RichText::new("essential").small().weak());
                                }
                            })
                            .response;
                        if !row.app.description.is_empty() {
                            name.on_hover_text(&row.app.description);
                        }

                        ui.label(row.installed.as_deref().unwrap_or("—"));

                        match &row.latest {
                            None => ui.spinner(),
                            Some(Ok(r)) => ui.label(&r.version),
                            Some(Err(e)) => ui
                                .colored_label(ui.visuals().error_fg_color, "error")
                                .on_hover_text(e),
                        };

                        status_cell(ui, row);
                        ui.end_row();
                    }
                });
        });
    }
}

fn status_cell(ui: &mut egui::Ui, row: &Row) {
    let visuals = ui.visuals().clone();
    match &row.status {
        Status::Working(msg) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(msg);
            });
        }
        Status::Done(msg) => {
            ui.colored_label(GOOD, msg);
        }
        Status::Failed(err) => {
            ui.colored_label(visuals.error_fg_color, "Failed")
                .on_hover_text(err);
        }
        Status::Idle => match row.action() {
            None => {
                ui.label("");
            }
            Some(Action::Install) => {
                ui.label("Not installed");
            }
            Some(Action::Update) => {
                ui.colored_label(visuals.warn_fg_color, "Update available");
            }
            Some(Action::UpToDate) => {
                ui.colored_label(GOOD, "Up to date");
            }
        },
    }
}

fn detect_all(apps: &[AppEntry]) -> Vec<Option<String>> {
    let entries = detect::uninstall_entries();
    let state = State::load();
    apps.iter()
        .map(|app| detect::installed_version(app, &entries, &state))
        .collect()
}

impl eframe::App for RestockApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_messages();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ui, |ui| self.table(ui));
    }
}
