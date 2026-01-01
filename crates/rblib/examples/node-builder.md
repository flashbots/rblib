# Using Builder Playground with rblib

You can quickly spin up an op-stack devnet using builder-playground.
The quickest workflow to get op-stack running against your local rblib builder instance is:

1. Clone [flashbots/builder-playground](https://github.com/flashbots/builder-playground.git) and start an OPStack chain.

```
git clone git@github.com:flashbots/builder-playground.git
cd builder-playground
go run main.go cook opstack --external-builder http://host.docker.internal:4444
```

2. Create a reth builder node by turning main rblib pipeline in a payload builder service. We will use the `pipeline-op`
   as an example.

3. Run in builder playground mode:

```text
cargo run --example pipeline-op -- node --http --http.port 2222 \
    --authrpc.addr 0.0.0.0 --authrpc.port 4444 --authrpc.jwtsecret $HOME/.playground/devnet/jwtsecret \
    --chain $HOME/.playground/devnet/l2-genesis.json --datadir /tmp/builder --disable-discovery --port 30333 \
    --trusted-peers enode://79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8@127.0.0.1:30304
```

Here the `datadir` contains the db, so be sure to clean up the db state between runs:

```
rm -rf /tmp/builder/
```
