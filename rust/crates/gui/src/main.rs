//! `claw-gui` — лёгкий egui-фронтенд для агента claw.
//!
//! UI-поток рисует egui; фоновый поток ([`agent`]) владеет `ConversationRuntime`
//! и стримит события в UI через каналы. Модель по умолчанию — локальный
//! OpenAI-совместимый `qwen35-397b-a17b-fp8` (см. [`config`]).

mod agent;
mod app;
mod config;
mod protocol;
mod slash;

use app::AgentApp;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("claw")
            .with_inner_size([1000.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "claw-gui",
        native_options,
        Box::new(|_cc| Ok(Box::new(AgentApp::new()))),
    )
}
