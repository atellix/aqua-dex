const { Buffer } = require('buffer')
const { DateTime } = require('luxon')
const { v4: uuidv4, parse: uuidparse } = require('uuid')
const { PublicKey, Keypair, SystemProgram, SYSVAR_RENT_PUBKEY } = require('@solana/web3.js')
const { TOKEN_PROGRAM_ID } = require('@solana/spl-token')
const { promisify } = require('util')
const exec = promisify(require('child_process').exec)
const fs = require('fs').promises
const base32 = require('base32.js')
const anchor = require('@project-serum/anchor')

const provider = anchor.AnchorProvider.env()
//const provider = anchor.Provider.local()
anchor.setProvider(provider)

async function main() {
    const txBytes = 1
    const txRent = await provider.connection.getMinimumBalanceForRentExemption(txBytes)
    console.log('Bytes: ' + txBytes)
    console.log('Rent: ' + txRent)
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
