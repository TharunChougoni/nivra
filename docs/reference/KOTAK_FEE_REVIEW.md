# Kotak fee research — 8 October 2026

Historical public estimates, **not an activated account fee profile**. Current
model-led simulation costs are in [strategy](../STRATEGY.md).

| Component, ordinary NSE non-delivery cash | Dated public estimate |
| --- | --- |
| Exchange transaction / IPFT | ₹306.99 / ₹0.01 per crore, each side |
| SEBI non-debt fee | ₹10/crore, each side |
| Qualifying non-delivery STT | 0.025%, sell |
| Stamp, non-debenture securities | 0.003%, buy |
| GST | 18% of applicable service-charge base |

[NSE/FA/73061](https://nsearchives.nseindia.com/content/circulars/FA73061.pdf)
changed the transaction/IPFT split effective March 2026;
[NSE's levy table](https://www.nseindia.com/static/invest/first-time-investor-sebi-turnover-fees-stt-other-levies)
defines classifications. [Kotak's calculator](https://www.kotakneo.com/calculator/brokerage-calculator/)
is indicative. Public API brokerage waivers do not remove taxes or certify this
account's plan, delivery/ETF classification, exceptions or invoice aggregation.

[STT rule 4](https://www.sebi.gov.in/sebi_data/attachdocs/apr-2017/1492087548939.pdf)
provides nearest-rupee rounding with half upward. This is distinct from the
simulation's optional aggregate **upward** `Fees.rounding_increment`. Per-fill
simulation charges do not establish broker partial-fill/invoice rounding.
`Fees.cash_requirement` and affordable-lot sizing include configured costs/slippage;
zero brokerage advertising never authorises zero aggregate costs.

Account-specific certification is not established. Preserve fee-version/run
bindings, original receipts and charges. Future evidence work is tracked in
[GitHub Issues](https://github.com/TharunChougoni/nivra/issues); this reference
grants no live authority.
