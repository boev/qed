use super::{PoolError, PoolInfo, PoolReader, TokenMeta, TokenSide, WalletHolding};
use crate::chain::Chain;
use crate::state::RpcRateLimiter;
use async_trait::async_trait;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

const RPC_USER_AGENT: &str = "qed/0.1";
const RPC_MAX_ATTEMPTS: usize = 4;

/// Pump.fun program id and BondingCurve account layout:
/// https://raw.githubusercontent.com/pump-fun/pump-public-docs/main/idl/pump.json
pub const PUMP_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const PUMP_BONDING_CURVE_DISCRIMINATOR: [u8; 8] = [23, 183, 248, 55, 96, 216, 172, 96];
const PUMP_BONDING_CURVE_QUOTE_MINT_OFFSET: usize = 83;
const PUMP_BONDING_CURVE_DATA_SIZE: usize = 151;

/// PumpSwap program id and Pool account layout:
/// https://raw.githubusercontent.com/pump-fun/pump-public-docs/main/idl/pump_amm.json
pub const PUMPSWAP_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
/// Raydium AMM v4 program id:
/// https://raw.githubusercontent.com/raydium-io/raydium-amm/master/README.md
/// AmmInfo field offsets:
/// https://raw.githubusercontent.com/raydium-io/raydium-amm/master/program/src/state.rs
pub const RAYDIUM_AMM_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
/// Raydium CPMM program id and PoolState field offsets:
/// https://docs.raydium.io/raydium/protocol/developers/addresses
/// https://raw.githubusercontent.com/raydium-io/raydium-cp-swap/master/programs/cp-swap/src/states/pool.rs
pub const RAYDIUM_CPMM_PROGRAM: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";
/// Raydium CLMM program id and PoolState field offsets:
/// https://raw.githubusercontent.com/raydium-io/raydium-clmm/master/programs/amm/src/states/pool.rs
pub const RAYDIUM_CLMM_PROGRAM: &str = "CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK";
/// Anchor's `PoolState` discriminator is followed by bump, amm_config, owner,
/// then token_mint_0=73, token_mint_1=105, token_vault_0=137,
/// token_vault_1=169. `PoolState::LEN` is 1544 bytes.
const RAYDIUM_CLMM_POOL_DISCRIMINATOR: [u8; 8] = [247, 237, 227, 245, 215, 195, 222, 70];
const RAYDIUM_CLMM_TOKEN_MINT_0_OFFSET: usize = 73;
const RAYDIUM_CLMM_TOKEN_MINT_1_OFFSET: usize = 105;
const RAYDIUM_CLMM_TOKEN_VAULT_0_OFFSET: usize = 137;
const RAYDIUM_CLMM_TOKEN_VAULT_1_OFFSET: usize = 169;
const RAYDIUM_CLMM_DATA_SIZE: usize = 1544;

/// Orca Whirlpool program id and Whirlpool field offsets:
/// https://raw.githubusercontent.com/orca-so/whirlpools/main/programs/whirlpool/src/state/whirlpool.rs
pub const ORCA_WHIRLPOOL_PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";
/// Anchor's `Whirlpool` discriminator is followed by token_mint_a=101,
/// token_vault_a=133, token_mint_b=181, token_vault_b=213.
/// `Whirlpool::LEN` is 653 bytes.
const ORCA_WHIRLPOOL_DISCRIMINATOR: [u8; 8] = [63, 149, 209, 12, 225, 128, 99, 9];
const ORCA_WHIRLPOOL_TOKEN_MINT_A_OFFSET: usize = 101;
const ORCA_WHIRLPOOL_TOKEN_VAULT_A_OFFSET: usize = 133;
const ORCA_WHIRLPOOL_TOKEN_MINT_B_OFFSET: usize = 181;
const ORCA_WHIRLPOOL_TOKEN_VAULT_B_OFFSET: usize = 213;
const ORCA_WHIRLPOOL_DATA_SIZE: usize = 653;

/// Meteora DLMM program id and LbPair field offsets:
/// https://raw.githubusercontent.com/MeteoraAg/dlmm-sdk/main/ts-client/src/dlmm/idl/idl.ts
/// https://raw.githubusercontent.com/MeteoraAg/dlmm-sdk/main/ts-client/src/dlmm/idl/idl.json
/// The bytemuck `LbPair` fields are token_x_mint=88, token_y_mint=120,
/// reserve_x=152, reserve_y=184, and the account is 904 bytes.
pub const METEORA_DLMM_PROGRAM: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";
const METEORA_DLMM_LB_PAIR_DISCRIMINATOR: [u8; 8] = [33, 11, 49, 98, 181, 101, 177, 13];
const METEORA_DLMM_TOKEN_X_MINT_OFFSET: usize = 88;
const METEORA_DLMM_TOKEN_Y_MINT_OFFSET: usize = 120;
const METEORA_DLMM_RESERVE_X_OFFSET: usize = 152;
const METEORA_DLMM_RESERVE_Y_OFFSET: usize = 184;
const METEORA_DLMM_DATA_SIZE: usize = 904;

const METAPLEX_METADATA_PROGRAM: &str = "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s";
const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

// PumpSwap Pool is an Anchor account with an eight-byte discriminator. The
// discriminator and fields are in the official PumpSwap IDL above. The MVP
// Pool version is 211 bytes: base_mint=43, quote_mint=75,
// pool_base_token_account=139, pool_quote_token_account=171.
const PUMPSWAP_POOL_DISCRIMINATOR: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];
const PUMPSWAP_BASE_MINT_OFFSET: usize = 43;
const PUMPSWAP_QUOTE_MINT_OFFSET: usize = 75;
const PUMPSWAP_BASE_VAULT_OFFSET: usize = 139;
const PUMPSWAP_QUOTE_VAULT_OFFSET: usize = 171;
const PUMPSWAP_DATA_SIZE: usize = 211;

// Raydium AMM v4's packed AmmInfo is documented in state.rs above. Sixteen
// u64 fields (128 bytes), Fees (64), and StateData (144) precede the public
// keys: coin_vault=336, pc_vault=368, coin_vault_mint=400,
// pc_vault_mint=432; the complete account is 752 bytes.
const RAYDIUM_AMM_COIN_VAULT_OFFSET: usize = 336;
const RAYDIUM_AMM_PC_VAULT_OFFSET: usize = 368;
const RAYDIUM_AMM_COIN_MINT_OFFSET: usize = 400;
const RAYDIUM_AMM_PC_MINT_OFFSET: usize = 432;
const RAYDIUM_AMM_DATA_SIZE: usize = 752;

// Raydium CPMM PoolState is an Anchor zero-copy account. Its declaration is
// in the official source above. The eight-byte discriminator precedes
// token_0_vault=8, token_1_vault=40, token_0_mint=104, token_1_mint=136;
// PoolState::LEN is 637 bytes including that discriminator.
const RAYDIUM_CPMM_TOKEN_0_VAULT_OFFSET: usize = 8;
const RAYDIUM_CPMM_TOKEN_1_VAULT_OFFSET: usize = 40;
const RAYDIUM_CPMM_TOKEN_0_MINT_OFFSET: usize = 104;
const RAYDIUM_CPMM_TOKEN_1_MINT_OFFSET: usize = 136;
const RAYDIUM_CPMM_DATA_SIZE: usize = 637;
#[derive(Clone)]
pub struct SolanaReader {
    client: reqwest::Client,
    rpc_url: String,
    rpc_limiter: Arc<RpcRateLimiter>,
}

impl SolanaReader {
    pub fn with_client_and_limiter(
        client: reqwest::Client,
        rpc_url: impl Into<String>,
        rpc_limiter: Arc<RpcRateLimiter>,
    ) -> Self {
        Self { client, rpc_url: rpc_url.into(), rpc_limiter }
    }

