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
async function main() {
    //var mint1 = await createTokenMint()
    //var mint2 = await createTokenMint()
    var mint1 = '3sd64AZF5fAC83i7wJ44Jxo145J6oE9fT2of6MtBjBeK'
    var mint2 = '3dkM9fyZ6AADz4SZLWh29rgrdwsLwKgubyM74wJzLdBs'
    console.log("Mints: " + mint1 + " " + mint2)
    const tokenMint1 = new PublicKey(mint1)
    const tokenMint2 = new PublicKey(mint2)

    var writeData = {}
    writeData['tokenMint1'] = tokenMint1.toString()
    writeData['tokenMint2'] = tokenMint2.toString()

    market = anchor.web3.Keypair.generate()
    marketState = anchor.web3.Keypair.generate()
    orders = anchor.web3.Keypair.generate()
    settle1 = anchor.web3.Keypair.generate()
    settle2 = anchor.web3.Keypair.generate()
    writeData['market'] = market.publicKey.toString()
    writeData['marketState'] = marketState.publicKey.toString()
    writeData['orders'] = orders.publicKey.toString()
    writeData['settle1'] = settle1.publicKey.toString()
    writeData['settle2'] = settle2.publicKey.toString()

    const ordersBytes = 130 + (16384 * 8)
    const ordersRent = await provider.connection.getMinimumBalanceForRentExemption(ordersBytes)

    const settleBytes = 130 + (16384 * 8)
    const settleRent = await provider.connection.getMinimumBalanceForRentExemption(settleBytes)

    const marketAgent = await programAddress([market.publicKey.toBuffer()])
    const tokenVault1 = await associatedTokenAddress(new PublicKey(marketAgent.pubkey), tokenMint1)
    const tokenVault2 = await associatedTokenAddress(new PublicKey(marketAgent.pubkey), tokenMint2)

    var tx = new anchor.web3.Transaction()
    tx.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: market.publicKey,
        space: aquadex.account.market.size,
        lamports: await provider.connection.getMinimumBalanceForRentExemption(aquadex.account.market.size),
        programId: aquadexPK,
    }))
    tx.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: marketState.publicKey,
        space: aquadex.account.marketState.size,
        lamports: await provider.connection.getMinimumBalanceForRentExemption(aquadex.account.marketState.size),
        programId: aquadexPK,
    }))
    tx.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: orders.publicKey,
        space: ordersBytes,
        lamports: ordersRent,
        programId: aquadexPK,
    }))
    tx.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: settle1.publicKey,
        space: settleBytes,
        lamports: settleRent,
        programId: aquadexPK,
    }))
    tx.add(anchor.web3.SystemProgram.createAccount({
        fromPubkey: provider.wallet.publicKey,
        newAccountPubkey: settle2.publicKey,
        space: settleBytes,
        lamports: settleRent,
        programId: aquadexPK,
    }))
    await provider.send(tx, [market, marketState, orders, settle1, settle2])

    console.log('Create Market')
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
}
```

## Trade tokens:
```javascript
function encodeOrderId(orderId) {
    const enc = new base32.Encoder({ type: "crockford", lc: true })
    var zflist = orderId.toBuffer().toJSON().data
    var zflen = 16 - zflist.length
    if (zflen > 0) {
        zfprefix = Array(zflen).fill(0)
        zflist = zfprefix.concat(zflist)
    }
    return enc.write(new Uint8Array(zflist)).finalize()
}

function formatOrder(order) {
    var res = {
        tokensFilled: order.tokensFilled.toString(),
        tokensPosted: order.tokensPosted.toString(),
        tokensDeposited: order.tokensDeposited.toString(),
        orderId: encodeOrderId(order.orderId),
    }
    return res
}

async function readMarketSpec() {
    var mjs
    try {
        mjs = await fs.readFile('market.json')
    } catch (error) {
        console.error('File Error: ', error)
    }
    mktData = JSON.parse(mjs.toString())

    marketPK = new PublicKey(mktData.market)
    marketStatePK = new PublicKey(mktData.marketState)
    ordersPK = new PublicKey(mktData.orders)
    settle1PK = new PublicKey(mktData.settle1)
    settle2PK = new PublicKey(mktData.settle2)
    marketAgent = await programAddress([marketPK.toBuffer()])
    tokenMint1 = new PublicKey(mktData.tokenMint1) // Market token
    tokenMint2 = new PublicKey(mktData.tokenMint2) // Pricing token
    tokenVault1 = await associatedTokenAddress(new PublicKey(marketAgent.pubkey), tokenMint1)
    tokenVault2 = await associatedTokenAddress(new PublicKey(marketAgent.pubkey), tokenMint2)

    settleRent = await provider.connection.getMinimumBalanceForRentExemption(settleBytes)
    tradeResultBytes = aquadex.account.tradeResult.size
    tradeResultRent = await provider.connection.getMinimumBalanceForRentExemption(tradeResultBytes)
    withdrawResultBytes = aquadex.account.withdrawResult.size
    withdrawResultRent = await provider.connection.getMinimumBalanceForRentExemption(withdrawResultBytes)
}

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

async function main() {
    await readMarketSpec()

    var ujs
    try {
        ujs = await fs.readFile('users.json')
    } catch (error) {
        console.error('File Error: ', error)
    }
    const users = JSON.parse(ujs.toString())

    // TODO: Make result account PDAs based on user key
    var resultData1 = anchor.web3.Keypair.generate()
    var tx = new anchor.web3.Transaction()
    tx.add(
        anchor.web3.SystemProgram.createAccount({
            fromPubkey: provider.wallet.publicKey,
            newAccountPubkey: resultData1.publicKey,
            space: tradeResultBytes,
            lamports: tradeResultRent,
            programId: aquadexPK
        })
    )
    await provider.send(tx, [resultData1])

    for (var i = 0; i < users.length; i++) {
        var user = users[i]
        console.log('User: ' + (i + 1) + ' PK: ' + user.pubkey)
        var userWallet = importSecretKey(user.secret)
        var userToken1 = await associatedTokenAddress(userWallet.publicKey, tokenMint1)
        var userToken2 = await associatedTokenAddress(userWallet.publicKey, tokenMint2)

        if ((i % 2) == 0) {
            console.log('Ask')
            await limitOrder('ask', userWallet, resultData1, 10, 5)
            var res = await aquadex.account.tradeResult.fetch(resultData1.publicKey)
            console.log(formatOrder(res))
        } else {
            console.log('Bid')
            await limitOrder('bid', userWallet, resultData1, 10, 5)
            var res = await aquadex.account.tradeResult.fetch(resultData1.publicKey)
            console.log(formatOrder(res))
        }

        /*if (i == 1) {
            process.exit(0)
        }*/
    }
}
```
