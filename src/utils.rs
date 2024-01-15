// Copyright 2023 Stefan Sundin
// Licensed under GNU GPL v3 or later

use crate::types;

use aws_sdk_route53::types::{HostedZone, RrType};
use std::net::IpAddr;

pub fn get_hosted_zone(
  zones: Vec<&HostedZone>,
  hosted_zone_type: types::HostedZoneType,
) -> Option<&HostedZone> {
  let private_zone = hosted_zone_type == types::HostedZoneType::Private;
  if let Some(zone) = zones.clone().into_iter().find(|z| {
    z.config
      .as_ref()
      .is_some_and(|c| c.private_zone == private_zone)
  }) {
    return Some(zone);
  } else if hosted_zone_type == types::HostedZoneType::PreferPublic {
    return zones.first().copied();
  }
  None
}

pub fn detect_record_type(v: Vec<String>) -> RrType {
  let mut addrs = v.into_iter().map(|text| text.parse::<IpAddr>());
  if addrs.all(|addr| addr.is_ok()) {
    if addrs.all(|addr| addr.unwrap().is_ipv4()) {
      return RrType::A;
    } else if addrs.all(|addr| addr.unwrap().is_ipv6()) {
      return RrType::Aaaa;
    }
    // else {
    //   TODO: Support a mix of IPv4 and IPv6 and set both A and AAAA records
    // }
  }
  RrType::Txt
}

// The data that is retrieved so far exists in the same location in both the V3 and V4 endpoints.
// https://docs.aws.amazon.com/AmazonECS/latest/developerguide/task-metadata-endpoint.html
pub async fn get_ecs_task_metadata() -> Option<types::EcsTaskMetadata> {
  if let Ok(ecs_container_metadata_uri) =
    std::env::var("ECS_CONTAINER_METADATA_URI_V4").or(std::env::var("ECS_CONTAINER_METADATA_URI"))
  {
    let url = format!("{}/task", ecs_container_metadata_uri);
    let response = reqwest::get(url.as_str()).await.unwrap();
    if response.status() != reqwest::StatusCode::OK {
      panic!(
        "response from {} returned non-200 status code: {}",
        url,
        response.status()
      )
    }
    Some(response.json::<types::EcsTaskMetadata>().await.unwrap())
  } else {
    None
  }
}
