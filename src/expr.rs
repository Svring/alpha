use serde_json::Value;

pub const BASIC_OPS: [&str; 6] = ["log", "sqrt", "reverse", "rank", "zscore", "normalize"];
pub const TS_OPS: [&str; 4] = ["ts_rank", "ts_zscore", "ts_delta", "ts_sum"];
pub const GROUP_OPS: [&str; 5] = [
    "group_neutralize",
    "group_rank",
    "group_normalize",
    "group_scale",
    "group_zscore",
];

pub fn process_datafields(fields: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    for item in fields {
        let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let dtype = item.get("type").and_then(|v| v.as_str()).unwrap_or_default();
        if dtype == "MATRIX" {
            out.push(wrap_backfill(id));
        } else if dtype == "VECTOR" {
            out.push(wrap_backfill(&format!("vec_avg({id})")));
            out.push(wrap_backfill(&format!("vec_sum({id})")));
        }
    }
    out
}

fn wrap_backfill(field: &str) -> String {
    format!("winsorize(ts_backfill({field}, 120), std=4)")
}

pub fn first_order_factory(fields: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for f in fields {
        out.push(f.clone());
        for op in BASIC_OPS {
            out.push(format!("{op}({f})"));
        }
        for op in TS_OPS {
            for d in [5, 22, 66, 120, 240] {
                out.push(format!("{op}({f}, {d})"));
            }
        }
        for op in GROUP_OPS {
            for g in ["market", "sector", "industry", "subindustry"] {
                out.push(format!("{op}({f},densify({g}))"));
            }
        }
    }
    out
}

pub fn second_order_group(expressions: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for e in expressions {
        for op in GROUP_OPS {
            for g in ["market", "sector", "industry", "subindustry"] {
                out.push(format!("{op}({e},densify({g}))"));
            }
        }
    }
    out
}

pub fn adjusted_decay(turnover: f64, decay: i32) -> Option<i32> {
    if turnover > 0.7 {
        Some(decay * 4)
    } else if turnover > 0.6 {
        Some(decay * 3 + 3)
    } else if turnover > 0.5 {
        Some(decay * 3)
    } else if turnover > 0.4 {
        Some(decay * 2)
    } else if turnover > 0.35 {
        Some(decay + 4)
    } else if turnover > 0.3 {
        Some(decay + 2)
    } else {
        None
    }
}
