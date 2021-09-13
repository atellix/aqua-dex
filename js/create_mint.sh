#!/bin/bash

TMP=$(mktemp)
solana-keygen new --silent --no-bip39-passphrase --force --outfile $TMP 2>&1 > /dev/null
MINT=$(solana-keygen pubkey $TMP)
spl-token create-token --decimals 4 --output json -- $TMP 2>&1 > /dev/null
spl-token create-account $MINT --output json 2>&1 > /dev/null
spl-token mint $MINT 100000 --output json 2>&1 > /dev/null
rm $TMP
echo -n $MINT

