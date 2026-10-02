use super::{PoolError, PoolInfo, PoolReader, TokenMeta, TokenSide, WalletHolding};
use crate::chain::Chain;
use crate::state::RpcRateLimiter;
use alloy::{
    network::Ethereum,
    primitives::{Address, B256, U256, address, aliases::U24},
    providers::{DynProvider, Provider, ProviderBuilder},
    rpc::client::RpcClient,
    rpc::types::Filter,
    sol,
};
use alloy_json_rpc::{RequestPacket, ResponsePacket, ResponsePayload, RpcError};
use alloy_transport::{TransportError, TransportErrorKind, TransportFut};
use alloy_transport_http::reqwest::{Client as AlloyHttpClient, Url as AlloyUrl};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fmt::Debug;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::{Layer, Service};
sol! {
    #[sol(rpc)]
    interface Erc20 {
        function symbol() external view returns (string value);
        function name() external view returns (string value);
        function decimals() external view returns (uint8 value);
        function totalSupply() external view returns (uint256 value);
        function balanceOf(address owner) external view returns (uint256 value);
        /// ERC-8056's optional stock-token split/dividend multiplier.
        function uiMultiplier() external view returns (uint256 value);
    }

    #[sol(rpc)]
    interface V2Pair {
        function token0() external view returns (address token);
        function token1() external view returns (address token);
        function getReserves() external view returns (
            uint112 reserve0,
            uint112 reserve1,
            uint32 blockTimestampLast
        );
    }

    #[sol(rpc)]
    interface V3Pool {
        function token0() external view returns (address token);
        function token1() external view returns (address token);
        function fee() external view returns (uint24 value);
        function liquidity() external view returns (uint128 value);
        function slot0() external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint8 feeProtocol,
            bool unlocked
        );
        function factory() external view returns (address value);
    }

    #[sol(rpc)]
    interface V2Factory {
        function getPair(address tokenA, address tokenB) external view returns (address pair);
    }

    #[sol(rpc)]
    interface V3Factory {
        function getPool(address tokenA, address tokenB, uint24 fee) external view returns (address pool);
    }

    #[sol(rpc)]
    interface StateView {
        function getSlot0(bytes32 poolId) external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint24 protocolFee,
            uint24 lpFee
        );
        function getLiquidity(bytes32 poolId) external view returns (uint128 liquidity);
    }

    event Initialize(
        bytes32 indexed id,
        address indexed currency0,
        address indexed currency1,
        uint24 fee,
        int24 tickSpacing,
        address hooks,
        uint160 sqrtPriceX96,
        int24 tick
    );
}
fn record_eth_call<T: Debug>(params: Value, result: &T) {
    let value = Value::String(format!("{result:?}"));
    crate::attest::record_read("eth_call", params, &value, true, None, None);
}

const MAX_POOL_DISCOVERY_CANDIDATES: usize = 128;
const MAX_POOL_DISCOVERY_OPERATIONS: usize = 512;
const POOL_DISCOVERY_DEADLINE: Duration = Duration::from_secs(5);

struct PoolDiscoveryBudget {
    started: Instant,
    operations: usize,
}

impl PoolDiscoveryBudget {
    fn new() -> Self {
        Self { started: Instant::now(), operations: 0 }
    }

    fn consume(&mut self) -> Result<(), PoolError> {
        if self.started.elapsed() >= POOL_DISCOVERY_DEADLINE {
            return Err(PoolError::BudgetExceeded("deadline"));
        }
        self.operations = self.operations.saturating_add(1);
        if self.operations > MAX_POOL_DISCOVERY_OPERATIONS {
            return Err(PoolError::BudgetExceeded("RPC operation budget"));
        }
        Ok(())
    }
}
const V3_FEES: [u32; 4] = [100, 500, 3_000, 10_000];

// Uniswap's official deployment feed: https://developers.uniswap.org/deployments.json
const UNI_V2_ETHEREUM: Address = address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
const UNI_V2_BASE: Address = address!("8909Dc15e40173Ff4699343b6eB8132c65e18eC6");
const UNI_V2_BNB: Address = address!("8909Dc15e40173Ff4699343b6eB8132c65e18eC6");
const UNI_V2_ROBINHOOD: Address = address!("8bcEaA40B9AcdfAedF85AdF4FF01F5Ad6517937f");

// Uniswap v3 deployment pages:
// https://developers.uniswap.org/docs/protocols/v3/deployments/v3-ethereum-deployments
// https://developers.uniswap.org/docs/protocols/v3/deployments/v3-base-deployments
// https://developers.uniswap.org/docs/protocols/v3/deployments/v3-bnb-deployments
// https://developers.uniswap.org/docs/protocols/v3/deployments/v3-robinhood-chain-deployments
const UNI_V3_ETHEREUM: Address = address!("1F98431c8aD98523631AE4a59f267346ea31F984");
const UNI_V3_BASE: Address = address!("33128a8fC17869897dcE68Ed026d694621f6FDfD");
const UNI_V3_BNB: Address = address!("dB1d10011AD0Ff90774D0C6Bb92e5C5c8b4461F7");
const UNI_V3_ROBINHOOD: Address = address!("1f7d7550B1b028f7571E69A784071F0205FD2EfA");

// PancakeSwap v3 deployments: https://developer.pancakeswap.finance/contracts/v3/addresses
const PANCAKE_V3_FACTORY: Address = address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865");

