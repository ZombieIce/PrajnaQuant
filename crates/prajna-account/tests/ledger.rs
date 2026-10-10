use prajna_account::{
    Fill, FillCosts, FillPricing, LedgerError, Order, OrderId, Side, TradingAccount,
    VirtualPortfolioId,
};
use prajna_domain::{Notional, Price, Quantity, TimestampNs};
use std::collections::BTreeMap;

fn n(s: &str) -> Notional {
    s.parse().unwrap()
}
fn p(s: &str) -> Price {
    s.parse().unwrap()
}
fn q(s: &str) -> Quantity {
    s.parse().unwrap()
}
fn account() -> TradingAccount {
    TradingAccount::new(
        "CNY".parse().unwrap(),
        "XSHG".parse().unwrap(),
        n("10000"),
        [
            (VirtualPortfolioId(1), n("4000")),
            (VirtualPortfolioId(2), n("5000")),
        ],
    )
    .unwrap()
}
fn order(id: u64, vp: u64, side: Side, qty: &str) -> Order {
    Order::new(
        OrderId(id),
        VirtualPortfolioId(vp),
        "A.XSHG".parse().unwrap(),
        side,
        q(qty),
        TimestampNs::from_unix_nanos(10),
    )
    .unwrap()
}
fn fill(order: Order, raw: &str, pricing: FillPricing, costs: FillCosts) -> Fill {
    Fill::new(
        order,
        TimestampNs::from_unix_nanos(11),
        p(raw),
        pricing,
        costs,
    )
    .unwrap()
}
fn raw_fill(order: Order, price: &str) -> Fill {
    fill(
        order,
        price,
        FillPricing::SeparateSlippage {
            slippage: Notional::ZERO,
        },
        FillCosts::ZERO,
    )
}
fn settle(account: &mut TradingAccount, fill: Fill) {
    account.submit_order(fill.order().clone()).unwrap();
    account.apply_fill(fill).unwrap();
}
fn prices(price: &str) -> BTreeMap<prajna_domain::InstrumentId, Price> {
    BTreeMap::from([("A.XSHG".parse().unwrap(), p(price))])
}

#[test]
fn fixed_capital_allocation_retains_unallocated_cash() {
    let a = account();
    assert_eq!(a.initial_cash(), n("10000"));
    assert_eq!(a.unallocated_capital(), n("1000"));
    assert_eq!(
        a.portfolios()[&VirtualPortfolioId(1)].initial_capital(),
        n("4000")
    );
    let value = a.value(&BTreeMap::new()).unwrap();
    assert_eq!(value.account.cash, n("10000"));
    assert_eq!(value.account.equity, n("10000"));
    value.validate().unwrap();
}

#[test]
fn two_vps_buy_same_etf_with_independent_fills_and_exact_two_layer_equity() {
    let mut a = account();
    // VP1: 1000 principal + 1 commission + 4 top-up + 2 tax + 3 slippage = 1010.
    settle(
        &mut a,
        fill(
            order(1, 1, Side::Buy, "100"),
            "10",
            FillPricing::SeparateSlippage { slippage: n("3") },
            FillCosts {
                commission: n("1"),
                minimum_commission_top_up: n("4"),
                tax: n("2"),
            },
        ),
    );
    settle(&mut a, raw_fill(order(2, 2, Side::Buy, "200"), "10"));
    assert_eq!(a.fills().len(), 2);
    assert_eq!(a.fills()[0].order().portfolio_id(), VirtualPortfolioId(1));
    assert_eq!(a.fills()[1].order().portfolio_id(), VirtualPortfolioId(2));
    assert_eq!(a.positions()[&"A.XSHG".parse().unwrap()], q("300"));
    assert_eq!(a.cash(), n("6990"));
    let open = a.value(&prices("10")).unwrap();
    assert_eq!(open.account.equity, n("9990"));
    assert_eq!(open.portfolios[&VirtualPortfolioId(1)].equity, n("3990"));
    let close = a.value(&prices("12")).unwrap();
    assert_eq!(close.account.equity, n("10590"));
    assert_eq!(close.portfolios[&VirtualPortfolioId(1)].equity, n("4190"));
    assert_eq!(close.portfolios[&VirtualPortfolioId(2)].equity, n("5400"));
    open.validate().unwrap();
    close.validate().unwrap();
    // Selling VP1 does not consume VP2's position or alter its cash.
    settle(&mut a, raw_fill(order(3, 1, Side::Sell, "100"), "12"));
    assert!(
        a.portfolios()[&VirtualPortfolioId(1)]
            .positions()
            .is_empty()
    );
    assert_eq!(a.portfolios()[&VirtualPortfolioId(2)].cash(), n("3000"));
    assert_eq!(a.value(&prices("12")).unwrap().account.equity, n("10590"));
}