    #[cfg(test)]
    fn new(rpc_url: impl Into<String>) -> Self {
        Self::with_client_and_limiter(
            reqwest::Client::new(),
            rpc_url,
            Arc::new(RpcRateLimiter::new(4)),
        )
    }

    /// Decode one account without performing network I/O. This is public so
    /// fixture-based callers can validate layouts independently of an RPC.
    pub fn decode_account(
        address: &str,
        owner: &str,
        data: &[u8],
    ) -> Result<DecodedPool, PoolError> {
        let pool = canonical_address(address)?;
        let owner = canonical_address(owner)?;
        let decoded = match owner.as_str() {
            PUMPSWAP_PROGRAM => decode_pumpswap(&pool, data)?,
            RAYDIUM_AMM_PROGRAM => decode_raydium_amm(&pool, data)?,
            RAYDIUM_CPMM_PROGRAM => decode_raydium_cpmm(&pool, data)?,
            RAYDIUM_CLMM_PROGRAM => decode_raydium_clmm(&pool, data)?,
            ORCA_WHIRLPOOL_PROGRAM => decode_orca_whirlpool(&pool, data)?,
            METEORA_DLMM_PROGRAM => decode_meteora_dlmm(&pool, data)?,
            _ => return Err(PoolError::Unknown(format!("unsupported Solana pool owner {owner}"))),
        };
        Ok(decoded)
    }

    pub async fn wallet_holdings(
        &self,
        owner: &str,
        _known_tokens: &[String],
    ) -> Result<Vec<WalletHolding>, PoolError> {
        let owner = canonical_address(owner)?;
        let accounts = self.token_accounts_for_owner(&owner).await?;
        let mut holdings = HashMap::<String, u64>::new();
        for account in accounts {
            let Some((mint, amount)) = token_account_holding(&account.account.data) else {
                continue;
            };
            let entry = holdings.entry(mint).or_insert(0);
            *entry = entry.saturating_add(amount);
        }
        let mut output = Vec::with_capacity(holdings.len());
        for (mint, amount) in holdings {
            if amount == 0 {
                continue;
            }
            let metadata = self.token_meta(&mint).await.ok();
            output.push(WalletHolding {
                chain: Chain::Solana,
                token_address: mint,
                symbol: metadata.as_ref().and_then(|value| value.symbol.clone()),
                amount: amount.to_string(),
                decimals: metadata.as_ref().and_then(|value| value.decimals),
            });
        }
        Ok(output)
    }

    pub async fn token_meta(&self, address: &str) -> Result<TokenMeta, PoolError> {
        let address = canonical_address(address)?;
        let supply = self.token_supply(&address).await?;
        let mint_account = self.get_account_info(&address).await?;

        let mut symbol = None;
        let mut name = None;
        if let Ok(Some(metadata)) = self.metaplex_metadata(&address).await {
            symbol = metadata.symbol;
            name = metadata.name;
        }
        if (symbol.is_none() || name.is_none())
            && let Some(account) = &mint_account
            && account.owner == TOKEN_2022_PROGRAM
        {
            let extension = token_metadata_extension(&account.data)?;
            if symbol.is_none() {
                symbol = extension.as_ref().and_then(|item| item.symbol.clone());
            }
            if name.is_none() {
                name = extension.and_then(|item| item.name);
            }
        }

        Ok(TokenMeta {
            address,
            symbol: super::cap_token_text(symbol),
            name: super::cap_token_text(name),
            decimals: Some(supply.decimals),
            total_supply: Some(supply.amount),
        })
    }

    /// Find pools where `token` is either the declared base or quote mint.
    /// Candidate quotes constrain the opposite side, so callers can ask for
    /// the deepest stock/USDC/etc. pools without downloading every pool.
    pub async fn pools_for_token(
        &self,
        token: &str,
        candidate_quotes: &[String],
    ) -> Result<Vec<PoolInfo>, PoolError> {
        let token = canonical_address(token)?;
        let quotes: HashSet<String> =
            candidate_quotes.iter().filter_map(|address| canonical_address(address).ok()).collect();
        let mut accounts = Vec::new();
        for (program, base_offset, quote_offset, size) in [
            (
                PUMPSWAP_PROGRAM,
                PUMPSWAP_BASE_MINT_OFFSET,
                PUMPSWAP_QUOTE_MINT_OFFSET,
                PUMPSWAP_DATA_SIZE,
            ),
            (
                RAYDIUM_AMM_PROGRAM,
                RAYDIUM_AMM_COIN_MINT_OFFSET,
                RAYDIUM_AMM_PC_MINT_OFFSET,
                RAYDIUM_AMM_DATA_SIZE,
            ),
            (
                RAYDIUM_CPMM_PROGRAM,
                RAYDIUM_CPMM_TOKEN_0_MINT_OFFSET,
                RAYDIUM_CPMM_TOKEN_1_MINT_OFFSET,
                RAYDIUM_CPMM_DATA_SIZE,
            ),
            (
                RAYDIUM_CLMM_PROGRAM,
                RAYDIUM_CLMM_TOKEN_MINT_0_OFFSET,
                RAYDIUM_CLMM_TOKEN_MINT_1_OFFSET,
                RAYDIUM_CLMM_DATA_SIZE,
            ),
            (
                ORCA_WHIRLPOOL_PROGRAM,
                ORCA_WHIRLPOOL_TOKEN_MINT_A_OFFSET,
                ORCA_WHIRLPOOL_TOKEN_MINT_B_OFFSET,
                ORCA_WHIRLPOOL_DATA_SIZE,
            ),
            (
                METEORA_DLMM_PROGRAM,
                METEORA_DLMM_TOKEN_X_MINT_OFFSET,
                METEORA_DLMM_TOKEN_Y_MINT_OFFSET,
                METEORA_DLMM_DATA_SIZE,
            ),
        ] {
            let base = self
                .get_program_accounts(
                    program,
                    vec![
                        RpcFilter::DataSize { data_size: size },
                        RpcFilter::Memcmp {
                            memcmp: RpcMemcmp { offset: base_offset, bytes: token.clone() },
                        },
                    ],
                )
                .await?;
            let quote = self
                .get_program_accounts(
                    program,
                    vec![
                        RpcFilter::DataSize { data_size: size },
                        RpcFilter::Memcmp {
                            memcmp: RpcMemcmp { offset: quote_offset, bytes: token.clone() },
                        },
                    ],
                )
                .await?;
            accounts.extend(base);
            accounts.extend(quote);
        }

        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        for account in accounts {
            let pool = canonical_address(&account.pubkey)?;
            if !seen.insert(pool.clone()) {
                continue;
            }
            let decoded = match Self::decode_account(&pool, account.owner(), account.data()) {
                Ok(decoded) => decoded,
                Err(_) => continue,
            };
            let opposite = if decoded.base_mint == token {
                &decoded.quote_mint
            } else if decoded.quote_mint == token {
                &decoded.base_mint
            } else {
                continue;
            };
            if !quotes.is_empty() && !quotes.contains(opposite) {
                continue;
            }
            candidates.push(decoded);
        }

        let mut vaults = Vec::with_capacity(candidates.len() * 2);
        for pool in &candidates {
            vaults.push(pool.base_vault.clone());
            vaults.push(pool.quote_vault.clone());
        }
        let balances = self.batch_token_balances(&vaults).await?;
        let mut result = Vec::with_capacity(candidates.len());
        for decoded in candidates {
            let base_balance = balances.get(&decoded.base_vault).cloned();
            let quote_balance = balances.get(&decoded.quote_vault).cloned();
            result.push(PoolInfo {
                chain: Chain::Solana,
                pool: decoded.pool,
                dex: decoded.dex.to_owned(),
                base: TokenSide {
                    address: decoded.base_mint,
                    symbol: None,
                    decimals: base_balance.as_ref().map(|value| value.decimals),
                    balance: base_balance.map(|value| value.amount),
                },
                quote: TokenSide {
                    address: decoded.quote_mint,
                    symbol: None,
                    decimals: quote_balance.as_ref().map(|value| value.decimals),
                    balance: quote_balance.map(|value| value.amount),
                },
            });
        }
        result.sort_by(|left, right| {
            decimal_cmp(right.quote.balance.as_deref(), left.quote.balance.as_deref())
        });
        Ok(result)
    }

