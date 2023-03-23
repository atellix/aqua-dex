# aqua-dex

![alt text](https://media.atellix.net/aqua_dex_logo.png)

AquaDEX protocol. A limit orderbook matching engine.

# Decentralized EXchange on Solana

## Administration and Trading Commands:

### Notations:

"mkt" - Market Tokens (quantities)
"prc" - Pricing Tokens (prices)

#### create_market

Create a new trading market.

1. agent_nonce: u8 - Bump seed of Token Agent
2. mkt_vault_nonce: u8 - Bump seed of Market Vault associated token account
3. prc_vault_nonce: u8 - Bump seed of Pricing Vault associated token account
4. mkt_decimals: u8 - Decimals of the market token
5. prc_decimals: u8 - Decimals of the pricing token
6. mkt_mint_type: u8 - Mint type of the market token (0 = SPL, 1 = AST-1)
7. prc_mint_type: u8 - Mint type of the pricing token (0 = SPL, 1 = AST-1)
8. manager_actions: bool - 0: full self-custody markets; 1: enable "manager_cancel_order", "manager_withdraw" & "manager_vault_withdraw" functions
9. expire_enable: bool - Enable orders to expire
10. expire_min: bool - Minimum time (in seconds) before an order can expire. Must be 1 second or greater.
11. min_quantity: bool - Minimum quantity (can be 0)
12. tick_decimals: u8 - 10^X decimals in raw tokens will be rounded from midpoint
13. taker_fee: u8 - Taker commission fee (X / 10,000,000; or 1,000 = 1 basis point)
14. maker_rebate: u8 - Maker rebate (X / 10,000,000; or 1,000 = 1 basis point)
15. log_fee: u8 - Log fee (reserve space in settlement log; can be 0 when using "user vaults")
16. log_rebate: u8 - Log rebate (when closing settled position; can be 0 when using "user vaults")
17. log_reimburse: u8 - Log reimburse (for creating new settlement log accounts; can be 0 when using "user vaults")
18. mkt_vault_uuid: u128 - Market Vault UUID (for AST-1 security tokens only, otherwise: 0)
19. prc_vault_uuid: u128 - Pricing Vault UUID (for AST-1 security tokens only, otherwise: 0)

#### limit_bid

Place a "bid" limit order to purchase market tokens at a certain maximum price, or less (in pricing tokens).

1. quantity: u64 - Limit bid quantity (in market tokens)
2. price: u64 - Limit bid price (in pricing tokens)
3. post: bool - Post the order order to the orderbook, otherwise it must be filled immediately
4. fill: bool - Require orders that are not posted to be filled completely (ignored for posted orders)
5. expires: i64 - Unix timestamp for order expiration (must be in the future, must exceed minimum duration)
6. preview: bool - Do not execute just preview and return what would have happened
7. rollover: bool - Perform settlement log rollover

#### limit_ask

Place an "ask" limit order to sell market tokens at a certain minimum price, or more (in pricing tokens).

1. quantity: u64 - Limit bid quantity (in market tokens)
2. price: u64 - Limit bid price (in pricing tokens)
3. post: bool - Post the order order to the orderbook, otherwise it must be filled immediately
4. fill: bool - Require orders that are not posted to be filled completely (ignored for posted orders)
5. expires: i64 - Unix timestamp for order expiration (must be in the future, must exceed minimum duration)
6. preview: bool - Do not execute just preview and return what would have happened
7. rollover: bool - Perform settlement log rollover

#### cancel_order

Cancel a pending order, and withdraw tokens from the vault.

1. side: u8 - Orderbook side of cancelled order: 0 - Bid, 1 - Ask
2. order_id: u128 - Order ID to cancel

#### withdraw

Withdraw tokens from orders cleared by counter-parties.

## Create a market:

```javascript
    var mint1 = 'So11111111111111111111111111111111111111112'
    var mint2 = 'Gh9ZwEmdLJ8DscKNTkTqPbNwLNNBjuSzaG9Vp2KGtKJr'

    console.log("Mints: " + mint1 + " " + mint2)
    const tokenMint1 = new PublicKey(mint1)
    const tokenMint2 = new PublicKey(mint2)

    market = anchor.web3.Keypair.generate()
    marketPK = market.publicKey
    marketState = anchor.web3.Keypair.generate()
    orders = anchor.web3.Keypair.generate()
    tradeLog = anchor.web3.Keypair.generate()
    settle1 = anchor.web3.Keypair.generate()
    settle2 = anchor.web3.Keypair.generate()
    writeData['market'] = marketPK.toString()
    writeData['marketState'] = marketState.publicKey.toString()
    writeData['orders'] = orders.publicKey.toString()
    writeData['tradeLog'] = tradeLog.publicKey.toString()
    writeData['settle1'] = settle1.publicKey.toString()
    writeData['settle2'] = settle2.publicKey.toString()

    const ordersBytes = 226 + (16384 * 6)
    const ordersRent = await provider.connection.getMinimumBalanceForRentExemption(ordersBytes)

    const tradeLogBytes = 326 + (16384 * 1)
    const tradeLogRent = await provider.connection.getMinimumBalanceForRentExemption(tradeLogBytes)

    const settleBytes = 326 + (16384 * 6)
    const settleRent = await provider.connection.getMinimumBalanceForRentExemption(settleBytes)

    const marketAgent = await programAddress([marketPK.toBuffer()], aquadexPK)
    const marketAgentPK = new PublicKey(marketAgent.pubkey)
    const marketAdmin = await programAddress([marketPK.toBuffer(), Buffer.from('admin', 'utf8')], aquadexPK)
    const marketAdminPK = new PublicKey(marketAdmin.pubkey)
    const tokenVault1 = await associatedTokenAddress(marketAgentPK, tokenMint1)
    const tokenVault2 = await associatedTokenAddress(marketAgentPK, tokenMint2)

    var ta = new anchor.web3.Transaction()
    ta.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: marketPK,
        space: aquadex.account.market.size,
        lamports: await provider.connection.getMinimumBalanceForRentExemption(aquadex.account.market.size),
        programId: aquadexPK,
    }))
    ta.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: marketState.publicKey,
        space: aquadex.account.marketState.size,
        lamports: await provider.connection.getMinimumBalanceForRentExemption(aquadex.account.marketState.size),
        programId: aquadexPK,
    }))
    ta.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: orders.publicKey,
        space: ordersBytes,
        lamports: ordersRent,
        programId: aquadexPK,
    }))
    ta.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: tradeLog.publicKey,
        space: tradeLogBytes,
        lamports: tradeLogRent,
        programId: aquadexPK,
    }))
    ta.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: settle1.publicKey,
        space: settleBytes,
        lamports: settleRent,
        programId: aquadexPK,
    }))
    ta.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: settle2.publicKey,
        space: settleBytes,
        lamports: settleRent,
        programId: aquadexPK,
    }))
    console.log(await provider.sendAndConfirm(ta, [market, marketState, orders, tradeLog, settle1, settle2]))

    // Sleep at least 180 seconds to be sure the validator is up-to-date with the accounts created above

    tx.add(ComputeBudgetProgram.setComputeUnitLimit({units: 1000000}))
    tx.add(aquadex.instruction.createMarket(
        marketAgent.nonce,
        tokenVault1.nonce,
        tokenVault2.nonce,
        9,                                      // Mkt Decimals
        6,                                      // Prc Decimals
        0,                                      // Mkt Mint Type - 0: SPL, 1: AST-1
        0,                                      // Prc Mint Type 
        false,                                  // Manager actions (FALSE for trustless mode)
        true,                                   // Expire enable
        3,                                      // Tick size
        new anchor.BN(1),                       // Min expire
        new anchor.BN(0),                       // Min quantity
        new anchor.BN(3600),                    // Taker fee (X / 10,000,000; or 1,000 = 1 BPS)
        new anchor.BN(2500),                    // Maker rebate (X / 10,000,000; or 1,000 = 1 BPS)
        new anchor.BN(0),                       // Log fee (reserve space in settlement log)
        new anchor.BN(0),                       // Log rebate (when closing settled position)
        new anchor.BN(0),                       // Log reimburse (for creating new settlement log accounts)
        new anchor.BN(0),                       // Mkt Token UUID (AST-1 Tokens only)
        new anchor.BN(0),                       // Prc Token UUID (AST-1 Tokens only)
        {
            accounts: {
                market: marketPK,
                admin: marketAdminPK,
                state: marketState.publicKey,
                tradeLog: tradeLog.publicKey,
                agent: new PublicKey(marketAgent.pubkey),
                manager: provider.wallet.publicKey,
                feeManager: provider.wallet.publicKey,
                vaultManager: provider.wallet.publicKey,
                mktMint: tokenMint1,
                mktVault: new PublicKey(tokenVault1.pubkey),
                prcMint: tokenMint2,
                prcVault: new PublicKey(tokenVault2.pubkey),
                orders: orders.publicKey,
                settleA: settle1.publicKey,
                settleB: settle2.publicKey,
                splTokenProg: TOKEN_PROGRAM_ID,
                ascTokenProg: SPL_ASSOCIATED_TOKEN,
                systemProgram: SystemProgram.programId,
                systemRent: SYSVAR_RENT_PUBKEY,
            },
        }
    ))
    console.log('Create Market')
    console.log(await provider.sendAndConfirm(tx))
```

## Trade tokens:
```javascript
async function limitOrder(orderType, user, result, qty, price) {

    // Fetch up-to-date data (market data can be cached, state data should be reloaded)
    var marketData = await aquadex.account.market.fetch(marketPK)
    var stateData = await aquadex.account.market.fetch(marketData.state)

    var settle1PK = stateData.settleA
    var settle2PK = stateData.settleB
    var userToken1 = await associatedTokenAddress(user.publicKey, marketData.mktMint)
    var userToken2 = await associatedTokenAddress(user.publicKey, marketData.prcMint)

    var params = {
        accounts: {
            market: marketPK,
            state: marketSpec.state,
            agent: marketSpec.agent
            tradeLog: marketSpec.tradeLog
            user: user.publicKey,
            userMktToken: new PublicKey(userToken1.pubkey),
            userPrcToken: new PublicKey(userToken2.pubkey),
            mktVault: marketData.mktVault,
            prcVault: marketData.prcVault,
            orders: marketData.orders,
            settleA: stateData.settleA,
            settleB: stateData.settleB,
            result: result.publicKey,
            splTokenProg: TOKEN_PROGRAM_ID,
        },
        signers: [user, result],
    }
    var rollover = false
    var signers = [user, result]
    var tx = new anchor.web3.Transaction()

    // Market roll-over will not happen if the settlement log is frequently cleared to "user vaults"

    if (mktSpec.logRollover) {
        console.log("--- PERFORMING SETTLEMENT LOG ROLLOVER ---")
        rollover = true
        const settle = anchor.web3.Keypair.generate()
        const settleBytes = 326 + (16384 * 6)
        const settleRent = await provider.connection.getMinimumBalanceForRentExemption(settleBytes)
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
            new anchor.BN(qty),             // Quantity
            new anchor.BN(price),           // Price
            true,                           // Post order
            false,                          // Fill or cancel
            new anchor.BN(0),               // Order expiry
            preview,                        // Preview
            rollover,                       // Rollover settlement log
            params,
        ))
    } else {
        tx.add(await aquadex.instruction.limitAsk(
            new anchor.BN(qty),             // Quantity
            new anchor.BN(price),           // Price
            true,                           // Post order
            false,                          // Fill or cancel
            new anchor.BN(0),               // Order expiry
            preview,                        // Preview
            rollover,                       // Rollover settlement log
            params,
        ))
    }
    await provider.send(tx, signers)
}

// Create temp account for optional result data

var resultData = anchor.web3.Keypair.generate()
var tx = new anchor.web3.Transaction()
tx.add(
    anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: resultData.publicKey,
        space: tradeResultBytes,
        lamports: tradeResultRent,
        programId: aquadexPK
    })
)
await provider.sendAndConfirm(tx, [resultData])

// Perform a limit bid

console.log('Bid')
console.log(await limitOrder('bid', userWallet, resultData, qty.toFixed(0), price.toFixed(0), false))
var res = await aquadex.account.tradeResult.fetch(resultData.publicKey)

// Perform a limit ask

console.log('Ask')
console.log(await limitOrder('ask', userWallet, resultData, qty.toFixed(0), price.toFixed(0), false))
var res = await aquadex.account.tradeResult.fetch(resultData.publicKey)

```
