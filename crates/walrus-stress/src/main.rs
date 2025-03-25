// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Load generators for stress testing the Walrus nodes.

use std::{
    collections::{BTreeMap, HashMap},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::Context;
use clap::{Parser, Subcommand};
use rand::{seq::SliceRandom, RngCore};
use sui_sdk::wallet_context::WalletContext;
use sui_types::base_types::ObjectID;
use walrus_service::{
    client::{metrics::ClientMetrics, Config, Refiller},
    utils::load_from_yaml,
};
use walrus_sui::{
    client::{CoinType, ReadClient, SuiContractClient, MIN_STAKING_THRESHOLD},
    config::load_wallet_context_from_path,
    types::StorageNode,
    utils::SuiNetwork,
};

use crate::generator::LoadGenerator;

mod generator;

/// The amount of gas or MIST to refil each time.
const COIN_REFILL_AMOUNT: u64 = 500_000_000;
/// The minimum balance to keep in the wallet.
const MIN_BALANCE: u64 = 1_000_000_000;

#[derive(Parser)]
#[clap(rename_all = "kebab-case")]
#[clap(name = env!("CARGO_BIN_NAME"))]
#[derive(Debug)]
struct Args {
    /// The path to the client configuration file containing the system object address.
    #[clap(long, default_value = "./working_dir/client_config.yaml")]
    config_path: PathBuf,

    /// Sui network for which the config is generated.
    #[clap(long, default_value = "testnet")]
    sui_network: SuiNetwork,

    /// The port on which metrics are exposed.
    #[clap(long, default_value = "9584")]
    metrics_port: u16,

    /// The path to the Sui Wallet to be used for funding the gas.
    ///
    /// If specified, the funds to run the stress client will be taken from this wallet. Otherwise,
    /// the stress client will try to use the faucet.
    #[clap(long)]
    wallet_path: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug, Clone)]
#[clap(rename_all = "kebab-case")]
enum Commands {
    /// Register nodes based on parameters exported by the `walrus-node setup` command, send the
    /// storage-node capability to the respective node's wallet, and optionally stake with them.
    Stress(StressArgs),
    /// Deploy the Walrus system contract on the Sui network.
    Staking(StakingArgs),
}

#[derive(Parser, Debug, Clone)]
#[clap(rename_all = "kebab-case")]
#[command(author, version, about = "Walrus stress load generator", long_about = None)]
struct StressArgs {
    /// The target write load to submit to the system (writes/minute).
    /// The actual load may be limited by the number of clients.
    /// If the write load is 0, a single write will be performed to enable reads.
    #[clap(long, default_value_t = 60)]
    write_load: u64,
    /// The target read load to submit to the system (reads/minute).
    /// The actual load may be limited by the number of clients.
    #[clap(long, default_value_t = 60)]
    read_load: u64,
    /// The number of clients to use for the load generation for reads and writes.
    #[clap(long, default_value = "10")]
    n_clients: NonZeroUsize,
    /// The binary logarithm of the minimum blob size to use for the load generation.
    ///
    /// Blobs sizes are uniformly distributed across the powers of two between
    /// this and the maximum blob size.
    #[clap(long, default_value = "10")]
    min_size_log2: u8,
    /// The binary logarithm of the maximum blob size to use for the load generation.
    #[clap(long, default_value = "20")]
    max_size_log2: u8,
    /// The period in milliseconds to check if gas needs to be refilled.
    ///
    /// This is useful for continuous load testing where the gas budget need to be refilled
    /// periodically.
    #[clap(long, default_value = "1000")]
    gas_refill_period_millis: NonZeroU64,
    /// The fraction of writes that write inconsistent blobs.
    #[clap(long, default_value_t = 0.0)]
    inconsistent_blob_rate: f64,
}

#[derive(Parser, Debug, Clone)]
#[clap(rename_all = "kebab-case")]
#[command(author, version, about = "Walrus staking load generator", long_about = None)]
struct StakingArgs {
    /// The period in seconds to check if restaking is needed.
    #[clap(long, default_value = "10")]
    restaking_period_seconds: NonZeroU64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let _ = tracing_subscriber::fmt::try_init();
    let config: Config =
        load_from_yaml(args.config_path).context("Failed to load client config")?;

    // Start the metrics server.
    let metrics_address = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), args.metrics_port);
    let registry_service = mysten_metrics::start_prometheus_server(metrics_address);
    let prometheus_registry = registry_service.default_registry();
    let metrics = Arc::new(ClientMetrics::new(&prometheus_registry));
    tracing::info!("starting metrics server on {metrics_address}");

    let wallet = load_wallet_context_from_path(args.wallet_path)?;
    match args.command {
        Commands::Stress(stress_args) => {
            run_stress(config, metrics, wallet, args.sui_network, stress_args).await
        }
        Commands::Staking(staking_args) => run_staking(config, metrics, wallet, staking_args).await,
    }
}

