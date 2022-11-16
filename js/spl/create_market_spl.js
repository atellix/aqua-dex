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

async function createTokenMint() {
    var res = await exec('./new_token.sh')
    return res.stdout
}

async function main() {
    //var mint1 = await createTokenMint()
    //var mint2 = await createTokenMint()
    var mint1 = 'So11111111111111111111111111111111111111112'
    var mint2 = 'Gh9ZwEmdLJ8DscKNTkTqPbNwLNNBjuSzaG9Vp2KGtKJr'
    console.log("Mints: " + mint1 + " " + mint2)
    const tokenMint1 = new PublicKey(mint1)
    const tokenMint2 = new PublicKey(mint2)

    var writeData = {}
    writeData['tokenMint1'] = tokenMint1.toString()
    writeData['tokenMint2'] = tokenMint2.toString()

    market = anchor.web3.Keypair.generate()
    marketPK = market.publicKey
    //marketPK = new PublicKey('DiugnZvAuabA21qhNWfRvAJT6cpx1xyXRTF3P4b4xyJw')
    marketState = anchor.web3.Keypair.generate()
    orders = anchor.web3.Keypair.generate()
    settle1 = anchor.web3.Keypair.generate()
    settle2 = anchor.web3.Keypair.generate()
    writeData['market'] = marketPK.toString()
    writeData['marketState'] = marketState.publicKey.toString()
    writeData['orders'] = orders.publicKey.toString()
    writeData['settle1'] = settle1.publicKey.toString()
    writeData['settle2'] = settle2.publicKey.toString()

    const ordersBytes = 130 + (16384 * 7)
    const ordersRent = await provider.connection.getMinimumBalanceForRentExemption(ordersBytes)

    const settleBytes = 130 + (16384 * 8)
    const settleRent = await provider.connection.getMinimumBalanceForRentExemption(settleBytes)

    const accountId1 = uuidv4()
    const accountBuf1 = Buffer.from(uuidparse(accountId1).reverse())

    const marketAgent = await programAddress([marketPK.toBuffer()], aquadexPK)
    const marketAgentPK = new PublicKey(marketAgent.pubkey)
    const tokenVault1 = await associatedTokenAddress(marketAgentPK, tokenMint1)
    const tokenVault2 = await associatedTokenAddress(marketAgentPK, tokenMint2)

    console.log("Market Agent: " + marketAgent.pubkey)

    console.log("Token Vault: " + tokenVault1.pubkey)
    console.log("Token Vault UUID: " + accountId1)

    console.log("Market Size: " + aquadex.account.market.size)
    console.log("MarketState Size: " + aquadex.account.marketState.size)

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
    console.log(await provider.sendAndConfirm(ta, [market, marketState]))

    var tx = new anchor.web3.Transaction()
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

    console.log({
        market: marketPK.toString(),
        state: marketState.publicKey.toString(),
        agent: new PublicKey(marketAgent.pubkey).toString(),
        manager: provider.wallet.publicKey.toString(),
        mktMint: tokenMint1.toString(),
        mktVault: new PublicKey(tokenVault1.pubkey).toString(),
        prcMint: tokenMint2.toString(),
        prcVault: new PublicKey(tokenVault2.pubkey).toString(),
        orders: orders.publicKey.toString(),
        settleA: settle1.publicKey.toString(),
        settleB: settle2.publicKey.toString(),
        splTokenProg: TOKEN_PROGRAM_ID.toString(),
        ascTokenProg: SPL_ASSOCIATED_TOKEN.toString(),
        sysProg: SystemProgram.programId.toString(),
        sysRent: SYSVAR_RENT_PUBKEY.toString(),
    })
    tx.add(aquadex.instruction.createMarket(
        marketAgent.nonce,
        tokenVault1.nonce,
        tokenVault2.nonce,
        9,
        6,
        0,                                      // Mkt Mint Type - 0: SPL, 1: AST
        0,                                      // Prc Mint Type 
        false,                                  // Manager withdrawals (FALSE for trustless mode)
        false,                                  // Manager order cancel (FALSE for trustless mode)
        true,                                   // Expire enable
        new anchor.BN(1),                       // Min expire
        new anchor.BN(0),                       // Taker fee (X / 10,000,000)
        new anchor.BN(0),                       // Log fee (reserve space in settlement log)
        new anchor.BN(0),                       // Log rebate (when closing settled position)
        new anchor.BN(0),                       // Log reimburse (for creating new settlement log accounts)
        new anchor.BN(0),                       // Mkt Token UUID (Security Tokens only)
        new anchor.BN(0),                       // Prc Token UUID (Security Tokens only)
        {
            accounts: {
                market: marketPK,
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
                systemProgram: SystemProgram.programId,
                systemRent: SYSVAR_RENT_PUBKEY,
            },
        }
    ))

    console.log('Create Market')
    console.log(await provider.sendAndConfirm(tx, [orders, settle1, settle2]))

    try {
        await fs.writeFile('market_wsol_usdc_1.json', JSON.stringify(writeData, null, 4))
    } catch (error) {
        console.log("File Error: " + error)
    }
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