    async fn read_pool_impl(&self, address: &str) -> Result<PoolInfo, PoolError> {
        let address = canonical_address(address)?;
        let account = self.get_account_info(&address).await?.ok_or_else(|| {
            PoolError::Unknown(format!("Solana account {address} does not exist"))
        })?;
        let mut pool = if account.owner == PUMP_PROGRAM {
            self.read_pump_bonding_curve(&address, &account.data).await?
        } else {
            let decoded = Self::decode_account(&address, &account.owner, &account.data)?;
            let (base_balance, quote_balance) = tokio::try_join!(
                self.token_account_balance(&decoded.base_vault),
                self.token_account_balance(&decoded.quote_vault)
            )?;
            PoolInfo {
                chain: Chain::Solana,
                pool: decoded.pool,
                dex: decoded.dex.to_owned(),
                base: TokenSide {
                    address: decoded.base_mint,
                    symbol: None,
                    decimals: Some(base_balance.decimals),
                    balance: Some(base_balance.amount),
                },
                quote: TokenSide {
                    address: decoded.quote_mint,
                    symbol: None,
                    decimals: Some(quote_balance.decimals),
                    balance: Some(quote_balance.amount),
                },
            }
        };
        let metadata = self
            .batch_token_metadata(&[pool.base.address.clone(), pool.quote.address.clone()])
            .await?;
        apply_token_metadata(&mut pool, &metadata);
        Ok(pool)
    }

    async fn read_pump_bonding_curve(
        &self,
        pool: &str,
        data: &[u8],
    ) -> Result<PoolInfo, PoolError> {
        let quote_mint = decode_pump_bonding_curve(data)?;
        let accounts = self.token_accounts_for_owner(pool).await?;
        let mut base_vault = None;
        let mut quote_vault = None;
        for account in accounts {
            let Some(mint) =
                account.data().get(..32).map(|bytes| bs58::encode(bytes).into_string())
            else {
                continue;
            };
            if mint == quote_mint {
                quote_vault = Some(account.pubkey);
            } else if base_vault.is_none() {
                base_vault = Some((mint, account.pubkey));
            }
        }
        let (base_mint, base_vault) = base_vault.ok_or_else(|| {
            PoolError::Unknown(format!("Pump.fun pool {pool} has no base token account"))
        })?;
        let quote_vault = quote_vault.ok_or_else(|| {
            PoolError::Unknown(format!("Pump.fun pool {pool} has no quote token account"))
        })?;
        let (base_balance, quote_balance) = tokio::try_join!(
            self.token_account_balance(&base_vault),
            self.token_account_balance(&quote_vault)
        )?;
        Ok(PoolInfo {
            chain: Chain::Solana,
            pool: pool.to_owned(),
            dex: "pumpfun".to_owned(),
            base: TokenSide {
                address: base_mint,
                symbol: None,
                decimals: Some(base_balance.decimals),
                balance: Some(base_balance.amount),
            },
            quote: TokenSide {
                address: quote_mint,
                symbol: None,
                decimals: Some(quote_balance.decimals),
                balance: Some(quote_balance.amount),
            },
        })
    }

    async fn rpc<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T, PoolError> {
        let request = RpcRequest { jsonrpc: "2.0", id: 1, method: method.to_owned(), params };
        for attempt in 0..RPC_MAX_ATTEMPTS {
            self.rpc_limiter.acquire().await;
            let response = self
                .client
                .post(&self.rpc_url)
                .header(reqwest::header::USER_AGENT, RPC_USER_AGENT)
                .json(&request)
                .send()
                .await
                .map_err(|error| PoolError::Reader(format!("{method}: {error}")))?;
            let status = response.status();
            let body = crate::net::body(response)
                .await
                .map_err(|_| PoolError::Reader(format!("{method}: response body unavailable")))?;
            let body = String::from_utf8(body)
                .map_err(|_| PoolError::Reader(format!("{method}: response was not UTF-8")))?;
            if status == StatusCode::TOO_MANY_REQUESTS {
                if attempt + 1 < RPC_MAX_ATTEMPTS {
                    backoff(attempt).await;
                    continue;
                }
                return Err(http_rpc_error(method, status, &body));
            }
            if !status.is_success() {
                return Err(http_rpc_error(method, status, &body));
            }

            let value: Value = serde_json::from_str(&body).map_err(|error| {
                PoolError::Reader(format!(
                    "{method}: invalid JSON-RPC response: {error}; body: {body}"
                ))
            })?;
            let envelope: RpcEnvelope = serde_json::from_value(value.clone()).map_err(|error| {
                PoolError::Reader(format!(
                    "{method}: invalid JSON-RPC response: {error}; body: {body}"
                ))
            })?;
            if let Some(error) = envelope.error {
                if error.is_retryable() && attempt + 1 < RPC_MAX_ATTEMPTS {
                    backoff(attempt).await;
                    continue;
                }
                return Err(PoolError::Reader(format!("{method}: {error}")));
            }
            let result = value.get("result").ok_or_else(|| {
                PoolError::Reader(format!(
                    "{method}: invalid JSON-RPC response: missing result; body: {body}"
                ))
            })?;
            crate::attest::record_read(
                method,
                request.params.clone(),
                result,
                method == "getAccountInfo",
                None,
                (method == "getSlot").then(|| result.as_u64()).flatten(),
            );
            return serde_json::from_value(result.clone()).map_err(|error| {
                PoolError::Reader(format!(
                    "{method}: invalid JSON-RPC result: {error}; body: {body}"
                ))
            });
        }
        unreachable!("RPC retry loop always returns")
    }

    async fn get_account_info(&self, address: &str) -> Result<Option<RpcAccount>, PoolError> {
        let response: RpcContext<Option<RpcAccount>> = self
            .rpc(
                "getAccountInfo",
                json!([address, { "encoding": "base64", "commitment": "confirmed" }]),
            )
            .await?;
        Ok(response.value)
    }

    async fn get_multiple_accounts(
        &self,
        addresses: &[String],
    ) -> Result<Vec<Option<RpcAccount>>, PoolError> {
        let response: RpcContext<Vec<Option<RpcAccount>>> = self
            .rpc(
                "getMultipleAccounts",
                json!([addresses, { "encoding": "base64", "commitment": "confirmed" }]),
            )
            .await?;
        Ok(response.value)
    }

    async fn get_program_accounts(
        &self,
        program: &str,
        filters: Vec<RpcFilter>,
    ) -> Result<Vec<RpcProgramAccount>, PoolError> {
        self.rpc(
            "getProgramAccounts",
            json!([
                program,
                { "encoding": "base64", "commitment": "confirmed", "filters": filters }
            ]),
        )
        .await
    }

    async fn token_accounts_for_owner(
        &self,
        owner: &str,
    ) -> Result<Vec<RpcProgramAccount>, PoolError> {
        let mut accounts = self.get_token_accounts_by_owner(TOKEN_PROGRAM, owner).await?;
        accounts.extend(self.get_token_accounts_by_owner(TOKEN_2022_PROGRAM, owner).await?);
        Ok(accounts)
    }

    async fn get_token_accounts_by_owner(
        &self,
        program: &str,
        owner: &str,
    ) -> Result<Vec<RpcProgramAccount>, PoolError> {
        self.rpc(
            "getTokenAccountsByOwner",
            json!([
                owner,
                { "programId": program },
                { "encoding": "base64", "commitment": "confirmed" }
            ]),
        )
        .await
    }

    async fn token_account_balance(&self, address: &str) -> Result<TokenAmount, PoolError> {
        let response: TokenBalanceResponse = self
            .rpc("getTokenAccountBalance", json!([address, { "commitment": "confirmed" }]))
            .await?;
        Ok(response.value)
    }