// Uniswap v4 StateView deployments: https://developers.uniswap.org/docs/protocols/v4/deployments
const V4_STATEVIEW_ETHEREUM: Address = address!("7ffe42c4a5deea5b0fec41c94c136cf115597227");
const V4_STATEVIEW_BASE: Address = address!("a3c0c9b65bad0b08107aa264b0f3db444b867a71");
const V4_STATEVIEW_BNB: Address = address!("d13dd3d6e93f276fafc9db9e6bb47c1180aee0c4");
const V4_STATEVIEW_ROBINHOOD: Address = address!("f3334192d15450cdd385c8b70e03f9a6bd9e673b");
// Uniswap v4 deployments: https://developers.uniswap.org/docs/protocols/v4/deployments
const V4_POOLMANAGER_ETHEREUM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");
const V4_POOLMANAGER_BASE: Address = address!("498581ff718922c3f8e6a244956af099b2652b2b");
const V4_POOLMANAGER_BNB: Address = address!("28e2ea090877bf75740558f6bfb36a5ffee9e9df");
const V4_POOLMANAGER_ROBINHOOD: Address = address!("8366a39cc670b4001a1121b8f6a443a643e40951");
// Creation transaction blocks from the public chain explorers:
// Ethereum: https://eth.blockscout.com/tx/0x747e0e02b7590eed32cface28e83260884e0b80675f5ae223c6888053aa68528
// Base: https://base.blockscout.com/tx/0x25f482fbd94cdea11b018732e455b8e9a940b933cabde3c0c5dd63ea65e85349
// Robinhood: https://robinhoodchain.blockscout.com/tx/0x4fb28d4935866f462582c6c931c6f2705e55f5be5eb178c7d8d9329a95c44c41
const V4_DEPLOYMENT_ETHEREUM: u64 = 21_688_329;
const V4_DEPLOYMENT_BASE: u64 = 25_350_988;
const V4_DEPLOYMENT_ROBINHOOD: u64 = 9_070;
// BNB deployment entry and transaction:
// https://github.com/Uniswap/contracts/blob/main/deployments/56.md
// https://bscscan.com/tx/0x64b395f1b0b3c734a477c802bc8cc3ce394f328c651290d0d166946048487bbe
const V4_DEPLOYMENT_BNB: u64 = 45_970_610;

#[derive(Clone, Copy)]
struct Deployments {
    uniswap_v2: Option<Address>,
    uniswap_v3: Option<Address>,
    pancake_v3: Option<Address>,
    v4_state_view: Option<Address>,
    v4_pool_manager: Option<Address>,
    v4_deployment_block: u64,
}

fn deployments(chain: Chain) -> Deployments {
    match chain {
        Chain::RobinhoodChain => Deployments {
            uniswap_v2: Some(UNI_V2_ROBINHOOD),
            uniswap_v3: Some(UNI_V3_ROBINHOOD),
            pancake_v3: Some(PANCAKE_V3_FACTORY),
            v4_state_view: Some(V4_STATEVIEW_ROBINHOOD),
            v4_pool_manager: Some(V4_POOLMANAGER_ROBINHOOD),
            v4_deployment_block: V4_DEPLOYMENT_ROBINHOOD,
        },
        Chain::Base => Deployments {
            uniswap_v2: Some(UNI_V2_BASE),
            uniswap_v3: Some(UNI_V3_BASE),
            pancake_v3: Some(PANCAKE_V3_FACTORY),
            v4_state_view: Some(V4_STATEVIEW_BASE),
            v4_pool_manager: Some(V4_POOLMANAGER_BASE),
            v4_deployment_block: V4_DEPLOYMENT_BASE,
        },
        Chain::Ethereum => Deployments {
            uniswap_v2: Some(UNI_V2_ETHEREUM),
            uniswap_v3: Some(UNI_V3_ETHEREUM),
            pancake_v3: Some(PANCAKE_V3_FACTORY),
            v4_state_view: Some(V4_STATEVIEW_ETHEREUM),
            v4_pool_manager: Some(V4_POOLMANAGER_ETHEREUM),
            v4_deployment_block: V4_DEPLOYMENT_ETHEREUM,
        },
        Chain::Bnb => Deployments {
            uniswap_v2: Some(UNI_V2_BNB),
            uniswap_v3: Some(UNI_V3_BNB),
            pancake_v3: Some(PANCAKE_V3_FACTORY),
            v4_state_view: Some(V4_STATEVIEW_BNB),
            v4_pool_manager: Some(V4_POOLMANAGER_BNB),
            v4_deployment_block: V4_DEPLOYMENT_BNB,
        },
        Chain::Solana => Deployments {
            uniswap_v2: None,
            uniswap_v3: None,
            pancake_v3: None,
            v4_state_view: None,
            v4_pool_manager: None,
            v4_deployment_block: 0,
        },
    }
}

const RPC_MAX_ATTEMPTS: usize = 4;
const RPC_BACKOFF: [Duration; RPC_MAX_ATTEMPTS] =
    [Duration::from_millis(250), Duration::from_secs(1), Duration::from_secs(3), Duration::ZERO];

#[derive(Clone)]
struct BoundedHttp {
    client: AlloyHttpClient,
    url: AlloyUrl,
}

impl BoundedHttp {
    fn new(client: AlloyHttpClient, url: AlloyUrl) -> Self {
        Self { client, url }
    }
}

