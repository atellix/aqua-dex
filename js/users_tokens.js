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

function exportSecretKey(keyPair) {
    var enc = new base32.Encoder({ type: "crockford", lc: true })
    return enc.write(keyPair.secretKey).finalize()
}

function importSecretKey(keyStr) {
    var dec = new base32.Decoder({ type: "crockford" })
    var spec = dec.write(keyStr).finalize()
    return Keypair.fromSecretKey(new Uint8Array(spec))
}

async function main() {
    var mjs
    try {
        mjs = await fs.readFile('market.json')
    } catch (error) {
        console.error('File Error: ', error)
    }
    const mktData = JSON.parse(mjs.toString())
    const tokenMint1 = new PublicKey(mktData.tokenMint1) // Market token
    const tokenMint2 = new PublicKey(mktData.tokenMint2) // Pricing token

    var ujs
    try {
        ujs = await fs.readFile('users.json')
    } catch (error) {
        console.error('File Error: ', error)
    }
    const users = JSON.parse(ujs.toString())
      
    for (var i = 0; i < users.length; i++) {
        user = users[i]
        console.log('User: ' + (i + 1) + ' PK: ' + user.pubkey)
        userWallet = importSecretKey(user.secret)

        // Airdrop SOL
        var airdropSig = await provider.connection.requestAirdrop(userWallet.publicKey, anchor.web3.LAMPORTS_PER_SOL * 10)
        await provider.connection.confirmTransaction(airdropSig)

        // Create associated tokens
        var token1 = new Token(provider.connection, tokenMint1, TOKEN_PROGRAM_ID, userWallet)
        var token2 = new Token(provider.connection, tokenMint2, TOKEN_PROGRAM_ID, userWallet)
        var userToken1 = await token1.getOrCreateAssociatedAccountInfo(userWallet.publicKey)
        var userToken2 = await token2.getOrCreateAssociatedAccountInfo(userWallet.publicKey)

        // Mint tokens
        var token1 = new Token(provider.connection, tokenMint1, TOKEN_PROGRAM_ID, provider.wallet.payer)
        var token2 = new Token(provider.connection, tokenMint2, TOKEN_PROGRAM_ID, provider.wallet.payer)
        await token1.mintTo(userToken1.address, provider.wallet.payer, [], '1000000000000')
        await token2.mintTo(userToken2.address, provider.wallet.payer, [], '1000000000000')
    }
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
