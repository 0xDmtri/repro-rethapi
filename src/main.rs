use eyre::Result;
use std::path::Path;
use std::sync::Arc;

use alloy_rpc_types::simulate::MAX_SIMULATE_BLOCKS;
use reth_beacon_consensus::EthBeaconConsensus;
use reth_blockchain_tree::{
    BlockchainTree, BlockchainTreeConfig, ShareableBlockchainTree, TreeExternals,
};
use reth_chainspec::ChainSpecBuilder;
use reth_db::{open_db_read_only, DatabaseEnv};
use reth_network_api::noop::NoopNetwork;
use reth_node_ethereum::{EthEvmConfig, EthExecutorProvider, EthereumNode};
use reth_node_types::NodeTypesWithDBAdapter;
use reth_primitives::constants::ETHEREUM_BLOCK_GAS_LIMIT;
use reth_provider::CanonStateSubscriptions;
use reth_provider::{
    providers::{BlockchainProvider, StaticFileProvider},
    ProviderFactory,
};
use reth_rpc::EthApi;
use reth_rpc_eth_types::{
    EthStateCache, EthStateCacheConfig, FeeHistoryCache, FeeHistoryCacheConfig, GasPriceOracle,
    GasPriceOracleConfig,
};
use reth_rpc_server_types::constants::{DEFAULT_ETH_PROOF_WINDOW, DEFAULT_PROOF_PERMITS};
use reth_tasks::{pool::BlockingTaskPool, TokioTaskExecutor};
use reth_transaction_pool::{blobstore::NoopBlobStore, validate::EthTransactionValidatorBuilder};
use reth_transaction_pool::{
    CoinbaseTipOrdering, EthPooledTransaction, EthTransactionValidator, Pool,
    TransactionValidationTaskExecutor,
};

#[tokio::main]
async fn main() -> Result<()> {
    let reth_api = get_reth_api("your_path_to_node")?;

    let mut stream = reth_api.provider().subscribe_to_canonical_state();

    while let Ok(notification) = stream.recv().await {
        match notification {
            reth_provider::CanonStateNotification::Reorg { old, new } => {
                dbg!(old);
                dbg!(new);
            }
            reth_provider::CanonStateNotification::Commit { new } => {
                dbg!(new);
            }
        }
    }

    Ok(())
}

type RethProvider = BlockchainProvider<NodeTypesWithDBAdapter<EthereumNode, Arc<DatabaseEnv>>>;
type RethTxPool = Pool<
    TransactionValidationTaskExecutor<EthTransactionValidator<RethProvider, EthPooledTransaction>>,
    CoinbaseTipOrdering<EthPooledTransaction>,
    NoopBlobStore,
>;
type RethApi = EthApi<RethProvider, RethTxPool, NoopNetwork, EthEvmConfig>;

/// Make this chain agnostic
pub fn get_reth_api(path: impl ToString) -> Result<RethApi> {
    let db_path = path.to_string();
    let db_path = Path::new(&db_path);
    let db = open_db_read_only(&db_path.join("db"), Default::default())?;
    let spec = Arc::new(ChainSpecBuilder::mainnet().build());
    let evm_config = EthEvmConfig::new(spec.clone());

    let provider_factory =
        ProviderFactory::<NodeTypesWithDBAdapter<EthereumNode, Arc<DatabaseEnv>>>::new(
            db.into(),
            spec.clone(),
            StaticFileProvider::read_only(db_path.join("static_files"), true)?,
        );

    let tree_externals = TreeExternals::new(
        provider_factory.clone(),
        Arc::new(EthBeaconConsensus::new(spec.clone())),
        EthExecutorProvider::ethereum(spec.clone()),
    );

    let tree_config = BlockchainTreeConfig::default();

    let blockchain_tree =
        ShareableBlockchainTree::new(BlockchainTree::new(tree_externals, tree_config)?);

    let provider = BlockchainProvider::new(provider_factory.clone(), Arc::new(blockchain_tree))?;

    let task_executor = TokioTaskExecutor::default();

    let state_cache = EthStateCache::spawn_with(
        provider.clone(),
        EthStateCacheConfig::default(),
        task_executor.clone(),
        evm_config.clone(),
    );

    let transaction_validator = EthTransactionValidatorBuilder::new(spec.clone()).build_with_tasks(
        provider.clone(),
        task_executor.clone(),
        NoopBlobStore::default(),
    );

    let tx_pool = reth_transaction_pool::Pool::eth_pool(
        transaction_validator,
        NoopBlobStore::default(),
        Default::default(),
    );

    let fee_history = FeeHistoryCache::new(
        EthStateCache::spawn_with(
            provider.clone(),
            EthStateCacheConfig::default(),
            task_executor.clone(),
            evm_config.clone(),
        ),
        FeeHistoryCacheConfig::default(),
    );

    let api = EthApi::new(
        provider.clone(),
        tx_pool,
        NoopNetwork::default(),
        state_cache.clone(),
        GasPriceOracle::new(provider, GasPriceOracleConfig::default(), state_cache),
        ETHEREUM_BLOCK_GAS_LIMIT,
        MAX_SIMULATE_BLOCKS,
        DEFAULT_ETH_PROOF_WINDOW,
        BlockingTaskPool::build()?,
        fee_history,
        evm_config,
        DEFAULT_PROOF_PERMITS,
    );

    Ok(api)
}