impl Service<RequestPacket> for BoundedHttp {
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        let this = self.clone();
        Box::pin(async move {
            let response = this
                .client
                .post(this.url)
                .json(&request)
                .headers(request.headers())
                .send()
                .await
                .map_err(TransportErrorKind::custom)?;
            let status = response.status();
            let mut body = Vec::new();
            if response
                .content_length()
                .is_some_and(|length| length as usize > crate::net::MAX_RESPONSE_BYTES)
            {
                return Err(TransportErrorKind::custom_str(
                    "RPC response exceeded the 4 MiB limit",
                ));
            }
            let mut response = response;
            while let Some(chunk) = response.chunk().await.map_err(TransportErrorKind::custom)? {
                if body.len() + chunk.len() > crate::net::MAX_RESPONSE_BYTES {
                    return Err(TransportErrorKind::custom_str(
                        "RPC response exceeded the 4 MiB limit",
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            if !status.is_success() {
                return Err(TransportErrorKind::http_error(
                    status.as_u16(),
                    String::from_utf8_lossy(&body).into_owned(),
                ));
            }
            serde_json::from_slice(&body)
                .map_err(|error| TransportError::deser_err(error, String::from_utf8_lossy(&body)))
        })
    }
}

#[derive(Clone)]
struct RpcLayer {
    limiter: Arc<RpcRateLimiter>,
}

impl RpcLayer {
    fn new(limiter: Arc<RpcRateLimiter>) -> Self {
        Self { limiter }
    }
}

#[derive(Clone)]
struct RpcService<S> {
    inner: S,
    limiter: Arc<RpcRateLimiter>,
}

impl<S> Layer<S> for RpcLayer {
    type Service = RpcService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RpcService { inner, limiter: Arc::clone(&self.limiter) }
    }
}

fn retryable_response(response: &ResponsePacket) -> bool {
    response.as_error().is_some_and(|error| {
        matches!(error.code, 429 | -32_005 | -32_429)
            || error.message.to_ascii_lowercase().contains("rate limit")
    })
}

fn retryable_error(error: &TransportError) -> bool {
    match error {
        RpcError::ErrorResp(payload) => {
            matches!(payload.code, 429 | -32_005 | -32_429)
                || payload.message.to_ascii_lowercase().contains("rate limit")
        }
        RpcError::DeserError { text, .. } => text.contains("429"),
        RpcError::Transport(error) => {
            error.as_http_error().is_some_and(|error| error.status == 429)
        }
        _ => false,
    }
}

fn response_within_limit(response: &ResponsePacket) -> bool {
    fn payload_size(payload: &ResponsePayload) -> usize {
        match payload {
            ResponsePayload::Success(value) => value.get().len(),
            ResponsePayload::Failure(error) => {
                64 + error.message.len() + error.data.as_ref().map_or(0, |data| data.get().len())
            }
        }
    }
    let size = match response {
        ResponsePacket::Single(response) => payload_size(&response.payload),
        ResponsePacket::Batch(responses) => {
            responses.iter().map(|response| payload_size(&response.payload)).sum()
        }
    };
    size <= crate::net::MAX_RESPONSE_BYTES
}

impl<S> Service<RequestPacket> for RpcService<S>
where
    S: Service<RequestPacket, Response = ResponsePacket, Error = TransportError>
        + Send
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(
        &mut self,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(context)
    }

    fn call(&mut self, request: RequestPacket) -> Self::Future {
        let mut inner = self.inner.clone();
        let limiter = Arc::clone(&self.limiter);
        Box::pin(async move {
            for (attempt, backoff) in RPC_BACKOFF.iter().enumerate() {
                limiter.acquire().await;
                let result: Result<ResponsePacket, TransportError> = match tokio::time::timeout(
                    Duration::from_secs(30),
                    inner.call(request.clone()),
                )
                .await
                {
                    Ok(Ok(response)) if response_within_limit(&response) => Ok(response),
                    Ok(Ok(_)) => Err(TransportError::local_usage_str(
                        "RPC response exceeded the 4 MiB limit",
                    )),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(TransportError::local_usage_str("RPC request timed out")),
                };
                let retry = match &result {
                    Ok(response) => retryable_response(response),
                    Err(error) => retryable_error(error),
                };
                if !retry || attempt + 1 == RPC_MAX_ATTEMPTS {
                    return result;
                }
                tokio::time::sleep(*backoff).await;
            }
            unreachable!("RPC retry loop always returns")
        })
    }
}

/// One Alloy HTTP provider and pool reader for one EVM chain.
#[derive(Clone)]
pub struct EvmReader {
    chain: Chain,
    provider: DynProvider<Ethereum>,
}

impl EvmReader {
    /// Connect one long-lived HTTP provider for `chain`.
    #[cfg(test)]
    pub fn new(chain: Chain, rpc_url: &str) -> Result<Self, PoolError> {
        Self::new_with_limiter(chain, rpc_url, Arc::new(RpcRateLimiter::new(8)))
    }

    pub fn new_with_limiter(
        chain: Chain,
        rpc_url: &str,
        rpc_limiter: Arc<RpcRateLimiter>,
    ) -> Result<Self, PoolError> {
        if chain == Chain::Solana {
            return Err(PoolError::Reader("an EVM reader cannot use Solana".to_owned()));
        }
        let url: AlloyUrl = rpc_url
            .parse()
            .map_err(|error| PoolError::Reader(format!("invalid EVM RPC URL: {error}")))?;
        let client = AlloyHttpClient::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("qed/0.1")
            .build()
            .map_err(|_| PoolError::Reader("could not build RPC HTTP client".to_owned()))?;
        let service = RpcLayer::new(rpc_limiter).layer(BoundedHttp::new(client, url.clone()));
        let is_local = matches!(url.host_str(), None | Some("localhost" | "127.0.0.1"));
        let rpc_client = RpcClient::new(service, is_local);
        let provider = ProviderBuilder::new().connect_client(rpc_client).erased();
        Ok(Self { chain, provider })
    }
    pub async fn wallet_holdings(
        &self,
        owner: &str,
        known_tokens: &[String],
    ) -> Result<Vec<WalletHolding>, PoolError> {
        let owner = parse_address(owner)?;
        let mut tokens = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for token in known_tokens {
            let Ok(address) = parse_address(token) else { continue };
            if seen.insert(address) {
                tokens.push(address);
            }
        }
        let mut balances = Vec::new();
        for chunk in tokens.chunks(64) {
            let mut multicall = self.provider.multicall().dynamic();
            for token in chunk {
                multicall =
                    multicall.add_dynamic(Erc20::new(*token, &self.provider).balanceOf(owner));
            }
            let fallback_tokens = match multicall.try_aggregate(false).await {
                Ok(results) => {
                    let mut failed = Vec::new();
                    for (token, result) in chunk.iter().copied().zip(results) {
                        match result {
                            Ok(balance) => balances.push((token, balance)),
                            Err(_) => failed.push(token),
                        }
                    }
                    failed
                }
                Err(_) => chunk.to_vec(),
            };
            if !fallback_tokens.is_empty() {
                let fallback_concurrency = Arc::new(tokio::sync::Semaphore::new(8));
                let mut tasks = tokio::task::JoinSet::new();
                for token in fallback_tokens {
                    let provider = self.provider.clone();
                    let fallback_concurrency = Arc::clone(&fallback_concurrency);
                    tasks.spawn(async move {
                        let Ok(permit) = fallback_concurrency.acquire_owned().await else {
                            return (token, false, None);
                        };
                        let balance = Erc20::new(token, &provider).balanceOf(owner).call().await;
                        drop(permit);
                        (token, true, balance.ok())
                    });
                }
                while let Some(result) = tasks.join_next().await {
                    match result {
                        Ok((token, true, Some(balance))) => balances.push((token, balance)),
                        Ok(_) | Err(_) => {
                            return Err(PoolError::Reader(
                                "wallet balance scan incomplete".to_owned(),
                            ));
                        }
                    }
                }
            }
        }
        let mut holdings = Vec::new();
        for (token, balance) in balances {
            if balance.is_zero() {
                continue;
            }
            let contract = Erc20::new(token, &self.provider);
            let symbol = super::cap_token_text(contract.symbol().call().await.ok());
            let decimals = contract.decimals().call().await.ok();
            holdings.push(WalletHolding {
                chain: self.chain,
                token_address: canonical(token),
                symbol,
                amount: balance.to_string(),
                decimals,
            });
        }
        Ok(holdings)
    }

