//! `flux insights` — today's durable facts plus one bounded narration call.

use anyhow::{Context, Result};
use chrono::{Local, LocalResult, NaiveDate, NaiveTime, TimeZone};
use flux_flow::insights::{collect_facts, narrate, InsightScope};
use flux_secret::Redactor;
use tokio_util::sync::CancellationToken;

use crate::{open_event_store, resolve_cli_provider, resolve_model_spec};

fn today_bounds_ms() -> Result<(i64, i64, String)> {
    let today = Local::now().date_naive();
    let tomorrow = today
        .succ_opt()
        .ok_or_else(|| anyhow::anyhow!("local date has no following day"))?;
    let midnight = NaiveTime::from_hms_opt(0, 0, 0).expect("midnight is valid");
    let resolve = |date: NaiveDate| match Local.from_local_datetime(&date.and_time(midnight)) {
        LocalResult::Single(value) => Ok(value.timestamp_millis()),
        LocalResult::Ambiguous(earlier, _) => Ok(earlier.timestamp_millis()),
        LocalResult::None => Err(anyhow::anyhow!("local midnight does not exist for {date}")),
    };
    Ok((
        resolve(today)?,
        resolve(tomorrow)?,
        format!("today · {today}"),
    ))
}

pub(super) async fn run_insights(requested_model: Option<String>) -> Result<()> {
    let events = open_event_store()?;
    let (start_ms, end_ms, label) = today_bounds_ms()?;
    let redactor = Redactor::new();
    let pricing = flux_credentials::load_pricing_table();
    let facts = collect_facts(
        &events,
        &InsightScope::Interval {
            start_ms,
            end_ms,
            label,
        },
        &pricing,
        &redactor,
    )
    .context("derive today's insight facts")?;
    println!("{}", facts.render());
    if facts.is_empty() {
        return Ok(());
    }

    // Inspect first, construct the provider second: an empty day costs no credential lookup or
    // network-capable object, matching the command's zero-call contract.
    let cwd = std::env::current_dir().context("current dir")?;
    let config = flux_runtime::metadata::load_config(&cwd).context("load .flux/config.toml")?;
    let model_spec = resolve_model_spec(&requested_model, &config);
    let resolved = resolve_cli_provider(&model_spec, true)?;
    let provider = resolved.provider;
    let model = resolved.model;
    let canonical_spec = resolved.canonical_spec;
    let cancel = CancellationToken::new();
    let narration = narrate(provider.as_ref(), &model, &facts, None, &redactor, &cancel);
    tokio::pin!(narration);
    let (summary, usage) = tokio::select! {
        result = &mut narration => result,
        _ = tokio::signal::ctrl_c() => {
            cancel.cancel();
            narration.await
        }
    };
    if let Some(session) = facts.accounting_session.as_deref() {
        events
            .record_unscoped_call_usage(session, &canonical_spec, usage)
            .context("record insights model usage")?;
    }
    match summary {
        Ok(summary) => {
            println!("\nSummary\n{summary}");
            Ok(())
        }
        Err(error) => Err(anyhow::anyhow!(error)).context("generate insights summary"),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Timelike};

    use super::*;

    #[test]
    fn today_scope_is_the_half_open_local_calendar_day() {
        let (start_ms, end_ms, label) = today_bounds_ms().unwrap();
        let today = Local::now().date_naive();
        let start = Local.timestamp_millis_opt(start_ms).single().unwrap();
        let end = Local.timestamp_millis_opt(end_ms).single().unwrap();

        assert_eq!(start.date_naive(), today);
        assert_eq!(start.hour(), 0);
        assert_eq!(start.minute(), 0);
        assert_eq!(end.date_naive(), today.succ_opt().unwrap());
        assert_eq!(end.hour(), 0);
        assert_eq!(end.minute(), 0);
        assert!(label.contains(&today.to_string()));
    }
}