async fn run_stress(
    config: Config,
    metrics: Arc<ClientMetrics>,
    wallet: WalletContext,
    sui_network: SuiNetwork,
    args: StressArgs,
) -> anyhow::Result<()> {
    let n_clients = args.n_clients.get();

    // Start the write transaction generator.
    let gas_refill_period = Duration::from_millis(args.gas_refill_period_millis.get());

    let contract_client = config.new_contract_client(wallet, None).await?;

    let refiller = Refiller::new(
        contract_client,
        COIN_REFILL_AMOUNT,
        COIN_REFILL_AMOUNT,
        MIN_BALANCE,
    );
    let mut load_generator = LoadGenerator::new(
        n_clients,
        args.min_size_log2,
        args.max_size_log2,
        config,
        sui_network,
        gas_refill_period,
        metrics,
        refiller,
    )
    .await?;

    load_generator
        .start(args.write_load, args.read_load, args.inconsistent_blob_rate)
        .await?;
    Ok(())
}

enum StakingModeAtEpoch {
    Stake(u32),
    RequestWithdrawal(u32),
    Withdraw(u32),
}

async fn run_staking(
    config: Config,
    _metrics: Arc<ClientMetrics>,
    wallet: WalletContext,
    args: StakingArgs,
) -> anyhow::Result<()> {
    let _current_epoch = 0;
    // Start the re-staking machine.
    let restaking_period = Duration::from_secs(args.restaking_period_seconds.get());
    let contract_client: SuiContractClient = config.new_contract_client(wallet, None).await?;
    // The amount of WAL staked for StakedWal.
    let mut wal_staked: BTreeMap<ObjectID, u64> = Default::default();

    // REVIEW: is it OK to stake prior to epoch 1?
    let mut mode = StakingModeAtEpoch::Stake(1);
    loop {
        tokio::time::sleep(restaking_period).await;
        let mut committee = contract_client.read_client().current_committee().await?;
        let current_epoch = committee.epoch;
        let wal_balance = contract_client.balance(CoinType::Wal).await?;

        match mode {
            StakingModeAtEpoch::Stake(epoch) => {
                assert!(wal_staked.is_empty());
                if epoch <= current_epoch {
                    let mut nodes: Vec<StorageNode> = committee.members().to_vec();

                    // Shuffle the nodes to determine which ones get preferential staking
                    // treatment.
                    nodes.shuffle(&mut rand::thread_rng());

                    // Allocate half the WAL to various nodes. This is a linear walk over all of the
                    // WAL which we're going to stake, just to simplify the algorithm. Each "stake"
                    // below is one unit of MIN_STAKING_THRESHOLD.
                    let available_stakes = (wal_balance / MIN_STAKING_THRESHOLD) / 2;
                    let mut node_allocations = HashMap::<ObjectID, u64>::new();
                    for i in 0..available_stakes {
                        // Loop through the nodes in shuffled order until we've allocated all our
                        // stake.
                        node_allocations
                            .entry(nodes[i as usize % nodes.len()].node_id)
                            .and_modify(|x| *x += MIN_STAKING_THRESHOLD)
                            .or_insert(MIN_STAKING_THRESHOLD);
                    }
                    let node_ids_with_amounts: Vec<(ObjectID, u64)> =
                        node_allocations.into_iter().collect();
                    contract_client
                        .stake_with_pools(&node_ids_with_amounts)
                        .await?;
                }

                // Re-read the current epoch to avoid a race condition.
                committee = contract_client.read_client().current_committee().await?;
                // Some time after we've staked, we should schedule a withdrawal.
                mode = StakingModeAtEpoch::RequestWithdrawal(
                    committee.epoch + (rand::thread_rng().next_u32() % 3),
                );
            }
            StakingModeAtEpoch::RequestWithdrawal(staked_at_epoch) => {
                assert!(!wal_staked.is_empty());
                if staked_at_epoch < current_epoch {
                    // Request Withdrawal for any Wal we had staked.
                    for (&staked_wal_id, _amount) in wal_staked.iter() {
                        // Request to withdraw our WAL.
                        contract_client
                            .request_withdraw_stake(staked_wal_id)
                            .await?;
                    }
                }

                // Re-read the current epoch to avoid a race condition.
                committee = contract_client.read_client().current_committee().await?;
                // After we've scheduled a withdrawal, let's do a real withdrawal.
                mode = StakingModeAtEpoch::Withdraw(committee.epoch + 1)
            }
            StakingModeAtEpoch::Withdraw(epoch) => {
                if epoch <= current_epoch {
                    let mut wal_staked_temp = Default::default();
                    // Empty the current map so we can start our staking simulation anew.
                    std::mem::swap(&mut wal_staked_temp, &mut wal_staked);

                    // Unstake any Wal we had staked.
                    for (staked_wal_id, _amount) in wal_staked_temp.into_iter() {
                        // Actually withdraw our WAL.
                        contract_client.withdraw_stake(staked_wal_id).await?;
                    }
                }

                // Re-read the current epoch to avoid a race condition.
                committee = contract_client.read_client().current_committee().await?;
                // After we've withdrawn, let's schedule more staking.
                mode = StakingModeAtEpoch::Stake(
                    committee.epoch + (rand::thread_rng().next_u32() % 2),
                );
            }
        }

        // committee.
        // 1. See if there was already a staking plan for this epoch, if so, execute that plan.
        // 2. Enumerate existing nodes, and fabricate a new staking plan for epoch + 1, or update
        //    any existing plan with desired staking amount for each node.
        //    Call this the "staking plan".
        // 3. Maybe exit this sequence and go back to sleep.
        // 4. Calculate the amount to adjust each node in order to fulfill the new desired staking
        //    amounts.
        // 4. Request to withdraw any needed stake
    }
}
