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

function exportSecretKey(keyPair) {
    var enc = new base32.Encoder({ type: "crockford", lc: true })
    return enc.write(keyPair.secretKey).finalize()
}

function importSecretKey(keyStr) {
    var dec = new base32.Decoder({ type: "crockford" })
    var spec = dec.write(keyStr).finalize()
    return Keypair.fromSecretKey(new Uint8Array(spec))
}

async function storeUser(uid, kp) {
    var data = Array.from(kp.secretKey)
    try {
        await fs.writeFile('user_data/' + uid + '.json', JSON.stringify(data))
    } catch (error) {
        console.log("File Error: " + error)
    }
}

async function main() {
    var users = []
    for (var x = 0; x < 100; x++) {
        kp = anchor.web3.Keypair.generate()
        users.push({
            'pubkey': kp.publicKey.toString(),
            'secret': exportSecretKey(kp),
        })
    }
    try {
        await fs.writeFile('user_data/user_list.json', JSON.stringify(users, null, 4))
    } catch (error) {
        console.log("File Error: " + error)
    }
    for (var i = 0; i < users.length; i++) {
        user = users[i]
        console.log('User: ' + (i + 1) + ' PK: ' + user.pubkey)
        userWallet = importSecretKey(user.secret)
        await storeUser(i + 1, userWallet)
        //process.exit(0)
    }
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
