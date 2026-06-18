# DEX offer crossing (legacy Taker) — design & implementation plan

Status: design. Scope: byte-exact reproduction of rippled's **legacy Taker**
offer-crossing for play-forward replay of mainnet ledgers. This is the largest
remaining play-forward surface and is decomposed into independently validated
increments below.

Context: `OfferCreate` currently *places* a resting offer and sweeps unfunded
offers from the inverse book (`crates/tx-engine/src/handlers/offer_create.rs`)
but never **crosses** — it does not match against the book, fill, adjust
balances, consume offers, or place a remainder. Metadata fidelity for
non-crossing offers is byte-exact (#316000, PR #157); directory ordering is
amendment-gated (#158); real `Rules` are derived from the ledger (#159).

## Why "legacy Taker"

The `FlowCross` amendment replaced the `Taker` crossing engine. Mainnet ledgers
from 2013–2014 (the small-state ledgers usable by the e2e harness) predate it,
so byte-exact replay needs the **pre-Flow Taker** algorithm. Gate on
`featureFlowCross` once Flow is implemented; until then the Taker path is
unconditional (empty `Rules` ⇒ no FlowCross ⇒ Taker, matching 2013).

Reference source: `XRPLF/rippled` tag `1.12.0` (last tag carrying the full
`Taker.cpp`/`Taker.h`; on 3.1.3/develop the legacy files are emptied because
FlowCross is mandatory). Original paths: `src/ripple/app/tx/impl/{Taker,CreateOffer,OfferStream,BookTip}.*`, `src/ripple/protocol/STAmount.cpp`, `src/ripple/ledger/impl/View.cpp`.

## Oracle ledger: mainnet #338500 (the first crossing increment target)

Single-tx 2013 ledger, full-fill XRP↔IOU crossing with a transfer fee — the
simplest non-trivial crossing. Decoded via the hub
(`RXRPL_PLAY_FORWARD_RPC`, `100.123.2.126:5005`):

- Taker `rE5PjKY…` OfferCreate seq 11: `TakerGets = 44925000000` drops (XRP out),
  `TakerPays = 1 BTC` (rvYAfWj issuer). Wants 1 BTC for 44925 XRP.
- Crosses resting offer of `r4FbZZ5…` seq 68: `TakerGets = 1 BTC`,
  `TakerPays = 44925000000`. Exact full match.

Effects (the byte-exact target):
- Taker AccountRoot: `49999999900 → 5074999890` (−44925000000 XRP −10 fee), seq 11→12.
- Owner AccountRoot: `43416639889 → 88341639889` (+44925000000 XRP), OwnerCount 5→4.
- Taker RippleState (BTC): balance `0 → -1` (taker receives **net** 1 BTC).
- Owner RippleState (BTC): `-3.022389776875825 → -2.020389776875825`
  (owner pays **grossed** 1.002389776875825 BTC; the 0.002389… delta is the BTC
  issuer's transfer fee, burned). ⇒ transfer rate ≈ 1.002389776875825.
- Owner Offer **deleted** (DeletedNode Offer + DeletedNode book DirectoryNode +
  owner DirectoryNode modified + OwnerCount −1).
- Taker fully crossed ⇒ **no Offer placed** (no CreatedNode Offer). `tesSUCCESS`.

Other increments need their own oracles (found by scanning OfferCreate metadata
for `Offer` nodes whose `FinalFields.Account != tx.Account`): partial fill
(Offer ModifiedNode with reduced TakerGets/TakerPays), multi-offer walk, IOU↔IOU,
remainder placement (CreatedNode Offer after crossing), tfSell / tfPassive /
tfFillOrKill / tfImmediateOrCancel.

## Algorithm (byte-critical summary; full detail from rippled 1.12.0)

Orientation: during crossing, **in/out are reversed vs the tx fields** —
taker.in = TakerGets (what the new offerer pays out), taker.out = TakerPays.
Quality is read from the offer's directory, never recomputed.

1. **Book walk** (`BookTip`/`OfferStream`): iterate the inverse book
   `(gets, pays)` directories in ascending index order = best quality first.
   `BookTip::step` deletes the *previous* tip before advancing. `OfferStream::step`
   applies a fixed skip/delete sequence (missing entry → erase; expired →
   permRmOffer; zero amount → permRmOffer; unfunded → compare vs cancelView,
   remove if "found unfunded") — the comment marks this order protocol-breaking
   to change.
2. **Threshold**: cross only while tip quality ≥ taker's own offer quality
   (`reject(q) = q < threshold_`). `tfPassive` ⇒ `++threshold_` (strict).
   Direct book skips the taker's *own* offers (`step_account`).
3. **Fill math** (`flow_xrp_to_iou`/`flow_iou_to_xrp`/`flow_iou_to_iou`): start
   at the full offer, apply clamps **in the exact order** (owner funds, taker
   desired out [buy only], taker funds, remaining in), each recomputing the
   dependent side via `qual_mul`/`qual_div` (`= multiply/divide(.., rate)` then
   `std::min(result, output)`). `order.{in,out}` = net amounts; `issuers.{in,out}`
   = grossed (× transfer rate) for redeem/issue.
4. **Apply per fill** (`Taker::fill`, strict order): (1) consume offer
   (`TakerPays -= order.in`, `TakerGets -= order.out`, update in place); (2) input
   side taker→owner (`redeemIOU(taker, issuers.in)` then `issueIOU(owner, order.in)`,
   or `transferXRP`); (3) output side owner→taker (`redeemIOU(owner, issuers.out)`
   then `issueIOU(taker, order.out)`, or `transferXRP`). Transfer-fee burn =
   grossed − net.
5. **Deletion**: a fully-consumed (or owner-dry) offer is deleted on the *next*
   `BookTip::step` via `offerDelete` (dirRemove owner + book, OwnerCount −1, erase).
   Partial fill ⇒ amounts reduced in place, offer stays.
6. **Remainder** (`remaining_offer`): if fully done ⇒ place nothing; else rescale
   leftover at the **original** quality with `roundUp=true` (`divRound` for sell,
   `mulRound` for buy) and place a new Offer (owner+book dir insert, OwnerCount
   +1, original uRate).
7. **Flags**: tfPassive (strict threshold, lsfPassive); tfSell (no out-clamp,
   keep all remaining in); tfImmediateOrCancel (no placement); tfFillOrKill —
   **pre-fix1578 (2013): partial ⇒ tesSUCCESS but discarded**, not tecKILLED.
   Result codes: tecUNFUNDED_OFFER, tecKILLED (amendment), tecINSUF_RESERVE_OFFER,
   tecDIR_FULL, tefINTERNAL.

### Rounding (the prerequisite — see gap below)

- Non-rounding `multiply` (IOU pre-Number: `muldiv(a,b,10^14)+7`, offset +14) /
  `divide` (`muldiv(num,10^17,den)+5`, offset −17), with native scale-up loops.
  Gate the post-switchover `Number` path off for old ledgers
  (`getSTNumberSwitchover()==false`).
- Directional `mulRound`/`divRound` use the legacy `canonicalizeRound`
  (the `loops>=2 ? 9 : 10` XRP quirk: fractional drop ≥0.1 rounds up). Used only
  for remainder placement and `ceil_in`/`ceil_out`. `*Strict` variants are
  amendment-only — not the 2013 path.
- Quality encoding `getRate(out,in) = ((exp+100)<<56)|mantissa`.

## rxrpl mapping & gaps

Exists: `rxrpl_amount::IOUAmount` with `multiply/divide/mul_round/div_round/
canonicalize_round`, `quality::{get_rate, offer_quality}`; `owner_dir` paginated
insert/remove (gated, #158); offer placement + book-dir keying (#156); RippleState
manipulation in `payment.rs` rippling; `keylet::book_dir`.

Gaps (build order):
1. **Unified amount (XRP drops + IOU) with rippled-exact arithmetic.** The Taker
   mixes native and IOU in one `STAmount`. `IOUAmount` covers IOU; need an
   `Amount`/`STAmount` enum (XRP|IOU) with `multiply`/`divide`/`mulRound`/
   `divRound` matching rippled across native↔IOU. Validate as pure functions
   against vectors from the oracle (e.g. 1 BTC × transfer-rate = 1.002389776875825;
   the 44925-XRP legs). **No engine needed to test this.**
2. **Transfer rate**: read issuer `AccountRoot.TransferRate`, `effective_rate`
   (parity when issuer is a party or self-send).
3. **Book walk** over inverse-book quality directories (reuse `owner_dir` page
   chain reading; ascending index; skip own/unfunded/expired).
4. **Fill clamps** (the three `flow_*`) as pure functions over the unified amount.
5. **State effects**: `redeemIOU`/`issueIOU` (RippleState, line create/delete,
   noRipple/defaultRipple) and `transferXRP` (AccountRoot) — factor from
   `payment.rs` where possible.
6. **Offer consume/delete** (reuse `owner_dir::dir_remove`, gated) + OwnerCount.
7. **Remainder placement** (reuse existing placement) with original-quality rescale.
8. Wire into `OfferCreate::apply` ahead of placement; gate on `!FlowCross`.

## Validation per increment

Each increment lands only when its oracle ledger is byte-exact via the e2e
harness (`play_forward_end_to_end_mainnet`) **and** the per-tx metadata harness
(`offer_meta_diff_mainnet`, `play_forward.rs`) shows 0 diffs, **and** hive
consensus + propagation stay green (crossing touches the apply path). Reserve
constants for 2013 ledgers: 200 XRP base / 50 XRP increment.

Increment 1 target: **#338500** (full-fill XRP↔IOU + transfer fee, no remainder).
