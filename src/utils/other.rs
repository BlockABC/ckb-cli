use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use ckb_index::{with_index_db, IndexDatabase, VERSION};
use ckb_sdk::{
    rpc::AlertMessage,
    wallet::{KeyStore, ScryptType},
    Address, AddressPayload, CodeHashIndex, GenesisInfo, HttpRpcClient, NetworkType,
};
use ckb_types::{
    core::BlockView,
    packed::{CellOutput, OutPoint},
    prelude::*,
    H160, H256,
};
use clap::ArgMatches;
use colored::Colorize;
use rpassword::prompt_password_stdout;

use super::arg_parser::{AddressParser, ArgParser, FixedHashParser, PubkeyHexParser};

pub fn read_password(repeat: bool, prompt: Option<&str>) -> Result<String, String> {
    let prompt = prompt.unwrap_or("Password");
    let pass =
        prompt_password_stdout(format!("{}: ", prompt).as_str()).map_err(|err| err.to_string())?;
    if repeat {
        let repeat_pass =
            prompt_password_stdout("Repeat password: ").map_err(|err| err.to_string())?;
        if pass != repeat_pass {
            return Err("Passwords do not match".to_owned());
        }
    }
    Ok(pass)
}

pub fn get_key_store(ckb_cli_dir: &PathBuf) -> Result<KeyStore, String> {
    let mut keystore_dir = ckb_cli_dir.clone();
    keystore_dir.push("keystore");
    fs::create_dir_all(&keystore_dir)
        .map_err(|err| err.to_string())
        .and_then(|_| {
            KeyStore::from_dir(keystore_dir, ScryptType::default()).map_err(|err| err.to_string())
        })
}

pub fn get_address(network: Option<NetworkType>, m: &ArgMatches) -> Result<AddressPayload, String> {
    let address_opt: Option<Address> = AddressParser::default()
        .set_network_opt(network)
        .set_short(CodeHashIndex::Sighash)
        .from_matches_opt(m, "address", false)?;
    let pubkey: Option<secp256k1::PublicKey> =
        PubkeyHexParser.from_matches_opt(m, "pubkey", false)?;
    let lock_arg: Option<H160> =
        FixedHashParser::<H160>::default().from_matches_opt(m, "lock-arg", false)?;
    let address = address_opt
        .map(|address| address.payload().clone())
        .or_else(|| pubkey.map(|pubkey| AddressPayload::from_pubkey(&pubkey)))
        .or_else(|| lock_arg.map(AddressPayload::from_pubkey_hash))
        .ok_or_else(|| "Please give one argument".to_owned())?;
    Ok(address)
}

pub fn get_singer(
    key_store: KeyStore,
) -> impl Fn(&H160, &H256) -> Result<[u8; 65], String> + 'static {
    move |lock_arg: &H160, tx_hash_hash: &H256| {
        let prompt = format!("Password for [{:x}]", lock_arg);
        let password = read_password(false, Some(prompt.as_str()))?;
        let signature = key_store
            .sign_recoverable_with_password(lock_arg, None, tx_hash_hash, password.as_bytes())
            .map_err(|err| err.to_string())?;
        let (recov_id, data) = signature.serialize_compact();
        let mut signature_bytes = [0u8; 65];
        signature_bytes[0..64].copy_from_slice(&data[0..64]);
        signature_bytes[64] = recov_id.to_i32() as u8;
        Ok(signature_bytes)
    }
}

pub fn check_alerts(rpc_client: &mut HttpRpcClient) {
    if let Some(alerts) = rpc_client
        .get_blockchain_info()
        .ok()
        .map(|info| info.alerts)
    {
        for AlertMessage {
            id,
            priority,
            notice_until,
            message,
        } in alerts
        {
            if notice_until.0
                >= SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("Time went backwards")
                    .as_secs()
                    * 1000
            {
                eprintln!(
                    "[{}]: id={}, priority={}, message={}",
                    "alert".yellow().bold(),
                    id.to_string().blue().bold(),
                    priority.to_string().blue().bold(),
                    message.yellow().bold(),
                )
            }
        }
    }
}

pub fn get_genesis_info(
    genesis_info: &Option<GenesisInfo>,
    rpc_client: &mut HttpRpcClient,
) -> Result<GenesisInfo, String> {
    if let Some(genesis_info) = genesis_info {
        Ok(genesis_info.clone())
    } else {
        let genesis_block: BlockView = rpc_client
            .get_block_by_number(0)?
            .ok_or_else(|| String::from("Can not get genesis block"))?
            .into();
        GenesisInfo::from_block(&genesis_block)
    }
}

pub fn get_live_cell_with_cache(
    cache: &mut HashMap<(OutPoint, bool), CellOutput>,
    client: &mut HttpRpcClient,
    out_point: OutPoint,
    with_data: bool,
) -> Result<CellOutput, String> {
    if let Some(output) = cache.get(&(out_point.clone(), with_data)).cloned() {
        Ok(output)
    } else {
        let output = get_live_cell(client, out_point.clone(), with_data)?;
        cache.insert((out_point, with_data), output.clone());
        Ok(output)
    }
}

pub fn get_live_cell(
    client: &mut HttpRpcClient,
    out_point: OutPoint,
    with_data: bool,
) -> Result<CellOutput, String> {
    let cell = client.get_live_cell(out_point, with_data)?;
    if cell.status != "live" {
        return Err(format!("Invalid cell status: {}", cell.status));
    }
    let cell_status = cell.status.clone();
    cell.cell
        .map(|cell| cell.output.into())
        .ok_or_else(|| format!("Invalid input cell, status: {}", cell_status))
}

pub fn get_network_type(rpc_client: &mut HttpRpcClient) -> Result<NetworkType, String> {
    let chain_info = rpc_client.get_blockchain_info()?;
    NetworkType::from_raw_str(chain_info.chain.as_str())
        .ok_or_else(|| format!("Unexpected network type: {}", chain_info.chain))
}

pub fn index_dirname() -> String {
    format!("index-v{}", VERSION)
}

pub fn sync_to_tip(rpc_client: &mut HttpRpcClient, index_dir: &PathBuf) -> Result<(), String> {
    let genesis_block: BlockView = rpc_client
        .get_block_by_number(0)?
        .expect("Can not get genesis block?")
        .into();
    let genesis_hash: H256 = genesis_block.hash().unpack();
    let tip_number = rpc_client.get_tip_block_number()?;
    let network_type = get_network_type(rpc_client)?;
    let genesis_info = GenesisInfo::from_block(&genesis_block).unwrap();
    loop {
        let synced = with_index_db(index_dir.clone(), genesis_hash.clone(), |backend, cf| {
            IndexDatabase::from_db(backend, cf, network_type, genesis_info.clone(), false)
                .map(|db| db.last_number().unwrap_or(0))
                .or_else(|_| Ok(0))
        });
        if synced.unwrap_or(0) == tip_number {
            break;
        }
    }
    Ok(())
}