    pub async fn token_meta(&self, address: &str) -> Result<TokenMeta, PoolError> {
        let token = parse_address(address)?;
        let contract = Erc20::new(token, &self.provider);
        let symbol = super::cap_token_text(contract.symbol().call().await.ok());
        record_eth_call(json!([canonical(token), "symbol()"]), &symbol);
        let name = super::cap_token_text(contract.name().call().await.ok());
        record_eth_call(json!([canonical(token), "name()"]), &name);
        let decimals = contract.decimals().call().await.ok();
        record_eth_call(json!([canonical(token), "decimals()"]), &decimals);
        let total_supply = contract.totalSupply().call().await.ok().map(|value| value.to_string());
        record_eth_call(json!([canonical(token), "totalSupply()"]), &total_supply);

        if let Ok(value) = contract.uiMultiplier().call().await {
            record_eth_call(json!([canonical(token), "uiMultiplier()"]), &value);
            tracing::debug!(
                token = %token,
                ui_multiplier = %value,
                "ERC-8056 uiMultiplier observed; raw balances remain unscaled"
            );
        }

        Ok(TokenMeta { address: canonical(token), symbol, name, decimals, total_supply })
    }

    /// Find factory pools for a token and candidate quote addresses, then
    /// order them by descending raw quote-token balance.
    pub async fn pools_for_token(
        &self,
        token: &str,
        candidate_quotes: &[String],
    ) -> Result<Vec<PoolInfo>, PoolError> {
        tokio::time::timeout(
            POOL_DISCOVERY_DEADLINE,
            self.pools_for_token_bounded(token, candidate_quotes),
        )
        .await
        .map_err(|_| PoolError::BudgetExceeded("deadline"))?
    }

    async fn pools_for_token_bounded(
        &self,
        token: &str,
        candidate_quotes: &[String],
    ) -> Result<Vec<PoolInfo>, PoolError> {
        let token = parse_address(token)?;
        let mut seen_quotes = HashSet::new();
        let quotes = candidate_quotes
            .iter()
            .filter_map(|quote| quote.parse::<Address>().ok())
            .filter(|quote| *quote != token)
            .filter(|quote| seen_quotes.insert(*quote))
            .collect::<Vec<_>>();
        let configured = deployments(self.chain);
        let mut budget = PoolDiscoveryBudget::new();
        let mut pools = Vec::new();
        let mut seen = Vec::new();

        for (candidate_index, quote) in quotes.into_iter().enumerate() {
            if candidate_index >= MAX_POOL_DISCOVERY_CANDIDATES {
                return Err(PoolError::BudgetExceeded("candidate quote budget"));
            }
            if let Some(factory) = configured.uniswap_v2 {
                budget.consume()?;
                let address =
                    V2Factory::new(factory, &self.provider).getPair(token, quote).call().await.ok();
                record_eth_call(
                    json!([
                        format!("{factory:?}"),
                        "getPair",
                        format!("{token:?}"),
                        format!("{quote:?}")
                    ]),
                    &address,
                );
                self.push_pool(&mut pools, &mut seen, address, candidate_quotes, &mut budget)
                    .await?;
            }
            if let Some(factory) = configured.uniswap_v3 {
                for fee in V3_FEES {
                    budget.consume()?;
                    let address = V3Factory::new(factory, &self.provider)
                        .getPool(token, quote, U24::from_limbs([u64::from(fee)]))
                        .call()
                        .await
                        .ok();
                    record_eth_call(
                        json!([
                            format!("{factory:?}"),
                            "getPool",
                            format!("{token:?}"),
                            format!("{quote:?}"),
                            fee
                        ]),
                        &address,
                    );
                    self.push_pool(&mut pools, &mut seen, address, candidate_quotes, &mut budget)
                        .await?;
                }
            }
            if let Some(factory) = configured.pancake_v3 {
                for fee in V3_FEES {
                    budget.consume()?;
                    let address = V3Factory::new(factory, &self.provider)
                        .getPool(token, quote, U24::from_limbs([u64::from(fee)]))
                        .call()
                        .await
                        .ok();
                    record_eth_call(
                        json!([
                            format!("{factory:?}"),
                            "getPool",
                            format!("{token:?}"),
                            format!("{quote:?}"),
                            fee
                        ]),
                        &address,
                    );
                    self.push_pool(&mut pools, &mut seen, address, candidate_quotes, &mut budget)
                        .await?;
                }
            }
        }

        pools.sort_by(|left, right| {
            quote_balance(right)
                .cmp(&quote_balance(left))
                .then_with(|| left.pool.to_ascii_lowercase().cmp(&right.pool.to_ascii_lowercase()))
        });
        Ok(pools)
    }

