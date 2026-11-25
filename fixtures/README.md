# Fixture provenance

| Fixture | Source |
| --- | --- |
| `TEST\|` identifiers, prices, capital, fees, sessions and events | Synthetic. Not exchange data or a market calendar. |
| Upstox HTTP fixtures in tests | Documented field shapes with made-up values, from the [historical candle](https://upstox.com/developer/api-documentation/v3/get-historical-candle-data/) and [option chain](https://upstox.com/developer/api-documentation/get-pc-option-chain/) API pages (read 2026-09-22) |
| `upstox_full_quote_doc_example.json` | The documented NHPC example on the [full market quote](https://upstox.com/developer/api-documentation/get-full-market-quote/) page (read 2026-10-27). Not a captured receipt. |
| `rust/history-legacy/` | Three real, credential-free daily candle receipts with one shard manifest each, kept for the history reuse regression test |
| `rust/frozen-goldens.json` | Manifest of the frozen vectors that `nivra-tools golden-check` verifies |

Fixtures do not prove broker access. Supply production calendars and instrument
metadata with their provenance.
