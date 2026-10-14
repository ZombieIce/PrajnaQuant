"""Test-only pure Decisions; Rust remains Factor/account/performance authority."""
import prajna


def s2_declaration(params):
    factor_params = {key: params[key] for key in
                     ("short", "long", "vol", "w_s", "w_l", "w_v", "trend")}
    return prajna.Strategy(
        "acceptance.s2", "1", bar_columns=(),
        factors=(prajna.FactorDependency("score", "rotation_score", factor_params),),
        rebalance_every=params["rebalance_every"],
    )


def s2_decide(view, params, previous):
    current = [
        row for row in view.factors["score"].to_pylist()
        if row["session_date"].isoformat() == view.session_date and row["status"] == "ok"
    ]
    current.sort(key=lambda row: (-row["value"], row["instrument_id"]))
    selected = current[:params["top_k"]]
    return prajna.Decision(
        {row["instrument_id"]: 1.0 / len(selected) for row in selected},
        [(row["instrument_id"], row["value"], index + 1)
         for index, row in enumerate(current)],
    )


def s3_declaration():
    # Two adjacent MA20/60 windows; prev_decision is intent, never actual holdings.
    return prajna.Strategy("acceptance.s3", "1", lookback_sessions=61)


def s3_decide(view, params, previous):
    # Compare nanoseconds directly rather than boxing every Arrow timestamp.
    sessions = view.sessions["session_date"].to_pylist()
    cutoffs = view.sessions["ts_close"].cast("int64").to_pylist()
    availability = view.bars["available_at"].cast("int64").to_pylist()
    bars = {(row["instrument_id"], row["session_date"]): row
            for row in view.bars.select(["instrument_id", "session_date", "close"]).to_pylist()}
    for row, time in zip(bars.values(), availability):
        row["available_at"] = time
    held = set(previous.targets) if previous else set()
    for instrument in view.universe:
        gaps = []
        for window, cutoff in ((sessions[:-1], cutoffs[-2]), (sessions[1:], cutoffs[-1])):
            rows = [bars.get((instrument, session)) for session in window]
            if any(row is None or row["available_at"] > cutoff for row in rows):
                gaps.append(None)
                continue
            closes = [float(row["close"]) for row in rows]
            gaps.append(sum(closes[-20:]) / 20 - sum(closes) / 60)
        prior, current = gaps
        if prior is not None and current is not None:
            if prior <= 0 < current:
                held.add(instrument)
            if prior >= 0 > current:
                held.discard(instrument)
    return prajna.Decision({instrument: 1.0 / len(view.universe) for instrument in held})