#[test]
fn embedded_and_separate_slippage_reconcile_every_fee_without_double_charge() {
    for (side, execution, raw_cash, final_cash) in [
        (Side::Buy, "10.1", "-1000", "-1017"),
        (Side::Sell, "9.9", "1000", "983"),
    ] {
        let costs = FillCosts {
            commission: n("1"),
            minimum_commission_top_up: n("4"),
            tax: n("2"),
        };
        let embedded = fill(
            order(1, 1, side, "100"),
            "10",
            FillPricing::EmbeddedSlippage {
                execution_price: p(execution),
            },
            costs,
        );
        let separate = fill(
            order(1, 1, side, "100"),
            "10",
            FillPricing::SeparateSlippage { slippage: n("10") },
            costs,
        );
        for f in [&embedded, &separate] {
            assert_eq!(f.raw_notional(), n("1000"));
            assert_eq!(f.slippage(), n("10"));
            assert_eq!(f.costs().total_commission().unwrap(), n("5"));
            assert_eq!(f.total_fees(), n("17"));
            assert_eq!(f.cash_delta(), n(final_cash));
            assert_eq!(
                n(raw_cash).mantissa() - f.cash_delta().mantissa(),
                n("17").mantissa()
            );
        }
        assert_eq!(embedded.price(), p(execution));
        assert_eq!(separate.price(), p("10"));
        assert_eq!(
            embedded.trade_notional(),
            if side == Side::Buy {
                n("1010")
            } else {
                n("990")
            }
        );
    }
}

#[test]
fn fractional_decimal_fills_and_valuation_match_hand_calculation() {
    let mut a = account();
    settle(&mut a, raw_fill(order(1, 1, Side::Buy, "1.25"), "2.4"));
    assert_eq!(a.cash(), n("9997"));
    assert_eq!(a.value(&prices("3.2")).unwrap().account.equity, n("10001"));
    settle(&mut a, raw_fill(order(2, 1, Side::Sell, "0.25"), "3.2"));
    assert_eq!(a.cash(), n("9997.8"));
    assert_eq!(a.positions()[&"A.XSHG".parse().unwrap()], q("1"));
    assert_eq!(a.value(&prices("3.2")).unwrap().account.equity, n("10001"));
}

#[test]
fn scale_18_half_even_marks_are_consolidated_without_rounding_again() {
    let mut a = account();
    settle(&mut a, raw_fill(order(1, 1, Side::Buy, "1e-18"), "1"));
    settle(&mut a, raw_fill(order(2, 2, Side::Buy, "1e-18"), "1"));
    let value = a.value(&prices("0.5")).unwrap();
    let mark = &value.account.positions[&"A.XSHG".parse().unwrap()];
    assert_eq!(mark.quantity, q("2e-18"));
    assert_eq!(mark.market_value, Notional::ZERO); // two half-even 0.5-unit marks round to zero.
    assert_eq!(value.account.equity, n("9999.999999999999999998"));
    value.validate().unwrap();
    // Embedded slippage is the difference of rounded notionals, preserving settlement.
    let f = fill(
        order(3, 1, Side::Buy, "1e-18"),
        "0.5",
        FillPricing::EmbeddedSlippage {
            execution_price: p("1.5"),
        },
        FillCosts::ZERO,
    );
    assert_eq!(f.raw_notional(), Notional::ZERO);
    assert_eq!(f.trade_notional(), n("2e-18"));
    assert_eq!(f.slippage(), n("2e-18"));
    assert_eq!(f.cash_delta(), n("-2e-18"));
}

#[test]
fn rejects_overallocation_negative_cash_negative_allocation_and_duplicate_vps() {
    let build = |cash, allocations: Vec<_>| {
        TradingAccount::new(
            "CNY".parse().unwrap(),
            "XSHG".parse().unwrap(),
            cash,
            allocations,
        )
    };
    assert_eq!(build(n("-1"), vec![]), Err(LedgerError::NegativeCash));
    assert_eq!(
        build(n("1"), vec![(VirtualPortfolioId(1), n("2"))]),
        Err(LedgerError::InvalidAllocation)
    );
    assert_eq!(
        build(n("1"), vec![(VirtualPortfolioId(1), n("-1"))]),
        Err(LedgerError::InvalidAllocation)
    );
    assert_eq!(
        build(
            n("1"),
            vec![
                (VirtualPortfolioId(1), n("0")),
                (VirtualPortfolioId(1), n("0"))
            ]
        ),
        Err(LedgerError::DuplicatePortfolio(VirtualPortfolioId(1)))
    );
    assert!(
        build(n("0"), vec![])
            .unwrap()
            .value(&BTreeMap::new())
            .is_ok()
    );
}

