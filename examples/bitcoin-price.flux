# bitcoin-price.flux — deterministically fetch the current BTC/USD spot price from Coinbase.
#
# No model is involved: the flow calls one fixed public API endpoint and extracts the price with a
# bounded regex.
# Run with: `flux flow run examples/bitcoin-price.flux`

flow bitcoin-price -> String
  $response = web.fetch({url: "https://api.coinbase.com/v2/prices/BTC-USD/spot", raw: true})
  $price = regex_extract({s: $response, pattern: "\"amount\"\\s*:\\s*\"([0-9]+(?:\\.[0-9]+)?)\"", group: 1})
  assert $price, "Coinbase response did not contain a BTC/USD price"
  return $price
