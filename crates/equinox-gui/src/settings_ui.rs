//! Generates setting controls from [`SettingSpec`]; control changes are
//! written to the config and persisted immediately.

use gio::prelude::*;
use libadwaita as Adw;
use libadwaita::prelude::*;
use serde_json::json;

use equinox_core::config::Config;
use equinox_core::source::{SettingSpec, SourceSettings};

/// Build controls for all settings of a source.
pub fn build_rows(config: &Config, source_id: &str, specs: &[SettingSpec]) -> Vec<gtk4::Widget> {
    specs
        .iter()
        .map(|s| build_row(config, source_id, s))
        .collect()
}

fn build_row(config: &Config, source_id: &str, spec: &SettingSpec) -> gtk4::Widget {
    let settings: SourceSettings = config.settings_for(source_id);
    let cfg = config.clone();
    let sid = source_id.to_owned();
    let key = spec.key().to_owned();

    match spec {
        SettingSpec::String { label, default, .. } => {
            let row = Adw::EntryRow::new();
            row.set_title(label);
            row.set_text(&settings.get_str(&key, default));
            let c = cfg.clone();
            let s = sid.clone();
            let k = key.clone();
            row.connect_text_notify(move |row| {
                c.set_source_setting(&s, &k, json!(row.text().to_string()));
            });
            row.upcast()
        }
        SettingSpec::Int {
            label,
            default,
            min,
            max,
            ..
        } => {
            let current = settings.get_i64(&key, *default).clamp(*min, *max) as f64;
            let adj = gtk4::Adjustment::new(current, *min as f64, *max as f64, 1.0, 10.0, 0.0);
            let row = Adw::SpinRow::new(Some(&adj), 0.0, 0);
            row.set_title(label);
            let c = cfg.clone();
            let s = sid.clone();
            let k = key.clone();
            adj.connect_value_changed(move |a| {
                c.set_source_setting(&s, &k, json!(a.value() as i64));
            });
            row.upcast()
        }
        SettingSpec::Bool { label, default, .. } => {
            let row = Adw::SwitchRow::new();
            row.set_title(label);
            row.set_active(settings.get_bool(&key, *default));
            let c = cfg.clone();
            let s = sid.clone();
            let k = key.clone();
            row.connect_active_notify(move |sw| {
                c.set_source_setting(&s, &k, json!(sw.is_active()));
            });
            row.upcast()
        }
        SettingSpec::Choice {
            label,
            choices,
            default,
            ..
        } => {
            let strings: Vec<&str> = choices
                .iter()
                .map(|(_, display)| display.as_str())
                .collect();
            let model = gtk4::StringList::new(&strings);
            let row = Adw::ComboRow::new();
            row.set_title(label);
            row.set_model(Some(&model));
            let current = settings.get_str(&key, default);
            let idx = choices.iter().position(|(v, _)| *v == current).unwrap_or(0) as u32;
            row.set_selected(idx);
            // Owned copy so the 'static closure doesn't borrow the spec
            let choices: Vec<(&'static str, String)> = choices.clone();
            let c = cfg.clone();
            let s = sid.clone();
            let k = key.clone();
            row.connect_selected_notify(move |row| {
                if let Some((v, _)) = choices.get(row.selected() as usize) {
                    c.set_source_setting(&s, &k, json!(v));
                }
            });
            row.upcast()
        }
    }
}
