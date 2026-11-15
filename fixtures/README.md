# Fixture provenance

All TEST| identifiers, prices, capital, fees, sessions and events in generated demo
fixtures are explicitly synthetic. They are not exchange data or market calendars.
Upstox HTTP fixtures in tests reproduce documented field shapes, with fabricated
test values. These synthetic fixtures do not establish broker access.

Sources: https://upstox.com/developer/api-documentation/v3/get-historical-candle-data/
and https://upstox.com/developer/api-documentation/get-pc-option-chain/ (2026-09-22).
Production calendars and instrument metadata must be supplied with provenance.

`rust/history-legacy/` contains three credential-free daily receipts and one shard
manifest per receipt retained for the history reuse regression. These are real
candle evidence, not synthetic prices or calendar proof.

`rust/bootstrap-paper.example.json` is a sanitized legacy delivery-test fixture,
not an installation template. Its home and filesystem IDs are synthetic;
account/scope aliases, dates and paper-policy values remain compatibility
literals required by the legacy validators, not broker account IDs or balances.
Never install this fixture or substitute it for an original journal binding.

`upstox_full_quote_doc_example.json` is derived from the documented NHPC Full Market Quote example; source: https://upstox.com/developer/api-documentation/get-full-market-quote/ (2026-10-27). It is documentation evidence, not a captured market receipt.