#[test]
fn cash_shortage_cannot_borrow_from_another_vp_or_unallocated_capital() {
    let mut a = account();
    let f = raw_fill(order(1, 1, Side::Buy, "401"), "10");
    a.submit_order(f.order().clone()).unwrap();
    let before = a.clone();
    assert_eq!(a.apply_fill(f), Err(LedgerError::NegativeCash));
    assert_eq!(a, before);
    let f = fill(
        order(2, 1, Side::Buy, "400"),
        "10",
        FillPricing::SeparateSlippage {
            slippage: n("1e-18"),
        },
        FillCosts::ZERO,
    );
    a.submit_order(f.order().clone()).unwrap();
    let before = a.clone();
    assert_eq!(a.apply_fill(f), Err(LedgerError::NegativeCash));
    assert_eq!(a, before);
    settle(&mut a, raw_fill(order(3, 1, Side::Buy, "400"), "10"));
    assert_eq!(
        a.portfolios()[&VirtualPortfolioId(1)].cash(),
        Notional::ZERO
    );
}

#[test]
fn rejects_short_sale_even_if_other_vp_holds_enough_and_preserves_state() {
    let mut a = account();
    settle(&mut a, raw_fill(order(1, 2, Side::Buy, "100"), "10"));
    let f = raw_fill(order(2, 1, Side::Sell, "1"), "10");
    a.submit_order(f.order().clone()).unwrap();
    let before = a.clone();
    assert_eq!(a.apply_fill(f), Err(LedgerError::InsufficientPosition));
    assert_eq!(a, before);
}

#[test]
fn rejects_invalid_trade_inputs_and_fill_before_submission() {
    for qty in ["-1", "0"] {
        assert_eq!(
            Order::new(
                OrderId(1),
                VirtualPortfolioId(1),
                "A.XSHG".parse().unwrap(),
                Side::Buy,
                q(qty),
                TimestampNs::from_unix_nanos(10)
            ),
            Err(LedgerError::NonPositiveQuantity)
        );
    }
    let o = order(1, 1, Side::Buy, "1");
    let make = |price, pricing, costs, time| {
        Fill::new(
            o.clone(),
            TimestampNs::from_unix_nanos(time),
            price,
            pricing,
            costs,
        )
    };
    let raw = FillPricing::SeparateSlippage {
        slippage: Notional::ZERO,
    };
    for price in ["0", "-1"] {
        assert_eq!(
            make(p(price), raw, FillCosts::ZERO, 11),
            Err(LedgerError::NonPositivePrice)
        );
        assert_eq!(
            make(
                p("1"),
                FillPricing::EmbeddedSlippage {
                    execution_price: p(price)
                },
                FillCosts::ZERO,
                11
            ),
            Err(LedgerError::NonPositivePrice)
        );
    }
    for costs in [
        FillCosts {
            commission: n("-1"),
            ..FillCosts::ZERO
        },
        FillCosts {
            minimum_commission_top_up: n("-1"),
            ..FillCosts::ZERO
        },
        FillCosts {
            tax: n("-1"),
            ..FillCosts::ZERO
        },
    ] {
        assert_eq!(make(p("1"), raw, costs, 11), Err(LedgerError::NegativeFee));
    }
    assert_eq!(
        make(
            p("1"),
            FillPricing::SeparateSlippage { slippage: n("-1") },
            FillCosts::ZERO,
            11
        ),
        Err(LedgerError::NegativeFee)
    );
    assert_eq!(
        make(
            p("1"),
            FillPricing::EmbeddedSlippage {
                execution_price: p("0.9")
            },
            FillCosts::ZERO,
            11
        ),
        Err(LedgerError::FavorableSlippage)
    );
    assert_eq!(
        make(p("1"), raw, FillCosts::ZERO, 9),
        Err(LedgerError::FillBeforeOrder)
    );
}