    pub async fn read_pool_with_quotes(
        &self,
        address: &str,
        candidate_quotes: &[String],
    ) -> Result<PoolInfo, PoolError> {
        let pool = parse_address(address)?;
        let contract = V3Pool::new(pool, &self.provider);
        let token0 = contract
            .token0()
            .call()
            .await
            .map_err(|error| PoolError::Reader(format!("reading pool token0: {error}")))?;
        record_eth_call(json!([canonical(pool), "token0()"]), &token0);
        let token1 = contract
            .token1()
            .call()
            .await
            .map_err(|error| PoolError::Reader(format!("reading pool token1: {error}")))?;
        record_eth_call(json!([canonical(pool), "token1()"]), &token1);

        let reserves = V2Pair::new(pool, &self.provider).getReserves().call().await;
        record_eth_call(json!([canonical(pool), "getReserves()"]), &reserves.is_ok());
        let dex = if reserves.is_ok() {
            "uniswap-v2"
        } else {
            let slot0 = contract.slot0().call().await;
            record_eth_call(json!([canonical(pool), "slot0()"]), &slot0.is_ok());
            if slot0.is_ok() {
                let factory = contract.factory().call().await.ok();
                record_eth_call(json!([canonical(pool), "factory()"]), &factory);
                match factory {
                    Some(factory)
                        if deployments(self.chain)
                            .pancake_v3
                            .is_some_and(|known| known == factory) =>
                    {
                        "pancake-v3"
                    }
                    _ => "uniswap-v3",
                }
            } else {
                return Err(PoolError::Unknown(format!("unsupported EVM pool {pool}")));
            }
        };

        let token0_side = self.token_side(pool, token0).await;
        let token1_side = self.token_side(pool, token1).await;
        let token0_matches = candidate_quotes.iter().any(|quote| same_address(quote, token0));
        let token1_matches = candidate_quotes.iter().any(|quote| same_address(quote, token1));
        let token0_is_quote = token0_matches && !token1_matches;
        let (base, quote) =
            if token0_is_quote { (token1_side, token0_side) } else { (token0_side, token1_side) };

        Ok(PoolInfo { chain: self.chain, pool: canonical(pool), dex: dex.to_owned(), base, quote })
    }
    async fn read_v4_pool_data(&self, pool_id: &str) -> Result<(PoolInfo, Vec<String>), PoolError> {
        let pool_id = B256::from_str(pool_id).map_err(|_| PoolError::InvalidAddress)?;
        let configured = deployments(self.chain);
        let state_view_address = configured
            .v4_state_view
            .ok_or_else(|| PoolError::Unknown("v4 StateView is unavailable".to_owned()))?;
        let pool_manager = configured
            .v4_pool_manager
            .ok_or_else(|| PoolError::Unknown("v4 PoolManager is unavailable".to_owned()))?;
        let state_view = StateView::new(state_view_address, &self.provider);
        let slot0 = state_view
            .getSlot0(pool_id)
            .call()
            .await
            .map_err(|error| PoolError::Reader(format!("reading v4 slot0: {error}")))?;
        record_eth_call(
            json!([format!("{state_view_address:?}"), "getSlot0", pool_id.to_string()]),
            &true,
        );
        let liquidity = state_view
            .getLiquidity(pool_id)
            .call()
            .await
            .map_err(|error| PoolError::Reader(format!("reading v4 liquidity: {error}")))?;
        record_eth_call(
            json!([format!("{state_view_address:?}"), "getLiquidity", pool_id.to_string()]),
            &liquidity,
        );
        if slot0.sqrtPriceX96 == 0 && liquidity == 0 {
            return Err(PoolError::Unknown(format!("v4 pool {pool_id} is not initialised")));
        }

        let filter = Filter::new()
            .address(pool_manager)
            .event("Initialize(bytes32,address,address,uint24,int24,address,uint160,int24)")
            .topic1(pool_id);
        let latest_block = self.provider.get_block_number().await.map_err(|error| {
            PoolError::Reader(format!("reading latest block for v4 logs: {error}"))
        })?;
        let latest_value = Value::String(format!("0x{latest_block:x}"));
        crate::attest::record_read(
            "eth_blockNumber",
            json!([]),
            &latest_value,
            false,
            Some(latest_block),
            None,
        );
        const LOG_CHUNK: u64 = 5_000_000;
        let mut from_block = configured.v4_deployment_block;
        let mut event = None;
        while from_block <= latest_block {
            let to_block = from_block.saturating_add(LOG_CHUNK - 1).min(latest_block);
            let logs = self
                .provider
                .get_logs(&filter.clone().from_block(from_block).to_block(to_block))
                .await
                .map_err(|error| {
                    PoolError::Reader(format!("reading v4 Initialize logs: {error}"))
                })?;
            record_eth_call(
                json!([format!("{pool_manager:?}"), "eth_getLogs", from_block, to_block]),
                &logs,
            );
            event = logs.iter().find_map(|log| {
                log.log_decode::<Initialize>().ok().map(|decoded| decoded.data().clone())
            });
            if event.is_some() || to_block == latest_block {
                break;
            }
            from_block = to_block.saturating_add(1);
        }
        let event = event.ok_or_else(|| {
            PoolError::Unknown(format!("v4 Initialize event not found for {pool_id}"))
        })?;

        let q96 = U256::from(1_u8) << 96;
        let liquidity = U256::from(liquidity);
        let sqrt_price = U256::from(slot0.sqrtPriceX96);
        let amount0 = if sqrt_price == 0 { U256::ZERO } else { liquidity * q96 / sqrt_price };
        let amount1 = liquidity * sqrt_price / q96;
        let token0 = self.token_side_with_balance(event.currency0, amount0).await;
        let token1 = self.token_side_with_balance(event.currency1, amount1).await;
        let pool = PoolInfo {
            chain: self.chain,
            pool: pool_id.to_string(),
            dex: "uniswap-v4".to_owned(),
            base: token0,
            quote: token1,
        };
        let evidence = vec![
            format!(
                "Uniswap v4 Initialize resolved currencies {} and {}.",
                event.currency0, event.currency1
            ),
            format!(
                "Uniswap v4 fee {} (tick spacing {}) and hooks address {}.",
                event.fee, event.tickSpacing, event.hooks
            ),
            format!(
                "v4 reserves are approximate, v4 concentrated liquidity: amount0={} and amount1={} raw units from liquidity {} and sqrtPriceX96 {}.",
                amount0, amount1, liquidity, slot0.sqrtPriceX96
            ),
        ];
        Ok((pool, evidence))
    }
    pub async fn read_v4_pool(&self, pool_id: &str) -> Result<(PoolInfo, Vec<String>), PoolError> {
        self.read_v4_pool_data(pool_id).await
    }

