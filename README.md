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

## Create a market:

```javascript
    await aquadex.rpc.createMarket(
        marketAgent.nonce,
        tokenVault1.nonce,
        tokenVault2.nonce,
        true,                   // Expire enable
        new anchor.BN(1),       // Min expire
        {
            accounts: {
                market: market.publicKey,
                state: marketState.publicKey,
                agent: new PublicKey(marketAgent.pubkey),
                manager: provider.wallet.publicKey,
                mktMint: tokenMint1,
                mktVault: new PublicKey(tokenVault1.pubkey),
                prcMint: tokenMint2,
                prcVault: new PublicKey(tokenVault2.pubkey),
                orders: orders.publicKey,
                settleA: settle1.publicKey,
                settleB: settle2.publicKey,
                splTokenProg: TOKEN_PROGRAM_ID,
                ascTokenProg: SPL_ASSOCIATED_TOKEN,
                sysProg: SystemProgram.programId,
                sysRent: SYSVAR_RENT_PUBKEY,
            }
        }
    )
```

## Trade tokens:
```javascript
async function limitOrder(orderType, user, result, qty, price) {
    var userToken1 = await associatedTokenAddress(user.publicKey, tokenMint1)
    var userToken2 = await associatedTokenAddress(user.publicKey, tokenMint2)
    var mktSpec = await aquadex.account.market.fetch(marketPK)
    settle1PK = mktSpec.settleA
    settle2PK = mktSpec.settleB
    var params = {
        accounts: {
            market: marketPK,
            state: marketStatePK,
            agent: new PublicKey(marketAgent.pubkey),
            user: user.publicKey,
            userMktToken: new PublicKey(userToken1.pubkey),
            userPrcToken: new PublicKey(userToken2.pubkey),
            mktVault: new PublicKey(tokenVault1.pubkey),
            prcVault: new PublicKey(tokenVault2.pubkey),
            orders: ordersPK,
            settleA: settle1PK,
            settleB: settle2PK,
            result: result.publicKey,
            splTokenProg: TOKEN_PROGRAM_ID,
        },
        signers: [user, result],
    }
    var rollover = false
    var signers = [user, result]
    var tx = new anchor.web3.Transaction()
    if (mktSpec.logRollover) {
        console.log("--- PERFORMING SETTLEMENT LOG ROLLOVER ---")
        rollover = true
        var settle = anchor.web3.Keypair.generate()
        signers.push(settle)
        tx.add(anchor.web3.SystemProgram.createAccount({
            fromPubkey: provider.wallet.publicKey,
            newAccountPubkey: settle.publicKey,
            space: settleBytes,
            lamports: settleRent,
            programId: aquadexPK,
        }))
        params['remainingAccounts'] = [
            { pubkey: settle.publicKey, isWritable: true, isSigner: false },
        ]
    }
    if (orderType === 'bid') {
        tx.add(await aquadex.instruction.limitBid(
            rollover,                       // Rollover settlement log
            new anchor.BN(qty * 10000),     // Quantity
            new anchor.BN(price * 10000),   // Price
            true,
            false,
            new anchor.BN(0),               // Order expiry
            params,
        ))
    } else {
        tx.add(await aquadex.instruction.limitAsk(
            rollover,                       // Rollover settlement log
            new anchor.BN(qty * 10000),     // Quantity
            new anchor.BN(price * 10000),   // Price
            true,
            false,
            new anchor.BN(0),               // Order expiry
            params,
        ))
    }
    await provider.send(tx, signers)
}
```
