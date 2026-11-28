# NSE F&O option paper costs, 27 November 2026

These are simulation assumptions for long Nifty index options bought and sold
through an Upstox account. They are not an account fee certificate. The
contract note for a real account is the only record of what the account pays.
Current simulation settings are in [strategy](../STRATEGY.md#fees).

## Charges on an option order

Rates apply to the traded premium, which is price × quantity. Source dates are
the date printed on the source. The Upstox page has no date, so the table gives
the date it was read.

| Component | Rate | Side | Source | Source date |
| --- | --- | --- | --- | --- |
| Upstox brokerage | ₹20 per executed order | Each | [Upstox brokerage charges](https://upstox.com/brokerage-charges/) | Undated, read 27 Nov 2026 |
| NSE transaction charge | ₹3,552.99 per crore of premium | Each | [NSE/FA/73061](https://nsearchives.nseindia.com/content/circulars/FA73061.pdf) | 27 Feb 2026, in force from 1 Mar 2026 |
| NSE IPFT contribution | ₹0.01 per crore of premium | Each | [NSE/FA/73061](https://nsearchives.nseindia.com/content/circulars/FA73061.pdf) | 27 Feb 2026, in force from 1 Mar 2026 |
| SEBI turnover fee | ₹10 per crore, 0.0001% | Each | [NSE levy table](https://www.nseindia.com/static/invest/first-time-investor-sebi-turnover-fees-stt-other-levies), Upstox page | Undated |
| Stamp duty | 0.003% of premium | Buy | [NSE stamp duty page](https://www.nseindia.com/static/invest/first-time-investor-stamp-duty-charges-taxes), Upstox page | Uniform rates from 1 Jul 2020 |
| STT | 0.15% of premium | Sell | [NSE/FATAX/73524](https://nsearchives.nseindia.com/content/circulars/FATAX73524.pdf) | 31 Mar 2026, in force from 1 Apr 2026 |
| GST | 18% of brokerage, exchange charges and SEBI fee | Each | Upstox page | Undated |

The Finance Act, 2026 raised STT on the sale of an option from 0.10% to 0.15%
of premium. The rate applies to trades on or after 1 April 2026.

NSE/FA/73061 moved ₹49.99 of the IPFT contribution into the transaction
charge. The total stays ₹3,553 per crore. The Upstox page still shows an older
IPFT line of ₹0.50 per lakh. The NSE circular is the newer source.

Upstox lists the GST base as brokerage, transaction charges and IPFT, without
the SEBI fee. Most brokers also charge GST on the SEBI fee. This model includes
it. The difference is ₹0.18 per crore of premium.

Upstox had a ₹10 per order rate for API orders. Its brokerage page dates it to
31 December 2025. An
[Upstox community post](https://community.upstox.com/t/stay-ahead-of-the-curve-we-re-extending-10-order-api-pricing-till-march-2026/13277)
extended it to 31 March 2026. No later extension was found, so this model uses
₹20.

## Exercise

An option held to expiry in the money is exercised. NSE/FATAX/73524 charges
0.15% STT on its intrinsic value, paid by the buyer. Other charges on the
settlement value also apply. The runtime sells before expiry, so this model
has no exercise charges. A long option held into expiry would cost more than
this model shows.

## Nifty lot size

One Nifty 50 lot is 65 units.
[NSE/FAOP/70616](https://nsearchives.nseindia.com/content/circulars/FAOP70616.pdf),
dated 3 October 2025, cut the lot from 75 to 65. Weekly contracts used 65 from
the 6 January 2026 expiry. NSE's
[market lot file](https://nsearchives.nseindia.com/content/fo/fo_mktlots.csv),
last modified 27 November 2026, lists 65 for every Nifty expiry from
December 2026.

## Model values

`FeeArithmetic` in `rust/nivra-core/src/finance.rs` charges each fill
`per_order + turnover × bps / 10000`. It then rounds the total up to
`rounding_increment`.

`per_order` is brokerage with GST:

```text
20 × 1.18 = 23.60
```

Each side pays the exchange charge and SEBI fee with GST on both:

```text
exchange  3,553 / 1 crore = 3.553 bps
SEBI         10 / 1 crore = 0.01 bps
with GST  (3.553 + 0.01) × 1.18 = 4.20434 bps
```

The buy side adds stamp duty. The sell side adds STT:

```text
buy_bps  = 4.20434 + 0.3  = 4.50434
sell_bps = 4.20434 + 15   = 19.20434
```

### Slippage

The runtime fills a buy at the ask and a sell at the bid. Each fill already
pays the spread. `slippage_bps` then moves the price against the trade and
rounds it to the 0.05 tick against the trade. Any value above zero costs at
least one tick per fill.

Use 5 bps. This is one tick per side for premiums up to 100 and two ticks for
premiums of 130 to 200. This assumes that at-the-money weekly Nifty options
quote a one-tick spread with enough depth for a few lots. This review did not
measure spreads or depth. The extra tick covers price movement within the
2-second quote age limit. The current example of 10 bps
costs two ticks at a premium of 100 and four ticks at 200. Use 10 bps as a
stress setting, not the default.

### Rounding

Use 0.01. The fee is an amount in rupees and paise, not an option price. The
0.05 option tick does not apply to it. Rounding up to the paisa adds less than
₹0.01 per fill. STT rule 4 rounds STT to the nearest rupee on the contract
note, which covers a whole day of trades. The model does not copy that
rounding.

## Configuration fragment

```json
{"paper_costs_by_segment":{"NSE_FO":{"per_order":"23.60","buy_bps":"4.50434",
"sell_bps":"19.20434","slippage_bps":"5","rounding_increment":"0.01"}}}
```

## Worked example

One Nifty lot of 65 units, bought at a fill price of 100 and sold at a fill
price of 130.

| Charge | Buy, ₹6,500 premium | Sell, ₹8,450 premium |
| --- | --- | --- |
| Brokerage | 20.000000 | 20.000000 |
| NSE transaction and IPFT | 2.309450 | 3.002285 |
| SEBI fee | 0.006500 | 0.008450 |
| GST | 4.016871 | 4.141932 |
| Stamp duty | 0.195000 | 0 |
| STT | 0 | 12.675000 |
| Total | 26.527821 | 39.827667 |
| Model charge, rounded up to 0.01 | 26.53 | 39.83 |

The model charge equals the sum of the components on both sides. Total
charges for the round trip are ₹66.36. The gross profit is ₹1,950. The net
profit is ₹1,883.64.

With 5 bps slippage, an ask of 100 fills at 100.05 and a bid of 130 fills at
129.90. Slippage then costs ₹9.75 and the fees are ₹66.35. The net profit is
₹1,873.90.

## What was not checked

- The current text of the SEBI fee schedule. The ₹10 per crore rate comes from
  NSE's levy table, the Upstox page and an older SEBI consolidated copy.
  sebi.gov.in could not be reached on 27 November 2026.
- The statute text for stamp duty on option premium. The rate comes from NSE's
  stamp duty page and the Upstox page.
- Upstox invoice and contract note rounding, and whether Upstox charges any
  per-order or per-day amount not on its public page.
- The account's own plan. Upstox Plus lists brokerage of up to ₹30 per order.