    async fn token_supply(&self, address: &str) -> Result<TokenAmount, PoolError> {
        let response: TokenBalanceResponse =
            self.rpc("getTokenSupply", json!([address, { "commitment": "confirmed" }])).await?;
        Ok(response.value)
    }

    async fn batch_token_balances(
        &self,
        addresses: &[String],
    ) -> Result<HashMap<String, TokenAmount>, PoolError> {
        let mut unique = Vec::with_capacity(addresses.len());
        let mut seen = HashSet::new();
        for address in addresses {
            if seen.insert(address.clone()) {
                unique.push(address.clone());
            }
        }
        let mut balances = HashMap::new();
        for chunk in unique.chunks(100) {
            let accounts = self.get_multiple_accounts(chunk).await?;
            for (address, account) in chunk.iter().zip(accounts) {
                if let Some(account) = account
                    && let Some(amount) = token_account_amount(&account.data)
                {
                    balances.insert(address.clone(), amount);
                }
            }
        }
        Ok(balances)
    }

    async fn batch_token_metadata(
        &self,
        addresses: &[String],
    ) -> Result<HashMap<String, TokenMeta>, PoolError> {
        let mut unique = Vec::with_capacity(addresses.len());
        let mut seen = HashSet::new();
        for address in addresses {
            if seen.insert(address.clone()) {
                unique.push(address.clone());
            }
        }
        let accounts = self.get_multiple_accounts(&unique).await?;
        let mut metadata = HashMap::new();
        for (address, account) in unique.into_iter().zip(accounts) {
            let Some(account) = account else { continue };
            let decimals = account.data.get(44).copied();
            let supply = account
                .data
                .get(36..44)
                .and_then(|bytes| bytes.try_into().ok())
                .map(u64::from_le_bytes)
                .map(|value| value.to_string());
            let extension = token_metadata_extension(&account.data).ok().flatten();
            metadata.insert(
                address.clone(),
                TokenMeta {
                    address,
                    symbol: extension.as_ref().and_then(|value| value.symbol.clone()),
                    name: extension.and_then(|value| value.name),
                    decimals,
                    total_supply: supply,
                },
            );
        }
        Ok(metadata)
    }

    async fn metaplex_metadata(&self, mint: &str) -> Result<Option<MetadataFields>, PoolError> {
        // Metaplex's canonical PDA is derived from ["metadata", program id,
        // mint]. The raw-RPC implementation uses the equivalent indexed lookup
        // (mint memcmp at the Metadata account's documented byte 33) so it does
        // not need a Solana SDK or a second crypto dependency.
        // Source: https://developers.metaplex.com/token-metadata
        let accounts = self
            .get_program_accounts(
                METAPLEX_METADATA_PROGRAM,
                vec![RpcFilter::Memcmp {
                    memcmp: RpcMemcmp { offset: 33, bytes: mint.to_owned() },
                }],
            )
            .await?;
        for account in accounts {
            if let Some(fields) = decode_metaplex_metadata(account.data()) {
                return Ok(Some(fields));
            }
        }
        Ok(None)
    }
}

fn apply_token_metadata(pool: &mut PoolInfo, metadata: &HashMap<String, TokenMeta>) {
    for side in [&mut pool.base, &mut pool.quote] {
        if let Some(meta) = metadata.get(&side.address) {
            if side.symbol.is_none() {
                side.symbol = meta.symbol.clone();
            }
            if side.decimals.is_none() {
                side.decimals = meta.decimals;
            }
        }
    }
}

#[async_trait]
impl PoolReader for SolanaReader {
    fn chain(&self) -> Chain {
        Chain::Solana
    }
    async fn record_position(&self) -> Result<(), PoolError> {
        let _: u64 = self.rpc("getSlot", json!([{ "commitment": "confirmed" }])).await?;
        Ok(())
    }

    async fn read_pool(&self, address: &str) -> Result<PoolInfo, PoolError> {
        self.read_pool_impl(address).await
    }

    async fn token_meta(&self, address: &str) -> Result<TokenMeta, PoolError> {
        SolanaReader::token_meta(self, address).await
    }
    async fn wallet_holdings(
        &self,
        owner: &str,
        known_tokens: &[String],
    ) -> Result<Vec<WalletHolding>, PoolError> {
        SolanaReader::wallet_holdings(self, owner, known_tokens).await
    }

    async fn pools_for_token(
        &self,
        token: &str,
        candidate_quotes: &[String],
    ) -> Result<Vec<PoolInfo>, PoolError> {
        SolanaReader::pools_for_token(self, token, candidate_quotes).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPool {
    pub pool: String,
    pub dex: &'static str,
    pub base_mint: String,
    pub quote_mint: String,
    pub base_vault: String,
    pub quote_vault: String,
}

#[derive(Debug, Serialize)]
struct RpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    params: Value,
}

#[derive(Debug, Deserialize)]
struct RpcEnvelope {
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    message: String,
    #[serde(default)]
    data: Option<Value>,
}
impl RpcError {
    fn is_retryable(&self) -> bool {
        matches!(self.code, Some(429 | -32_005 | -32_429))
            || self.message.to_ascii_lowercase().contains("rate limit")
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "JSON-RPC error")?;
        if let Some(code) = self.code {
            write!(formatter, " {code}")?;
        }
        write!(formatter, ": {}", self.message)?;
        if let Some(data) = &self.data {
            write!(formatter, " ({data})")?;
        }
        Ok(())
    }
}

async fn backoff(attempt: usize) {
    const DELAYS_MS: [u64; RPC_MAX_ATTEMPTS - 1] = [250, 1_000, 3_000];
    tokio::time::sleep(Duration::from_millis(DELAYS_MS[attempt])).await;
}

fn http_rpc_error(method: &str, status: StatusCode, body: &str) -> PoolError {
    let detail = serde_json::from_str::<RpcEnvelope>(body)
        .ok()
        .and_then(|envelope| envelope.error)
        .map(|error| error.to_string())
        .unwrap_or_else(|| format!("body: {body}"));
    PoolError::Reader(format!("{method}: HTTP {status}: {detail}"))
}
#[derive(Debug, Deserialize)]
struct RpcContext<T> {
    value: T,
}

#[derive(Debug, Clone)]
struct RpcAccount {
    owner: String,
    data: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize)]
struct RpcProgramAccount {
    pubkey: String,
    account: RpcAccount,
}
impl RpcProgramAccount {
    fn owner(&self) -> &str {
        &self.account.owner
    }

    fn data(&self) -> &[u8] {
        &self.account.data
    }
}
#[derive(Debug, Clone, Deserialize)]
struct TokenBalanceResponse {
    value: TokenAmount,
}

