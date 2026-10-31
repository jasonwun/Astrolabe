//! EndpointSlices (#58), shown only as a Service's related section (Freelens
//! 088d085, `network-services/service-details-endpoint-slices.tsx` and
//! `kube-object/src/specifics/endpoint-slice.ts`). The list projection is
//! that related table, which is why it has no Namespace or Age column; an
//! EndpointSlice list view would have to revisit it.

use astrolabe_kube::k8s_openapi::api::discovery::v1::EndpointSlice;
use astrolabe_kube::k8s_openapi::jiff::Timestamp;

use super::{Column, DetailModel, ResourceTable, detail, list_table};

/// Name / Type / Ports / Endpoints, as Freelens's Service details table
/// shows a Service's slices.
pub fn project_endpoint_slices(slices: &[&EndpointSlice], now: Timestamp) -> ResourceTable {
    list_table(ENDPOINT_SLICE_COLUMNS, slices, now)
}

const ENDPOINT_SLICE_COLUMNS: &[Column<EndpointSlice>] = &[
    Column::NAME,
    Column::new("Type", |slice, _| slice.address_type.clone()),
    Column::new("Ports", |slice, _| ports(slice)),
    Column::new("Endpoints", |slice, _| endpoints(slice)),
];

/// `getPortsString`: `port/protocol` per entry, protocol defaulting to
/// TCP. An entry without a port (all ports) shows the protocol alone where
/// upstream prints `undefined/TCP`.
fn ports(slice: &EndpointSlice) -> String {
    slice
        .ports
        .iter()
        .flatten()
        .map(|port| {
            let protocol = port.protocol.as_deref().unwrap_or("TCP");
            match port.port {
                Some(number) => format!("{number}/{protocol}"),
                None => protocol.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `getEndpointsString`: every endpoint's addresses, flattened in order.
fn endpoints(slice: &EndpointSlice) -> String {
    slice
        .endpoints
        .iter()
        .flatten()
        .flat_map(|endpoint| &endpoint.addresses)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A minimal detail for the related row's click: the table's values as
/// fields, not Freelens's full EndpointSlice details.
pub fn endpoint_slice_detail(slice: &EndpointSlice, now: Timestamp) -> DetailModel {
    let status = vec![
        ("Address Type".to_string(), slice.address_type.clone()),
        ("Ports".to_string(), ports(slice)),
        ("Endpoints".to_string(), endpoints(slice)),
    ];
    let value = serde_json::to_value(slice).unwrap_or_default();
    detail("EndpointSlice", slice, &value, status, now)
}
