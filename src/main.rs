// Copyright 2023 Stefan Sundin
// Licensed under GNU GPL v3 or later

use aws_sdk_route53::types::{ChangeStatus, RrType};
use clap::Parser;
use std::net::IpAddr;
use std::str::FromStr;
use std::{thread, time};

#[derive(Parser)]
#[command(arg_required_else_help(true))]
struct Arguments {
  #[arg(
    long,
    help = "The Hosted Zone ID (optional, will be looked up automatically based on --dns-name if omitted)"
  )]
  hosted_zone_id: Option<String>,

  #[arg(long, help = "DNS record name to update (e.g. service.example.com)")]
  dns_name: String,

  #[arg(
    long,
    help = "DNS record type (optional, is auto-detected from --dns-value or --value-from-url when possible, TXT is used as fallback)"
  )]
  dns_type: Option<String>,

  #[arg(long, help = "DNS record value")]
  dns_value: Option<String>,

  #[arg(
    long,
    help = "Get the value from a URL (e.g. https://checkip.amazonaws.com/)"
  )]
  value_from_url: Option<String>,

  #[arg(long, help = "Wait for the change to propagate in Route 53")]
  wait: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), std::io::Error> {
  let args = Arguments::parse();
  if args.dns_value.is_some() && args.value_from_url.is_some() {
    panic!("can't use both --dns-value and --value-from-url.");
  }

  let mut dns_type = None;
  if args.dns_type.is_some() {
    dns_type = Some(
      aws_sdk_route53::types::RrType::from_str(args.dns_type.clone().unwrap().as_str()).unwrap(),
    );
    if matches!(dns_type.clone().unwrap(), RrType::Unknown(_)) {
      panic!("unknown DNS type: {}", args.dns_type.unwrap());
    }
  }

  let mut dns_value;
  if args.dns_value.is_some() {
    dns_value = args.dns_value;
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
    dns_value = Some(response_text);
  } else {
    panic!("value must be supplied with --dns-value or --value-from-url.");
  }

  if dns_type.is_none() {
    dns_type = Some(detect_record_type(dns_value.clone().unwrap().as_str()));
  }

  // TXT records must be enclosed in quotes
  if matches!(dns_type.clone().unwrap(), RrType::Txt) {
    let v = dns_value.clone().unwrap();
    if !v.starts_with('"') && !v.ends_with('"') {
      dns_value = Some(format!("\"{}\"", v));
    }
  }

  let region_provider =
    aws_config::meta::region::RegionProviderChain::default_provider().or_else("us-east-1");
  let shared_config = aws_config::from_env().region(region_provider).load().await;
  let route53_config = aws_sdk_route53::config::Builder::from(&shared_config);
  let route53_client = aws_sdk_route53::client::Client::from_conf(route53_config.build());

  let rr = aws_sdk_route53::types::ResourceRecord::builder()
    .set_value(dns_value)
    .build();
  let rrs = aws_sdk_route53::types::ResourceRecordSet::builder()
    .ttl(300)
    .name(args.dns_name.clone())
    .set_type(dns_type)
    .resource_records(rr)
    .build();
  let change = aws_sdk_route53::types::Change::builder()
    .action(aws_sdk_route53::types::ChangeAction::Upsert)
    .resource_record_set(rrs)
    .build();
  let change_batch = aws_sdk_route53::types::ChangeBatch::builder()
    .changes(change)
    .build();

  let hosted_zone_id;
  if args.hosted_zone_id.is_some() {
    hosted_zone_id = args.hosted_zone_id.unwrap();
  } else {
    let response = route53_client
      .list_hosted_zones()
      .send()
      .await
      .expect("could not list hosted zones");
    if response.is_truncated() {
      panic!("you have a lot of hosted zones and this program does not paginate yet, please use --hosted-zone-id");
    }

    let mut search_name = args.dns_name.clone();
    if !search_name.ends_with(".") {
      search_name = search_name + ".";
    }

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
        hosted_zone_id = zones.first().unwrap().id().unwrap().to_string();
        break;
      } else {
        panic!("multiple zones with name: {}", search_name);
      }
    }
  }

  let response = route53_client
    .change_resource_record_sets()
    .hosted_zone_id(hosted_zone_id)
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
