# Worker for DOD miner pool

A [DOD (Doge-on-Doge)](https://dod.cool) worker that submits PoW answers to a mining pool.

**Usage**

```
MINER_ID=XXXX cargo run --release <hostname> <port>
```

Where the default hostname is `localhost` and port is `3030`.

The `MINER_ID` is how you identifier yourself with the mining pool.
Or in other words, the mining pool decides whom to pay based on the `MINER_ID`.
You can run multiple workers under the same `MINER_ID`.

**Protocol**

This part is technical detail, and only useful if you are building a mining pool.

New job is obtained from HTTP GET, for example:

```
$ curl http://localhost:3030/job

{
  "buffer": "0100000001091497bd22f9170feb8d396ca668e02ec823442062e7b9cfe722222abd9cb5d20000000000fdffffff034905000000000000225120f5a11ea39c10b92898a53ac14b65d778870b5028ca12a93612b63a498e10b43b0000000000000000126a109d4b1212d0c917e668e55bbeb5eda7171b510100000000002251201d650546387f83c06f71300709447c9a9608f03e87c179dd4cbf01f825cf06e500000000",
  "hex_start": 101,
  "remote_hash": "091497bd22f9170feb8d396ca668e02ec823442062e7b9cfe722222abd9cb5d2",
  "pre": 8,
  "post_hex": "5",
  "next_block_time": 1729664582963,
  "block_height": 15871,
  "submitted": false, 
}
```

Answer is submitted via HTTP POST, for example:

```
$ curl -H 'content-type: application/json' -H 'miner-id: XXXXX' http://localhost:3030/answer \
       -d '{"block_height":15871, "nonce":"71020818e45de5fc516008f55b22bab7"}'
```

Note that even partial answers are submitted so that a pool can calculate each miner's contribution.
It is up to the mining pool to check if a solution is correct, and to decide how the rewards are split between miners.

**Limitations**

- The worker tries to guess available cores which is usually the number of threads divided by 2, but it could be wrong.
- The worker only uses HTTP not HTTPS, which is not secure.

PRs are welcome!
