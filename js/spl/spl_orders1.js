const { Buffer } = require('buffer')
const { DateTime } = require("luxon")
const { v4: uuidv4, parse: uuidparse, stringify: uuidstringify } = require('uuid')
const { PublicKey, Keypair, SystemProgram, SYSVAR_RENT_PUBKEY } = require('@solana/web3.js')
const { TOKEN_PROGRAM_ID } = require('@solana/spl-token')
const { promisify } = require('util')
const exec = promisify(require('child_process').exec)
const fs = require('fs').promises
const base32 = require("base32.js")
const anchor = require('@project-serum/anchor')

const provider = anchor.AnchorProvider.env()
//const provider = anchor.Provider.local()
anchor.setProvider(provider)

const aquadex = anchor.workspace.AquaDex
const aquadexPK = aquadex.programId

const SPL_ASSOCIATED_TOKEN = new PublicKey('ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL')
async function associatedTokenAddress(walletAddress, tokenMintAddress) {
    const addr = await PublicKey.findProgramAddress(
        [walletAddress.toBuffer(), TOKEN_PROGRAM_ID.toBuffer(), tokenMintAddress.toBuffer()],
        SPL_ASSOCIATED_TOKEN
    )
    const res = { 'pubkey': await addr[0].toString(), 'nonce': addr[1] }
    return res
}

async function programAddress(inputs, program = aquadexPK) {
    const addr = await PublicKey.findProgramAddress(inputs, program)
    const res = { 'pubkey': await addr[0].toString(), 'nonce': addr[1] }
    return res
}

function exportSecretKey(keyPair) {
    var enc = new base32.Encoder({ type: "crockford", lc: true })
    return enc.write(keyPair.secretKey).finalize()
}

function importSecretKey(keyStr) {
    var dec = new base32.Decoder({ type: "crockford" })
    var spec = dec.write(keyStr).finalize()
    return Keypair.fromSecretKey(new Uint8Array(spec))
}

function buf2hex(buffer) { // buffer is an ArrayBuffer
  return [...new Uint8Array(buffer)]
      .map(x => x.toString(16).padStart(2, '0'))
      .join('');
}

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

function decodeOrderId(orderId) {
    var dec = new base32.Decoder({ type: "crockford" })
    var spec = dec.write(orderId).finalize()
    var arr = new Uint8Array(spec)
    var arrhex = [...arr].map(x => x.toString(16).padStart(2, '0')).join('')
    var bgn = BigInt('0x' + arrhex)
    return new anchor.BN(bgn.toString())
}

function formatOrder(order) {
    var res = {
        tokensReceived: order.tokensReceived.toString(),
        tokensSent: order.tokensSent.toString(),
        tokensFee: order.tokensFee.toString(),
        postedQuantity: order.postedQuantity.toString(),
        orderId: encodeOrderId(order.orderId),
    }
    return res
}

function formatWithdraw(order) {
    var res = {
        marketTokens: order.mktTokens.toString(),
        pricingTokens: order.prcTokens.toString(),
    }
    return res
}

async function createTokenMint() {
    var res = await exec('./new_token.sh')
    return res.stdout
}

