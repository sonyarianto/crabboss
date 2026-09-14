//! Play-log reports: ranged lists, CSV/XLSX export, and the 24h
//! "recently played" strip (same log, all kinds).

use crabcore::library::TrackKind;

use super::super::App;
use crate::widgets::report_range_bounds;

pub(crate) fn range_changed(state: &mut App, i: usize) {
    state.report_range = i.min(3);
    state.refresh_report();
}

pub(crate) fn export(state: &mut App) {
    let path = rfd::FileDialog::new()
        .set_title("Export play report (CSV / XLSX)")
        .set_file_name("crabboss-report.csv")
        .add_filter("CSV", &["csv"])
        .add_filter("Excel", &["xlsx"])
        .save_file();
    let Some(path) = path else {
        return;
    };
    let (from, _) = report_range_bounds(state.report_range);
    let entries = crabcore::report::play_report(
        &state.library,
        from,
        chrono::Utc::now(),
        &[TrackKind::Jingle, TrackKind::Ad],
    )
    .unwrap_or_default();
    // Format follows the chosen extension (CSV stays the default);
    // anything else falls back to CSV rather than failing the export.
    let xlsx = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("xlsx"));
    let bytes = if xlsx {
        match crabcore::report::to_xlsx(&entries) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Report export failed: {e}");
                state.report_summary = format!("Export failed: {e}");
                return;
            }
        }
    } else {
        crabcore::report::to_csv(&entries).into_bytes()
    };
    match std::fs::write(&path, bytes) {
        Ok(()) => {
            tracing::info!(
                "Report exported: {} ({} rows)",
                path.display(),
                entries.len()
            );
            state.report_summary = format!("Exported {} rows to {}", entries.len(), path.display());
        }
        Err(e) => tracing::error!("Report export failed: {}", e),
    }
}

impl App {
    pub(crate) fn refresh_report(&mut self) {
        // Generated snapshot (not live state): on a read failure the
        // screen clears and says so, instead of showing a stale or
        // half-built report as if valid.
        let fail = |me: &mut Self, e: &crabcore::CrabError| {
            let msg = format!("Report read failed: {e}");
            tracing::warn!("{msg}");
            me.report_summary = msg;
            me.report_entries.clear();
            me.recent_plays.clear();
        };
        let (from, label) = report_range_bounds(self.report_range);
        let to = chrono::Utc::now();
        let entries = match crabcore::report::play_report(
            &self.library,
            from,
            to,
            &[TrackKind::Jingle, TrackKind::Ad],
        ) {
            Ok(e) => e,
            Err(e) => {
                fail(self, &e);
                return;
            }
        };
        let airtime: f64 = entries.iter().filter_map(|e| e.duration_secs).sum();
        self.report_summary = format!(
            "{}: {} plays - {:.0} min music airtime (jingles/ads excluded{})",
            label,
            entries.len(),
            airtime / 60.0,
            if entries.len() > 100 {
                "; showing newest 100"
            } else {
                ""
            }
        );
        self.report_entries = entries.into_iter().take(100).collect();
        // Recently played: same log, last 24h, all kinds (jingles/ads get
        // their kind tag in the view instead of being hidden).
        let day_ago = to - chrono::Duration::hours(24);
        self.recent_plays = match crabcore::report::play_report(&self.library, day_ago, to, &[]) {
            Ok(r) => r.into_iter().take(15).collect(),
            Err(e) => {
                fail(self, &e);
                return;
            }
        };
    }
}
