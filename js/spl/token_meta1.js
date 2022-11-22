const { Buffer } = require('buffer')
const { DateTime } = require("luxon")
const { v4: uuidv4, parse: uuidparse, stringify: uuidstringify } = require('uuid')
const { PublicKey, Keypair, SystemProgram, SYSVAR_RENT_PUBKEY } = require('@solana/web3.js')
const { TOKEN_PROGRAM_ID } = require('@solana/spl-token')
const { promisify } = require('util')
const exec = promisify(require('child_process').exec)
const fs = require('fs').promises
const base32 = require('base32.js')
const anchor = require('@project-serum/anchor')
const { Metadata, PROGRAM_ID: METAPLEX_PROGRAM_ID } = require('@metaplex-foundation/mpl-token-metadata')

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

function showData(spec) {
    var r = {}
    for (var i in spec) {
        if (typeof spec[i] === 'object' && spec[i].constructor.name === 'Object') {
            r[i] = showData(spec[i])
        } else if (typeof spec[i].toString !== 'undefined') {
            r[i] = spec[i].toString()
        }
    }
    return r
}

async function main() {
    var ndjs
    var market = process.argv[2]
    try {
        ndjs = await fs.readFile(market)
    } catch (error) {
        console.error('File Error: ', error)
    }
    const mktData = JSON.parse(ndjs.toString())
    //console.log(mktData)
    var mintPubkey = new PublicKey('USDV47taduJQSPePwGsFR9GPbYRbmRt8BCx2fRRYJEt')
    var dataAddr = await programAddress([
        Buffer.from('metadata'),
        METAPLEX_PROGRAM_ID.toBuffer(),
        mintPubkey.toBuffer(),
    ], METAPLEX_PROGRAM_ID)
    console.log('Metadata Account: ' + dataAddr.pubkey)
    var meta = await Metadata.fromAccountAddress(provider.connection, new PublicKey(dataAddr.pubkey))
    var name = meta.data.name
    var symbol = meta.data.symbol
    var uri = meta.data.uri
    name = name.replace(/\x00/g, '')
    symbol = symbol.replace(/\x00/g, '')
    uri = uri.replace(/\x00/g, '')
    var spec = {
        'name': name,
        'symbol': symbol,
        'uri': uri,
    }
    console.log(spec)
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
