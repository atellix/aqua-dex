#!/usr/bin/env python3
from string import Template
tmpl = """---
json_rpc_url: "https://api.devnet.solana.com"
websocket_url: ""
keypair_path: /Users/mfrager/Build/solana/aqua-dex/js/users/$uid.json
address_labels:
  "11111111111111111111111111111111": System Program
commitment: confirmed
"""
tmpl.strip()
for i in range(100):
    j = i + 1
    with open(f'user_{j}.yml', 'w') as f:
        f.write(Template(tmpl).substitute(uid=j))
