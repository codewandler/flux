//! Small in-memory live datasource used by the SDK adoption example and its integration tests.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use flux_core::{Error, Result};
use flux_sdk::datasource::{
    FilterKey, FilterType, FilterValue, Filters, LiveAccess, LiveDatasource, LiveEntity,
    LiveSchema, Page, PageRequest, Reference, Row,
};
use flux_sdk::tools::ToolContext;

/// Hermetic support-system backend with tickets and customers.
pub struct SupportBackend {
    rows: BTreeMap<&'static str, Vec<SupportRow>>,
    entries: AtomicUsize,
}

impl SupportBackend {
    /// Build a deterministic fixture suitable for examples and tests.
    pub fn new() -> Self {
        let tickets = vec![
            SupportRow::new(
                Row {
                    id: "T-100".into(),
                    title: "Checkout retries".into(),
                    summary: "Payments retry after a gateway timeout".into(),
                    reference: Some(Reference::Entity {
                        entity: "customer".into(),
                        id: "C-10".into(),
                    }),
                },
                [
                    ("state", FilterValue::String("open".into())),
                    ("priority", FilterValue::Int(2)),
                    ("escalated", FilterValue::Bool(true)),
                ],
            ),
            SupportRow::new(
                Row {
                    id: "T-102".into(),
                    title: "Invoice export delayed".into(),
                    summary: "A scheduled export has not completed".into(),
                    reference: Some(Reference::Entity {
                        entity: "customer".into(),
                        id: "C-20".into(),
                    }),
                },
                [
                    ("state", FilterValue::String("open".into())),
                    ("priority", FilterValue::Int(2)),
                    ("escalated", FilterValue::Bool(true)),
                ],
            ),
            SupportRow::new(
                Row {
                    id: "T-200".into(),
                    title: "Resolved password reset".into(),
                    summary: "The customer confirmed access was restored".into(),
                    reference: None,
                },
                [
                    ("state", FilterValue::String("closed".into())),
                    ("priority", FilterValue::Int(1)),
                    ("escalated", FilterValue::Bool(false)),
                ],
            ),
        ];
        let customers = vec![
            SupportRow::new(
                Row {
                    id: "C-10".into(),
                    title: "Northwind GmbH".into(),
                    summary: "Enterprise customer in EMEA".into(),
                    reference: Some(Reference::Url {
                        url: "https://support.example/customers/C-10".into(),
                    }),
                },
                [("region", FilterValue::String("emea".into()))],
            ),
            SupportRow::new(
                Row {
                    id: "C-20".into(),
                    title: "Contoso Inc.".into(),
                    summary: "Business customer in North America".into(),
                    reference: Some(Reference::Url {
                        url: "https://support.example/customers/C-20".into(),
                    }),
                },
                [("region", FilterValue::String("north-america".into()))],
            ),
        ];

        Self {
            rows: BTreeMap::from([("ticket", tickets), ("customer", customers)]),
            entries: AtomicUsize::new(0),
        }
    }

    /// Number of list/get calls that reached the backend implementation.
    pub fn entries(&self) -> usize {
        self.entries.load(Ordering::Relaxed)
    }

    fn rows_for(&self, entity: &str) -> Result<&[SupportRow]> {
        self.rows
            .get(entity)
            .map(Vec::as_slice)
            .ok_or_else(|| Error::Other(format!("support backend does not know entity `{entity}`")))
    }
}

impl Default for SupportBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LiveDatasource for SupportBackend {
    fn schema(&self) -> LiveSchema {
        LiveSchema {
            entities: vec![
                LiveEntity {
                    entity: "ticket".into(),
                    filters: vec![
                        FilterKey {
                            name: "state".into(),
                            ty: FilterType::Enum(vec!["open".into(), "closed".into()]),
                            required: false,
                            description: Some("Ticket workflow state".into()),
                        },
                        FilterKey {
                            name: "priority".into(),
                            ty: FilterType::Int,
                            required: false,
                            description: Some("Numeric support priority".into()),
                        },
                        FilterKey {
                            name: "escalated".into(),
                            ty: FilterType::Bool,
                            required: false,
                            description: Some("Whether escalation is active".into()),
                        },
                    ],
                    default_page: 2,
                    max_page: 10,
                    description: Some("Support tickets".into()),
                },
                LiveEntity {
                    entity: "customer".into(),
                    filters: vec![FilterKey {
                        name: "region".into(),
                        ty: FilterType::String,
                        required: false,
                        description: Some("Customer region slug".into()),
                    }],
                    default_page: 2,
                    max_page: 10,
                    description: Some("Support customers".into()),
                },
            ],
        }
    }

    fn access(&self) -> Vec<LiveAccess> {
        Vec::new()
    }

    async fn list(
        &self,
        _ctx: &ToolContext,
        entity: &str,
        page: PageRequest,
        filters: &Filters,
    ) -> Result<Page<Row>> {
        self.entries.fetch_add(1, Ordering::Relaxed);
        let rows = self.rows_for(entity)?;
        let offset = decode_cursor(entity, page.cursor.as_deref())?;
        let matching = rows
            .iter()
            .filter(|row| {
                filters
                    .iter()
                    .all(|(name, value)| row.filters.get(name) == Some(value))
            })
            .collect::<Vec<_>>();
        let start = offset.min(matching.len());
        let end = start.saturating_add(page.limit).min(matching.len());
        let rows = matching[start..end]
            .iter()
            .map(|row| row.row.clone())
            .collect();
        let next = (end < matching.len()).then(|| format!("v1:{entity}:{end}"));
        Ok(Page { rows, next })
    }

    async fn get(&self, _ctx: &ToolContext, entity: &str, id: &str) -> Result<Option<Row>> {
        self.entries.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .rows_for(entity)?
            .iter()
            .find(|row| row.row.id == id)
            .map(|row| row.row.clone()))
    }
}

struct SupportRow {
    row: Row,
    filters: Filters,
}

impl SupportRow {
    fn new<const N: usize>(row: Row, filters: [(&str, FilterValue); N]) -> Self {
        Self {
            row,
            filters: filters.into_iter().collect(),
        }
    }
}

fn decode_cursor(entity: &str, cursor: Option<&str>) -> Result<usize> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let prefix = format!("v1:{entity}:");
    cursor
        .strip_prefix(&prefix)
        .and_then(|offset| offset.parse::<usize>().ok())
        .ok_or_else(|| {
            Error::Other(format!(
                "support backend rejected cursor `{cursor}` for entity `{entity}`"
            ))
        })
}
