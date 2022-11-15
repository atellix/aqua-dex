const anchor = require('@project-serum/anchor')

const provider = anchor.AnchorProvider.env()
//const provider = anchor.AnchorProvider.local()
anchor.setProvider(provider)

async function main() {
    console.log(await provider.connection.getMinimumBalanceForRentExemption(60))
}

console.log('Begin')
main().then(() => console.log('Success')).catch(error => {
    console.log(error)
})
