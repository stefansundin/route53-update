// Copyright 2023 Stefan Sundin
// Licensed under GNU GPL v3 or later

use aws_sdk_route53::types::{ChangeStatus, RrType};
use clap::Parser;
use std::str::FromStr;
use std::{thread, time};

#[derive(Parser)]
#[command(arg_required_else_help(true))]
struct Arguments {
  #[arg(long, help = "The Hosted Zone ID")]
  hosted_zone_id: String,

  #[arg(long, help = "DNS record name to update (e.g. service.example.com)")]
  dns_name: String,

  #[arg(long, help = "DNS record type", default_value = "A")]
  dns_type: String,

  #[arg(long, help = "DNS record value")]
  dns_value: String,

  #[arg(long, help = "Wait for the change to propagate in Route 53")]
  wait: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), std::io::Error> {
  let args = Arguments::parse();
  let dns_type = aws_sdk_route53::types::RrType::from_str(args.dns_type.as_str()).unwrap();
  if matches!(dns_type, RrType::Unknown(_)) {
    panic!("unknown DNS type: {}", args.dns_type);
  }

  let region_provider =
    aws_config::meta::region::RegionProviderChain::default_provider().or_else("us-east-1");
  let shared_config = aws_config::from_env().region(region_provider).load().await;
  let route53_config = aws_sdk_route53::config::Builder::from(&shared_config);
  let route53_client = aws_sdk_route53::client::Client::from_conf(route53_config.build());

  let rr = aws_sdk_route53::types::ResourceRecord::builder()
    .value(args.dns_value)
    .build();
  let rrs = aws_sdk_route53::types::ResourceRecordSet::builder()
    .ttl(300)
    .name(args.dns_name)
    .r#type(dns_type)
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
    .hosted_zone_id(args.hosted_zone_id)
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
