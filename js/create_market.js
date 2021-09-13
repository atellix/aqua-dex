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
    var mint1 = await createTokenMint()
    var mint2 = await createTokenMint()
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

    try {
        await fs.writeFile('market.json', JSON.stringify(writeData, null, 4))
    } catch (error) {
        console.log("File Error: " + error)
    }
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
