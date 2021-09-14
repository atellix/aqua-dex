const { Buffer } = require('buffer')
const { DateTime } = require("luxon")
const { v4: uuidv4, parse: uuidparse } = require('uuid')
const { PublicKey, Keypair, SystemProgram, SYSVAR_RENT_PUBKEY } = require('@solana/web3.js')
const { TOKEN_PROGRAM_ID } = require('@solana/spl-token')
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

async function createTokenMint() {
    var res = await exec('./new_token.sh')
    return res.stdout
}

async function main() {
    var ndjs
    try {
        ndjs = await fs.readFile('market.json')
    } catch (error) {
        console.error('File Error: ', error)
    }
    const mktData = JSON.parse(ndjs.toString())
    console.log(mktData)
    const marketPK = new PublicKey(mktData.market)
    const marketStatePK = new PublicKey(mktData.marketState)
    const ordersPK = new PublicKey(mktData.orders)
    const settle1PK = new PublicKey(mktData.settle1)
    const settle2PK = new PublicKey(mktData.settle2)

    const tokenMint1 = new PublicKey(mktData.tokenMint1) // Market token
    const tokenMint2 = new PublicKey(mktData.tokenMint2) // Pricing token

    const marketAgent = await programAddress([marketPK.toBuffer()])
    const tokenVault1 = await associatedTokenAddress(new PublicKey(marketAgent.pubkey), tokenMint1)
    const tokenVault2 = await associatedTokenAddress(new PublicKey(marketAgent.pubkey), tokenMint2)

    const userToken1 = await associatedTokenAddress(provider.wallet.publicKey, tokenMint1)
    const userToken2 = await associatedTokenAddress(provider.wallet.publicKey, tokenMint2)

    console.log('Market Agent: ' + marketAgent.pubkey)
    console.log('User Token: ' + userToken2.pubkey)
    console.log('Vault Token: ' + tokenVault2.pubkey)

    console.log('Limit Bid')
    await aquadex.rpc.limitBid(
        new anchor.BN(10 * 10000), // Quantity
        new anchor.BN(5 * 10000), // Price
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
                splTokenProg: TOKEN_PROGRAM_ID,
            }
        }
    )

    console.log('Limit Ask')
    await aquadex.rpc.limitAsk(
        new anchor.BN(11 * 10000), // Quantity
        new anchor.BN(7 * 10000), // Price
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
                splTokenProg: TOKEN_PROGRAM_ID,
            }
        }
    )

}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
