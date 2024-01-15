// Copyright 2023 Stefan Sundin
// Licensed under GNU GPL v3 or later

pub mod types;
pub mod utils;

use aws_sdk_route53::types::{ChangeStatus, RrType};
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
  record_type: Option<aws_sdk_route53::types::RrType>,

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
    help = "Get the value from a specific source (supported: 'auto')"
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
    help = "Use a specific IP address type (supported: 'public' or 'private')"
  )]
  ip_address_type: Option<types::IPAddressType>,

  #[arg(
    long,
    help = "TTL for the DNS record (optional, if an existing record exists then its TTL will be copied, 300 is used as fallback)"
  )]
  ttl: Option<i64>,

  #[arg(long, help = "Wait for the change to propagate in Route 53")]
  wait: bool,

  #[arg(long, help = "Delete potentially conflicting records (A, AAAA, CNAME)")]
  clear: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), std::io::Error> {
  env_logger::init();

  let mut args = Arguments::parse();
  if !args.value.is_empty() && args.value_from.is_some()
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
    if source == types::ValueFromSource::Auto {
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
          args.value = network.ipv4_addresses.clone().unwrap();
        } else if args.record_type == Some(RrType::Aaaa) && network.ipv6_addresses.is_some() {
          args.value = network.ipv6_addresses.clone().unwrap();
        }
      } else {
        // TODO: Try the ec2 metadata endpoint
        panic!("unable to detect environment (--value-from auto)")
      }
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

    let mut search_name = args.record_name.clone();
    loop {
      let zones: Vec<_> = response
        .hosted_zones()
        .into_iter()
        .filter(|zone| zone.name().eq(&search_name))
        .collect();
      if zones.len() == 0 {
        let search_split = search_name.split_once(".");
        if search_split.is_some() {
          search_name = search_split.unwrap().1.to_string();
        } else {
          panic!("could not find the hosted zone for: {}", args.record_name);
        }
      } else if zones.len() == 1 {
        let zone = zones.first().unwrap();
        args.hosted_zone_id = Some(zone.id().to_string());
        eprintln!("Found hosted zone: {} ({})", zone.id(), zone.name());
        break;
      } else {
        panic!("multiple zones with name: {}", search_name);
      }
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

      let mut change_batch_builder = aws_sdk_route53::types::ChangeBatch::builder();
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
        let change = aws_sdk_route53::types::Change::builder()
          .action(aws_sdk_route53::types::ChangeAction::Delete)
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

  let rrs = aws_sdk_route53::types::ResourceRecordSet::builder()
    .set_ttl(args.ttl)
    .name(args.record_name.clone())
    .set_type(args.record_type.clone())
    .set_resource_records(Some(
      args
        .value
        .into_iter()
        .map(|v| {
          aws_sdk_route53::types::ResourceRecord::builder()
            .value(v)
            .build()
            .expect("error building resource record")
        })
        .collect(),
    ))
    .build()
    .expect("error building resource record set");
  let change = aws_sdk_route53::types::Change::builder()
    .action(aws_sdk_route53::types::ChangeAction::Upsert)
    .resource_record_set(rrs)
    .build()
    .expect("error building change set");
  let change_batch = aws_sdk_route53::types::ChangeBatch::builder()
    .changes(change)
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
