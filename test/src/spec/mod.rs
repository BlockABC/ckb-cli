mod dao;
mod rpc;

pub use dao::*;
pub use rpc::*;

use crate::setup::Setup;
use ckb_app_config::CKBAppConfig;
use ckb_chain_spec::ChainSpec;

pub trait Spec {
    fn modify_ckb_toml(&self, _ckb_toml: &mut CKBAppConfig) {}

    fn modify_spec_toml(&self, _spec_toml: &mut ChainSpec) {}

    fn run(&self, setup: &mut Setup);
}
