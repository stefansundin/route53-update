// Copyright 2023 Stefan Sundin
// Licensed under GNU GPL v3 or later

use aws_sdk_route53::types::{ChangeStatus, RrType};
use clap::Parser;
use std::net::IpAddr;
use std::{thread, time};

#[derive(Parser)]
#[command(arg_required_else_help(true))]
struct Arguments {
  #[arg(
    long,
    help = "The Hosted Zone ID (optional, will be looked up automatically based on --dns-name if omitted)"
  )]
  hosted_zone_id: Option<String>,

  #[arg(
    long,
    value_name = "NAME",
    help = "DNS record name to update (e.g. service.example.com)"
  )]
  dns_name: String,

  #[arg(
    long,
    value_enum,
    value_name = "TYPE",
    help = "DNS record type (optional, is auto-detected from --dns-value or --value-from-url when possible, TXT is used as fallback)"
  )]
  dns_type: Option<aws_sdk_route53::types::RrType>,

  #[arg(long, value_name = "VALUE", help = "DNS record value")]
  dns_value: Option<String>,

  #[arg(
    long,
    value_name = "URL",
    help = "Get the value from a URL (e.g. https://checkip.amazonaws.com/)"
  )]
  value_from_url: Option<String>,

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
  let mut args = Arguments::parse();
  if args.dns_value.is_some() && args.value_from_url.is_some() {
    panic!("can't use both --dns-value and --value-from-url.");
  } else if args.dns_value.is_none() && args.value_from_url.is_none() {
    panic!("value must be supplied with --dns-value or --value-from-url.");
  } else if args.dns_type.is_some() {
    if matches!(args.dns_type, Some(RrType::Unknown(_))) {
      panic!("unknown DNS type: {:?}", args.dns_type.unwrap());
    } else if args.dns_type == Some(RrType::Txt) && args.clear {
      panic!("--clear only works with A, AAAA, or CNAME");
    }
  }

  if !args.dns_name.ends_with(".") {
    args.dns_name = args.dns_name + ".";
  }

  if args.value_from_url.is_some() {
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
    args.dns_value = Some(response_text);
  }

  if args.dns_type.is_none() {
    args.dns_type = Some(detect_record_type(args.dns_value.clone().unwrap().as_str()));
    if args.dns_type == Some(RrType::Txt) && args.clear {
      panic!("--clear only works with A, AAAA, or CNAME");
    }
  }

  // TXT records must be enclosed in quotes
  if matches!(args.dns_type, Some(RrType::Txt)) {
    let v = args.dns_value.clone().unwrap();
    if !v.starts_with('"') && !v.ends_with('"') {
      args.dns_value = Some(format!("\"{}\"", v));
    }
  }

  let region_provider =
    aws_config::meta::region::RegionProviderChain::default_provider().or_else("us-east-1");
  let shared_config = aws_config::from_env().region(region_provider).load().await;
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

    let mut search_name = args.dns_name.clone();
    loop {
      let zones: Vec<_> = response
        .hosted_zones()
        .unwrap()
        .into_iter()
        .filter(|zone| zone.name().unwrap().eq(&search_name))
        .collect();
      if zones.len() == 0 {
        let search_split = search_name.split_once(".");
        if search_split.is_some() {
          search_name = search_split.unwrap().1.to_string();
        } else {
          panic!("could not find the hosted zone for: {}", args.dns_name);
        }
      } else if zones.len() == 1 {
        let zone = zones.first().unwrap();
        args.hosted_zone_id = Some(zone.id().unwrap().to_string());
        eprintln!(
          "Found hosted zone: {} ({})",
          zone.id().unwrap(),
          zone.name().unwrap()
        );
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
        .unwrap()
        .into_iter()
        .find(|r| r.name() == Some(&args.dns_name) && r.r#type() == args.dns_type.as_ref())
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
        .unwrap()
        .into_iter()
        .filter(|r| r.name() == Some(&args.dns_name))
        .filter(|r| {
          args.dns_type == Some(RrType::Cname)
            || (r.r#type() == Some(&RrType::A)
              || r.r#type() == Some(&RrType::Aaaa)
              || r.r#type() == Some(&RrType::Cname))
        })
        .filter(|r| r.r#type() != args.dns_type.clone().as_ref())
      {
        let change = aws_sdk_route53::types::Change::builder()
          .action(aws_sdk_route53::types::ChangeAction::Delete)
          .resource_record_set(r.clone())
          .build();
        change_batch_builder = change_batch_builder.changes(change);
        eprintln!(
          "Will delete {} {}",
          r.r#type().unwrap().as_str(),
          r.name().unwrap()
        )
      }

      let change_batch = change_batch_builder.build();
      if change_batch.changes().is_some() {
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

  let rr = aws_sdk_route53::types::ResourceRecord::builder()
    .set_value(args.dns_value)
    .build();
  let rrs = aws_sdk_route53::types::ResourceRecordSet::builder()
    .set_ttl(args.ttl)
    .name(args.dns_name.clone())
    .set_type(args.dns_type.clone())
    .resource_records(rr)
    .build();
  let change = aws_sdk_route53::types::Change::builder()
    .action(aws_sdk_route53::types::ChangeAction::Upsert)
    .resource_record_set(rrs)
    .build();
  let change_batch = aws_sdk_route53::types::ChangeBatch::builder()
    .changes(change)
    .build();

  let response = route53_client
    .change_resource_record_sets()
    .set_hosted_zone_id(args.hosted_zone_id)
    .change_batch(change_batch)
    .send()
    .await
    .expect("could not update DNS");

  println!("{:?}", response);

  if args.wait {
    let change_id = response.change_info().unwrap().id().unwrap();

    loop {
      thread::sleep(time::Duration::from_millis(1000));
      let response = route53_client
        .get_change()
        .id(change_id)
        .send()
        .await
        .expect("could not poll change status");
      eprintln!("{:?}", response);
      let change_status = response.change_info().unwrap().status().unwrap();
      if matches!(change_status, ChangeStatus::Insync) {
        break;
      }
    }
  }

  return Ok(());
}

fn detect_record_type(text: &str) -> RrType {
  let addr = text.parse::<IpAddr>();
  if addr.is_ok() {
    let unwrapped_addr = addr.unwrap();
    if unwrapped_addr.is_ipv4() {
      aws_sdk_route53::types::RrType::A
    } else if unwrapped_addr.is_ipv6() {
      aws_sdk_route53::types::RrType::Aaaa
    } else {
      panic!();
    }
  } else {
    aws_sdk_route53::types::RrType::Txt
  }
}
