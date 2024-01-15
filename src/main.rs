// Copyright 2023 Stefan Sundin
// Licensed under GNU GPL v3 or later

pub mod types;
pub mod utils;

use aws_sdk_route53::types::{
  Change, ChangeAction, ChangeBatch, ChangeStatus, ResourceRecord, ResourceRecordSet, RrType,
};
use clap::Parser;
use std::{thread, time};

#[derive(Parser)]
#[command(arg_required_else_help(true))]
struct Arguments {
  #[arg(
    long,
    help = "The Hosted Zone ID (optional, will be looked up automatically based on --record-name if omitted)"
  )]
  hosted_zone_id: Option<String>,

  #[arg(
    long,
    help = "Look up the Hosted Zone ID based on this name instead of using the record name (optional, conflicts with --hosted-zone-id)"
  )]
  hosted_zone_name: Option<String>,

  #[arg(
    long,
    help = "Filter the hosted zones based on the type (supported: 'prefer-public', 'public' or 'private')",
    default_value = "prefer-public"
  )]
  hosted_zone_type: types::HostedZoneType,

  #[arg(
    long,
    value_name = "NAME",
    help = "Record name to update (e.g. service.example.com)"
  )]
  record_name: String,

  #[arg(
    long,
    value_enum,
    value_name = "TYPE",
    help = "Record type (optional, is auto-detected from --value or --value-from-url when possible, TXT is used as fallback)"
  )]
  record_type: Option<RrType>,

  #[arg(
    short,
    long,
    value_name = "VALUE",
    help = "Record value (can be specified multiple times)"
  )]
  value: Vec<String>,

  #[arg(
    long,
    value_enum,
    value_name = "SOURCE",
    help = "Get the value from a specific source (supported: 'auto', 'ec2-metadata', or 'ecs-metadata')"
  )]
  value_from: Option<types::ValueFromSource>,

  #[arg(
    long,
    value_name = "URL",
    help = "Get the value from a URL (e.g. https://checkip.amazonaws.com/)"
  )]
  value_from_url: Option<String>,

  #[arg(
    long,
    value_enum,
    value_name = "TYPE",
    help = "Use a specific IP address type (supported: 'public' or 'private')",
    default_value = "public"
  )]
  ip_address_type: types::IPAddressType,

  #[arg(
    long,
    help = "TTL for the DNS record (optional, if an existing record exists then its TTL will be copied, 300 is used as fallback)"
  )]
  ttl: Option<i64>,

  #[arg(long, help = "Change batch comment")]
  comment: Option<String>,

  #[arg(long, help = "Wait for the change to propagate in Route 53")]
  wait: bool,

  #[arg(long, help = "Delete potentially conflicting records (A, AAAA, CNAME)")]
  clear: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), std::io::Error> {
  env_logger::init();

  let mut args = Arguments::parse();
  if args.hosted_zone_id.is_some() && args.hosted_zone_name.is_some() {
    panic!("can only use one of --hosted-zone-id or --hosted-zone-name.");
  } else if !args.value.is_empty() && args.value_from.is_some()
    || !args.value.is_empty() && args.value_from_url.is_some()
    || args.value_from.is_some() && args.value_from_url.is_some()
  {
    panic!("can only use one of --value, --value-from, or --value-from-url.");
  } else if args.value.is_empty() && args.value_from.is_none() && args.value_from_url.is_none() {
    panic!("value must be supplied with either --value, --value-from, or --value-from-url.");
  } else if args.record_type.is_some() && args.record_type == Some(RrType::Txt) && args.clear {
    panic!("--clear only works with A, AAAA, or CNAME");
  }

  if !args.record_name.ends_with(".") {
    args.record_name = args.record_name + ".";
  }

  if args.value_from.is_some() {
    let source = args.value_from.unwrap();

    // --value-from ecs-metadata
    if source == types::ValueFromSource::EcsMetadata || source == types::ValueFromSource::Auto {
      if let Some(ecs_task_metadata) = utils::get_ecs_task_metadata().await {
        eprintln!("ecs_task_metadata: {:?}", ecs_task_metadata);
        // This naively grabs the IP for first container in the task, this should perhaps be configurable.
        // If you use awsvpc networking mode then all the containers will have the same IP.
        let network = ecs_task_metadata
          .containers
          .first()
          .unwrap()
          .networks
          .first()
          .unwrap();
        if args.record_type == Some(RrType::A) && network.ipv4_addresses.is_some() {
          args.value = network
            .ipv4_addresses
            .clone()
            .unwrap()
            .into_iter()
            .filter(|address| !address.is_empty()) // The ECS metadata service can annoyingly return "IPv4Addresses": [""]
            .collect();
        } else if args.record_type == Some(RrType::Aaaa) && network.ipv6_addresses.is_some() {
          args.value = network.ipv6_addresses.clone().unwrap();
        }
      }
    }

    // --value-from ec2-metadata
    if source == types::ValueFromSource::Ec2Metadata
      || (source == types::ValueFromSource::Auto && args.value.is_empty())
    {
      let path = match (args.record_type.clone(), args.ip_address_type) {
        (Some(RrType::A) | None, types::IPAddressType::Public) => "public-ipv4",
        (Some(RrType::A) | None, types::IPAddressType::Private) => "local-ipv4",
        (Some(RrType::Aaaa), _) => "ipv6",
        _ => panic!("--value-from is only usable with --record-type A or AAAA"),
      };
      let imds_client = aws_config::imds::client::Client::builder().build();
      if let Ok(value) = imds_client
        .get(format!("/latest/meta-data/{}", path).as_str())
        .await
      {
        args.value.push(value.as_ref().to_string());
      }
    }

    if source == types::ValueFromSource::Auto && args.value.is_empty() {
      panic!("unable to auto-detect an IP address to use (missing ECS environment variables and unable to connect to the EC2 instance metadata service)");
    }
  } else if args.value_from_url.is_some() {
    let url = args.value_from_url.unwrap();
    let response = reqwest::get(url.as_str()).await.unwrap();
    if response.status() != reqwest::StatusCode::OK {
      panic!(
        "response from {} returned non-200 status code: {}",
        url,
        response.status()
      )
    }
    let response_text = response.text().await.unwrap().trim().to_string();
    eprintln!("{} returned {:?}", url, response_text);
    args.value = vec![response_text];
  }

  // Sanity check
  if args.value.is_empty() {
    panic!("somehow value is {:?}", args.value);
  }

  if args.record_type.is_none() {
    args.record_type = Some(utils::detect_record_type(args.value.clone()));
    if args.record_type == Some(RrType::Txt) && args.clear {
      panic!("--clear only works with A, AAAA, or CNAME");
    }
  }

  // TXT records must be enclosed in quotes
  if matches!(args.record_type, Some(RrType::Txt)) {
    args.value = args
      .value
      .into_iter()
      .map(|v: String| {
        if v.starts_with('"') && v.ends_with('"') {
          v
        } else {
          format!("\"{}\"", v)
        }
      })
      .collect();
  }

  let region_provider =
    aws_config::meta::region::RegionProviderChain::default_provider().or_else("us-east-1");
  let shared_config = aws_config::defaults(aws_config::BehaviorVersion::v2023_11_09())
    .region(region_provider)
    .load()
    .await;
  let route53_config = aws_sdk_route53::config::Builder::from(&shared_config);
  let route53_client = aws_sdk_route53::client::Client::from_conf(route53_config.build());

  if args.hosted_zone_id.is_none() {
    let response = route53_client
      .list_hosted_zones()
      .send()
      .await
      .expect("could not list hosted zones");
    if response.is_truncated() {
      panic!("you have a lot of hosted zones and this program does not paginate yet, please use --hosted-zone-id");
    }

    let hosted_zone;
    if let Some(mut hosted_zone_name) = args.hosted_zone_name {
      if !hosted_zone_name.ends_with(".") {
        hosted_zone_name = hosted_zone_name + ".";
      }
      hosted_zone = utils::get_hosted_zone(
        response
          .hosted_zones()
          .into_iter()
          .filter(|zone| zone.name() == hosted_zone_name)
          .collect(),
        args.hosted_zone_type,
      );
      if hosted_zone.is_none() {
        panic!(
          "could not find a hosted zone with name: {}",
          hosted_zone_name
        );
      }
    } else {
      let mut search_name = args.record_name.clone();
      let mut hosted_zone_type = if args.hosted_zone_type == types::HostedZoneType::Public
        || args.hosted_zone_type == types::HostedZoneType::PreferPublic
      {
        types::HostedZoneType::Public
      } else {
        types::HostedZoneType::Private
      };
      loop {
        let zone = utils::get_hosted_zone(
          response
            .hosted_zones()
            .into_iter()
            .filter(|zone| zone.name().eq(&search_name))
            .collect(),
          hosted_zone_type,
        );
        if zone.is_some() {
          hosted_zone = zone;
          break;
        } else {
          let search_split = search_name.split_once(".");
          if search_split.is_some() {
            search_name = search_split.unwrap().1.to_string();
          } else if args.hosted_zone_type == types::HostedZoneType::PreferPublic
            && hosted_zone_type == types::HostedZoneType::Public
          {
            hosted_zone_type = types::HostedZoneType::Private;
          } else {
            panic!("could not find the hosted zone for: {}", args.record_name);
          }
        }
      }
    }

    if let Some(zone) = hosted_zone {
      args.hosted_zone_id = Some(zone.id.to_string());
      eprintln!("Found hosted zone: {} ({})", zone.id(), zone.name());
    } else {
      panic!("could not find the hosted zone");
    }
  }

  let hosted_zone_id = args.hosted_zone_id.clone().unwrap();
  if args.ttl.is_none() || args.clear {
    let response = route53_client
      .list_resource_record_sets()
      .hosted_zone_id(hosted_zone_id.clone())
      .send()
      .await
      .expect("could not list record sets");

    if response.is_truncated() {
      eprintln!("This zone has a lot of record sets and this program does not paginate yet, so --clear might clear everything.");
    }

    if args.ttl.is_none() {
      args.ttl = response
        .resource_record_sets()
        .into_iter()
        .find(|r| r.name() == &args.record_name && Some(r.r#type()) == args.record_type.as_ref())
        .map(|r| r.ttl().unwrap());
      if args.ttl.is_some() {
        eprintln!("Copied TTL from existing record: {}", args.ttl.unwrap())
      } else {
        args.ttl = Some(300);
        eprintln!("Using default TTL: {}", args.ttl.unwrap())
      }
    }

    if args.clear {
      // To avoid errors of the following kind, we have to delete records before we UPSERT:
      // RRSet of type CNAME with DNS name service.example.com. is not permitted as it conflicts with other records with the same DNS name in zone example.com.

      let mut change_batch_builder = ChangeBatch::builder();
      for r in response
        .resource_record_sets()
        .into_iter()
        .filter(|r| r.name() == &args.record_name)
        .filter(|r| {
          args.record_type == Some(RrType::Cname)
            || (r.r#type() == &RrType::A
              || r.r#type() == &RrType::Aaaa
              || r.r#type() == &RrType::Cname)
        })
        .filter(|r| Some(r.r#type()) != args.record_type.as_ref())
      {
        let change = Change::builder()
          .action(ChangeAction::Delete)
          .resource_record_set(r.clone())
          .build()
          .expect("error building change set");
        change_batch_builder = change_batch_builder.changes(change);
        eprintln!("Will delete {} {}", r.r#type().as_str(), r.name())
      }

      let change_batch = change_batch_builder
        .build()
        .expect("error building change batch");
      if !change_batch.changes().is_empty() {
        route53_client
          .change_resource_record_sets()
          .hosted_zone_id(hosted_zone_id.clone())
          .change_batch(change_batch)
          .send()
          .await
          .expect("could not delete DNS records");
      }
    }
  }

  let rrs = ResourceRecordSet::builder()
    .set_ttl(args.ttl)
    .name(args.record_name.clone())
    .set_type(args.record_type.clone())
    .set_resource_records(Some(
      args
        .value
        .into_iter()
        .map(|v| {
          ResourceRecord::builder()
            .value(v)
            .build()
            .expect("error building resource record")
        })
        .collect(),
    ))
    .build()
    .expect("error building resource record set");
  let change = Change::builder()
    .action(ChangeAction::Upsert)
    .resource_record_set(rrs)
    .build()
    .expect("error building change set");
  let change_batch = ChangeBatch::builder()
    .changes(change)
    .set_comment(args.comment)
    .build()
    .expect("error building change batch");

  eprintln!("{:?}", change_batch);

  let response = route53_client
    .change_resource_record_sets()
    .set_hosted_zone_id(args.hosted_zone_id)
    .change_batch(change_batch)
    .send()
    .await
    .expect("could not update DNS");

  println!("{:?}", response);

  if args.wait {
    let change_id = response.change_info().unwrap().id();

    loop {
      thread::sleep(time::Duration::from_millis(1000));
      let response = route53_client
        .get_change()
        .id(change_id)
        .send()
        .await
        .expect("could not poll change status");
      eprintln!("{:?}", response);
      let change_status = response.change_info().unwrap().status();
      if matches!(change_status, ChangeStatus::Insync) {
        break;
      }
    }
  }

  return Ok(());
}
