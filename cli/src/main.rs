use chrono::Utc;
use clap::ArgMatches;
use crypto::sha256;
use keys::{Address, KeyPair};
use prost::Message;
use proto::chain::transaction::{Contract, Raw as TransactionRaw};
use proto::contract as contract_pb;

mod account;
mod proposal;
mod system;
mod util;
mod witness;

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(crate) fn custom_error(msg: &str) -> Box<dyn std::error::Error> {
    Box::new(std::io::Error::new(std::io::ErrorKind::Other, msg))
}

fn main() -> Result<()> {
    let yaml = clap::load_yaml!("cli.yml");
    let matches = clap::App::from_yaml(yaml).get_matches();

    let cntr = match matches.subcommand() {
        ("transfer", Some(arg_matches)) => transfer(arg_matches),
        ("account", Some(arg_matches)) => account::main(arg_matches),
        ("witness", Some(arg_matches)) => witness::main(arg_matches),
        ("system", Some(arg_matches)) => system::main(arg_matches),
        ("proposal", Some(arg_matches)) => proposal::main(arg_matches).ok(),
        // commands::transfer::main(arg_matches),
        // ("list", Some(arg_matches)) => commands::list::main(arg_matches),
        _ => unimplemented!(),
    };

    pack_and_send(cntr.expect("illegal contract"), &matches)
}

fn transfer(matches: &ArgMatches) -> Option<Contract> {
    let from: Address = matches.value_of("SENDER")?.parse().ok()?;
    let to: Address = matches.value_of("RECIPIENT")?.parse().ok()?;
    let amount = util::parse_amount_with_currency(matches.value_of("AMOUNT")?, "TRX", 6)?;

    let transfer = contract_pb::TransferContract {
        owner_address: from.as_bytes().to_vec(),
        to_address: to.as_bytes().to_vec(),
        amount: amount,
    };

    Some(transfer.into())
}

fn pack_and_send(cntr: Contract, matches: &ArgMatches) -> Result<()> {
    let ref_block_hash = get_ref_block_hash(matches)?;

    let raw = TransactionRaw {
        contract: Some(cntr),
        ref_block_bytes: ref_block_hash[6..8].to_vec(),
        ref_block_hash: ref_block_hash[8..16].to_vec(),
        expiration: Utc::now().timestamp_millis() + 60_000,
        ..Default::default()
    };
    let mut buf = Vec::with_capacity(255);
    raw.encode(&mut buf)?;

    let hex_priv_key = matches.value_of("private-key").unwrap();
    let kp = KeyPair::from_private(hex_priv_key.parse()?)?;

    let sig = kp.private().sign(&buf)?;

    println!("RAW => {}", hex::encode(&buf));
    println!("TXN Hash => {:?}", sha256(&buf));
    println!("SIG => {}", hex::encode(sig.as_bytes()));

    if !matches.is_present("dont-broadcast") {
        send_raw_transaction(&hex::encode(&buf), &hex::encode(sig.as_bytes()), matches)?;
    }

    Ok(())
}

fn send_raw_transaction(raw: &str, signature: &str, matches: &ArgMatches) -> Result<()> {
    let rpc_url = matches.value_of("rpc-url").expect("has default; qed");

    let client = reqwest::blocking::Client::new();
    let mutation = r#"{ "query": "mutation { txn: sendRawTransaction(rawData: \"RAW\", signatures: [\"SIG\"]) }" }"#;

    let mutation = mutation.replace("RAW", raw).replace("SIG", signature);
    let resp = client
        .post(rpc_url)
        .header("User-Agent", "Opentron Cli/0.1.0")
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .body(mutation)
        .send()?;

    let payload: serde_json::Value = resp.json()?;

    println!("{}", serde_json::to_string_pretty(&payload)?);
    let hash = &payload["data"]["txn"];
    println!("=> {}", hash);
    Ok(())
}

fn get_ref_block_hash(matches: &ArgMatches) -> Result<Vec<u8>> {
    let rpc_url = matches.value_of("rpc-url").expect("has default; qed");

    let client = reqwest::blocking::Client::new();
    // "operationName":null,
    // "variables":{},
    let query = r#"{
        "query":"{ refBlock: block { hash } }"
    }"#;
    let resp = client
        .post(rpc_url)
        .header("User-Agent", "Opentron Cli/0.1.0")
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .body(query)
        .send()?;

    let payload: serde_json::Value = resp.json()?;

    println!("{}", serde_json::to_string_pretty(&payload)?);
    let hash = &payload["data"]["refBlock"]["hash"];
    hex::decode(hash.as_str().unwrap()).map_err(From::from)
}
