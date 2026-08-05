//! Historical Usage Observatory TUI state (C-522..C-524).
//!
//! Rendering is a pure projection to text rows so layout/state tests do not need a terminal clock.
//! The chat renderer applies the active theme to these rows; no color carries meaning here.

use flux_capabilities::usage_observatory::{
    buckets, compare_previous, flux_facts, groups, replay_frame, GroupBy, ReplayClock, UsageFact,
    UsageFilter, UsageRange,
};
use flux_core::PricingTable;
use flux_events::EventStore;

pub(crate) const HELP: &str = "Space play/pause · r restart · ←/→ seek · +/- speed · f fit · 4/1/7 window · g group · m motion · Esc close";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservatoryLayout {
    Wide,
    Medium,
    Compact,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UsageObservatory {
    pub(crate) facts: Vec<UsageFact>,
    pub(crate) filter: UsageFilter,
    pub(crate) group_by: GroupBy,
    pub(crate) clock: ReplayClock,
    pub(crate) focused: usize,
}

impl UsageObservatory {
    pub(crate) fn new(facts: Vec<UsageFact>, range: UsageRange) -> Self {
        Self {
            facts,
            filter: UsageFilter::default(),
            group_by: GroupBy::Harness,
            clock: ReplayClock::new(range),
            focused: 0,
        }
    }

    /// Metadata-only Flux loader. Cross-harness adapters can append facts before construction; this
    /// path intentionally asks the event store only for stream ids and typed events.
    pub(crate) fn from_store(
        store: &EventStore,
        pricing: &PricingTable,
    ) -> flux_core::Result<Self> {
        let mut facts = Vec::new();
        for stream in store.all_streams()? {
            let events = store.load_stream(&stream, None)?;
            facts.extend(flux_facts(&stream, &events, pricing));
        }
        let end = facts
            .iter()
            .filter_map(UsageFact::event_ms)
            .max()
            .unwrap_or(UsageRange::DAY_MS);
        Ok(Self::new(
            facts,
            UsageRange::trailing(end.saturating_add(1), UsageRange::DAY_MS),
        ))
    }

    pub(crate) fn set_window(&mut self, duration_ms: i64) {
        let end = self.clock.range.end_ms;
        self.clock
            .rebase(UsageRange::trailing(end, duration_ms.max(1)));
        self.focused = 0;
    }

    pub(crate) fn cycle_group(&mut self) {
        self.group_by = match self.group_by {
            GroupBy::Harness => GroupBy::Provider,
            GroupBy::Provider => GroupBy::Model,
            GroupBy::Model => GroupBy::Route,
            GroupBy::Route => GroupBy::Harness,
        };
        self.focused = 0;
    }

    pub(crate) fn change_speed(&mut self, faster: bool) {
        const SPEEDS: &[f64] = &[0.5, 1.0, 2.0, 5.0, 10.0, 25.0, 50.0, 100.0];
        let index = SPEEDS
            .iter()
            .position(|value| *value == self.clock.speed)
            .unwrap_or(1);
        let next = if faster {
            (index + 1).min(SPEEDS.len() - 1)
        } else {
            index.saturating_sub(1)
        };
        self.clock.set_speed(SPEEDS[next]);
    }

    pub(crate) fn layout(width: u16) -> ObservatoryLayout {
        if width >= 100 {
            ObservatoryLayout::Wide
        } else if width >= 64 {
            ObservatoryLayout::Medium
        } else {
            ObservatoryLayout::Compact
        }
    }

    /// Pure, bounded rows for the active cursor and filters.
    pub(crate) fn lines(&self, width: u16, height: u16) -> Vec<String> {
        let layout = Self::layout(width);
        let plot_width = match layout {
            ObservatoryLayout::Wide => width.saturating_sub(18),
            ObservatoryLayout::Medium => width.saturating_sub(12),
            ObservatoryLayout::Compact => width.saturating_sub(4),
        }
        .max(1) as usize;
        let frame = replay_frame(&self.facts, &self.clock, &self.filter, plot_width.min(32));
        let comparison = compare_previous(&self.facts, self.clock.range, &self.filter);
        let rows = groups(&self.facts, self.clock.range, &self.filter, self.group_by);
        let series = buckets(&self.facts, self.clock.range, plot_width, &self.filter);
        let play = if self.clock.playing { "▶" } else { "Ⅱ" };
        let motion = if self.clock.reduced_motion {
            "no-motion"
        } else {
            "motion"
        };
        let mut out = vec![format!(
            " usage observatory · {}× · {play} · {motion} · cursor {} / {} ",
            self.clock.speed,
            self.clock
                .cursor_ms
                .saturating_sub(self.clock.range.start_ms),
            self.clock.range.duration_ms(),
        )];
        out.push(format!(
            " tokens {} · cost ${:.4} + {} unpriced · calls {} · sessions {} ",
            frame.cumulative.usage.total(),
            frame.cumulative.priced_usd(),
            frame.cumulative.unpriced_calls,
            frame.cumulative.calls,
            frame.cumulative.sessions,
        ));
        if self.facts.is_empty() {
            out.push(" no usage metadata in selected range ".into());
        } else {
            let spark = series
                .iter()
                .map(|bucket| match bucket.totals.usage.total() {
                    0 => '·',
                    1..=999 => '▂',
                    1_000..=9_999 => '▄',
                    _ => '█',
                })
                .collect::<String>();
            out.push(format!(" timeline {spark}"));
            if layout != ObservatoryLayout::Compact {
                out.push(" activity flow (provider is unknown unless source-proven) ".into());
                for pulse in frame.pulses.iter().take(3) {
                    out.push(format!(
                        " {} → {} → {}  ×{} · {} tok · {} ",
                        pulse.harness.label(),
                        pulse.provider,
                        pulse.model,
                        pulse.count,
                        pulse.tokens,
                        pulse.cost_status.as_str(),
                    ));
                }
            }
            out.push(format!(
                " group: {:?} · compare: previous equal period ",
                self.group_by
            ));
            for (index, row) in rows
                .iter()
                .take(match layout {
                    ObservatoryLayout::Wide => 8,
                    ObservatoryLayout::Medium => 5,
                    ObservatoryLayout::Compact => 3,
                })
                .enumerate()
            {
                let focus = if index == self.focused { '›' } else { ' ' };
                out.push(format!(
                    " {focus} {} · {} calls · {} tok · ${:.4} + {} unpriced ",
                    row.key,
                    row.totals.calls,
                    row.totals.usage.total(),
                    row.totals.priced_usd(),
                    row.totals.unpriced_calls,
                ));
            }
            let token_delta = comparison
                .token_percent
                .map(|value| format!("{value:+.1}%"))
                .unwrap_or_else(|| "no baseline".into());
            out.push(format!(" previous period · tokens {token_delta}"));
        }
        out.push(format!(" {HELP} "));
        out.truncate(height.max(1) as usize);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_capabilities::harness::HarnessKind;
    use flux_capabilities::usage_observatory::{ProviderAttribution, TimePrecision};
    use flux_core::Usage;

    fn fixture() -> Vec<UsageFact> {
        HarnessKind::ALL
            .into_iter()
            .enumerate()
            .map(|(index, harness)| {
                UsageFact::priced(
                    harness,
                    format!("s{index}"),
                    if index == 0 {
                        "routed/vendor/model"
                    } else {
                        "gpt-5"
                    },
                    if index == 3 {
                        ProviderAttribution::Proven("opencode-provider".into())
                    } else {
                        ProviderAttribution::Unknown
                    },
                    Some(index as i64 + 1),
                    TimePrecision::Call,
                    Usage {
                        input_tokens: (index + 1) as u64,
                        ..Default::default()
                    },
                    &PricingTable::builtin(),
                )
            })
            .collect()
    }

    #[test]
    fn observatory_panels_share_cursor_and_filters() {
        let mut view = UsageObservatory::new(fixture(), UsageRange::new(0, 10).unwrap());
        view.clock.seek(3);
        view.filter.harnesses.insert(HarnessKind::Flux);
        let text = view.lines(120, 40).join("\n");
        assert!(text.contains("calls 1"));
        assert!(text.contains("flux"));
        assert!(!text.contains("Codex →"));
    }

    #[test]
    fn usage_observatory_layout_matrix() {
        let mut states = vec![UsageObservatory::new(
            Vec::new(),
            UsageRange::new(0, 10).unwrap(),
        )];
        let mut rich = UsageObservatory::new(fixture(), UsageRange::new(0, 10).unwrap());
        rich.clock.reduced_motion = true;
        states.push(rich);
        for state in &states {
            for width in [42, 80, 120] {
                let lines = state.lines(width, 24);
                assert!(!lines.is_empty());
                assert!(lines.len() <= 24);
                assert!(lines.iter().all(|line| line.chars().count() < 512));
            }
        }
    }

    #[test]
    fn usage_help_reaches_live_and_historical_modes() {
        assert!(HELP.contains("play/pause"));
        assert!(HELP.contains("Esc close"));
        assert_ne!("/usage", "/usage history");
    }

    #[test]
    fn seven_day_observatory_stays_bounded_and_responsive() {
        let facts = (0..50_000)
            .map(|index| {
                UsageFact::priced(
                    HarnessKind::ALL[index % 4],
                    format!("s{}", index % 100),
                    "gpt-5",
                    ProviderAttribution::Unknown,
                    Some(index as i64),
                    TimePrecision::Call,
                    Usage {
                        input_tokens: 1,
                        ..Default::default()
                    },
                    &PricingTable::builtin(),
                )
            })
            .collect();
        let mut view =
            UsageObservatory::new(facts, UsageRange::new(0, UsageRange::WEEK_MS).unwrap());
        view.clock.seek(UsageRange::WEEK_MS);
        assert!(view.lines(120, 40).len() <= 40);
    }
}
