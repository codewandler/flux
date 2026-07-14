use flux_datasource::live::{
    FilterKey, FilterType, FilterValue, Filters, LiveEntity, LiveSchema, Page, PageRequest,
    Reference, Row,
};
use serde_json::json;

#[test]
fn live_contracts_round_trip_without_capability_handles() {
    let schema = LiveSchema {
        entities: vec![LiveEntity {
            entity: "ticket".into(),
            filters: vec![
                FilterKey {
                    name: "state".into(),
                    ty: FilterType::Enum(vec!["open".into(), "closed".into()]),
                    required: true,
                    description: Some("Ticket state".into()),
                },
                FilterKey {
                    name: "active".into(),
                    ty: FilterType::Bool,
                    required: false,
                    description: None,
                },
            ],
            default_page: 25,
            max_page: 100,
            description: Some("Support tickets".into()),
        }],
    };
    let encoded = serde_json::to_value(&schema).unwrap();
    assert_eq!(
        serde_json::from_value::<LiveSchema>(encoded).unwrap(),
        schema
    );

    let page = Page {
        rows: vec![Row {
            id: "T-42".into(),
            title: "Broken login".into(),
            summary: "Customer cannot sign in".into(),
            reference: Some(Reference::Entity {
                entity: "ticket".into(),
                id: "T-42".into(),
            }),
        }],
        next: Some("cursor/雪?next=1".into()),
    };
    let encoded = serde_json::to_value(&page).unwrap();
    assert_eq!(serde_json::from_value::<Page<Row>>(encoded).unwrap(), page);
    assert_eq!(
        serde_json::to_value(Reference::Url {
            url: "https://tickets.example/T-42".into(),
        })
        .unwrap(),
        json!({"kind": "url", "url": "https://tickets.example/T-42"})
    );
}

#[test]
fn filters_are_scalar_and_serialize_in_deterministic_key_order() {
    let filters = Filters::from_iter([
        ("state", FilterValue::String("open".into())),
        ("active", FilterValue::Bool(true)),
        ("priority", FilterValue::Int(3)),
    ]);
    assert_eq!(
        serde_json::to_string(&filters).unwrap(),
        r#"{"active":true,"priority":3,"state":"open"}"#
    );
    assert_eq!(filters.get("priority"), Some(&FilterValue::Int(3)));

    let request = PageRequest {
        cursor: Some("opaque:1".into()),
        limit: 50,
    };
    assert_eq!(
        serde_json::from_value::<PageRequest>(serde_json::to_value(&request).unwrap()).unwrap(),
        request
    );
}