async function main() {
    var ndjs
    try {
        ndjs = await fs.readFile('market_wsol_usdc_1.json')
    } catch (error) {
        console.error('File Error: ', error)
    }
    const mktData = JSON.parse(ndjs.toString())
    //console.log(mktData)
    const marketPK = new PublicKey(mktData.market)
    const marketStatePK = new PublicKey(mktData.marketState)
    const ordersPK = new PublicKey(mktData.orders)
    const settle1PK = new PublicKey(mktData.settle1)
    const settle2PK = new PublicKey(mktData.settle2)

    const tokenMint1 = new PublicKey(mktData.tokenMint1) // Market token
    const tokenMint2 = new PublicKey(mktData.tokenMint2) // Pricing token

    const marketAgent = await programAddress([marketPK.toBuffer()], aquadexPK)
    const marketAgentPK = new PublicKey(marketAgent.pubkey)
    const tokenVault1 = await associatedTokenAddress(new PublicKey(marketAgent.pubkey), tokenMint1)
    const tokenVault2 = await associatedTokenAddress(new PublicKey(marketAgent.pubkey), tokenMint2)

    const userToken1 = await associatedTokenAddress(provider.wallet.publicKey, tokenMint1)
    const userToken2 = await associatedTokenAddress(provider.wallet.publicKey, tokenMint2)

    const tradeResultBytes = aquadex.account.tradeResult.size
    const tradeResultRent = await provider.connection.getMinimumBalanceForRentExemption(tradeResultBytes)
    const withdrawResultBytes = aquadex.account.withdrawResult.size
    const withdrawResultRent = await provider.connection.getMinimumBalanceForRentExemption(withdrawResultBytes)

    console.log('Market Agent: ' + marketAgent.pubkey)
    console.log('User Token: ' + userToken2.pubkey)
    console.log('Vault Token: ' + tokenVault2.pubkey)

    //res = await aquadex.account.market.fetch(marketPK)
    //console.log(res)

    var resultAccounts = {}
    var resultData1
    var resultData2
    if (false) {
        console.log("Create Result Accounts")
        resultData1 = anchor.web3.Keypair.generate()
        resultData2 = anchor.web3.Keypair.generate()
        const tx = new anchor.web3.Transaction()
        tx.add(
            anchor.web3.SystemProgram.createAccount({
                fromPubkey: provider.wallet.publicKey,
                newAccountPubkey: resultData1.publicKey,
                space: tradeResultBytes,
                lamports: tradeResultRent,
                programId: aquadexPK
            })
        )
        tx.add(
            anchor.web3.SystemProgram.createAccount({
                fromPubkey: provider.wallet.publicKey,
                newAccountPubkey: resultData2.publicKey,
                space: withdrawResultBytes,
                lamports: withdrawResultRent,
                programId: aquadexPK
            })
        )
        await provider.sendAndConfirm(tx, [resultData1, resultData2])
        try {
            await fs.writeFile('result_accts_1.json', JSON.stringify({
                'TradeResult': exportSecretKey(resultData1),
                'WithdrawResult': exportSecretKey(resultData2),
            }, null, 4))
        } catch (error) {
            console.log("File Error: " + error)
        }
    } else {
        var rsjs
        try {
            rsjs = await fs.readFile('result_accts_1.json')
        } catch (error) {
            console.error('File Error: ', error)
        }
        resultAccounts = JSON.parse(rsjs.toString())
        resultData1 = importSecretKey(resultAccounts['TradeResult'])
        resultData2 = importSecretKey(resultAccounts['WithdrawResult'])
    }

    var order1

    if (false) {
        console.log('Limit Ask 1')
        console.log(await aquadex.rpc.limitAsk(
            new anchor.BN(1 * (10**9)),    // Quantity
            new anchor.BN(14.62 * (10**6)),  // Price
            true,                            // Post
            false,                           // Fill
            new anchor.BN(0),                // Order expiry
            false,                           // Settlement Log Rollover
            {
                accounts: {
                    market: marketPK,
                    state: marketStatePK,
                    agent: new PublicKey(marketAgent.pubkey),
                    user: provider.wallet.publicKey,
                    userMktToken: new PublicKey(userToken1.pubkey),
                    userPrcToken: new PublicKey(userToken2.pubkey),
                    mktVault: new PublicKey(tokenVault1.pubkey),
                    prcVault: new PublicKey(tokenVault2.pubkey),
                    orders: ordersPK,
                    settleA: settle1PK,
                    settleB: settle2PK,
                    result: resultData1.publicKey,
                    splTokenProg: TOKEN_PROGRAM_ID,
                },
                signers: [resultData1],
            }
        ))
        var res = await aquadex.account.tradeResult.fetch(resultData1.publicKey)
        order1 = res.orderId
        console.log(formatOrder(res))
    }

    if (true) {
        console.log('Cancel Order 1')
        var orderId = decodeOrderId('00000000vwap00000000000008')
        //var orderId = order1
        console.log(await aquadex.rpc.cancelOrder(
            1, // 0 - Bid, 1 - Ask
            orderId,
            {
                accounts: {
                    market: marketPK,
                    state: marketStatePK,
                    agent: new PublicKey(marketAgent.pubkey),
                    owner: provider.wallet.publicKey,
                    userMktToken: new PublicKey(userToken1.pubkey),
                    userPrcToken: new PublicKey(userToken2.pubkey),
                    mktVault: new PublicKey(tokenVault1.pubkey),
                    prcVault: new PublicKey(tokenVault2.pubkey),
                    orders: ordersPK,
                    result: resultData2.publicKey,
                    splTokenProg: TOKEN_PROGRAM_ID,
                },
                signers: [resultData2],
            }
        ))
        res = await aquadex.account.withdrawResult.fetch(resultData2.publicKey)
        console.log(formatWithdraw(res))
    }

    if (false) {
        console.log('Limit Ask 2')
        await aquadex.rpc.limitAsk(
            new anchor.BN(10 * 10000), // Quantity
            new anchor.BN(5.1 * 10000), // Price
            true,
            false,
            new anchor.BN(0),           // Order expiry
            false,
            {
                accounts: {
                    market: marketPK,
                    state: marketStatePK,
                    agent: new PublicKey(marketAgent.pubkey),
                    user: provider.wallet.publicKey,
                    userMktToken: new PublicKey(userToken1.pubkey),
                    userPrcToken: new PublicKey(userToken2.pubkey),
                    mktVault: new PublicKey(tokenVault1.pubkey),
                    prcVault: new PublicKey(tokenVault2.pubkey),
                    orders: ordersPK,
                    settleA: settle1PK,
                    settleB: settle2PK,
                    result: resultData1.publicKey,
                    splTokenProg: TOKEN_PROGRAM_ID,
                    astTokenProg: securityTokenPK,
                },
                signers: [resultData1],
            }
        )
        res = await aquadex.account.tradeResult.fetch(resultData1.publicKey)
        console.log(formatOrder(res))
    }

    if (false) {
        console.log('Limit Bid')
        console.log(await aquadex.rpc.limitBid(
            new anchor.BN(25 * 1000000),        // Quantity
            new anchor.BN(2.5 * 1000000),       // Price
            true,                       // Post order
            false,                      // Require filled if not posted
            new anchor.BN(0),           // Order expiry
            false,                      // Log rollover
            {
                accounts: {
                    market: marketPK,
                    state: marketStatePK,
                    agent: new PublicKey(marketAgent.pubkey),
                    user: provider.wallet.publicKey,
                    userMktToken: new PublicKey(userToken1.pubkey),
                    userPrcToken: new PublicKey(userToken2.pubkey),
                    mktVault: new PublicKey(tokenVault1.pubkey),
                    prcVault: new PublicKey(tokenVault2.pubkey),
                    orders: ordersPK,
                    settleA: settle1PK,
                    settleB: settle2PK,
                    result: resultData1.publicKey,
                    splTokenProg: TOKEN_PROGRAM_ID,
                },
                signers: [resultData1],
            }
        ))
        res = await aquadex.account.tradeResult.fetch(resultData1.publicKey)
        console.log(formatOrder(res))
    }
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