    async fn push_pool(
        &self,
        pools: &mut Vec<PoolInfo>,
        seen: &mut Vec<String>,
        address: Option<Address>,
        candidate_quotes: &[String],
        budget: &mut PoolDiscoveryBudget,
    ) -> Result<(), PoolError> {
        let Some(address) = address.filter(|address| *address != Address::ZERO) else {
            return Ok(());
        };
        let key = format!("{address:?}");
        if seen.iter().any(|entry| entry == &key) {
            return Ok(());
        }
        budget.consume()?;
        if let Ok(pool) = self.read_pool_with_quotes(&canonical(address), candidate_quotes).await {
            seen.push(key);
            pools.push(pool);
        }
        Ok(())
    }
    async fn token_side(&self, pool: Address, token: Address) -> TokenSide {
        let contract = Erc20::new(token, &self.provider);
        let symbol = super::cap_token_text(contract.symbol().call().await.ok());
        record_eth_call(json!([canonical(token), "symbol()"]), &symbol);
        let decimals = contract.decimals().call().await.ok();
        record_eth_call(json!([canonical(token), "decimals()"]), &decimals);
        let balance = contract.balanceOf(pool).call().await.ok().map(|value| value.to_string());
        record_eth_call(json!([canonical(token), "balanceOf", canonical(pool)]), &balance);
        TokenSide { address: canonical(token), symbol, decimals, balance }
    }
    async fn token_side_with_balance(&self, token: Address, balance: U256) -> TokenSide {
        let contract = Erc20::new(token, &self.provider);
        let symbol = super::cap_token_text(contract.symbol().call().await.ok());
        record_eth_call(json!([canonical(token), "symbol()"]), &symbol);
        let decimals = contract.decimals().call().await.ok();
        record_eth_call(json!([canonical(token), "decimals()"]), &decimals);
        TokenSide {
            address: canonical(token),
            symbol,
            decimals,
            balance: Some(balance.to_string()),
        }
    }
}

#[async_trait]
impl PoolReader for EvmReader {
    fn chain(&self) -> Chain {
        self.chain
    }

    async fn read_pool(&self, address: &str) -> Result<PoolInfo, PoolError> {
        self.read_pool_with_quotes(address, &[]).await
    }

    async fn read_v4_pool(&self, pool_id: &str) -> Result<(PoolInfo, Vec<String>), PoolError> {
        EvmReader::read_v4_pool(self, pool_id).await
    }

    async fn token_meta(&self, address: &str) -> Result<TokenMeta, PoolError> {
        EvmReader::token_meta(self, address).await
    }

    async fn pools_for_token(
        &self,
        token: &str,
        candidate_quotes: &[String],
    ) -> Result<Vec<PoolInfo>, PoolError> {
        EvmReader::pools_for_token(self, token, candidate_quotes).await
    }
    async fn wallet_holdings(
        &self,
        owner: &str,
        known_tokens: &[String],
    ) -> Result<Vec<WalletHolding>, PoolError> {
        EvmReader::wallet_holdings(self, owner, known_tokens).await
    }

    async fn code_at(&self, address: &str) -> Result<Vec<u8>, PoolError> {
        let address = parse_address(address)?;
        let code =
            self.provider.get_code_at(address).await.map_err(|error| {
                PoolError::Reader(format!("reading contract bytecode: {error}"))
            })?;
        let encoded = code.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let value = Value::String(format!("0x{encoded}"));
        crate::attest::record_read(
            "eth_getCode",
            json!([canonical(address), "latest"]),
            &value,
            false,
            None,
            None,
        );
        Ok(code.to_vec())
    }