#[derive(Debug, Clone, Deserialize)]
struct TokenAmount {
    amount: String,
    decimals: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MetadataFields {
    name: Option<String>,
    symbol: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(untagged)]
enum RpcFilter {
    DataSize {
        #[serde(rename = "dataSize")]
        data_size: usize,
    },
    Memcmp {
        memcmp: RpcMemcmp,
    },
}

#[derive(Debug, Serialize, Clone)]
struct RpcMemcmp {
    offset: usize,
    bytes: String,
}

fn decode_pump_bonding_curve(data: &[u8]) -> Result<String, PoolError> {
    if data.len() != PUMP_BONDING_CURVE_DATA_SIZE || data[..8] != PUMP_BONDING_CURVE_DISCRIMINATOR {
        return Err(PoolError::Unknown("invalid Pump.fun BondingCurve account layout".to_owned()));
    }
    pubkey_at(data, PUMP_BONDING_CURVE_QUOTE_MINT_OFFSET)
}

fn decode_pumpswap(pool: &str, data: &[u8]) -> Result<DecodedPool, PoolError> {
    if data.len() < PUMPSWAP_DATA_SIZE || data[..8] != PUMPSWAP_POOL_DISCRIMINATOR {
        return Err(PoolError::Unknown("invalid PumpSwap Pool account layout".to_owned()));
    }
    Ok(DecodedPool {
        pool: pool.to_owned(),
        dex: "pumpswap",
        base_mint: pubkey_at(data, PUMPSWAP_BASE_MINT_OFFSET)?,
        quote_mint: pubkey_at(data, PUMPSWAP_QUOTE_MINT_OFFSET)?,
        base_vault: pubkey_at(data, PUMPSWAP_BASE_VAULT_OFFSET)?,
        quote_vault: pubkey_at(data, PUMPSWAP_QUOTE_VAULT_OFFSET)?,
    })
}

fn decode_raydium_amm(pool: &str, data: &[u8]) -> Result<DecodedPool, PoolError> {
    if data.len() < RAYDIUM_AMM_DATA_SIZE {
        return Err(PoolError::Unknown("invalid Raydium AMM v4 account layout".to_owned()));
    }
    Ok(DecodedPool {
        pool: pool.to_owned(),
        dex: "raydium-amm",
        base_mint: pubkey_at(data, RAYDIUM_AMM_COIN_MINT_OFFSET)?,
        quote_mint: pubkey_at(data, RAYDIUM_AMM_PC_MINT_OFFSET)?,
        base_vault: pubkey_at(data, RAYDIUM_AMM_COIN_VAULT_OFFSET)?,
        quote_vault: pubkey_at(data, RAYDIUM_AMM_PC_VAULT_OFFSET)?,
    })
}

fn decode_raydium_cpmm(pool: &str, data: &[u8]) -> Result<DecodedPool, PoolError> {
    if data.len() < RAYDIUM_CPMM_DATA_SIZE {
        return Err(PoolError::Unknown("invalid Raydium CPMM account layout".to_owned()));
    }
    Ok(DecodedPool {
        pool: pool.to_owned(),
        dex: "raydium-cpmm",
        base_mint: pubkey_at(data, RAYDIUM_CPMM_TOKEN_0_MINT_OFFSET)?,
        quote_mint: pubkey_at(data, RAYDIUM_CPMM_TOKEN_1_MINT_OFFSET)?,
        base_vault: pubkey_at(data, RAYDIUM_CPMM_TOKEN_0_VAULT_OFFSET)?,
        quote_vault: pubkey_at(data, RAYDIUM_CPMM_TOKEN_1_VAULT_OFFSET)?,
    })
}

fn decode_raydium_clmm(pool: &str, data: &[u8]) -> Result<DecodedPool, PoolError> {
    if data.len() < RAYDIUM_CLMM_DATA_SIZE || data[..8] != RAYDIUM_CLMM_POOL_DISCRIMINATOR {
        return Err(PoolError::Unknown("invalid Raydium CLMM account layout".to_owned()));
    }
    Ok(DecodedPool {
        pool: pool.to_owned(),
        dex: "raydium-clmm",
        base_mint: pubkey_at(data, RAYDIUM_CLMM_TOKEN_MINT_0_OFFSET)?,
        quote_mint: pubkey_at(data, RAYDIUM_CLMM_TOKEN_MINT_1_OFFSET)?,
        base_vault: pubkey_at(data, RAYDIUM_CLMM_TOKEN_VAULT_0_OFFSET)?,
        quote_vault: pubkey_at(data, RAYDIUM_CLMM_TOKEN_VAULT_1_OFFSET)?,
    })
}

fn decode_orca_whirlpool(pool: &str, data: &[u8]) -> Result<DecodedPool, PoolError> {
    if data.len() < ORCA_WHIRLPOOL_DATA_SIZE || data[..8] != ORCA_WHIRLPOOL_DISCRIMINATOR {
        return Err(PoolError::Unknown("invalid Orca Whirlpool account layout".to_owned()));
    }
    Ok(DecodedPool {
        pool: pool.to_owned(),
        dex: "orca-whirlpool",
        base_mint: pubkey_at(data, ORCA_WHIRLPOOL_TOKEN_MINT_A_OFFSET)?,
        quote_mint: pubkey_at(data, ORCA_WHIRLPOOL_TOKEN_MINT_B_OFFSET)?,
        base_vault: pubkey_at(data, ORCA_WHIRLPOOL_TOKEN_VAULT_A_OFFSET)?,
        quote_vault: pubkey_at(data, ORCA_WHIRLPOOL_TOKEN_VAULT_B_OFFSET)?,
    })
}

fn decode_meteora_dlmm(pool: &str, data: &[u8]) -> Result<DecodedPool, PoolError> {
    if data.len() < METEORA_DLMM_DATA_SIZE || data[..8] != METEORA_DLMM_LB_PAIR_DISCRIMINATOR {
        return Err(PoolError::Unknown("invalid Meteora DLMM account layout".to_owned()));
    }
    Ok(DecodedPool {
        pool: pool.to_owned(),
        dex: "meteora-dlmm",
        base_mint: pubkey_at(data, METEORA_DLMM_TOKEN_X_MINT_OFFSET)?,
        quote_mint: pubkey_at(data, METEORA_DLMM_TOKEN_Y_MINT_OFFSET)?,
        base_vault: pubkey_at(data, METEORA_DLMM_RESERVE_X_OFFSET)?,
        quote_vault: pubkey_at(data, METEORA_DLMM_RESERVE_Y_OFFSET)?,
    })
}

fn pubkey_at(data: &[u8], offset: usize) -> Result<String, PoolError> {
    let bytes = data
        .get(offset..offset + 32)
        .ok_or_else(|| PoolError::Unknown("pool account ended before public key".to_owned()))?;
    Ok(bs58::encode(bytes).into_string())
}

fn canonical_address(address: &str) -> Result<String, PoolError> {
    let bytes = bs58::decode(address).into_vec().map_err(|_| PoolError::InvalidAddress)?;
    if bytes.len() != 32 {
        return Err(PoolError::InvalidAddress);
    }
    Ok(bs58::encode(bytes).into_string())
}

fn decode_base64(input: &str) -> Result<Vec<u8>, PoolError> {
    let bytes: Vec<u8> = input.bytes().filter(|byte| !byte.is_ascii_whitespace()).collect();
    if !bytes.len().is_multiple_of(4) {
        return Err(PoolError::Reader("invalid base64 account data length".to_owned()));
    }
    let mut output = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.as_slice().as_chunks::<4>().0 {
        let padding_two = chunk[2] == b'=';
        let padding_one = chunk[3] == b'=';
        if padding_two && !padding_one {
            return Err(PoolError::Reader("invalid base64 padding".to_owned()));
        }
        let value = |byte: u8| -> Result<u8, PoolError> {
            match byte {
                b'A'..=b'Z' => Ok(byte - b'A'),
                b'a'..=b'z' => Ok(byte - b'a' + 26),
                b'0'..=b'9' => Ok(byte - b'0' + 52),
                b'+' => Ok(62),
                b'/' => Ok(63),
                b'=' => Ok(0),
                _ => Err(PoolError::Reader("invalid base64 account data".to_owned())),
            }
        };
        let a = value(chunk[0])?;
        let b = value(chunk[1])?;
        let c = value(chunk[2])?;
        let d = value(chunk[3])?;
        output.push((a << 2) | (b >> 4));
        if !padding_two {
            output.push((b << 4) | (c >> 2));
        }
        if !padding_one {
            output.push((c << 6) | d);
        }
    }
    Ok(output)
}
fn token_account_holding(data: &[u8]) -> Option<(String, u64)> {
    if data.len() < 72 {
        return None;
    }
    let mint = bs58::encode(&data[..32]).into_string();
    let amount = u64::from_le_bytes(data[64..72].try_into().ok()?);
    Some((mint, amount))
}

fn token_account_amount(data: &[u8]) -> Option<TokenAmount> {
    if data.len() < 72 {
        return None;
    }
    Some(TokenAmount {
        amount: u64::from_le_bytes(data[64..72].try_into().ok()?).to_string(),
        decimals: 0,
    })
}

fn decode_metaplex_metadata(data: &[u8]) -> Option<MetadataFields> {
    if data.len() < 65 || data[0] == 0 {
        return None;
    }
    let mut offset = 65;
    let name = borsh_string(data, &mut offset)?;
    let symbol = borsh_string(data, &mut offset)?;
    Some(MetadataFields { name: clean_text(name), symbol: clean_text(symbol) })
}

fn token_metadata_extension(data: &[u8]) -> Result<Option<MetadataFields>, PoolError> {
    // Token-2022 mint accounts may reserve zero-filled TLV space before the
    // metadata extension. Scan for the extension header instead of assuming
    // every four-byte slot is a contiguous TLV.
    let mut offset = 82;
    while offset + 4 <= data.len() {
        let extension_type = u16::from_le_bytes([data[offset], data[offset + 1]]);
        let length = usize::from(u16::from_le_bytes([data[offset + 2], data[offset + 3]]));
        let payload_start = offset + 4;
        let Some(end) = payload_start.checked_add(length) else {
            return Err(PoolError::Reader("Token-2022 extension length overflow".to_owned()));
        };
        if extension_type == 19
            && end <= data.len()
            && let Some((name, symbol)) = token_metadata_strings(&data[payload_start..end], 64)
                .or_else(|| token_metadata_strings(&data[payload_start..end], 32))
        {
            return Ok(Some(MetadataFields { name, symbol }));
        }
        offset += 1;
    }
    Ok(None)
}

fn token_metadata_strings(data: &[u8], start: usize) -> Option<(Option<String>, Option<String>)> {
    let mut cursor = start;
    let name = borsh_string(data, &mut cursor).and_then(clean_text);
    let symbol = borsh_string(data, &mut cursor).and_then(clean_text);
    if name.is_some() || symbol.is_some() { Some((name, symbol)) } else { None }
}

fn borsh_string<'a>(data: &'a [u8], offset: &mut usize) -> Option<&'a [u8]> {
    let length =
        usize::try_from(u32::from_le_bytes(data.get(*offset..*offset + 4)?.try_into().ok()?))
            .ok()?;
    *offset += 4;
    let end = offset.checked_add(length)?;
    let value = data.get(*offset..end)?;
    *offset = end;
    Some(value)
}

