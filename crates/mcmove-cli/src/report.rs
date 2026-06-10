//! Terminal front-end for `mcmove_core::Reporter`, backed by `indicatif`.

use std::sync::Mutex;

use indicatif::{ProgressBar, ProgressStyle};
use mcmove_core::{Progress, Reporter};

/// Renders core progress events as an `indicatif` progress bar plus log lines.
pub struct CliReporter {
    bar: Mutex<Option<ProgressBar>>,
}

impl CliReporter {
    pub fn new() -> Self {
        Self { bar: Mutex::new(None) }
    }
}

impl Reporter for CliReporter {
    fn report(&self, event: Progress) {
        let mut slot = self.bar.lock().unwrap();
        match event {
            Progress::Phase { name } => {
                if let Some(bar) = slot.take() {
                    bar.finish_and_clear();
                }
                let bar = ProgressBar::new_spinner();
                bar.set_message(name);
                *slot = Some(bar);
            }
            Progress::Total { units } => {
                if let Some(bar) = slot.as_ref() {
                    bar.set_length(units);
                    bar.set_style(
                        ProgressStyle::with_template("{msg} [{bar:30}] {pos}/{len}")
                            .unwrap()
                            .progress_chars("=>-"),
                    );
                }
            }
            Progress::Advance { units, label } => {
                if let Some(bar) = slot.as_ref() {
                    if !label.is_empty() {
                        bar.set_message(label);
                    }
                    bar.inc(units);
                }
            }
            Progress::Info { message } => match slot.as_ref() {
                Some(bar) => bar.println(message),
                None => println!("{message}"),
            },
            Progress::Warn { message } => {
                let line = format!("! {message}");
                match slot.as_ref() {
                    Some(bar) => bar.println(line),
                    None => eprintln!("{line}"),
                }
            }
            Progress::PhaseDone { name } => {
                if let Some(bar) = slot.take() {
                    bar.finish_with_message(name);
                }
            }
        }
    }
}