    async fn record_position(&self) -> Result<(), PoolError> {
        let block = self
            .provider
            .get_block_number()
            .await
            .map_err(|error| PoolError::Reader(format!("reading block number: {error}")))?;
        let value = Value::String(format!("0x{block:x}"));
        crate::attest::record_read("eth_blockNumber", json!([]), &value, false, Some(block), None);
        Ok(())
    }
}

fn parse_address(value: &str) -> Result<Address, PoolError> {
    Address::from_str(value).map_err(|_| PoolError::InvalidAddress)
}

fn canonical(address: Address) -> String {
    address.to_string()
}

fn same_address(value: &str, address: Address) -> bool {
    value.parse::<Address>().is_ok_and(|candidate| candidate == address)
}

fn quote_balance(pool: &PoolInfo) -> U256 {
    pool.quote.balance.as_deref().and_then(|value| U256::from_str(value).ok()).unwrap_or(U256::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::sol_types::SolEvent;
    use serde_json::{Value, json};
    use std::{collections::HashMap, sync::Arc};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };

    const TOKEN0: &str = "0x0000000000000000000000000000000000000011";
    const TOKEN1: &str = "0x0000000000000000000000000000000000000012";
    const POOL: &str = "0x0000000000000000000000000000000000000022";

    fn address_word(address: &str) -> String {
        format!("0x{}{}", "0".repeat(24), address.trim_start_matches("0x"))
    }

    fn uint_word(value: u64) -> String {
        format!("{value:064x}")
    }
    fn string_result(value: &str) -> String {
        let encoded = value.as_bytes().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let padding = "0".repeat((64 - encoded.len() % 64) % 64);
        format!("0x{}{}{}{}", uint_word(32), uint_word(value.len() as u64), encoded, padding)
    }

    fn call_key(to: &str, data: &str) -> String {
        format!("call:{}:{}", to.to_ascii_lowercase(), data.to_ascii_lowercase())
    }

    fn selector_key(to: &str, selector: &str) -> String {
        format!("selector:{}:{}", to.to_ascii_lowercase(), selector.to_ascii_lowercase())
    }

    fn get_code_key(address: &str) -> String {
        format!("code:{}", address.to_ascii_lowercase())
    }

    fn responses() -> HashMap<String, String> {
        HashMap::new()
    }

    fn put_call(map: &mut HashMap<String, String>, to: &str, data: &str, result: String) {
        map.insert(call_key(to, data), result);
    }

    fn put_selector(map: &mut HashMap<String, String>, to: &str, selector: &str, result: String) {
        map.insert(selector_key(to, selector), result);
    }

    fn standard_v2_pool(map: &mut HashMap<String, String>, pool: &str, token0: &str, token1: &str) {
        put_call(map, pool, "0x0dfe1681", address_word(token0));
        put_call(map, pool, "0xd21220a7", address_word(token1));
        put_call(
            map,
            pool,
            "0x0902f1ac",
            format!("0x{}{}{}", uint_word(1_000), uint_word(2_000), uint_word(3)),
        );
    }

    fn standard_v3_pool(map: &mut HashMap<String, String>, pool: &str, token0: &str, token1: &str) {
        put_call(map, pool, "0x0dfe1681", address_word(token0));
        put_call(map, pool, "0xd21220a7", address_word(token1));
        put_call(
            map,
            pool,
            "0x3850c7bd",
            format!(
                "0x{}{}{}{}{}{}{}",
                uint_word(1),
                uint_word(2),
                uint_word(3),
                uint_word(4),
                uint_word(5),
                uint_word(6),
                uint_word(1)
            ),
        );
    }

    fn token_balances(
        map: &mut HashMap<String, String>,
        pool: &str,
        token0: &str,
        token0_balance: u64,
        token1: &str,
        token1_balance: u64,
    ) {
        let balance_data = format!("0x70a08231{}{}", "0".repeat(24), pool.trim_start_matches("0x"));
        put_call(map, token0, &balance_data, format!("0x{}", uint_word(token0_balance)));
        put_call(map, token1, &balance_data, format!("0x{}", uint_word(token1_balance)));
    }

    async fn fixture_server(responses: HashMap<String, String>) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let responses = Arc::new(responses);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let responses = Arc::clone(&responses);
                tokio::spawn(async move {
                    let _ = serve_request(stream, responses).await;
                });
            }
        });
        (format!("http://{address}"), task)
    }

    async fn serve_request(
        mut stream: TcpStream,
        responses: Arc<HashMap<String, String>>,
    ) -> std::io::Result<()> {
        let body = read_http_body(&mut stream).await?;
        let request: Value = serde_json::from_slice(&body).unwrap();
        let id = request.get("id").cloned().unwrap_or(json!(1));
        let key = request_key(&request);
        let selector = request_selector_key(&request);
        let result = responses.get(&key).or_else(|| responses.get(&selector)).cloned();
        let response = match result {
            Some(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            None => json!({
                "jsonrpc":"2.0",
                "id":id,
                "error":{"code":-32000,"message":"fixture miss"}
            }),
        };
        let response = serde_json::to_vec(&response).unwrap();
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            response.len()
        );
        stream.write_all(header.as_bytes()).await?;
        stream.write_all(&response).await?;
        Ok(())
    }

    async fn read_http_body(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 4096];
        let (body_start, body_length) = loop {
            let count = stream.read(&mut chunk).await?;
            if count == 0 {
                return Ok(Vec::new());
            }
            bytes.extend_from_slice(&chunk[..count]);
            let Some(headers_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let body_start = headers_end + 4;
            let headers = String::from_utf8_lossy(&bytes[..headers_end]);
            let body_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().ok())
                })
                .flatten()
                .unwrap_or(0);
            break (body_start, body_length);
        };
        while bytes.len() < body_start + body_length {
            let count = stream.read(&mut chunk).await?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        Ok(bytes[body_start..body_start + body_length].to_vec())
    }

    fn request_key(request: &Value) -> String {
        match request.get("method").and_then(Value::as_str) {
            Some("eth_call") => {
                let params = request["params"][0].clone();
                let data = params
                    .get("data")
                    .or_else(|| params.get("input"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                call_key(params["to"].as_str().unwrap_or_default(), data)
            }
            Some("eth_getCode") => get_code_key(request["params"][0].as_str().unwrap_or_default()),
            _ => String::new(),
        }
    }

    fn request_selector_key(request: &Value) -> String {
        if request.get("method").and_then(Value::as_str) != Some("eth_call") {
            return String::new();
        }
        let params = request["params"][0].clone();
        let data = params
            .get("data")
            .or_else(|| params.get("input"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        selector_key(params["to"].as_str().unwrap_or_default(), &data[..data.len().min(10)])
    }

    async fn reader_for(
        responses: HashMap<String, String>,
        chain: Chain,
    ) -> (EvmReader, JoinHandle<()>) {
        let (url, task) = fixture_server(responses).await;
        let reader = EvmReader::new(chain, &url).unwrap();
        (reader, task)
    }

    #[tokio::test]
    async fn decodes_v2_pool_and_quote_candidate() {
        let mut map = responses();
        standard_v2_pool(&mut map, POOL, TOKEN0, TOKEN1);
        token_balances(&mut map, POOL, TOKEN0, 41, TOKEN1, 99);
        let (reader, task) = reader_for(map, Chain::Base).await;
        let pool = reader.read_pool_with_quotes(POOL, &[TOKEN0.to_owned()]).await.unwrap();
        task.abort();

        assert_eq!(pool.dex, "uniswap-v2");
        assert_eq!(pool.quote.address, TOKEN0);
        assert_eq!(pool.base.address, TOKEN1);
        assert_eq!(pool.quote.balance.as_deref(), Some("41"));
        assert_eq!(pool.base.balance.as_deref(), Some("99"));
    }

    #[tokio::test]
    async fn decodes_v3_pool() {
        let pool = "0x0000000000000000000000000000000000000033";
        let mut map = responses();
        standard_v3_pool(&mut map, pool, TOKEN0, TOKEN1);
        token_balances(&mut map, pool, TOKEN0, 100, TOKEN1, 200);
        let (reader, task) = reader_for(map, Chain::Ethereum).await;
        let decoded = reader.read_pool(pool).await.unwrap();
        task.abort();

        assert_eq!(decoded.dex, "uniswap-v3");
        assert_eq!(decoded.base.address, TOKEN0);
        assert_eq!(decoded.quote.address, TOKEN1);
        assert_eq!(decoded.quote.balance.as_deref(), Some("200"));
    }

    #[tokio::test]
    async fn reads_token_metadata() {
        let token = "0x0000000000000000000000000000000000000044";
        let mut map = responses();
        put_call(&mut map, token, "0x95d89b41", string_result("TEST"));
        put_call(&mut map, token, "0x06fdde03", string_result("Demo Token"));
        put_call(&mut map, token, "0x313ce567", format!("0x{}", uint_word(6)));
        put_call(&mut map, token, "0x18160ddd", format!("0x{}", uint_word(123_456)));
        let (reader, task) = reader_for(map, Chain::Base).await;
        let metadata = reader.token_meta(token).await.unwrap();
        task.abort();

        assert_eq!(metadata.address, token);
        assert_eq!(metadata.symbol.as_deref(), Some("TEST"));
        assert_eq!(metadata.name.as_deref(), Some("Demo Token"));
        assert_eq!(metadata.decimals, Some(6));
        assert_eq!(metadata.total_supply.as_deref(), Some("123456"));
    }

    #[tokio::test]
    async fn orders_pools_by_quote_balance() {
        let token = "0x00000000000000000000000000000000000000aa";
        let quote = "0x00000000000000000000000000000000000000bb";
        let pool1 = "0x00000000000000000000000000000000000000c1";
        let pool2 = "0x00000000000000000000000000000000000000c2";
        let mut map = responses();
        let base_factory = "0x8909dc15e40173ff4699343b6eb8132c65e18ec6";
        put_selector(&mut map, base_factory, "0xe6a43905", address_word(pool1));
        let uni_factory = "0x33128a8fc17869897dce68ed026d694621f6fdfd";
        put_selector(&mut map, uni_factory, "0x1698ee82", address_word(pool2));
        standard_v2_pool(&mut map, pool1, token, quote);
        token_balances(&mut map, pool1, token, 10, quote, 20);
        standard_v3_pool(&mut map, pool2, token, quote);
        token_balances(&mut map, pool2, token, 10, quote, 200);

        let (reader, task) = reader_for(map, Chain::Base).await;
        let pools = reader.pools_for_token(token, &[quote.to_owned()]).await.unwrap();
        task.abort();

        assert_eq!(pools.len(), 2);
        assert_eq!(pools[0].pool, canonical(pool2.parse().unwrap()));
        assert_eq!(pools[0].quote.balance.as_deref(), Some("200"));
        assert_eq!(pools[1].quote.balance.as_deref(), Some("20"));
    }

    #[tokio::test]
    async fn distinguishes_empty_and_non_empty_code() {
        let deployed = "0x0000000000000000000000000000000000000055";
        let absent = "0x0000000000000000000000000000000000000056";
        let mut map = responses();
        map.insert(get_code_key(deployed), "0x6001600055".to_owned());
        map.insert(get_code_key(absent), "0x".to_owned());
        let (reader, task) = reader_for(map, Chain::RobinhoodChain).await;
        let deployed_code = reader.code_at(deployed).await.unwrap();
        let absent_code = reader.code_at(absent).await.unwrap();
        task.abort();

        assert!(!deployed_code.is_empty());
        assert!(absent_code.is_empty());
    }
    #[test]
    fn decodes_recorded_v4_initialize_fixture() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/evm/robinhood-fami-jinqian-v4.json"
        ))
        .unwrap();
        let log = &fixture["get_logs_response"]["result"][0];
        let topics = log["topics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|topic| B256::from_str(topic.as_str().unwrap()).unwrap())
            .collect::<Vec<_>>();
        let data =
            alloy::hex::decode(log["data"].as_str().unwrap().trim_start_matches("0x")).unwrap();
        let decoded = Initialize::decode_raw_log_validate(topics, &data).unwrap();

        assert_eq!(decoded.id, B256::from_str(fixture["pool_id"].as_str().unwrap()).unwrap());
        assert_eq!(decoded.currency0, address!("5d2e81cb3a6fece856b824dfd7e1d6d3dbad8cd9"));
        assert_eq!(decoded.currency1, address!("e81880c1c5054245e036359f5c7be31606e79f56"));
        assert_eq!(decoded.fee.to::<u32>(), 3_000);
        assert_eq!(decoded.tickSpacing.as_i32(), 60);
        assert_eq!(decoded.hooks, address!("d88dcacf4e77b7ff2778aee824639d30cb1640cc"));
        assert_eq!(
            U256::from(decoded.sqrtPriceX96),
            U256::from_str("952611805490758559858168847769").unwrap()
        );
        assert_eq!(decoded.tick.as_i32(), 49_740);
    }
}