fn clean_text(value: &[u8]) -> Option<String> {
    let value = String::from_utf8_lossy(value).trim_matches(char::from(0)).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn decimal_cmp(left: Option<&str>, right: Option<&str>) -> std::cmp::Ordering {
    match (left, right) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(left), Some(right)) => {
            let left = left.trim_start_matches('0');
            let right = right.trim_start_matches('0');
            let left = if left.is_empty() { "0" } else { left };
            let right = if right.is_empty() { "0" } else { right };
            left.len().cmp(&right.len()).then_with(|| left.cmp(right))
        }
    }
}

impl<'de> Deserialize<'de> for RpcAccount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireAccount {
            owner: String,
            data: (String, String),
        }
        let wire = WireAccount::deserialize(deserializer)?;
        let data = decode_base64(&wire.data.0).map_err(serde::de::Error::custom)?;
        Ok(Self { owner: wire.owner, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn filled(size: usize, value: u8) -> Vec<u8> {
        vec![value; size]
    }

    #[test]
    fn decodes_wallet_token_account_fixture() {
        let mut data = vec![0u8; 165];
        data[..32].copy_from_slice(&[7u8; 32]);
        data[64..72].copy_from_slice(&1_234_567u64.to_le_bytes());
        let (mint, amount) = token_account_holding(&data).expect("token account");
        assert_eq!(mint, bs58::encode([7u8; 32]).into_string());
        assert_eq!(amount, 1_234_567);
    }

    #[test]
    fn decodes_all_pool_layouts() {
        let mut pump = filled(PUMPSWAP_DATA_SIZE, 1);
        pump[..8].copy_from_slice(&PUMPSWAP_POOL_DISCRIMINATOR);
        let decoded = decode_pumpswap("11111111111111111111111111111111", &pump).expect("pump");
        assert_eq!(decoded.dex, "pumpswap");
        assert_eq!(decoded.base_mint, bs58::encode([1u8; 32]).into_string());
        assert_eq!(decoded.quote_vault, bs58::encode([1u8; 32]).into_string());

        let raydium = filled(RAYDIUM_AMM_DATA_SIZE, 2);
        let decoded =
            decode_raydium_amm("11111111111111111111111111111111", &raydium).expect("amm");
        assert_eq!(decoded.dex, "raydium-amm");
        assert_eq!(decoded.base_mint, bs58::encode([2u8; 32]).into_string());

        let cpmm = filled(RAYDIUM_CPMM_DATA_SIZE, 3);
        let decoded = decode_raydium_cpmm("11111111111111111111111111111111", &cpmm).expect("cpmm");
        assert_eq!(decoded.dex, "raydium-cpmm");
        assert_eq!(decoded.quote_mint, bs58::encode([3u8; 32]).into_string());

        let mut raydium_clmm = filled(RAYDIUM_CLMM_DATA_SIZE, 4);
        raydium_clmm[..8].copy_from_slice(&RAYDIUM_CLMM_POOL_DISCRIMINATOR);
        let decoded =
            decode_raydium_clmm("11111111111111111111111111111111", &raydium_clmm).expect("clmm");
        assert_eq!(decoded.dex, "raydium-clmm");
        assert_eq!(decoded.base_mint, bs58::encode([4u8; 32]).into_string());
        assert_eq!(decoded.quote_vault, bs58::encode([4u8; 32]).into_string());

        let mut whirlpool = filled(ORCA_WHIRLPOOL_DATA_SIZE, 5);
        whirlpool[..8].copy_from_slice(&ORCA_WHIRLPOOL_DISCRIMINATOR);
        let decoded = decode_orca_whirlpool("11111111111111111111111111111111", &whirlpool)
            .expect("whirlpool");
        assert_eq!(decoded.quote_vault, bs58::encode([5u8; 32]).into_string());

        let mut meteora = filled(METEORA_DLMM_DATA_SIZE, 6);
        meteora[..8].copy_from_slice(&METEORA_DLMM_LB_PAIR_DISCRIMINATOR);
        let decoded =
            decode_meteora_dlmm("11111111111111111111111111111111", &meteora).expect("meteora");
        assert_eq!(decoded.dex, "meteora-dlmm");
        assert_eq!(decoded.base_mint, bs58::encode([6u8; 32]).into_string());
        assert_eq!(decoded.quote_vault, bs58::encode([6u8; 32]).into_string());
    }

    #[test]
    fn decodes_recorded_xstocks_pool_fixtures() {
        let fixtures = [
            (
                include_str!("../../tests/fixtures/solana/raydium_clmm_pool.json"),
                "49iMatQtoyabsYAQc8GafVq6aeBFVDxSRH44oiatyyw6",
                RAYDIUM_CLMM_PROGRAM,
                RAYDIUM_CLMM_DATA_SIZE,
                "Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh",
                "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            ),
            (
                include_str!("../../tests/fixtures/solana/orca_whirlpool_pool.json"),
                "6R4r93V5fcMzc13CL2enEepDSYcr4Qx3ptZBDwudTXCo",
                ORCA_WHIRLPOOL_PROGRAM,
                ORCA_WHIRLPOOL_DATA_SIZE,
                "Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh",
                "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            ),
            (
                include_str!("../../tests/fixtures/solana/meteora_dlmm_pool.json"),
                "FCn5zw4gAcfRpQgst5ThFuzBGXbbJ6RocVErgC4vJ9j1",
                METEORA_DLMM_PROGRAM,
                METEORA_DLMM_DATA_SIZE,
                "Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh",
                "So11111111111111111111111111111111111111112",
            ),
        ];
        for (fixture, address, owner, size, stock_mint, other_mint) in fixtures {
            let fixture: Value = serde_json::from_str(fixture).expect("pool fixture");
            let actual_owner = fixture["result"]["value"]["owner"].as_str().expect("owner");
            assert_eq!(actual_owner, owner);
            let encoded = fixture["result"]["value"]["data"][0].as_str().expect("pool data");
            let data = decode_base64(encoded).expect("pool base64");
            assert_eq!(data.len(), size);
            let decoded =
                SolanaReader::decode_account(address, actual_owner, &data).expect("decode");
            assert!(decoded.base_mint == stock_mint || decoded.quote_mint == stock_mint);
            assert!(decoded.base_mint == other_mint || decoded.quote_mint == other_mint);
        }
    }

    #[test]
    fn decodes_recorded_pumpfun_fixtures() {
        let pool: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/solana/pumpfun_pool.json"))
                .expect("pool fixture");
        let encoded = pool["result"]["value"]["data"][0].as_str().expect("pool data");
        let data = decode_base64(encoded).expect("pool base64");
        assert_eq!(data.len(), PUMP_BONDING_CURVE_DATA_SIZE);
        assert_eq!(
            decode_pump_bonding_curve(&data).expect("bonding curve"),
            "Xsc9qvGR1efVDFGLrVsmkzv3qi45LTBjeUKSPmx9qEh"
        );

        let base_balance: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/solana/pumpfun_base_vault_balance.json"
        ))
        .expect("base balance fixture");
        assert_eq!(base_balance["result"]["value"]["decimals"], 6);
        let quote_balance: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/solana/pumpfun_quote_vault_balance.json"
        ))
        .expect("quote balance fixture");
        assert_eq!(quote_balance["result"]["value"]["decimals"], 8);

        let base_supply: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/solana/pumpfun_base_supply.json"
        ))
        .expect("base supply fixture");
        assert_eq!(base_supply["result"]["value"]["amount"], "999999999999999");
        let nvdax_supply: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/solana/nvdax_supply.json"))
                .expect("NVDAx supply fixture");
        assert_eq!(nvdax_supply["result"]["value"]["amount"], "32127563954377");

        for path in [
            include_str!("../../tests/fixtures/solana/pumpfun_base_metaplex.json"),
            include_str!("../../tests/fixtures/solana/nvdax_metaplex.json"),
        ] {
            let metadata: Value = serde_json::from_str(path).expect("metadata fixture");
            assert!(metadata["result"].as_array().is_some_and(Vec::is_empty));
        }

        let base_mint: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/solana/pumpfun_base_mint.json"
        ))
        .expect("base mint fixture");
        let base_data = decode_base64(
            base_mint["result"]["value"]["data"][0].as_str().expect("base mint data"),
        )
        .expect("base mint base64");
        let base_metadata = token_metadata_extension(&base_data).expect("base metadata");
        assert_eq!(
            base_metadata,
            Some(MetadataFields {
                name: Some("NokiaBrick".to_owned()),
                symbol: Some("BRICK".to_owned())
            })
        );
        let nvdax_mint: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/solana/nvdax_mint.json"))
                .expect("NVDAx mint fixture");
        let nvdax_data = decode_base64(
            nvdax_mint["result"]["value"]["data"][0].as_str().expect("NVDAx mint data"),
        )
        .expect("NVDAx mint base64");
        let nvdax_metadata = token_metadata_extension(&nvdax_data).expect("NVDAx metadata");
        assert_eq!(
            nvdax_metadata,
            Some(MetadataFields {
                name: Some("NVIDIA xStock".to_owned()),
                symbol: Some("NVDAx".to_owned())
            })
        );
    }

    #[test]
    fn decodes_metadata_and_token2022_strings() {
        let mut data = vec![1u8; 65];
        data.extend_from_slice(&(4u32.to_le_bytes()));
        data.extend_from_slice(b"Name");
        data.extend_from_slice(&(3u32.to_le_bytes()));
        data.extend_from_slice(b"SYM");
        assert_eq!(
            decode_metaplex_metadata(&data),
            Some(MetadataFields { name: Some("Name".to_owned()), symbol: Some("SYM".to_owned()) })
        );

        let mut mint = vec![0u8; 82];
        let mut extension = vec![0u8; 64];
        extension.extend_from_slice(&(4u32.to_le_bytes()));
        extension.extend_from_slice(b"Name");
        extension.extend_from_slice(&(3u32.to_le_bytes()));
        extension.extend_from_slice(b"SYM");
        mint.extend_from_slice(&19u16.to_le_bytes());
        mint.extend_from_slice(&(u16::try_from(extension.len()).expect("length")).to_le_bytes());
        mint.extend_from_slice(&extension);
        assert_eq!(
            token_metadata_extension(&mint).expect("extension"),
            Some(MetadataFields { name: Some("Name".to_owned()), symbol: Some("SYM".to_owned()) })
        );
    }

    #[tokio::test]
    async fn orders_program_fixture_pools_by_quote_balance() {
        let token = bs58::encode([9u8; 32]).into_string();
        let quote = bs58::encode([8u8; 32]).into_string();
        let mut first = filled(PUMPSWAP_DATA_SIZE, 0);
        first[..8].copy_from_slice(&PUMPSWAP_POOL_DISCRIMINATOR);
        first[PUMPSWAP_BASE_MINT_OFFSET..PUMPSWAP_BASE_MINT_OFFSET + 32].copy_from_slice(&[9; 32]);
        first[PUMPSWAP_QUOTE_MINT_OFFSET..PUMPSWAP_QUOTE_MINT_OFFSET + 32]
            .copy_from_slice(&[8; 32]);
        first[PUMPSWAP_BASE_VAULT_OFFSET..PUMPSWAP_BASE_VAULT_OFFSET + 32]
            .copy_from_slice(&[2; 32]);
        first[PUMPSWAP_QUOTE_VAULT_OFFSET..PUMPSWAP_QUOTE_VAULT_OFFSET + 32]
            .copy_from_slice(&[3; 32]);
        let mut second = first.clone();
        second[PUMPSWAP_BASE_VAULT_OFFSET..PUMPSWAP_BASE_VAULT_OFFSET + 32]
            .copy_from_slice(&[5; 32]);
        second[PUMPSWAP_QUOTE_VAULT_OFFSET..PUMPSWAP_QUOTE_VAULT_OFFSET + 32]
            .copy_from_slice(&[6; 32]);
        let first_pool = bs58::encode([1u8; 32]).into_string();
        let second_pool = bs58::encode([4u8; 32]).into_string();

        let mut responses = VecDeque::new();
        responses.push_back(rpc_program_response(vec![
            rpc_program_account(&first_pool, PUMPSWAP_PROGRAM, &first),
            rpc_program_account(&second_pool, PUMPSWAP_PROGRAM, &second),
        ]));
        for _ in 0..11 {
            responses.push_back(rpc_program_response(Vec::new()));
        }
        responses.push_back(rpc_multiple_response(vec![
            rpc_token_account(&bs58::encode([2u8; 32]).into_string(), 10),
            rpc_token_account(&bs58::encode([3u8; 32]).into_string(), 100),
            rpc_token_account(&bs58::encode([5u8; 32]).into_string(), 20),
            rpc_token_account(&bs58::encode([6u8; 32]).into_string(), 200),
        ]));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move { serve_responses(listener, responses).await });
        let reader = SolanaReader::new(format!("http://{address}"));
        let pools = reader.pools_for_token(&token, &[quote]).await.expect("pools");
        assert_eq!(pools.len(), 2);
        assert_eq!(pools[0].pool, second_pool);
        assert_eq!(pools[0].quote.balance.as_deref(), Some("200"));
        assert_eq!(pools[1].quote.balance.as_deref(), Some("100"));
        server.await.expect("server").expect("responses");
    }

    #[tokio::test]
    async fn discovers_new_program_pools_for_token() {
        let token = bs58::encode([9u8; 32]).into_string();
        let quote = bs58::encode([8u8; 32]).into_string();
        let mut raydium_clmm = filled(RAYDIUM_CLMM_DATA_SIZE, 0);
        raydium_clmm[..8].copy_from_slice(&RAYDIUM_CLMM_POOL_DISCRIMINATOR);
        raydium_clmm[RAYDIUM_CLMM_TOKEN_MINT_0_OFFSET..RAYDIUM_CLMM_TOKEN_MINT_0_OFFSET + 32]
            .copy_from_slice(&[9; 32]);
        raydium_clmm[RAYDIUM_CLMM_TOKEN_MINT_1_OFFSET..RAYDIUM_CLMM_TOKEN_MINT_1_OFFSET + 32]
            .copy_from_slice(&[8; 32]);
        raydium_clmm[RAYDIUM_CLMM_TOKEN_VAULT_0_OFFSET..RAYDIUM_CLMM_TOKEN_VAULT_0_OFFSET + 32]
            .copy_from_slice(&[10; 32]);
        raydium_clmm[RAYDIUM_CLMM_TOKEN_VAULT_1_OFFSET..RAYDIUM_CLMM_TOKEN_VAULT_1_OFFSET + 32]
            .copy_from_slice(&[11; 32]);
        let mut whirlpool = filled(ORCA_WHIRLPOOL_DATA_SIZE, 0);
        whirlpool[..8].copy_from_slice(&ORCA_WHIRLPOOL_DISCRIMINATOR);
        whirlpool[ORCA_WHIRLPOOL_TOKEN_MINT_A_OFFSET..ORCA_WHIRLPOOL_TOKEN_MINT_A_OFFSET + 32]
            .copy_from_slice(&[9; 32]);
        whirlpool[ORCA_WHIRLPOOL_TOKEN_MINT_B_OFFSET..ORCA_WHIRLPOOL_TOKEN_MINT_B_OFFSET + 32]
            .copy_from_slice(&[8; 32]);
        whirlpool[ORCA_WHIRLPOOL_TOKEN_VAULT_A_OFFSET..ORCA_WHIRLPOOL_TOKEN_VAULT_A_OFFSET + 32]
            .copy_from_slice(&[12; 32]);
        whirlpool[ORCA_WHIRLPOOL_TOKEN_VAULT_B_OFFSET..ORCA_WHIRLPOOL_TOKEN_VAULT_B_OFFSET + 32]
            .copy_from_slice(&[13; 32]);
        let mut meteora = filled(METEORA_DLMM_DATA_SIZE, 0);
        meteora[..8].copy_from_slice(&METEORA_DLMM_LB_PAIR_DISCRIMINATOR);
        meteora[METEORA_DLMM_TOKEN_X_MINT_OFFSET..METEORA_DLMM_TOKEN_X_MINT_OFFSET + 32]
            .copy_from_slice(&[9; 32]);
        meteora[METEORA_DLMM_TOKEN_Y_MINT_OFFSET..METEORA_DLMM_TOKEN_Y_MINT_OFFSET + 32]
            .copy_from_slice(&[8; 32]);
        meteora[METEORA_DLMM_RESERVE_X_OFFSET..METEORA_DLMM_RESERVE_X_OFFSET + 32]
            .copy_from_slice(&[14; 32]);
        meteora[METEORA_DLMM_RESERVE_Y_OFFSET..METEORA_DLMM_RESERVE_Y_OFFSET + 32]
            .copy_from_slice(&[15; 32]);

        let mut responses = VecDeque::new();
        for _ in 0..6 {
            responses.push_back(rpc_program_response(Vec::new()));
        }
        responses.push_back(rpc_program_response(vec![rpc_program_account(
            &bs58::encode([20u8; 32]).into_string(),
            RAYDIUM_CLMM_PROGRAM,
            &raydium_clmm,
        )]));
        responses.push_back(rpc_program_response(Vec::new()));
        responses.push_back(rpc_program_response(vec![rpc_program_account(
            &bs58::encode([21u8; 32]).into_string(),
            ORCA_WHIRLPOOL_PROGRAM,
            &whirlpool,
        )]));
        responses.push_back(rpc_program_response(Vec::new()));
        responses.push_back(rpc_program_response(vec![rpc_program_account(
            &bs58::encode([22u8; 32]).into_string(),
            METEORA_DLMM_PROGRAM,
            &meteora,
        )]));
        responses.push_back(rpc_program_response(Vec::new()));
        responses.push_back(rpc_multiple_response(vec![
            rpc_token_account(&bs58::encode([10u8; 32]).into_string(), 20),
            rpc_token_account(&bs58::encode([11u8; 32]).into_string(), 100),
            rpc_token_account(&bs58::encode([12u8; 32]).into_string(), 30),
            rpc_token_account(&bs58::encode([13u8; 32]).into_string(), 200),
            rpc_token_account(&bs58::encode([14u8; 32]).into_string(), 40),
            rpc_token_account(&bs58::encode([15u8; 32]).into_string(), 300),
        ]));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move { serve_responses(listener, responses).await });
        let reader = SolanaReader::new(format!("http://{address}"));
        let pools = reader.pools_for_token(&token, &[quote]).await.expect("pools");
        assert_eq!(pools.len(), 3);
        assert_eq!(pools[0].dex, "meteora-dlmm");
        assert_eq!(pools[1].dex, "orca-whirlpool");
        assert_eq!(pools[2].dex, "raydium-clmm");
        assert_eq!(pools[0].quote.balance.as_deref(), Some("300"));
        server.await.expect("server").expect("responses");
    }

    fn rpc_program_account(pubkey: &str, owner: &str, data: &[u8]) -> Value {
        json!({"pubkey": pubkey, "account": {"owner": owner, "data": [base64_for_test(data), "base64"]}})
    }

    fn rpc_program_response(accounts: Vec<Value>) -> String {
        serde_json::to_string(&json!({"jsonrpc":"2.0", "id":1, "result": accounts})).expect("json")
    }

    fn rpc_token_account(address: &str, amount: u64) -> Value {
        let mut data = vec![0u8; 72];
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        json!({"address": address, "owner": "11111111111111111111111111111111", "data": [base64_for_test(&data), "base64"]})
    }

    fn rpc_multiple_response(accounts: Vec<Value>) -> String {
        let values: Vec<Value> = accounts
            .into_iter()
            .map(|account| json!({"owner": account["owner"], "data": account["data"]}))
            .collect();
        serde_json::to_string(
            &json!({"jsonrpc":"2.0", "id":1, "result": {"context": {"slot": 0}, "value": values}}),
        )
        .expect("json")
    }

    async fn serve_responses(
        listener: TcpListener,
        mut responses: VecDeque<String>,
    ) -> Result<(), std::io::Error> {
        while let Some(response) = responses.pop_front() {
            let (mut stream, _) = listener.accept().await?;
            let mut request = vec![0u8; 16 * 1024];
            let _ = stream.read(&mut request).await?;
            let message = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response.len(),
                response
            );
            stream.write_all(message.as_bytes()).await?;
        }
        Ok(())
    }

    fn base64_for_test(bytes: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut output = String::new();
        for chunk in bytes.chunks(3) {
            let a = chunk[0];
            let b = *chunk.get(1).unwrap_or(&0);
            let c = *chunk.get(2).unwrap_or(&0);
            output.push(TABLE[(a >> 2) as usize] as char);
            output.push(TABLE[((a & 3) << 4 | b >> 4) as usize] as char);
            output.push(if chunk.len() > 1 {
                TABLE[((b & 15) << 2 | c >> 6) as usize] as char
            } else {
                '='
            });
            output.push(if chunk.len() > 2 { TABLE[(c & 63) as usize] as char } else { '=' });
        }
        output
    }
}
