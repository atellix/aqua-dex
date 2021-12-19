# aqua-dex

![alt text](https://atellix.network/images/atellix/aqua_dex_logo.png)

AquaDEX powered by the Infinitrade™ order matching engine

# Decentralized EXchange on Solana

## Program Commands:

#### create_market

Create a new trading market.

1. agent_nonce: u8 - Bump seed of Token Agent
2. mkt_vault_nonce: u8 - Bump seed of Market Vault associated token account
3. prc_vault_nonce: u8 - Bump seed of Pricing Vault associated token account
4. expire_enable: bool - Enable orders to expire
5. expire_min: i64 - Minimum time (in seconds) before an order can expire. Must be 1 second or greater.

#### limit_bid

Place a "bid" limit order to purchase market tokens at a certain maximum price, or less (in pricing tokens).

#### limit_ask

Place an "ask" limit order to sell market tokens at a certain minimum price, or more (in pricing tokens).

#### market_bid

*coming soon*

#### market_ask

*coming soon*

#### cancel_order



Cancel an existing order.

#### withdraw

Withdraw tokens from orders cleared by counter-parties.
