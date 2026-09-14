//! Play-log reports: ranged lists, CSV export, and the 24h
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
        .set_title("Export play report (CSV)")
        .set_file_name("crabboss-report.csv")
        .add_filter("CSV", &["csv"])
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
    match std::fs::write(&path, crabcore::report::to_csv(&entries)) {
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