#[test]
fn order_ownership_identity_venue_and_single_full_fill_are_enforced() {
    let mut a = account();
    assert_eq!(
        a.submit_order(order(1, 3, Side::Buy, "1")),
        Err(LedgerError::UnknownPortfolio(VirtualPortfolioId(3)))
    );
    let foreign = Order::new(
        OrderId(1),
        VirtualPortfolioId(1),
        "A.XSHE".parse().unwrap(),
        Side::Buy,
        q("1"),
        TimestampNs::from_unix_nanos(10),
    )
    .unwrap();
    assert_eq!(a.submit_order(foreign), Err(LedgerError::WrongVenue));
    let f = raw_fill(order(1, 1, Side::Buy, "1"), "10");
    assert_eq!(
        a.apply_fill(f.clone()),
        Err(LedgerError::UnknownOrder(OrderId(1)))
    );
    a.submit_order(f.order().clone()).unwrap();
    assert_eq!(
        a.submit_order(f.order().clone()),
        Err(LedgerError::DuplicateOrder(OrderId(1)))
    );
    let before = a.clone();
    assert_eq!(
        a.apply_fill(raw_fill(order(1, 2, Side::Buy, "1"), "10")),
        Err(LedgerError::OrderMismatch)
    );
    assert_eq!(a, before);
    a.apply_fill(f.clone()).unwrap();
    let before = a.clone();
    assert_eq!(a.apply_fill(f), Err(LedgerError::AlreadyFilled(OrderId(1))));
    assert_eq!(a, before);
}

#[test]
fn missing_zero_and_negative_held_valuation_prices_are_errors() {
    let mut a = account();
    settle(&mut a, raw_fill(order(1, 1, Side::Buy, "1"), "10"));
    assert_eq!(
        a.value(&BTreeMap::new()),
        Err(LedgerError::MissingPrice("A.XSHG".parse().unwrap()))
    );
    for price in ["0", "-1"] {
        assert_eq!(a.value(&prices(price)), Err(LedgerError::NonPositivePrice));
    }
    // A carried price is explicit input; no fabricated zero price or new Fill.
    assert_eq!(a.value(&prices("10")).unwrap().account.equity, n("10000"));
    assert_eq!(a.fills().len(), 1);
}

#[test]
fn corrupt_cash_quantity_marks_and_equity_snapshots_fail_exact_checks() {
    let mut a = account();
    settle(&mut a, raw_fill(order(1, 1, Side::Buy, "1"), "10"));
    let good = a.value(&prices("12")).unwrap();
    let id = "A.XSHG".parse().unwrap();
    for mutate in [
        |v: &mut prajna_account::AccountValuation| v.account.cash = n("9990.000000000000000001"),
        |v: &mut prajna_account::AccountValuation| v.account.equity = n("10002.000000000000000001"),
        |v: &mut prajna_account::AccountValuation| {
            v.portfolios.get_mut(&VirtualPortfolioId(1)).unwrap().equity =
                n("4002.000000000000000001")
        },
    ] {
        let mut bad = good.clone();
        mutate(&mut bad);
        assert!(matches!(bad.validate(), Err(LedgerError::Invariant(_))));
    }
    let mut bad = good.clone();
    bad.account.positions.get_mut(&id).unwrap().quantity = q("2");
    assert!(matches!(bad.validate(), Err(LedgerError::Invariant(_))));
    let mut bad = good.clone();
    bad.portfolios
        .get_mut(&VirtualPortfolioId(1))
        .unwrap()
        .positions
        .get_mut(&id)
        .unwrap()
        .market_value = n("11");
    assert!(matches!(bad.validate(), Err(LedgerError::Invariant(_))));
    let mut bad = good;
    bad.portfolios
        .get_mut(&VirtualPortfolioId(1))
        .unwrap()
        .positions
        .get_mut(&id)
        .unwrap()
        .price = p("0");
    assert_eq!(bad.validate(), Err(LedgerError::NonPositivePrice));
}

#[test]
fn aggregate_quantity_overflow_rejects_fill_atomically() {
    let mut a = account();
    let huge = Quantity::MAX.to_string();
    settle(&mut a, raw_fill(order(1, 1, Side::Buy, &huge), "1e-18"));
    let f = raw_fill(order(2, 2, Side::Buy, &huge), "1e-18");
    a.submit_order(f.order().clone()).unwrap();
    let before = a.clone();
    assert!(matches!(a.apply_fill(f), Err(LedgerError::Arithmetic(_))));
    assert_eq!(a, before);
    assert!(a.value(&prices("2")).is_err()); // valuation notional overflow.
}
