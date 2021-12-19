# aqua-dex

![alt text](https://atellix.network/images/atellix/aqua_dex_logo.png)

AquaDEX powered by the Infinitrade™ order matching engine

# Decentralized EXchange on Solana

## Administration and Trading Commands:

#### create_market

Create a new trading market.

1. agent_nonce: u8 - Bump seed of Token Agent
2. mkt_vault_nonce: u8 - Bump seed of Market Vault associated token account
3. prc_vault_nonce: u8 - Bump seed of Pricing Vault associated token account
4. expire_enable: bool - Enable orders to expire
5. expire_min: i64 - Minimum time (in seconds) before an order can expire. Must be 1 second or greater.

#### limit_bid

Place a "bid" limit order to purchase market tokens at a certain maximum price, or less (in pricing tokens).

1. rollover: bool - Perform settlement log rollover
2. quantity: u64 - Limit bid quantity (in market tokens)
3. price: u64 - Limit bid price (in pricing tokens)
4. post: bool - Post the order order to the orderbook, otherwise it must be filled immediately
5. fill: bool - Require orders that are not posted to be filled completely (ignored for posted orders)
6. expires: i64 - Unix timestamp for order expiration (must be in the future, must exceed minimum duration)

#### limit_ask

Place an "ask" limit order to sell market tokens at a certain minimum price, or more (in pricing tokens).

1. rollover: bool - Perform settlement log rollover
2. quantity: u64 - Limit ask quantity (in market tokens)
3. price: u64 - Limit ask price (in pricing tokens)
4. post: bool - Post the order order to the orderbook, otherwise it must be filled immediately
5. fill: bool - Require orders that are not posted to be filled completely (ignored for posted orders)
6. expires: i64 - Unix timestamp for order expiration (must be in the future, must exceed minimum duration)

#### market_bid

*coming soon*

#### market_ask

*coming soon*

#### cancel_order

Cancel a pending order, and withdraw tokens from the vault.

1. side: u8 - Orderbook side of cancelled order: 0 - Bid, 1 - Ask
2. order_id: u128 - Order ID to cancel

#### withdraw

Withdraw tokens from orders cleared by counter-parties.
