// Copyright 2023 Stefan Sundin
// Licensed under GNU GPL v3 or later

use serde::Deserialize;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HostedZoneType {
  PreferPublic,
  Public,
  Private,
}
impl From<&str> for HostedZoneType {
  fn from(s: &str) -> Self {
    match s {
      "prefer-public" => HostedZoneType::PreferPublic,
      "public" => HostedZoneType::Public,
      "private" => HostedZoneType::Private,
      v => panic!("unsupported value: {}", v),
    }
  }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum IPAddressType {
  Public,
  Private,
}
impl From<&str> for IPAddressType {
  fn from(s: &str) -> Self {
    match s {
      "public" => IPAddressType::Public,
      "private" => IPAddressType::Private,
      v => panic!("unsupported value: {}", v),
    }
  }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ValueFromSource {
  Auto,
  Ec2Metadata,
  EcsMetadata,
}
impl From<&str> for ValueFromSource {
  fn from(s: &str) -> Self {
    match s {
      "auto" => ValueFromSource::Auto,
      "ec2-metadata" => ValueFromSource::Ec2Metadata,
      "ecs-metadata" => ValueFromSource::EcsMetadata,
      v => panic!("unsupported value: {}", v),
    }
  }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct EcsTaskMetadata {
  pub containers: Vec<EcsContainerMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct EcsContainerMetadata {
  pub networks: Vec<EcsContainerNetworkMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct EcsContainerNetworkMetadata {
  #[serde(rename = "IPv4Addresses")]
  pub ipv4_addresses: Option<Vec<String>>,
  #[serde(rename = "IPv6Addresses")]
  pub ipv6_addresses: Option<Vec<String>>,
}
