This is a tiny program that can update DNS records in Amazon Route 53.

The program is brand new so the command line arguments may change. Use with caution.

Please note that some features like `--clear` will delete records which can be disastrous if used incorrectly. Please do not experiment in a production environment!

## Installation

I will publish precompiled binaries once the program has become more stable. For the time being, you can compile from source by running the following command:

```
cargo install --git https://github.com/stefansundin/route53-update.git --branch main
```

## Docker

There's a beta docker image available on ECR: https://gallery.ecr.aws/stefansundin/route53-update

```
public.ecr.aws/stefansundin/route53-update:beta
```

For example usage with Amazon ECS, see [examples](examples).

## Usage

```
Usage: route53-update [OPTIONS] --record-name <NAME>

Options:
      --hosted-zone-id <HOSTED_ZONE_ID>
          The Hosted Zone ID (optional, will be looked up automatically based on --record-name if omitted)
      --hosted-zone-name <HOSTED_ZONE_NAME>
          Look up the Hosted Zone ID based on this name instead of using the record name (optional, conflicts with --hosted-zone-id)
      --hosted-zone-type <HOSTED_ZONE_TYPE>
          Filter the hosted zones based on the type (supported: 'prefer-public', 'public' or 'private') [default: prefer-public]
      --record-name <NAME>
          Record name to update (e.g. service.example.com)
      --record-type <TYPE>
          Record type (optional, is auto-detected from --value or --value-from-url when possible, TXT is used as fallback)
  -v, --value <VALUE>
          Record value (can be specified multiple times)
      --value-from <SOURCE>
          Get the value from a specific source (supported: 'auto', 'ec2-metadata', or 'ecs-metadata')
      --value-from-url <URL>
          Get the value from a URL (e.g. https://checkip.amazonaws.com/)
      --ip-address-type <TYPE>
          Use a specific IP address type (supported: 'public' or 'private') [default: public]
      --ttl <TTL>
          TTL for the DNS record (optional, if an existing record exists then its TTL will be copied, 300 is used as fallback)
      --wait
          Wait for the change to propagate in Route 53
      --clear
          Delete potentially conflicting records (A, AAAA, CNAME)
  -h, --help
          Print help
```
