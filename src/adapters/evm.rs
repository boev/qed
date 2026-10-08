use crate::adapters::state::RpcRateLimiter;
use crate::domain::chain::Chain;
use crate::domain::pool::{
    PoolError, PoolInfo, TokenMeta, TokenSide, WalletHolding, cap_token_text,
};
use crate::domain::registry::{self, Entry};
use crate::domain::statement::StatementHolding;
use crate::ports::ChainReader;
use alloy::{
    eips::BlockId,
    network::Ethereum,
    primitives::{
        Address, B256, FixedBytes, U256, address,
        aliases::{I24, U24},
    },
    providers::{DynProvider, Provider, ProviderBuilder},
    rpc::client::RpcClient,
    rpc::types::Filter,
    sol,
    sol_types::SolValue,
};
use alloy_json_rpc::{RequestPacket, ResponsePacket, ResponsePayload, RpcError};
use alloy_transport::{TransportError, TransportErrorKind, TransportFut};
use alloy_transport_http::reqwest::{Client as AlloyHttpClient, Url as AlloyUrl};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
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
    interface TokenPowerProbe {
        function paused() external view returns (bool value);
        function isPaused() external view returns (bool value);
        function owner() external view returns (address value);
        function pauser() external view returns (address value);
        function sanctionsList() external view returns (address value);
        function isBlacklisted(address wallet) external view returns (bool value);
        function isBlackListed(address wallet) external view returns (bool value);
        function implementation() external view returns (address value);
    }

    #[sol(rpc)]
    interface SanctionsList {
        function isSanctioned(address wallet) external view returns (bool value);
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
        function factory() external view returns (address value);
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
        function tickSpacing() external view returns (int24 value);
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
    interface RamsesV3Factory {
        function getPool(address tokenA, address tokenB, int24 tickSpacing)
            external
            view
            returns (address pool);
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
    #[sol(rpc)]
    interface PositionManager {
        function poolKeys(bytes25 poolId) external view returns (
            address currency0,
            address currency1,
            uint24 fee,
            int24 tickSpacing,
            address hooks
        );
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
    crate::ports::record_read("eth_call", params, &value, true, None, None);
}

fn raw_hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(2 + bytes.len() * 2);
    value.push_str("0x");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

fn address_word(value: &Address) -> String {
    format!("0x{:064x}", U256::from_be_slice(value.as_slice()))
}

// PublicNode returned this exact JSON-RPC message for the default Ethereum endpoint's `paused()` revert.
const PUBLICNODE_REVERT_MESSAGES: &[&str] = &["execution reverted"];

fn is_deterministic_revert_response(error: &alloy::contract::Error) -> bool {
    let alloy::contract::Error::TransportError(RpcError::ErrorResp(payload)) = error else {
        return false;
    };
    payload.code == 3
        || PUBLICNODE_REVERT_MESSAGES
            .iter()
            .any(|message| payload.message.eq_ignore_ascii_case(message))
}

fn record_power_call<T>(
    to: Address,
    calldata: &[u8],
    block: u64,
    method: &str,
    result: Result<T, alloy::contract::Error>,
    encode_result: impl FnOnce(&T) -> String,
) -> (Option<T>, Option<crate::domain::powers::Reason>) {
    let params = json!([
        {"to": canonical(to), "data": raw_hex(calldata)},
        format!("0x{block:x}")
    ]);
    match result {
        Ok(value) => {
            let raw_result = Value::String(encode_result(&value));
            crate::ports::record_read("eth_call", params, &raw_result, true, Some(block), None);
            (Some(value), None)
        }
        Err(error) => {
            let revert_data = error.as_revert_data();
            let absent = revert_data.is_some()
                || matches!(
                    &error,
                    alloy::contract::Error::UnknownFunction(_)
                        | alloy::contract::Error::UnknownSelector(_)
                        | alloy::contract::Error::ZeroData(_, _)
                        | alloy::contract::Error::AbiError(_)
                )
                || is_deterministic_revert_response(&error);
            let raw_result = if let Some(data) = revert_data {
                Value::String(raw_hex(&data))
            } else if absent {
                Value::String("0x".to_owned())
            } else {
                json!({"available": false})
            };
            crate::ports::record_read("eth_call", params, &raw_result, true, Some(block), None);
            let unavailable = (!absent).then(|| {
                crate::domain::powers::Reason::new(
                    contract_error_reason_code(&error),
                    format!("{method} read did not complete; this observation is unavailable."),
                )
            });
            (None, unavailable)
        }
    }
}

const MAX_POOL_DISCOVERY_CANDIDATES: usize = 128;
const MAX_POOL_DISCOVERY_OPERATIONS: usize = 512;
const POOL_DISCOVERY_DEADLINE: Duration = Duration::from_secs(5);
const EIP1967_IMPLEMENTATION_SLOT: &str =
    "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc";
const EIP1967_ADMIN_SLOT: &str =
    "0xb53127684a568b3173ae13b9f8a6016e243e63b6e8ee1178d6a717850b5d6103";
const EIP1967_BEACON_SLOT: &str =
    "0xa3f0ad74e5423aebfd80d3ef4346578335a9a72aeaee59ff6cb3582b35133d50";

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

// PancakeSwap deployments: https://developer.pancakeswap.finance/contracts/v3/addresses
const PANCAKE_V2_BNB: Address = address!("ca143ce32fe78f1f7019d7d551a6402fc5350c73");
const PANCAKE_V3_FACTORY: Address = address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865");
// Ramses' official deployment table and RobinScan-verified factory:
// https://www.ramses.xyz/docs/contract-addresses
const RAMSES_V3_ROBINHOOD: Address = address!("e0c4ceb92d08ca985bb70fe0a22feb121a9854a8");
// Uniswap v4 StateView and PositionManager deployments:
// https://developers.uniswap.org/docs/protocols/v4/deployments
const V4_STATEVIEW_ETHEREUM: Address = address!("7ffe42c4a5deea5b0fec41c94c136cf115597227");
const V4_STATEVIEW_BASE: Address = address!("a3c0c9b65bad0b08107aa264b0f3db444b867a71");
const V4_STATEVIEW_BNB: Address = address!("d13dd3d6e93f276fafc9db9e6bb47c1180aee0c4");
const V4_STATEVIEW_ROBINHOOD: Address = address!("f3334192d15450cdd385c8b70e03f9a6bd9e673b");
const V4_POSITION_MANAGER_ETHEREUM: Address = address!("bd216513d74c8cf14cf4747e6aaa6420ff64ee9e");
const V4_POSITION_MANAGER_BASE: Address = address!("7c5f5a4bbd8fd63184577525326123b519429bdc");
const V4_POSITION_MANAGER_BNB: Address = address!("7a4a5c919ae2541aed11041a1aeee68f1287f95b");
const V4_POSITION_MANAGER_ROBINHOOD: Address = address!("58daec3116aae6d93017baaea7749052e8a04fa7");
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
// https://bscscan.com/tx/0x64b395f1b0c3b734a477c802bc8cc3ce394f328c651290d0d166946048487bbe
const V4_DEPLOYMENT_BNB: u64 = 45_970_610;

#[derive(Clone, Copy)]
struct Deployments {
    uniswap_v2: Option<Address>,
    pancake_v2: Option<Address>,
    uniswap_v3: Option<Address>,
    pancake_v3: Option<Address>,
    ramses_v3: Option<Address>,
    v4_state_view: Option<Address>,
    v4_position_manager: Option<Address>,
    v4_pool_manager: Option<Address>,
    v4_deployment_block: u64,
}

fn deployments(chain: Chain) -> Deployments {
    match chain {
        Chain::RobinhoodChain => Deployments {
            uniswap_v2: Some(UNI_V2_ROBINHOOD),
            pancake_v2: None,
            uniswap_v3: Some(UNI_V3_ROBINHOOD),
            pancake_v3: None,
            ramses_v3: Some(RAMSES_V3_ROBINHOOD),
            v4_state_view: Some(V4_STATEVIEW_ROBINHOOD),
            v4_position_manager: Some(V4_POSITION_MANAGER_ROBINHOOD),
            v4_pool_manager: Some(V4_POOLMANAGER_ROBINHOOD),
            v4_deployment_block: V4_DEPLOYMENT_ROBINHOOD,
        },
        Chain::Base => Deployments {
            uniswap_v2: Some(UNI_V2_BASE),
            pancake_v2: None,
            uniswap_v3: Some(UNI_V3_BASE),
            pancake_v3: None,
            ramses_v3: None,
            v4_state_view: Some(V4_STATEVIEW_BASE),
            v4_position_manager: Some(V4_POSITION_MANAGER_BASE),
            v4_pool_manager: Some(V4_POOLMANAGER_BASE),
            v4_deployment_block: V4_DEPLOYMENT_BASE,
        },
        Chain::Ethereum => Deployments {
            uniswap_v2: Some(UNI_V2_ETHEREUM),
            pancake_v2: None,
            uniswap_v3: Some(UNI_V3_ETHEREUM),
            pancake_v3: None,
            ramses_v3: None,
            v4_state_view: Some(V4_STATEVIEW_ETHEREUM),
            v4_position_manager: Some(V4_POSITION_MANAGER_ETHEREUM),
            v4_pool_manager: Some(V4_POOLMANAGER_ETHEREUM),
            v4_deployment_block: V4_DEPLOYMENT_ETHEREUM,
        },
        Chain::Bnb => Deployments {
            uniswap_v2: Some(UNI_V2_BNB),
            pancake_v2: Some(PANCAKE_V2_BNB),
            uniswap_v3: Some(UNI_V3_BNB),
            pancake_v3: Some(PANCAKE_V3_FACTORY),
            ramses_v3: None,
            v4_state_view: Some(V4_STATEVIEW_BNB),
            v4_position_manager: Some(V4_POSITION_MANAGER_BNB),
            v4_pool_manager: Some(V4_POOLMANAGER_BNB),
            v4_deployment_block: V4_DEPLOYMENT_BNB,
        },
        Chain::Solana => Deployments {
            uniswap_v2: None,
            pancake_v2: None,
            uniswap_v3: None,
            pancake_v3: None,
            ramses_v3: None,
            v4_state_view: None,
            v4_position_manager: None,
            v4_pool_manager: None,
            v4_deployment_block: 0,
        },
    }
}

const RPC_MAX_ATTEMPTS: usize = 4;
const RPC_BACKOFF: [Duration; RPC_MAX_ATTEMPTS] =
    [Duration::from_millis(250), Duration::from_secs(1), Duration::from_secs(3), Duration::ZERO];
const STATEMENT_MULTICALL_BATCH: usize = 256;

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
                .is_some_and(|length| length as usize > crate::adapters::net::MAX_RESPONSE_BYTES)
            {
                return Err(TransportErrorKind::custom_str(
                    "RPC response exceeded the 4 MiB limit",
                ));
            }
            let mut response = response;
            while let Some(chunk) = response.chunk().await.map_err(TransportErrorKind::custom)? {
                if body.len() + chunk.len() > crate::adapters::net::MAX_RESPONSE_BYTES {
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

pub(crate) fn transient_reason_code(error: &TransportError) -> &'static str {
    if retryable_error(error) {
        return "rate_limited";
    }
    match error {
        RpcError::DeserError { .. } => "parse_error",
        RpcError::Transport(error)
            if error.as_http_error().is_some_and(|error| matches!(error.status, 408 | 504)) =>
        {
            "rpc_timeout"
        }
        RpcError::Transport(error)
            if error
                .as_custom()
                .and_then(|error| error.downcast_ref::<reqwest::Error>())
                .is_some_and(reqwest::Error::is_timeout) =>
        {
            "rpc_timeout"
        }
        _ => "rpc_unavailable",
    }
}

pub(crate) fn contract_error_reason_code(error: &alloy::contract::Error) -> &'static str {
    match error {
        alloy::contract::Error::TransportError(error) => transient_reason_code(error),
        _ => "rpc_unavailable",
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
    size <= crate::adapters::net::MAX_RESPONSE_BYTES
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
            let (successful, fallback_tokens) =
                classify_multicall_results(chunk, multicall.try_aggregate(false).await);
            balances.extend(successful);
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
            let symbol = cap_token_text(contract.symbol().call().await.ok());
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
    pub async fn statement_holdings(
        &self,
        owner: &str,
        entries: &[Entry],
        requested_block: Option<u64>,
    ) -> Result<(Vec<StatementHolding>, crate::domain::statement::StatementPosition), PoolError>
    {
        let owner = parse_address(owner)?;
        let block =
            match requested_block {
                Some(block) => block,
                None => self.provider.get_block_number().await.map_err(|error| {
                    PoolError::Reader(format!("reading statement block: {error}"))
                })?,
            };
        if requested_block.is_none() {
            let value = Value::String(format!("0x{block:x}"));
            crate::ports::record_read(
                "eth_blockNumber",
                json!([]),
                &value,
                false,
                Some(block),
                None,
            );
        }

        let mut tokens = Vec::new();
        let mut seen = HashSet::new();
        for entry in
            entries.iter().filter(|entry| entry.chain == self.chain && registry::matchable(entry))
        {
            if let Ok(token) = parse_address(&entry.contract)
                && seen.insert(token)
            {
                tokens.push(token);
            }
        }
        let mut balances = Vec::new();
        for chunk in tokens.chunks(STATEMENT_MULTICALL_BATCH) {
            let mut multicall = self.provider.multicall().dynamic();
            for token in chunk {
                multicall =
                    multicall.add_dynamic(Erc20::new(*token, &self.provider).balanceOf(owner));
            }
            let multicall = multicall.block(BlockId::number(block));
            let (mut successful, fallback_tokens) =
                classify_multicall_results(chunk, multicall.try_aggregate(false).await);
            for token in fallback_tokens {
                let balance = Erc20::new(token, &self.provider)
                    .balanceOf(owner)
                    .block(BlockId::number(block))
                    .call()
                    .await
                    .map_err(|error| {
                        PoolError::Reader(format!("statement balance read failed: {error}"))
                    })?;
                successful.push((token, balance));
            }
            balances.extend(successful);
        }

        let mut nonzero_balances = Vec::with_capacity(balances.len());
        for (token, balance) in balances {
            let result = Value::String(format!("0x{balance:064x}"));
            crate::ports::record_read(
                "eth_call",
                json!([canonical(token), "balanceOf", canonical(owner), format!("0x{block:x}")]),
                &result,
                true,
                Some(block),
                None,
            );
            if !balance.is_zero() {
                nonzero_balances.push((token, balance));
            }
        }

        let mut decimals_by_token = HashMap::with_capacity(nonzero_balances.len());
        for chunk in nonzero_balances.chunks(STATEMENT_MULTICALL_BATCH) {
            let tokens = chunk.iter().map(|(token, _)| *token).collect::<Vec<_>>();
            let mut multicall = self.provider.multicall().dynamic();
            for token in &tokens {
                multicall = multicall.add_dynamic(Erc20::new(*token, &self.provider).decimals());
            }
            let multicall = multicall.block(BlockId::number(block));
            let (mut successful, fallback_tokens) =
                classify_multicall_results(&tokens, multicall.try_aggregate(false).await);
            for token in fallback_tokens {
                let decimals = Erc20::new(token, &self.provider)
                    .decimals()
                    .block(BlockId::number(block))
                    .call()
                    .await
                    .map_err(|error| {
                        PoolError::Reader(format!("statement token decimals failed: {error}"))
                    })?;
                successful.push((token, decimals));
            }
            for (token, decimals) in successful {
                let calldata = Erc20::new(token, &self.provider).decimals().calldata().to_vec();
                crate::ports::record_read(
                    "eth_call",
                    json!([
                        canonical(token),
                        "decimals",
                        format!("0x{}", raw_hex(&calldata)),
                        format!("0x{block:x}")
                    ]),
                    &Value::from(decimals),
                    true,
                    Some(block),
                    None,
                );
                decimals_by_token.insert(token, decimals);
            }
        }

        let mut holdings = Vec::with_capacity(nonzero_balances.len());
        for (token, balance) in nonzero_balances {
            let decimals = decimals_by_token.remove(&token).ok_or_else(|| {
                PoolError::Reader("statement token decimals were not returned".to_owned())
            })?;
            holdings.push(StatementHolding {
                holding: WalletHolding {
                    chain: self.chain,
                    token_address: canonical(token),
                    symbol: None,
                    amount: balance.to_string(),
                    decimals: Some(decimals),
                },
                slot: None,
            });
        }
        Ok((
            holdings,
            crate::domain::statement::StatementPosition {
                chain: self.chain,
                wallet: canonical(owner),
                block: Some(block),
                min_slot: None,
                max_slot: None,
            },
        ))
    }

    pub async fn token_meta(&self, address: &str) -> Result<TokenMeta, PoolError> {
        let token = parse_address(address)?;
        let contract = Erc20::new(token, &self.provider);
        let symbol = cap_token_text(contract.symbol().call().await.ok());
        record_eth_call(json!([canonical(token), "symbol()"]), &symbol);
        let name = cap_token_text(contract.name().call().await.ok());
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

        let configured = deployments(self.chain);
        let pair = V2Pair::new(pool, &self.provider);
        let reserves = pair.getReserves().call().await;
        record_eth_call(json!([canonical(pool), "getReserves()"]), &reserves.is_ok());
        let v2_factory = if reserves.is_ok() {
            let factory =
                pair.factory().call().await.map_err(|error| {
                    PoolError::Reader(format!("reading v2 pool factory: {error}"))
                })?;
            record_eth_call(json!([canonical(pool), "factory()"]), &factory);
            if configured.uniswap_v2 == Some(factory) || configured.pancake_v2 == Some(factory) {
                Some(factory)
            } else {
                None
            }
        } else {
            None
        };
        let dex = if let Some(factory) = v2_factory {
            let registered = V2Factory::new(factory, &self.provider)
                .getPair(token0, token1)
                .call()
                .await
                .map_err(|error| {
                    PoolError::Reader(format!("checking v2 factory membership: {error}"))
                })?;
            record_eth_call(
                json!([canonical(factory), "getPair", canonical(token0), canonical(token1)]),
                &registered,
            );
            if registered != pool {
                return Err(PoolError::UnsupportedVenue(format!(
                    "pool {pool} is not registered by its V2 factory"
                )));
            }
            if configured.pancake_v2 == Some(factory) { "pancake-v2" } else { "uniswap-v2" }
        } else {
            let slot0 = contract.slot0().call().await;
            record_eth_call(json!([canonical(pool), "slot0()"]), &slot0.is_ok());
            if slot0.is_err() {
                return Err(PoolError::UnsupportedVenue(format!(
                    "pool {pool} does not use a supported V2 or V3 interface"
                )));
            }
            let factory =
                contract.factory().call().await.map_err(|error| {
                    PoolError::Reader(format!("reading V3 pool factory: {error}"))
                })?;
            record_eth_call(json!([canonical(pool), "factory()"]), &factory);
            if configured.ramses_v3 == Some(factory) {
                let tick_spacing = contract.tickSpacing().call().await.map_err(|error| {
                    PoolError::Reader(format!("reading Ramses V3 tick spacing: {error}"))
                })?;
                record_eth_call(json!([canonical(pool), "tickSpacing()"]), &tick_spacing);
                let registered = RamsesV3Factory::new(factory, &self.provider)
                    .getPool(token0, token1, tick_spacing)
                    .call()
                    .await
                    .map_err(|error| {
                        PoolError::Reader(format!("checking Ramses V3 factory membership: {error}"))
                    })?;
                record_eth_call(
                    json!([
                        canonical(factory),
                        "getPool",
                        canonical(token0),
                        canonical(token1),
                        tick_spacing
                    ]),
                    &registered,
                );
                if registered != pool {
                    return Err(PoolError::UnsupportedVenue(format!(
                        "pool {pool} is not registered by the configured Ramses V3 factory"
                    )));
                }
                "ramses-v3"
            } else {
                let (known_factory, dex) = if configured.uniswap_v3 == Some(factory) {
                    (factory, "uniswap-v3")
                } else if configured.pancake_v3 == Some(factory) {
                    (factory, "pancake-v3")
                } else {
                    return Err(PoolError::UnsupportedVenue(format!(
                        "pool {pool} is not from a supported V3 factory"
                    )));
                };
                let fee =
                    contract.fee().call().await.map_err(|error| {
                        PoolError::Reader(format!("reading V3 pool fee: {error}"))
                    })?;
                record_eth_call(json!([canonical(pool), "fee()"]), &fee);
                let registered = V3Factory::new(known_factory, &self.provider)
                    .getPool(token0, token1, fee)
                    .call()
                    .await
                    .map_err(|error| {
                        PoolError::Reader(format!("checking V3 factory membership: {error}"))
                    })?;
                record_eth_call(
                    json!([
                        canonical(known_factory),
                        "getPool",
                        canonical(token0),
                        canonical(token1),
                        fee
                    ]),
                    &registered,
                );
                if registered != pool {
                    return Err(PoolError::UnsupportedVenue(format!(
                        "pool {pool} is not registered by its V3 factory"
                    )));
                }
                dex
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
    fn is_log_range_limit_error(error: &impl std::fmt::Display) -> bool {
        let message = error.to_string().to_ascii_lowercase();
        message.contains("range")
            && [
                "limit",
                "maximum",
                "max ",
                "exceed",
                "too large",
                "too wide",
                "too many",
                "supported",
            ]
            .iter()
            .any(|marker| message.contains(marker))
    }
    async fn read_v4_pool_data(&self, pool_id: &str) -> Result<(PoolInfo, Vec<String>), PoolError> {
        let pool_id = B256::from_str(pool_id).map_err(|_| PoolError::InvalidAddress)?;
        let configured = deployments(self.chain);
        let state_view_address = configured
            .v4_state_view
            .ok_or_else(|| PoolError::Unknown("v4 StateView is unavailable".to_owned()))?;
        let position_manager_address = configured
            .v4_position_manager
            .ok_or_else(|| PoolError::Unknown("v4 PositionManager is unavailable".to_owned()))?;
        let pool_manager = configured
            .v4_pool_manager
            .ok_or_else(|| PoolError::Unknown("v4 PoolManager is unavailable".to_owned()))?;

        let pool_prefix = FixedBytes::<25>::from_slice(&pool_id.as_slice()[..25]);
        let pool_key = PositionManager::new(position_manager_address, &self.provider)
            .poolKeys(pool_prefix)
            .call()
            .await
            .map_err(|error| PoolError::Reader(format!("reading v4 pool key: {error}")))?;
        let pool_key_read = json!({
            "currency0": format!("{:?}", pool_key.currency0),
            "currency1": format!("{:?}", pool_key.currency1),
            "fee": format!("{:?}", pool_key.fee),
            "tickSpacing": format!("{:?}", pool_key.tickSpacing),
            "hooks": format!("{:?}", pool_key.hooks)
        });
        record_eth_call(
            json!([
                format!("{position_manager_address:?}"),
                "poolKeys",
                raw_hex(pool_prefix.as_slice())
            ]),
            &pool_key_read,
        );
        let pool_key_is_empty = pool_key.currency0 == Address::ZERO
            && pool_key.currency1 == Address::ZERO
            && pool_key.fee == U24::ZERO
            && pool_key.tickSpacing == I24::ZERO
            && pool_key.hooks == Address::ZERO;
        let resolved_key = if pool_key_is_empty {
            None
        } else {
            let resolved_hash = alloy::primitives::keccak256(
                (
                    pool_key.currency0,
                    pool_key.currency1,
                    pool_key.fee,
                    pool_key.tickSpacing,
                    pool_key.hooks,
                )
                    .abi_encode(),
            );
            if resolved_hash != pool_id {
                return Err(PoolError::Unknown(format!("v4 pool key hash mismatch for {pool_id}")));
            }
            Some((
                pool_key.currency0,
                pool_key.currency1,
                pool_key.fee,
                pool_key.tickSpacing,
                pool_key.hooks,
            ))
        };

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

        let (currency0, currency1, fee, tick_spacing, hooks, key_source) = match resolved_key {
            Some((currency0, currency1, fee, tick_spacing, hooks)) => {
                (currency0, currency1, fee, tick_spacing, hooks, "PositionManager.poolKeys")
            }
            None => {
                const MAX_LOG_WINDOWS: usize = 20;
                const SCAN_DEADLINE: Duration = Duration::from_secs(10);
                let filter = Filter::new()
                    .address(pool_manager)
                    .event("Initialize(bytes32,address,address,uint24,int24,address,uint160,int24)")
                    .topic1(pool_id);
                let log_chunk: u64 = if self.chain == Chain::RobinhoodChain { 10 } else { 1_000 };
                let event = tokio::time::timeout(SCAN_DEADLINE, async {
                    let latest_block = self.provider.get_block_number().await.map_err(|error| {
                        PoolError::Reader(format!("reading latest block for v4 logs: {error}"))
                    })?;
                    let latest_value = Value::String(format!("0x{latest_block:x}"));
                    crate::ports::record_read(
                        "eth_blockNumber",
                        json!([]),
                        &latest_value,
                        false,
                        Some(latest_block),
                        None,
                    );
                    let max_span = log_chunk.saturating_mul(MAX_LOG_WINDOWS as u64);
                    let mut from_block = latest_block
                        .saturating_sub(max_span.saturating_sub(1))
                        .max(configured.v4_deployment_block);
                    for _ in 0..MAX_LOG_WINDOWS {
                        if from_block > latest_block {
                            break;
                        }
                        let to_block = from_block.saturating_add(log_chunk - 1).min(latest_block);
                        let logs = self
                            .provider
                            .get_logs(&filter.clone().from_block(from_block).to_block(to_block))
                            .await
                            .map_err(|error| {
                                if Self::is_log_range_limit_error(&error) {
                                    PoolError::RpcLimit("provider rejected v4 Initialize log range")
                                } else {
                                    PoolError::Reader(format!(
                                        "reading v4 Initialize logs: {error}"
                                    ))
                                }
                            })?;
                        record_eth_call(
                            json!([
                                format!("{pool_manager:?}"),
                                "eth_getLogs",
                                from_block,
                                to_block
                            ]),
                            &logs,
                        );
                        if let Some(event) = logs.iter().find_map(|log| {
                            log.log_decode::<Initialize>()
                                .ok()
                                .map(|decoded| decoded.data().clone())
                        }) {
                            return Ok::<_, PoolError>(Some(event));
                        }
                        if to_block == latest_block {
                            break;
                        }
                        from_block = to_block.saturating_add(1);
                    }
                    Ok(None)
                })
                .await
                .map_err(|_| PoolError::RpcLimit("v4 Initialize scan deadline exceeded"))??;
                let event = event.ok_or(PoolError::RpcLimit(
                    "v4 Initialize event is outside the bounded recent scan window",
                ))?;
                (
                    event.currency0,
                    event.currency1,
                    event.fee,
                    event.tickSpacing,
                    event.hooks,
                    "bounded recent Initialize log",
                )
            }
        };

        let q96 = U256::from(1_u8) << 96;
        let liquidity = U256::from(liquidity);
        let sqrt_price = U256::from(slot0.sqrtPriceX96);
        let amount0 = if sqrt_price == 0 { U256::ZERO } else { liquidity * q96 / sqrt_price };
        let amount1 = liquidity * sqrt_price / q96;
        let token0 = self.token_side_with_balance(currency0, amount0).await;
        let token1 = self.token_side_with_balance(currency1, amount1).await;
        let pool = PoolInfo {
            chain: self.chain,
            pool: pool_id.to_string(),
            dex: "uniswap-v4".to_owned(),
            base: token0,
            quote: token1,
        };
        let evidence = vec![
            format!("Uniswap v4 pool key source: {key_source}."),
            format!("Uniswap v4 pool key resolved currencies {currency0} and {currency1}."),
            format!(
                "Uniswap v4 fee {fee} (tick spacing {tick_spacing}) and hooks address {hooks}."
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
        let symbol = cap_token_text(contract.symbol().call().await.ok());
        record_eth_call(json!([canonical(token), "symbol()"]), &symbol);
        let decimals = contract.decimals().call().await.ok();
        record_eth_call(json!([canonical(token), "decimals()"]), &decimals);
        let balance = contract.balanceOf(pool).call().await.ok().map(|value| value.to_string());
        record_eth_call(json!([canonical(token), "balanceOf", canonical(pool)]), &balance);
        TokenSide { address: canonical(token), symbol, decimals, balance }
    }
    async fn token_side_with_balance(&self, token: Address, balance: U256) -> TokenSide {
        let contract = Erc20::new(token, &self.provider);
        let symbol = cap_token_text(contract.symbol().call().await.ok());
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
    async fn power_storage_address(
        &self,
        contract: Address,
        slot: &str,
        block_id: BlockId,
        block: u64,
    ) -> (Option<String>, Option<crate::domain::powers::Reason>) {
        let key = U256::from_str_radix(slot.trim_start_matches("0x"), 16)
            .expect("the EIP-1967 slot constants are valid");
        let params = json!([canonical(contract), slot, format!("0x{block:x}")]);
        let value = match self.provider.get_storage_at(contract, key).block_id(block_id).await {
            Ok(value) => value,
            Err(error) => {
                crate::ports::record_read(
                    "eth_getStorageAt",
                    params,
                    &json!({"available": false}),
                    true,
                    Some(block),
                    None,
                );
                return (
                    None,
                    Some(crate::domain::powers::Reason::new(
                        transient_reason_code(&error),
                        "EIP-1967 storage read did not complete; this observation is unavailable.",
                    )),
                );
            }
        };
        let raw_value = Value::String(format!("0x{value:064x}"));
        crate::ports::record_read("eth_getStorageAt", params, &raw_value, true, Some(block), None);
        let bytes = value.to_be_bytes::<32>();
        let address = Address::from_slice(&bytes[12..]);
        ((address != Address::ZERO).then(|| canonical(address)), None)
    }
}
#[async_trait]
impl ChainReader for EvmReader {
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
    async fn statement_holdings(
        &self,
        owner: &str,
        entries: &[Entry],
        block: Option<u64>,
    ) -> Result<(Vec<StatementHolding>, crate::domain::statement::StatementPosition), PoolError>
    {
        EvmReader::statement_holdings(self, owner, entries, block).await
    }
    async fn power_facts(
        &self,
        address: &str,
    ) -> Result<crate::domain::powers::PowerFacts, PoolError> {
        let contract = parse_address(address)?;
        let block = self.provider.get_block_number().await.map_err(|error| {
            PoolError::Reader(format!("reading current block number failed: {error}"))
        })?;
        let block_result = Value::String(format!("0x{block:x}"));
        crate::ports::record_read(
            "eth_blockNumber",
            json!([]),
            &block_result,
            false,
            Some(block),
            None,
        );
        let block_id = BlockId::number(block);
        let mut unavailable = Vec::new();
        let (implementation, issue) = self
            .power_storage_address(contract, EIP1967_IMPLEMENTATION_SLOT, block_id, block)
            .await;
        unavailable.extend(issue);
        let (admin, issue) =
            self.power_storage_address(contract, EIP1967_ADMIN_SLOT, block_id, block).await;
        unavailable.extend(issue);
        let (beacon, issue) =
            self.power_storage_address(contract, EIP1967_BEACON_SLOT, block_id, block).await;
        unavailable.extend(issue);

        let probe = TokenPowerProbe::new(contract, &self.provider);
        let call = probe.paused();
        let calldata = call.calldata().to_vec();
        let (paused, issue) = record_power_call(
            contract,
            &calldata,
            block,
            "paused()",
            call.block(block_id).call().await,
            |value| format!("0x{:064x}", u8::from(*value)),
        );
        unavailable.extend(issue);

        let call = probe.isPaused();
        let calldata = call.calldata().to_vec();
        let (is_paused, issue) = record_power_call(
            contract,
            &calldata,
            block,
            "isPaused()",
            call.block(block_id).call().await,
            |value| format!("0x{:064x}", u8::from(*value)),
        );
        unavailable.extend(issue);

        let call = probe.owner();
        let calldata = call.calldata().to_vec();
        let (owner_value, issue) = record_power_call(
            contract,
            &calldata,
            block,
            "owner()",
            call.block(block_id).call().await,
            address_word,
        );
        unavailable.extend(issue);

        let call = probe.pauser();
        let calldata = call.calldata().to_vec();
        let (pauser_value, issue) = record_power_call(
            contract,
            &calldata,
            block,
            "pauser()",
            call.block(block_id).call().await,
            address_word,
        );
        unavailable.extend(issue);

        let call = probe.sanctionsList();
        let calldata = call.calldata().to_vec();
        let (sanctions_list_value, issue) = record_power_call(
            contract,
            &calldata,
            block,
            "sanctionsList()",
            call.block(block_id).call().await,
            address_word,
        );
        unavailable.extend(issue);
        let call = probe.isBlacklisted(Address::ZERO);
        let calldata = call.calldata().to_vec();
        let (blacklisted_probe, issue) = record_power_call(
            contract,
            &calldata,
            block,
            "isBlacklisted(address)",
            call.block(block_id).call().await,
            |value| format!("0x{:064x}", u8::from(*value)),
        );
        unavailable.extend(issue);

        let call = probe.isBlackListed(Address::ZERO);
        let calldata = call.calldata().to_vec();
        let (blacklisted_legacy_probe, issue) = record_power_call(
            contract,
            &calldata,
            block,
            "isBlackListed(address)",
            call.block(block_id).call().await,
            |value| format!("0x{:064x}", u8::from(*value)),
        );
        unavailable.extend(issue);
        let wallet_blacklist_supported =
            blacklisted_probe.is_some() || blacklisted_legacy_probe.is_some();

        let beacon_implementation = if let Some(beacon) = beacon.as_deref() {
            let beacon = parse_address(beacon)?;
            let beacon_probe = TokenPowerProbe::new(beacon, &self.provider);
            let call = beacon_probe.implementation();
            let calldata = call.calldata().to_vec();
            let (implementation, issue) = record_power_call(
                beacon,
                &calldata,
                block,
                "beacon.implementation()",
                call.block(block_id).call().await,
                address_word,
            );
            unavailable.extend(issue);
            implementation.filter(|implementation| *implementation != Address::ZERO).map(canonical)
        } else {
            None
        };

        let mut facts =
            crate::domain::powers::evm::analyze(crate::domain::powers::evm::ProbeSnapshot {
                implementation,
                admin,
                beacon,
                beacon_implementation,
                paused,
                is_paused,
                owner: owner_value.filter(|owner| *owner != Address::ZERO).map(canonical),
                pauser: pauser_value.filter(|pauser| *pauser != Address::ZERO).map(canonical),
                sanctions_list: sanctions_list_value
                    .filter(|list| *list != Address::ZERO)
                    .map(canonical),
                unavailable: Vec::new(),
            });
        if wallet_blacklist_supported {
            facts.can_block.push(crate::domain::powers::Reason::new(
                "wallet_blacklist",
                "The token exposes a wallet blacklist query; an optional wallet is checked separately.",
            ));
        }
        facts.transient_failure = !unavailable.is_empty();
        facts.unavailable = unavailable;
        Ok(facts)
    }

    async fn wallet_restrictions(
        &self,
        contract: &str,
        wallet: &str,
        sanctions_list: Option<&str>,
    ) -> Result<crate::ports::WalletRestrictionReport, PoolError> {
        let contract = parse_address(contract)?;
        let wallet = parse_address(wallet)?;
        let block = self.provider.get_block_number().await.map_err(|error| {
            PoolError::Reader(format!("reading current block number failed: {error}"))
        })?;
        crate::ports::record_read(
            "eth_blockNumber",
            json!([]),
            &Value::String(format!("0x{block:x}")),
            false,
            Some(block),
            None,
        );
        let probe = TokenPowerProbe::new(contract, &self.provider);
        let call = probe.isBlacklisted(wallet);
        let calldata = call.calldata().to_vec();
        let (blacklisted, issue) = record_power_call(
            contract,
            &calldata,
            block,
            "isBlacklisted(address)",
            call.block(BlockId::number(block)).call().await,
            |value| format!("0x{:064x}", u8::from(*value)),
        );
        let mut complete = issue.is_none();

        let call = probe.isBlackListed(wallet);
        let calldata = call.calldata().to_vec();
        let (legacy_blacklisted, issue) = record_power_call(
            contract,
            &calldata,
            block,
            "isBlackListed(address)",
            call.block(BlockId::number(block)).call().await,
            |value| format!("0x{:064x}", u8::from(*value)),
        );
        complete &= issue.is_none();
        let mut wallet_probe_available = blacklisted.is_some() || legacy_blacklisted.is_some();

        let blocked_methods =
            [("isBlacklisted(wallet)", blacklisted), ("isBlackListed(wallet)", legacy_blacklisted)]
                .into_iter()
                .filter_map(|(method, blocked)| {
                    blocked.is_some_and(|blocked| blocked).then_some(method)
                })
                .collect::<Vec<_>>();
        let mut restrictions = Vec::with_capacity(2);
        if !blocked_methods.is_empty() {
            restrictions.push(crate::domain::powers::Reason::new(
                "wallet_blocked",
                format!("{} returned true at the observed block.", blocked_methods.join(" and ")),
            ));
        }
        if let Some(sanctions_list) = sanctions_list {
            let sanctions_list = parse_address(sanctions_list)?;
            let oracle = SanctionsList::new(sanctions_list, &self.provider);
            let call = oracle.isSanctioned(wallet);
            let calldata = call.calldata().to_vec();
            let (sanctioned, issue) = record_power_call(
                sanctions_list,
                &calldata,
                block,
                "isSanctioned(address)",
                call.block(BlockId::number(block)).call().await,
                |value| format!("0x{:064x}", u8::from(*value)),
            );
            complete &= issue.is_none() && sanctioned.is_some();
            wallet_probe_available = true;
            if sanctioned == Some(true) {
                restrictions.push(crate::domain::powers::Reason::new(
                    "wallet_sanctioned",
                    "The token's configured sanctions oracle returned true for this wallet.",
                ));
            }
        }
        Ok(crate::ports::WalletRestrictionReport {
            restrictions,
            complete,
            applicable: wallet_probe_available || sanctions_list.is_some(),
        })
    }

    async fn code_at(&self, address: &str) -> Result<Vec<u8>, PoolError> {
        let address = parse_address(address)?;
        let code =
            self.provider.get_code_at(address).await.map_err(|error| {
                PoolError::Reader(format!("reading contract bytecode: {error}"))
            })?;
        let encoded = code.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let value = Value::String(format!("0x{encoded}"));
        crate::ports::record_read(
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
        crate::ports::record_read("eth_blockNumber", json!([]), &value, false, Some(block), None);
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

fn classify_multicall_results<T: Copy, R, E, F>(
    requested: &[T],
    response: Result<Vec<Result<R, E>>, F>,
) -> (Vec<(T, R)>, Vec<T>) {
    let Ok(results) = response else {
        return (Vec::new(), requested.to_vec());
    };
    if results.len() != requested.len() {
        return (Vec::new(), requested.to_vec());
    }
    let mut successful = Vec::with_capacity(results.len());
    let mut failed = Vec::new();
    for (token, result) in requested.iter().copied().zip(results) {
        match result {
            Ok(value) => successful.push((token, value)),
            Err(_) => failed.push(token),
        }
    }
    (successful, failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::{
        sol,
        sol_types::{SolCall, SolEvent},
    };
    use serde_json::{Value, json};
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };
    sol! {
        interface TestMulticall3 {
            struct Call {
                address target;
                bytes callData;
            }
            struct Result {
                bool success;
                bytes returnData;
            }
            function tryAggregate(bool requireSuccess, Call[] calldata calls)
                external returns (Result[] memory returnData);
        }
    }
    const MULTICALL3_ADDRESS: &str = "0xca11bde05977b3631167028862be2a173976ca11";

    const TOKEN0: &str = "0x0000000000000000000000000000000000000011";
    const TOKEN1: &str = "0x0000000000000000000000000000000000000012";
    const POOL: &str = "0x0000000000000000000000000000000000000022";
    fn registry_entry(
        chain: Chain,
        contract: &str,
        ticker: &str,
    ) -> crate::domain::registry::Entry {
        crate::domain::registry::Entry {
            issuer: "Test issuer".to_owned(),
            ticker: ticker.to_owned(),
            name: ticker.to_owned(),
            chain,
            contract: contract.to_owned(),
            decimals: None,
            source: "test".to_owned(),
            source_url: "https://issuer.example".to_owned(),
            last_checked: "2026-10-04T00:00:00Z".to_owned(),
            removed_at: None,
            stale_since: None,
            official_deployments: Vec::new(),
        }
    }
    #[test]
    fn multicall_incomplete_result_counts_fall_back_for_every_token() {
        let requested = [1_u8, 2];
        for returned_count in [0, 1, 3] {
            let results = (0..returned_count).map(|value| Ok::<_, ()>(value)).collect();
            let (successful, fallback) =
                classify_multicall_results(&requested, Ok::<_, ()>(results));
            assert!(successful.is_empty());
            assert_eq!(fallback, requested);
        }

        let (successful, fallback) =
            classify_multicall_results(&requested, Ok::<_, ()>(vec![Ok::<_, ()>(10), Err(())]));
        assert_eq!(successful, vec![(1, 10)]);
        assert_eq!(fallback, vec![2]);

        let (successful, fallback) = classify_multicall_results(
            &requested,
            Err::<Vec<Result<u8, ()>>, _>("aggregate failure"),
        );
        assert!(successful.is_empty());
        assert_eq!(fallback, requested);
    }

    #[tokio::test]
    async fn evm_statement_reader_keeps_registered_balance_at_requested_block() {
        let owner = "0x0000000000000000000000000000000000000001";
        let requested_block = 42;
        let mut map = responses();
        let balance_data =
            format!("0x70a08231{}{}", "0".repeat(24), owner.trim_start_matches("0x"));
        put_call(&mut map, TOKEN0, &balance_data, format!("0x{}", uint_word(123_456)));
        put_call(&mut map, TOKEN0, "0x313ce567", format!("0x{}", uint_word(6)));
        let (reader, task) = reader_for(map, Chain::Base).await;

        let entries = [registry_entry(Chain::Base, TOKEN0, "NVDA")];
        let (holdings, position) = reader
            .statement_holdings(owner, &entries, Some(requested_block))
            .await
            .expect("statement holdings");
        task.abort();

        assert_eq!(holdings.len(), 1);
        assert_eq!(holdings[0].holding.chain, Chain::Base);
        assert_eq!(holdings[0].holding.token_address, TOKEN0);
        assert_eq!(holdings[0].holding.amount, "123456");
        assert_eq!(holdings[0].holding.decimals, Some(6));
        assert_eq!(holdings[0].slot, None);
        assert_eq!(position.block, Some(requested_block));
        assert_eq!(position.min_slot, None);
        assert_eq!(position.max_slot, None);
    }
    #[tokio::test]
    async fn evm_statement_batches_balances_and_decimals_for_large_registry() {
        let owner = "0x0000000000000000000000000000000000000001";
        let requested_block = 42;
        let mut responses = responses();
        let mut entries = Vec::with_capacity(300);
        for index in 0..300 {
            let token_index = index + 1;
            let token = format!("0x{token_index:040x}");
            let balance_data =
                format!("0x70a08231{}{}", "0".repeat(24), owner.trim_start_matches("0x"));
            put_call(
                &mut responses,
                &token,
                &balance_data,
                format!("0x{}", uint_word(token_index as u64)),
            );
            put_call(
                &mut responses,
                &token,
                "0x313ce567",
                format!("0x{}", uint_word((index % 18) as u64)),
            );
            entries.push(registry_entry(Chain::Base, &token, &format!("T{index}")));
        }
        let (url, task, requests) = fixture_server_with_trace(responses).await;
        let reader = EvmReader::new(Chain::Base, &url).unwrap();

        let (holdings, position) = reader
            .statement_holdings(owner, &entries, Some(requested_block))
            .await
            .expect("large statement holdings");
        let requests = requests.lock().expect("recorded requests").clone();
        task.abort();

        assert_eq!(holdings.len(), 300);
        assert_eq!(position.block, Some(requested_block));
        let by_token = holdings
            .into_iter()
            .map(|holding| {
                (
                    holding.holding.token_address.to_ascii_lowercase(),
                    (holding.holding.amount, holding.holding.decimals),
                )
            })
            .collect::<HashMap<_, _>>();
        for index in 0..300 {
            let token_index = index + 1;
            let token = format!("0x{token_index:040x}");
            let expected = (token_index.to_string(), Some((index % 18) as u8));
            assert_eq!(by_token.get(&token), Some(&expected));
        }

        let multicall_requests =
            requests.iter().filter(|request| request["method"] == "eth_call").collect::<Vec<_>>();
        assert_eq!(requests.len(), 4);
        assert_eq!(multicall_requests.len(), 4);
        let mut balance_calls = 0;
        let mut decimals_calls = 0;
        for request in multicall_requests {
            assert_eq!(request["params"][0]["to"], MULTICALL3_ADDRESS);
            assert_eq!(request["params"][1], format!("0x{requested_block:x}"));
            let calldata = request["params"][0]
                .get("input")
                .or_else(|| request["params"][0].get("data"))
                .and_then(Value::as_str)
                .expect("multicall calldata");
            let calldata = alloy::hex::decode(calldata.trim_start_matches("0x"))
                .expect("multicall input encoding");
            let decoded = TestMulticall3::tryAggregateCall::abi_decode(&calldata)
                .expect("tryAggregate request");
            assert!(decoded.calls.len() <= STATEMENT_MULTICALL_BATCH);
            for call in decoded.calls {
                let selector = &call.callData.as_ref()[..4];
                if selector == [0x70, 0xa0, 0x82, 0x31] {
                    balance_calls += 1;
                } else if selector == [0x31, 0x3c, 0xe5, 0x67] {
                    decimals_calls += 1;
                } else {
                    panic!("unexpected multicall selector");
                }
            }
        }
        assert_eq!(balance_calls, 300);
        assert_eq!(decimals_calls, 300);
    }

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

    fn storage_key(address: &str, slot: &str) -> String {
        format!("storage:{}:{}", address.to_ascii_lowercase(), slot.to_ascii_lowercase())
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

    const RPC_ERROR_FIXTURE_PREFIX: &str = "__rpc_error__:";
    const TRANSPORT_FAILURE_FIXTURE: &str = "__transport_failure__";

    fn rpc_error_fixture(code: i64, message: &str, data: Option<&str>) -> String {
        let mut error = json!({"code": code, "message": message});
        if let Some(data) = data {
            error["data"] = json!(data);
        }
        format!("{RPC_ERROR_FIXTURE_PREFIX}{error}")
    }

    fn standard_v2_pool(map: &mut HashMap<String, String>, pool: &str, token0: &str, token1: &str) {
        standard_v2_pool_from_factory(
            map,
            pool,
            token0,
            token1,
            "0x8909dc15e40173ff4699343b6eb8132c65e18ec6",
        );
    }

    fn standard_v2_pool_from_factory(
        map: &mut HashMap<String, String>,
        pool: &str,
        token0: &str,
        token1: &str,
        factory: &str,
    ) {
        put_call(map, pool, "0x0dfe1681", address_word(token0));
        put_call(map, pool, "0xd21220a7", address_word(token1));
        put_call(
            map,
            pool,
            "0x0902f1ac",
            format!("0x{}{}{}", uint_word(1_000), uint_word(2_000), uint_word(3)),
        );
        put_call(map, pool, &function_selector("factory()"), address_word(factory));
        put_selector(map, factory, "0xe6a43905", address_word(pool));
    }

    fn standard_v3_pool(
        map: &mut HashMap<String, String>,
        pool: &str,
        token0: &str,
        token1: &str,
        factory: &str,
    ) {
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
        put_call(map, pool, &function_selector("factory()"), address_word(factory));
        put_call(map, pool, &function_selector("fee()"), format!("0x{}", uint_word(3_000)));
        put_selector(map, factory, "0x1698ee82", address_word(pool));
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
        let (url, task) = fixture_server_inner(responses, None).await;
        (url, task)
    }

    async fn fixture_server_with_trace(
        responses: HashMap<String, String>,
    ) -> (String, JoinHandle<()>, Arc<Mutex<Vec<Value>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (url, task) = fixture_server_inner(responses, Some(Arc::clone(&requests))).await;
        (url, task, requests)
    }

    async fn fixture_server_inner(
        responses: HashMap<String, String>,
        requests: Option<Arc<Mutex<Vec<Value>>>>,
    ) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let responses = Arc::new(responses);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let responses = Arc::clone(&responses);
                let requests = requests.as_ref().map(Arc::clone);
                tokio::spawn(async move {
                    let _ = serve_request(stream, responses, requests).await;
                });
            }
        });
        (format!("http://{address}"), task)
    }

    async fn serve_request(
        mut stream: TcpStream,
        responses: Arc<HashMap<String, String>>,
        requests: Option<Arc<Mutex<Vec<Value>>>>,
    ) -> std::io::Result<()> {
        let body = read_http_body(&mut stream).await?;
        let request: Value = serde_json::from_slice(&body).unwrap();
        if let Some(requests) = requests {
            requests.lock().expect("request log lock").push(request.clone());
        }
        let id = request.get("id").cloned().unwrap_or(json!(1));
        let result = multicall_fixture_result(&request, &responses).or_else(|| {
            let key = request_key(&request);
            let selector = request_selector_key(&request);
            responses.get(&key).or_else(|| responses.get(&selector)).cloned()
        });
        let response = match result {
            Some(result) if result == TRANSPORT_FAILURE_FIXTURE => return Ok(()),
            Some(result) if result.starts_with(RPC_ERROR_FIXTURE_PREFIX) => {
                let error: Value = serde_json::from_str(
                    result.strip_prefix(RPC_ERROR_FIXTURE_PREFIX).expect("error prefix"),
                )
                .unwrap();
                json!({"jsonrpc":"2.0","id":id,"error":error})
            }
            Some(result) => {
                let result = if request["method"] == "eth_getLogs" {
                    serde_json::from_str::<Value>(&result)
                        .expect("fixture eth_getLogs response is JSON")
                } else {
                    Value::String(result)
                };
                json!({"jsonrpc":"2.0","id":id,"result":result})
            }
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

    fn multicall_fixture_result(
        request: &Value,
        responses: &HashMap<String, String>,
    ) -> Option<String> {
        if request["method"] != "eth_call" || request["params"][0]["to"] != MULTICALL3_ADDRESS {
            return None;
        }
        let calldata = request["params"][0]
            .get("input")
            .or_else(|| request["params"][0].get("data"))?
            .as_str()?;
        let calldata = alloy::hex::decode(calldata.trim_start_matches("0x")).ok()?;
        let multicall = TestMulticall3::tryAggregateCall::abi_decode(&calldata).ok()?;
        let results = multicall
            .calls
            .into_iter()
            .map(|call| {
                let data = format!("0x{}", alloy::hex::encode(call.callData.as_ref()));
                let key = call_key(&format!("{:#x}", call.target), &data);
                let result = responses.get(&key).or_else(|| {
                    responses.get(&selector_key(
                        &format!("{:#x}", call.target),
                        &data[..data.len().min(10)],
                    ))
                });
                match result {
                    Some(result)
                        if !result.starts_with(RPC_ERROR_FIXTURE_PREFIX)
                            && result != TRANSPORT_FAILURE_FIXTURE =>
                    {
                        alloy::hex::decode(result.trim_start_matches("0x"))
                            .map(|bytes| TestMulticall3::Result {
                                success: true,
                                returnData: bytes.into(),
                            })
                            .unwrap_or(TestMulticall3::Result {
                                success: false,
                                returnData: Default::default(),
                            })
                    }
                    _ => TestMulticall3::Result { success: false, returnData: Default::default() },
                }
            })
            .collect::<Vec<_>>();
        let encoded = TestMulticall3::tryAggregateCall::abi_encode_returns_tuple(&(results,));
        Some(format!("0x{}", alloy::hex::encode(encoded)))
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
            Some("eth_blockNumber") => "eth_blockNumber".to_owned(),
            Some("eth_call") => {
                let params = request["params"][0].clone();
                let data = params
                    .get("data")
                    .or_else(|| params.get("input"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                call_key(params["to"].as_str().unwrap_or_default(), data)
            }
            Some("eth_getStorageAt") => storage_key(
                request["params"][0].as_str().unwrap_or_default(),
                request["params"][1].as_str().unwrap_or_default(),
            ),
            Some("eth_getCode") => get_code_key(request["params"][0].as_str().unwrap_or_default()),
            Some("eth_getLogs") => "eth_getLogs".to_owned(),
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

    fn power_probe_responses(paused: String, is_paused: String) -> HashMap<String, String> {
        let mut map = responses();
        map.insert("eth_blockNumber".to_owned(), "0x2a".to_owned());
        for slot in [EIP1967_IMPLEMENTATION_SLOT, EIP1967_ADMIN_SLOT, EIP1967_BEACON_SLOT] {
            map.insert(storage_key(TOKEN0, slot), format!("0x{}", uint_word(0)));
        }
        put_selector(&mut map, TOKEN0, &function_selector("paused()"), paused);
        put_selector(&mut map, TOKEN0, &function_selector("isPaused()"), is_paused);
        let zero = address_word("0x0000000000000000000000000000000000000000");
        for getter in ["owner()", "pauser()", "sanctionsList()"] {
            put_selector(&mut map, TOKEN0, &function_selector(getter), zero.clone());
        }
        let not_blacklisted = format!("0x{}", uint_word(0));
        for getter in ["isBlacklisted(address)", "isBlackListed(address)"] {
            put_selector(&mut map, TOKEN0, &function_selector(getter), not_blacklisted.clone());
        }
        map
    }

    fn function_selector(signature: &str) -> String {
        let hash = alloy::primitives::keccak256(signature.as_bytes());
        raw_hex(&hash[..4])
    }

    fn v4_pool_key_fixture() -> (B256, HashMap<String, String>) {
        let currency0 = TOKEN0.parse::<Address>().unwrap();
        let currency1 = TOKEN1.parse::<Address>().unwrap();
        let hooks = Address::ZERO;
        let fee = U24::from(3_000_u32);
        let tick_spacing = I24::try_from(60_i32).unwrap();
        let pool_id = alloy::primitives::keccak256(
            (currency0, currency1, fee, tick_spacing, hooks).abi_encode(),
        );
        let mut map = responses();
        let position_manager = format!("{V4_POSITION_MANAGER_ROBINHOOD:?}");
        put_selector(
            &mut map,
            &position_manager,
            &function_selector("poolKeys(bytes25)"),
            format!(
                "0x{}{}{}{}{}",
                address_word(TOKEN0).trim_start_matches("0x"),
                address_word(TOKEN1).trim_start_matches("0x"),
                uint_word(3_000),
                uint_word(60),
                address_word("0x0000000000000000000000000000000000000000").trim_start_matches("0x")
            ),
        );
        let state_view = format!("{V4_STATEVIEW_ROBINHOOD:?}");
        put_selector(
            &mut map,
            &state_view,
            &function_selector("getSlot0(bytes32)"),
            format!(
                "0x{:064x}{}{}{}",
                U256::from(1_u8) << 96,
                uint_word(0),
                uint_word(0),
                uint_word(0)
            ),
        );
        put_selector(
            &mut map,
            &state_view,
            &function_selector("getLiquidity(bytes32)"),
            format!("0x{}", uint_word(1)),
        );
        (pool_id, map)
    }

    #[tokio::test]
    async fn resolves_v4_pool_from_verified_position_manager_key_without_logs() {
        let (pool_id, map) = v4_pool_key_fixture();
        let (url, task, requests) = fixture_server_with_trace(map).await;
        let reader = EvmReader::new(Chain::RobinhoodChain, &url).unwrap();
        let (pool, evidence) = reader.read_v4_pool(&pool_id.to_string()).await.unwrap();
        task.abort();

        assert_eq!(pool.dex, "uniswap-v4");
        assert_eq!(pool.base.address, TOKEN0);
        assert_eq!(pool.quote.address, TOKEN1);
        assert!(evidence.iter().any(|line| line.contains("pool key resolved")));
        let requests = requests.lock().expect("request trace lock");
        assert!(requests.iter().all(|request| request["method"] != "eth_getLogs"));
        let key_call = requests
            .iter()
            .find(|request| {
                request["method"] == "eth_call"
                    && request["params"][0]["to"] == format!("{V4_POSITION_MANAGER_ROBINHOOD:?}")
            })
            .expect("PositionManager poolKeys call");
        let calldata = key_call["params"][0]
            .get("input")
            .or_else(|| key_call["params"][0].get("data"))
            .and_then(Value::as_str)
            .expect("poolKeys calldata");
        let expected_prefix = format!("{:0<64}", alloy::hex::encode(&pool_id.as_slice()[..25]));
        assert_eq!(&calldata[10..], expected_prefix);
    }

    #[tokio::test]
    async fn v4_recent_scan_stops_at_the_window_cap_with_typed_rpc_limit() {
        let (pool_id, mut map) = v4_pool_key_fixture();
        let position_manager = format!("{V4_POSITION_MANAGER_ROBINHOOD:?}");
        put_selector(
            &mut map,
            &position_manager,
            &function_selector("poolKeys(bytes25)"),
            format!(
                "0x{}{}{}{}{}",
                "0".repeat(64),
                "0".repeat(64),
                uint_word(0),
                uint_word(0),
                "0".repeat(64)
            ),
        );
        map.insert("eth_blockNumber".to_owned(), format!("0x{:x}", V4_DEPLOYMENT_ROBINHOOD + 199));
        map.insert("eth_getLogs".to_owned(), "[]".to_owned());
        let (url, task, requests) = fixture_server_with_trace(map).await;
        let reader = EvmReader::new(Chain::RobinhoodChain, &url).unwrap();
        let result = reader.read_v4_pool(&pool_id.to_string()).await;
        task.abort();

        assert!(
            matches!(
                result,
                Err(PoolError::RpcLimit(reason)) if reason.contains("bounded recent scan window")
            ),
            "unexpected V4 fallback result: {result:?}"
        );
        let requests = requests.lock().expect("request trace lock");
        let windows = requests
            .iter()
            .filter(|request| request["method"] == "eth_getLogs")
            .collect::<Vec<_>>();
        assert_eq!(windows.len(), 20);
        assert_eq!(
            windows[0]["params"][0]["fromBlock"],
            format!("0x{:x}", V4_DEPLOYMENT_ROBINHOOD)
        );
        assert_eq!(
            windows[19]["params"][0]["toBlock"],
            format!("0x{:x}", V4_DEPLOYMENT_ROBINHOOD + 199)
        );
    }

    #[tokio::test]
    async fn rejects_v4_pool_key_hash_mismatch_without_log_fallback() {
        let (_, map) = v4_pool_key_fixture();
        let (url, task, requests) = fixture_server_with_trace(map).await;
        let reader = EvmReader::new(Chain::RobinhoodChain, &url).unwrap();
        let unrelated_id = format!("0x{}", "ab".repeat(32));
        let result = reader.read_v4_pool(&unrelated_id).await;
        task.abort();

        assert!(matches!(
            result,
            Err(PoolError::Unknown(reason)) if reason.contains("pool key hash mismatch")
        ));
        assert!(
            requests
                .lock()
                .expect("request trace lock")
                .iter()
                .all(|request| request["method"] != "eth_getLogs")
        );
    }

    #[tokio::test]
    async fn wallet_restriction_reader_checks_both_blacklist_spellings() {
        let wallet = "0x0000000000000000000000000000000000000042";
        for method in ["isBlacklisted(address)", "isBlackListed(address)"] {
            for (blocked, expected) in [(0, false), (1, true)] {
                let mut map = responses();
                map.insert("eth_blockNumber".to_owned(), "0x2a".to_owned());
                let absent_method = if method == "isBlacklisted(address)" {
                    "isBlackListed(address)"
                } else {
                    "isBlacklisted(address)"
                };
                put_selector(
                    &mut map,
                    TOKEN0,
                    &function_selector(absent_method),
                    rpc_error_fixture(-32000, "execution reverted", None),
                );
                put_selector(
                    &mut map,
                    TOKEN0,
                    &function_selector(method),
                    format!("0x{}", uint_word(blocked)),
                );
                let (reader, task) = reader_for(map, Chain::Base).await;
                let report = reader
                    .wallet_restrictions(TOKEN0, wallet, None)
                    .await
                    .expect("blacklist result");
                task.abort();
                assert_eq!(
                    report.restrictions.iter().any(|reason| reason.code == "wallet_blocked"),
                    expected,
                    "{method}"
                );
                assert!(
                    report.complete,
                    "a deterministic absent spelling does not invalidate the available contract query"
                );
            }
        }
    }

    #[tokio::test]
    async fn wallet_restriction_reader_marks_absent_probes_not_applicable() {
        let wallet = "0x0000000000000000000000000000000000000042";
        let mut map = responses();
        map.insert("eth_blockNumber".to_owned(), "0x2a".to_owned());
        for method in ["isBlacklisted(address)", "isBlackListed(address)"] {
            put_selector(
                &mut map,
                TOKEN0,
                &function_selector(method),
                rpc_error_fixture(-32000, "execution reverted", None),
            );
        }
        let (reader, task) = reader_for(map, Chain::Base).await;
        let report =
            reader.wallet_restrictions(TOKEN0, wallet, None).await.expect("wallet query result");
        task.abort();

        assert!(report.complete);
        assert!(!report.applicable);
        assert!(report.restrictions.is_empty());
    }

    #[tokio::test]
    async fn wallet_restriction_reader_uses_configured_sanctions_oracle() {
        let wallet = "0x0000000000000000000000000000000000000042";
        let oracle = "0x0000000000000000000000000000000000000099";
        let mut map = responses();
        map.insert("eth_blockNumber".to_owned(), "0x2a".to_owned());
        for method in ["isBlacklisted(address)", "isBlackListed(address)"] {
            put_selector(
                &mut map,
                TOKEN0,
                &function_selector(method),
                rpc_error_fixture(-32000, "execution reverted", None),
            );
        }
        put_selector(
            &mut map,
            oracle,
            &function_selector("isSanctioned(address)"),
            format!("0x{}", uint_word(1)),
        );
        let (reader, task) = reader_for(map, Chain::Base).await;
        let report = reader
            .wallet_restrictions(TOKEN0, wallet, Some(oracle))
            .await
            .expect("sanctions oracle result");
        task.abort();

        assert!(report.complete);
        assert!(report.restrictions.iter().any(|reason| reason.code == "wallet_sanctioned"));
    }

    #[tokio::test]
    async fn wallet_reader_keeps_known_blacklist_and_sanction_reasons_when_another_probe_fails() {
        let wallet = "0x0000000000000000000000000000000000000042";
        let oracle = "0x0000000000000000000000000000000000000099";
        let mut map = responses();
        map.insert("eth_blockNumber".to_owned(), "0x2a".to_owned());
        put_selector(
            &mut map,
            TOKEN0,
            &function_selector("isBlacklisted(address)"),
            format!("0x{}", uint_word(1)),
        );
        put_selector(
            &mut map,
            oracle,
            &function_selector("isSanctioned(address)"),
            format!("0x{}", uint_word(1)),
        );
        let (reader, task) = reader_for(map, Chain::Base).await;
        let report = reader
            .wallet_restrictions(TOKEN0, wallet, Some(oracle))
            .await
            .expect("partial wallet observation");
        task.abort();

        assert!(!report.complete);
        assert_eq!(
            report.restrictions.iter().map(|reason| reason.code.as_str()).collect::<Vec<_>>(),
            ["wallet_blocked", "wallet_sanctioned"]
        );
    }

    #[tokio::test]
    async fn powers_reads_decode_at_one_block_and_mark_transport_failures_unavailable() {
        let block = 42;
        let beacon = TOKEN1;
        let mut map = responses();
        map.insert("eth_blockNumber".to_owned(), format!("0x{block:x}"));
        map.insert(storage_key(TOKEN0, EIP1967_IMPLEMENTATION_SLOT), format!("0x{}", uint_word(0)));
        map.insert(storage_key(TOKEN0, EIP1967_ADMIN_SLOT), format!("0x{}", uint_word(0)));
        map.insert(storage_key(TOKEN0, EIP1967_BEACON_SLOT), address_word(beacon));
        put_selector(
            &mut map,
            TOKEN0,
            &function_selector("paused()"),
            format!("0x{}", uint_word(1)),
        );
        put_selector(
            &mut map,
            TOKEN0,
            &function_selector("isPaused()"),
            format!("0x{}", uint_word(0)),
        );
        put_selector(&mut map, TOKEN0, &function_selector("owner()"), address_word(TOKEN1));
        put_selector(
            &mut map,
            TOKEN0,
            &function_selector("pauser()"),
            TRANSPORT_FAILURE_FIXTURE.to_owned(),
        );
        put_selector(&mut map, TOKEN0, &function_selector("sanctionsList()"), "0x".to_owned());
        let not_blacklisted = format!("0x{}", uint_word(0));
        for getter in ["isBlacklisted(address)", "isBlackListed(address)"] {
            put_selector(&mut map, TOKEN0, &function_selector(getter), not_blacklisted.clone());
        }
        put_selector(
            &mut map,
            beacon,
            &function_selector("implementation()"),
            address_word(TOKEN0),
        );

        let (reader, task) = reader_for(map, Chain::Base).await;
        let (facts, log) = crate::ports::capture_reads(reader.power_facts(TOKEN0)).await;
        task.abort();
        let facts = facts.expect("token-power probes complete");

        assert!(facts.source_is_proxy);
        assert_eq!(facts.source_target.as_deref(), Some(TOKEN0));
        assert!(facts.can_block.iter().any(|reason| {
            reason.code == "pausable" && reason.detail.contains("currently paused")
        }));
        assert!(facts.can_change_rules.iter().any(|reason| reason.code == "owner_getter"));
        assert!(facts.transient_failure);
        assert_eq!(
            facts.unavailable.iter().map(|reason| reason.code.as_str()).collect::<Vec<_>>(),
            ["rpc_unavailable"]
        );
        assert_eq!(log.block, Some(block));
        assert!(
            log.reads
                .iter()
                .filter(|read| { read.method == "eth_call" || read.method == "eth_getStorageAt" })
                .all(|read| read.block == Some(block))
        );

        let pauser_selector = function_selector("pauser()");
        let pauser_read = log
            .reads
            .iter()
            .find(|read| {
                read.method == "eth_call"
                    && read.params[0]["data"]
                        .as_str()
                        .is_some_and(|data| data.starts_with(&pauser_selector))
            })
            .expect("pauser call captured");
        assert_eq!(pauser_read.raw_result, Some(json!({"available": false})));
        let empty_decode = log
            .reads
            .iter()
            .find(|read| {
                read.method == "eth_call"
                    && read.params[0]["data"]
                        .as_str()
                        .is_some_and(|data| data.starts_with(&function_selector("sanctionsList()")))
            })
            .expect("empty ABI result captured");
        assert_eq!(empty_decode.raw_result, Some(json!("0x")));
        assert!(empty_decode.block == Some(block));
    }
    #[tokio::test]
    async fn deterministic_paused_reverts_are_absent_with_or_without_revert_data() {
        for data in [None, Some("0x")] {
            let map = power_probe_responses(
                rpc_error_fixture(3, "execution reverted", data),
                format!("0x{}", uint_word(0)),
            );
            let (reader, task) = reader_for(map, Chain::Ethereum).await;
            let (facts, log) = crate::ports::capture_reads(reader.power_facts(TOKEN0)).await;
            task.abort();
            let facts = facts.expect("token-power probes complete");
            assert!(!facts.transient_failure, "revert data shape: {data:?}");
            assert!(facts.unavailable.is_empty(), "revert data shape: {data:?}");
            let pausable = facts
                .can_block
                .iter()
                .find(|reason| reason.code == "pausable")
                .expect("isPaused() provides a pause-state fact");
            assert!(pausable.detail.contains("isPaused() is implemented"));

            let paused_read = log
                .reads
                .iter()
                .find(|read| {
                    read.method == "eth_call"
                        && read.params[0]["data"]
                            .as_str()
                            .is_some_and(|call| call.starts_with(&function_selector("paused()")))
                })
                .expect("paused() read captured");
            assert_eq!(paused_read.raw_result, Some(json!("0x")));
        }
    }

    #[tokio::test]
    async fn pause_getter_rpc_errors_are_classified_as_transient_or_absent() {
        for (code, message, expected_unavailable, expected_raw) in [
            (-32005, "rate limit exceeded", Some("rate_limited"), json!({"available": false})),
            (-32000, "header not found", Some("rpc_unavailable"), json!({"available": false})),
            (-32000, "execution reverted", None, json!("0x")),
        ] {
            let map = power_probe_responses(
                rpc_error_fixture(code, message, None),
                format!("0x{}", uint_word(0)),
            );
            let (reader, task) = reader_for(map, Chain::Ethereum).await;
            let (facts, log) = crate::ports::capture_reads(reader.power_facts(TOKEN0)).await;
            task.abort();
            let facts = facts.expect("token-power probes complete");
            assert_eq!(facts.transient_failure, expected_unavailable.is_some(), "{message}");
            if let Some(expected) = expected_unavailable {
                assert_eq!(
                    facts.unavailable.iter().map(|reason| reason.code.as_str()).collect::<Vec<_>>(),
                    [expected],
                    "{message}"
                );
            } else {
                assert!(facts.unavailable.is_empty(), "{message}");
            }
            let paused_read = log
                .reads
                .iter()
                .find(|read| {
                    read.method == "eth_call"
                        && read.params[0]["data"]
                            .as_str()
                            .is_some_and(|call| call.starts_with(&function_selector("paused()")))
                })
                .expect("paused() read captured");
            assert_eq!(paused_read.raw_result, Some(expected_raw), "{message}");
        }
    }

    #[tokio::test]
    async fn is_paused_getter_falls_back_when_paused_is_absent() {
        for (is_paused, expected_detail) in
            [(true, "currently paused"), (false, "currently not paused")]
        {
            let expected_raw = format!("0x{}", uint_word(if is_paused { 1 } else { 0 }));
            let map = power_probe_responses(
                rpc_error_fixture(-32000, "execution reverted", None),
                expected_raw.clone(),
            );
            let (reader, task) = reader_for(map, Chain::Ethereum).await;
            let (facts, log) = crate::ports::capture_reads(reader.power_facts(TOKEN0)).await;
            task.abort();
            let facts = facts.expect("token-power probes complete");
            assert!(!facts.transient_failure);
            assert!(facts.unavailable.is_empty());
            let pausable = facts
                .can_block
                .iter()
                .find(|reason| reason.code == "pausable")
                .expect("isPaused() provides a pause-state fact");
            assert!(pausable.detail.contains("isPaused() is implemented"));
            assert!(pausable.detail.contains(expected_detail));

            let is_paused_read = log
                .reads
                .iter()
                .find(|read| {
                    read.method == "eth_call"
                        && read.params[0]["data"]
                            .as_str()
                            .is_some_and(|call| call.starts_with(&function_selector("isPaused()")))
                })
                .expect("isPaused() read captured");
            assert_eq!(is_paused_read.raw_result, Some(json!(expected_raw)));
            assert_eq!(is_paused_read.block, Some(42));

            let paused_read = log
                .reads
                .iter()
                .find(|read| {
                    read.method == "eth_call"
                        && read.params[0]["data"]
                            .as_str()
                            .is_some_and(|call| call.starts_with(&function_selector("paused()")))
                })
                .expect("paused() read captured");
            assert_eq!(paused_read.raw_result, Some(json!("0x")));
        }
    }

    #[tokio::test]
    async fn absent_pause_getters_produce_no_pausable_fact() {
        let absent = rpc_error_fixture(-32000, "execution reverted", None);
        let map = power_probe_responses(absent.clone(), absent);
        let (reader, task) = reader_for(map, Chain::Ethereum).await;
        let facts = reader.power_facts(TOKEN0).await.expect("token-power probes complete");
        task.abort();

        assert!(!facts.transient_failure);
        assert!(facts.unavailable.is_empty());
        assert!(!facts.can_block.iter().any(|reason| reason.code == "pausable"));
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
    async fn recognizes_pancake_v2_factory_membership_and_records_provenance() {
        let pool_address = "0x0000000000000000000000000000000000000035";
        let factory = format!("{PANCAKE_V2_BNB:?}");
        let mut map = responses();
        standard_v2_pool_from_factory(&mut map, pool_address, TOKEN0, TOKEN1, &factory);
        token_balances(&mut map, pool_address, TOKEN0, 41, TOKEN1, 99);
        let (reader, task) = reader_for(map, Chain::Bnb).await;
        let (result, evidence) =
            crate::ports::capture_reads(reader.read_pool_with_quotes(pool_address, &[])).await;
        task.abort();

        let pool = result.expect("registered Pancake V2 pool");
        assert_eq!(pool.dex, "pancake-v2");
        assert!(evidence.reads.iter().any(|read| {
            read.method == "eth_call"
                && read.params[0]
                    .as_str()
                    .is_some_and(|address| address.eq_ignore_ascii_case(&factory))
                && read.params[1] == "getPair"
        }));
    }

    #[tokio::test]
    async fn recognizes_ramses_v3_factory_membership_and_records_provenance() {
        let pool_address = "0x0000000000000000000000000000000000000036";
        let factory = format!("{RAMSES_V3_ROBINHOOD:?}");
        let mut map = responses();
        standard_v3_pool(&mut map, pool_address, TOKEN0, TOKEN1, &factory);
        put_call(
            &mut map,
            pool_address,
            &function_selector("tickSpacing()"),
            format!("0x{}", uint_word(60)),
        );
        put_selector(
            &mut map,
            &factory,
            &function_selector("getPool(address,address,int24)"),
            address_word(pool_address),
        );
        token_balances(&mut map, pool_address, TOKEN0, 41, TOKEN1, 99);
        let (reader, task) = reader_for(map, Chain::RobinhoodChain).await;
        let (result, evidence) =
            crate::ports::capture_reads(reader.read_pool_with_quotes(pool_address, &[])).await;
        task.abort();

        let pool = result.expect("registered Ramses V3 pool");
        assert_eq!(pool.dex, "ramses-v3");
        assert!(
            evidence
                .reads
                .iter()
                .any(|read| { read.params.get(1).is_some_and(|method| method == "tickSpacing()") })
        );
        let get_pool_reads: Vec<_> = evidence
            .reads
            .iter()
            .filter(|read| read.params.get(1).is_some_and(|method| method == "getPool"))
            .collect();
        assert!(
            get_pool_reads.iter().any(|read| {
                read.method == "eth_call"
                    && read.params[0]
                        .as_str()
                        .is_some_and(|address| address.eq_ignore_ascii_case(&factory))
                    && read.params[2] == canonical(TOKEN0.parse().unwrap())
                    && read.params[3] == canonical(TOKEN1.parse().unwrap())
                    && (read.params[4].as_i64() == Some(60)
                        || read.params[4].as_str().and_then(|value| value.parse::<i64>().ok())
                            == Some(60))
            }),
            "Ramses getPool read evidence: {get_pool_reads:?}"
        );
    }
    #[tokio::test]
    async fn recognizes_pancake_v3_factory_membership_and_records_provenance() {
        let pool_address = "0x0000000000000000000000000000000000000038";
        let factory = format!("{PANCAKE_V3_FACTORY:?}");
        let mut map = responses();
        standard_v3_pool(&mut map, pool_address, TOKEN0, TOKEN1, &factory);
        token_balances(&mut map, pool_address, TOKEN0, 41, TOKEN1, 99);
        let (reader, task) = reader_for(map, Chain::Bnb).await;
        let (result, evidence) =
            crate::ports::capture_reads(reader.read_pool_with_quotes(pool_address, &[])).await;
        task.abort();

        let pool = result.expect("registered Pancake V3 pool");
        assert_eq!(pool.dex, "pancake-v3");
        let membership_read = evidence
            .reads
            .iter()
            .find(|read| {
                read.method == "eth_call"
                    && read.params.get(1).is_some_and(|method| method == "getPool")
            })
            .expect("Pancake V3 getPool read evidence");
        assert!(
            membership_read.params[0]
                .as_str()
                .is_some_and(|address| address.eq_ignore_ascii_case(&factory))
        );
        assert_eq!(membership_read.params[2], canonical(TOKEN0.parse().unwrap()));
        assert_eq!(membership_read.params[3], canonical(TOKEN1.parse().unwrap()));
        let fee = membership_read.params[4].as_u64().or_else(|| {
            membership_read.params[4].as_str().and_then(|value| {
                value.parse::<u64>().ok().or_else(|| {
                    value.strip_prefix("0x").and_then(|hex| u64::from_str_radix(hex, 16).ok())
                })
            })
        });
        assert_eq!(fee, Some(3_000));
    }

    #[tokio::test]
    async fn unknown_v3_factory_is_typed_as_unsupported_venue() {
        let pool_address = "0x0000000000000000000000000000000000000037";
        let mut map = responses();
        standard_v3_pool(
            &mut map,
            pool_address,
            TOKEN0,
            TOKEN1,
            "0x0000000000000000000000000000000000000001",
        );
        let (reader, task) = reader_for(map, Chain::Base).await;
        let result = reader.read_pool(pool_address).await;
        task.abort();

        assert!(matches!(result, Err(PoolError::UnsupportedVenue(_))));
    }

    #[tokio::test]
    async fn decodes_v3_pool() {
        let pool = "0x0000000000000000000000000000000000000033";
        let mut map = responses();
        standard_v3_pool(
            &mut map,
            pool,
            TOKEN0,
            TOKEN1,
            "0x1f98431c8ad98523631ae4a59f267346ea31f984",
        );
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
    async fn pair_getters_alone_do_not_establish_pool_provenance() {
        let fake_pool = "0x0000000000000000000000000000000000000034";
        let mut map = responses();
        put_call(&mut map, fake_pool, "0x0dfe1681", address_word(TOKEN0));
        put_call(&mut map, fake_pool, "0xd21220a7", address_word(TOKEN1));
        put_call(
            &mut map,
            fake_pool,
            "0x0902f1ac",
            format!("0x{}{}{}", uint_word(1_000), uint_word(2_000), uint_word(3)),
        );
        let (reader, task) = reader_for(map, Chain::Base).await;
        let result = reader.read_pool_with_quotes(fake_pool, &[]).await;
        task.abort();

        assert!(
            matches!(result, Err(PoolError::Reader(reason)) if reason.contains("v2 pool factory"))
        );
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
        standard_v3_pool(&mut map, pool2, token, quote, uni_factory);
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
    #[test]
    fn transient_reason_codes_use_typed_transport_errors() {
        let rate_limited = TransportErrorKind::http_error(429, String::new());
        assert_eq!(transient_reason_code(&rate_limited), "rate_limited");

        let timeout = TransportErrorKind::http_error(504, String::new());
        assert_eq!(transient_reason_code(&timeout), "rpc_timeout");
        let contract_timeout = alloy::contract::Error::TransportError(
            TransportErrorKind::http_error(504, String::new()),
        );
        assert_eq!(contract_error_reason_code(&contract_timeout), "rpc_timeout");

        let url_timeout =
            TransportErrorKind::custom_str("request https://rpc.example/?timeout=5000 failed");
        assert_eq!(transient_reason_code(&url_timeout), "rpc_unavailable");
    }
}
