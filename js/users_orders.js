const { Buffer } = require('buffer')
const { DateTime } = require("luxon")
const { v4: uuidv4, parse: uuidparse } = require('uuid')
const { PublicKey, Keypair, SystemProgram, SYSVAR_RENT_PUBKEY } = require('@solana/web3.js')
const { TOKEN_PROGRAM_ID, Token } = require('@solana/spl-token')
const { promisify } = require('util')
const exec = promisify(require('child_process').exec)
const fs = require('fs').promises
const base32 = require("base32.js")
const anchor = require('@project-serum/anchor')

const provider = anchor.Provider.env()
//const provider = anchor.Provider.local()
anchor.setProvider(provider)

const aquadex = anchor.workspace.AquaDex
const aquadexPK = aquadex.programId

const SPL_TOKEN_BYTES = 165
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

async function main() {
    var mjs
    try {
        mjs = await fs.readFile('market.json')
    } catch (error) {
        console.error('File Error: ', error)
    }
    const mktData = JSON.parse(mjs.toString())

    const marketPK = new PublicKey(mktData.market)
    const marketStatePK = new PublicKey(mktData.marketState)
    const ordersPK = new PublicKey(mktData.orders)
    const settle1PK = new PublicKey(mktData.settle1)
    const settle2PK = new PublicKey(mktData.settle2)
    const marketAgent = await programAddress([marketPK.toBuffer()])
    const tokenMint1 = new PublicKey(mktData.tokenMint1) // Market token
    const tokenMint2 = new PublicKey(mktData.tokenMint2) // Pricing token
    const tokenVault1 = await associatedTokenAddress(new PublicKey(marketAgent.pubkey), tokenMint1)
    const tokenVault2 = await associatedTokenAddress(new PublicKey(marketAgent.pubkey), tokenMint2)

    const tradeResultBytes = aquadex.account.tradeResult.size
    const tradeResultRent = await provider.connection.getMinimumBalanceForRentExemption(tradeResultBytes)
    const withdrawResultBytes = aquadex.account.withdrawResult.size
    const withdrawResultRent = await provider.connection.getMinimumBalanceForRentExemption(withdrawResultBytes)

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
            await aquadex.rpc.limitAsk(
                new anchor.BN(10 * 10000), // Quantity
                new anchor.BN(5 * 10000),  // Price
                true,
                false,
                new anchor.BN(0),           // Order expiry
                {
                    accounts: {
                        market: marketPK,
                        state: marketStatePK,
                        agent: new PublicKey(marketAgent.pubkey),
                        user: userWallet.publicKey,
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
                    signers: [userWallet],
                }
            )
            var res = await aquadex.account.tradeResult.fetch(resultData1.publicKey)
            console.log(formatOrder(res))
        } else {
            console.log('Bid')
            await aquadex.rpc.limitBid(
                new anchor.BN(10 * 10000), // Quantity
                new anchor.BN(5 * 10000),  // Price
                true,
                false,
                new anchor.BN(0),           // Order expiry
                {
                    accounts: {
                        market: marketPK,
                        state: marketStatePK,
                        agent: new PublicKey(marketAgent.pubkey),
                        user: userWallet.publicKey,
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
                    signers: [userWallet],
                }
            )
            var res = await aquadex.account.tradeResult.fetch(resultData1.publicKey)
            console.log(formatOrder(res))
        }

        /*if (i == 9) {
            process.exit(0)
        }*/
    }
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
