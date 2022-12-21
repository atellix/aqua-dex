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

async function programAddress(inputs, program = aquadexPK) {
    const addr = await PublicKey.findProgramAddress(inputs, program)
    const res = { 'pubkey': await addr[0].toString(), 'nonce': addr[1] }
    return res
}

console.log("User: " + provider.wallet.publicKey.toString())

async function main() {
    var jsres = await exec('solana program show --output json ' + aquadexPK.toString())
    var res = JSON.parse(jsres.stdout)
    const programData = res.programdataAddress

    const rootData = await programAddress([aquadexPK.toBuffer()], aquadexPK)
    console.log("Root Data: " + rootData.pubkey)

    console.log('Deactivate')
    await aquadex.rpc.deactivate(
        {
            accounts: {
                program: aquadexPK,
                programAdmin: provider.wallet.publicKey,
                programData: new PublicKey(programData),
                rootData: new PublicKey(rootData.pubkey),
                systemProgram: SystemProgram.programId,
            }
        }
    )
}

main().then(() => process.exit(0)).catch(error => {
    console.log(error)
})
