//! The **schedule** adapter (`kind = "schedule" | "cron"`): a cron timer (or a one-shot `startup`) that
//! delivers an event under the channel's name on each tick.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use cron::Schedule;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use flux_lang::program::ChannelDecl;

use crate::config::ScheduleSettings;
use crate::{Channel, Deliverer};

pub struct ScheduleChannel {
    name: String,
    kind: Kind,
}

enum Kind {
    // Boxed: `Schedule` is large, and `Startup` is unit — keeps the enum small.
    Cron(Box<Schedule>),
    Startup,
}

impl ScheduleChannel {
    pub fn from_decl(decl: &ChannelDecl) -> anyhow::Result<Self> {
        let s: ScheduleSettings = serde_json::from_value(decl.settings.clone())
            .map_err(|e| anyhow::anyhow!("channel `{}` settings: {e}", decl.name))?;
        let kind = match (s.schedule, s.on) {
            (Some(expr), None) => Kind::Cron(Box::new(parse_cron(&expr)?)),
            (None, Some(on)) if on == "startup" => Kind::Startup,
            (None, Some(other)) => anyhow::bail!(
                "channel `{}`: unsupported `on = \"{other}\"` (only \"startup\")",
                decl.name
            ),
            (Some(_), Some(_)) => {
                anyhow::bail!(
                    "channel `{}`: set either `schedule` or `on`, not both",
                    decl.name
                )
            }
            (None, None) => {
                anyhow::bail!(
                    "channel `{}`: a schedule channel needs `schedule` or `on`",
                    decl.name
                )
            }
        };
        Ok(Self {
            name: decl.name.clone(),
            kind,
        })
    }
}

/// Accept a 5-field crontab (`"0 9 * * *"`) or the `cron` crate's native 6/7-field seconds-first form
/// (`"* * * * * *"`). The crate requires a seconds field, so a 5-field expression is normalized by
/// prepending `"0 "`.
fn parse_cron(expr: &str) -> anyhow::Result<Schedule> {
    let trimmed = expr.trim();
    let normalized = if trimmed.split_whitespace().count() == 5 {
        format!("0 {trimmed}")
    } else {
        trimmed.to_string()
    };
    Schedule::from_str(&normalized).map_err(|e| anyhow::anyhow!("invalid cron `{expr}`: {e}"))
}

#[async_trait]
impl Channel for ScheduleChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&self, d: Arc<dyn Deliverer>, cancel: CancellationToken) -> anyhow::Result<()> {
        match &self.kind {
            Kind::Startup => {
                fire(&self.name, &d).await;
                Ok(())
            }
            Kind::Cron(schedule) => loop {
                let now = Utc::now();
                let Some(next) = schedule.after(&now).next() else {
                    return Ok(());
                };
                let dur = (next - now).to_std().unwrap_or(std::time::Duration::ZERO);
                tokio::select! {
                    _ = cancel.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(dur) => fire(&self.name, &d).await,
                }
            },
        }
    }
}

/// Deliver one scheduled event under the channel name; a delivery error is logged, not fatal.
async fn fire(name: &str, d: &Arc<dyn Deliverer>) {
    let payload = json!({ "at": Utc::now().to_rfc3339(), "name": name });
    if let Err(e) = d.deliver(name, payload).await {
        eprintln!("channel `{name}`: delivery failed: {e}");
    }
}
